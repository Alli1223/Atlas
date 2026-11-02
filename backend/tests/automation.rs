//! End-to-end tests for automation: rules fire from real changes (through the activity pump),
//! conditions gate them, smart values render, the audit log records the trace, loops stop,
//! scheduled and webhook triggers run, and dry runs change nothing.

mod common;

use atlas::jobs;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use serde_json::{Value, json};

async fn rule(app: &App, cookie: &str, project: &str, name: &str, spec: Value) -> String {
    app.expect(
        post(
            &format!("/api/v1/projects/{project}/automation/rules"),
            Some(cookie),
            json!({ "name": name, "spec": spec }),
        ),
        StatusCode::CREATED,
    )
    .await
    .id()
}

async fn drain(app: &App) {
    jobs::pump_until_idle(&app.state())
        .await
        .expect("pump failed");
}

async fn comments(app: &App, cookie: &str, key: &str) -> Vec<String> {
    app.expect(
        get(&format!("/api/v1/cards/{key}/comments"), Some(cookie)),
        StatusCode::OK,
    )
    .await
    .rows()
    .iter()
    .map(|c| text(c, "body"))
    .collect()
}

async fn runs(app: &App, cookie: &str, rule_id: &str) -> Vec<Value> {
    app.expect(
        get(
            &format!("/api/v1/automation/rules/{rule_id}/runs"),
            Some(cookie),
        ),
        StatusCode::OK,
    )
    .await
    .rows()
}

#[tokio::test]
async fn a_transition_rule_comments_with_smart_values_and_logs_its_trace() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "AU", "programming").await;
    let id = rule(
        &app,
        &admin,
        "AU",
        "Congratulate",
        json!({
            "trigger": { "type": "cardTransitioned", "to": "Done" },
            "conditions": [{ "type": "field", "value": "{{card.summary}}", "op": "contains", "expected": "ship" }],
            "actions": [{ "type": "comment", "body": "{{card.key}} moved {{changelog.status.fromString}} → {{changelog.status.toString}} by {{initiator.displayName|\"someone\"}}" }],
        }),
    )
    .await;
    let shipped = card(&app, &admin, &p, "Ship the release").await;
    let other = card(&app, &admin, &p, "Something else").await;
    drain(&app).await;

    for key in [&shipped, &other] {
        app.expect(
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "statusId": p.status("Done") }),
            ),
            StatusCode::OK,
        )
        .await;
    }
    drain(&app).await;

    let said = comments(&app, &admin, &shipped).await;
    assert_eq!(
        said,
        vec![format!("{shipped} moved To Do → Done by Administrator")]
    );
    assert_eq!(
        comments(&app, &admin, &other).await.len(),
        0,
        "the condition gated it"
    );

    let log = runs(&app, &admin, &id).await;
    let statuses: Vec<&str> = log.iter().filter_map(|r| r["status"].as_str()).collect();
    assert!(
        statuses.contains(&"success") && statuses.contains(&"no_match"),
        "{statuses:?}"
    );
    let success = log
        .iter()
        .find(|r| r["status"] == "success")
        .expect("a success");
    let resolutions = success["trace"]["resolutions"]
        .as_array()
        .expect("resolutions");
    assert!(
        resolutions
            .iter()
            .any(|r| r["expr"] == "card.key" && r["value"] == shipped.as_str())
    );

    // The comment is attributed to the rule's owner, with an automation actor.
    let history: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT body, actor FROM comments WHERE body LIKE '%moved%'")
            .fetch_all(app.db.reader())
            .await
            .expect("query");
    assert!(
        history[0]
            .1
            .as_deref()
            .is_some_and(|a| a.starts_with(&format!("automation:{id}:1")))
    );
}

#[tokio::test]
async fn two_rules_that_trigger_each_other_stop_at_the_depth_cap() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "LOOP", "blank").await;
    // Ping changes the summary, which fires pong, which changes it back, which fires ping…
    let ping = rule(
        &app,
        &admin,
        "LOOP",
        "Ping",
        json!({
            "trigger": { "type": "fieldChanged", "fields": ["summary"] },
            "conditions": [{ "type": "field", "value": "{{card.summary}}", "op": "notEquals", "expected": "pong" }],
            "actions": [{ "type": "edit", "summary": "pong" }],
        }),
    )
    .await;
    rule(
        &app,
        &admin,
        "LOOP",
        "Pong",
        json!({
            "trigger": { "type": "fieldChanged", "fields": ["summary"] },
            "conditions": [{ "type": "field", "value": "{{card.summary}}", "op": "equals", "expected": "pong" }],
            "actions": [{ "type": "edit", "summary": "ping" }],
        }),
    )
    .await;
    let key = card(&app, &admin, &p, "start").await;
    drain(&app).await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "summary": "go" }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;

    let log = runs(&app, &admin, &ping).await;
    assert!(
        log.iter().any(|r| r["status"] == "loop_blocked") || {
            // Pong may be the one that hits the cap; check both rules' logs.
            let all: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM automation_runs WHERE status = 'loop_blocked'",
            )
            .fetch_one(app.db.reader())
            .await
            .expect("count");
            all > 0
        },
        "the chain must end in a loop_blocked run: {log:?}"
    );
    let changes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM card_history WHERE field = 'summary'")
            .fetch_one(app.db.reader())
            .await
            .expect("count");
    assert!(changes <= 7, "bounded: {changes} summary changes");
}

#[tokio::test]
async fn the_seeded_job_search_rule_nudges_stale_applications() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "JOB", "job-search").await;
    let rules = app
        .expect(
            get("/api/v1/projects/JOB/automation/rules", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["spec"]["trigger"]["type"], "scheduled");

    let stale = card(&app, &admin, &p, "Acme").await;
    let fresh = card(&app, &admin, &p, "Globex").await;
    for key in [&stale, &fresh] {
        app.expect(
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "statusId": p.status("Applied") }),
            ),
            StatusCode::OK,
        )
        .await;
    }
    // Age one application by eight days, and make the rule due.
    let eight_days_ago = (atlas::test_support::now() - chrono::Duration::days(8)).to_rfc3339();
    sqlx::query("UPDATE cards SET updated_at = ? WHERE key = ?")
        .bind(&eight_days_ago)
        .bind(&stale)
        .execute(app.db.writer())
        .await
        .expect("age");
    sqlx::query("UPDATE automation_rules SET next_run_at = ?")
        .bind(&eight_days_ago)
        .execute(app.db.writer())
        .await
        .expect("due");

    let ran = atlas::automation::engine::run_due(&app.state(), atlas::test_support::now())
        .await
        .expect("run");
    assert_eq!(ran, 1);
    let said = comments(&app, &admin, &stale).await;
    let log: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT status, trace, error FROM automation_runs")
            .fetch_all(app.db.reader())
            .await
            .expect("runs");
    assert_eq!(said.len(), 1, "{log:?}");
    assert!(said[0].contains("follow up with Acme"), "{said:?}");
    assert!(read_card(&app, &admin, &stale).await["dueDate"].is_string());
    assert_eq!(comments(&app, &admin, &fresh).await.len(), 0);

    // It is not due again for a day.
    let ran = atlas::automation::engine::run_due(&app.state(), atlas::test_support::now())
        .await
        .expect("run");
    assert_eq!(ran, 0);
}

#[tokio::test]
async fn the_seeded_programming_rule_routes_critical_bugs_to_the_lead() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "SW", "programming").await;
    let admin_id = app
        .expect(get("/api/v1/auth/me", Some(&admin)), StatusCode::OK)
        .await
        .id();
    let types = app
        .expect(
            get("/api/v1/projects/SW/card-types", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let bug = types
        .iter()
        .find(|t| t["name"] == "Bug")
        .map(|t| text(t, "id"))
        .expect("Bug");
    let priorities = app
        .expect(
            get("/api/v1/projects/SW/priorities", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let highest = priorities
        .iter()
        .find(|t| t["name"] == "Highest")
        .map(|t| text(t, "id"))
        .expect("Highest");
    let key = card(&app, &admin, &p, "Prod is down").await;
    drain(&app).await;

    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "typeId": bug, "priorityId": highest }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    assert_eq!(
        read_card(&app, &admin, &key).await["assigneeId"],
        admin_id.as_str()
    );
}

#[tokio::test]
async fn manual_runs_dry_runs_and_webhooks() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "MAN", "blank").await;
    let key = card(&app, &admin, &p, "Target").await;

    let spec = json!({
        "trigger": { "type": "manual" },
        "actions": [
            { "type": "setVariable", "name": "who", "value": "{{initiator.username}}" },
            { "type": "addTag", "name": "touched-by-{{vars.who}}" },
            { "type": "createChildren", "summaries": ["Step one", "Step two"] },
        ],
    });

    // A dry run renders and reports, and changes nothing.
    let dry = app
        .expect(
            post(
                "/api/v1/projects/MAN/automation/dry-run",
                Some(&admin),
                json!({ "spec": spec, "cardKey": key }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(dry["status"], "dry_run", "{dry}");
    let children = app
        .expect(
            get(&format!("/api/v1/cards/{key}/children"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(children.as_array().map_or(0, Vec::len), 0);

    let id = rule(&app, &admin, "MAN", "Prepare", spec).await;
    let result = app
        .expect(
            post(
                &format!("/api/v1/automation/rules/{id}/run"),
                Some(&admin),
                json!({ "cardKey": key }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(result["status"], "success", "{result}");
    let tags = app
        .expect(
            get(&format!("/api/v1/cards/{key}/tags"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(tags[0]["name"], "touched-by-Admin");
    let children = app
        .expect(
            get(&format!("/api/v1/cards/{key}/children"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(children.as_array().map_or(0, Vec::len), 2, "{children}");

    // A webhook rule: a token is issued once; the public receiver needs it.
    let hook = rule(
        &app,
        &admin,
        "MAN",
        "Inbound",
        json!({
            "trigger": { "type": "incomingWebhook", "aql": format!("key = {key}") },
            "actions": [{ "type": "comment", "body": "CI says {{webhook.body.state}}" }],
        }),
    )
    .await;
    let token = app
        .expect(
            post_empty(
                &format!("/api/v1/automation/rules/{hook}/webhook-token"),
                Some(&admin),
            ),
            StatusCode::OK,
        )
        .await
        .text("token");
    let deliver = |token: &str| {
        Request::builder()
            .method(Method::POST)
            .uri(format!("/hooks/automation/{token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(r#"{"state":"green"}"#))
            .expect("request")
    };
    app.expect(deliver(&"0".repeat(64)), StatusCode::NOT_FOUND)
        .await;
    app.expect(deliver("not-a-token"), StatusCode::NOT_FOUND)
        .await;
    app.expect(deliver(&token), StatusCode::ACCEPTED).await;
    assert_eq!(
        comments(&app, &admin, &key).await,
        vec!["CI says green".to_owned()]
    );
}

#[tokio::test]
async fn defining_rules_is_owner_work_and_invalid_rules_are_refused() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "OWN", "blank").await;
    let (mia_id, mia) = user(&app, &admin, "mia", "member").await;
    grant(&app, &admin, "OWN", &mia_id, "member").await;
    let spec =
        json!({ "trigger": { "type": "manual" }, "actions": [{ "type": "comment", "body": "x" }] });
    app.expect(
        post(
            "/api/v1/projects/OWN/automation/rules",
            Some(&mia),
            json!({ "name": "nope", "spec": spec }),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    for bad in [
        json!({ "trigger": { "type": "manual" }, "actions": [] }),
        json!({ "trigger": { "type": "scheduled", "intervalMinutes": 1 }, "actions": [{ "type": "comment", "body": "x" }] }),
        json!({ "trigger": { "type": "manual" }, "conditions": [{ "type": "regex", "value": "x", "pattern": "(" }], "actions": [{ "type": "comment", "body": "x" }] }),
    ] {
        app.expect(
            post(
                "/api/v1/projects/OWN/automation/rules",
                Some(&admin),
                json!({ "name": "bad", "spec": bad }),
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
}

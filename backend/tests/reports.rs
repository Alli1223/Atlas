//! End-to-end tests for reports and dashboards: the daily snapshot (and its one-time
//! backfill), every project report, cycle burndown and report through a real start/complete,
//! the AQL-driven aggregates with access scoping, the cost report, and dashboard privacy.

mod common;

use atlas::domain::snapshot;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn chart(app: &App, cookie: &str, path: &str) -> Value {
    app.expect(get(path, Some(cookie)), StatusCode::OK)
        .await
        .json()
}

fn last(series: &Value, id: &str) -> f64 {
    series["series"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["id"] == id))
        .and_then(|x| x["values"].as_array())
        .and_then(|v| v.last())
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("no series {id} in {series}"))
}

#[tokio::test]
// These assert against exact small integer counts straight from the daily snapshot
// aggregation, never accumulated float arithmetic, so exact comparison is the right check.
#[allow(clippy::float_cmp)]
async fn flow_reports_are_group_bys_over_the_daily_snapshot() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "FLOW", "job-search").await;
    let a = card(&app, &admin, &p, "Acme").await;
    let _b = card(&app, &admin, &p, "Globex").await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{a}"),
            Some(&admin),
            json!({ "statusId": p.status("Rejected") }),
        ),
        StatusCode::OK,
    )
    .await;

    // The first run backfills, then writes today.
    let written = snapshot::take(&app.db, atlas::test_support::now())
        .await
        .expect("snapshot");
    assert_eq!(written, 2);
    // Idempotent within a day.
    snapshot::take(&app.db, atlas::test_support::now())
        .await
        .expect("snapshot");

    let cfd = chart(
        &app,
        &admin,
        "/api/v1/projects/FLOW/reports/cumulative-flow?days=7",
    )
    .await;
    assert_eq!(cfd["days"].as_array().map(Vec::len), Some(7));
    assert_eq!(last(&cfd, &p.status("Rejected")), 1.0);
    assert_eq!(last(&cfd, &p.status("Interested")), 1.0);

    let cvr = chart(
        &app,
        &admin,
        "/api/v1/projects/FLOW/reports/created-vs-resolved?days=7",
    )
    .await;
    assert_eq!(last(&cvr, "createdTotal"), 2.0);
    assert_eq!(last(&cvr, "resolvedTotal"), 1.0);

    let age = chart(
        &app,
        &admin,
        "/api/v1/projects/FLOW/reports/average-age?days=7",
    )
    .await;
    assert_eq!(last(&age, "age"), 0.0, "created today, so zero days old");

    let control = chart(&app, &admin, "/api/v1/projects/FLOW/reports/control-chart").await;
    assert_eq!(control["points"].as_array().map(Vec::len), Some(1));

    // A job search has no cycles: its cycle reports refuse rather than lie.
    app.expect(
        get("/api/v1/projects/FLOW/reports/velocity", Some(&admin)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one cycle's whole life: plan, start, creep, finish, reopen
async fn a_cycle_has_a_burndown_and_a_report_that_survive_its_completion() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "CYC", "programming").await;
    let done = card(&app, &admin, &p, "Will finish").await;
    let open = card(&app, &admin, &p, "Will carry over").await;
    let cycle = app
        .expect(
            post(
                "/api/v1/projects/CYC/cycles",
                Some(&admin),
                json!({ "name": "Sprint 1" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    for key in [&done, &open] {
        app.expect(
            post(
                &format!("/api/v1/cards/{key}/cycle"),
                Some(&admin),
                json!({ "cycleId": cycle }),
            ),
            StatusCode::NO_CONTENT,
        )
        .await;
        app.expect(
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "estimate": 3 }),
            ),
            StatusCode::OK,
        )
        .await;
    }
    let today = atlas::test_support::now().date_naive();
    app.expect(
        post(
            &format!("/api/v1/cycles/{cycle}/start"),
            Some(&admin),
            json!({ "startDate": today.to_string(), "endDate": (today + chrono::Duration::days(13)).to_string() }),
        ),
        StatusCode::OK,
    )
    .await;
    // A card added after the start is scope creep.
    let late = card(&app, &admin, &p, "Added late").await;
    app.expect(
        post(
            &format!("/api/v1/cards/{late}/cycle"),
            Some(&admin),
            json!({ "cycleId": cycle }),
        ),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{done}"),
            Some(&admin),
            json!({ "statusId": p.status("Done") }),
        ),
        StatusCode::OK,
    )
    .await;
    atlas::domain::cycle_snapshot::take(&app.db, atlas::test_support::now())
        .await
        .expect("daily");

    let burn = chart(&app, &admin, &format!("/api/v1/cycles/{cycle}/burndown")).await;
    assert_eq!(burn["unit"], "points");
    assert_eq!(
        burn["ideal"][0], 6.0,
        "the commitment: two cards of 3 points"
    );
    assert_eq!(burn["remaining"][0], 3.0, "{burn}");

    app.expect(
        post(
            &format!("/api/v1/cycles/{cycle}/complete"),
            Some(&admin),
            json!({ "carryTo": { "kind": "backlog" } }),
        ),
        StatusCode::OK,
    )
    .await;
    // Reopening the finished card afterwards must not rewrite what the cycle completed.
    app.expect(
        patch(
            &format!("/api/v1/cards/{done}"),
            Some(&admin),
            json!({ "statusId": p.status("To Do") }),
        ),
        StatusCode::OK,
    )
    .await;

    let report = chart(&app, &admin, &format!("/api/v1/cycles/{cycle}/report")).await;
    assert_eq!(report["committed"], 6.0, "{report}");
    assert_eq!(report["completed"], 3.0, "{report}");
    assert_eq!(
        report["added"], 0.0,
        "the late card has no estimate: {report}"
    );
    let late_row = report["cards"]
        .as_array()
        .and_then(|c| c.iter().find(|x| x["key"] == late.as_str()))
        .expect("late card");
    assert_eq!(late_row["added"], true);
    assert_eq!(late_row["committed"], false);

    let velocity = chart(&app, &admin, "/api/v1/projects/CYC/reports/velocity").await;
    assert_eq!(velocity["cycles"][0]["committed"], 6.0, "{velocity}");
    assert_eq!(velocity["cycles"][0]["completed"], 3.0, "{velocity}");
}

#[tokio::test]
async fn aggregates_only_count_what_the_caller_can_see() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let mine = project(&app, &admin, "MINE", "blank").await;
    let secret = project(&app, &admin, "SECRET", "blank").await;
    card(&app, &admin, &mine, "One").await;
    card(&app, &admin, &mine, "Two").await;
    card(&app, &admin, &secret, "Hidden").await;
    let (vic_id, vic) = user(&app, &admin, "vic", "member").await;
    grant(&app, &admin, "MINE", &vic_id, "viewer").await;

    let all = app
        .expect(
            post(
                "/api/v1/reports/aggregate",
                Some(&admin),
                json!({ "aql": "", "groupBy": "project" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(all["cards"], 3);
    let theirs = app
        .expect(
            post(
                "/api/v1/reports/aggregate",
                Some(&vic),
                json!({ "aql": "", "groupBy": "project" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(theirs["cards"], 2, "{theirs}");
    assert_eq!(theirs["buckets"].as_array().map(Vec::len), Some(1));

    let grid = app
        .expect(
            post(
                "/api/v1/reports/two-dimensional",
                Some(&admin),
                json!({ "aql": "", "x": "status", "y": "project" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    let total: i64 = grid["cells"]
        .as_array()
        .expect("rows")
        .iter()
        .flat_map(|r| r.as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_i64())
        .sum();
    assert_eq!(total, 3);

    app.expect(
        post(
            "/api/v1/reports/aggregate",
            Some(&admin),
            json!({ "aql": "", "groupBy": "password_hash" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let costs = app
        .expect(
            get("/api/v1/reports/agent-costs", Some(&vic)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(costs["totalUsd"], 0.0);
}

#[tokio::test]
async fn dashboards_are_private_unless_shared_and_only_the_owner_edits() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (_, eve) = user(&app, &admin, "eve", "member").await;
    let body = json!({
        "name": "Mine",
        "gadgets": [{ "id": "g1", "kind": "pie", "title": "By status", "config": { "aql": "", "groupBy": "status" }, "x": 0, "y": 0, "w": 6, "h": 4 }],
    });
    let id = app
        .expect(
            post("/api/v1/dashboards", Some(&admin), body.clone()),
            StatusCode::CREATED,
        )
        .await
        .id();
    app.expect(
        get(&format!("/api/v1/dashboards/{id}"), Some(&eve)),
        StatusCode::NOT_FOUND,
    )
    .await;

    let mut shared = body.clone();
    shared["shared"] = json!(true);
    app.expect(
        put(
            &format!("/api/v1/dashboards/{id}"),
            Some(&admin),
            shared.clone(),
        ),
        StatusCode::OK,
    )
    .await;
    let seen = app
        .expect(
            get(&format!("/api/v1/dashboards/{id}"), Some(&eve)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(seen["editable"], false);
    app.expect(
        put(&format!("/api/v1/dashboards/{id}"), Some(&eve), shared),
        StatusCode::NOT_FOUND,
    )
    .await;
    app.expect(
        delete(&format!("/api/v1/dashboards/{id}"), Some(&eve)),
        StatusCode::NOT_FOUND,
    )
    .await;

    let mut bad = body;
    bad["gadgets"][0]["x"] = json!(10);
    app.expect(
        post("/api/v1/dashboards", Some(&admin), bad),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

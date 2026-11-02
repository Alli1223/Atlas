//! Phase 18 end to end: bulk operations (preview, then apply), CSV import and export, the
//! project bundle, card templates and the trash.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn status_of(app: &App, admin: &str, key: &str) -> String {
    app.expect(
        get(&format!("/api/v1/cards/{key}"), Some(admin)),
        StatusCode::OK,
    )
    .await
    .text("statusId")
}

fn done_name(p: &Project) -> String {
    p.statuses
        .iter()
        .find(|s| s["category"] == "done")
        .map(|s| text(s, "name"))
        .expect("a done status")
}

async fn tags_of(app: &App, admin: &str, key: &str) -> Vec<String> {
    app.expect(
        get(&format!("/api/v1/cards/{key}/tags"), Some(admin)),
        StatusCode::OK,
    )
    .await
    .rows()
    .iter()
    .map(|t| text(t, "name"))
    .collect()
}

#[tokio::test]
async fn a_bulk_preview_saves_nothing_and_apply_matches_it() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "BLK", "programming").await;
    let a = card(&app, &admin, &p, "One").await;
    let b = card(&app, &admin, &p, "Two").await;
    let before = status_of(&app, &admin, &a).await;
    let done = done_name(&p);

    let body = json!({
        "keys": [a, b, "BLK-999"],
        "operation": { "kind": "transition", "status": done },
    });
    let preview = app
        .expect(
            post("/api/v1/bulk", Some(&admin), body.clone()),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["changed"], 2, "{preview}");
    assert_eq!(preview["failed"], 1);
    assert_eq!(preview["cards"][2]["error"], "not found");
    assert_eq!(
        status_of(&app, &admin, &a).await,
        before,
        "a preview saves nothing"
    );

    let mut apply = body;
    apply["preview"] = json!(false);
    let applied = app
        .expect(post("/api/v1/bulk", Some(&admin), apply), StatusCode::OK)
        .await
        .json();
    assert_eq!(applied["changed"], 2);
    assert_eq!(status_of(&app, &admin, &a).await, p.status_in("done"));

    // Running it again changes nothing.
    let again = app
        .expect(
            post(
                "/api/v1/bulk",
                Some(&admin),
                json!({ "keys": [a], "operation": { "kind": "transition", "status": done }, "preview": false }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(again["unchanged"], 1, "{again}");
}

#[tokio::test]
async fn bulk_tag_delete_and_restore_through_the_trash() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "TRS", "blank").await;
    let a = card(&app, &admin, &p, "One").await;
    let b = card(&app, &admin, &p, "Two").await;

    let run = |operation: Value| {
        post(
            "/api/v1/bulk",
            Some(&admin),
            json!({ "keys": [a, b], "operation": operation, "preview": false }),
        )
    };
    app.expect(
        run(json!({ "kind": "tag", "name": "urgent" })),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tags_of(&app, &admin, &a).await, vec!["urgent".to_owned()]);
    app.expect(
        run(json!({ "kind": "untag", "name": "urgent" })),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tags_of(&app, &admin, &b).await, Vec::<String>::new());

    app.expect(run(json!({ "kind": "delete" })), StatusCode::OK)
        .await;
    let trash = app
        .expect(
            get("/api/v1/projects/TRS/trash", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(trash.len(), 2);

    // A deleted card can only be restored.
    let edit = app
        .expect(
            run(json!({ "kind": "edit", "dueDate": "2027-01-01" })),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(edit["failed"], 2, "{edit}");

    app.expect(run(json!({ "kind": "restore" })), StatusCode::OK)
        .await;
    let trash = app
        .expect(
            get("/api/v1/projects/TRS/trash", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(trash.len(), 0);
}

#[tokio::test]
async fn bulk_cannot_reach_a_card_the_caller_cannot_edit() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "PRV", "blank").await;
    let secret = card(&app, &admin, &p, "Private").await;
    let (viewer_id, viewer) = user(&app, &admin, "vera", "member").await;

    let body = json!({ "keys": [secret], "operation": { "kind": "delete" }, "preview": false });
    let report = app
        .expect(
            post("/api/v1/bulk", Some(&viewer), body.clone()),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(
        report["cards"][0]["error"], "not found",
        "an outsider cannot probe keys"
    );

    grant(&app, &admin, "PRV", &viewer_id, "viewer").await;
    let report = app
        .expect(post("/api/v1/bulk", Some(&viewer), body), StatusCode::OK)
        .await
        .json();
    assert_eq!(report["failed"], 1);
    assert_eq!(report["cards"][0]["error"], "you cannot edit this card");

    let too_many: Vec<String> = (0..1001).map(|n| format!("PRV-{n}")).collect();
    app.expect(
        post(
            "/api/v1/bulk",
            Some(&admin),
            json!({ "keys": too_many, "operation": { "kind": "delete" } }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

const APPLICATIONS: &str = "Company,Status,Labels,Labels,Due date\r\n\
Acme,Applied,remote,dream,2026-12-01\r\n\
\"Globex, Inc\",Interested,,,\r\n";

#[tokio::test]
async fn a_job_search_spreadsheet_imports_all_or_nothing() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "JOB", "job-search").await;

    let plan = app
        .expect(
            post(
                "/api/v1/projects/JOB/import/csv/preview",
                Some(&admin),
                json!({ "csv": APPLICATIONS }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(plan["total"], 2, "{plan}");
    assert_eq!(plan["problems"], json!([]), "{plan}");
    assert_eq!(plan["mapping"][0], "summary");

    // One bad row refuses the whole file.
    let broken = format!("{APPLICATIONS}Initech,Nope,,,\r\n");
    let refused = app
        .send(post(
            "/api/v1/projects/JOB/import/csv",
            Some(&admin),
            json!({ "csv": broken }),
        ))
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.raw_body
    );
    let problems = refused.json()["problems"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(problems.len(), 1, "{}", refused.raw_body);
    assert_eq!(problems[0]["row"], 3);
    let board = app
        .expect(
            post(
                "/api/v1/search",
                Some(&admin),
                json!({ "aql": "project = JOB" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(board["total"], 0, "nothing was half-imported");

    let imported = app
        .expect(
            post(
                "/api/v1/projects/JOB/import/csv",
                Some(&admin),
                json!({ "csv": APPLICATIONS }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    let keys = imported["keys"].as_array().cloned().unwrap_or_default();
    assert_eq!(keys.len(), 2);
    let first = keys[0].as_str().unwrap_or_default().to_owned();
    let acme = app
        .expect(
            get(&format!("/api/v1/cards/{first}"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(acme["summary"], "Acme");
    assert_eq!(acme["dueDate"], "2026-12-01");
    let mut labels = tags_of(&app, &admin, &first).await;
    labels.sort();
    assert_eq!(labels, vec!["dream".to_owned(), "remote".to_owned()]);
}

#[tokio::test]
async fn a_csv_export_reimports_and_is_scoped_by_access() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "EXP", "blank").await;
    card(&app, &admin, &p, "=HYPERLINK(\"evil\")").await;
    card(&app, &admin, &p, "Plain, with a comma").await;

    let exported = app
        .expect(
            get("/api/v1/export/csv?aql=project%20%3D%20EXP", Some(&admin)),
            StatusCode::OK,
        )
        .await;
    assert!(
        exported
            .header(axum::http::header::CONTENT_TYPE)
            .starts_with("text/csv")
    );
    let csv = exported.raw_body.clone();
    assert!(
        csv.starts_with("Issue key,Issue ID,Parent,Summary"),
        "{csv}"
    );
    assert!(csv.contains("\"Plain, with a comma\""), "{csv}");
    assert!(csv.contains("'=HYPERLINK"), "a formula is defused: {csv}");

    // Someone without access exports nothing.
    let (_, stranger) = user(&app, &admin, "sam", "member").await;
    let theirs = app
        .expect(
            get(
                "/api/v1/export/csv?aql=project%20%3D%20EXP",
                Some(&stranger),
            ),
            StatusCode::OK,
        )
        .await;
    assert_eq!(theirs.raw_body.lines().count(), 1, "{}", theirs.raw_body);

    // The export is the importer's own format.
    project(&app, &admin, "BACK", "blank").await;
    let imported = app
        .expect(
            post(
                "/api/v1/projects/BACK/import/csv",
                Some(&admin),
                json!({ "csv": csv }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(
        imported["keys"].as_array().map(Vec::len),
        Some(2),
        "{imported}"
    );
}

/// A programming project `SRC` with a comment, a link and a tag on its first card.
async fn source_project(app: &App, admin: &str) {
    let p = project(app, admin, "SRC", "programming").await;
    let a = card(app, admin, &p, "With a comment").await;
    let b = card(app, admin, &p, "Linked").await;
    app.expect(
        post(
            &format!("/api/v1/cards/{a}/comments"),
            Some(admin),
            json!({ "body": "Remember the edge case." }),
        ),
        StatusCode::CREATED,
    )
    .await;
    app.expect(
        post(
            &format!("/api/v1/cards/{a}/links"),
            Some(admin),
            json!({ "targetKey": b, "linkType": "blocks" }),
        ),
        StatusCode::OK,
    )
    .await;
    app.expect(
        post(
            "/api/v1/bulk",
            Some(admin),
            json!({ "keys": [a], "operation": { "kind": "tag", "name": "keep" }, "preview": false }),
        ),
        StatusCode::OK,
    )
    .await;
}

fn names(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|r| text(r, "name")).collect()
}

#[tokio::test]
async fn a_bundle_round_trips_a_project() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    source_project(&app, &admin).await;
    let bundle = app
        .expect(
            get("/api/v1/projects/SRC/export", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(bundle["format"], "atlas.project/1");
    assert_eq!(bundle["cards"].as_array().map(Vec::len), Some(2));

    let copy = app
        .expect(
            post(
                "/api/v1/projects/import",
                Some(&admin),
                json!({ "bundle": bundle, "key": "COPY" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(copy["key"], "COPY");
    let found = app
        .expect(
            post(
                "/api/v1/search",
                Some(&admin),
                json!({ "aql": "project = COPY ORDER BY created ASC" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(found["total"], 2, "{found}");
    let copied = text(&found["cards"][0], "key");
    let comments = app
        .expect(
            get(&format!("/api/v1/cards/{copied}/comments"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(comments.len(), 1);
    assert_eq!(
        tags_of(&app, &admin, &copied).await,
        vec!["keep".to_owned()]
    );
    let links = app
        .expect(
            get(&format!("/api/v1/cards/{copied}/links"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(links.len(), 1, "the link came along, between the copies");

    // Same configuration, by name.
    let source_statuses = names(
        &app.expect(
            get("/api/v1/projects/SRC/statuses", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows(),
    );
    let copy_statuses = names(
        &app.expect(
            get("/api/v1/projects/COPY/statuses", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows(),
    );
    assert_eq!(source_statuses, copy_statuses);
}

#[tokio::test]
async fn copying_config_alone_and_refusals() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    source_project(&app, &admin).await;

    // "Copy config from project": no cards, same templates.
    app.expect(
        post(
            "/api/v1/projects/SRC/duplicate",
            Some(&admin),
            json!({ "key": "CONF", "name": "Config only" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    let found = app
        .expect(
            post(
                "/api/v1/search",
                Some(&admin),
                json!({ "aql": "project = CONF" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(found["total"], 0);
    let templates = app
        .expect(
            get("/api/v1/projects/CONF/card-templates", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(names(&templates), vec!["Bug report".to_owned()]);

    // A taken key and a foreign document are refused.
    let bundle = app
        .expect(
            get(
                "/api/v1/projects/SRC/export?includeCards=false",
                Some(&admin),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(bundle["cards"], json!([]));
    let mut again = json!({ "bundle": bundle, "key": "CONF" });
    app.expect(
        post("/api/v1/projects/import", Some(&admin), again.clone()),
        StatusCode::CONFLICT,
    )
    .await;
    again["key"] = json!("ELSE");
    again["bundle"]["format"] = json!("something/9");
    app.expect(
        post("/api/v1/projects/import", Some(&admin), again),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

#[tokio::test]
async fn card_templates_are_project_configuration() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "TPL", "job-search").await;
    let seeded = app
        .expect(
            get("/api/v1/projects/TPL/card-templates", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(seeded.len(), 1);
    assert!(text(&seeded[0], "description").contains("- [ ] Tailor CV"));

    let created = app
        .expect(
            post(
                "/api/v1/projects/TPL/card-templates",
                Some(&admin),
                json!({ "name": "Recruiter call", "summary": "Call with ", "labels": ["call"] }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    let id = text(&created, "id");
    app.expect(
        post(
            "/api/v1/projects/TPL/card-templates",
            Some(&admin),
            json!({ "name": "Recruiter call" }),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let replaced = app
        .expect(
            put(
                &format!("/api/v1/card-templates/{id}"),
                Some(&admin),
                json!({ "name": "Recruiter screen", "summary": "Screen with " }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(replaced["name"], "Recruiter screen");

    // A member may read templates but not change them.
    let (member_id, member) = user(&app, &admin, "mo", "member").await;
    grant(&app, &admin, "TPL", &member_id, "member").await;
    app.expect(
        get("/api/v1/projects/TPL/card-templates", Some(&member)),
        StatusCode::OK,
    )
    .await;
    app.expect(
        delete(&format!("/api/v1/card-templates/{id}"), Some(&member)),
        StatusCode::FORBIDDEN,
    )
    .await;
    app.expect(
        delete(&format!("/api/v1/card-templates/{id}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
}

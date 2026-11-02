//! End-to-end tests for custom fields: the global registry, per-project layouts, typed values,
//! their history, defaults on create, and AQL over them (`customField()`, `milestone`,
//! `component`).

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn search(app: &App, cookie: &str, aql: &str) -> Vec<String> {
    let reply = app
        .expect(
            post("/api/v1/search", Some(cookie), json!({ "aql": aql })),
            StatusCode::OK,
        )
        .await
        .json();
    let mut keys: Vec<String> = reply["cards"]
        .as_array()
        .unwrap_or_else(|| panic!("no cards in {reply}"))
        .iter()
        .map(|c| text(c, "key"))
        .collect();
    keys.sort();
    keys
}

async fn define(app: &App, admin: &str, body: Value) -> String {
    app.expect(
        post("/api/v1/custom-fields", Some(admin), body),
        StatusCode::CREATED,
    )
    .await
    .id()
}

#[tokio::test]
async fn defining_a_field_is_admin_only_and_names_are_global() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (_, member) = user(&app, &admin, "mo", "member").await;

    app.expect(
        post(
            "/api/v1/custom-fields",
            Some(&member),
            json!({ "name": "Company", "fieldType": "text" }),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    define(
        &app,
        &admin,
        json!({ "name": "Company", "fieldType": "text" }),
    )
    .await;
    app.expect(
        post(
            "/api/v1/custom-fields",
            Some(&admin),
            json!({ "name": "company", "fieldType": "number" }),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    // A choice field needs options.
    app.expect(
        post(
            "/api/v1/custom-fields",
            Some(&admin),
            json!({ "name": "Mode", "fieldType": "select" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // Anyone signed in can read the registry.
    let list = app
        .expect(get("/api/v1/custom-fields", Some(&member)), StatusCode::OK)
        .await
        .rows();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, end to end: define, lay out, set, refuse, query
async fn values_are_typed_recorded_in_history_and_queryable_across_projects() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let jobs = project(&app, &admin, "JOBS", "job-search").await;
    let others = project(&app, &admin, "MORE", "job-search").await;

    let company = define(
        &app,
        &admin,
        json!({ "name": "Company", "fieldType": "text" }),
    )
    .await;
    let work_mode = define(
        &app,
        &admin,
        json!({ "name": "Work mode", "fieldType": "select", "options": ["Remote", "Hybrid", "Office"] }),
    )
    .await;
    let salary = define(
        &app,
        &admin,
        json!({ "name": "Salary", "fieldType": "number" }),
    )
    .await;

    for p in [&jobs, &others] {
        app.expect(
            put(
                &format!("/api/v1/projects/{}/field-layout", p.key),
                Some(&admin),
                json!({ "entries": [
                    { "fieldId": company, "required": true },
                    { "fieldId": work_mode, "defaultValue": "Remote" },
                    { "fieldId": salary },
                ] }),
            ),
            StatusCode::OK,
        )
        .await;
    }

    let acme = card(&app, &admin, &jobs, "Acme").await;
    let globex = card(&app, &admin, &others, "Globex").await;

    // The layout default landed at create.
    let fields = app
        .expect(
            get(&format!("/api/v1/cards/{acme}/fields"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let mode_value = fields
        .iter()
        .find(|f| f["entry"]["field"]["id"] == work_mode.as_str())
        .map(|f| f["value"].clone());
    assert_eq!(mode_value, Some(json!("Remote")));

    app.expect(
        patch(
            &format!("/api/v1/cards/{acme}/fields"),
            Some(&admin),
            json!({ "values": { company.clone(): "Acme Corp", salary.clone(): "85000" } }),
        ),
        StatusCode::OK,
    )
    .await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{globex}/fields"),
            Some(&admin),
            json!({ "values": { company.clone(): "Globex", work_mode.clone(): "hybrid" } }),
        ),
        StatusCode::OK,
    )
    .await;

    // Bad values and clearing a required field are refused, atomically.
    for body in [
        json!({ salary.clone(): "lots" }),
        json!({ work_mode.clone(): "Moon" }),
        json!({ company.clone(): null }),
        json!({ "not-a-field": 1 }),
    ] {
        app.expect(
            patch(
                &format!("/api/v1/cards/{acme}/fields"),
                Some(&admin),
                json!({ "values": body }),
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }

    // History records the change under the field's id, with display values.
    let history = app
        .expect(
            get(&format!("/api/v1/cards/{acme}/history"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let row = history
        .iter()
        .find(|h| h["field"] == format!("cf:{company}"))
        .unwrap_or_else(|| panic!("no custom field history in {history:?}"));
    assert_eq!(row["toDisplay"], "Acme Corp");

    // One field, every project.
    assert_eq!(
        search(&app, &admin, r#"key IN customField("Company")"#)
            .await
            .len(),
        2
    );
    assert_eq!(
        search(&app, &admin, r#"key IN customField("work mode", "HYBRID")"#).await,
        vec![globex.clone()]
    );
    assert_eq!(
        search(
            &app,
            &admin,
            r#"key IN customField("Company", "acme corp")"#
        )
        .await,
        vec![acme.clone()]
    );

    // An option still in use cannot be removed.
    app.expect(
        patch(
            &format!("/api/v1/custom-fields/{work_mode}"),
            Some(&admin),
            json!({ "options": ["Remote", "Office"] }),
        ),
        StatusCode::CONFLICT,
    )
    .await;
}

#[tokio::test]
async fn milestones_and_components_are_queryable() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "Q", "programming").await;
    let a = card(&app, &admin, &p, "A").await;
    let b = card(&app, &admin, &p, "B").await;

    let v1 = app
        .expect(
            post(
                "/api/v1/projects/Q/milestones",
                Some(&admin),
                json!({ "name": "v1" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    let ui = app
        .expect(
            post(
                "/api/v1/projects/Q/components",
                Some(&admin),
                json!({ "name": "UI" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    app.expect(
        patch(
            &format!("/api/v1/cards/{a}"),
            Some(&admin),
            json!({ "milestoneId": v1 }),
        ),
        StatusCode::OK,
    )
    .await;
    app.expect(
        put(
            &format!("/api/v1/cards/{b}/components"),
            Some(&admin),
            json!({ "componentIds": [ui] }),
        ),
        StatusCode::OK,
    )
    .await;

    assert_eq!(
        search(&app, &admin, "milestone = v1").await,
        vec![a.clone()]
    );
    assert_eq!(
        search(&app, &admin, "milestone IS EMPTY").await,
        vec![b.clone()]
    );
    assert_eq!(
        search(&app, &admin, "component = UI").await,
        vec![b.clone()]
    );
    assert_eq!(
        search(&app, &admin, "component IS EMPTY").await,
        vec![a.clone()]
    );
    assert_eq!(
        search(&app, &admin, "milestone CHANGED").await,
        vec![a.clone()]
    );
    assert_eq!(search(&app, &admin, "milestone WAS v1").await, vec![a]);
}

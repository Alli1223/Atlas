//! End-to-end workflow-engine tests, over the real router, the real middleware
//! stack, and a real database.
//!
//! `domain::workflow`'s unit tests prove the gate *parsing* in isolation. These
//! prove the execution contract itself — the thing only a running engine can
//! demonstrate, and the thing that would be a silent correctness failure rather
//! than a crash if it were wrong:
//!
//! - an illegal transition (no such edge) is rejected;
//! - a failing **condition** HIDES a transition (absent from the available list)
//!   and rejects a direct attempt;
//! - a failing **validator** REJECTS with the field named, leaves the status
//!   unchanged, and does **not** run post-functions;
//! - **`ChildBlocking`** stops a card entering Done while a child is open, and
//!   lets it once the children are done;
//! - a **`SetResolution`** post-function sets the resolution on a Done transition,
//!   and leaving Done clears it (via `card.rs`'s resolution rules);
//! - a **failing post-function rolls the whole transition back** — the card does
//!   not move;
//! - the **job-search** workflow round-trips cleanly, proving the engine is not
//!   secretly Scrum-shaped;
//! - **existing card moves still work** under the permissive default workflow.
//!
//! The `App` harness is lifted from `tests/domain.rs`, as its handoff intended.

use atlas::api::{self, AppState};
use atlas::auth::seed::DEFAULT_ADMIN_USERNAME;
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use atlas::{auth::seed, auth::session};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct App {
    db: Db,
    config: Config,
    _temp: TempDb,
}

impl App {
    async fn new() -> Self {
        let temp = TempDb::new();
        let config = temp.config();
        let db = Db::connect(&config).await.expect("failed to open database");
        db::migrate::run(&db).await.expect("failed to migrate");
        seed::ensure_default_admin(&db)
            .await
            .expect("failed to seed the default admin");
        Self {
            db,
            config,
            _temp: temp,
        }
    }

    fn router(&self) -> Router {
        api::router(AppState::new(self.db.clone(), self.config.clone()))
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self
            .router()
            .oneshot(request)
            .await
            .expect("request failed");
        Reply::from(response).await
    }
}

struct Reply {
    status: StatusCode,
    set_cookie: Vec<String>,
    raw_body: String,
}

impl Reply {
    async fn from(response: axum::response::Response) -> Self {
        let status = response.status();
        let set_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(ToOwned::to_owned)
            .collect();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("failed to read the body");
        Self {
            status,
            set_cookie,
            raw_body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.raw_body)
            .unwrap_or_else(|err| panic!("body was not JSON ({err}): {}", self.raw_body))
    }

    fn session_cookie(&self) -> Option<String> {
        self.set_cookie
            .iter()
            .find(|c| c.starts_with(session::COOKIE_NAME))
            .and_then(|c| c.split(';').next())
            .and_then(|c| c.split_once('='))
            .map(|(_, value)| value.to_owned())
    }

    fn id(&self) -> String {
        self.json()["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in: {}", self.raw_body))
            .to_owned()
    }

    fn key(&self) -> String {
        self.json()["key"]
            .as_str()
            .unwrap_or_else(|| panic!("no key in: {}", self.raw_body))
            .to_owned()
    }
}

fn request(method: Method, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
    }
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("failed to build the request"),
        None => builder
            .body(Body::empty())
            .expect("failed to build the request"),
    }
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::GET, uri, cookie, None)
}
fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::POST, uri, cookie, Some(body))
}

async fn admin_past_the_gate(app: &App) -> String {
    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": DEFAULT_ADMIN_USERNAME, "password": ADMIN_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let cookie = reply.session_cookie().expect("login must set a cookie");

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply
        .session_cookie()
        .expect("change-password must issue a new session")
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn rows(reply: &Reply) -> Vec<Value> {
    reply
        .json()
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array: {}", reply.raw_body))
        .clone()
}

fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("no string {field:?} in {value}"))
        .to_owned()
}

/// A project, with its statuses and default card type resolved by name.
struct Project {
    key: String,
    card_type: String,
    statuses: Vec<(String, String)>,
}

impl Project {
    fn status(&self, name: &str) -> String {
        self.statuses
            .iter()
            .find(|(_, n)| n == name)
            .map_or_else(|| panic!("no status {name:?}"), |(id, _)| id.clone())
    }
}

async fn project(app: &App, admin: &str, key: &str, template: &str) -> Project {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": template }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let types = rows(&app.send(get(&format!("/api/v1/projects/{key}/card-types"), Some(admin))).await);
    let card_type = types
        .iter()
        .find(|t| t["isDefault"].as_bool() == Some(true))
        .map_or_else(|| panic!("{key} has no default card type"), |t| text(t, "id"));

    let statuses = rows(&app.send(get(&format!("/api/v1/projects/{key}/statuses"), Some(admin))).await)
        .iter()
        .map(|s| (text(s, "id"), text(s, "name")))
        .collect();

    Project {
        key: key.to_owned(),
        card_type,
        statuses,
    }
}

/// Creates a card and returns its key.
async fn card(app: &App, admin: &str, project: &Project, summary: &str) -> String {
    card_of_type(app, admin, project, &project.card_type, summary, None).await
}

/// Creates a card of a given type, optionally under a parent, returning its key.
async fn card_of_type(
    app: &App,
    admin: &str,
    project: &Project,
    type_id: &str,
    summary: &str,
    parent_id: Option<&str>,
) -> String {
    let mut body = json!({ "typeId": type_id, "summary": summary });
    if let Some(parent) = parent_id {
        body["parentId"] = json!(parent);
    }
    let reply = app
        .send(post(&format!("/api/v1/projects/{}/cards", project.key), Some(admin), body))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.key()
}

async fn card_id(app: &App, admin: &str, key: &str) -> String {
    app.send(get(&format!("/api/v1/cards/{key}"), Some(admin)))
        .await
        .id()
}

async fn card_status(app: &App, admin: &str, key: &str) -> String {
    text(
        &app.send(get(&format!("/api/v1/cards/{key}"), Some(admin))).await.json(),
        "statusId",
    )
}

/// The project's permissive default workflow id.
async fn default_workflow(app: &App, admin: &str, project_key: &str) -> String {
    let workflows = rows(
        &app.send(get(&format!("/api/v1/projects/{project_key}/workflows"), Some(admin)))
            .await,
    );
    workflows
        .iter()
        .find(|w| w["isDefault"].as_bool() == Some(true))
        .map_or_else(|| panic!("{project_key} has no default workflow"), |w| text(w, "id"))
}

/// Creates a custom, enforcing workflow over the given statuses and routes the
/// project's default card type through it. Returns the workflow id.
async fn custom_workflow(app: &App, admin: &str, project: &Project, status_ids: &[String]) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/workflows", project.key),
            Some(admin),
            json!({
                "name": "Custom",
                "statusIds": status_ids,
                "cardTypeIds": [project.card_type],
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

/// Adds a transition, returning its id. The three gate lists are passed as JSON
/// arrays, mirroring the request body.
#[allow(clippy::too_many_arguments)]
async fn transition(
    app: &App,
    admin: &str,
    workflow_id: &str,
    name: &str,
    from: Option<&str>,
    to: &str,
    conditions: Value,
    validators: Value,
    post_functions: Value,
) -> String {
    let mut body = json!({
        "name": name,
        "toStatusId": to,
        "conditions": conditions,
        "validators": validators,
        "postFunctions": post_functions,
    });
    if let Some(from) = from {
        body["fromStatusId"] = json!(from);
    }
    let reply = app
        .send(post(&format!("/api/v1/workflows/{workflow_id}/transitions"), Some(admin), body))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

/// The names of a card's currently-available transitions.
async fn available(app: &App, admin: &str, card_key: &str) -> Vec<String> {
    rows(&app.send(get(&format!("/api/v1/cards/{card_key}/transitions"), Some(admin))).await)
        .iter()
        .map(|t| text(t, "name"))
        .collect()
}

/// A card's comment bodies.
async fn comments(app: &App, admin: &str, card_key: &str) -> Vec<String> {
    rows(&app.send(get(&format!("/api/v1/cards/{card_key}/comments"), Some(admin))).await)
        .iter()
        .map(|c| text(c, "body"))
        .collect()
}

/// Moves a card to a status through the board endpoint. Returns the reply.
async fn move_to(app: &App, admin: &str, card_key: &str, status_id: &str) -> Reply {
    app.send(post(
        &format!("/api/v1/cards/{card_key}/move"),
        Some(admin),
        json!({ "statusId": status_id }),
    ))
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_illegal_transition_with_no_edge_is_rejected() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "EDGE", "blank").await;

    let todo = p.status("To Do");
    let in_progress = p.status("In Progress");
    let done = p.status("Done");

    // A workflow with exactly one edge: To Do -> In Progress. Nothing reaches Done.
    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), in_progress.clone(), done.clone()]).await;
    transition(&app, &admin, &wf, "Start", Some(&todo), &in_progress, json!([]), json!([]), json!([])).await;

    let key = card(&app, &admin, &p, "task").await;

    // The legal edge works.
    let ok = move_to(&app, &admin, &key, &in_progress).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.raw_body);

    // There is no edge to Done: rejected, and the card stays put.
    let bad = move_to(&app, &admin, &key, &done).await;
    assert_eq!(bad.status, StatusCode::CONFLICT, "{}", bad.raw_body);
    assert_eq!(card_status(&app, &admin, &key).await, in_progress);
}

#[tokio::test]
async fn a_failing_condition_hides_a_transition_and_rejects_a_direct_attempt() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "COND", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    // "Only the assignee may resolve" — a condition. The card has no assignee, and
    // the admin is not it, so the condition fails and the transition is hidden.
    let resolve = transition(
        &app, &admin, &wf, "Resolve", Some(&todo), &done,
        json!([{ "kind": "OnlyAssignee" }]), json!([]), json!([]),
    ).await;

    let key = card(&app, &admin, &p, "task").await;

    // Hidden: absent from the available list.
    assert!(
        !available(&app, &admin, &key).await.contains(&"Resolve".to_owned()),
        "a transition whose condition fails must not be offered"
    );

    // Attempted directly through the board: rejected as if it did not exist.
    let bad = move_to(&app, &admin, &key, &done).await;
    assert_eq!(bad.status, StatusCode::CONFLICT, "{}", bad.raw_body);
    assert_eq!(card_status(&app, &admin, &key).await, todo);

    // Attempted directly through the named-transition endpoint: also rejected.
    let bad = app
        .send(post(
            &format!("/api/v1/cards/{key}/transitions/{resolve}"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(bad.status, StatusCode::CONFLICT, "{}", bad.raw_body);
    assert_eq!(card_status(&app, &admin, &key).await, todo);
}

#[tokio::test]
async fn a_failing_validator_rejects_names_the_field_and_runs_no_post_function() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "VALID", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    // The "resolution + comment on Done" screen: a validator requiring an
    // assignee, plus a post-function that would add a comment if the move ran.
    let resolve = transition(
        &app, &admin, &wf, "Finish", Some(&todo), &done,
        json!([]),
        json!([{ "kind": "RequiredField", "config": { "field": "assignee" } }]),
        json!([{ "kind": "AddComment", "config": { "body": "auto: finished" } }]),
    ).await;

    let key = card(&app, &admin, &p, "task").await;

    // The transition is OFFERED (its condition passes) ...
    assert!(available(&app, &admin, &key).await.contains(&"Finish".to_owned()));

    // ... but rejected with the field named, because there is no assignee.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{key}/transitions/{resolve}"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.raw_body);
    assert!(reply.raw_body.contains("assignee"), "the field must be named: {}", reply.raw_body);

    // The status did not change ...
    assert_eq!(card_status(&app, &admin, &key).await, todo);
    // ... and, crucially, the post-function did NOT run: no comment was added.
    assert!(
        comments(&app, &admin, &key).await.is_empty(),
        "a rejected validator must not let a post-function run"
    );
}

#[tokio::test]
async fn child_blocking_stops_closing_a_parent_with_an_open_child() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "CHILD", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    transition(
        &app, &admin, &wf, "Close", None, &done,
        json!([{ "kind": "ChildBlocking" }]), json!([]), json!([]),
    ).await;

    // A parent (Group, level 1) with a child (Card, level 0). Route the Group
    // type through the workflow too, so the parent's move is enforced — otherwise
    // the parent would sit on the permissive default and close freely.
    let types = rows(&app.send(get(&format!("/api/v1/projects/{}/card-types", p.key), Some(&admin))).await);
    let group = types.iter().find(|t| t["name"] == "Group").map(|t| text(t, "id")).expect("Group type");
    let assigned = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/workflows/{wf}"),
            Some(&admin),
            Some(json!({ "cardTypeIds": [group] })),
        ))
        .await;
    assert_eq!(assigned.status, StatusCode::OK, "{}", assigned.raw_body);

    let parent = card_of_type(&app, &admin, &p, &group, "parent", None).await;
    let parent_id = card_id(&app, &admin, &parent).await;
    let child = card_of_type(&app, &admin, &p, &p.card_type, "child", Some(&parent_id)).await;

    // The child is open, so the parent cannot enter Done — the transition is
    // hidden and the direct attempt is rejected.
    assert!(!available(&app, &admin, &parent).await.contains(&"Close".to_owned()));
    let blocked = move_to(&app, &admin, &parent, &done).await;
    assert_eq!(blocked.status, StatusCode::CONFLICT, "{}", blocked.raw_body);
    assert_eq!(card_status(&app, &admin, &parent).await, todo);

    // Finish the child (it moves under the same workflow — the Close edge is
    // global, so it applies to the child too).
    let child_done = move_to(&app, &admin, &child, &done).await;
    assert_eq!(child_done.status, StatusCode::OK, "{}", child_done.raw_body);

    // Now the parent may close.
    assert!(available(&app, &admin, &parent).await.contains(&"Close".to_owned()));
    let ok = move_to(&app, &admin, &parent, &done).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.raw_body);
    assert_eq!(card_status(&app, &admin, &parent).await, done);
}

#[tokio::test]
async fn a_set_resolution_post_function_sets_it_on_done_and_leaving_done_clears_it() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "RESO", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    // Pick a specific resolution to set.
    let resolutions = rows(&app.send(get(&format!("/api/v1/projects/{}/resolutions", p.key), Some(&admin))).await);
    let wont_do = resolutions.iter().find(|r| r["name"] == "Won't Do").map(|r| text(r, "id")).expect("Won't Do");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    let finish = transition(
        &app, &admin, &wf, "Finish", Some(&todo), &done,
        json!([]), json!([]),
        json!([{ "kind": "SetResolution", "config": { "resolutionId": wont_do } }]),
    ).await;
    transition(&app, &admin, &wf, "Reopen", Some(&done), &todo, json!([]), json!([]), json!([])).await;

    let key = card(&app, &admin, &p, "task").await;

    let reply = app
        .send(post(&format!("/api/v1/cards/{key}/transitions/{finish}"), Some(&admin), json!({})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    // The post-function set the specific resolution, not just any default.
    assert_eq!(reply.json()["resolutionId"], wont_do);
    assert_eq!(reply.json()["resolved"], true);

    // Leaving Done clears the resolution — via card.rs's resolution rules, with no
    // post-function needed on the Reopen edge.
    let reopened = move_to(&app, &admin, &key, &todo).await;
    assert_eq!(reopened.status, StatusCode::OK, "{}", reopened.raw_body);
    assert_eq!(reopened.json()["resolutionId"], Value::Null);
    assert_eq!(reopened.json()["resolved"], false);
}

#[tokio::test]
async fn a_failing_post_function_rolls_the_whole_transition_back() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ROLL", "blank").await;
    let other = project(&app, &admin, "OTHER", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    // A SetResolution post-function pointing at a resolution from ANOTHER project.
    // It validates against this project, fails, and must roll the move back.
    let other_resolutions =
        rows(&app.send(get(&format!("/api/v1/projects/{}/resolutions", other.key), Some(&admin))).await);
    let foreign = text(&other_resolutions[0], "id");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    let finish = transition(
        &app, &admin, &wf, "Finish", Some(&todo), &done,
        json!([]), json!([]),
        json!([{ "kind": "SetResolution", "config": { "resolutionId": foreign } }]),
    ).await;

    let key = card(&app, &admin, &p, "task").await;

    let reply = app
        .send(post(&format!("/api/v1/cards/{key}/transitions/{finish}"), Some(&admin), json!({})))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.raw_body);

    // The card did not move: the failing post-function rolled everything back.
    assert_eq!(card_status(&app, &admin, &key).await, todo);
    assert_eq!(app.send(get(&format!("/api/v1/cards/{key}"), Some(&admin))).await.json()["resolved"], false);
}

#[tokio::test]
async fn the_job_search_workflow_round_trips_from_interested_to_rejected() {
    // The domain-neutrality proof: the engine expresses a linear, branching,
    // non-Scrum workflow cleanly, and every step is legal.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "JOB", "job-search").await;

    let stages = [
        "Interested", "Applied", "Phone Screen", "Interview", "Take-home", "Offer",
    ];
    let status_ids: Vec<String> = stages
        .iter()
        .chain(["Accepted", "Rejected", "Ghosted"].iter())
        .map(|n| p.status(n))
        .collect();

    let wf = custom_workflow(&app, &admin, &p, &status_ids).await;

    // The linear spine: each stage to the next.
    for pair in stages.windows(2) {
        let from = p.status(pair[0]);
        let to = p.status(pair[1]);
        transition(&app, &admin, &wf, &format!("To {}", pair[1]), Some(&from), &to, json!([]), json!([]), json!([])).await;
    }
    // The three terminal branches from Offer.
    for ending in ["Accepted", "Rejected", "Ghosted"] {
        let to = p.status(ending);
        transition(&app, &admin, &wf, ending, Some(&p.status("Offer")), &to, json!([]), json!([]), json!([])).await;
    }
    // Ghosted can happen from any stage after applying — a global edge.
    transition(&app, &admin, &wf, "Ghost", None, &p.status("Ghosted"), json!([]), json!([]), json!([])).await;

    let application = card(&app, &admin, &p, "Dream Corp").await;

    // Walk the spine, then reject.
    for stage in &stages[1..] {
        let reply = move_to(&app, &admin, &application, &p.status(stage)).await;
        assert_eq!(reply.status, StatusCode::OK, "step to {stage}: {}", reply.raw_body);
        assert_eq!(card_status(&app, &admin, &application).await, p.status(stage));
    }
    let rejected = move_to(&app, &admin, &application, &p.status("Rejected")).await;
    assert_eq!(rejected.status, StatusCode::OK, "{}", rejected.raw_body);
    // Rejected is a done status, so the card is now resolved.
    assert_eq!(card_status(&app, &admin, &application).await, p.status("Rejected"));
    assert_eq!(
        app.send(get(&format!("/api/v1/cards/{application}"), Some(&admin))).await.json()["resolved"],
        true
    );

    // A step the workflow does not allow — Interested straight to Offer — is
    // rejected, proving the spine is enforced and not decorative.
    let fresh = card(&app, &admin, &p, "Second Corp").await;
    let illegal = move_to(&app, &admin, &fresh, &p.status("Offer")).await;
    assert_eq!(illegal.status, StatusCode::CONFLICT, "{}", illegal.raw_body);
    // ...but the global Ghost edge is available from the start.
    let ghosted = move_to(&app, &admin, &fresh, &p.status("Ghosted")).await;
    assert_eq!(ghosted.status, StatusCode::OK, "{}", ghosted.raw_body);
}

#[tokio::test]
async fn existing_card_moves_still_work_under_the_permissive_default_workflow() {
    // The no-regression guarantee: a project born from a template gets a default
    // workflow that permits every move its statuses imply, so a card walks To Do
    // -> In Progress -> Done -> To Do with no custom transitions defined.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "REG", "programming").await;

    // The project really does have a default workflow.
    let wf = default_workflow(&app, &admin, &p.key).await;
    let workflow = app.send(get(&format!("/api/v1/workflows/{wf}"), Some(&admin))).await;
    assert_eq!(workflow.json()["isDefault"], true);

    let key = card(&app, &admin, &p, "task").await;

    // The default workflow offers a move to every other status.
    let offered = available(&app, &admin, &key).await;
    assert!(offered.iter().any(|n| n.contains("In Progress")), "default offers moves: {offered:?}");

    for status in ["In Progress", "Done", "To Do", "Blocked"] {
        let reply = move_to(&app, &admin, &key, &p.status(status)).await;
        assert_eq!(reply.status, StatusCode::OK, "move to {status}: {}", reply.raw_body);
        assert_eq!(card_status(&app, &admin, &key).await, p.status(status));
    }
}

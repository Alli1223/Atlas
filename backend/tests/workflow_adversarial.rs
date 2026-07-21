//! Adversarial probes against the workflow engine's invariants.
//!
//! `tests/workflow.rs` proves the happy-path execution contract. This file tries
//! to *break* it, one attack per test:
//!
//! 1. a transition id from another project's workflow cannot be executed on a card
//!    (the named-transition endpoint must not be a cross-workflow escape hatch);
//! 2. a project **viewer** cannot execute a transition, only read them;
//! 3. a condition-hidden transition that also carries a post-function, attacked
//!    directly, rejects with **no side effect** — no comment, no event, no move;
//! 4. the resolution invariant (ADR §E) holds through the transition path: a
//!    screen-supplied resolution on a move to a *non-done* status is cleared, and
//!    a rolled-back transition leaves no `workflow_events` row;
//! 5. `ChildBlocking` is direct-children-only — a done child with an open
//!    grandchild does not block the parent (the documented rule), and a done
//!    direct child does not either;
//! 6. an `UpdateField` post-function pointing a user field at a non-existent user
//!    is refused as a bad request (422) rather than an opaque 500, and rolls back.
//!
//! The harness mirrors `tests/workflow.rs`.

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
// Harness (lifted from tests/workflow.rs)
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
        Self { db, config, _temp: temp }
    }

    fn router(&self) -> Router {
        api::router(AppState::new(self.db.clone(), self.config.clone()))
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router().oneshot(request).await.expect("request failed");
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
        None => builder.body(Body::empty()).expect("failed to build the request"),
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

/// Creates a user of the given instance role and logs them in.
async fn user(app: &App, admin: &str, username: &str, role: &str) -> (String, String) {
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(admin),
            json!({
                "username": username,
                "password": GOOD_PASSWORD,
                "role": role,
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let id = reply.id();

    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let cookie = reply.session_cookie().expect("login must set a cookie");
    (id, cookie)
}

/// Grants a project role to a user.
async fn grant(app: &App, admin: &str, project_key: &str, user_id: &str, role: &str) {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/members"),
            Some(admin),
            json!({ "userId": user_id, "role": role }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

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

    Project { key: key.to_owned(), card_type, statuses }
}

async fn card(app: &App, admin: &str, project: &Project, summary: &str) -> String {
    card_of_type(app, admin, project, &project.card_type, summary, None).await
}

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
    app.send(get(&format!("/api/v1/cards/{key}"), Some(admin))).await.id()
}

async fn card_status(app: &App, admin: &str, key: &str) -> String {
    text(
        &app.send(get(&format!("/api/v1/cards/{key}"), Some(admin))).await.json(),
        "statusId",
    )
}

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

async fn available(app: &App, admin: &str, card_key: &str) -> Vec<String> {
    rows(&app.send(get(&format!("/api/v1/cards/{card_key}/transitions"), Some(admin))).await)
        .iter()
        .map(|t| text(t, "name"))
        .collect()
}

async fn comments(app: &App, admin: &str, card_key: &str) -> Vec<String> {
    rows(&app.send(get(&format!("/api/v1/cards/{card_key}/comments"), Some(admin))).await)
        .iter()
        .map(|c| text(c, "body"))
        .collect()
}

async fn move_to(app: &App, admin: &str, card_key: &str, status_id: &str) -> Reply {
    app.send(post(
        &format!("/api/v1/cards/{card_key}/move"),
        Some(admin),
        json!({ "statusId": status_id }),
    ))
    .await
}

async fn exec(app: &App, cookie: &str, card_key: &str, transition_id: &str, body: Value) -> Reply {
    app.send(post(
        &format!("/api/v1/cards/{card_key}/transitions/{transition_id}"),
        Some(cookie),
        body,
    ))
    .await
}

/// How many `workflow_events` rows a card has — the `FireEvent` sink.
async fn event_count(app: &App, card_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workflow_events WHERE card_id = ?")
        .bind(card_id)
        .fetch_one(app.db.reader())
        .await
        .expect("count query failed")
}

// ---------------------------------------------------------------------------
// 1. Cross-workflow escape: a transition id from another project's workflow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_transition_from_another_project_cannot_be_executed_on_a_card() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let a = project(&app, &admin, "AONE", "blank").await;
    let b = project(&app, &admin, "BTWO", "blank").await;

    // A real, condition-free transition in project B's custom workflow.
    let b_todo = b.status("To Do");
    let b_done = b.status("Done");
    let b_wf = custom_workflow(&app, &admin, &b, &[b_todo.clone(), b_done.clone()]).await;
    let b_finish =
        transition(&app, &admin, &b_wf, "Finish", Some(&b_todo), &b_done, json!([]), json!([]), json!([])).await;

    // A card in project A. Its type routes through A's permissive default, and B's
    // transition is nothing to do with it.
    let a_card = card(&app, &admin, &a, "task").await;
    let before = card_status(&app, &admin, &a_card).await;

    // The access layer scopes this route on the CARD's project (A), which the admin
    // owns — so we are past the gate. The engine itself must refuse: the transition
    // belongs to a different workflow. That is a 404, not a move.
    let reply = exec(&app, &admin, &a_card, &b_finish, json!({})).await;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "a foreign-workflow transition must not execute: {}",
        reply.raw_body
    );
    assert_eq!(card_status(&app, &admin, &a_card).await, before, "the card must not have moved");
}

// ---------------------------------------------------------------------------
// 2. A project viewer cannot execute a transition
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_project_viewer_can_read_but_not_execute_a_transition() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "VIEW", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");
    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    let finish =
        transition(&app, &admin, &wf, "Finish", Some(&todo), &done, json!([]), json!([]), json!([])).await;

    let key = card(&app, &admin, &p, "task").await;

    // A Member instance user who is only a *viewer* on this project.
    let (viewer_id, viewer) = user(&app, &admin, "peeker", "member").await;
    grant(&app, &admin, &p.key, &viewer_id, "viewer").await;

    // Reading the available transitions is allowed (Viewer).
    let read = app.send(get(&format!("/api/v1/cards/{key}/transitions"), Some(&viewer))).await;
    assert_eq!(read.status, StatusCode::OK, "a viewer may read transitions: {}", read.raw_body);

    // Executing one is not (needs Member). The card must not move.
    let reply = exec(&app, &viewer, &key, &finish, json!({})).await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a project viewer must not execute a transition: {}",
        reply.raw_body
    );
    assert_eq!(card_status(&app, &admin, &key).await, todo, "the card must not have moved");
}

// ---------------------------------------------------------------------------
// 3. A hidden transition with a post-function, attacked directly, has no effect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_hidden_transition_attacked_directly_runs_no_post_function() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "HIDE", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");
    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;

    // Hidden by OnlyAssignee (the card has no assignee), and it carries a comment
    // post-function AND a fire-event post-function. If the hide gate leaked, either
    // would leave a visible trace.
    let resolve = transition(
        &app, &admin, &wf, "Resolve", Some(&todo), &done,
        json!([{ "kind": "OnlyAssignee" }]),
        json!([]),
        json!([
            { "kind": "AddComment", "config": { "body": "auto: resolved" } },
            { "kind": "FireEvent", "config": { "event": "resolved" } },
        ]),
    )
    .await;

    let key = card(&app, &admin, &p, "task").await;
    let id = card_id(&app, &admin, &key).await;

    assert!(!available(&app, &admin, &key).await.contains(&"Resolve".to_owned()));

    let reply = exec(&app, &admin, &key, &resolve, json!({})).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // No move, no comment, no event.
    assert_eq!(card_status(&app, &admin, &key).await, todo);
    assert!(comments(&app, &admin, &key).await.is_empty(), "a hidden transition must add no comment");
    assert_eq!(event_count(&app, &id).await, 0, "a hidden transition must fire no event");
}

// ---------------------------------------------------------------------------
// 4a. Resolution invariant: a screen resolution on a non-done move is cleared
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_screen_resolution_on_a_move_to_a_non_done_status_is_cleared() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "NDR", "blank").await;

    let todo = p.status("To Do");
    let in_progress = p.status("In Progress");
    let done = p.status("Done");

    let resolutions =
        rows(&app.send(get(&format!("/api/v1/projects/{}/resolutions", p.key), Some(&admin))).await);
    let done_res = text(&resolutions[0], "id");

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), in_progress.clone(), done.clone()]).await;
    // A transition into a NON-done status (In Progress).
    let start =
        transition(&app, &admin, &wf, "Start", Some(&todo), &in_progress, json!([]), json!([]), json!([])).await;

    let key = card(&app, &admin, &p, "task").await;

    // The screen hands a resolution to a move that lands in a non-done column. The
    // §E rule must win: not-done => no resolution, regardless of what was supplied.
    let reply = exec(&app, &admin, &key, &start, json!({ "resolutionId": done_res })).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["statusId"], in_progress);
    assert_eq!(
        reply.json()["resolutionId"],
        Value::Null,
        "a card in a non-done status must never carry a resolution"
    );
    assert_eq!(reply.json()["resolved"], false);
}

// ---------------------------------------------------------------------------
// 4b. A rolled-back transition leaves no event and no comment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_rolled_back_transition_records_no_event_and_no_comment() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATOM", "blank").await;
    let other = project(&app, &admin, "ELSE", "blank").await;

    let todo = p.status("To Do");
    let done = p.status("Done");

    // A resolution from another project — the SetResolution validation fails and
    // rolls the move back. The FireEvent and AddComment post-functions on the same
    // transition must therefore leave no trace.
    let foreign = {
        let rs = rows(&app.send(get(&format!("/api/v1/projects/{}/resolutions", other.key), Some(&admin))).await);
        text(&rs[0], "id")
    };

    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), done.clone()]).await;
    let finish = transition(
        &app, &admin, &wf, "Finish", Some(&todo), &done,
        json!([]),
        json!([]),
        json!([
            { "kind": "FireEvent", "config": { "event": "finished" } },
            { "kind": "SetResolution", "config": { "resolutionId": foreign } },
        ]),
    )
    .await;

    let key = card(&app, &admin, &p, "task").await;
    let id = card_id(&app, &admin, &key).await;

    let reply = exec(&app, &admin, &key, &finish, json!({ "comment": "here goes" })).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.raw_body);

    assert_eq!(card_status(&app, &admin, &key).await, todo, "the card must not have moved");
    assert_eq!(event_count(&app, &id).await, 0, "a rolled-back transition must record no event");
    assert!(
        comments(&app, &admin, &key).await.is_empty(),
        "a rolled-back transition must persist no comment"
    );
}

// ---------------------------------------------------------------------------
// 5. ChildBlocking is direct-children-only
// ---------------------------------------------------------------------------

/// Routes both the Group (parent) and default (child) types through a workflow
/// with a single global `Close`->`Done` edge guarded by `ChildBlocking`, then returns
/// the workflow id and the Group type id.
async fn child_blocking_workflow(app: &App, admin: &str, p: &Project) -> (String, String) {
    let done = p.status("Done");
    let wf = custom_workflow(app, admin, p, &[p.status("To Do"), done.clone()]).await;
    transition(app, admin, &wf, "Close", None, &done, json!([{ "kind": "ChildBlocking" }]), json!([]), json!([])).await;

    let types = rows(&app.send(get(&format!("/api/v1/projects/{}/card-types", p.key), Some(admin))).await);
    let group = types.iter().find(|t| t["name"] == "Group").map(|t| text(t, "id")).expect("Group type");
    let assigned = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/workflows/{wf}"),
            Some(admin),
            Some(json!({ "cardTypeIds": [group] })),
        ))
        .await;
    assert_eq!(assigned.status, StatusCode::OK, "{}", assigned.raw_body);
    (wf, group)
}

#[tokio::test]
async fn child_blocking_ignores_a_done_direct_child_even_with_an_open_grandchild() {
    // The exact off-by-one the review names: parent -> DONE child -> OPEN
    // grandchild. The rule is "cannot enter Done with an open *direct* child", so
    // a done direct child must NOT block the parent, no matter what hangs beneath
    // it. This is the documented Jira semantics (sub-task blocking is one level).
    //
    // The `blank` template's three levels give a real grandchild: Group(1) ->
    // Card(0) -> Sub-task(-1).
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "GRAND", "blank").await;
    let (_wf, group) = child_blocking_workflow(&app, &admin, &p).await;

    let done = p.status("Done");
    let types = rows(&app.send(get(&format!("/api/v1/projects/{}/card-types", p.key), Some(&admin))).await);
    let subtask = types.iter().find(|t| t["name"] == "Sub-task").map(|t| text(t, "id")).expect("Sub-task type");

    // parent (Group) -> child (Card). No grandchild yet.
    let parent = card_of_type(&app, &admin, &p, &group, "parent", None).await;
    let parent_id = card_id(&app, &admin, &parent).await;
    let child = card_of_type(&app, &admin, &p, &p.card_type, "child", Some(&parent_id)).await;
    let child_id = card_id(&app, &admin, &child).await;

    // The parent's only direct child is still open, so the parent cannot close.
    let blocked = move_to(&app, &admin, &parent, &done).await;
    assert_eq!(blocked.status, StatusCode::CONFLICT, "open direct child blocks: {}", blocked.raw_body);

    // Close the child while it has no children. Then hang an OPEN grandchild
    // (Sub-task, routed through the permissive default) under the now-done child.
    let close_child = move_to(&app, &admin, &child, &done).await;
    assert_eq!(close_child.status, StatusCode::OK, "{}", close_child.raw_body);
    let _grandchild = card_of_type(&app, &admin, &p, &subtask, "grandchild", Some(&child_id)).await;

    // The parent's only direct child is done, so the parent may close — the open
    // grandchild two levels down is not the parent's concern. The rule looks
    // exactly one level down, never deeper.
    let close_parent = move_to(&app, &admin, &parent, &done).await;
    assert_eq!(
        close_parent.status,
        StatusCode::OK,
        "a done direct child must not block, whatever hangs beneath it: {}",
        close_parent.raw_body
    );
    assert_eq!(card_status(&app, &admin, &parent).await, done);
}

// ---------------------------------------------------------------------------
// 6. UpdateField pointing a user field at a non-existent user
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_update_field_post_function_naming_a_missing_user_is_a_bad_request_not_a_500() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "UPD", "blank").await;

    let todo = p.status("To Do");
    let in_progress = p.status("In Progress");
    let wf = custom_workflow(&app, &admin, &p, &[todo.clone(), in_progress.clone()]).await;

    // An UpdateField post-function that assigns the card to a user id that names
    // nobody. The patch path validates assignee ids (422 for a phantom); the
    // transition path must be just as safe — and must never surface a raw
    // foreign-key violation as an opaque 500, nor half-apply the move.
    let start = transition(
        &app, &admin, &wf, "Start", Some(&todo), &in_progress,
        json!([]),
        json!([]),
        json!([{ "kind": "UpdateField", "config": { "field": "assignee", "value": "no-such-user-id" } }]),
    )
    .await;

    let key = card(&app, &admin, &p, "task").await;

    let reply = exec(&app, &admin, &key, &start, json!({})).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a post-function naming a missing user must be a 422, not a 500: {}",
        reply.raw_body
    );
    assert_eq!(card_status(&app, &admin, &key).await, todo, "the failed transition must roll back");
}

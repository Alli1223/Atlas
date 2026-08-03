//! End-to-end tests for the cycles HTTP surface, over the real router,
//! middleware stack, and database.
//!
//! `domain::cycle`'s unit tests prove the state machine itself (dates,
//! single-active-cycle, carry-over, scope tracking) in isolation. These prove
//! the HTTP wiring on top of it: routes are reachable, request/response DTOs
//! round-trip in the wire's camelCase, and domain errors surface as the right
//! status codes through a real request.
//!
//! The project-access gating of every route (an outsider gets a 404) is proven
//! exhaustively by `every_project_scoped_route_refuses_an_outsider` in
//! `tests/project_access.rs`; here the caller is always the instance admin, who
//! is past the gate, so the assertions are about the handler logic itself.

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
fn patch(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PATCH, uri, cookie, Some(body))
}
fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
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

async fn project(app: &App, admin: &str, key: &str, template: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": template }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    key.to_owned()
}

async fn default_type(app: &App, admin: &str, key: &str) -> String {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{key}/card-types"),
            Some(admin),
        ))
        .await;
    reply.json()[0]["id"]
        .as_str()
        .expect("a default card type")
        .to_owned()
}

async fn card(app: &App, admin: &str, key: &str, type_id: &str, summary: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{key}/cards"),
            Some(admin),
            json!({ "typeId": type_id, "summary": summary }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("a card key").to_owned()
}

async fn create_cycle(app: &App, admin: &str, key: &str, name: &str) -> Value {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{key}/cycles"),
            Some(admin),
            json!({ "name": name }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creating_a_cycle_requires_the_project_to_have_cycles_enabled() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "BLANK", "blank").await;

    let reply = app
        .send(post(
            "/api/v1/projects/BLANK/cycles",
            Some(&admin),
            json!({ "name": "Sprint 1" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_cycle_lists_at_the_project_and_reads_at_its_own_id() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;

    let created = create_cycle(&app, &admin, "ATLAS", "Sprint 1").await;
    assert_eq!(created["name"], "Sprint 1");
    assert_eq!(created["state"], "future");
    assert_eq!(created["startDate"], Value::Null);

    let listed = app
        .send(get("/api/v1/projects/ATLAS/cycles", Some(&admin)))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.raw_body);
    let rows = listed.json();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], created["id"]);
}

#[tokio::test]
async fn patching_renames_and_clears_the_goal() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;
    let cycle = create_cycle(&app, &admin, "ATLAS", "Sprint 1").await;
    let id = cycle["id"].as_str().unwrap();

    let reply = app
        .send(patch(
            &format!("/api/v1/cycles/{id}"),
            Some(&admin),
            json!({ "name": "Sprint One", "goal": "Ship the thing" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["name"], "Sprint One");
    assert_eq!(reply.json()["goal"], "Ship the thing");

    let cleared = app
        .send(patch(
            &format!("/api/v1/cycles/{id}"),
            Some(&admin),
            json!({ "goal": null }),
        ))
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.raw_body);
    assert_eq!(cleared.json()["goal"], Value::Null);

    // An empty patch is rejected rather than silently doing nothing.
    let empty = app
        .send(patch(
            &format!("/api/v1/cycles/{id}"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(
        empty.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        empty.raw_body
    );
}

#[tokio::test]
async fn starting_a_second_cycle_while_one_is_active_conflicts() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;
    let a = create_cycle(&app, &admin, "ATLAS", "Sprint A").await;
    let b = create_cycle(&app, &admin, "ATLAS", "Sprint B").await;
    let a_id = a["id"].as_str().unwrap();
    let b_id = b["id"].as_str().unwrap();

    let started = app
        .send(post(
            &format!("/api/v1/cycles/{a_id}/start"),
            Some(&admin),
            json!({ "startDate": "2026-01-01", "endDate": "2026-01-14" }),
        ))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.raw_body);
    assert_eq!(started.json()["state"], "active");

    let conflict = app
        .send(post(
            &format!("/api/v1/cycles/{b_id}/start"),
            Some(&admin),
            json!({ "startDate": "2026-01-01", "endDate": "2026-01-14" }),
        ))
        .await;
    assert_eq!(
        conflict.status,
        StatusCode::CONFLICT,
        "{}",
        conflict.raw_body
    );
}

#[tokio::test]
async fn a_cards_cycle_membership_can_be_added_and_removed_through_its_own_endpoint() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;
    let type_id = default_type(&app, &admin, "ATLAS").await;
    let card_key = card(&app, &admin, "ATLAS", &type_id, "Do the thing").await;
    let cycle = create_cycle(&app, &admin, "ATLAS", "Sprint 1").await;
    let cycle_id = cycle["id"].as_str().unwrap();

    // Not in a cycle yet.
    let none = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND, "{}", none.raw_body);

    let added = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
            json!({ "cycleId": cycle_id }),
        ))
        .await;
    assert_eq!(added.status, StatusCode::NO_CONTENT, "{}", added.raw_body);

    let now_in = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(now_in.status, StatusCode::OK, "{}", now_in.raw_body);
    assert_eq!(now_in.json()["id"], cycle_id);

    let removed = app
        .send(delete(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(
        removed.status,
        StatusCode::NO_CONTENT,
        "{}",
        removed.raw_body
    );

    let gone = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.raw_body);

    // Removing again is a no-op, not an error.
    let noop = app
        .send(delete(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(noop.status, StatusCode::NO_CONTENT, "{}", noop.raw_body);
}

#[tokio::test]
async fn completing_carries_an_incomplete_card_into_a_brand_new_cycle_and_reopening_replans_the_end_date()
 {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;
    let type_id = default_type(&app, &admin, "ATLAS").await;
    let card_key = card(&app, &admin, "ATLAS", &type_id, "Carried over").await;
    let cycle = create_cycle(&app, &admin, "ATLAS", "Sprint 1").await;
    let id = cycle["id"].as_str().unwrap().to_owned();

    let added = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
            json!({ "cycleId": id }),
        ))
        .await;
    assert_eq!(added.status, StatusCode::NO_CONTENT, "{}", added.raw_body);

    let started = app
        .send(post(
            &format!("/api/v1/cycles/{id}/start"),
            Some(&admin),
            json!({ "startDate": "2026-01-01", "endDate": "2026-01-14" }),
        ))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.raw_body);

    // Never marked done, so it is incomplete when the cycle closes.
    let completed = app
        .send(post(
            &format!("/api/v1/cycles/{id}/complete"),
            Some(&admin),
            json!({ "carryTo": { "kind": "newCycle", "name": "Sprint 2" } }),
        ))
        .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.raw_body);
    assert_eq!(completed.json()["state"], "closed");

    // The card followed the card into the freshly-created cycle.
    let now_in = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(now_in.status, StatusCode::OK, "{}", now_in.raw_body);
    assert_eq!(now_in.json()["name"], "Sprint 2");
    assert_ne!(now_in.json()["id"], id);

    // Reopening the original replans its end date but keeps its start date.
    let reopened = app
        .send(post(
            &format!("/api/v1/cycles/{id}/reopen"),
            Some(&admin),
            json!({ "endDate": "2026-01-21" }),
        ))
        .await;
    assert_eq!(reopened.status, StatusCode::OK, "{}", reopened.raw_body);
    assert_eq!(reopened.json()["state"], "active");
    assert_eq!(reopened.json()["startDate"], "2026-01-01");
    assert_eq!(reopened.json()["endDate"], "2026-01-21");
}

#[tokio::test]
async fn completing_carries_an_incomplete_card_into_an_existing_cycle() {
    // A regression test for `CarryToRequest::ExistingCycle`'s wire shape: every other
    // request body in this API is camelCase, and this variant's `cycle_id` field was
    // briefly an exception (a `#[serde(rename_all)]` on an enum renames its variants, not
    // a struct variant's own fields, so it needs its own attribute too).
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS", "programming").await;
    let type_id = default_type(&app, &admin, "ATLAS").await;
    let card_key = card(&app, &admin, "ATLAS", &type_id, "Carried over").await;
    let closing = create_cycle(&app, &admin, "ATLAS", "Sprint 1").await;
    let closing_id = closing["id"].as_str().unwrap().to_owned();
    let target = create_cycle(&app, &admin, "ATLAS", "Sprint 2").await;
    let target_id = target["id"].as_str().unwrap().to_owned();

    let added = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
            json!({ "cycleId": closing_id }),
        ))
        .await;
    assert_eq!(added.status, StatusCode::NO_CONTENT, "{}", added.raw_body);

    let started = app
        .send(post(
            &format!("/api/v1/cycles/{closing_id}/start"),
            Some(&admin),
            json!({ "startDate": "2026-01-01", "endDate": "2026-01-14" }),
        ))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.raw_body);

    // Never marked done, so it is incomplete when the cycle closes.
    let completed = app
        .send(post(
            &format!("/api/v1/cycles/{closing_id}/complete"),
            Some(&admin),
            json!({ "carryTo": { "kind": "existingCycle", "cycleId": target_id } }),
        ))
        .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.raw_body);
    assert_eq!(completed.json()["state"], "closed");

    let now_in = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/cycle"),
            Some(&admin),
        ))
        .await;
    assert_eq!(now_in.status, StatusCode::OK, "{}", now_in.raw_body);
    assert_eq!(now_in.json()["id"], target_id);
}

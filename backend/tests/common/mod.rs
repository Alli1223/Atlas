//! The integration-test harness shared by the test files written from Phase 3's leftovers on.
//!
//! Older test files each carry their own copy of this (the `App`/`Reply`/`admin_past_the_gate`
//! trio lifted from `tests/auth.rs`); newer ones `mod common;` it instead. Every helper here
//! drives the real router through `tower::ServiceExt::oneshot` — every layer runs, nothing
//! listens on a port.
//!
//! `dead_code` is allowed because each test binary compiles this module separately and uses a
//! different subset of it.
#![allow(dead_code)]

use atlas::api::{self, AppState};
use atlas::auth::seed::{self, DEFAULT_ADMIN_USERNAME};
use atlas::auth::session;
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The seeded admin password.
pub(crate) const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy.
pub(crate) const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

/// A migrated, seeded database and the router over it.
pub(crate) struct App {
    pub(crate) db: Db,
    pub(crate) config: Config,
    pub(crate) temp: TempDb,
}

impl App {
    pub(crate) async fn new() -> Self {
        let temp = TempDb::new();
        let config = temp.config();
        Self::with_config(temp, config).await
    }

    pub(crate) async fn with_config(temp: TempDb, config: Config) -> Self {
        let db = Db::connect(&config).await.expect("failed to open database");
        db::migrate::run(&db).await.expect("failed to migrate");
        seed::ensure_default_admin(&db)
            .await
            .expect("failed to seed the default admin");
        Self { db, config, temp }
    }

    pub(crate) fn state(&self) -> AppState {
        AppState::new(self.db.clone(), self.config.clone())
    }

    /// A fresh router. Rebuilt per request because `oneshot` consumes it.
    pub(crate) fn router(&self) -> Router {
        api::router(self.state())
    }

    pub(crate) async fn send(&self, request: Request<Body>) -> Reply {
        let response = self
            .router()
            .oneshot(request)
            .await
            .expect("request failed");
        Reply::from(response).await
    }

    /// Sends and asserts the status, returning the reply.
    pub(crate) async fn expect(&self, request: Request<Body>, status: StatusCode) -> Reply {
        let reply = self.send(request).await;
        assert_eq!(reply.status, status, "{}", reply.raw_body);
        reply
    }
}

/// A response, with its body already read.
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) bytes: Vec<u8>,
    pub(crate) raw_body: String,
}

impl Reply {
    pub(crate) async fn from(response: axum::response::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .expect("failed to read the body")
            .to_vec();
        Self {
            status,
            headers,
            raw_body: String::from_utf8_lossy(&bytes).into_owned(),
            bytes,
        }
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.raw_body)
            .unwrap_or_else(|err| panic!("body was not JSON ({err}): {}", self.raw_body))
    }

    pub(crate) fn rows(&self) -> Vec<Value> {
        self.json()
            .as_array()
            .unwrap_or_else(|| panic!("expected a JSON array: {}", self.raw_body))
            .clone()
    }

    pub(crate) fn header(&self, name: header::HeaderName) -> String {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    pub(crate) fn session_cookie(&self) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with(session::COOKIE_NAME))
            .and_then(|c| c.split(';').next())
            .and_then(|c| c.split_once('='))
            .map(|(_, value)| value.to_owned())
    }

    pub(crate) fn id(&self) -> String {
        self.text("id")
    }

    pub(crate) fn key(&self) -> String {
        self.text("key")
    }

    pub(crate) fn text(&self, field: &str) -> String {
        self.json()[field]
            .as_str()
            .unwrap_or_else(|| panic!("no string {field:?} in: {}", self.raw_body))
            .to_owned()
    }
}

/// A required string field of a JSON object.
pub(crate) fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("no string {field:?} in {value}"))
        .to_owned()
}

pub(crate) fn request(
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
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

/// A request with a raw (non-JSON) body, for uploads.
pub(crate) fn raw(
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    content_type: &str,
    body: Vec<u8>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len());
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
    }
    builder
        .body(Body::from(body))
        .expect("failed to build the request")
}

pub(crate) fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::GET, uri, cookie, None)
}

pub(crate) fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::POST, uri, cookie, Some(body))
}

pub(crate) fn post_empty(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::POST, uri, cookie, None)
}

pub(crate) fn put(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PUT, uri, cookie, Some(body))
}

pub(crate) fn patch(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PATCH, uri, cookie, Some(body))
}

pub(crate) fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
}

/// Signs the admin in and gets it past the forced-reset gate.
pub(crate) async fn admin_past_the_gate(app: &App) -> String {
    let reply = app
        .expect(
            post(
                "/api/v1/auth/login",
                None,
                json!({ "username": DEFAULT_ADMIN_USERNAME, "password": ADMIN_PASSWORD }),
            ),
            StatusCode::OK,
        )
        .await;
    let cookie = reply.session_cookie().expect("login must set a cookie");
    let reply = app
        .expect(
            post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
            ),
            StatusCode::OK,
        )
        .await;
    reply
        .session_cookie()
        .expect("change-password must issue a new session")
}

/// Creates a user with an instance role (`admin`, `member`, `viewer`), signs them in, and
/// returns `(user id, session cookie)`. Created with `mustChangePassword: false`, so the
/// session is already past the forced-reset gate.
pub(crate) async fn user(app: &App, admin: &str, username: &str, role: &str) -> (String, String) {
    let id = app
        .expect(
            post(
                "/api/v1/users",
                Some(admin),
                json!({
                    "username": username,
                    "password": GOOD_PASSWORD,
                    "role": role,
                    "mustChangePassword": false,
                }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    let cookie = app
        .expect(
            post(
                "/api/v1/auth/login",
                None,
                json!({ "username": username, "password": GOOD_PASSWORD }),
            ),
            StatusCode::OK,
        )
        .await
        .session_cookie()
        .expect("login must set a cookie");
    (id, cookie)
}

/// Grants a user a role on a project.
pub(crate) async fn grant(app: &App, admin: &str, project_key: &str, user_id: &str, role: &str) {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/members"),
            Some(admin),
            json!({ "userId": user_id, "role": role }),
        ))
        .await;
    assert!(
        reply.status == StatusCode::CREATED || reply.status == StatusCode::OK,
        "{}",
        reply.raw_body
    );
}

/// A project created from a template, with handles on its seeded config.
pub(crate) struct Project {
    pub(crate) key: String,
    pub(crate) id: String,
    pub(crate) card_type: String,
    pub(crate) statuses: Vec<Value>,
}

impl Project {
    /// A status's id by name.
    pub(crate) fn status(&self, name: &str) -> String {
        self.statuses
            .iter()
            .find(|s| s["name"] == name)
            .map_or_else(
                || panic!("no status {name:?} in {}", self.key),
                |s| text(s, "id"),
            )
    }

    /// The first status of a category (`todo`, `in_progress`, `done`).
    pub(crate) fn status_in(&self, category: &str) -> String {
        self.statuses
            .iter()
            .find(|s| s["category"] == category)
            .map_or_else(
                || panic!("no {category} status in {}", self.key),
                |s| text(s, "id"),
            )
    }
}

/// Creates a project from a template (`programming`, `3d-modeling`, `job-search`, `blank`).
pub(crate) async fn project(app: &App, admin: &str, key: &str, template: &str) -> Project {
    let created = app
        .expect(
            post(
                "/api/v1/projects",
                Some(admin),
                json!({ "key": key, "name": key, "template": template }),
            ),
            StatusCode::CREATED,
        )
        .await;
    let types = app
        .expect(
            get(&format!("/api/v1/projects/{key}/card-types"), Some(admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let card_type = types
        .iter()
        .find(|t| t["isDefault"].as_bool() == Some(true))
        .map_or_else(
            || panic!("{key} has no default card type"),
            |t| text(t, "id"),
        );
    let statuses = app
        .expect(
            get(&format!("/api/v1/projects/{key}/statuses"), Some(admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    Project {
        key: key.to_owned(),
        id: created.id(),
        card_type,
        statuses,
    }
}

/// Creates a card and returns its key.
pub(crate) async fn card(app: &App, cookie: &str, project: &Project, summary: &str) -> String {
    app.expect(
        post(
            &format!("/api/v1/projects/{}/cards", project.key),
            Some(cookie),
            json!({ "typeId": project.card_type, "summary": summary }),
        ),
        StatusCode::CREATED,
    )
    .await
    .key()
}

/// Reads a card back.
pub(crate) async fn read_card(app: &App, cookie: &str, key: &str) -> Value {
    app.expect(
        get(&format!("/api/v1/cards/{key}"), Some(cookie)),
        StatusCode::OK,
    )
    .await
    .json()
}

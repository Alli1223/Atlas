//! End-to-end tests for the GitHub integration's HTTP surface, over the real
//! router, middleware stack, and database.
//!
//! These cover the paths that do **not** need to reach `api.github.com`: the
//! guards that fire before any outbound call (no repo linked, unknown or
//! non-GitHub credential, a card with no links). The happy paths that create a
//! real branch are exercised manually against a real PAT + repo — the same
//! posture as the credential-validation tests, which also call GitHub for real.
//!
//! The project-access gating of every route (an outsider gets a 404) is proven
//! exhaustively by `every_project_scoped_route_refuses_an_outsider` in
//! `tests/project_access.rs`; here the caller is always the instance admin, who is
//! past the gate, so the assertions are about the handler logic itself.

use atlas::api::{self, AppState};
use atlas::auth::seed::{self, DEFAULT_ADMIN_USERNAME};
use atlas::auth::session;
use atlas::config::{Config, SecretString};
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";
const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A migrated, seeded database with the vault live, and the router over it.
struct App {
    db: Db,
    config: Config,
    _temp: TempDb,
}

impl App {
    async fn new() -> Self {
        let temp = TempDb::new();
        let config = Config {
            master_key: Some(SecretString::new(MASTER_KEY)),
            ..temp.config()
        };
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

/// A response, with its body already read.
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

fn put(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PUT, uri, cookie, Some(body))
}

fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
}

/// Signs the admin in and past the forced-reset gate; returns the session cookie.
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

/// Creates a project and returns the id of its first card type.
async fn create_project(app: &App, admin: &str, key: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": "programming" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(get(
            &format!("/api/v1/projects/{key}/card-types"),
            Some(admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.json()[0]["id"]
        .as_str()
        .expect("a card type")
        .to_owned()
}

/// Creates a card and returns its key.
async fn create_card(app: &App, admin: &str, project_key: &str, type_id: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(admin),
            json!({ "typeId": type_id, "summary": "Add login" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("a card key").to_owned()
}

/// Stores a credential and returns its id.
async fn store_credential(app: &App, admin: &str, provider: &str, secret: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(admin),
            json!({ "provider": provider, "label": "test", "secret": secret }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unlinked_project_reports_no_repo_and_cannot_be_unlinked() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_project(&app, &admin, "ATLAS").await;

    let reply = app
        .send(get("/api/v1/projects/ATLAS/repo", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    // Unlinking when nothing is linked is a 404, not a silent success.
    let reply = app
        .send(delete("/api/v1/projects/ATLAS/repo", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_fresh_cards_git_links_are_an_empty_list() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    let reply = app
        .send(get(
            &format!("/api/v1/cards/{card}/git-links"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json(), json!([]), "a fresh card has no git links");

    app.db.close().await;
}

#[tokio::test]
async fn creating_a_branch_without_a_linked_repo_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    // No repo is linked to the project, so there is nothing to branch on — a 409
    // that never reaches GitHub, rather than a 500.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card}/branch"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn linking_rejects_an_unknown_or_non_github_credential() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_project(&app, &admin, "ATLAS").await;

    // An id that names no credential at all.
    let reply = app
        .send(put(
            "/api/v1/projects/ATLAS/repo",
            Some(&admin),
            json!({ "credentialId": "no-such-id", "owner": "octocat", "repo": "hello" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // A real credential, but for the wrong provider: it cannot drive a repo link,
    // and this is caught before any GitHub call.
    let gemini = store_credential(&app, &admin, "gemini", "ya29.not-a-github-token").await;
    let reply = app
        .send(put(
            "/api/v1/projects/ATLAS/repo",
            Some(&admin),
            json!({ "credentialId": gemini, "owner": "octocat", "repo": "hello" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn the_repo_picker_rejects_a_missing_or_non_github_credential() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(get("/api/v1/credentials/no-such-id/repos", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    // A gemini credential is not a GitHub credential, so the picker treats it as
    // absent rather than calling GitHub with it.
    let gemini = store_credential(&app, &admin, "gemini", "ya29.not-a-github-token").await;
    let reply = app
        .send(get(
            &format!("/api/v1/credentials/{gemini}/repos"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

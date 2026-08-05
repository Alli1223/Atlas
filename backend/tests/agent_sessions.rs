//! End-to-end tests for "Run Claude Code against this card", over the real router,
//! middleware stack, and database.
//!
//! `agent::orchestrator`'s own unit tests prove the wiring itself (workspace → spawn → persist
//! → drain → finish) against a fake runner and workspace preparer. These prove the HTTP layer
//! on top of it: routes are reachable, the right project role gates them, and the two guards
//! that fire before any subprocess is spawned (no session, no linked repo) surface as the
//! right status codes through a real request.
//!
//! The project-access gating of every route (an outsider gets a 404) is proven exhaustively by
//! `every_project_scoped_route_refuses_an_outsider` in `tests/project_access.rs`; here the
//! caller is always the instance admin, who is past the gate.

use std::future::Future;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use atlas::agent::runner::{AgentRunner, LocalRunner};
use atlas::agent::workspace::WorkspacePreparer;
use atlas::api::{self, AppState};
use atlas::auth::seed::DEFAULT_ADMIN_USERNAME;
use atlas::config::{Config, SecretString};
use atlas::db::{self, Db};
use atlas::error::AppResult;
use atlas::secrets::vault::Vault;
use atlas::test_support::TempDb;
use atlas::{auth::seed, auth::session};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";
const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A private temporary directory, removed on drop — the same small helper every subprocess
/// test module in this codebase writes for itself rather than pulling in a `tempfile`
/// dependency (see `agent::runner`'s test module for the original).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("atlas-agent-sessions-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("failed to create a temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes an executable shell script standing in for the `claude` CLI.
fn fake_program(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-claude");
    let mut file = std::fs::File::create(&path).expect("failed to create the fake program file");
    writeln!(file, "#!/bin/sh\n{body}").expect("failed to write the fake program");
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("failed to make the fake program executable");
    path
}

/// A [`WorkspacePreparer`] that hands back a fixed local directory, touching no database, no
/// vault, and no network — so a test can exercise the "Run with Claude" endpoint without a
/// real GitHub credential or a linked repo at all.
struct FixedWorkspace(PathBuf);

impl WorkspacePreparer for FixedWorkspace {
    fn prepare<'a>(
        &'a self,
        _db: &'a Db,
        _vault: &'a Vault,
        _project_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = AppResult<PathBuf>> + Send + 'a>> {
        let path = self.0.clone();
        Box::pin(async move { Ok(path) })
    }
}

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

    /// The router with a fake [`AgentRunner`]/[`WorkspacePreparer`] swapped in, so a run
    /// actually "completes" without a real `claude` CLI, GitHub credential, or network call.
    fn router_with_fakes(&self, runner: Arc<dyn AgentRunner>, workspace: PathBuf) -> Router {
        let state = AppState {
            agent_runner: runner,
            workspace_preparer: Arc::new(FixedWorkspace(workspace)),
            ..AppState::new(self.db.clone(), self.config.clone())
        };
        api::router(state)
    }

    async fn send(&self, router: &Router, request: Request<Body>) -> Reply {
        let response = router
            .clone()
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

/// Signs the admin in and past the forced-reset gate; returns the session cookie.
async fn admin_past_the_gate(app: &App, router: &Router) -> String {
    let reply = app
        .send(
            router,
            post(
                "/api/v1/auth/login",
                None,
                json!({ "username": DEFAULT_ADMIN_USERNAME, "password": ADMIN_PASSWORD }),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let cookie = reply.session_cookie().expect("login must set a cookie");

    let reply = app
        .send(
            router,
            post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply
        .session_cookie()
        .expect("change-password must issue a new session")
}

/// Creates a project and returns the id of its first card type.
async fn create_project(app: &App, router: &Router, admin: &str, key: &str) -> String {
    let reply = app
        .send(
            router,
            post(
                "/api/v1/projects",
                Some(admin),
                json!({ "key": key, "name": key, "template": "programming" }),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(
            router,
            get(&format!("/api/v1/projects/{key}/card-types"), Some(admin)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.json()[0]["id"]
        .as_str()
        .expect("a card type")
        .to_owned()
}

/// Creates a card and returns its key.
async fn create_card(
    app: &App,
    router: &Router,
    admin: &str,
    project_key: &str,
    type_id: &str,
    summary: &str,
    description: Option<&str>,
) -> String {
    let mut body = json!({ "typeId": type_id, "summary": summary });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    let reply = app
        .send(
            router,
            post(
                &format!("/api/v1/projects/{project_key}/cards"),
                Some(admin),
                body,
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("a card key").to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn starting_a_run_without_a_linked_repo_is_a_conflict() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Fix the thing",
        None,
    )
    .await;

    // No repo is linked to the project, so there is no workspace to run in — the real
    // `GitWorkspacePreparer` refuses this before any subprocess is spawned.
    let reply = app
        .send(
            &router,
            post(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_fresh_cards_agent_sessions_are_an_empty_list() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Fix the thing",
        None,
    )
    .await;

    let reply = app
        .send(
            &router,
            get(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json(), json!([]));

    app.db.close().await;
}

#[tokio::test]
async fn an_unknown_session_id_is_not_found() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;

    let reply = app
        .send(
            &router,
            get("/api/v1/agent-sessions/no-such-session", Some(&admin)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn starting_a_run_sends_the_cards_summary_and_description_and_records_it_running() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Fix the thing",
        Some("Do the needful."),
    )
    .await;

    let scripts = TempDir::new();
    let program = fake_program(&scripts.0, "sleep 60");
    let fake_router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        std::env::temp_dir(),
    );

    let reply = app
        .send(
            &fake_router,
            post(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_eq!(reply.json()["status"], "running");
    assert_eq!(reply.json()["prompt"], "Fix the thing\n\nDo the needful.");
    let session_id = reply.json()["id"].as_str().unwrap().to_owned();

    // Polling it back by id, and via the card's session list, both see the same thing.
    let reply = app
        .send(
            &fake_router,
            get(
                &format!("/api/v1/agent-sessions/{session_id}"),
                Some(&admin),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["id"], session_id);

    let reply = app
        .send(
            &fake_router,
            get(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json().as_array().unwrap().len(), 1);
    assert_eq!(reply.json()[0]["id"], session_id);

    app.db.close().await;
}

#[tokio::test]
async fn a_run_that_finishes_is_reflected_on_the_next_poll() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Fix the thing",
        None,
    )
    .await;

    let scripts = TempDir::new();
    let program = fake_program(
        &scripts.0,
        r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"whatever","result":"done","total_cost_usd":0.05,"terminal_reason":"completed"}'"#,
    );
    let fake_router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        std::env::temp_dir(),
    );

    let reply = app
        .send(
            &fake_router,
            post(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let session_id = reply.json()["id"].as_str().unwrap().to_owned();

    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reply = app
                .send(
                    &fake_router,
                    get(
                        &format!("/api/v1/agent-sessions/{session_id}"),
                        Some(&admin),
                    ),
                )
                .await;
            if reply.json()["status"] != "running" {
                return reply;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session must reach a terminal status");

    assert_eq!(finished.json()["status"], "completed");
    assert_eq!(finished.json()["resultText"], "done");
    assert_eq!(finished.json()["totalCostUsd"], 0.05);

    app.db.close().await;
}

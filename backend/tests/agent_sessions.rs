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
async fn selecting_codex_persists_the_provider_and_interprets_its_terminal_event() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(&app, &router, &admin, "ATLAS", &type_id, "Task", None).await;
    let scripts = TempDir::new();
    let program = fake_program(
        &scripts.0,
        r#"
        echo '{"type":"item.completed","item":{"type":"agent_message","text":"Implemented."}}'
        echo '{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}'
    "#,
    );
    let router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        scripts.0.clone(),
    );
    let reply = app
        .send(
            &router,
            post(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
                json!({"provider":"codex"}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_eq!(reply.json()["provider"], "codex");
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reply = app
                .send(
                    &router,
                    get(&format!("/api/v1/agent-sessions/{id}"), Some(&admin)),
                )
                .await;
            if reply.json()["status"] != "running" {
                break reply.json();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["status"], "completed");
    assert_eq!(finished["resultText"], "Implemented.");
    assert_eq!(
        finished["totalCostUsd"],
        Value::Null,
        "Do not invent costs for CLI subscriptions"
    );
}

#[tokio::test]
async fn an_unknown_provider_is_rejected_instead_of_starting_claude() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    let card = create_card(&app, &router, &admin, "ATLAS", &type_id, "Task", None).await;
    let reply = app
        .send(
            &router,
            post(
                &format!("/api/v1/cards/{card}/agent-sessions"),
                Some(&admin),
                json!({"provider":"not-a-provider"}),
            ),
        )
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );
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

    // The transcript write happens before the outcome write in the same detached task, so by
    // the time the poll above observed a terminal status, the transcript is already there too.
    let transcript = app
        .send(
            &fake_router,
            get(
                &format!("/api/v1/agent-sessions/{session_id}/transcript"),
                Some(&admin),
            ),
        )
        .await;
    assert_eq!(transcript.status, StatusCode::OK, "{}", transcript.raw_body);
    let lines = transcript.json();
    let lines = lines.as_array().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["seq"], 0);
    assert!(
        lines[0]["line"]
            .as_str()
            .unwrap()
            .contains(r#""result":"done""#)
    );

    app.db.close().await;
}

#[tokio::test]
async fn an_unknown_sessions_transcript_is_not_found() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;

    let reply = app
        .send(
            &router,
            get(
                "/api/v1/agent-sessions/no-such-session/transcript",
                Some(&admin),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn cancelling_a_running_session_eventually_shows_it_cancelled() {
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
    // Never produces a result event on its own — only cancellation ends this run.
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
    let session_id = reply.json()["id"].as_str().unwrap().to_owned();

    let reply = app
        .send(
            &fake_router,
            post(
                &format!("/api/v1/agent-sessions/{session_id}/cancel"),
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);

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

    assert_eq!(finished.json()["status"], "cancelled");

    app.db.close().await;
}

#[tokio::test]
async fn cancelling_an_unknown_session_is_not_found() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;

    let reply = app
        .send(
            &router,
            post(
                "/api/v1/agent-sessions/no-such-session/cancel",
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn cancelling_a_session_that_already_finished_is_a_conflict() {
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
        r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"whatever","total_cost_usd":0.01,"terminal_reason":"completed"}'"#,
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

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
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
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session must reach a terminal status");

    let reply = app
        .send(
            &fake_router,
            post(
                &format!("/api/v1/agent-sessions/{session_id}/cancel"),
                Some(&admin),
                json!({}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

async fn auto_fixture() -> (App, TempDir, Router, String) {
    let app = App::new().await;
    let scripts = TempDir::new();
    let program = fake_program(
        &scripts.0,
        r#"
        case "$*" in
          *"You manage this project board"*)
            echo '{"type":"result","subtype":"success","is_error":false,"result":"{\"cardKey\":\"ATLAS-1\",\"provider\":\"codex\",\"reason\":\"Build the foundation first\",\"blocked\":[{\"cardKey\":\"ATLAS-2\",\"blockedBy\":\"ATLAS-1\",\"reason\":\"Requires the foundation\"}]}","num_turns":1,"session_id":"plan","total_cost_usd":0,"terminal_reason":"completed"}'
            ;;
          *)
            sleep 3
            echo '{"type":"item.completed","item":{"type":"agent_message","text":"Implemented and checked"}}'
            echo '{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20,"cached_input_tokens":80}}'
            ;;
        esac
    "#,
    );
    let router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        scripts.0.clone(),
    );
    let admin = admin_past_the_gate(&app, &router).await;
    let type_id = create_project(&app, &router, &admin, "ATLAS").await;
    create_card(&app, &router, &admin, "ATLAS", &type_id, "Foundation", None).await;
    create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Dependent feature",
        None,
    )
    .await;
    create_card(
        &app,
        &router,
        &admin,
        "ATLAS",
        &type_id,
        "Independent",
        None,
    )
    .await;
    (app, scripts, router, admin)
}

async fn wait_for_auto_field(
    app: &App,
    router: &Router,
    admin: &str,
    field: &str,
    value: &str,
) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let job = app
                .send(router, get("/api/v1/projects/ATLAS/auto", Some(admin)))
                .await
                .json();
            assert!(job["status"] != "failed", "coordinator failed: {job}");
            if job[field] == value {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Auto must reach the requested state")
}

#[tokio::test]
async fn auto_assigns_a_worker_and_tracks_blockers_and_review() {
    let (app, _scripts, router, admin) = auto_fixture().await;
    let response = app
        .send(
            &router,
            post(
                "/api/v1/projects/ATLAS/auto",
                Some(&admin),
                json!({"provider":"claude", "maxTasks":3}),
            ),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::CREATED,
        "{}",
        response.raw_body
    );
    let duplicate = app
        .send(
            &router,
            post(
                "/api/v1/projects/ATLAS/auto",
                Some(&admin),
                json!({"provider":"claude", "maxTasks":1}),
            ),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    wait_for_auto_field(&app, &router, &admin, "currentCardKey", "ATLAS-1").await;
    let active_lane: String = sqlx::query_scalar(
        "SELECT s.name FROM cards c JOIN statuses s ON s.id = c.status_id WHERE c.key = 'ATLAS-1'",
    )
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(active_lane, "In Progress");
    assert_eq!(
        app.send(
            &router,
            post("/api/v1/projects/ATLAS/auto/stop", Some(&admin), json!({}))
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let job = wait_for_auto_field(&app, &router, &admin, "status", "stopped").await;
    assert_eq!(job["completedTasks"], 1);
    assert_auto_ticket_effects(&app, &router, &admin).await;
    let activity = app
        .send(
            &router,
            get("/api/v1/projects/ATLAS/auto/activity", Some(&admin)),
        )
        .await;
    assert_eq!(activity.status, StatusCode::OK);
    assert!(
        activity
            .json()
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["provider"] == "codex" && event["sessionId"].is_string())
    );
    let usage = app
        .send(&router, get("/api/v1/agent-usage", Some(&admin)))
        .await;
    assert_eq!(usage.status, StatusCode::OK);
    assert!(
        usage
            .json()
            .as_array()
            .unwrap()
            .iter()
            .any(|usage| usage["provider"] == "codex" && usage["inputTokens"] == 100)
    );

    app.db.close().await;
}

async fn assert_auto_ticket_effects(app: &App, router: &Router, admin: &str) {
    let first = app
        .send(router, get("/api/v1/cards/ATLAS-1/automation", Some(admin)))
        .await
        .json();
    assert_eq!(first["assignment"]["provider"], "codex");
    let second = app
        .send(router, get("/api/v1/cards/ATLAS-2/automation", Some(admin)))
        .await
        .json();
    assert_eq!(second["blockedBy"][0]["key"], "ATLAS-1");
    assert_eq!(second["blockedBy"][0]["done"], false);
    let lanes: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.key, s.name FROM cards c JOIN statuses s ON s.id = c.status_id ORDER BY c.key",
    )
    .fetch_all(app.db.reader())
    .await
    .expect("failed to read ticket lanes");
    assert!(lanes.contains(&("ATLAS-1".to_owned(), "In Review".to_owned())));
    assert!(lanes.contains(&("ATLAS-2".to_owned(), "Blocked".to_owned())));
    let blocked_run = app
        .send(
            router,
            post(
                "/api/v1/cards/ATLAS-2/agent-sessions",
                Some(admin),
                json!({"provider":"claude"}),
            ),
        )
        .await;
    assert_eq!(blocked_run.status, StatusCode::CONFLICT);
    let cycle = app
        .send(
            router,
            post(
                "/api/v1/cards/ATLAS-1/dependencies",
                Some(admin),
                json!({"blockedBy":"ATLAS-2", "reason":"Circular"}),
            ),
        )
        .await;
    assert_eq!(cycle.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_dependency_cannot_reference_another_projects_ticket() {
    let app = App::new().await;
    let router = app.router();
    let admin = admin_past_the_gate(&app, &router).await;
    let first_type = create_project(&app, &router, &admin, "FIRST").await;
    let second_type = create_project(&app, &router, &admin, "OTHER").await;
    create_card(&app, &router, &admin, "FIRST", &first_type, "Task", None).await;
    create_card(
        &app,
        &router,
        &admin,
        "OTHER",
        &second_type,
        "Private task",
        None,
    )
    .await;
    let response = app
        .send(
            &router,
            post(
                "/api/v1/cards/FIRST-1/dependencies",
                Some(&admin),
                json!({"blockedBy":"OTHER-1", "reason":"Must not cross scope"}),
            ),
        )
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_dependencies")
        .fetch_one(app.db.reader())
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.db.close().await;
}

#[tokio::test]
async fn a_worker_reported_blocker_is_attached_to_its_ticket() {
    let (app, scripts, _router, admin) = auto_fixture().await;
    let program = fake_program(
        &scripts.0,
        r#"
        echo '{"type":"result","subtype":"success","is_error":false,"result":"ATLAS_BLOCKED_BY: ATLAS-2 | Need its implementation first","num_turns":1,"session_id":"blocked","total_cost_usd":0,"terminal_reason":"completed"}'
    "#,
    );
    let router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        scripts.0.clone(),
    );
    let response = app
        .send(
            &router,
            post(
                "/api/v1/cards/ATLAS-1/agent-sessions",
                Some(&admin),
                json!({"provider":"claude"}),
            ),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::CREATED,
        "{}",
        response.raw_body
    );
    let id = response.json()["id"].as_str().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let finalized: bool = sqlx::query_scalar(
                "SELECT finalized_at IS NOT NULL FROM agent_sessions WHERE id = ?",
            )
            .bind(&id)
            .fetch_one(app.db.reader())
            .await
            .unwrap();
            if finalized {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let info = app
        .send(
            &router,
            get("/api/v1/cards/ATLAS-1/automation", Some(&admin)),
        )
        .await
        .json();
    assert_eq!(info["blockedBy"][0]["key"], "ATLAS-2");
    let lane: String = sqlx::query_scalar(
        "SELECT s.name FROM cards c JOIN statuses s ON s.id = c.status_id WHERE c.key = 'ATLAS-1'",
    )
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(lane, "Blocked");
    app.db.close().await;
}

#[tokio::test]
async fn auto_stops_when_workflow_rules_prevent_the_review_move() {
    let (app, scripts, _router, admin) = auto_fixture().await;
    let program = fake_program(
        &scripts.0,
        r#"
        case "$*" in
          *"You manage this project board"*)
            echo '{"type":"result","subtype":"success","is_error":false,"result":"{\"cardKey\":\"ATLAS-1\",\"provider\":\"claude\",\"reason\":\"Ready task\",\"blocked\":[]}","num_turns":1,"session_id":"plan","total_cost_usd":0,"terminal_reason":"completed"}' ;;
          *)
            echo '{"type":"result","subtype":"success","is_error":false,"result":"Checked","num_turns":1,"session_id":"worker","total_cost_usd":0,"terminal_reason":"completed"}' ;;
        esac
    "#,
    );
    let router = app.router_with_fakes(
        Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        scripts.0.clone(),
    );
    let card = atlas::domain::card::find_by_key(&app.db, "ATLAS-1")
        .await
        .unwrap()
        .unwrap();
    let statuses = atlas::domain::config::statuses(&app.db, &card.project_id)
        .await
        .unwrap();
    let ids: Vec<String> = statuses.iter().map(|status| status.id.clone()).collect();
    let mut tx = app.db.begin_write().await.unwrap();
    let strict = atlas::domain::workflow::insert(
        &mut tx,
        &card.project_id,
        "Strict workflow",
        false,
        &ids,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    atlas::domain::workflow::assign_card_types(
        &mut tx,
        &strict.id,
        &card.project_id,
        std::slice::from_ref(&card.type_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let response = app
        .send(
            &router,
            post(
                "/api/v1/projects/ATLAS/auto",
                Some(&admin),
                json!({"provider":"claude", "maxTasks":0}),
            ),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let job = app
                .send(&router, get("/api/v1/projects/ATLAS/auto", Some(&admin)))
                .await
                .json();
            if job["status"] == "failed" {
                assert!(job["message"].as_str().unwrap().contains("workflow"));
                assert_eq!(job["completedTasks"], 0);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_sessions")
        .fetch_one(app.db.reader())
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    app.db.close().await;
}

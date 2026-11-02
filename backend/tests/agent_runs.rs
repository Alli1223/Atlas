//! End-to-end tests for Phase 13's remaining pieces: per-project agent settings, the
//! concurrency cap and its persistent queue, completion actions (summary comment, moving the
//! card, notifying the starter), re-runs, the stale-run sweep, and the extra MCP tools.
//!
//! Runs use a shell script standing in for the `claude` CLI and a fixed local directory as the
//! workspace — no real subprocess of consequence, network call, or API cost.

mod common;

use std::future::Future;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use atlas::agent::mcp::{capability, tools};
use atlas::agent::runner::LocalRunner;
use atlas::agent::workspace::WorkspacePreparer;
use atlas::api::{self, AppState};
use atlas::config::SecretString;
use atlas::db::Db;
use atlas::domain::agent_session::{self, NewAgentSession};
use atlas::error::AppResult;
use atlas::secrets::vault::Vault;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{Value, json};
use tower::ServiceExt;

const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

const COMPLETES: &str = r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"session_id":"s","result":"Fixed the bug and added a test.","total_cost_usd":0.42,"duration_ms":1234,"usage":{"input_tokens":10},"terminal_reason":"completed"}'"#;

struct Scripts(PathBuf);

impl Scripts {
    fn new(body: &str) -> (Self, PathBuf) {
        let dir = std::env::temp_dir().join(format!("atlas-agent-runs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("fake-claude");
        let mut file = std::fs::File::create(&path).expect("script");
        writeln!(file, "#!/bin/sh\n{body}").expect("write");
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (Self(dir), path)
    }
}

impl Drop for Scripts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// An app with the vault live, and one long-lived state with a fake CLI swapped in — kept
/// across requests so the cancel registry and live buffers are shared, as in production.
async fn app(max_concurrent: usize) -> App {
    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    config.master_key = Some(SecretString::new(MASTER_KEY));
    config.agent_max_concurrent = max_concurrent;
    App::with_config(temp, config).await
}

fn fake_state(app: &App, program: &Path) -> AppState {
    AppState {
        agent_runner: Arc::new(LocalRunner::with_program(program.to_string_lossy())),
        workspace_preparer: Arc::new(FixedWorkspace(std::env::temp_dir())),
        ..app.state()
    }
}

async fn send(router: &Router, request: Request<Body>) -> Reply {
    Reply::from(router.clone().oneshot(request).await.expect("request")).await
}

async fn start(router: &Router, cookie: &str, key: &str) -> Value {
    let reply = send(
        router,
        post(
            &format!("/api/v1/cards/{key}/agent-sessions"),
            Some(cookie),
            json!({}),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()
}

async fn wait_until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn a_completed_run_reports_moves_its_card_and_tells_its_starter() {
    let app = app(2).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "RUN", "programming").await;
    let key = card(&app, &admin, &p, "Fix the bug").await;
    let (_scripts, program) = Scripts::new(COMPLETES);
    let router = api::router(fake_state(&app, &program));

    let session = start(&router, &admin, &key).await;
    let id = text(&session, "id");

    wait_until("the summary comment", || async {
        let comments = app
            .expect(
                get(&format!("/api/v1/cards/{key}/comments"), Some(&admin)),
                StatusCode::OK,
            )
            .await
            .rows();
        !comments.is_empty()
    })
    .await;
    let comments = app
        .expect(
            get(&format!("/api/v1/cards/{key}/comments"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let body = text(&comments[0], "body");
    assert!(body.contains("Fixed the bug and added a test."), "{body}");
    assert!(body.contains("$0.42") && body.contains("3 turns"), "{body}");

    wait_until("the card to move to In Review", || async {
        read_card(&app, &admin, &key).await["statusId"] == p.status("In Review").as_str()
    })
    .await;

    let finished = app
        .expect(
            get(&format!("/api/v1/agent-sessions/{id}"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(finished["status"], "completed");
    assert_eq!(finished["durationMs"], 1234);
    assert!(
        finished["usage"]
            .as_str()
            .unwrap_or_default()
            .contains("input_tokens"),
        "{finished}"
    );

    wait_until("the starter's notification", || async {
        let bell = app
            .expect(get("/api/v1/notifications", Some(&admin)), StatusCode::OK)
            .await
            .json();
        bell["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|n| n["kind"] == "agent"))
    })
    .await;

    // The comment was written with an agent actor tag.
    let actor: Option<String> = sqlx::query_scalar("SELECT actor FROM comments LIMIT 1")
        .fetch_one(app.db.reader())
        .await
        .expect("actor");
    assert_eq!(actor.as_deref(), Some(format!("agent:{id}").as_str()));

    // A re-run is a new session that remembers what it re-ran.
    let rerun = send(
        &router,
        post_empty(&format!("/api/v1/agent-sessions/{id}/rerun"), Some(&admin)),
    )
    .await;
    assert_eq!(rerun.status, StatusCode::CREATED, "{}", rerun.raw_body);
    assert_eq!(rerun.json()["rerunOf"], id.as_str());
    assert_eq!(rerun.json()["prompt"], finished["prompt"]);
}

#[tokio::test]
async fn runs_past_the_cap_queue_and_launch_when_a_slot_frees() {
    let app = app(1).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "Q", "programming").await;
    let a = card(&app, &admin, &p, "First").await;
    let b = card(&app, &admin, &p, "Second").await;
    let (_scripts, program) = Scripts::new("sleep 60");
    let state = fake_state(&app, &program);
    let router = api::router(state.clone());

    let first = start(&router, &admin, &a).await;
    assert_eq!(first["queued"], false);
    let second = start(&router, &admin, &b).await;
    assert_eq!(second["queued"], true, "{second}");
    assert_eq!(second["status"], "running");

    // Nothing launches while the slot is taken.
    assert_eq!(
        atlas::agent::dispatch::dispatch_queued(&state)
            .await
            .expect("dispatch"),
        0
    );

    let first_id = text(&first, "id");
    let reply = send(
        &router,
        post_empty(
            &format!("/api/v1/agent-sessions/{first_id}/cancel"),
            Some(&admin),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);
    wait_until("the first run to stop", || async {
        app.expect(
            get(&format!("/api/v1/agent-sessions/{first_id}"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json()["status"]
            == "cancelled"
    })
    .await;

    assert_eq!(
        atlas::agent::dispatch::dispatch_queued(&state)
            .await
            .expect("dispatch"),
        1
    );
    let second_id = text(&second, "id");
    let now_running = app
        .expect(
            get(&format!("/api/v1/agent-sessions/{second_id}"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(now_running["queued"], false);

    // While the second holds the only slot, a third queues — and cancelling a *queued* run
    // needs no process at all.
    let third = start(&router, &admin, &a).await;
    assert_eq!(third["queued"], true);
    let third_id = text(&third, "id");
    let reply = send(
        &router,
        post_empty(
            &format!("/api/v1/agent-sessions/{third_id}/cancel"),
            Some(&admin),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);
    let cancelled = app
        .expect(
            get(&format!("/api/v1/agent-sessions/{third_id}"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["queued"], false);

    // Clean up the second.
    send(
        &router,
        post_empty(
            &format!("/api/v1/agent-sessions/{second_id}/cancel"),
            Some(&admin),
        ),
    )
    .await;
}

#[tokio::test]
async fn risky_settings_need_an_owner_and_an_explicit_acknowledgement() {
    let app = app(2).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "SET", "programming").await;
    let (mia_id, mia) = user(&app, &admin, "mia", "member").await;
    grant(&app, &admin, "SET", &mia_id, "member").await;

    let defaults = app
        .expect(
            get("/api/v1/projects/SET/agent-settings", Some(&mia)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(defaults["permissionMode"], "dontAsk");
    assert_eq!(defaults["useApiKey"], false);

    let mut bypass = defaults.clone();
    bypass["permissionMode"] = json!("bypassPermissions");
    app.expect(
        put(
            "/api/v1/projects/SET/agent-settings",
            Some(&mia),
            json!({ "settings": bypass, "acknowledgeRisk": true }),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    app.expect(
        put(
            "/api/v1/projects/SET/agent-settings",
            Some(&admin),
            json!({ "settings": bypass }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let saved = app
        .expect(
            put(
                "/api/v1/projects/SET/agent-settings",
                Some(&admin),
                json!({ "settings": bypass, "acknowledgeRisk": true }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(saved["permissionMode"], "bypassPermissions");

    let mut backlog = defaults;
    backlog["destinationStatusId"] = json!(p.status("To Do"));
    let saved = app
        .expect(
            put(
                "/api/v1/projects/SET/agent-settings",
                Some(&admin),
                json!({ "settings": backlog }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(
        saved["destinationStatusId"],
        p.status("To Do").as_str(),
        "backlog is a selectable destination"
    );
}

#[tokio::test]
async fn a_restart_fails_runs_left_executing_but_keeps_the_queue() {
    let app = app(2).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "SWP", "blank").await;
    let key = card(&app, &admin, &p, "Interrupted").await;
    let found = atlas::domain::card::find_by_key(&app.db, &key)
        .await
        .expect("q")
        .expect("card");
    let mut tx = app.db.begin_write().await.expect("tx");
    let stale = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id: &found.id,
            claude_session_id: "a",
            prompt: "p",
            started_by: None,
        },
        atlas::test_support::now(),
    )
    .await
    .expect("insert");
    let waiting = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id: &found.id,
            claude_session_id: "b",
            prompt: "p",
            started_by: None,
        },
        atlas::test_support::now(),
    )
    .await
    .expect("insert");
    agent_session::mark_queued(&mut tx, &waiting.id, "{}")
        .await
        .expect("queue");
    tx.commit().await.expect("commit");

    let swept = atlas::agent::dispatch::sweep_stale(&app.state())
        .await
        .expect("sweep");
    assert_eq!(swept, 1);
    let stale = agent_session::find_by_id(&app.db, &stale.id)
        .await
        .expect("q")
        .expect("row");
    assert_eq!(stale.status.to_string(), "failed");
    assert!(
        stale
            .error_message
            .unwrap_or_default()
            .contains("restarted")
    );
    let waiting = agent_session::find_by_id(&app.db, &waiting.id)
        .await
        .expect("q")
        .expect("row");
    assert!(waiting.status.is_running() && waiting.queued);
}

#[tokio::test]
async fn the_new_mcp_tools_act_on_the_session_card_only() {
    let app = app(2).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "MCP", "programming").await;
    let key = card(&app, &admin, &p, "Parent story").await;
    let admin_id = app
        .expect(get("/api/v1/auth/me", Some(&admin)), StatusCode::OK)
        .await
        .id();
    let found = atlas::domain::card::find_by_key(&app.db, &key)
        .await
        .expect("q")
        .expect("card");
    let mut tx = app.db.begin_write().await.expect("tx");
    let session = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id: &found.id,
            claude_session_id: "c",
            prompt: "p",
            started_by: Some(&admin_id),
        },
        atlas::test_support::now(),
    )
    .await
    .expect("insert");
    let minted = capability::mint(&mut tx, &session.id, atlas::test_support::now())
        .await
        .expect("mint");
    tx.commit().await.expect("commit");
    let resolved = capability::resolve(&app.db, &minted.token)
        .await
        .expect("resolve")
        .expect("live");
    let ctx = tools::ToolContext::build(&app.db, resolved)
        .await
        .expect("ctx");

    let created = tools::call(
        &ctx,
        "atlas_create_card",
        &json!({ "summary": "Follow-up: flaky test" }),
    )
    .await
    .expect("create");
    let created = created["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(created.starts_with("Created MCP-"), "{created}");
    let children = app
        .expect(
            get(&format!("/api/v1/cards/{key}/children"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(
        children.as_array().map(Vec::len),
        Some(1),
        "a follow-up is a child of the session card"
    );

    tools::call(&ctx, "atlas_add_tag", &json!({ "name": "needs-review" }))
        .await
        .expect("tag");
    let tags = app
        .expect(
            get(&format!("/api/v1/cards/{key}/tags"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert!(tags.iter().any(|t| t["name"] == "needs-review"));
    assert!(
        tools::call(&ctx, "atlas_add_tag", &json!({ "name": "two words" }))
            .await
            .is_err()
    );

    tools::call(
        &ctx,
        "atlas_set_status",
        &json!({ "status": "In Progress" }),
    )
    .await
    .expect("status");
    assert_eq!(
        read_card(&app, &admin, &key).await["statusId"],
        p.status("In Progress").as_str()
    );

    // Every write carries the agent's actor tag.
    let actors: Vec<Option<String>> =
        sqlx::query_scalar("SELECT actor FROM card_history WHERE field = 'status'")
            .fetch_all(app.db.reader())
            .await
            .expect("history");
    assert!(
        actors
            .iter()
            .all(|a| a.as_deref() == Some(format!("agent:{}", session.id).as_str())),
        "{actors:?}"
    );
}

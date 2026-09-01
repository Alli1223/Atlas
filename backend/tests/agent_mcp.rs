//! The Atlas MCP server's security boundary and tool behaviour.
//!
//! A Claude Code run reaches these tools with a per-session capability token, and the token is
//! the whole authorisation boundary. These tests prove the properties that would be a breach
//! if false:
//!
//! - a token is valid only while its session is running, and dies the moment it ends;
//! - a tool acts only on the session's card — there is no argument that could name another;
//! - authority is the starting user's, re-checked at call time (a viewer cannot move);
//! - a move goes through the workflow, so an illegal target is refused;
//! - the token is stored hashed and never surfaces in an API response.

use atlas::agent::mcp::capability;
use atlas::agent::mcp::tools::{self, ToolContext};
use atlas::api::{self, AppState};
use atlas::auth::seed::DEFAULT_ADMIN_USERNAME;
use atlas::auth::session;
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::domain::agent_session::{
    self, AgentSession, AgentSessionStatus, NewAgentSession, SessionOutcome,
};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

struct App {
    db: Db,
    config: Config,
    _temp: TempDb,
}

impl App {
    async fn new() -> Self {
        let temp = TempDb::new();
        let config = temp.config();
        let db = Db::connect(&config).await.expect("open database");
        db::migrate::run(&db).await.expect("migrate");
        atlas::auth::seed::ensure_default_admin(&db)
            .await
            .expect("seed admin");
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
        let response = self.router().oneshot(request).await.expect("request");
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
            .expect("read body");
        Self {
            status,
            set_cookie,
            raw_body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.raw_body)
            .unwrap_or_else(|err| panic!("body not JSON ({err}): {}", self.raw_body))
    }

    fn str_field(&self, key: &str) -> String {
        self.json()[key]
            .as_str()
            .unwrap_or_else(|| panic!("no {key} in {}", self.raw_body))
            .to_owned()
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

// Takes the body by value so callers can pass a `json!` literal directly.
#[allow(clippy::needless_pass_by_value)]
fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
    }
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("build request")
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
    let cookie = reply.session_cookie().expect("login cookie");
    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.session_cookie().expect("new session")
}

/// Creates a user of the given instance role, logs them in, and returns `(id, cookie)`.
async fn user(app: &App, admin: &str, username: &str, role: &str) -> (String, String) {
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(admin),
            json!({
                "username": username,
                "displayName": username,
                "password": GOOD_PASSWORD,
                "role": role,
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let id = reply.str_field("id");
    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    (id, reply.session_cookie().expect("cookie"))
}

/// The signed-in user's id, from `/auth/me`.
async fn me(app: &App, cookie: &str) -> String {
    let reply = app
        .send(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/auth/me")
                .header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    reply.str_field("id")
}

/// Seeds a programming-template project (permissive default workflow) and returns its key.
async fn project(app: &App, admin: &str, key: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": "programming" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    key.to_owned()
}

async fn add_member(app: &App, admin: &str, project_key: &str, user_id: &str, role: &str) {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/members"),
            Some(admin),
            json!({ "userId": user_id, "role": role }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

/// Creates a card (of the project's first card type) and returns `(id, key)`.
async fn card(app: &App, cookie: &str, project_key: &str, summary: &str) -> (String, String) {
    let types = app
        .send(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/projects/{project_key}/card-types"))
                .header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    let type_id = types.json()[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no card type: {}", types.raw_body))
        .to_owned();

    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(cookie),
            json!({ "summary": summary, "typeId": type_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    (reply.str_field("id"), reply.str_field("key"))
}

/// Inserts a running agent session for a card and mints its capability, returning
/// `(session_id, token)`.
async fn session_with_token(app: &App, card_id: &str, started_by: &str) -> (String, String) {
    let mut tx = app.db.begin_write().await.expect("begin");
    let session = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id,
            claude_session_id: "cli-session",
            prompt: "do the thing",
            started_by: Some(started_by),
        },
        Utc::now(),
    )
    .await
    .expect("insert session");
    let minted = capability::mint(&mut tx, &session.id, Utc::now())
        .await
        .expect("mint");
    tx.commit().await.expect("commit");
    (session.id, minted.token)
}

async fn finish_session(app: &App, session: &AgentSession) {
    let mut tx = app.db.begin_write().await.expect("begin");
    agent_session::finish(
        &mut tx,
        session,
        &SessionOutcome {
            status: AgentSessionStatus::Completed,
            result_text: Some("done"),
            total_cost_usd: Some(0.0),
            num_turns: Some(1),
            error_message: None,
        },
        Utc::now(),
    )
    .await
    .expect("finish");
    tx.commit().await.expect("commit");
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_running_sessions_token_resolves_but_a_finished_one_is_dead() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let key = project(&app, &admin, "PROJ").await;
    let (card_id, _card_key) = card(&app, &admin, &key, "a task").await;

    // The admin is an implicit owner, so it can start a session and act.
    let admin_id = me(&app, &admin).await;
    let (session_id, token) = session_with_token(&app, &card_id, &admin_id).await;

    // While running, it resolves to the session.
    let resolved = capability::resolve(&app.db, &token)
        .await
        .expect("resolve")
        .expect("a running session resolves");
    assert_eq!(resolved.id, session_id);

    // Finish it, and the very same token is now dead — before revoke has even run.
    finish_session(&app, &resolved).await;
    assert!(
        capability::resolve(&app.db, &token)
            .await
            .expect("resolve")
            .is_none(),
        "a finished session's token must not resolve"
    );
}

#[tokio::test]
async fn an_unknown_token_never_resolves() {
    let app = App::new().await;
    assert!(
        capability::resolve(&app.db, "not-a-real-token")
            .await
            .expect("resolve")
            .is_none()
    );
}

#[tokio::test]
async fn the_token_is_stored_hashed_not_in_the_clear() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let key = project(&app, &admin, "PROJ").await;
    let (card_id, _) = card(&app, &admin, &key, "a task").await;
    let (session_id, token) = session_with_token(&app, &card_id, &me(&app, &admin).await).await;

    // The stored key is the token's SHA-256, not the token. A read of the table yields nothing
    // that authenticates.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT token_hash FROM agent_capabilities WHERE session_id = ?")
            .bind(&session_id)
            .fetch_optional(app.db.reader())
            .await
            .expect("query");
    let stored = stored.expect("a row exists");
    assert_ne!(stored, token, "the raw token must not be stored");
    assert_eq!(stored.len(), 64, "a SHA-256 hex digest is 64 chars");
}

#[tokio::test]
async fn move_card_takes_a_legal_move_and_refuses_an_illegal_one() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let key = project(&app, &admin, "PROJ").await;
    let (card_id, card_key) = card(&app, &admin, &key, "a task").await;
    let admin_id = me(&app, &admin).await;
    let (_sid, token) = session_with_token(&app, &card_id, &admin_id).await;
    let session = capability::resolve(&app.db, &token).await.unwrap().unwrap();
    let ctx = ToolContext::build(&app.db, session).await.expect("ctx");

    // A legal move to a real status (the programming template has "In Progress").
    let ok = tools::call(
        &ctx,
        "atlas_move_card",
        &json!({ "to_status": "In Progress" }),
    )
    .await
    .expect("a legal move succeeds");
    let text = ok["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(&card_key), "{text}");
    assert!(text.contains("In Progress"), "{text}");

    // An illegal target: no such status. Refused, not silently ignored.
    let err = tools::call(&ctx, "atlas_move_card", &json!({ "to_status": "Nirvana" })).await;
    assert!(
        err.is_err(),
        "a move to a nonexistent status must be refused"
    );
}

#[tokio::test]
async fn a_viewer_starter_cannot_move_the_card_but_a_member_can() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let key = project(&app, &admin, "PROJ").await;
    let (card_id, _) = card(&app, &admin, &key, "a task").await;

    let (viewer_id, _) = user(&app, &admin, "vera", "member").await;
    add_member(&app, &admin, &key, &viewer_id, "viewer").await;
    let (member_id, _) = user(&app, &admin, "mo", "member").await;
    add_member(&app, &admin, &key, &member_id, "member").await;

    // A run started by the viewer cannot move the card — authority is re-checked at call time.
    let (_s1, viewer_token) = session_with_token(&app, &card_id, &viewer_id).await;
    let vs = capability::resolve(&app.db, &viewer_token)
        .await
        .unwrap()
        .unwrap();
    let vctx = ToolContext::build(&app.db, vs).await.expect("ctx");
    let denied = tools::call(
        &vctx,
        "atlas_move_card",
        &json!({ "to_status": "In Progress" }),
    )
    .await;
    assert!(denied.is_err(), "a viewer's run must not move the card");
    // But it can still read.
    assert!(
        tools::call(&vctx, "atlas_get_card", &json!({}))
            .await
            .is_ok()
    );

    // A run started by the member can move it.
    let (_s2, member_token) = session_with_token(&app, &card_id, &member_id).await;
    let ms = capability::resolve(&app.db, &member_token)
        .await
        .unwrap()
        .unwrap();
    let mctx = ToolContext::build(&app.db, ms).await.expect("ctx");
    assert!(
        tools::call(
            &mctx,
            "atlas_move_card",
            &json!({ "to_status": "In Progress" })
        )
        .await
        .is_ok(),
        "a member's run may move the card"
    );
}

#[tokio::test]
async fn a_comment_is_attributed_to_the_agent() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let key = project(&app, &admin, "PROJ").await;
    let (card_id, _) = card(&app, &admin, &key, "a task").await;
    let admin_id = me(&app, &admin).await;
    let (_sid, token) = session_with_token(&app, &card_id, &admin_id).await;
    let session = capability::resolve(&app.db, &token).await.unwrap().unwrap();
    let ctx = ToolContext::build(&app.db, session).await.expect("ctx");

    tools::call(
        &ctx,
        "atlas_comment_card",
        &json!({ "body": "I did the work." }),
    )
    .await
    .expect("comment posts");

    let body: String = sqlx::query_scalar("SELECT body FROM comments WHERE card_id = ? LIMIT 1")
        .bind(&card_id)
        .fetch_one(app.db.reader())
        .await
        .expect("query");
    assert!(
        body.contains("Claude Code session"),
        "agent marker missing: {body}"
    );
    assert!(body.contains("I did the work."), "body missing: {body}");
}

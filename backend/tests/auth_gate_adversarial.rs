//! Adversarial tests for the forced-password-reset gate and the session layer.
//!
//! These exist to *break* Phase 2, not to demonstrate it. Where `tests/auth.rs`
//! asserts the intended behaviour on a hand-picked list of routes, these derive
//! the list from the router itself, so a route added in Phase 5 that escapes the
//! gate fails here without anybody remembering to update a constant.

use atlas::api::{self, AppState};
use atlas::auth::seed::DEFAULT_ADMIN_USERNAME;
use atlas::auth::{session, user};
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use sha2::Digest as _;
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

/// The three routes the gate is documented to let past, as `(method, path)`.
const ALLOWLIST: &[(&str, &str)] = &[
    ("post", "/api/v1/auth/change-password"),
    ("post", "/api/v1/auth/logout"),
    ("get", "/api/v1/auth/me"),
];

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
        atlas::auth::seed::ensure_default_admin(&db)
            .await
            .expect("failed to seed");
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
        let status = response.status();
        let set_cookie: Vec<String> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(ToOwned::to_owned)
            .collect();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("failed to read body");
        Reply {
            status,
            set_cookie,
            raw_body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }
}

struct Reply {
    status: StatusCode,
    set_cookie: Vec<String>,
    raw_body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.raw_body)
            .unwrap_or_else(|e| panic!("body was not JSON ({e}): {}", self.raw_body))
    }

    fn session_cookie(&self) -> Option<String> {
        self.set_cookie
            .iter()
            .find(|c| c.starts_with(session::COOKIE_NAME))
            .and_then(|c| c.split(';').next())
            .and_then(|c| c.split_once('='))
            .map(|(_, v)| v.to_owned())
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
            .expect("failed to build request"),
        None => builder
            .body(Body::empty())
            .expect("failed to build request"),
    }
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::GET, uri, cookie, None)
}

/// How many audit events of `kind` have been recorded.
async fn count_events(app: &App, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM auth_events WHERE kind = ?")
        .bind(kind)
        .fetch_one(app.db.reader())
        .await
        .expect("failed to count auth events")
}

/// SHA-256 of `input`, lowercase hex.
///
/// Written out here rather than imported from `atlas`: this is the independent
/// recomputation that `the_stored_session_id_is_the_sha256_of_the_token` checks
/// the implementation against, so sharing the implementation's own helper would
/// make the assertion tautological.
fn sha256_hex(input: &str) -> String {
    use std::fmt::Write as _;
    sha2::Sha256::digest(input.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        },
    )
}

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::POST, uri, cookie, Some(body))
}

async fn login(app: &App, username: &str, password: &str) -> Reply {
    app.send(post(
        "/api/v1/auth/login",
        None,
        json!({ "username": username, "password": password }),
    ))
    .await
}

/// A session for the seeded admin, still behind the forced-reset gate.
async fn gated_admin(app: &App) -> String {
    let reply = login(app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.session_cookie().expect("login must set a cookie")
}

/// A session for the seeded admin that has cleared the gate.
async fn free_admin(app: &App) -> String {
    let cookie = gated_admin(app).await;
    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.session_cookie().expect("must rotate")
}

// ---------------------------------------------------------------------------
// Route enumeration: every route the router serves, from the router itself
// ---------------------------------------------------------------------------

/// Every `(method, path)` under `/api/v1`, read out of the live OpenAPI
/// document.
///
/// Derived rather than listed on purpose. A hand-maintained list of routes to
/// check *is* the per-handler check the gate exists to replace: it goes stale
/// the moment somebody adds a route and forgets, which is precisely the bug
/// being hunted.
async fn api_v1_routes(app: &App) -> Vec<(Method, String)> {
    let reply = app.send(get(api::OPENAPI_JSON_PATH, None)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let spec = reply.json();
    let paths = spec["paths"].as_object().expect("spec has no paths");

    let mut routes = Vec::new();
    for (path, item) in paths {
        if !path.starts_with(api::API_V1_PREFIX) {
            continue;
        }
        let operations = item.as_object().expect("path item is not an object");
        for method in operations.keys() {
            let parsed = match method.as_str() {
                "get" => Method::GET,
                "post" => Method::POST,
                "put" => Method::PUT,
                "patch" => Method::PATCH,
                "delete" => Method::DELETE,
                other => panic!("unhandled method {other} on {path}"),
            };
            routes.push((parsed, path.clone()));
        }
    }

    routes.sort_by_key(|(method, path)| (path.clone(), method.as_str().to_owned()));
    assert!(
        routes.len() > 20,
        "only {} routes found — the spec is not being read correctly",
        routes.len()
    );
    routes
}

/// Replaces `{param}` placeholders with a value that routes but matches nothing.
fn concretise(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push_str("nonexistent");
            }
            '}' => in_param = false,
            _ if in_param => {}
            _ => out.push(c),
        }
    }
    out
}

/// A body that is *syntactically* plausible for any handler, so that a rejection
/// cannot be blamed on deserialisation.
///
/// It does not matter that it is semantically wrong: the gate runs in middleware,
/// long before any handler deserialises anything. If a route answers 422 instead
/// of 403 here, the request reached the handler — which is the finding.
fn probe_body() -> Value {
    json!({})
}

#[tokio::test]
async fn every_route_under_api_v1_is_gated_except_the_documented_three() {
    // THE test for this phase. Enumerates the router's own surface and proves the
    // forced-reset gate covers all of it.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    let routes = api_v1_routes(&app).await;
    let mut escaped = Vec::new();
    let mut checked = 0;

    for (method, path) in &routes {
        let allowlisted = ALLOWLIST
            .iter()
            .any(|(m, p)| m.eq_ignore_ascii_case(method.as_str()) && p == path);
        if allowlisted {
            continue;
        }
        checked += 1;

        let uri = concretise(path);
        let body = if matches!(*method, Method::GET | Method::DELETE) {
            None
        } else {
            Some(probe_body())
        };
        let reply = app
            .send(request(method.clone(), &uri, Some(&cookie), body))
            .await;

        // 403 with the marker is the only acceptable answer. Anything else means
        // the request reached the handler.
        let gated = reply.status == StatusCode::FORBIDDEN
            && reply.json()["type"] == "urn:atlas:error:password-change-required";

        if !gated {
            escaped.push(format!(
                "{method} {uri} -> {} {}",
                reply.status,
                reply.json()["type"]
            ));
        }
    }

    assert!(
        escaped.is_empty(),
        "{} of {checked} routes escaped the forced-reset gate:\n  {}",
        escaped.len(),
        escaped.join("\n  ")
    );
}

#[tokio::test]
async fn the_gate_covers_routes_from_every_phase_not_just_auth_and_users() {
    // A narrower, louder version of the test above: the Phase 3/4 surface is
    // where a gate that was only wired for Phase 2 would show up.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    let probes = [
        (Method::GET, "/api/v1/projects", None),
        (Method::GET, "/api/v1/project-templates", None),
        (
            Method::POST,
            "/api/v1/projects",
            Some(json!({"key":"ATL","name":"Atlas","template":"blank"})),
        ),
        (Method::GET, "/api/v1/projects/ATL/cards", None),
        (
            Method::POST,
            "/api/v1/projects/ATL/cards",
            Some(json!({"summary":"x"})),
        ),
        (Method::GET, "/api/v1/cards/ATL-1", None),
        (
            Method::PATCH,
            "/api/v1/cards/ATL-1",
            Some(json!({"summary":"x"})),
        ),
        (Method::GET, "/api/v1/cards/ATL-1/history", None),
        (Method::GET, "/api/v1/cards/ATL-1/comments", None),
        (
            Method::POST,
            "/api/v1/cards/ATL-1/comments",
            Some(json!({"body":"x"})),
        ),
        (Method::GET, "/api/v1/projects/ATL/tags", None),
        (
            Method::POST,
            "/api/v1/projects/ATL/tags",
            Some(json!({"name":"x"})),
        ),
        (Method::GET, "/api/v1/projects/ATL/statuses", None),
        (Method::GET, "/api/v1/projects/ATL/hierarchy-levels", None),
    ];

    for (method, uri, body) in probes {
        let reply = app
            .send(request(method.clone(), uri, Some(&cookie), body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{method} {uri} escaped the gate: {}",
            reply.raw_body
        );
        assert_eq!(
            reply.json()["type"],
            "urn:atlas:error:password-change-required",
            "{method} {uri} was refused for the wrong reason"
        );
    }
}

#[tokio::test]
async fn a_gated_account_cannot_escape_by_dressing_the_path_up() {
    // The allowlist is an exact string match on OriginalUri. These are the
    // shapes that break a naive prefix/suffix/normalisation check.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    // Each of these must NOT reach `list_users` with a 200.
    let dodges = [
        "/api/v1/users",
        "/api/v1//users",
        "/api/v1/users/",
        "/api/v1/./users",
        "/api/v1/auth/me/../users",
        "/api/v1/auth/../users",
        // Percent-encoded 'u' in /users.
        "/api/v1/%75sers",
        // Percent-encoded 'm' in /auth/me, which must not open the allowlist for
        // some *other* target either.
        "/api/v1/auth/%6de",
    ];

    for uri in dodges {
        let reply = app.send(get(uri, Some(&cookie))).await;
        assert_ne!(
            reply.status,
            StatusCode::OK,
            "{uri} reached a protected handler while must_change_password was set: {}",
            reply.raw_body
        );
        // Either gated (403) or unrouted (404). Never served.
        assert!(
            matches!(reply.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
            "{uri} answered {} — neither gated nor absent: {}",
            reply.status,
            reply.raw_body
        );
    }
}

#[tokio::test]
async fn a_head_request_cannot_slip_past_the_gate_on_a_get_route() {
    // axum's `get()` also answers HEAD. If the gate matched on path alone, or if
    // HEAD were treated as exempt the way the origin check treats it, HEAD would
    // be a read oracle over every protected route.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    let reply = app
        .send(request(Method::HEAD, "/api/v1/users", Some(&cookie), None))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "HEAD /api/v1/users escaped the forced-reset gate"
    );
}

// ---------------------------------------------------------------------------
// The gate is not merely a 403 — it must refuse the *effect* too
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gated_admin_cannot_clear_its_own_gate_through_the_user_api() {
    // The privilege-escalation shape: PATCH /users/{id} can set
    // must_change_password = false, and the gated account is an *admin*. If the
    // gate ever failed open on that route, the seeded Admin/Admin account could
    // unlock itself without ever knowing a password other than `Admin`.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    let me = app.send(get("/api/v1/auth/me", Some(&cookie))).await;
    assert_eq!(me.status, StatusCode::OK);
    let id = me.json()["id"]
        .as_str()
        .expect("me carries an id")
        .to_owned();

    let reply = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(&cookie),
            Some(json!({ "mustChangePassword": false })),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a gated admin unlocked itself: {}",
        reply.raw_body
    );

    // And the flag really is untouched in the database, not merely in the reply.
    let still_gated = user::find_by_id(&app.db, &id)
        .await
        .unwrap()
        .expect("the admin exists");
    assert!(
        still_gated.must_change_password,
        "must_change_password was cleared without a password change"
    );

    // The gate is still up on the next request.
    assert_eq!(
        app.send(get("/api/v1/users", Some(&cookie))).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_gated_admin_cannot_mint_a_second_ungated_admin() {
    // The other escalation: create a fresh admin with mustChangePassword=false
    // and log in as that instead. Blocked by the gate, and provably: no user is
    // created.
    let app = App::new().await;
    let cookie = gated_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&cookie),
            json!({
                "username": "backdoor",
                "password": GOOD_PASSWORD,
                "role": "admin",
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.raw_body);

    assert!(
        user::find_by_username(&app.db, "backdoor")
            .await
            .unwrap()
            .is_none(),
        "a gated admin created a user"
    );
    assert_eq!(
        login(&app, "backdoor", GOOD_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_gate_follows_the_flag_and_can_be_reimposed_on_a_live_session() {
    // The gate reads `must_change_password` fresh on every request rather than
    // from anything cached at login. So an admin re-imposing it takes effect on
    // the victim's very next request, with no session rotation involved.
    let app = App::new().await;
    let admin = free_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({
                "username": "member",
                "password": GOOD_PASSWORD,
                "role": "member",
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let id = reply.json()["id"].as_str().unwrap().to_owned();

    let member = login(&app, "member", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&member))).await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.send(get("/api/v1/projects", Some(&member)))
            .await
            .status,
        StatusCode::OK,
        "an ungated member must be able to work"
    );

    // The admin re-imposes the gate on the live session.
    let reply = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(&admin),
            Some(json!({ "mustChangePassword": true })),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Same cookie, next request, now gated.
    let reply = app.send(get("/api/v1/projects", Some(&member))).await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "the gate did not apply to an already-established session"
    );
    assert_eq!(
        reply.json()["type"],
        "urn:atlas:error:password-change-required"
    );
}

// ---------------------------------------------------------------------------
// Session storage and comparison
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_session_token_appears_nowhere_in_the_database_file() {
    // Stronger than "the id column is a digest": greps every text column of every
    // table on disk. A token that leaked into auth_events.detail or a user_agent
    // would be a usable credential sitting in a table that gets rendered.
    let app = App::new().await;
    let cookie = free_admin(&app).await;
    app.send(get("/api/v1/auth/sessions", Some(&cookie))).await;

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(app.db.reader())
            .await
            .unwrap();

    for table in &tables {
        if table.starts_with("sqlite_") || table.starts_with("_sqlx") {
            continue;
        }
        // `quote()` renders every column of every row as SQL text, so this cannot
        // miss a column somebody adds later.
        //
        // `AssertSqlSafe` because sqlx 0.9 requires `&'static str` otherwise, and
        // a table name cannot be one here. The interpolated value comes from
        // `sqlite_master` in a database this test created a moment ago — it is
        // not reachable by any input — which is exactly the audit the wrapper
        // asks for.
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT group_concat(quote(t.*), ' ') FROM \"{table}\" AS t"
        )))
        .fetch_all(app.db.reader())
        .await
        .unwrap_or_default();

        for row in rows {
            assert!(
                !row.contains(&cookie),
                "the raw session token is stored in {table}: {row}"
            );
        }
    }
}

#[tokio::test]
async fn the_stored_session_id_is_the_sha256_of_the_token_and_not_the_token() {
    let app = App::new().await;
    let token = free_admin(&app).await;

    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions")
        .fetch_all(app.db.reader())
        .await
        .unwrap();
    assert_eq!(ids.len(), 1);

    // Recomputed here, independently of the implementation: if `digest` is ever
    // swapped for something else, this fails rather than silently agreeing.
    let expected = sha256_hex(&token);

    assert_eq!(ids[0], expected, "sessions.id is not SHA-256(token)");
    assert_ne!(ids[0], token);
    assert_eq!(ids[0].len(), 64);
}

#[tokio::test]
async fn a_revoked_session_is_dead_on_the_very_next_request_not_at_cookie_expiry() {
    // Server-side sessions exist for exactly this. Deleting the row must be
    // enough — no cache, no grace window, no reader-pool staleness.
    let app = App::new().await;
    let admin = free_admin(&app).await;

    let phone = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&phone))).await.status,
        StatusCode::OK
    );

    // Revoke it out from under the request path, by row, bypassing the API — so
    // this proves the *load* path re-checks rather than the handler being polite.
    let reply = app.send(get("/api/v1/auth/sessions", Some(&admin))).await;
    let sessions = reply.json();
    let other = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == false)
        .expect("two sessions");
    let id = other["id"].as_str().unwrap();

    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(app.db.writer())
        .await
        .unwrap();

    // The writer pool wrote it; the reader pool must see it immediately. If the
    // two pools ever disagreed, a revoked session would keep working.
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&phone))).await.status,
        StatusCode::UNAUTHORIZED,
        "a session revoked in the database still authenticated a request"
    );
}

#[tokio::test]
async fn a_session_row_forged_with_a_guessed_id_cannot_be_used_without_the_token() {
    // The property that makes storing the digest worth anything: an attacker who
    // can *read* the sessions table (a backup, a SQL injection elsewhere, a
    // stray log) still cannot turn a row into a cookie.
    let app = App::new().await;
    let admin = free_admin(&app).await;

    let id: String = sqlx::query_scalar("SELECT id FROM sessions LIMIT 1")
        .fetch_one(app.db.reader())
        .await
        .unwrap();

    // The digest itself, presented as the cookie, must not authenticate: the
    // server would have to hash it again, and SHA-256(digest) != digest.
    let reply = app.send(get("/api/v1/auth/me", Some(&id))).await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "the stored session id worked as a session token"
    );

    // And the real token still does, so the test above is not passing by
    // breaking everything.
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&admin))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_session_cookie_that_is_a_prefix_or_extension_of_a_real_token_is_refused() {
    // A digest lookup is exact, but this is what a `LIKE`, a truncating compare,
    // or a length-blind memcmp would let through.
    let app = App::new().await;
    let token = free_admin(&app).await;

    let mutations = [
        token[..token.len() - 1].to_owned(),
        format!("{token}a"),
        format!("a{token}"),
        token.to_uppercase(),
        token.to_lowercase(),
        format!("{token}%"),
        format!("{}%", &token[..8]),
        format!("{}' OR '1'='1", &token[..8]),
    ];

    for mutation in mutations {
        if mutation == token {
            continue;
        }
        let reply = app.send(get("/api/v1/auth/me", Some(&mutation))).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "a mutated token authenticated: {mutation}"
        );
    }
}

#[tokio::test]
async fn logging_out_one_session_does_not_touch_the_others() {
    // The inverse of the revoke test: over-revoking is a bug too, and a
    // `DELETE FROM sessions WHERE user_id = ?` in logout would pass every
    // "revoked session is dead" test while logging the user out everywhere.
    let app = App::new().await;
    let laptop = free_admin(&app).await;
    let phone = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    assert_eq!(
        app.send(post("/api/v1/auth/logout", Some(&laptop), json!({})))
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&laptop))).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&phone))).await.status,
        StatusCode::OK,
        "logging out one device logged out another"
    );
}

// ---------------------------------------------------------------------------
// Username oracle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_login_failure_mode_is_byte_identical() {
    // Unknown username, wrong password, and deactivated account. Three
    // distinguishable answers would be two oracles.
    let app = App::new().await;
    let admin = free_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({
                "username": "dormant",
                "password": GOOD_PASSWORD,
                "role": "member",
                "mustChangePassword": false,
            }),
        ))
        .await;
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    app.send(post(
        &format!("/api/v1/users/{id}/deactivate"),
        Some(&admin),
        json!({}),
    ))
    .await;

    let unknown = login(&app, "no-such-account-anywhere", "wrong").await;
    let wrong_password = login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    let deactivated = login(&app, "dormant", GOOD_PASSWORD).await;

    for (name, reply) in [
        ("unknown username", &unknown),
        ("wrong password", &wrong_password),
        ("deactivated account", &deactivated),
    ] {
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{name}");
        assert!(reply.session_cookie().is_none(), "{name} issued a session");
    }

    assert_eq!(
        unknown.raw_body, wrong_password.raw_body,
        "an unknown username is distinguishable from a wrong password"
    );
    assert_eq!(
        unknown.raw_body, deactivated.raw_body,
        "a deactivated account is distinguishable from an unknown username"
    );
}

#[tokio::test]
async fn a_deactivated_account_is_not_revealed_by_being_cheaper_to_refuse() {
    // The subtle half: `login` verifies the password *before* it checks
    // is_active. Short-circuiting on is_active would skip Argon2 and make a
    // disabled account answer in microseconds — a free account-status oracle.
    let app = App::new().await;
    let admin = free_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({
                "username": "dormant",
                "password": GOOD_PASSWORD,
                "role": "member",
                "mustChangePassword": false,
            }),
        ))
        .await;
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    app.send(post(
        &format!("/api/v1/users/{id}/deactivate"),
        Some(&admin),
        json!({}),
    ))
    .await;

    let start = std::time::Instant::now();
    let reply = login(&app, "dormant", GOOD_PASSWORD).await;
    let elapsed = start.elapsed();

    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(
        elapsed >= std::time::Duration::from_millis(10),
        "a deactivated account was refused in {elapsed:?} — no Argon2 ran, so the response time \
         says the account exists but is disabled"
    );
}

#[tokio::test]
async fn the_lockout_response_is_the_same_for_a_real_and_an_imaginary_username() {
    // "This username rate-limits" must not be a membership oracle — it would
    // undo the dummy hash entirely.
    let app = App::new().await;

    for _ in 0..10 {
        login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    }
    for _ in 0..10 {
        login(&app, "no-such-account-anywhere", "wrong").await;
    }

    let real = login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    let imaginary = login(&app, "no-such-account-anywhere", "wrong").await;

    assert_eq!(real.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        imaginary.status,
        StatusCode::TOO_MANY_REQUESTS,
        "an unknown username was not locked out, which reveals that it is unknown"
    );
    assert_eq!(
        real.raw_body, imaginary.raw_body,
        "a locked real username is distinguishable from a locked imaginary one"
    );
}

// ---------------------------------------------------------------------------
// Brute force on the change-password re-authentication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn change_password_rate_limits_the_current_password_guess_like_login_does() {
    // `ChangePasswordRequest::current_password` exists for one stated reason:
    // "a borrowed unlocked laptop must not be enough to take an account over
    // permanently". That threat model is precisely an attacker who holds the
    // session but not the password — so the re-check is the only thing standing
    // between them and a permanent takeover, and an unlimited number of guesses
    // at it is not a check.
    //
    // `login` allows ten guesses per fifteen minutes. This asserts the same
    // ceiling here, against the same counter: guesses at one secret must not be
    // cheaper because they arrive on a different route.
    let app = App::new().await;
    let cookie = free_admin(&app).await;

    let mut statuses = Vec::new();
    for _ in 0..15 {
        let reply = app
            .send(post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": "wrong", "newPassword": "an entirely new passphrase" }),
            ))
            .await;
        statuses.push(reply.status);
    }

    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "15 wrong current-password guesses were all answered {:?} — the re-authentication on \
         change-password is an unlimited online brute-force oracle, while login stops at {} \
         guesses",
        statuses.first(),
        atlas::auth::lockout::MAX_FAILURES,
    );

    // And the lock is the *shared* one: an attacker must not be able to burn the
    // guesses here and then start again from zero on login.
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD).await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "change-password guesses did not count against the login counter, so the two routes are \
         two independent budgets for guessing one secret"
    );
}

#[tokio::test]
async fn a_locked_out_change_password_is_refused_before_argon2_runs() {
    // The lock must be consulted *before* the hash, exactly as login does it —
    // otherwise the lockout is decorative and the route stays a CPU sink: every
    // refused request would still burn ~50 ms and 19 MiB on the blocking pool.
    //
    // Asserted on the audit log rather than on a stopwatch. "This took under N
    // ms" is a claim about the machine, not about the code: it flakes under
    // parallel load, and load only ever pushes it the wrong way. The event trail
    // is exact — reaching the verify at all emits `login_failed`, so a locked
    // request that produces only `login_locked_out` provably short-circuited
    // before the password was ever hashed.
    let app = App::new().await;
    let cookie = free_admin(&app).await;

    let wrong = || {
        post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": "wrong", "newPassword": "an entirely new passphrase" }),
        )
    };

    for _ in 0..atlas::auth::lockout::MAX_FAILURES {
        app.send(wrong()).await;
    }

    let failures_before = count_events(&app, "login_failed").await;

    let reply = app.send(wrong()).await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        reply.raw_body
    );
    assert_eq!(reply.json()["type"], "urn:atlas:error:locked-out");

    assert_eq!(
        count_events(&app, "login_failed").await,
        failures_before,
        "a locked-out change-password still recorded a failed verification, so it hashed the \
         password before consulting the lock"
    );
    assert!(
        count_events(&app, "login_locked_out").await > 0,
        "the refusal was not audited as a lockout"
    );
}

#[tokio::test]
async fn a_correct_current_password_still_works_and_forgives_the_counter() {
    // The other direction, so the fix above cannot be "always refuse": a user who
    // fumbles the old password a few times and then gets it right must not be
    // left one typo from a lockout.
    let app = App::new().await;
    let cookie = free_admin(&app).await;

    for _ in 0..3 {
        let reply = app
            .send(post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": "wrong", "newPassword": "an entirely new passphrase" }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": GOOD_PASSWORD, "newPassword": "an entirely new passphrase" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // The counter was forgiven, so login is not sitting near a lock.
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, "an entirely new passphrase").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
}

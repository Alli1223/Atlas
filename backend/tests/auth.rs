//! End-to-end auth tests, over the real router, the real middleware stack and a
//! real database.
//!
//! These drive HTTP through `tower::ServiceExt::oneshot` — no TCP, no ports, no
//! races, and every layer still runs. `tests/health.rs` established the pattern.
//!
//! # What these are for
//!
//! The unit tests next to each module prove the pieces. These prove the
//! *assembly*, which is where the security properties actually live: the gate is
//! a layer, the cookie is set by a jar, the extractor reads an extension the
//! middleware wrote. Every one of those seams is invisible to a unit test, and
//! the forced-reset gate in particular can only be verified through the router
//! that mounts it.

use atlas::api::{self, AppState};
use atlas::auth::seed::DEFAULT_ADMIN_USERNAME;
use atlas::auth::{password, seed, session};
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The seeded credentials, which every test starts from.
const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy, used wherever the password itself is
/// not what is under test.
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A migrated, seeded database and the router over it.
///
/// `TempDb` comes back too: dropping it deletes the database.
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

    /// A fresh router. Rebuilt per request because `oneshot` consumes it.
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
    /// Every `Set-Cookie` on the response.
    set_cookie: Vec<String>,
    /// The raw body — asserted on directly wherever a leak is what is at stake.
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
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
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

    /// The session cookie's value from `Set-Cookie`, if one was set.
    fn session_cookie(&self) -> Option<String> {
        self.set_cookie
            .iter()
            .find(|c| c.starts_with(session::COOKIE_NAME))
            .and_then(|c| c.split(';').next())
            .and_then(|c| c.split_once('='))
            .map(|(_, value)| value.to_owned())
    }
}

/// A request builder that carries a session cookie, if given one.
fn request(method: Method, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
    }

    // `expect` rather than `unwrap`: clippy's `allow-unwrap-in-tests` only
    // covers `#[test]` bodies, and these helpers are ordinary module-level
    // functions. `tests/health.rs` does the same.
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

/// Signs in and returns the session cookie.
async fn login(app: &App, username: &str, password: &str) -> Reply {
    app.send(post(
        "/api/v1/auth/login",
        None,
        json!({ "username": username, "password": password }),
    ))
    .await
}

/// Signs in as the seeded admin and returns its (still password-gated) cookie.
async fn login_admin(app: &App) -> String {
    let reply = login(app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply
        .session_cookie()
        .expect("login must set a session cookie")
}

/// Signs the admin in and gets it past the forced-reset gate.
async fn admin_past_the_gate(app: &App) -> String {
    let cookie = login_admin(app).await;
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

/// Creates a user through the admin API and returns its id.
async fn create_user(app: &App, admin: &str, username: &str, role: &str) -> String {
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
    reply.json()["id"]
        .as_str()
        .expect("a created user must carry an id")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_works_and_sets_an_httponly_session_cookie() {
    let app = App::new().await;
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["username"], "Admin");
    assert_eq!(reply.json()["role"], "admin");

    let cookie = reply
        .set_cookie
        .iter()
        .find(|c| c.starts_with(session::COOKIE_NAME))
        .expect("login must set the session cookie");

    // The attributes that make the cookie safe, asserted on the wire rather
    // than on the builder — this is what the browser actually receives.
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    // Dev is plain HTTP: a Secure cookie here would be silently dropped.
    assert!(!cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn login_is_case_insensitive_in_the_username_but_not_the_password() {
    let app = App::new().await;

    for spelling in ["Admin", "admin", "ADMIN", "aDmIn"] {
        let reply = login(&app, spelling, ADMIN_PASSWORD).await;
        assert_eq!(reply.status, StatusCode::OK, "{spelling} could not sign in");
    }

    // The password is not folded, and must not be.
    let reply = login(&app, "Admin", "admin").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_wrong_password_is_refused_and_sets_no_cookie() {
    let app = App::new().await;
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, "definitely not the password").await;

    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(
        reply.session_cookie().is_none(),
        "a failed login must not issue a session"
    );
}

#[tokio::test]
async fn an_unknown_username_fails_with_exactly_the_same_response_as_a_wrong_password() {
    // The username oracle, closed. If these two responses differ in *any*
    // observable way, an attacker can enumerate every account in the instance.
    let app = App::new().await;

    let wrong_password = login(&app, DEFAULT_ADMIN_USERNAME, "not the password").await;
    let unknown_user = login(&app, "nobody-by-that-name", "not the password").await;

    assert_eq!(wrong_password.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_user.status, StatusCode::UNAUTHORIZED);

    // Byte-for-byte identical bodies, not merely the same status.
    assert_eq!(wrong_password.raw_body, unknown_user.raw_body);
    assert_eq!(
        wrong_password.json()["type"],
        "urn:atlas:error:unauthorized"
    );
    assert_eq!(
        wrong_password.json()["detail"],
        "Invalid username or password."
    );

    // And neither says anything about which half was wrong.
    let rendered = unknown_user.raw_body.to_lowercase();
    for leak in ["no such", "unknown", "does not exist", "not found", "admin"] {
        assert!(!rendered.contains(leak), "{leak:?} leaked: {rendered}");
    }
}

#[tokio::test]
async fn an_unknown_username_costs_the_same_argon2_work_as_a_real_one() {
    // The *timing* half of the oracle, which the equal-bodies test above cannot
    // see. An early return for an unknown username would answer in microseconds
    // while a real one pays ~50ms of Argon2id.
    //
    // Asserted as a floor rather than a ratio: ratios between two ~50ms samples
    // are noise on a loaded CI box and would flake. Argon2id at m=19MiB, t=2 is
    // never anywhere near 10ms, so this floor cannot pass by accident — and it
    // fails immediately if anyone "optimises" the unknown-user path.
    let app = App::new().await;

    let start = std::time::Instant::now();
    let reply = login(&app, "nobody-by-that-name", "not the password").await;
    let elapsed = start.elapsed();

    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(
        elapsed >= std::time::Duration::from_millis(10),
        "an unknown username answered in {elapsed:?} — no password was hashed, so the response \
         time reveals that the account does not exist"
    );
}

#[tokio::test]
async fn a_deactivated_user_cannot_sign_in_even_with_the_right_password() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = create_user(&app, &admin, "temp", "member").await;

    // It works first.
    assert_eq!(
        login(&app, "temp", GOOD_PASSWORD).await.status,
        StatusCode::OK
    );

    let reply = app
        .send(post(
            &format!("/api/v1/users/{id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // ...and then it does not, with the same document a wrong password gets —
    // "this account is disabled" would be a second oracle.
    let refused = login(&app, "temp", GOOD_PASSWORD).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.json()["detail"], "Invalid username or password.");
    assert!(refused.session_cookie().is_none());
}

// ---------------------------------------------------------------------------
// The forced-reset gate — the one that matters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_admin_session_is_403_on_every_protected_route_with_the_machine_readable_marker() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    // A real, valid, authenticated session — and it can do nothing.
    let reply = app.send(get("/api/v1/users", Some(&cookie))).await;

    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "the seeded admin reached a protected route without changing its password: {}",
        reply.raw_body
    );

    // The marker the frontend keys on. It must NOT have to match on prose.
    assert_eq!(
        reply.json()["type"],
        "urn:atlas:error:password-change-required",
        "the 403 carries no machine-readable marker: {}",
        reply.raw_body
    );
    assert_eq!(reply.json()["status"], 403);
    // Still an ordinary RFC 7807 document, with the instance filled in.
    assert_eq!(reply.json()["instance"], "/api/v1/users");
    assert!(reply.json()["title"].is_string());
}

#[tokio::test]
async fn the_gate_blocks_every_verb_and_route_except_the_documented_three() {
    // The point of implementing this as middleware: a route nobody thought about
    // is gated by default. Anything reachable here that should not be is a hole.
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    let blocked = [
        get("/api/v1/users", Some(&cookie)),
        get("/api/v1/auth/sessions", Some(&cookie)),
        post(
            "/api/v1/users",
            Some(&cookie),
            json!({
                "username": "sneaky", "password": GOOD_PASSWORD, "role": "admin"
            }),
        ),
        request(
            Method::DELETE,
            "/api/v1/auth/sessions/whatever",
            Some(&cookie),
            None,
        ),
        request(
            Method::PATCH,
            "/api/v1/users/whatever",
            Some(&cookie),
            Some(json!({"role":"admin"})),
        ),
    ];

    for request in blocked {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{method} {uri} escaped the forced-reset gate"
        );
        assert_eq!(
            reply.json()["type"],
            "urn:atlas:error:password-change-required",
            "{method} {uri} was refused for the wrong reason"
        );
    }
}

#[tokio::test]
async fn the_three_allowlisted_routes_stay_open_or_the_account_is_bricked() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    // `me` — the SPA needs it to discover *why* it is blocked.
    let reply = app.send(get("/api/v1/auth/me", Some(&cookie))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["mustChangePassword"],
        true,
        "me must tell the client to redirect"
    );

    // `logout` — a user who cannot change their password must still be able to
    // leave.
    let reply = app
        .send(post("/api/v1/auth/logout", Some(&cookie), json!({})))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    // `change-password` — the way out. Proven by the test below.
}

#[tokio::test]
async fn changing_the_password_unblocks_the_gate() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    // Blocked...
    assert_eq!(
        app.send(get("/api/v1/users", Some(&cookie))).await.status,
        StatusCode::FORBIDDEN
    );

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["mustChangePassword"],
        false,
        "the response must tell the client the gate is lifted"
    );

    // ...and unblocked, on the new cookie.
    let new_cookie = reply
        .session_cookie()
        .expect("a new session must be issued");
    let reply = app.send(get("/api/v1/users", Some(&new_cookie))).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "the gate did not lift after the password changed: {}",
        reply.raw_body
    );
    assert!(reply.json().as_array().unwrap().len() == 1);
}

#[tokio::test]
async fn the_gate_does_not_block_an_ordinary_user() {
    // The other direction: the gate must be *off* for everyone else, or every
    // route in Atlas is 403 forever.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "member", "member").await;

    let cookie = login(&app, "member", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    let reply = app.send(get("/api/v1/auth/me", Some(&cookie))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["mustChangePassword"], false);
}

#[tokio::test]
async fn a_user_created_with_a_forced_reset_is_gated_too() {
    // The gate is a property of the flag, not of the seeder.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({ "username": "newbie", "password": GOOD_PASSWORD, "role": "member" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["mustChangePassword"],
        true,
        "create must default to a forced reset"
    );

    let cookie = login(&app, "newbie", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let reply = app.send(get("/api/v1/auth/sessions", Some(&cookie))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.json()["type"],
        "urn:atlas:error:password-change-required"
    );
}

// ---------------------------------------------------------------------------
// Session fixation and rotation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_session_id_changes_when_the_password_changes() {
    // Session fixation: if the id survived a password change, the credential
    // would change while the thing that grants access did not.
    let app = App::new().await;
    let before = login_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&before),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let after = reply
        .session_cookie()
        .expect("a new session must be issued");
    assert_ne!(before, after, "the session id did not rotate");

    // And the old one is dead — rotation that leaves the old id valid is not
    // rotation.
    let reply = app.send(get("/api/v1/auth/me", Some(&before))).await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "the pre-change session still works"
    );

    let reply = app.send(get("/api/v1/auth/me", Some(&after))).await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn changing_the_password_logs_every_other_device_out() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // Two more devices.
    let laptop = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    let phone = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&laptop))).await.status,
        StatusCode::OK
    );

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&admin),
            json!({ "currentPassword": GOOD_PASSWORD, "newPassword": "another fine passphrase" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    for (name, cookie) in [("laptop", laptop), ("phone", phone)] {
        assert_eq!(
            app.send(get("/api/v1/auth/me", Some(&cookie))).await.status,
            StatusCode::UNAUTHORIZED,
            "the {name} session survived a password change"
        );
    }
}

#[tokio::test]
async fn a_wrong_current_password_does_not_change_anything() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": "not the password", "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // The old password still works, the new one does not, and the session did
    // not rotate.
    assert!(reply.session_cookie().is_none());
    assert_eq!(
        login(&app, "Admin", ADMIN_PASSWORD).await.status,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, "Admin", GOOD_PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
}

// ---------------------------------------------------------------------------
// The `Admin` password rule and the policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_cannot_be_kept_as_the_new_password() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    for candidate in ["Admin", "admin", "ADMIN", "aDmIn"] {
        let reply = app
            .send(post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": candidate }),
            ))
            .await;

        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{candidate:?} was accepted as a replacement for the default password"
        );
        // And the message says *why*, rather than complaining about length.
        assert!(
            reply.json()["detail"]
                .as_str()
                .unwrap()
                .contains("cannot be reused"),
            "{}",
            reply.raw_body
        );
    }

    // The gate is still up, because nothing changed.
    assert_eq!(
        app.send(get("/api/v1/users", Some(&cookie))).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_password_policy_is_enforced_on_change() {
    let app = App::new().await;
    let cookie = login_admin(&app).await;

    let rejected = [
        ("elevenchars", "too short"),
        ("password1234", "a famously common password"),
        ("Admin", "the default"),
    ];

    for (candidate, why) in rejected {
        let reply = app
            .send(post(
                "/api/v1/auth/change-password",
                Some(&cookie),
                json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": candidate }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{candidate:?} was accepted despite being {why}"
        );
    }

    // ...and the username itself.
    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&cookie),
            json!({ "currentPassword": ADMIN_PASSWORD, "newPassword": "AdminAdminAdmin" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "a long non-default password is fine: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn the_new_password_must_differ_from_the_current_one() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&admin),
            json!({ "currentPassword": GOOD_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.json()["detail"]
            .as_str()
            .unwrap()
            .contains("different"),
        "{}",
        reply.raw_body
    );
}

// ---------------------------------------------------------------------------
// The seed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_admin_seed_is_idempotent_and_does_not_resurrect_a_deleted_admin() {
    let app = App::new().await;

    // Already seeded by the harness. Re-running must do nothing.
    assert!(!seed::ensure_default_admin(&app.db).await.unwrap());
    assert!(!seed::ensure_default_admin(&app.db).await.unwrap());

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(app.db.reader())
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Make another admin, delete the seeded one, and reboot.
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "alastair", "admin").await;
    sqlx::query("DELETE FROM users WHERE username = 'Admin'")
        .execute(app.db.writer())
        .await
        .unwrap();

    assert!(
        !seed::ensure_default_admin(&app.db).await.unwrap(),
        "Admin/Admin was recreated on an instance that already has users"
    );
    assert_eq!(
        login(&app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

// ---------------------------------------------------------------------------
// No hash ever reaches a client
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_response_body_ever_contains_a_password_hash() {
    // Asserted on the raw JSON of every response the API can produce for a
    // user, not on the type: the type is the mechanism, this is the property.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = create_user(&app, &admin, "member", "member").await;

    let member = login(&app, "member", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    let bodies = vec![
        login(&app, "member", GOOD_PASSWORD).await.raw_body,
        app.send(get("/api/v1/auth/me", Some(&member)))
            .await
            .raw_body,
        app.send(get("/api/v1/users", Some(&admin))).await.raw_body,
        app.send(get(&format!("/api/v1/users/{id}"), Some(&admin)))
            .await
            .raw_body,
        app.send(get("/api/v1/auth/sessions", Some(&member)))
            .await
            .raw_body,
        app.send(post(
            "/api/v1/users",
            Some(&admin),
            json!({ "username": "another", "password": GOOD_PASSWORD, "role": "viewer" }),
        ))
        .await
        .raw_body,
        app.send(request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(&admin),
            Some(json!({ "displayName": "Renamed" })),
        ))
        .await
        .raw_body,
        app.send(post(
            &format!("/api/v1/users/{id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await
        .raw_body,
        // And an error path, which is where a Debug impl usually leaks one.
        login(&app, "member", "wrong").await.raw_body,
    ];

    for body in bodies {
        let lowered = body.to_lowercase();
        for needle in [
            "argon2",
            "password_hash",
            "passwordhash",
            "$argon",
            "m=19456",
        ] {
            assert!(
                !lowered.contains(needle),
                "{needle:?} appeared in a response body: {body}"
            );
        }
        // The plaintext must not come back either.
        assert!(
            !body.contains(GOOD_PASSWORD),
            "a password was echoed back: {body}"
        );
    }
}

#[tokio::test]
async fn no_response_schema_declares_a_password_field() {
    // A hash can also leak through the generated TypeScript client's types,
    // which come from here.
    //
    // Asserted on each schema's *properties* rather than on the document text:
    // the prose descriptions are doc comments, and several of them discuss
    // `password_hash` precisely because keeping it out of the DTO is the point.
    // Substring-matching the whole document would fail on its own commentary.
    let app = App::new().await;
    let reply = app.send(get(api::OPENAPI_JSON_PATH, None)).await;
    assert_eq!(reply.status, StatusCode::OK);

    let json = reply.json();

    for schema in ["UserDto", "SessionDto"] {
        let properties = json["components"]["schemas"][schema]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{schema} is not in the spec"));

        for name in properties.keys() {
            assert!(
                !name.to_lowercase().contains("password") || name == "mustChangePassword",
                "{schema} declares a password-bearing field: {name}"
            );
        }
    }

    // UserDto is the response type, so pin its shape exactly — a field added
    // here in future is a deliberate act, not an accident.
    let user_fields: Vec<&String> = json["components"]["schemas"]["UserDto"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert!(
        !user_fields.iter().any(|f| f.contains("Hash")),
        "{user_fields:?}"
    );

    // ...while the routes really are documented.
    for path in [
        "/api/v1/auth/login",
        "/api/v1/auth/logout",
        "/api/v1/auth/me",
        "/api/v1/auth/change-password",
        "/api/v1/auth/sessions",
        "/api/v1/auth/sessions/{id}",
        "/api/v1/users",
        "/api/v1/users/{id}",
        "/api/v1/users/{id}/deactivate",
    ] {
        assert!(json["paths"][path].is_object(), "{path} is not in the spec");
    }
    // axum 0.8 syntax: a `:id` anywhere would be a runtime panic.
    assert!(!reply.raw_body.contains("/:"), "0.7-style route parameter");
}

// ---------------------------------------------------------------------------
// Lockout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ten_failures_lock_the_account_out_and_the_lock_expires() {
    let app = App::new().await;

    // Nine failures: still merely wrong.
    for i in 1..10 {
        let reply = login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "locked out after only {i} failures"
        );
    }

    // The tenth locks it.
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // ...and now even the *correct* password is refused, with a 429 that says
    // how long. This is the assertion that proves the lock is real: a lockout
    // that still admits the right password is decorative.
    let reply = login(&app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD).await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the correct password was accepted while locked out: {}",
        reply.raw_body
    );
    assert_eq!(reply.json()["type"], "urn:atlas:error:locked-out");
    assert!(
        reply.json()["detail"]
            .as_str()
            .unwrap()
            .contains("15 minutes"),
        "{}",
        reply.raw_body
    );
    assert!(reply.session_cookie().is_none());

    // Wind the clock past the lock by rewriting the row — the alternative is a
    // fifteen-minute test.
    sqlx::query("UPDATE login_attempts SET locked_until = '2020-01-01T00:00:00.000000+00:00'")
        .execute(app.db.writer())
        .await
        .unwrap();

    let reply = login(&app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "the lock did not expire: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_lockout_is_per_username_and_does_not_stop_anyone_else() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "member", "member").await;

    for _ in 0..10 {
        login(&app, "member", "wrong").await;
    }
    assert_eq!(
        login(&app, "member", GOOD_PASSWORD).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    // The admin is untouched: one user's lockout must not be everyone's.
    assert_eq!(
        login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_unknown_username_is_locked_out_exactly_like_a_real_one() {
    // Otherwise "this username locks out" is a membership oracle, and the dummy
    // hash above was pointless.
    let app = App::new().await;

    for _ in 0..10 {
        login(&app, "nobody-by-that-name", "wrong").await;
    }

    let reply = login(&app, "nobody-by-that-name", "wrong").await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a username that does not exist was not rate-limited: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_successful_login_forgives_the_counter() {
    let app = App::new().await;

    for _ in 0..9 {
        login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    }
    assert_eq!(
        login(&app, DEFAULT_ADMIN_USERNAME, ADMIN_PASSWORD)
            .await
            .status,
        StatusCode::OK
    );

    // Nine more must not lock: the counter reset. Otherwise a user who mistypes
    // nine times and then succeeds is one typo from a lockout forever.
    for i in 0..9 {
        let reply = login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "locked out {i} failures after a successful login"
        );
    }
}

// ---------------------------------------------------------------------------
// Role guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewer_cannot_reach_an_admin_route() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let viewer_id = create_user(&app, &admin, "viewer", "viewer").await;

    let viewer = login(&app, "viewer", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    // Authenticated, so `me` works...
    let reply = app.send(get("/api/v1/auth/me", Some(&viewer))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["role"], "viewer");

    // ...and every admin route is 403. Not 401: they *are* authenticated, and
    // signing in again would not help.
    let admin_routes = [
        get("/api/v1/users", Some(&viewer)),
        get(&format!("/api/v1/users/{viewer_id}"), Some(&viewer)),
        post(
            "/api/v1/users",
            Some(&viewer),
            json!({
                "username": "escalation", "password": GOOD_PASSWORD, "role": "admin"
            }),
        ),
        request(
            Method::PATCH,
            &format!("/api/v1/users/{viewer_id}"),
            Some(&viewer),
            Some(json!({ "role": "admin" })),
        ),
        post(
            &format!("/api/v1/users/{viewer_id}/deactivate"),
            Some(&viewer),
            json!({}),
        ),
    ];

    for request in admin_routes {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a viewer reached {method} {uri}"
        );
        assert_eq!(reply.json()["type"], "urn:atlas:error:forbidden");
    }

    // And the privilege escalation really did not happen.
    let reply = app
        .send(get(&format!("/api/v1/users/{viewer_id}"), Some(&admin)))
        .await;
    assert_eq!(reply.json()["role"], "viewer");
}

#[tokio::test]
async fn a_member_cannot_reach_an_admin_route_either() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "member", "member").await;

    let member = login(&app, "member", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    assert_eq!(
        app.send(get("/api/v1/users", Some(&member))).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_anonymous_request_to_a_protected_route_is_401_not_403() {
    // "Log in" and "you may not" are different instructions to a client.
    let app = App::new().await;

    for request in [get("/api/v1/auth/me", None), get("/api/v1/users", None)] {
        let uri = request.uri().to_string();
        let reply = app.send(request).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(reply.json()["type"], "urn:atlas:error:unauthorized");
    }
}

#[tokio::test]
async fn a_garbage_cookie_is_simply_unauthenticated() {
    let app = App::new().await;

    for cookie in ["", "not-a-token", "../../etc/passwd", &"a".repeat(5000)] {
        let reply = app.send(get("/api/v1/auth/me", Some(cookie))).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "a junk cookie was not rejected cleanly"
        );
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_revoked_session_stops_working_immediately() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // A second device to revoke from the first.
    let phone = login(&app, DEFAULT_ADMIN_USERNAME, GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&phone))).await.status,
        StatusCode::OK
    );

    let reply = app.send(get("/api/v1/auth/sessions", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let sessions = reply.json();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 2);

    // Exactly one is flagged current, and it is not the one we are about to kill.
    let current: Vec<&Value> = sessions.iter().filter(|s| s["current"] == true).collect();
    assert_eq!(current.len(), 1, "exactly one session is the current one");

    let other = sessions
        .iter()
        .find(|s| s["current"] == false)
        .expect("the phone session must be listed");
    let other_id = other["id"].as_str().unwrap();

    let reply = app
        .send(request(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{other_id}"),
            Some(&admin),
            None,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    // Immediately, on the very next request — not when the cookie expires.
    let reply = app.send(get("/api/v1/auth/me", Some(&phone))).await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "a revoked session still works"
    );
    // The revoker is unaffected.
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&admin))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_listed_session_id_is_not_a_usable_token() {
    // The id is the token's SHA-256. Handing it back to the API as a cookie must
    // not authenticate anything — otherwise `GET /auth/sessions` hands out
    // working credentials for every device the user owns.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app.send(get("/api/v1/auth/sessions", Some(&admin))).await;
    let id = reply.json()[0]["id"].as_str().unwrap().to_owned();
    assert_ne!(id, admin);

    let reply = app.send(get("/api/v1/auth/me", Some(&id))).await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "a session id from the API worked as a session token"
    );
}

#[tokio::test]
async fn a_user_cannot_revoke_someone_elses_session() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "member", "member").await;

    let member = login(&app, "member", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();

    // The member learns the admin's session id... somehow. It still must not
    // work, and must look exactly like an id that does not exist.
    let admin_session = app.send(get("/api/v1/auth/sessions", Some(&admin))).await;
    let admin_id = admin_session.json()[0]["id"].as_str().unwrap().to_owned();

    let reply = app
        .send(request(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{admin_id}"),
            Some(&member),
            None,
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "a member revoked an admin's session"
    );

    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&admin))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn logout_revokes_the_session_and_clears_the_cookie() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(post("/api/v1/auth/logout", Some(&admin), json!({})))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let removal = reply
        .set_cookie
        .iter()
        .find(|c| c.starts_with(session::COOKIE_NAME))
        .expect("logout must clear the cookie");
    assert!(removal.contains("Max-Age=0"), "{removal}");
    // The removal cookie's Path must match the real one, or the browser adds a
    // second cookie and the user stays logged in.
    assert!(removal.contains("Path=/"), "{removal}");

    // The server-side session is gone too — clearing the cookie alone would
    // leave a working credential in whatever already copied it.
    let reply = app.send(get("/api/v1/auth/me", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logging_out_twice_is_not_an_error() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    assert_eq!(
        app.send(post("/api/v1/auth/logout", Some(&admin), json!({})))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    // The client's goal — "this browser holds no session" — is already met.
    assert_eq!(
        app.send(post("/api/v1/auth/logout", Some(&admin), json!({})))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.send(post("/api/v1/auth/logout", None, json!({})))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn deactivating_a_user_kills_their_live_sessions_at_once() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = create_user(&app, &admin, "temp", "member").await;

    let temp = login(&app, "temp", GOOD_PASSWORD)
        .await
        .session_cookie()
        .unwrap();
    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&temp))).await.status,
        StatusCode::OK
    );

    app.send(post(
        &format!("/api/v1/users/{id}/deactivate"),
        Some(&admin),
        json!({}),
    ))
    .await;

    assert_eq!(
        app.send(get("/api/v1/auth/me", Some(&temp))).await.status,
        StatusCode::UNAUTHORIZED,
        "a deactivated user's session survived"
    );
}

// ---------------------------------------------------------------------------
// CSRF / origin check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_state_changing_request_from_a_foreign_origin_is_refused() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/users")
        .header(header::ORIGIN, "https://evil.test")
        .header(header::COOKIE, format!("{}={admin}", session::COOKIE_NAME))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "csrf", "password": GOOD_PASSWORD, "role": "admin" }).to_string(),
        ))
        .unwrap();

    let reply = app.send(request).await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a cross-origin write was accepted: {}",
        reply.raw_body
    );

    // And it really did not happen.
    let reply = app.send(get("/api/v1/users", Some(&admin))).await;
    assert_eq!(reply.json().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_configured_origin_may_make_state_changing_requests() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // The dev default in `Config`.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/users")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::COOKIE, format!("{}={admin}", session::COOKIE_NAME))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "legit", "password": GOOD_PASSWORD, "role": "member" }).to_string(),
        ))
        .unwrap();

    let reply = app.send(request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

#[tokio::test]
async fn a_same_origin_request_is_allowed_without_any_cors_configuration() {
    // The single-binary deploy. If this ever breaks, production rejects every
    // write while dev works perfectly.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/users")
        .header(header::HOST, "atlas.example.com")
        .header(header::ORIGIN, "https://atlas.example.com")
        .header(header::COOKIE, format!("{}={admin}", session::COOKIE_NAME))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "same-origin", "password": GOOD_PASSWORD, "role": "member" })
                .to_string(),
        ))
        .unwrap();

    let reply = app.send(request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

#[tokio::test]
async fn a_cross_origin_login_attempt_is_refused_before_any_password_is_checked() {
    // Login is state-changing: it sets a cookie. An attacker's page forcing a
    // victim's browser to log in *as the attacker* is a real attack (it makes
    // the victim's subsequent work land in the attacker's account).
    let app = App::new().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::ORIGIN, "https://evil.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "Admin", "password": ADMIN_PASSWORD }).to_string(),
        ))
        .unwrap();

    let reply = app.send(request).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(reply.session_cookie().is_none());
}

#[tokio::test]
async fn a_safe_method_is_not_origin_checked() {
    // GETs do not change state, and a cross-origin read is CORS's problem, not
    // CSRF's. Checking them would break every link into Atlas.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/auth/me")
        .header(header::ORIGIN, "https://evil.test")
        .header(header::COOKIE, format!("{}={admin}", session::COOKIE_NAME))
        .body(Body::empty())
        .unwrap();

    assert_eq!(app.send(request).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_non_browser_client_with_no_origin_header_still_works() {
    // curl, scripts, integrations. Every browser has sent Origin on
    // cross-origin state-changing requests for years, so "no Origin" is not
    // CSRF — and rejecting it would break every API client to defend against an
    // attack that requires a browser.
    let app = App::new().await;
    let cookie = login_admin(&app).await;
    assert!(!cookie.is_empty(), "login with no Origin header must work");

    let reply = app
        .send(post("/api/v1/auth/logout", Some(&cookie), json!({})))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// User administration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_duplicate_username_is_a_409_in_any_case() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "alastair", "member").await;

    for spelling in ["alastair", "Alastair", "ALASTAIR"] {
        let reply = app
            .send(post(
                "/api/v1/users",
                Some(&admin),
                json!({ "username": spelling, "password": GOOD_PASSWORD, "role": "member" }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::CONFLICT,
            "{spelling:?} was allowed to collide with an existing username"
        );
        assert_eq!(reply.json()["type"], "urn:atlas:error:conflict");
    }
}

#[tokio::test]
async fn the_last_active_admin_cannot_be_demoted_or_deactivated() {
    // Otherwise the instance is unadministrable and unrecoverable through the
    // API: there is nobody left who can promote anyone.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app.send(get("/api/v1/auth/me", Some(&admin))).await;
    let admin_id = reply.json()["id"].as_str().unwrap().to_owned();

    // Self-deactivation, via the dedicated route...
    let reply = app
        .send(post(
            &format!("/api/v1/users/{admin_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // ...and via PATCH, which must not be a back door around it.
    let reply = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/users/{admin_id}"),
            Some(&admin),
            Some(json!({ "isActive": false })),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // Demoting the only admin is refused too.
    let reply = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/users/{admin_id}"),
            Some(&admin),
            Some(json!({ "role": "viewer" })),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // Still an admin, still working.
    assert_eq!(
        app.send(get("/api/v1/users", Some(&admin))).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_admin_can_be_demoted_once_there_is_another_one() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let second = create_user(&app, &admin, "second-admin", "admin").await;

    let reply = app
        .send(request(
            Method::PATCH,
            &format!("/api/v1/users/{second}"),
            Some(&admin),
            Some(json!({ "role": "member" })),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["role"], "member");
}

#[tokio::test]
async fn a_patch_can_clear_an_email_and_absence_leaves_it_alone() {
    // The Option<Option<_>> contract, end to end. Without the double-option
    // deserializer, `{"email": null}` returns 200 and changes nothing.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = create_user(&app, &admin, "member", "member").await;

    let set = |body: Value| {
        request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(&admin),
            Some(body),
        )
    };

    let reply = app
        .send(set(json!({ "email": "member@example.com" })))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["email"], "member@example.com");

    // Absent: untouched.
    let reply = app.send(set(json!({ "displayName": "Renamed" }))).await;
    assert_eq!(
        reply.json()["email"],
        "member@example.com",
        "absent cleared the email"
    );
    assert_eq!(reply.json()["displayName"], "Renamed");

    // Null: cleared.
    let reply = app.send(set(json!({ "email": null }))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(
        reply.json()["email"].is_null(),
        "an explicit null did not clear the email: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_new_user_cannot_be_given_a_password_that_breaks_the_policy() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    for bad in ["short", "Admin", "password1234"] {
        let reply = app
            .send(post(
                "/api/v1/users",
                Some(&admin),
                json!({ "username": "victim", "password": bad, "role": "member" }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad:?} was accepted as a new user's password"
        );
    }
}

#[tokio::test]
async fn the_seeded_admin_is_the_only_user_a_fresh_instance_has() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app.send(get("/api/v1/users", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK);
    let users = reply.json();
    let users = users.as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["username"], "Admin");
    assert_eq!(users[0]["role"], "admin");
    assert_eq!(users[0]["isActive"], true);
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_events_are_recorded_and_never_contain_a_secret() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    login(&app, DEFAULT_ADMIN_USERNAME, "wrong").await;
    login(&app, "nobody-by-that-name", "wrong").await;
    app.send(post("/api/v1/auth/logout", Some(&admin), json!({})))
        .await;

    let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM auth_events")
        .fetch_all(app.db.reader())
        .await
        .unwrap();

    for expected in [
        "default_admin_seeded",
        "login_succeeded",
        "login_failed",
        "password_changed",
        "logged_out",
    ] {
        assert!(
            kinds.contains(&expected.to_owned()),
            "{expected} was not audited: {kinds:?}"
        );
    }

    // The audit log is not treated as secret, so hashing everything else is
    // pointless if the plaintext is one join away.
    let details: Vec<Option<String>> = sqlx::query_scalar("SELECT detail FROM auth_events")
        .fetch_all(app.db.reader())
        .await
        .unwrap();
    let rendered = format!("{details:?}").to_lowercase();
    for secret in [GOOD_PASSWORD, "argon2", "$argon", &admin.to_lowercase()] {
        assert!(
            !rendered.contains(&secret.to_lowercase()),
            "{secret:?} was written to the audit log: {rendered}"
        );
    }
}

#[tokio::test]
async fn a_failed_login_for_an_unknown_username_is_audited_with_no_user() {
    let app = App::new().await;
    login(&app, "nobody-by-that-name", "wrong").await;

    let (kind, user_id): (String, Option<String>) =
        sqlx::query_as("SELECT kind, user_id FROM auth_events WHERE kind = 'login_failed'")
            .fetch_one(app.db.reader())
            .await
            .unwrap();

    assert_eq!(kind, "login_failed");
    assert_eq!(user_id, None, "there is no user to attribute it to");
}

// ---------------------------------------------------------------------------
// Password hashing, as configured in the running app
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_stored_hash_is_argon2id_with_the_owasp_parameters() {
    // Asserted against what a real sign-up actually wrote, not against the
    // constants: `Params::new(19, ..)` would satisfy a constant test and store a
    // hash a thousand times cheaper to crack.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_user(&app, &admin, "member", "member").await;

    let hashes: Vec<String> = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_all(app.db.reader())
        .await
        .unwrap();
    assert_eq!(hashes.len(), 2);

    for hash in &hashes {
        assert!(hash.starts_with("$argon2id$v=19$"), "{hash}");
        assert!(hash.contains("m=19456,t=2,p=1"), "{hash}");
    }
    // Distinct salts: two users with the same password must not share a hash.
    assert_ne!(hashes[0], hashes[1]);

    // And the plaintext is nowhere in the table.
    assert!(!hashes.iter().any(|h| h.contains(GOOD_PASSWORD)));
    assert!(
        password::verify(GOOD_PASSWORD.to_owned(), hashes[1].clone())
            .await
            .unwrap()
    );
}

//! Adversarial tests: an attacker's view of Phases 2–4.
//!
//! `tests/auth.rs` and `tests/domain.rs` prove the features work. These try to
//! make them work *for the wrong person*, or make them say something they should
//! never say. The harness is lifted from `tests/domain.rs`, as its own handoff
//! from `tests/auth.rs` intended.
//!
//! Three questions, each asked of the real router over a real database:
//!
//! 1. Does any secret — a password, a hash, a session token — reach a response?
//! 2. Can a client reach a row it does not own, or one that does not exist,
//!    through an id it simply made up?
//! 3. Does hostile text reach SQL, or come back out as itself?

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

/// The seeded credentials every test starts from.
const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy.
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

/// The password given to accounts created during a test.
///
/// Distinct from [`GOOD_PASSWORD`] and deliberately unmistakable: the leak sweep
/// greps every response body for this string, so it must not be a substring of
/// anything a handler could legitimately echo.
const OTHER_PASSWORD: &str = "correct-horse-battery-staple-92";

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

/// Signs the admin in and gets it past the forced-reset gate.
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

/// Creates an account with `role` and signs it in, past its own reset gate.
///
/// Returns `(user_id, cookie)`.
async fn user_past_the_gate(
    app: &App,
    admin: &str,
    username: &str,
    role: &str,
) -> (String, String) {
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(admin),
            json!({
                "username": username,
                "password": OTHER_PASSWORD,
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
            json!({ "username": username, "password": OTHER_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let cookie = reply.session_cookie().expect("login must set a cookie");

    (id, cookie)
}

/// Creates a project and returns `(key, type_id)` for its first card type.
async fn project(app: &App, admin: &str, key: &str) -> (String, String) {
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
    let types = reply
        .json()
        .as_array()
        .unwrap_or_else(|| panic!("expected an array: {}", reply.raw_body))
        .clone();
    let type_id = types
        .iter()
        .find(|t| t["name"] == "Story")
        .unwrap_or_else(|| panic!("no Story type: {}", reply.raw_body))["id"]
        .as_str()
        .expect("a type id")
        .to_owned();

    (key.to_owned(), type_id)
}

async fn card(app: &App, admin: &str, project_key: &str, type_id: &str, summary: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(admin),
            json!({ "typeId": type_id, "summary": summary }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.key()
}

// ---------------------------------------------------------------------------
// 1. Secrets must not reach a response
// ---------------------------------------------------------------------------

/// Every marker that must never appear in a response body.
///
/// `$argon2` catches a PHC string from any source, which is the thing that
/// actually matters — a field renamed from `password_hash` to something else
/// still carries one.
const SECRET_MARKERS: &[&str] = &[
    "password_hash",
    "passwordHash",
    "$argon2",
    ADMIN_PASSWORD_MARKER,
    OTHER_PASSWORD,
    GOOD_PASSWORD,
];

/// `"Admin"` as a password is also the admin's *username*, so the bare word
/// cannot be swept for. This is the quoted-value form a JSON leak would take.
const ADMIN_PASSWORD_MARKER: &str = "\"password\":\"Admin\"";

fn assert_no_secrets(what: &str, body: &str) {
    for marker in SECRET_MARKERS {
        assert!(
            !body.contains(marker),
            "{what} leaked {marker:?} in its response body: {body}"
        );
    }
}

#[tokio::test]
async fn no_endpoint_that_describes_a_user_ever_emits_a_password_hash() {
    // The DTO is supposed to make this impossible by construction (`User` is not
    // `Serialize`). This asserts it on the *wire*, because "impossible by
    // construction" is a claim about the code as it is today, and the wire is
    // what an attacker reads.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (victim_id, victim) = user_past_the_gate(&app, &admin, "victim", "member").await;

    let sweep = [
        ("GET /auth/me", get("/api/v1/auth/me", Some(&admin))),
        (
            "GET /auth/sessions",
            get("/api/v1/auth/sessions", Some(&admin)),
        ),
        ("GET /users", get("/api/v1/users", Some(&admin))),
        (
            "GET /users/{id}",
            get(&format!("/api/v1/users/{victim_id}"), Some(&admin)),
        ),
        (
            "GET /auth/me (victim)",
            get("/api/v1/auth/me", Some(&victim)),
        ),
    ];

    for (what, request) in sweep {
        let reply = app.send(request).await;
        assert_eq!(reply.status, StatusCode::OK, "{what}: {}", reply.raw_body);
        assert_no_secrets(what, &reply.raw_body);
    }

    // The write paths echo a user back too, and they are the ones handling a
    // plaintext password in the same request.
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({
                "username": "sweep-target",
                "password": OTHER_PASSWORD,
                "role": "viewer",
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_no_secrets("POST /users", &reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/users/{victim_id}"),
            Some(&admin),
            json!({ "displayName": "Victim" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_no_secrets("PATCH /users/{id}", &reply.raw_body);

    // Login and change-password: the two handlers that hold a plaintext password
    // and a hash in the same scope.
    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": "victim", "password": OTHER_PASSWORD }),
        ))
        .await;
    assert_no_secrets("POST /auth/login", &reply.raw_body);

    let reply = app
        .send(post(
            "/api/v1/auth/change-password",
            Some(&victim),
            json!({ "currentPassword": OTHER_PASSWORD, "newPassword": GOOD_PASSWORD }),
        ))
        .await;
    assert_no_secrets("POST /auth/change-password", &reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_failed_login_never_says_which_half_was_wrong() {
    // A different document for "no such user" and "wrong password" is a username
    // oracle: it enumerates the instance without guessing a single password.
    let app = App::new().await;

    let unknown = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": "nobody-here", "password": "whatever-it-is" }),
        ))
        .await;
    let wrong = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": DEFAULT_ADMIN_USERNAME, "password": "not-the-password" }),
        ))
        .await;

    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unknown.raw_body, wrong.raw_body,
        "an unknown username and a wrong password must be byte-for-byte identical"
    );

    app.db.close().await;
}

#[tokio::test]
async fn the_session_list_shows_digests_and_never_the_token_in_the_cookie() {
    // The cookie's token is the credential; the row's id is its SHA-256. If the
    // list ever showed the token, `GET /auth/sessions` would hand an attacker
    // with one stolen session every *other* session on the account.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app.send(get("/api/v1/auth/sessions", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(
        !reply.raw_body.contains(&admin),
        "the session list echoed the caller's own session token: {}",
        reply.raw_body
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// 2. Authorisation — reaching a row that is not yours
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewer_cannot_touch_another_user_at_all() {
    // The whole `/users` surface is admin-only. A viewer is the least-privileged
    // role, so it is the one that proves the guard rather than the role.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (victim_id, _) = user_past_the_gate(&app, &admin, "victim", "member").await;
    let (_, viewer) = user_past_the_gate(&app, &admin, "nosy", "viewer").await;

    let attacks = [
        ("read the user list", get("/api/v1/users", Some(&viewer))),
        (
            "read another user",
            get(&format!("/api/v1/users/{victim_id}"), Some(&viewer)),
        ),
        (
            "edit another user",
            patch(
                &format!("/api/v1/users/{victim_id}"),
                Some(&viewer),
                json!({ "displayName": "pwned" }),
            ),
        ),
        (
            "promote another user",
            patch(
                &format!("/api/v1/users/{victim_id}"),
                Some(&viewer),
                json!({ "role": "admin" }),
            ),
        ),
        (
            "deactivate another user",
            post(
                &format!("/api/v1/users/{victim_id}/deactivate"),
                Some(&viewer),
                json!({}),
            ),
        ),
        (
            "create an admin",
            post(
                "/api/v1/users",
                Some(&viewer),
                json!({ "username": "backdoor", "password": OTHER_PASSWORD, "role": "admin" }),
            ),
        ),
    ];

    for (what, request) in attacks {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a viewer could {what}: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn a_member_cannot_promote_themselves_to_admin() {
    // Privilege escalation, the direct way. `/users` is admin-only, so a member
    // patching their *own* row is the same 403 — worth its own test because "it
    // is my own row" is exactly the argument for a self-service exception that
    // would open this up.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (member_id, member) = user_past_the_gate(&app, &admin, "climber", "member").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/users/{member_id}"),
            Some(&member),
            json!({ "role": "admin" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a member promoted themselves: {}",
        reply.raw_body
    );

    // ...and the role really did not move.
    let reply = app.send(get("/api/v1/auth/me", Some(&member))).await;
    assert_eq!(reply.json()["role"], "member", "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn one_user_cannot_revoke_another_users_session() {
    // Session ids are shown to their owner by `GET /auth/sessions`, so they are
    // not secret — the ownership check is the only thing standing between that
    // and logging anybody out at will.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (_, victim) = user_past_the_gate(&app, &admin, "victim", "member").await;
    let (_, attacker) = user_past_the_gate(&app, &admin, "attacker", "member").await;

    let reply = app.send(get("/api/v1/auth/sessions", Some(&victim))).await;
    let victim_session = reply.json()[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no session id: {}", reply.raw_body))
        .to_owned();

    let reply = app
        .send(request(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{victim_session}"),
            Some(&attacker),
            None,
        ))
        .await;
    // 404 rather than 403: "that is not yours" would confirm the id is real.
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "one user revoked another's session: {}",
        reply.raw_body
    );

    // The victim's session still works.
    let reply = app.send(get("/api/v1/auth/me", Some(&victim))).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "the victim was logged out by someone else"
    );

    app.db.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_viewer_cannot_write_anything_in_the_domain() {
    // Viewer is read-only across projects, cards, comments and tags. One test
    // over the whole write surface, because the failure mode is one route that
    // forgot.
    //
    // The viewer is made an **owner** of the project on purpose, and that is
    // what makes this test worth more than it looks. Since per-project access
    // landed, a viewer with no grant is refused with 404 — they cannot see the
    // project, so every answer below would be "not found" and the test would
    // prove nothing about writing. Granting them the *most* privileged project
    // role available strips that away and leaves exactly one question: does the
    // instance role still stop them?
    //
    // It must. The instance role is a ceiling, not a floor
    // (`domain::member::ProjectRole::capped_by`): an `owner` row held by an
    // instance Viewer resolves to `viewer`, so every write below is 403 — from
    // the project role, on a project they can see perfectly well.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (project_key, type_id) = project(&app, &admin, "ATLAS").await;
    let key = card(&app, &admin, &project_key, &type_id, "Card").await;
    let (viewer_id, viewer) = user_past_the_gate(&app, &admin, "readonly", "viewer").await;

    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/members"),
            Some(&admin),
            json!({ "userId": viewer_id, "role": "owner" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["effectiveRole"],
        "viewer",
        "an instance viewer was granted `owner` and the API says they are one: {}",
        reply.raw_body
    );

    // ...and they really can read it, so a 403 below is about the write and not
    // about visibility.
    assert_eq!(
        app.send(get(
            &format!("/api/v1/projects/{project_key}"),
            Some(&viewer)
        ))
        .await
        .status,
        StatusCode::OK,
        "the project owner cannot see their own project"
    );

    let attacks = [
        (
            "create a project",
            post(
                "/api/v1/projects",
                Some(&viewer),
                json!({ "key": "EVIL", "name": "Evil", "template": "blank" }),
            ),
        ),
        (
            "edit a project",
            patch(
                &format!("/api/v1/projects/{project_key}"),
                Some(&viewer),
                json!({ "name": "pwned" }),
            ),
        ),
        (
            "archive a project",
            post(
                &format!("/api/v1/projects/{project_key}/archive"),
                Some(&viewer),
                json!({}),
            ),
        ),
        (
            "create a card",
            post(
                &format!("/api/v1/projects/{project_key}/cards"),
                Some(&viewer),
                json!({ "typeId": type_id, "summary": "evil" }),
            ),
        ),
        (
            "edit a card",
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&viewer),
                json!({ "summary": "pwned" }),
            ),
        ),
        (
            "delete a card",
            request(
                Method::DELETE,
                &format!("/api/v1/cards/{key}"),
                Some(&viewer),
                None,
            ),
        ),
        (
            "comment on a card",
            post(
                &format!("/api/v1/cards/{key}/comments"),
                Some(&viewer),
                json!({ "body": "evil" }),
            ),
        ),
        (
            "create a tag",
            post(
                &format!("/api/v1/projects/{project_key}/tags"),
                Some(&viewer),
                json!({ "name": "evil", "colour": "#ff0000" }),
            ),
        ),
    ];

    for (what, request) in attacks {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a viewer could {what}: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// 3. Made-up ids and hostile text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_made_up_assignee_is_refused_rather_than_crashing_the_request() {
    // `assigneeId` and `reporterId` are the two card fields written straight
    // through with no project scoping — the others (`statusId`, `priorityId`,
    // `resolutionId`, `typeId`) are each checked against the project first.
    // The database's foreign key is the only thing left, and a raw FK violation
    // surfaces as a 500: an internal error, logged as an incident, for what is
    // plainly a bad request. 4xx or 5xx is the whole question here.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (project_key, type_id) = project(&app, &admin, "ATLAS").await;
    let key = card(&app, &admin, &project_key, &type_id, "Card").await;

    for (field, value) in [
        ("assigneeId", "no-such-user"),
        ("reporterId", "no-such-user"),
    ] {
        let reply = app
            .send(patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ field: value }),
            ))
            .await;

        assert!(
            reply.status.is_client_error(),
            "PATCH {field} pointing at a user that does not exist must be a client error, not {}: \
             {}",
            reply.status,
            reply.raw_body
        );

        // The create path takes the same two fields and must answer the same way.
        let reply = app
            .send(post(
                &format!("/api/v1/projects/{project_key}/cards"),
                Some(&admin),
                json!({ "typeId": type_id, "summary": "Card", field: value }),
            ))
            .await;
        assert!(
            reply.status.is_client_error(),
            "POST {field} pointing at a user that does not exist must be a client error, not {}: \
             {}",
            reply.status,
            reply.raw_body
        );
    }

    // A real user id is still accepted — the check must not reject everybody.
    let (victim_id, _) = user_past_the_gate(&app, &admin, "assignable", "member").await;
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": victim_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "a real user must still be assignable: {}",
        reply.raw_body
    );
    assert_eq!(reply.json()["assigneeId"], victim_id);

    // ...including a deactivated one: accounts are never hard-deleted precisely
    // because cards reference them, so a card assigned to someone who has since
    // left must keep saying so.
    let reply = app
        .send(post(
            &format!("/api/v1/users/{victim_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": Value::Null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": victim_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "a deactivated user must still be assignable — existence is the check, not eligibility: {}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn hostile_text_is_stored_and_returned_verbatim_never_executed() {
    // Two claims at once. The SQL payloads prove nothing is concatenated into a
    // query — a real injection would drop the table and the later read would
    // fail. The HTML/markdown payload proves Atlas stores *source* and does not
    // pre-render: a body that comes back escaped, or altered, would mean
    // something rendered it on the way in.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (project_key, type_id) = project(&app, &admin, "ATLAS").await;

    let payloads = [
        "'; DROP TABLE cards; --",
        "\" OR \"1\"=\"1",
        "1'); DELETE FROM users WHERE ('1'='1",
        "<script>alert('xss')</script>",
        "Robert'); DROP TABLE students;--",
        "\u{202e}bidi override",
    ];

    for payload in payloads {
        let key = card(&app, &admin, &project_key, &type_id, payload).await;

        let reply = app
            .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
        assert_eq!(
            reply.json()["summary"],
            payload,
            "the summary did not survive a round trip unchanged"
        );

        let reply = app
            .send(post(
                &format!("/api/v1/cards/{key}/comments"),
                Some(&admin),
                json!({ "body": payload }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
        assert_eq!(
            reply.json()["body"],
            payload,
            "the comment body was altered on the way in — markdown source must be stored raw"
        );
    }

    // A control character in a summary is refused outright rather than stored —
    // a NUL is not whitespace, so trimming does not remove it, and a summary is
    // a single line by definition.
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(&admin),
            json!({ "typeId": type_id, "summary": "embedded\u{0}nul" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a NUL in a summary must be refused: {}",
        reply.raw_body
    );

    // The tables are all still there, which is what "nothing was concatenated"
    // actually looks like.
    let reply = app.send(get("/api/v1/users", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "users survived");
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "cards survived");
    assert_eq!(
        reply.json()["total"],
        json!(payloads.len()),
        "every hostile summary is still a row: {}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn hostile_text_in_a_path_or_a_query_string_does_not_reach_sql() {
    // The same claim from the other side: ids and keys arrive in the URL, and a
    // 404 or a 4xx is the only acceptable answer. A 500 would mean the text got
    // far enough to break something.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (project_key, _) = project(&app, &admin, "ATLAS").await;

    let probes = [
        "/api/v1/cards/'%20OR%20'1'%3D'1",
        "/api/v1/cards/ATLAS-1'%3B%20DROP%20TABLE%20cards%3B%20--",
        "/api/v1/projects/'%3B%20DROP%20TABLE%20projects%3B%20--",
        "/api/v1/users/'%20OR%201%3D1%20--",
        "/api/v1/comments/'%20OR%201%3D1%20--",
    ];

    for probe in probes {
        let reply = app.send(get(probe, Some(&admin))).await;
        assert!(
            reply.status.is_client_error(),
            "{probe} returned {} — hostile path text must never reach SQL: {}",
            reply.status,
            reply.raw_body
        );
    }

    // A filter value is the other injection surface, and it is the one that gets
    // interpolated in most codebases.
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/cards?statusId=%27%20OR%20%271%27%3D%271"),
            Some(&admin),
        ))
        .await;
    assert!(
        reply.status.is_success() || reply.status.is_client_error(),
        "a hostile filter value returned {}: {}",
        reply.status,
        reply.raw_body
    );

    // Everything is still standing.
    let reply = app.send(get("/api/v1/projects", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "projects survived");

    app.db.close().await;
}

#[tokio::test]
async fn an_internal_error_never_carries_sql_or_a_column_name_to_the_client() {
    // `AppError::Internal` is supposed to be opaque. The unit test in `error.rs`
    // proves the rendering; this proves it over the wire, on a request that
    // actually reaches the database with a value it cannot use.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (project_key, type_id) = project(&app, &admin, "ATLAS").await;
    let key = card(&app, &admin, &project_key, &type_id, "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": "no-such-user" }),
        ))
        .await;

    for marker in [
        "FOREIGN KEY",
        "constraint",
        "INSERT INTO",
        "UPDATE cards",
        "sqlx",
        "users (id)",
    ] {
        assert!(
            !reply.raw_body.contains(marker),
            "the response carried {marker:?} to the client: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

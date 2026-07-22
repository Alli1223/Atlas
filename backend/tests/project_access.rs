//! Per-project access, end to end, over the real router and a real database.
//!
//! These exist to *break* the gate in `auth::project_access`, not to demonstrate
//! it. The bug they were written against was total: before this landed, any
//! authenticated user could read every project and any Member could edit,
//! archive and write cards in every one of them.
//!
//! Two of them are worth more than the rest, and for the same reason
//! `tests/auth_gate_adversarial.rs` earns its keep — they are derived from the
//! router rather than from a list somebody has to remember to update:
//!
//! - [`every_project_scoped_route_refuses_an_outsider`] walks the real route
//!   table and probes each one with a stranger's cookie.
//! - [`every_route_under_api_v1_is_classified`] reads the live OpenAPI document
//!   and checks it against the scope table both ways.
//!
//! A hand-written list of routes to check *is* the per-handler check the layer
//! exists to replace: it goes stale the moment somebody adds a route and
//! forgets, which is precisely the bug being hunted.

use atlas::api::{self, AppState};
use atlas::auth::project_access;
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

// ---------------------------------------------------------------------------
// Harness — the shape `tests/auth.rs` established.
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

    fn id(&self) -> String {
        self.json()["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in {}", self.raw_body))
            .to_owned()
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

fn post(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::POST, uri, cookie, Some(body))
}

fn patch(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PATCH, uri, cookie, Some(body))
}

fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
}

/// The seeded admin, past the forced-reset gate.
async fn admin_past_the_gate(app: &App) -> String {
    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": DEFAULT_ADMIN_USERNAME, "password": ADMIN_PASSWORD }),
        ))
        .await;
    let cookie = reply.session_cookie().expect("login must set a cookie");

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

/// A fresh account with an instance role, signed in. Returns `(id, cookie)`.
async fn user(app: &App, admin: &str, username: &str, role: &str) -> (String, String) {
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
    let id = reply.id();

    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let cookie = reply.session_cookie().expect("login must set a cookie");

    (id, cookie)
}

/// A project and the ids of the config rows its template seeded.
struct Project {
    key: String,
    type_id: String,
    status_id: String,
    priority_id: String,
    resolution_id: String,
    level_id: String,
}

async fn project(app: &App, creator: &str, key: &str) -> Project {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(creator),
            json!({ "key": key, "name": key, "template": "programming" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let first = |path: &str| {
        let uri = format!("/api/v1/projects/{key}/{path}");
        async move {
            let reply = app.send(get(&uri, Some(creator))).await;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
            reply.json()[0]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no rows at {uri}: {}", reply.raw_body))
                .to_owned()
        }
    };

    Project {
        key: key.to_owned(),
        type_id: first("card-types").await,
        status_id: first("statuses").await,
        priority_id: first("priorities").await,
        resolution_id: first("resolutions").await,
        level_id: first("hierarchy-levels").await,
    }
}

async fn card(app: &App, cookie: &str, project: &Project, summary: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/cards", project.key),
            Some(cookie),
            json!({ "typeId": project.type_id, "summary": summary }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("a card key").to_owned()
}

/// Grants a project role.
async fn grant(app: &App, admin: &str, project_key: &str, user_id: &str, role: &str) {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/members"),
            Some(admin),
            json!({ "userId": user_id, "role": role }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

/// The keys in `GET /projects`.
async fn visible_projects(app: &App, cookie: &str) -> Vec<String> {
    let reply = app.send(get("/api/v1/projects", Some(cookie))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply
        .json()
        .as_array()
        .expect("an array")
        .iter()
        .map(|p| p["key"].as_str().expect("a key").to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// The headline claims
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_member_of_one_project_cannot_see_another() {
    // The bug this whole phase exists to close, stated as plainly as it can be.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let a = project(&app, &admin, "AAA").await;
    project(&app, &admin, "BBB").await;
    let (id, cookie) = user(&app, &admin, "insider", "member").await;
    grant(&app, &admin, &a.key, &id, "member").await;

    // A is theirs.
    assert_eq!(
        app.send(get("/api/v1/projects/AAA", Some(&cookie)))
            .await
            .status,
        StatusCode::OK
    );

    // B is 404 — not 403. A 403 would confirm that BBB exists, and the project
    // key namespace is small enough to walk: an outsider could map every project
    // on the instance one guess at a time without ever being let in. To someone
    // with no grant, an inaccessible project must be indistinguishable from one
    // that was never created.
    let reply = app.send(get("/api/v1/projects/BBB", Some(&cookie))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["type"],
        "urn:atlas:error:not-found",
        "an inaccessible project must look exactly like a missing one, and that includes the \
         problem document: {}",
        reply.raw_body
    );

    // And it is the same answer, field for field, as for a project that really
    // is absent. `instance` is the request's own path, so it necessarily differs
    // and is dropped before comparing — everything that could carry a signal
    // about *why* the answer is 404 has to match.
    let absent = app
        .send(get("/api/v1/projects/NOSUCHKEY", Some(&cookie)))
        .await;
    let without_instance = |reply: &Reply| {
        let mut json = reply.json();
        json.as_object_mut()
            .expect("a problem document")
            .remove("instance");
        json
    };
    assert_eq!(
        without_instance(&reply),
        without_instance(&absent),
        "an inaccessible project is distinguishable from a nonexistent one"
    );
    assert_eq!(reply.status, absent.status);

    // The list filters rather than refusing.
    assert_eq!(visible_projects(&app, &cookie).await, ["AAA"]);
}

#[tokio::test]
async fn the_project_list_filters_and_never_refuses() {
    // A 403 on a list is a bug: it turns "here is your work" into "you are not
    // allowed to have work". An inaccessible project is simply absent.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "AAA").await;
    project(&app, &admin, "BBB").await;
    let (_, nobody) = user(&app, &admin, "nobody", "member").await;

    let reply = app.send(get("/api/v1/projects", Some(&nobody))).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "a user with no projects got an error instead of an empty list: {}",
        reply.raw_body
    );
    assert_eq!(reply.json().as_array().unwrap().len(), 0);

    // Including with includeArchived, which is a second query and so a second
    // chance to forget the filter.
    let reply = app
        .send(get("/api/v1/projects?includeArchived=true", Some(&nobody)))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json().as_array().unwrap().len(),
        0,
        "includeArchived=true leaked every archived project on the instance: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_project_viewer_cannot_create_a_card_but_a_project_member_can() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATLAS").await;

    let (viewer_id, viewer) = user(&app, &admin, "looker", "member").await;
    grant(&app, &admin, &p.key, &viewer_id, "viewer").await;

    let (member_id, member) = user(&app, &admin, "doer", "member").await;
    grant(&app, &admin, &p.key, &member_id, "member").await;

    let create = |cookie: &str| {
        post(
            "/api/v1/projects/ATLAS/cards",
            Some(cookie),
            json!({ "typeId": p.type_id, "summary": "x" }),
        )
    };

    // 403, not 404: the viewer can see this project perfectly well — it is in
    // their list — so "it does not exist" would be a lie they could disprove in
    // one request. They know it is there; they are being told they may not.
    let reply = app.send(create(&viewer)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.raw_body);

    // ...and they can still read it.
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS/cards", Some(&viewer)))
            .await
            .status,
        StatusCode::OK
    );

    let reply = app.send(create(&member)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
}

#[tokio::test]
async fn a_project_member_cannot_administer_the_project_but_an_owner_can() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATLAS").await;

    let (member_id, member) = user(&app, &admin, "doer", "member").await;
    grant(&app, &admin, &p.key, &member_id, "member").await;

    let (owner_id, owner) = user(&app, &admin, "boss", "member").await;
    grant(&app, &admin, &p.key, &owner_id, "owner").await;

    let (stranger_id, _) = user(&app, &admin, "stranger", "member").await;

    // Everything an owner may do and a member may not, built fresh per caller —
    // a `Request` cannot be replayed, so the list is a function of the cookie.
    let owner_only = |cookie: &str| -> Vec<(&'static str, Request<Body>)> {
        vec![
            (
                "edit project settings",
                patch(
                    "/api/v1/projects/ATLAS",
                    Some(cookie),
                    json!({ "name": "Renamed" }),
                ),
            ),
            (
                "archive the project",
                post("/api/v1/projects/ATLAS/archive", Some(cookie), json!({})),
            ),
            (
                "add a member",
                post(
                    "/api/v1/projects/ATLAS/members",
                    Some(cookie),
                    json!({ "userId": stranger_id, "role": "viewer" }),
                ),
            ),
            (
                "add a status",
                post(
                    "/api/v1/projects/ATLAS/statuses",
                    Some(cookie),
                    json!({ "name": "Nope", "category": "todo", "position": 99 }),
                ),
            ),
        ]
    };

    // 403 and not 404: a member can see this project, so "it does not exist"
    // would be a lie they could disprove in one request.
    for (what, request) in owner_only(&member) {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a project member could {what}: {}",
            reply.raw_body
        );
    }

    for (what, request) in owner_only(&owner) {
        let reply = app.send(request).await;
        assert!(
            reply.status.is_success(),
            "a project owner could not {what}: {} {}",
            reply.status,
            reply.raw_body
        );
    }

    // A member may still read the member list — "who do I ask for more access?"
    // is not a privileged question, and a member who cannot answer it is stuck.
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS/members", Some(&member)))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_instance_admin_reaches_every_project_with_no_row_at_all() {
    // Rule 1. Without it an admin can be locked out of a project they are
    // nonetheless responsible for administering, and no API call can repair it —
    // the only fix would be editing the database by hand.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // A project the admin has nothing to do with: created by, and led by,
    // somebody else.
    let (member_id, member) = user(&app, &admin, "founder", "member").await;
    let p = project(&app, &member, "THEIRS").await;
    let c = card(&app, &member, &p, "Card").await;

    // The admin holds no project_members row here.
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members m JOIN users u ON u.id = m.user_id \
         WHERE u.username = 'Admin'",
    )
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(rows, 0, "the admin was given a row, so this proves nothing");

    for uri in [
        "/api/v1/projects/THEIRS",
        "/api/v1/projects/THEIRS/cards",
        "/api/v1/projects/THEIRS/members",
        "/api/v1/projects/THEIRS/statuses",
    ] {
        let reply = app.send(get(uri, Some(&admin))).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}: {}", reply.raw_body);
    }

    // ...and owner-level writes, which is the whole point of rule 1.
    let reply = app
        .send(patch(
            "/api/v1/projects/THEIRS",
            Some(&admin),
            json!({ "name": "Administered" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{c}"),
            Some(&admin),
            json!({ "summary": "Edited" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    assert_eq!(visible_projects(&app, &admin).await, ["THEIRS"]);
    let _ = member_id;
}

#[tokio::test]
async fn an_instance_viewer_who_is_a_project_owner_still_cannot_write() {
    // The instance role is a **ceiling, not a floor**. "This account is
    // read-only" is a statement about the account, and no project owner may
    // quietly overrule it by handing out a grant.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATLAS").await;
    let c = card(&app, &admin, &p, "Card").await;

    let (id, viewer) = user(&app, &admin, "readonly", "viewer").await;
    grant(&app, &admin, &p.key, &id, "owner").await;

    // The API is honest about it rather than echoing the row back.
    let reply = app
        .send(get("/api/v1/projects/ATLAS/members", Some(&admin)))
        .await;
    let row = reply
        .json()
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["userId"] == json!(id))
        .cloned()
        .expect("the viewer is in the member list");
    assert_eq!(row["role"], "owner", "the grant is stored as written");
    assert_eq!(
        row["effectiveRole"], "viewer",
        "the member list says an instance viewer is a project owner: {row}"
    );

    // Reading is fine — a viewer is a viewer.
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS", Some(&viewer)))
            .await
            .status,
        StatusCode::OK
    );

    // Every write, including the owner-only ones their row nominally grants.
    let writes = [
        patch(
            "/api/v1/projects/ATLAS",
            Some(&viewer),
            json!({ "name": "x" }),
        ),
        post("/api/v1/projects/ATLAS/archive", Some(&viewer), json!({})),
        post(
            "/api/v1/projects/ATLAS/members",
            Some(&viewer),
            json!({ "userId": id, "role": "viewer" }),
        ),
        post(
            "/api/v1/projects/ATLAS/cards",
            Some(&viewer),
            json!({ "typeId": p.type_id, "summary": "x" }),
        ),
        patch(
            &format!("/api/v1/cards/{c}"),
            Some(&viewer),
            json!({ "summary": "x" }),
        ),
        post(
            &format!("/api/v1/cards/{c}/comments"),
            Some(&viewer),
            json!({ "body": "x" }),
        ),
        post(
            "/api/v1/projects/ATLAS/tags",
            Some(&viewer),
            json!({ "name": "x" }),
        ),
        post(
            "/api/v1/projects/ATLAS/statuses",
            Some(&viewer),
            json!({ "name": "x", "category": "todo", "position": 9 }),
        ),
        delete(&format!("/api/v1/cards/{c}"), Some(&viewer)),
    ];

    for request in writes {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "an instance viewer wrote through {method} {uri}: {}",
            reply.raw_body
        );
    }
}

// ---------------------------------------------------------------------------
// The last-owner guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_last_owner_of_a_project_cannot_be_removed_or_demoted() {
    // The exact shape of the last-active-admin guard in `api::users`, one level
    // down. A project whose member list holds nobody who can manage it can only
    // be repaired by an instance admin, and "escalate to an admin" is the
    // outcome that guard exists to avoid.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    // The creator is the only owner.
    let owners: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members WHERE role = 'owner' AND project_id = \
         (SELECT id FROM projects WHERE key = 'ATLAS')",
    )
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(owners, 1);

    let demote = || {
        patch(
            &format!("/api/v1/projects/ATLAS/members/{founder_id}"),
            Some(&founder),
            json!({ "role": "member" }),
        )
    };
    let remove = || {
        delete(
            &format!("/api/v1/projects/ATLAS/members/{founder_id}"),
            Some(&founder),
        )
    };

    let reply = app.send(demote()).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
    let reply = app.send(remove()).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // The refusals are real, not cosmetic: the row is untouched.
    let role: String = sqlx::query_scalar(
        "SELECT role FROM project_members WHERE user_id = ? AND project_id = \
         (SELECT id FROM projects WHERE key = 'ATLAS')",
    )
    .bind(&founder_id)
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(role, "owner", "the last owner was demoted anyway");

    // With a second owner, both are allowed — so the guard is the count and not
    // a blanket refusal.
    let (second_id, _) = user(&app, &admin, "cofounder", "member").await;
    grant(&app, &founder, "ATLAS", &second_id, "owner").await;

    let reply = app.send(demote()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["role"], "member");

    // And now the *second* owner is the last one, so they are protected in turn.
    let reply = app
        .send(delete(
            &format!("/api/v1/projects/ATLAS/members/{second_id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "the guard did not follow the count: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn an_owner_row_that_grants_nothing_does_not_satisfy_the_last_owner_guard() {
    // The guard counts people who can do the job, not rows that look like they
    // could. An `owner` row held by an instance Viewer resolves to `viewer` (the
    // ceiling), so it cannot manage the project — and if it were counted, the
    // last *real* owner could be demoted straight past the guard, leaving exactly
    // the ownerless member list the guard exists to prevent.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    // A second `owner` row that grants nothing.
    let (viewer_id, _) = user(&app, &admin, "readonly", "viewer").await;
    grant(&app, &founder, "ATLAS", &viewer_id, "owner").await;

    // Two owner *rows* now, but only one owner.
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members WHERE role = 'owner' AND project_id = \
         (SELECT id FROM projects WHERE key = 'ATLAS')",
    )
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(rows, 2, "the fixture is not set up as intended");

    let reply = app
        .send(patch(
            &format!("/api/v1/projects/ATLAS/members/{founder_id}"),
            Some(&founder),
            json!({ "role": "member" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "the only real owner was demoted because a read-only account held an owner row: {}",
        reply.raw_body
    );

    // ...and the mirror image: removing the row that grants nothing is fine,
    // because it takes nothing away. The guard must not fire on it.
    let reply = app
        .send(delete(
            &format!("/api/v1/projects/ATLAS/members/{viewer_id}"),
            Some(&founder),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NO_CONTENT,
        "an owner row that grants nothing was protected as though it were the last owner: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_deactivated_owner_does_not_satisfy_the_last_owner_guard() {
    // The same argument as `auth::user::active_admin_count`'s `is_active = 1`: a
    // deactivated account cannot make any request at all, so it cannot be the
    // person keeping a project manageable.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let (dormant_id, _) = user(&app, &admin, "dormant", "member").await;
    grant(&app, &founder, "ATLAS", &dormant_id, "owner").await;
    let reply = app
        .send(post(
            &format!("/api/v1/users/{dormant_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/projects/ATLAS/members/{founder_id}"),
            Some(&founder),
            json!({ "role": "member" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "the only reachable owner was demoted because a deactivated account held an owner row: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_non_owner_member_can_be_removed_freely() {
    // The other direction, so the guard above cannot be "always refuse".
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let (helper_id, helper) = user(&app, &admin, "helper", "member").await;
    grant(&app, &founder, "ATLAS", &helper_id, "member").await;

    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS", Some(&helper)))
            .await
            .status,
        StatusCode::OK
    );

    let reply = app
        .send(delete(
            &format!("/api/v1/projects/ATLAS/members/{helper_id}"),
            Some(&founder),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    // Revocation takes effect on the very next request, with no session change.
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS", Some(&helper)))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "a revoked member could still read the project"
    );
    let _ = founder_id;
}

// ---------------------------------------------------------------------------
// The routes that are not keyed on a project key
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn card_comment_tag_and_config_routes_are_all_scoped() {
    // The routes a path-only layer could not have guarded: each is keyed on the
    // id of something whose project has to be looked up first. Enumerated one by
    // one, because "the card routes are scoped" is a claim about each of them.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let theirs = project(&app, &admin, "THEIRS").await;
    let their_card = card(&app, &admin, &theirs, "Card").await;

    // A comment and a tag inside the project the outsider cannot reach.
    let comment_id = app
        .send(post(
            &format!("/api/v1/cards/{their_card}/comments"),
            Some(&admin),
            json!({ "body": "private" }),
        ))
        .await
        .id();
    let tag_id = app
        .send(post(
            "/api/v1/projects/THEIRS/tags",
            Some(&admin),
            json!({ "name": "private" }),
        ))
        .await
        .id();

    // The outsider is a Member of a *different* project, so they are a fully
    // paid-up user of the instance — just not of this one.
    let mine = project(&app, &admin, "MINE").await;
    let (id, outsider) = user(&app, &admin, "outsider", "member").await;
    grant(&app, &admin, &mine.key, &id, "owner").await;

    let probes: Vec<(&str, Request<Body>)> = vec![
        // Cards, by card key.
        (
            "GET a card",
            get(&format!("/api/v1/cards/{their_card}"), Some(&outsider)),
        ),
        (
            "PATCH a card",
            patch(
                &format!("/api/v1/cards/{their_card}"),
                Some(&outsider),
                json!({ "summary": "pwned" }),
            ),
        ),
        (
            "DELETE a card",
            delete(&format!("/api/v1/cards/{their_card}"), Some(&outsider)),
        ),
        (
            "restore a card",
            post(
                &format!("/api/v1/cards/{their_card}/restore"),
                Some(&outsider),
                json!({}),
            ),
        ),
        (
            "move a card",
            post(
                &format!("/api/v1/cards/{their_card}/move"),
                Some(&outsider),
                json!({ "statusId": theirs.status_id }),
            ),
        ),
        (
            "reparent a card",
            post(
                &format!("/api/v1/cards/{their_card}/reparent"),
                Some(&outsider),
                json!({ "parentId": null }),
            ),
        ),
        (
            "a card's children",
            get(
                &format!("/api/v1/cards/{their_card}/children"),
                Some(&outsider),
            ),
        ),
        (
            "a card's history",
            get(
                &format!("/api/v1/cards/{their_card}/history"),
                Some(&outsider),
            ),
        ),
        // Comments.
        (
            "list comments",
            get(
                &format!("/api/v1/cards/{their_card}/comments"),
                Some(&outsider),
            ),
        ),
        (
            "post a comment",
            post(
                &format!("/api/v1/cards/{their_card}/comments"),
                Some(&outsider),
                json!({ "body": "x" }),
            ),
        ),
        (
            "edit a comment",
            patch(
                &format!("/api/v1/comments/{comment_id}"),
                Some(&outsider),
                json!({ "body": "x" }),
            ),
        ),
        (
            "delete a comment",
            delete(&format!("/api/v1/comments/{comment_id}"), Some(&outsider)),
        ),
        // Tags.
        (
            "list a project's tags",
            get("/api/v1/projects/THEIRS/tags", Some(&outsider)),
        ),
        (
            "create a tag",
            post(
                "/api/v1/projects/THEIRS/tags",
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "edit a tag",
            patch(
                &format!("/api/v1/tags/{tag_id}"),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "delete a tag",
            delete(&format!("/api/v1/tags/{tag_id}"), Some(&outsider)),
        ),
        (
            "merge a tag",
            post(
                &format!("/api/v1/tags/{tag_id}/merge"),
                Some(&outsider),
                json!({ "intoTagId": tag_id }),
            ),
        ),
        (
            "list a card's tags",
            get(&format!("/api/v1/cards/{their_card}/tags"), Some(&outsider)),
        ),
        (
            "tag a card",
            post(
                &format!("/api/v1/cards/{their_card}/tags"),
                Some(&outsider),
                json!({ "tagId": tag_id }),
            ),
        ),
        (
            "untag a card",
            delete(
                &format!("/api/v1/cards/{their_card}/tags/{tag_id}"),
                Some(&outsider),
            ),
        ),
        // Config, by project key...
        (
            "list hierarchy levels",
            get("/api/v1/projects/THEIRS/hierarchy-levels", Some(&outsider)),
        ),
        (
            "list card types",
            get("/api/v1/projects/THEIRS/card-types", Some(&outsider)),
        ),
        (
            "list statuses",
            get("/api/v1/projects/THEIRS/statuses", Some(&outsider)),
        ),
        (
            "list priorities",
            get("/api/v1/projects/THEIRS/priorities", Some(&outsider)),
        ),
        (
            "list resolutions",
            get("/api/v1/projects/THEIRS/resolutions", Some(&outsider)),
        ),
        (
            "add a hierarchy level",
            post(
                "/api/v1/projects/THEIRS/hierarchy-levels",
                Some(&outsider),
                json!({ "level": 9, "name": "x" }),
            ),
        ),
        (
            "add a card type",
            post(
                "/api/v1/projects/THEIRS/card-types",
                Some(&outsider),
                json!({ "name": "x", "level": 0 }),
            ),
        ),
        (
            "add a status",
            post(
                "/api/v1/projects/THEIRS/statuses",
                Some(&outsider),
                json!({ "name": "x", "category": "todo", "position": 9 }),
            ),
        ),
        (
            "add a priority",
            post(
                "/api/v1/projects/THEIRS/priorities",
                Some(&outsider),
                json!({ "name": "x", "rank": 9 }),
            ),
        ),
        (
            "add a resolution",
            post(
                "/api/v1/projects/THEIRS/resolutions",
                Some(&outsider),
                json!({ "name": "x", "position": 9 }),
            ),
        ),
        // ...and config by row id, which is the shape a path layer cannot reach.
        (
            "rename a hierarchy level",
            patch(
                &format!("/api/v1/hierarchy-levels/{}", theirs.level_id),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "edit a card type",
            patch(
                &format!("/api/v1/card-types/{}", theirs.type_id),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "edit a status",
            patch(
                &format!("/api/v1/statuses/{}", theirs.status_id),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "edit a priority",
            patch(
                &format!("/api/v1/priorities/{}", theirs.priority_id),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "edit a resolution",
            patch(
                &format!("/api/v1/resolutions/{}", theirs.resolution_id),
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        // Project members of a project they cannot see.
        (
            "list members",
            get("/api/v1/projects/THEIRS/members", Some(&outsider)),
        ),
        (
            "add a member",
            post(
                "/api/v1/projects/THEIRS/members",
                Some(&outsider),
                json!({ "userId": id, "role": "owner" }),
            ),
        ),
        // Project lifecycle.
        (
            "GET the project",
            get("/api/v1/projects/THEIRS", Some(&outsider)),
        ),
        (
            "PATCH the project",
            patch(
                "/api/v1/projects/THEIRS",
                Some(&outsider),
                json!({ "name": "x" }),
            ),
        ),
        (
            "archive it",
            post(
                "/api/v1/projects/THEIRS/archive",
                Some(&outsider),
                json!({}),
            ),
        ),
        (
            "restore it",
            post(
                "/api/v1/projects/THEIRS/restore",
                Some(&outsider),
                json!({}),
            ),
        ),
        (
            "delete it",
            delete("/api/v1/projects/THEIRS", Some(&outsider)),
        ),
    ];

    for (what, request) in probes {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "an outsider could {what} ({method} {uri}) — expected 404, which is what a project \
             they cannot see must look like: {}",
            reply.raw_body
        );
    }

    // Nothing was actually touched.
    let reply = app
        .send(get(&format!("/api/v1/cards/{their_card}"), Some(&admin)))
        .await;
    assert_eq!(
        reply.json()["summary"],
        "Card",
        "a card was edited: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_comment_is_the_authors_even_from_inside_the_project() {
    // The classic IDOR spot, and the one place the project layer deliberately
    // stops short. `/comments/{id}` carries no project, so the layer resolves one
    // and demands Member — but *both* of these users are Members of the same
    // project, so the layer waves them both through. From there the only thing
    // standing between B and A's words is a single line in `api::comments`, with
    // no second guard behind it.
    //
    // The layer cannot help here even in principle: "is this your comment" is not
    // a question about projects, and every test above stops at the project
    // boundary. So it is pinned from the inside.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let shared = project(&app, &admin, "SHARED").await;
    let (author_id, author) = user(&app, &admin, "author", "member").await;
    let (other_id, other) = user(&app, &admin, "other", "member").await;
    grant(&app, &admin, &shared.key, &author_id, "member").await;
    grant(&app, &admin, &shared.key, &other_id, "member").await;

    let card_key = card(&app, &author, &shared, "Card").await;
    let comment_id = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/comments"),
            Some(&author),
            json!({ "body": "the author's words" }),
        ))
        .await
        .id();

    // Both can *read* it — they share the project, and that is the point.
    let reply = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/comments"),
            Some(&other),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // 403 and not 404: they can see the comment, so "it does not exist" would be
    // a lie they could disprove in one request. They are being told they may not.
    let reply = app
        .send(patch(
            &format!("/api/v1/comments/{comment_id}"),
            Some(&other),
            json!({ "body": "pwned" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a project member edited another member's comment: {}",
        reply.raw_body
    );

    let reply = app
        .send(delete(
            &format!("/api/v1/comments/{comment_id}"),
            Some(&other),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a project member deleted another member's comment: {}",
        reply.raw_body
    );

    // An instance admin may delete it but must **not** edit it: taking words away
    // is moderation, putting words in someone's mouth under their name is not.
    // `api::comments::update_comment` is the one place an admin deliberately has
    // less power than the rules elsewhere would give them, so pin that asymmetry
    // rather than trusting the doc comment that asserts it.
    let reply = app
        .send(patch(
            &format!("/api/v1/comments/{comment_id}"),
            Some(&admin),
            json!({ "body": "pwned by an admin" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "an admin edited someone else's comment: {}",
        reply.raw_body
    );

    // The words are untouched.
    let reply = app
        .send(get(
            &format!("/api/v1/cards/{card_key}/comments"),
            Some(&author),
        ))
        .await;
    assert_eq!(
        reply.json()[0]["body"],
        "the author's words",
        "the comment was altered: {}",
        reply.raw_body
    );

    let reply = app
        .send(delete(
            &format!("/api/v1/comments/{comment_id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NO_CONTENT,
        "an admin must be able to moderate: {}",
        reply.raw_body
    );
}

/// The routes an outsider is allowed to reach, and why each one is on this list.
///
/// **This is the only hand-written list in
/// [`every_project_scoped_route_refuses_an_outsider`], and it is deliberately the
/// inverse of the one you would expect.** Listing the routes *to probe* would go
/// stale silently — a new route nobody added to it is a route nobody attacks, and
/// the test would keep passing while the hole sat there. Listing the routes to
/// *skip* fails the other way: a new route is probed by default, and making the
/// test pass means either scoping the route or arguing your way onto this list in
/// review. Same deny-by-default trade as `auth::project_access::SCOPES` itself.
///
/// Every entry is a route with no project in it at all:
///
/// - the auth routes act on the caller's own session, and login must work with no
///   session whatsoever;
/// - the user routes are instance administration, guarded by `RequireAdmin`;
/// - `GET /projects` filters rather than refuses — a list must never 403 — and is
///   pinned by [`the_project_list_filters_and_never_refuses`];
/// - `POST /projects` cannot be project-scoped: there is no project yet;
/// - `GET /project-templates` is static seed data and names no project.
const NOT_PROJECT_SCOPED: &[&str] = &[
    "POST /api/v1/auth/login",
    "POST /api/v1/auth/logout",
    "GET /api/v1/auth/me",
    "POST /api/v1/auth/change-password",
    "GET /api/v1/auth/sessions",
    "DELETE /api/v1/auth/sessions/{id}",
    "GET /api/v1/users",
    "POST /api/v1/users",
    "GET /api/v1/users/{id}",
    "PATCH /api/v1/users/{id}",
    "POST /api/v1/users/{id}/deactivate",
    "GET /api/v1/projects",
    "POST /api/v1/projects",
    "GET /api/v1/project-templates",
    // AQL search and saved filters: not project-scoped at the route level. A
    // search spans every project the caller can see and scopes itself in the
    // compiled SQL; filters are personal and checked by owner_id. So none of
    // these 404 for an "outsider" — there is no single project to be outside of.
    "POST /api/v1/search",
    "POST /api/v1/search/validate",
    "GET /api/v1/filters",
    "POST /api/v1/filters",
    "GET /api/v1/filters/{id}",
    "PATCH /api/v1/filters/{id}",
    "DELETE /api/v1/filters/{id}",
    "GET /api/v1/filters/{id}/results",
];

/// Every path parameter in the API, and a real value from a project the outsider
/// cannot reach.
///
/// Keyed on the segment *before* the parameter, because that is what says what
/// the id is an id *of* — `/comments/{id}` and `/statuses/{id}` are both `{id}`
/// and resolve to a project by completely different routes.
struct Targets {
    project_key: String,
    card_key: String,
    comment_id: String,
    tag_id: String,
    type_id: String,
    level_id: String,
    status_id: String,
    priority_id: String,
    resolution_id: String,
    member_user_id: String,
    workflow_id: String,
    transition_id: String,
    board_id: String,
}

impl Targets {
    /// A route template with every `{param}` replaced by something real.
    ///
    /// Returns `Err` naming the parameter when it meets one it has no value for,
    /// so that a Phase 8 route with a new parameter fails this test with an
    /// instruction rather than being quietly skipped — a probe that silently
    /// stops probing is worse than no probe.
    fn concrete(&self, path: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut previous = "";

        for segment in path.split('/').skip(1) {
            let value = if segment.starts_with('{') {
                match (previous, segment) {
                    ("projects", "{key}") => &self.project_key,
                    ("cards", "{key}") => &self.card_key,
                    ("comments", "{id}") => &self.comment_id,
                    // `/tags/{id}` and `/cards/{key}/tags/{tagId}` both sit under
                    // a `tags` segment and both name a tag.
                    ("tags", "{id}" | "{tagId}") => &self.tag_id,
                    ("card-types", "{id}") => &self.type_id,
                    ("hierarchy-levels", "{id}") => &self.level_id,
                    ("statuses", "{id}") => &self.status_id,
                    ("priorities", "{id}") => &self.priority_id,
                    ("resolutions", "{id}") => &self.resolution_id,
                    ("members", "{userId}") => &self.member_user_id,
                    ("workflows", "{id}") => &self.workflow_id,
                    // `/transitions/{id}` and `/cards/{key}/transitions/{id}`
                    // both name a transition.
                    ("transitions", "{id}") => &self.transition_id,
                    ("boards", "{id}") => &self.board_id,
                    _ => {
                        return Err(format!(
                            "no value known for {segment} after /{previous} in {path}. Teach \
                             `Targets` what it points at, so this route is attacked rather than \
                             skipped."
                        ));
                    }
                }
            } else {
                previous = segment;
                segment
            };
            out.push('/');
            out.push_str(value);
        }

        Ok(out)
    }
}

/// One body carrying every field any write route on the API asks for.
///
/// Sent to every write indiscriminately, which is coarse but deliberate: the
/// point is to give a route that *is* open enough to succeed with, so the probe
/// sees the hole rather than a validation error.
///
/// It does not make every write valid, and cannot — several request types are
/// `deny_unknown_fields`, so this object is rejected by them at deserialisation.
/// That is harmless here precisely because the probe demands **404 exactly**: a
/// 422 means a handler read the body, which means the layer let the request
/// through, which is the failure being hunted. Either way the route is reported.
fn every_write_body(actor_id: &str, targets: &Targets) -> Value {
    json!({
        "summary": "pwned",
        "body": "pwned",
        "name": "pwned",
        "userId": actor_id,
        "role": "owner",
        "tagId": targets.tag_id,
        "intoTagId": targets.tag_id,
        "parentId": null,
        "typeId": targets.type_id,
        "statusId": targets.status_id,
    })
}

/// Creates a custom workflow and a transition in it, so `/workflows/{id}` and
/// `/transitions/{id}` resolve to real rows in the project rather than 404ing for
/// an unrelated reason.
async fn furnish_workflow(app: &App, admin: &str, key: &str, status_id: &str) -> (String, String) {
    let workflow_id = app
        .send(post(
            &format!("/api/v1/projects/{key}/workflows"),
            Some(admin),
            json!({ "name": "Probe", "statusIds": [status_id] }),
        ))
        .await
        .id();
    let transition_id = app
        .send(post(
            &format!("/api/v1/workflows/{workflow_id}/transitions"),
            Some(admin),
            json!({ "name": "Probe", "toStatusId": status_id }),
        ))
        .await
        .id();
    (workflow_id, transition_id)
}

/// A project with one of everything in it, for an attacker to aim at.
///
/// Fully furnished on purpose: a route that resolves to no row 404s for reasons
/// that have nothing to do with access control, which would make this probe pass
/// while proving nothing.
async fn furnished_target_project(app: &App, admin: &str, key: &str) -> Targets {
    let theirs = project(app, admin, key).await;
    let their_card = card(app, admin, &theirs, "Card").await;

    let comment_id = app
        .send(post(
            &format!("/api/v1/cards/{their_card}/comments"),
            Some(admin),
            json!({ "body": "private" }),
        ))
        .await
        .id();
    let tag_id = app
        .send(post(
            &format!("/api/v1/projects/{key}/tags"),
            Some(admin),
            json!({ "name": "private" }),
        ))
        .await
        .id();

    // Somebody with a real grant, so `/members/{userId}` names a row that exists
    // rather than resolving to nothing for an uninteresting reason.
    let (insider_id, _) = user(app, admin, &format!("insider-{key}"), "member").await;
    grant(app, admin, key, &insider_id, "member").await;

    let (workflow_id, transition_id) = furnish_workflow(app, admin, key, &theirs.status_id).await;
    let board_id = furnish_board(app, admin, key).await;

    Targets {
        project_key: theirs.key.clone(),
        card_key: their_card,
        comment_id,
        tag_id,
        type_id: theirs.type_id.clone(),
        level_id: theirs.level_id.clone(),
        status_id: theirs.status_id.clone(),
        priority_id: theirs.priority_id.clone(),
        resolution_id: theirs.resolution_id.clone(),
        member_user_id: insider_id,
        workflow_id,
        transition_id,
        board_id,
    }
}

/// Saves a board in a project, so `/boards/{id}` resolves to a real row rather
/// than 404ing for an unrelated reason.
async fn furnish_board(app: &App, admin: &str, key: &str) -> String {
    app.send(post(
        &format!("/api/v1/projects/{key}/boards"),
        Some(admin),
        json!({ "name": "Probe" }),
    ))
    .await
    .id()
}

#[tokio::test]
async fn every_project_scoped_route_refuses_an_outsider() {
    // **The test the whole layer is for**, and the one this file's module doc
    // promises: not "the routes I remembered refuse an outsider" but *every route
    // the router actually serves*, enumerated from the live OpenAPI document that
    // `utoipa_axum::routes!` generates from the routes themselves.
    //
    // `card_comment_tag_and_config_routes_are_all_scoped` probes a hand-written
    // list of the same routes. That list is worth having — it is readable, and it
    // asserts the specific bodies and neighbours a generic probe cannot — but it
    // is exactly the artefact `auth::project_access`'s module doc warns about: it
    // cannot fail for a route that did not exist when it was written. This one
    // can, and that is the whole point.
    //
    // Every probe expects **exactly 404**, and nothing else is tolerated —
    // including a 422. An outsider has no access at all, so `member::require`
    // answers NotFound *in the layer*, before any handler has read the body. So a
    // 422 would mean the body reached a handler, which means the layer stood
    // aside, which is the hole being hunted. Accepting "anything that isn't 2xx"
    // would let exactly that through. And 403 is not tolerated either: it would
    // confirm the project exists and hand out the key namespace a guess at a time.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let targets = furnished_target_project(&app, &admin, "THEIRS").await;

    // The attacker: a fully paid-up instance Member who owns a project of their
    // own. Nothing about them is second-class — they simply have no grant here.
    let mine = project(&app, &admin, "MINE").await;
    let (outsider_id, outsider) = user(&app, &admin, "outsider", "member").await;
    grant(&app, &admin, &mine.key, &outsider_id, "owner").await;

    let reply = app.send(get(api::OPENAPI_JSON_PATH, None)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let spec = reply.json();
    let paths = spec["paths"].as_object().expect("the spec has paths");

    let mut probed = 0;
    let mut skipped = 0;
    let mut leaks: Vec<String> = Vec::new();

    for (path, item) in paths {
        if !path.starts_with(api::API_V1_PREFIX) {
            continue;
        }

        for method in item.as_object().expect("a path item").keys() {
            let parsed = match method.as_str() {
                "get" => Method::GET,
                "post" => Method::POST,
                "put" => Method::PUT,
                "patch" => Method::PATCH,
                "delete" => Method::DELETE,
                other => panic!("unhandled method {other} on {path}"),
            };

            let route = format!("{parsed} {path}");
            if NOT_PROJECT_SCOPED.contains(&route.as_str()) {
                skipped += 1;
                continue;
            }

            let uri = targets
                .concrete(path)
                .unwrap_or_else(|why| panic!("cannot probe {route}: {why}"));

            let body = match parsed {
                Method::POST | Method::PATCH | Method::PUT => {
                    Some(every_write_body(&outsider_id, &targets))
                }
                _ => None,
            };

            probed += 1;
            let reply = app.send(request(parsed, &uri, Some(&outsider), body)).await;

            if reply.status != StatusCode::NOT_FOUND {
                leaks.push(format!(
                    "{route}\n      probed as: {uri}\n      answered {} (expected 404)\n      {}",
                    reply.status,
                    reply.raw_body.chars().take(200).collect::<String>()
                ));
            }
        }
    }

    // Guards against the test passing vacuously — a spec that failed to parse, or
    // an allowlist that quietly swallowed the whole API, would otherwise look
    // like a clean run.
    assert_eq!(
        skipped,
        NOT_PROJECT_SCOPED.len(),
        "the unscoped allowlist has {} entries but only {skipped} matched a real route. A stale \
         entry means a route was renamed or removed — and if it was renamed, the new name is \
         being probed as project-scoped, which may be the real failure here.",
        NOT_PROJECT_SCOPED.len()
    );
    assert!(
        probed > 40,
        "only {probed} routes probed — the OpenAPI document is not being read correctly, so this \
         test would pass without attacking anything"
    );

    assert!(
        leaks.is_empty(),
        "{} of {probed} project-scoped route(s) did not refuse an outsider with 404:\n\n  - {}\n\n\
         An outsider is a full instance Member who owns their own project but has no grant on \
         THEIRS. Every route above either let them in, or told them THEIRS exists.",
        leaks.len(),
        leaks.join("\n  - ")
    );
}

#[tokio::test]
async fn a_retired_card_key_is_scoped_to_the_project_the_card_moved_to() {
    // `GET /cards/{key}` answers 301 for a retired key rather than 404, so the
    // gate has to see past `cards.key` into `card_key_history` — otherwise every
    // bookmark and commit message that key redirect exists for would 404 the
    // moment access control landed. And the access decision has to be about
    // where the card *is* now: if you cannot see where it went, you must not
    // learn that it went.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let from = project(&app, &admin, "FROM").await;
    let to = project(&app, &admin, "TO").await;
    let old_key = card(&app, &admin, &from, "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{old_key}"),
            Some(&admin),
            json!({ "projectKey": to.key }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let new_key = reply.json()["key"].as_str().unwrap().to_owned();
    assert_ne!(new_key, old_key, "the move must renumber the card");

    // Someone who can reach the destination follows the redirect.
    let (id, mover) = user(&app, &admin, "mover", "member").await;
    grant(&app, &admin, &to.key, &id, "member").await;
    let reply = app
        .send(get(&format!("/api/v1/cards/{old_key}"), Some(&mover)))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::MOVED_PERMANENTLY,
        "the retired key stopped redirecting: {}",
        reply.raw_body
    );

    // Someone who can only reach the *source* gets nothing — the card is not
    // theirs any more, and the old key must not be a window into where it went.
    let (stayer_id, stayer) = user(&app, &admin, "stayer", "member").await;
    grant(&app, &admin, &from.key, &stayer_id, "member").await;
    assert_eq!(
        app.send(get(&format!("/api/v1/cards/{old_key}"), Some(&stayer)))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "a retired key leaked a card that had moved out of the caller's project"
    );
}

#[tokio::test]
async fn a_card_cannot_be_moved_into_a_project_the_caller_cannot_reach() {
    // `PATCH /cards/{key}` with `projectKey` writes to a project the caller never
    // named in the URL, so the layer cannot see it — this is the one access check
    // that lives in a handler. Without it, Member on any one project would be a
    // licence to inject cards into every other one.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let mine = project(&app, &admin, "MINE").await;
    project(&app, &admin, "THEIRS").await;

    let (id, outsider) = user(&app, &admin, "outsider", "member").await;
    grant(&app, &admin, &mine.key, &id, "owner").await;
    let my_card = card(&app, &outsider, &mine, "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{my_card}"),
            Some(&outsider),
            json!({ "projectKey": "THEIRS" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "a card was pushed into a project the caller cannot reach: {}",
        reply.raw_body
    );

    // The card did not move.
    let reply = app
        .send(get(&format!("/api/v1/cards/{my_card}"), Some(&outsider)))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.json()["key"].as_str().unwrap().starts_with("MINE-"),
        "the card moved anyway: {}",
        reply.raw_body
    );

    // A project viewer on the destination is not enough either — writing needs
    // Member there, and this time they can see it, so it is a 403.
    let (viewer_id, viewer) = user(&app, &admin, "peeker", "member").await;
    grant(&app, &admin, &mine.key, &viewer_id, "member").await;
    grant(&app, &admin, "THEIRS", &viewer_id, "viewer").await;
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{my_card}"),
            Some(&viewer),
            json!({ "projectKey": "THEIRS" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a project viewer on the destination could move a card into it: {}",
        reply.raw_body
    );
}

#[tokio::test]
async fn a_global_tag_belongs_to_the_instance_and_not_to_any_project_owner() {
    // A global tag is usable from every project, so no project's owner has
    // authority over it — letting one project's member rename or delete it would
    // let them reach into every other project's boards.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATLAS").await;
    let (id, owner) = user(&app, &admin, "boss", "member").await;
    grant(&app, &admin, &p.key, &id, "owner").await;

    // Inserted directly, because a global tag is `tags.project_id IS NULL` and
    // no route creates one yet — `POST /projects/{key}/tags` always stamps the
    // project on. The schema has supported them since 0004 and the tag picker
    // already reads them, so the gate has to have an answer ready for the phase
    // that adds the route rather than discovering the question then.
    sqlx::query(
        "INSERT INTO tags (id, project_id, name, colour, created_at) VALUES (?, NULL, ?, NULL, ?)",
    )
    .bind("global-tag-1")
    .bind("company-wide")
    .bind("2026-07-17T00:00:00.000000+00:00")
    .execute(app.db.writer())
    .await
    .expect("failed to insert a global tag");
    let global_id = "global-tag-1".to_owned();

    // It really is global, and the project can see it.
    let reply = app
        .send(get("/api/v1/projects/ATLAS/tags", Some(&owner)))
        .await;
    assert!(
        reply.raw_body.contains("company-wide"),
        "the global tag is not offered to the project, so this tests nothing: {}",
        reply.raw_body
    );

    for request in [
        patch(
            &format!("/api/v1/tags/{global_id}"),
            Some(&owner),
            json!({ "name": "pwned" }),
        ),
        delete(&format!("/api/v1/tags/{global_id}"), Some(&owner)),
    ] {
        let method = request.method().clone();
        let reply = app.send(request).await;
        // 403 rather than 404: a global tag's existence is not a secret — every
        // project can already see it in its own tag list — so there is nothing to
        // hide, only something to refuse.
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a project owner could {method} a global tag: {}",
            reply.raw_body
        );
    }

    // The instance admin can.
    let reply = app
        .send(patch(
            &format!("/api/v1/tags/{global_id}"),
            Some(&admin),
            json!({ "name": "renamed-globally" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
}

// ---------------------------------------------------------------------------
// The gate itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_route_under_api_v1_is_classified() {
    // The property that makes forgetting hard: every route states its project
    // scope, and `api::router` panics if one does not — so this is really an
    // assertion about the error message rather than about the behaviour. It is
    // here because "the server did not start" is a worse bug report than "GET
    // /api/v1/whatever is not in SCOPES".
    //
    // Derived from the live OpenAPI document, which `utoipa_axum::routes!`
    // generates from the routes themselves. A hand-written list here would be
    // the very thing the gate exists to replace.
    let app = App::new().await;
    let reply = app.send(get(api::OPENAPI_JSON_PATH, None)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let spec = reply.json();
    let paths = spec["paths"].as_object().expect("the spec has paths");

    let mut unclassified = Vec::new();
    let mut checked = 0;

    for (path, item) in paths {
        if !path.starts_with(api::API_V1_PREFIX) {
            continue;
        }
        for method in item.as_object().expect("a path item").keys() {
            let parsed = match method.as_str() {
                "get" => Method::GET,
                "post" => Method::POST,
                "put" => Method::PUT,
                "patch" => Method::PATCH,
                "delete" => Method::DELETE,
                other => panic!("unhandled method {other} on {path}"),
            };
            checked += 1;
            if !project_access::is_classified(&parsed, path) {
                unclassified.push(format!("{parsed} {path}"));
            }
        }
    }

    assert!(
        checked > 20,
        "only {checked} routes found — the spec is not being read correctly"
    );
    assert!(
        unclassified.is_empty(),
        "{} of {checked} routes have no entry in auth::project_access::SCOPES:\n  {}",
        unclassified.len(),
        unclassified.join("\n  ")
    );
}

#[tokio::test]
async fn a_head_request_is_scoped_like_the_get_it_shares_a_handler_with() {
    // axum's `get()` also answers HEAD. If the gate looked HEAD up as itself it
    // would find nothing, and HEAD would become an unguarded read oracle over
    // every readable route — the same hole `auth_gate_adversarial` pins for the
    // forced-reset gate.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "THEIRS").await;
    let (_, outsider) = user(&app, &admin, "outsider", "member").await;

    let reply = app
        .send(request(
            Method::HEAD,
            "/api/v1/projects/THEIRS",
            Some(&outsider),
            None,
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "HEAD reached a project the caller cannot see"
    );

    // ...and it still works for someone who may read it, so the fix is not
    // "refuse every HEAD".
    let reply = app
        .send(request(
            Method::HEAD,
            "/api/v1/projects/THEIRS",
            Some(&admin),
            None,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn a_method_that_no_route_serves_is_405_and_not_500() {
    // The gate refuses anything it cannot classify, and a verb with no route is
    // the one case where "not in the table" is expected rather than a bug. It
    // must not be reported as an internal error — a 500 here would mean any bot
    // probing the API could fill the logs with incidents.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(get("/api/v1/comments/does-not-matter", Some(&admin)))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "GET on a PATCH/DELETE-only path: {}",
        reply.raw_body
    );

    let response = app
        .router()
        .oneshot(get("/api/v1/comments/x", Some(&admin)))
        .await
        .unwrap();
    let allow = response
        .headers()
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .expect("RFC 9110: a 405 MUST carry an Allow header");
    assert!(allow.contains("PATCH"), "{allow}");
    assert!(allow.contains("DELETE"), "{allow}");
    assert!(!allow.contains("GET"), "{allow}");
}

#[tokio::test]
async fn project_routes_still_require_a_session() {
    // The gate runs after `authenticate`, so it must not accidentally admit an
    // anonymous request while deciding it has no project role.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS").await;

    for uri in [
        "/api/v1/projects",
        "/api/v1/projects/ATLAS",
        "/api/v1/projects/ATLAS/members",
    ] {
        let reply = app.send(get(uri, None)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{uri} served an anonymous request: {}",
            reply.raw_body
        );
    }
}

// ---------------------------------------------------------------------------
// Migration 0005's backfill
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_backfill_leaves_an_existing_project_reachable_by_its_lead() {
    // Default deny plus an empty table would make every project on every existing
    // install unreachable the moment 0005 landed. This builds the pre-0005 world
    // — a project with a lead and no grants — and then runs the backfill over it.
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.unwrap();
    db::migrate::run(&db).await.unwrap();
    seed::ensure_default_admin(&db).await.unwrap();

    let app = App {
        db,
        config,
        _temp: temp,
    };
    let admin = admin_past_the_gate(&app).await;
    let (lead_id, lead) = user(&app, &admin, "lead", "member").await;
    let (other_id, other) = user(&app, &admin, "other", "member").await;
    project(&app, &lead, "LEGACY").await;

    // Rewind to before this migration: the grants are what 0005 introduced, so a
    // project that predates it has none.
    sqlx::query("DELETE FROM project_members")
        .execute(app.db.writer())
        .await
        .unwrap();

    // The lead is locked out — which is the disaster the backfill prevents.
    assert_eq!(
        app.send(get("/api/v1/projects/LEGACY", Some(&lead)))
            .await
            .status,
        StatusCode::OK,
        "the lead is an implicit owner even with no row, so this must still work"
    );

    // Re-run the backfill exactly as the migration does.
    sqlx::query(
        "INSERT OR IGNORE INTO project_members (project_id, user_id, role, added_at, added_by) \
         SELECT p.id, p.lead_id, 'owner', \
                strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now'), NULL \
           FROM projects p WHERE p.lead_id IS NOT NULL",
    )
    .execute(app.db.writer())
    .await
    .unwrap();

    // The lead now holds a real owner row, so the member list is honest about
    // who runs the project rather than being empty.
    let reply = app
        .send(get("/api/v1/projects/LEGACY/members", Some(&lead)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1, "{}", reply.raw_body);
    assert_eq!(members[0]["userId"], json!(lead_id));
    assert_eq!(members[0]["role"], "owner");
    assert_eq!(
        members[0]["addedBy"],
        Value::Null,
        "a backfilled grant was attributed to a person"
    );

    // The lead can administer it...
    assert_eq!(
        app.send(patch(
            "/api/v1/projects/LEGACY",
            Some(&lead),
            json!({ "name": "Still Mine" })
        ))
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(visible_projects(&app, &lead).await, ["LEGACY"]);

    // ...and the backfill did not hand the project to everybody on the way.
    assert_eq!(
        app.send(get("/api/v1/projects/LEGACY", Some(&other)))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(visible_projects(&app, &other).await, Vec::<String>::new());
    let _ = other_id;

    app.db.close().await;
}

#[tokio::test]
async fn the_backfill_gives_a_leadless_project_to_the_admins() {
    // The other half: a project with no lead has no natural owner, and the
    // alternative to this is a project whose member list is empty forever.
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.unwrap();
    db::migrate::run(&db).await.unwrap();
    seed::ensure_default_admin(&db).await.unwrap();
    let app = App {
        db,
        config,
        _temp: temp,
    };
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ORPHAN").await;

    // A project with no lead and no grants: the pre-0005 world again.
    sqlx::query("DELETE FROM project_members")
        .execute(app.db.writer())
        .await
        .unwrap();
    sqlx::query("UPDATE projects SET lead_id = NULL")
        .execute(app.db.writer())
        .await
        .unwrap();

    sqlx::query(
        "INSERT OR IGNORE INTO project_members (project_id, user_id, role, added_at, added_by) \
         SELECT p.id, u.id, 'owner', strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now'), NULL \
           FROM projects p CROSS JOIN users u \
          WHERE p.lead_id IS NULL AND u.role = 'admin' AND u.is_active = 1",
    )
    .execute(app.db.writer())
    .await
    .unwrap();

    let reply = app
        .send(get("/api/v1/projects/ORPHAN/members", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1, "the one seeded admin: {}", reply.raw_body);
    assert_eq!(members[0]["role"], "owner");

    app.db.close().await;
}

#[tokio::test]
async fn creating_a_project_makes_the_creator_its_owner() {
    // Default deny means there is no second chance: a project whose owner row
    // failed to land is a project its own creator cannot reach, and nothing but
    // an instance admin could repair it.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let reply = app
        .send(get("/api/v1/projects/ATLAS/members", Some(&founder)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1, "{}", reply.raw_body);
    assert_eq!(members[0]["userId"], json!(id));
    assert_eq!(members[0]["role"], "owner");
    assert_eq!(members[0]["effectiveRole"], "owner");

    // ...and they can immediately do owner things with it.
    assert_eq!(
        app.send(patch(
            "/api/v1/projects/ATLAS",
            Some(&founder),
            json!({ "name": "Mine" })
        ))
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(visible_projects(&app, &founder).await, ["ATLAS"]);
}

#[tokio::test]
async fn creating_a_project_for_someone_else_makes_them_an_owner_too() {
    // `leadId` names somebody else: they are an implicit owner via
    // `projects.lead_id` regardless, and the explicit row is what makes the
    // member list say so.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (lead_id, lead) = user(&app, &admin, "lead", "member").await;
    let (creator_id, creator) = user(&app, &admin, "creator", "member").await;

    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(&creator),
            json!({ "key": "ATLAS", "name": "Atlas", "template": "blank", "leadId": lead_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(get("/api/v1/projects/ATLAS/members", Some(&creator)))
        .await;
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(
        members.len(),
        2,
        "both the lead and the creator: {}",
        reply.raw_body
    );
    for member in members {
        assert_eq!(member["role"], "owner", "{member}");
    }

    for cookie in [&lead, &creator] {
        assert_eq!(
            app.send(get("/api/v1/projects/ATLAS", Some(cookie)))
                .await
                .status,
            StatusCode::OK
        );
    }
    let _ = creator_id;
}

// ---------------------------------------------------------------------------
// Attack: the instance role as a ceiling, on every write route there is
// ---------------------------------------------------------------------------

/// How many people the project's member list holds who can actually own it.
///
/// `domain::member::owner_count`'s predicate, asked of the database directly, so
/// that a test can assert the *state* the last-owner guard exists to protect
/// rather than only the status code of the request that tried to break it. A
/// guard that returns 409 while the row moves anyway would pass every test that
/// only reads the response.
async fn effective_owner_count(app: &App, project_key: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members m \
           JOIN users u ON u.id = m.user_id \
           JOIN projects p ON p.id = m.project_id \
          WHERE p.key = ? AND m.role = 'owner' AND u.is_active = 1 AND u.role != 'viewer'",
    )
    .bind(project_key)
    .fetch_one(app.db.reader())
    .await
    .expect("failed to count owners")
}

#[tokio::test]
async fn an_instance_viewer_cannot_write_through_any_project_scoped_route() {
    // The ceiling, proved the way `every_project_scoped_route_refuses_an_outsider`
    // proves default-deny: against **every** route the table actually classifies,
    // not the handful somebody remembered.
    //
    // `an_instance_viewer_who_is_a_project_owner_still_cannot_write` above probes
    // nine routes by hand. That list is readable and worth keeping, but it cannot
    // fail for a route added after it was written — and "the ceiling holds" is a
    // claim about every write route or it is not worth making. The attacker here
    // holds an `owner` row on the project, which is the strongest grant Atlas can
    // express: if the ceiling leaks anywhere, it leaks here.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let p = project(&app, &admin, "ATLAS").await;
    let card_key = card(&app, &admin, &p, "Card").await;
    let comment_id = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/comments"),
            Some(&admin),
            json!({ "body": "hello" }),
        ))
        .await
        .id();
    let tag_id = app
        .send(post(
            "/api/v1/projects/ATLAS/tags",
            Some(&admin),
            json!({ "name": "tag" }),
        ))
        .await
        .id();

    // The attacker: a read-only *instance* account holding an `owner` row. The
    // grant is real and the row says `owner`; the ceiling is the only thing
    // between them and every write below.
    let (viewer_id, viewer) = user(&app, &admin, "readonly", "viewer").await;
    grant(&app, &admin, "ATLAS", &viewer_id, "owner").await;

    // A second member, so `/members/{userId}` names somebody the attacker would
    // be editing rather than resolving to nothing for an unrelated reason.
    let (victim_id, _) = user(&app, &admin, "victim", "member").await;
    grant(&app, &admin, "ATLAS", &victim_id, "member").await;

    let (workflow_id, transition_id) = furnish_workflow(&app, &admin, &p.key, &p.status_id).await;
    let board_id = furnish_board(&app, &admin, &p.key).await;

    let targets = Targets {
        project_key: p.key.clone(),
        card_key: card_key.clone(),
        comment_id,
        tag_id: tag_id.clone(),
        type_id: p.type_id.clone(),
        level_id: p.level_id.clone(),
        status_id: p.status_id.clone(),
        priority_id: p.priority_id.clone(),
        resolution_id: p.resolution_id.clone(),
        member_user_id: victim_id.clone(),
        workflow_id,
        transition_id,
        board_id,
    };

    let mut probed = 0;
    let mut leaks: Vec<String> = Vec::new();

    // Driven from the real scope table rather than a list in this file. A write
    // route added in Phase 8 is attacked here the day it lands.
    for (method, template, min_role) in project_access::scoped_routes() {
        // Reads are exactly what an instance Viewer is *for*. The ceiling narrows
        // to `viewer`, so every Viewer-level route must keep working — a test that
        // demanded 403 everywhere would be pinning a broken product.
        if min_role == atlas::domain::member::ProjectRole::Viewer {
            continue;
        }

        let uri = targets
            .concrete(template)
            .unwrap_or_else(|why| panic!("cannot probe {method} {template}: {why}"));

        probed += 1;
        let reply = app
            .send(request(
                method.clone(),
                &uri,
                Some(&viewer),
                // The same body every other derived probe uses: enough to satisfy
                // whichever write this is, so a route that is genuinely open
                // answers 2xx rather than 422. A validation error here would be a
                // pass for entirely the wrong reason and would hide the hole.
                Some(every_write_body(&victim_id, &targets)),
            ))
            .await;

        // 403 and not 404: the ceiling is not concealment. They can see this
        // project — it is in their list and they can read every route on it — so
        // "it does not exist" would be a lie they could disprove in one request.
        if reply.status != StatusCode::FORBIDDEN {
            leaks.push(format!(
                "{method} {template}\n      probed as: {uri}\n      answered {} (expected 403)\n\
                 \x20     {}",
                reply.status,
                reply.raw_body.chars().take(200).collect::<String>()
            ));
        }
    }

    assert!(
        probed > 25,
        "only {probed} write routes probed — `scoped_routes()` is not being read correctly, so \
         this test would pass without attacking anything"
    );
    assert!(
        leaks.is_empty(),
        "{leaks_len} of {probed} write route(s) did not refuse an instance Viewer holding an \
         `owner` row:\n\n  - {}\n\nThe instance role is a **ceiling, not a floor**: \"this account \
         is read-only\" is a statement about the account, and no project owner may overrule it by \
         handing out a grant.",
        leaks.join("\n  - "),
        leaks_len = leaks.len()
    );

    // The mirror image, so the above cannot pass by refusing everything: the
    // reads their ceiling *does* permit still work.
    for uri in [
        "/api/v1/projects/ATLAS",
        "/api/v1/projects/ATLAS/cards",
        "/api/v1/projects/ATLAS/members",
        "/api/v1/projects/ATLAS/statuses",
    ] {
        let reply = app.send(get(uri, Some(&viewer))).await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "the ceiling took away a read as well: {uri}: {}",
            reply.raw_body
        );
    }
}

// ---------------------------------------------------------------------------
// Attack: lockout through the *user* routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deactivating_a_projects_only_owner_is_refused() {
    // `members::guard_last_owner` refuses to leave a project's member list with
    // nobody in it who can manage it. `POST /users/{id}/deactivate` reaches the
    // very same state from the other side — a deactivated account cannot make a
    // request, so its `owner` row grants nothing to anybody — and it never asks.
    //
    // This is the same argument as the last-active-admin guard one level down,
    // and it composes with it: refusing to demote the last admin is worth nothing
    // if the last *owner* can be swept away by a different route.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);

    let reply = app
        .send(post(
            &format!("/api/v1/users/{founder_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "the only owner of a project was deactivated, leaving it with none: {}",
        reply.raw_body
    );

    // The refusal is real, not cosmetic.
    assert_eq!(
        effective_owner_count(&app, "ATLAS").await,
        1,
        "ATLAS was left with no owner in its member list"
    );
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS", Some(&founder)))
            .await
            .status,
        StatusCode::OK,
        "the owner was deactivated anyway"
    );
}

#[tokio::test]
async fn demoting_a_projects_only_owner_to_instance_viewer_is_refused() {
    // The other half of the same hole, and the subtler one: nothing about
    // `PATCH /users/{id} {"role":"viewer"}` mentions projects at all. But the
    // instance role is a ceiling, so it silently turns every `owner` row that
    // account holds into a `viewer` — and a project whose only owner is now a
    // read-only account has no owner.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/users/{founder_id}"),
            Some(&admin),
            json!({ "role": "viewer" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "the only owner of a project was made a read-only account, leaving it with no owner: {}",
        reply.raw_body
    );
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);

    // ...and the third door into the same room: PATCH can deactivate too.
    let reply = app
        .send(patch(
            &format!("/api/v1/users/{founder_id}"),
            Some(&admin),
            json!({ "isActive": false }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "PATCH /users/{{id}} deactivated a project's only owner where POST .../deactivate would \
         not have: {}",
        reply.raw_body
    );
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);

    // The founder is untouched and can still administer the project.
    assert_eq!(
        app.send(patch(
            "/api/v1/projects/ATLAS",
            Some(&founder),
            json!({ "name": "Still Mine" })
        ))
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_owner_of_a_project_someone_else_also_owns_can_be_deactivated() {
    // The other direction, so the guard above cannot be "never deactivate anyone
    // who owns anything" — which would make a departing colleague unremovable and
    // is a worse product than the bug.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (leaver_id, leaver) = user(&app, &admin, "leaver", "member").await;
    project(&app, &leaver, "ATLAS").await;

    let (stayer_id, _) = user(&app, &admin, "stayer", "member").await;
    grant(&app, &admin, "ATLAS", &stayer_id, "owner").await;
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 2);

    let reply = app
        .send(post(
            &format!("/api/v1/users/{leaver_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "an owner could not be deactivated even though the project has another: {}",
        reply.raw_body
    );
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);
}

#[tokio::test]
async fn a_deactivated_account_can_still_be_edited_and_reactivated() {
    // The guard must fire on the *transition* out of ownership, not on the state.
    // An already-deactivated sole owner is already not an owner, so editing them
    // takes nothing further away — and refusing would make the account
    // unrepairable, which is the lockout this is all supposed to prevent.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (id, _cookie) = user(&app, &admin, "dormant", "member").await;

    let reply = app
        .send(post(
            &format!("/api/v1/users/{id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Now give them a project nobody else owns, from the admin's hands.
    project(&app, &admin, "ATLAS").await;
    grant(&app, &admin, "ATLAS", &id, "owner").await;

    for body in [
        json!({ "displayName": "Dormant" }),
        json!({ "role": "viewer" }),
    ] {
        let reply = app
            .send(patch(&format!("/api/v1/users/{id}"), Some(&admin), body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "an already-deactivated account could not be edited: {}",
            reply.raw_body
        );
    }

    // And back on, which must never be refused.
    let reply = app
        .send(patch(
            &format!("/api/v1/users/{id}"),
            Some(&admin),
            json!({ "isActive": true, "role": "member" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
}

#[tokio::test]
async fn no_sequence_of_member_and_lead_edits_leaves_a_project_ownerless() {
    // The migration's own note says the last-owner guard is load-bearing because
    // `PATCH /projects/{key}` can clear `lead_id`. This walks that claim: every
    // way the member list and the lead can be rearranged, checked against the one
    // invariant that matters after each step.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;
    let (second_id, _) = user(&app, &admin, "second", "member").await;
    grant(&app, &admin, "ATLAS", &second_id, "owner").await;

    // Drop the lead entirely: the founder keeps their row, so nothing is lost.
    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&founder),
            json!({ "leadId": null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(effective_owner_count(&app, "ATLAS").await >= 1);

    // Now strip the member list down to one owner...
    let reply = app
        .send(delete(
            &format!("/api/v1/projects/ATLAS/members/{founder_id}"),
            Some(&founder),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);

    // ...and the last one is protected, from both directions, with no lead left
    // to fall back on.
    for reply in [
        app.send(delete(
            &format!("/api/v1/projects/ATLAS/members/{second_id}"),
            Some(&admin),
        ))
        .await,
        app.send(patch(
            &format!("/api/v1/projects/ATLAS/members/{second_id}"),
            Some(&admin),
            json!({ "role": "viewer" }),
        ))
        .await,
    ] {
        assert_eq!(
            reply.status,
            StatusCode::CONFLICT,
            "a leadless project lost its last owner: {}",
            reply.raw_body
        );
    }
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 1);
}

// ---------------------------------------------------------------------------
// Attack: the race the last-owner guard would lose if it read outside the txn
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_owners_removing_each_other_at_once_cannot_leave_zero() {
    // The classic time-of-check/time-of-use on a "last one out" guard: both
    // requests count two owners, both conclude they are not the last, both delete.
    //
    // Atlas is safe here by construction rather than by luck, and it is worth
    // pinning *why*: `guard_last_owner` runs against `&mut *tx` — the same
    // transaction as the DELETE — and `Db::begin_write` is `BEGIN IMMEDIATE` on a
    // writer pool of exactly one connection. So the count and the delete cannot be
    // separated by another writer, and the loser's count is taken *after* the
    // winner commits. Move that count onto `db.reader()` and this test fails.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (a_id, a) = user(&app, &admin, "alfa", "member").await;
    let (b_id, b) = user(&app, &admin, "bravo", "member").await;

    project(&app, &a, "ATLAS").await;
    grant(&app, &a, "ATLAS", &b_id, "owner").await;
    // Neither of them leads it, so no implicit ownership props the project up and
    // the member list is the whole story.
    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&a),
            json!({ "leadId": null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 2);

    // Each owner tries to remove the other, at the same time.
    let (removed_b, removed_a) = tokio::join!(
        app.send(delete(
            &format!("/api/v1/projects/ATLAS/members/{b_id}"),
            Some(&a),
        )),
        app.send(delete(
            &format!("/api/v1/projects/ATLAS/members/{a_id}"),
            Some(&b),
        )),
    );

    assert_eq!(
        effective_owner_count(&app, "ATLAS").await,
        1,
        "both removals landed and ATLAS has no owner left: the last-owner count read a snapshot \
         from outside the transaction that deleted the row"
    );

    let statuses = [removed_b.status, removed_a.status];
    assert!(
        statuses.contains(&StatusCode::NO_CONTENT),
        "neither removal succeeded, so the two serialised into a deadlock rather than a queue: \
         {statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::CONFLICT),
        "one of the two removals should have found itself last and been refused: {statuses:?}\n{}\n{}",
        removed_b.raw_body,
        removed_a.raw_body
    );
}

// ---------------------------------------------------------------------------
// Attack: privilege escalation through the member routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_project_member_cannot_promote_themselves_or_anybody_else() {
    // The first thing an attacker with a foothold tries. Every member-management
    // route is `Scope::Project(Owner)`, so the layer refuses before the handler
    // runs — but "a member cannot promote themselves" is the claim, so state it.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "ATLAS").await;

    let (climber_id, climber) = user(&app, &admin, "climber", "member").await;
    grant(&app, &admin, "ATLAS", &climber_id, "member").await;
    let (friend_id, _) = user(&app, &admin, "friend", "member").await;

    let attempts: Vec<(&str, Request<Body>)> = vec![
        (
            "PATCH their own grant to owner",
            patch(
                &format!("/api/v1/projects/ATLAS/members/{climber_id}"),
                Some(&climber),
                json!({ "role": "owner" }),
            ),
        ),
        (
            "POST themselves a second, higher grant",
            post(
                "/api/v1/projects/ATLAS/members",
                Some(&climber),
                json!({ "userId": climber_id, "role": "owner" }),
            ),
        ),
        (
            "add an accomplice as owner",
            post(
                "/api/v1/projects/ATLAS/members",
                Some(&climber),
                json!({ "userId": friend_id, "role": "owner" }),
            ),
        ),
        (
            "make themselves the lead, which is implicit ownership",
            patch(
                "/api/v1/projects/ATLAS",
                Some(&climber),
                json!({ "leadId": climber_id }),
            ),
        ),
    ];

    for (what, request) in attempts {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a project member could {what}: {}",
            reply.raw_body
        );
    }

    // Nothing moved.
    let role: String = sqlx::query_scalar(
        "SELECT role FROM project_members WHERE user_id = ? AND project_id = \
         (SELECT id FROM projects WHERE key = 'ATLAS')",
    )
    .bind(&climber_id)
    .fetch_one(app.db.reader())
    .await
    .unwrap();
    assert_eq!(role, "member", "a member promoted themselves");
}

#[tokio::test]
async fn a_project_owner_cannot_touch_instance_roles() {
    // Owning a project is the top of the *project* ladder. It must not be a step
    // onto the instance one — otherwise "owner of a throwaway project I created
    // myself" is a path to admin, and any Member can create a project.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (boss_id, boss) = user(&app, &admin, "boss", "member").await;
    project(&app, &boss, "ATLAS").await;

    for (what, request) in [
        (
            "promote themselves to instance admin",
            patch(
                &format!("/api/v1/users/{boss_id}"),
                Some(&boss),
                json!({ "role": "admin" }),
            ),
        ),
        (
            "mint a fresh instance admin",
            post(
                "/api/v1/users",
                Some(&boss),
                json!({ "username": "puppet", "password": GOOD_PASSWORD, "role": "admin" }),
            ),
        ),
        ("read the user list", get("/api/v1/users", Some(&boss))),
    ] {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a project owner could {what}: {}",
            reply.raw_body
        );
    }

    // ...and the role really did not move.
    let reply = app.send(get("/api/v1/auth/me", Some(&boss))).await;
    assert_eq!(reply.json()["role"], "member", "{}", reply.raw_body);
}

#[tokio::test]
async fn naming_a_project_lead_grants_ownership_that_the_member_list_admits_to() {
    // `projects.lead_id` is implicit ownership by rule 2 of `member::resolve`:
    // naming a lead hands that person owner on the project, with no grant, no
    // `addedBy`, and — since `member::list` lists *rows* — no line in the one
    // place an owner audits access from.
    //
    // `create_project` already refuses to leave it there: it writes the lead an
    // explicit row precisely so "the member list is honest about who runs the
    // project rather than an empty list with a footnote". `PATCH /projects/{key}`
    // takes the same action and must reach the same state.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (founder_id, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let (silent_id, silent) = user(&app, &admin, "silent", "member").await;
    assert_eq!(
        app.send(get("/api/v1/projects/ATLAS", Some(&silent)))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "the fixture is wrong: `silent` already has access"
    );

    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&founder),
            json!({ "leadId": silent_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // They really are an owner now — this is not a hypothetical.
    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&silent),
            json!({ "name": "Mine Now" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "naming a lead did not actually grant ownership, so this test is checking the wrong \
         thing: {}",
        reply.raw_body
    );

    // So the member list has to say so.
    let reply = app
        .send(get("/api/v1/projects/ATLAS/members", Some(&founder)))
        .await;
    let members = reply.json();
    let row = members
        .as_array()
        .expect("an array")
        .iter()
        .find(|m| m["userId"] == json!(silent_id))
        .cloned();
    let row = row.unwrap_or_else(|| {
        panic!(
            "`silent` owns ATLAS but does not appear in its member list at all — an owner \
             auditing access would never see them: {}",
            reply.raw_body
        )
    });
    assert_eq!(
        row["effectiveRole"], "owner",
        "the member list understates what the lead can do: {row}"
    );

    let _ = founder_id;
}

#[tokio::test]
async fn naming_a_lead_who_does_not_exist_is_a_422_and_not_a_500() {
    // `projects.lead_id` REFERENCES users (id) with foreign keys ON, so an id that
    // names nobody is a `FOREIGN KEY constraint failed` — which reaches the caller
    // as a 500 they can do nothing with. `members::add_member` already makes this
    // exact argument about its own `userId` and answers 422; the two routes take
    // the same kind of input and must answer the same way.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (_, founder) = user(&app, &admin, "founder", "member").await;
    project(&app, &founder, "ATLAS").await;

    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&founder),
            json!({ "leadId": "no-such-user" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "PATCH with an unknown leadId: {}",
        reply.raw_body
    );

    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(&founder),
            json!({ "key": "NEW", "name": "New", "leadId": "no-such-user" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "POST with an unknown leadId: {}",
        reply.raw_body
    );
}

// ---------------------------------------------------------------------------
// Attack: migration 0005's backfill, replayed verbatim
// ---------------------------------------------------------------------------

/// Rewinds an already-migrated database to just before 0005 and replays it.
///
/// # Why this runs the migration's own text
///
/// The two backfill tests above paste a copy of the migration's `INSERT`s into
/// the test and run *that*. It is the trap `auth::project_access`'s module doc
/// warns about in another guise: the copy cannot fail when the original changes,
/// so the test proves the backfill in the test file works, which nobody was
/// worried about. `include_str!` reads the file the migrator embeds, so there is
/// exactly one backfill and this is it.
///
/// Dropping the table is a faithful rewind: `project_members` is the only thing
/// 0005 creates, so a database without it *is* a pre-0005 database.
async fn replay_migration_0005(db: &Db) {
    sqlx::query("DROP TABLE project_members")
        .execute(db.writer())
        .await
        .expect("failed to rewind past 0005");

    sqlx::raw_sql(include_str!("../migrations/0005_project_access.sql"))
        .execute(db.writer())
        .await
        .expect("migration 0005 failed to apply");
}

#[tokio::test]
async fn the_real_0005_backfill_leaves_every_pre_0005_project_with_an_owner() {
    // The migration's promise, stated as the invariant it is: **after 0005, every
    // project that already existed has at least one person in its member list who
    // can actually administer it.** Default deny plus an empty table would make
    // every project on every existing install unreachable the moment this landed,
    // and a backfill that only *mostly* holds is a support ticket per install.
    //
    // Four pre-0005 projects, one per shape of `projects.lead_id` that a real
    // database can be holding when the migrator runs.
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.unwrap();
    db::migrate::run(&db).await.unwrap();
    seed::ensure_default_admin(&db).await.unwrap();
    let app = App {
        db,
        config,
        _temp: temp,
    };
    let admin = admin_past_the_gate(&app).await;

    let (lead_id, lead) = user(&app, &admin, "lead", "member").await;
    let (dormant_id, _) = user(&app, &admin, "dormant", "member").await;
    let (readonly_id, _) = user(&app, &admin, "readonly", "viewer").await;

    for key in ["LEGACY", "ORPHAN", "DORMANT", "READONLY"] {
        project(&app, &admin, key).await;
    }

    // Rewind to the pre-0005 world: `projects.lead_id` is the only record of who
    // ran a project, and there are no grants because grants are what 0005 adds.
    let set_lead = |key: &'static str, lead: Option<String>| {
        let db = app.db.clone();
        async move {
            sqlx::query("UPDATE projects SET lead_id = ? WHERE key = ?")
                .bind(lead)
                .bind(key)
                .execute(db.writer())
                .await
                .unwrap();
        }
    };
    set_lead("LEGACY", Some(lead_id.clone())).await;
    // The case the brief singles out, and the one a long-lived install collects:
    // a project whose lead was never set, or was cleared when somebody left.
    set_lead("ORPHAN", None).await;
    // A lead who has since been deactivated. `users` are never hard-deleted, so
    // `lead_id` keeps pointing at them — and they cannot make a request at all.
    set_lead("DORMANT", Some(dormant_id.clone())).await;
    // A lead who is a read-only instance account. The ceiling caps their `owner`
    // row to `viewer`, so the row grants nothing.
    set_lead("READONLY", Some(readonly_id.clone())).await;

    let reply = app
        .send(post(
            &format!("/api/v1/users/{dormant_id}/deactivate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    replay_migration_0005(&app.db).await;

    // The invariant, on every one of them.
    for key in ["LEGACY", "ORPHAN", "DORMANT", "READONLY"] {
        assert!(
            effective_owner_count(&app, key).await >= 1,
            "after the 0005 backfill, {key} has nobody in its member list who can administer it. \
             Its lead cannot own it (deactivated, or a read-only account), and the backfill's \
             second statement only fires for a project whose lead_id IS NULL — so the row it \
             wrote grants nothing and no admin was given one either. The project is reachable \
             only by an instance admin's implicit ownership, which is the outcome the last-owner \
             guard exists to avoid."
        );
    }

    // The specific promises, so the count above cannot be satisfied by the wrong
    // person. The lead of LEGACY was already its implicit owner; the backfill just
    // writes it down.
    let reply = app
        .send(get("/api/v1/projects/LEGACY/members", Some(&lead)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1, "only the lead: {}", reply.raw_body);
    assert_eq!(members[0]["userId"], json!(lead_id));
    assert_eq!(members[0]["role"], "owner");
    assert_eq!(
        members[0]["addedBy"],
        Value::Null,
        "a backfilled grant was attributed to a person, inventing an audit trail"
    );
    assert_eq!(
        app.send(patch(
            "/api/v1/projects/LEGACY",
            Some(&lead),
            json!({ "name": "Still Mine" })
        ))
        .await
        .status,
        StatusCode::OK,
        "the lead cannot administer their own project after the backfill"
    );

    // ...and it did not hand every project to everybody on the way past.
    let (_, stranger) = user(&app, &admin, "stranger", "member").await;
    assert_eq!(
        visible_projects(&app, &stranger).await,
        Vec::<String>::new()
    );

    // A leadless project has no natural owner, so the admins get it — otherwise
    // its member list is empty forever.
    let reply = app
        .send(get("/api/v1/projects/ORPHAN/members", Some(&admin)))
        .await;
    let members = reply.json();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 1, "the one seeded admin: {}", reply.raw_body);
    assert_eq!(members[0]["role"], "owner");

    app.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_owners_deactivated_at_once_cannot_leave_a_project_ownerless() {
    // The same race as `two_owners_removing_each_other_at_once_cannot_leave_zero`,
    // aimed at `users::guard_sole_project_ownership` — the guard on the routes
    // that orphan a project without ever naming one.
    //
    // It matters more here than it does there, because the obvious way to write
    // that guard is the wrong one: it needs a count of owners, `Db::reader` is
    // right there, and reading through it would look fine in every test that
    // sends one request at a time. Two admins retiring two colleagues in the same
    // instant would then each see the other still owning ATLAS, and both would be
    // waved through. It reads through the write transaction instead, so the loser
    // counts *after* the winner commits.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let (a_id, a) = user(&app, &admin, "alfa", "member").await;
    let (b_id, _) = user(&app, &admin, "bravo", "member").await;

    project(&app, &a, "ATLAS").await;
    grant(&app, &a, "ATLAS", &b_id, "owner").await;
    let reply = app
        .send(patch(
            "/api/v1/projects/ATLAS",
            Some(&a),
            json!({ "leadId": null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(effective_owner_count(&app, "ATLAS").await, 2);

    // Both owners retire at once.
    let (gone_a, gone_b) = tokio::join!(
        app.send(post(
            &format!("/api/v1/users/{a_id}/deactivate"),
            Some(&admin),
            json!({}),
        )),
        app.send(post(
            &format!("/api/v1/users/{b_id}/deactivate"),
            Some(&admin),
            json!({}),
        )),
    );

    assert_eq!(
        effective_owner_count(&app, "ATLAS").await,
        1,
        "both accounts were deactivated and ATLAS has no owner left: the sole-ownership guard \
         counted owners from outside the transaction that deactivated the account"
    );

    let statuses = [gone_a.status, gone_b.status];
    assert!(
        statuses.contains(&StatusCode::OK),
        "neither deactivation succeeded: {statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::CONFLICT),
        "one of the two was the last owner by the time it ran and should have been refused: \
         {statuses:?}\n{}\n{}",
        gone_a.raw_body,
        gone_b.raw_body
    );
}

//! End-to-end AQL tests, over the real router and a real database.
//!
//! These drive HTTP through `tower::ServiceExt::oneshot`, the pattern
//! `tests/domain.rs` established: no ports, every middleware layer still runs.
//! They prove the claims that would be *design* failures if false —
//!
//! - each operator compiles and runs;
//! - each grammar-rule rejection fires (`IS` with a non-null, `=` on summary, `>`
//!   on a text field, `WAS` on a non-history field);
//! - `currentUser()` / `now()` / `startOfWeek(-1w)` resolve;
//! - `ORDER BY` sorts;
//! - **the accessible-projects scoping** — an outsider's AQL cannot see another
//!   project's cards (the big one);
//! - filter composition works and a reference cycle is refused, not looped;
//! - a parse error's span points at the right column.

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
        seed::ensure_default_admin(&db).await.expect("seed admin");
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
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
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
            .unwrap_or_else(|| panic!("no id in {}", self.raw_body))
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
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
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

/// Creates a Member user who can log in immediately, and returns their session.
async fn member(app: &App, admin: &str, username: &str) -> (String, String) {
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(admin),
            json!({
                "username": username,
                "displayName": username,
                "password": GOOD_PASSWORD,
                "role": "member",
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let user_id = reply.id();

    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    (user_id, reply.session_cookie().expect("member cookie"))
}

/// A project's config: the ids a test needs to create cards.
struct Fixture {
    key: String,
    type_id: String,
    statuses: Vec<(String, String)>,
    priorities: Vec<(String, String)>,
}

impl Fixture {
    fn status(&self, name: &str) -> &str {
        &self
            .statuses
            .iter()
            .find(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("no status {name}"))
            .0
    }
    fn priority(&self, name: &str) -> &str {
        &self
            .priorities
            .iter()
            .find(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("no priority {name}"))
            .0
    }
}

fn rows(reply: &Reply) -> Vec<Value> {
    reply
        .json()
        .as_array()
        .unwrap_or_else(|| panic!("expected array: {}", reply.raw_body))
        .clone()
}

async fn project(app: &App, admin: &str, key: &str) -> Fixture {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": "programming" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let cfg = |p: &str| format!("/api/v1/projects/{key}/{p}");
    let types = rows(&app.send(get(&cfg("card-types"), Some(admin))).await);
    // The lowest-level type is the default for a plain card.
    let type_id = types
        .iter()
        .min_by_key(|t| t["level"].as_i64().unwrap_or(0))
        .map(|t| t["id"].as_str().expect("type id").to_owned())
        .expect("a card type");

    let statuses = rows(&app.send(get(&cfg("statuses"), Some(admin))).await)
        .into_iter()
        .map(|v| {
            (
                v["id"].as_str().expect("config id").to_owned(),
                v["name"].as_str().expect("config name").to_owned(),
            )
        })
        .collect();
    let priorities = rows(&app.send(get(&cfg("priorities"), Some(admin))).await)
        .into_iter()
        .map(|v| {
            (
                v["id"].as_str().expect("config id").to_owned(),
                v["name"].as_str().expect("config name").to_owned(),
            )
        })
        .collect();

    Fixture {
        key: key.to_owned(),
        type_id,
        statuses,
        priorities,
    }
}

async fn card(app: &App, cookie: &str, fx: &Fixture, body: Value) -> String {
    let mut object = json!({ "typeId": fx.type_id });
    let map = object.as_object_mut().expect("object");
    for (k, v) in body.as_object().expect("body object") {
        map.insert(k.clone(), v.clone());
    }
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/cards", fx.key),
            Some(cookie),
            object,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("card key").to_owned()
}

/// Runs a search and returns the card keys it produced, in order.
async fn search_keys(app: &App, cookie: &str, aql: &str) -> Vec<String> {
    let reply = app
        .send(post("/api/v1/search", Some(cookie), json!({ "aql": aql })))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "AQL {aql:?}: {}",
        reply.raw_body
    );
    reply.json()["cards"]
        .as_array()
        .expect("cards array")
        .iter()
        .map(|c| c["key"].as_str().expect("card key").to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

// One flow exercises every operator against a shared fixture; splitting it would
// fragment a single coherent scenario and re-seed the project six times over.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn each_operator_runs_and_filters_as_expected() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let todo = fx.status("To Do").to_owned();
    let done = fx.status("Done").to_owned();
    let high = fx.priority("High").to_owned();

    let a = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "fix the login bug", "statusId": todo, "priorityId": high }),
    )
    .await;
    let b = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "ship the release", "statusId": done }),
    )
    .await;

    // Equality and its negation.
    assert_eq!(
        search_keys(&app, &admin, "status = \"To Do\"").await,
        vec![a.clone()]
    );
    assert_eq!(
        search_keys(&app, &admin, "status != \"To Do\"").await,
        vec![b.clone()]
    );

    // IN / NOT IN.
    assert_eq!(
        search_keys(&app, &admin, "status IN (\"To Do\", Done)")
            .await
            .len(),
        2
    );
    assert_eq!(
        search_keys(&app, &admin, "status NOT IN (Done)").await,
        vec![a.clone()]
    );

    // Full text.
    assert_eq!(
        search_keys(&app, &admin, "summary ~ login").await,
        vec![a.clone()]
    );
    assert_eq!(
        search_keys(&app, &admin, "text ~ release").await,
        vec![b.clone()]
    );
    assert_eq!(
        search_keys(&app, &admin, "summary !~ login").await,
        vec![b.clone()]
    );

    // Emptiness: a To Do card has no resolution; a Done card does.
    assert_eq!(
        search_keys(&app, &admin, "resolution IS EMPTY").await,
        vec![a.clone()]
    );
    assert_eq!(
        search_keys(&app, &admin, "resolution IS NOT EMPTY").await,
        vec![b.clone()]
    );

    // Priority ordering (High is more urgent than the seeded Medium/Low).
    assert!(
        search_keys(&app, &admin, "priority >= High")
            .await
            .contains(&a)
    );

    // currentUser(): the admin created both, and is the reporter.
    assert_eq!(
        search_keys(&app, &admin, "reporter = currentUser()")
            .await
            .len(),
        2
    );

    // now(): both were just created, so both are before now.
    assert_eq!(search_keys(&app, &admin, "created < now()").await.len(), 2);

    // startOfWeek(-1w): both created this week, so after last week's start.
    assert_eq!(
        search_keys(&app, &admin, "created >= startOfWeek(-1w)")
            .await
            .len(),
        2
    );

    // key.
    assert_eq!(
        search_keys(&app, &admin, &format!("key = {a}")).await,
        vec![a.clone()]
    );

    // An empty query returns everything the caller can see.
    assert_eq!(search_keys(&app, &admin, "").await.len(), 2);

    // WAS / CHANGED read the changelog, which creation does not write — only a
    // change does. Editing a plain field (priority, not a workflow-gated status)
    // produces a history row, and then the history operators see it.
    let low = fx.priority("Low").to_owned();
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{a}"),
            Some(&admin),
            json!({ "priorityId": low }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(
        search_keys(&app, &admin, "priority WAS High")
            .await
            .contains(&a)
    );
    assert!(
        search_keys(&app, &admin, "priority CHANGED")
            .await
            .contains(&a)
    );
    assert!(
        search_keys(&app, &admin, "priority CHANGED FROM High TO Low")
            .await
            .contains(&a)
    );
}

// ---------------------------------------------------------------------------
// Grammar-rule rejections
// ---------------------------------------------------------------------------

/// Asserts an AQL query is rejected with a 400 whose detail mentions `needle`.
async fn reject(app: &App, cookie: &str, aql: &str, needle: &str) {
    let reply = app
        .send(post("/api/v1/search", Some(cookie), json!({ "aql": aql })))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "{aql:?}: {}",
        reply.raw_body
    );
    let detail = reply.json()["detail"].as_str().unwrap_or("").to_lowercase();
    assert!(
        detail.contains(needle),
        "{aql:?} → {detail:?} (wanted {needle:?})"
    );
}

#[tokio::test]
async fn the_grammar_rules_are_enforced_with_helpful_errors() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let _fx = project(&app, &admin, "ATLAS").await;

    // IS with a non-null value.
    reject(&app, &admin, "resolution IS Done", "empty").await;
    // = on a text field.
    reject(&app, &admin, "summary = hello", "text").await;
    // > on a text field.
    reject(&app, &admin, "summary > hello", "orderable").await;
    // WAS on a non-history field.
    reject(&app, &admin, "summary WAS hello", "history").await;
    // An unknown field.
    reject(&app, &admin, "bogus = 1", "unknown field").await;
}

// ---------------------------------------------------------------------------
// ORDER BY
// ---------------------------------------------------------------------------

#[tokio::test]
async fn order_by_sorts_the_results() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let high = fx.priority("High").to_owned();
    let low = fx.priority("Low").to_owned();

    let urgent = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "urgent", "priorityId": high }),
    )
    .await;
    let lazy = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "lazy", "priorityId": low }),
    )
    .await;

    // Priority ascending = most urgent first (rank order).
    let by_priority = search_keys(&app, &admin, "ORDER BY priority ASC").await;
    assert_eq!(by_priority, vec![urgent.clone(), lazy.clone()]);

    // ...and the reverse.
    let reversed = search_keys(&app, &admin, "ORDER BY priority DESC").await;
    assert_eq!(reversed, vec![lazy, urgent]);
}

// ---------------------------------------------------------------------------
// The big one: accessible-projects scoping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_outsiders_query_cannot_see_another_projects_cards() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let mine = project(&app, &admin, "MINE").await;
    let theirs = project(&app, &admin, "THEIRS").await;

    let my_card = card(&app, &admin, &mine, json!({ "summary": "my work" })).await;
    let their_card = card(&app, &admin, &theirs, json!({ "summary": "secret work" })).await;

    // Bob is a member of MINE only.
    let (bob_id, bob) = member(&app, &admin, "bob").await;
    let reply = app
        .send(post(
            "/api/v1/projects/MINE/members",
            Some(&admin),
            json!({ "userId": bob_id, "role": "member" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    // Bob's own empty query returns only MINE's card.
    let visible = search_keys(&app, &bob, "").await;
    assert_eq!(
        visible,
        vec![my_card.clone()],
        "bob saw across the boundary"
    );

    // Even an explicit attempt to reach the other project returns nothing —
    // the query cannot scope itself out of the access predicate.
    let attempt = search_keys(&app, &bob, "project = THEIRS").await;
    assert!(attempt.is_empty(), "bob reached THEIRS: {attempt:?}");
    let by_key = search_keys(&app, &bob, &format!("key = {their_card}")).await;
    assert!(
        by_key.is_empty(),
        "bob reached a card by key across the boundary"
    );

    // The admin, who can see everything, still sees both.
    assert_eq!(search_keys(&app, &admin, "").await.len(), 2);
}

/// The top-level access test proves `project = THEIRS` and `key = <their card>`
/// are scoped. This proves the *subquery* clauses are too: a WAS/CHANGED history
/// EXISTS, a labels EXISTS, and a `linkedCards()` IN are each correlated to a
/// `cards` row, and the accessible-projects predicate must wrap all of them — not
/// only the top level. If any subquery were an unscoped join, an outsider could
/// read a card in a project they cannot see through it.
#[tokio::test]
async fn access_scoping_wraps_the_history_and_label_subqueries_too() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let mine = project(&app, &admin, "MINE").await;
    let theirs = project(&app, &admin, "THEIRS").await;

    // A card in THEIRS with a label and a priority change (so it has a matching
    // history row), that an outsider must never reach through any subquery.
    let high = theirs.priority("High").to_owned();
    let low = theirs.priority("Low").to_owned();
    let their_card = card(
        &app,
        &admin,
        &theirs,
        json!({ "summary": "secret work", "priorityId": high }),
    )
    .await;

    let reply = app
        .send(post(
            "/api/v1/projects/THEIRS/tags",
            Some(&admin),
            json!({ "name": "confidential" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let tag_id = reply.id();
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{their_card}/tags"),
            Some(&admin),
            json!({ "tagId": tag_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{their_card}"),
            Some(&admin),
            json!({ "priorityId": low }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Bob is a member of MINE only, with a plain card that carries none of that.
    let (bob_id, bob) = member(&app, &admin, "bob").await;
    let reply = app
        .send(post(
            "/api/v1/projects/MINE/members",
            Some(&admin),
            json!({ "userId": bob_id, "role": "member" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let _my_card = card(&app, &admin, &mine, json!({ "summary": "my work" })).await;

    // The admin, who sees everything, reaches THEIRS through each subquery — so
    // the clauses genuinely match it, which is what makes bob's empty results
    // meaningful rather than vacuous.
    assert!(
        search_keys(&app, &admin, "labels = confidential")
            .await
            .contains(&their_card),
        "the label clause should match for the admin"
    );
    assert!(
        search_keys(&app, &admin, "priority WAS High")
            .await
            .contains(&their_card),
        "the history clause should match for the admin"
    );

    // Bob reaches nothing in THEIRS through the label subquery, the history
    // subquery, the CHANGED subquery, or a linkedCards() lookup. The access
    // predicate wraps every one of them.
    assert!(
        search_keys(&app, &bob, "labels = confidential")
            .await
            .is_empty(),
        "bob reached THEIRS through the labels subquery"
    );
    assert!(
        search_keys(&app, &bob, "priority WAS High")
            .await
            .is_empty(),
        "bob reached THEIRS through the WAS history subquery"
    );
    assert!(
        search_keys(&app, &bob, "priority CHANGED").await.is_empty(),
        "bob reached THEIRS through the CHANGED history subquery"
    );
    assert!(
        search_keys(&app, &bob, &format!("key IN linkedCards({their_card})"))
            .await
            .is_empty(),
        "bob reached THEIRS through linkedCards()"
    );
}

/// `membersOf("HIDDEN")` must not report, through a matching card the caller can
/// see, whether a visible user also belongs to a project the caller cannot. The
/// project named to `membersOf` is gated by the caller's own access.
#[tokio::test]
async fn members_of_does_not_leak_membership_of_an_invisible_project() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let mine = project(&app, &admin, "MINE").await;
    let _theirs = project(&app, &admin, "THEIRS").await;

    // Carol belongs to both projects; Bob only to MINE. A card in MINE, which Bob
    // can see, is assigned to Carol — so the only thing `membersOf("THEIRS")` can
    // reveal to Bob is whether Carol (visible) is a member of THEIRS (invisible).
    let (carol_id, _carol) = member(&app, &admin, "carol").await;
    let (bob_id, bob) = member(&app, &admin, "bob").await;
    for user_id in [&carol_id, &bob_id] {
        let reply = app
            .send(post(
                "/api/v1/projects/MINE/members",
                Some(&admin),
                json!({ "userId": user_id, "role": "member" }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    }
    let reply = app
        .send(post(
            "/api/v1/projects/THEIRS/members",
            Some(&admin),
            json!({ "userId": carol_id, "role": "member" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let my_card = card(
        &app,
        &admin,
        &mine,
        json!({ "summary": "assigned to carol", "assigneeId": carol_id }),
    )
    .await;

    // The admin sees THEIRS, so `membersOf("THEIRS")` resolves to its members and
    // the card matches — which is what makes Bob's empty result meaningful rather
    // than a query that never matches anything for anyone.
    assert!(
        search_keys(&app, &admin, "assignee IN membersOf(\"THEIRS\")")
            .await
            .contains(&my_card),
        "the admin, who can see THEIRS, should match carol's card"
    );

    // Bob cannot see THEIRS, so for him `membersOf("THEIRS")` is empty and the
    // card does not match — he learns nothing about carol's membership of it.
    assert!(
        search_keys(&app, &bob, "assignee IN membersOf(\"THEIRS\")")
            .await
            .is_empty(),
        "membersOf leaked carol's membership of a project bob cannot see"
    );

    // Sanity: Bob's own project still works — the guard scopes, it does not break.
    assert!(
        search_keys(&app, &bob, "assignee IN membersOf(\"MINE\")")
            .await
            .contains(&my_card),
        "membersOf should still resolve for a project bob can see"
    );
}

// ---------------------------------------------------------------------------
// Filter composition and the cycle guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_compose_and_a_cycle_is_refused() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let done = fx.status("Done").to_owned();
    let open = card(&app, &admin, &fx, json!({ "summary": "open" })).await;
    let _shut = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "shut", "statusId": done }),
    )
    .await;

    // A saved filter, then a query that references it.
    let reply = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "Not done", "aql": "resolution IS EMPTY" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let composed = search_keys(&app, &admin, "filter = \"Not done\"").await;
    assert_eq!(
        composed,
        vec![open.clone()],
        "composition did not apply the filter"
    );

    // Referencing a filter's results endpoint returns the same.
    let filter_id = reply.id();
    let reply = app
        .send(get(
            &format!("/api/v1/filters/{filter_id}/results"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["total"].as_i64(), Some(1));

    // A direct self-cycle: a filter whose body references itself. Saving it is
    // fine (the reference is unresolved text); running it must error, not loop.
    let reply = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "Loopy", "aql": "filter = \"Loopy\"" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(post(
            "/api/v1/search",
            Some(&admin),
            json!({ "aql": "filter = \"Loopy\"" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "cycle not caught: {}",
        reply.raw_body
    );
    assert!(
        reply.json()["detail"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("cycle"),
        "{}",
        reply.raw_body
    );
}

#[tokio::test]
async fn an_indirect_filter_cycle_is_also_refused() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let _fx = project(&app, &admin, "ATLAS").await;

    // A → B → A.
    let a = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "A", "aql": "filter = \"B\"" }),
        ))
        .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.raw_body);
    let b = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "B", "aql": "filter = \"A\"" }),
        ))
        .await;
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.raw_body);

    let reply = app
        .send(post(
            "/api/v1/search",
            Some(&admin),
            json!({ "aql": "filter = \"A\"" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.raw_body);
}

// ---------------------------------------------------------------------------
// Validation and error spans
// ---------------------------------------------------------------------------

#[tokio::test]
async fn validate_reports_ok_and_returns_spans_for_errors() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // A good query validates and echoes a normalised form.
    let reply = app
        .send(post(
            "/api/v1/search/validate",
            Some(&admin),
            json!({ "aql": "status = Done AND priority > High" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["ok"], json!(true));
    assert!(reply.json()["query"].is_string());
    assert!(!reply.json()["fields"].as_array().unwrap().is_empty());

    // A bad one comes back with the column of the offending token. The `=` in
    // `summary = x` is at column 9 (1-based).
    let reply = app
        .send(post(
            "/api/v1/search/validate",
            Some(&admin),
            json!({ "aql": "summary = x" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["ok"], json!(false));
    let error = &reply.json()["error"];
    assert_eq!(error["column"].as_i64(), Some(9), "{}", reply.raw_body);
    assert_eq!(error["start"].as_i64(), Some(8));
}

// ---------------------------------------------------------------------------
// Filter CRUD and ownership
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_are_personal_and_a_bad_body_is_refused_at_save() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let _fx = project(&app, &admin, "ATLAS").await;
    let (_bob_id, bob) = member(&app, &admin, "bob").await;

    // A filter with un-typecheckable AQL is refused before it is stored.
    let reply = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "Broken", "aql": "summary = hello" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.raw_body);

    // The admin saves a real one.
    let reply = app
        .send(post(
            "/api/v1/filters",
            Some(&admin),
            json!({ "name": "Mine", "aql": "status = Done" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let filter_id = reply.id();

    // Bob cannot see it: someone else's filter is a 404, not a 403.
    let reply = app
        .send(get(&format!("/api/v1/filters/{filter_id}"), Some(&bob)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    // Bob's own filter list is empty; the admin's has one.
    assert!(rows(&app.send(get("/api/v1/filters", Some(&bob))).await).is_empty());
    assert_eq!(
        rows(&app.send(get("/api/v1/filters", Some(&admin))).await).len(),
        1
    );

    // The admin can rename and then delete it.
    let reply = app
        .send(patch(
            &format!("/api/v1/filters/{filter_id}"),
            Some(&admin),
            json!({ "name": "Renamed" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["name"], json!("Renamed"));

    let reply = app
        .send(delete(
            &format!("/api/v1/filters/{filter_id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);
}

//! End-to-end domain tests, over the real router, the real middleware stack and
//! a real database.
//!
//! These drive HTTP through `tower::ServiceExt::oneshot` — no TCP, no ports, no
//! races, and every layer still runs. `tests/health.rs` established the pattern
//! and `tests/auth.rs` extended it; the `App` harness and `admin_past_the_gate`
//! below are lifted from the latter, as its handoff intended.
//!
//! # What these are for
//!
//! The unit tests beside each module prove the pieces. These prove the claims —
//! the ones in `TODO.md` and `docs/adr/0002` that would be *design* failures
//! rather than bugs if they were false:
//!
//! - two concurrent creates never produce one key twice;
//! - every field change lands in the changelog, with raw *and* display values;
//! - the tree cannot be made to contain a cycle, or to grow past the depth cap;
//! - a card in a done column always says why it stopped, and one that leaves
//!   stops saying it;
//! - a retired key still resolves;
//! - rank survives the database and sorts with a plain `ORDER BY`;
//! - a three-level hierarchy that is *not* Epic/Story/Sub-task works identically.

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
use std::collections::HashSet;
use tower::ServiceExt;

/// The seeded credentials every test starts from.
const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy.
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

// ---------------------------------------------------------------------------
// Harness — lifted from tests/auth.rs, as its handoff intended.
// ---------------------------------------------------------------------------

/// A migrated, seeded database and the router over it.
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
    location: Option<String>,
    set_cookie: Vec<String>,
    raw_body: String,
}

impl Reply {
    async fn from(response: axum::response::Response) -> Self {
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
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
            location,
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

    /// The body's `id`, for a created resource.
    fn id(&self) -> String {
        self.json()["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in: {}", self.raw_body))
            .to_owned()
    }

    /// The body's `key`, for a created card or project.
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

fn post_empty(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::POST, uri, cookie, None)
}

fn patch(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PATCH, uri, cookie, Some(body))
}

fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A project, and the config ids a test needs to talk about it.
struct Project {
    key: String,
    types: Vec<(String, String, i64)>,
    statuses: Vec<(String, String, String)>,
    resolutions: Vec<(String, String)>,
    priorities: Vec<(String, String)>,
}

impl Project {
    /// The id of the card type with this name.
    fn card_type(&self, name: &str) -> &str {
        match self.types.iter().find(|(_, n, _)| n == name) {
            Some((id, ..)) => id,
            None => panic!("{} has no card type {name:?}", self.key),
        }
    }

    /// The id of the status with this name.
    fn status(&self, name: &str) -> &str {
        match self.statuses.iter().find(|(_, n, _)| n == name) {
            Some((id, ..)) => id,
            None => panic!("{} has no status {name:?}", self.key),
        }
    }

    /// The category of the status with this name.
    fn status_category(&self, name: &str) -> &str {
        match self.statuses.iter().find(|(_, n, _)| n == name) {
            Some((_, _, category)) => category,
            None => panic!("{} has no status {name:?}", self.key),
        }
    }

    /// The id of the resolution with this name.
    fn resolution(&self, name: &str) -> &str {
        match self.resolutions.iter().find(|(_, n)| n == name) {
            Some((id, _)) => id,
            None => panic!("{} has no resolution {name:?}", self.key),
        }
    }

    /// The id of the priority with this name.
    fn priority(&self, name: &str) -> &str {
        match self.priorities.iter().find(|(_, n)| n == name) {
            Some((id, _)) => id,
            None => panic!("{} has no priority {name:?}", self.key),
        }
    }
}

/// A JSON array from a response, or a panic showing what came back instead.
///
/// `expect` rather than `unwrap` throughout these helpers: clippy's
/// `allow-unwrap-in-tests` only covers `#[test]` bodies, and these are ordinary
/// module-level functions. `tests/auth.rs` and `tests/health.rs` do the same.
fn rows(reply: &Reply) -> Vec<Value> {
    reply
        .json()
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array: {}", reply.raw_body))
        .clone()
}

/// A required string field of a JSON object.
fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("no string {field:?} in {value}"))
        .to_owned()
}

/// A required integer field of a JSON object.
fn number(value: &Value, field: &str) -> i64 {
    value[field]
        .as_i64()
        .unwrap_or_else(|| panic!("no integer {field:?} in {value}"))
}

/// Creates a project from a template and reads its config back.
async fn project(app: &App, admin: &str, key: &str, template: &str) -> Project {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": template }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let config = |path: &'static str| format!("/api/v1/projects/{key}/{path}");

    let types = rows(&app.send(get(&config("card-types"), Some(admin))).await)
        .into_iter()
        .map(|v| (text(&v, "id"), text(&v, "name"), number(&v, "level")))
        .collect();

    let statuses = rows(&app.send(get(&config("statuses"), Some(admin))).await)
        .into_iter()
        .map(|v| (text(&v, "id"), text(&v, "name"), text(&v, "category")))
        .collect();

    let resolutions = rows(&app.send(get(&config("resolutions"), Some(admin))).await)
        .into_iter()
        .map(|v| (text(&v, "id"), text(&v, "name")))
        .collect();

    let priorities = rows(&app.send(get(&config("priorities"), Some(admin))).await)
        .into_iter()
        .map(|v| (text(&v, "id"), text(&v, "name")))
        .collect();

    Project {
        key: key.to_owned(),
        types,
        statuses,
        resolutions,
        priorities,
    }
}

/// Creates a card and returns its key.
async fn card(app: &App, admin: &str, project: &Project, type_name: &str, summary: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/cards", project.key),
            Some(admin),
            json!({ "typeId": project.card_type(type_name), "summary": summary }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.key()
}

/// A card's id, by key.
async fn card_id(app: &App, admin: &str, key: &str) -> String {
    app.send(get(&format!("/api/v1/cards/{key}"), Some(admin)))
        .await
        .id()
}

/// The keys of a project's cards, in the order the API returns them.
async fn board_order(app: &App, admin: &str, project_key: &str) -> Vec<String> {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    page_keys(&reply)
}

/// The `key` of every card in a `CardPageDto`.
fn page_keys(reply: &Reply) -> Vec<String> {
    reply.json()["cards"]
        .as_array()
        .unwrap_or_else(|| panic!("no cards array: {}", reply.raw_body))
        .iter()
        .map(|c| text(c, "key"))
        .collect()
}

/// The summaries of a card's children, in rank order.
async fn child_summaries(app: &App, admin: &str, key: &str) -> Vec<String> {
    let reply = app
        .send(get(&format!("/api/v1/cards/{key}/children"), Some(admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    rows(&reply).iter().map(|c| text(c, "summary")).collect()
}

/// How many of a project's cards match a query string.
async fn filtered_total(app: &App, admin: &str, project_key: &str, query: &str) -> i64 {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/cards?{query}"),
            Some(admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    number(&reply.json(), "total")
}

/// The card keys of a project's cards under a parent, in rank order.
async fn keys_under(app: &App, admin: &str, project_key: &str, parent_id: &str) -> Vec<String> {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/cards?parentId={parent_id}"),
            Some(admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    page_keys(&reply)
}

/// The number out of a card key: `7` from `ATLAS-7`.
///
/// Split on the **last** hyphen, which is why `project::validate_key` refuses a
/// key containing one.
fn card_number(key: &str) -> i64 {
    key.rsplit_once('-')
        .unwrap_or_else(|| panic!("{key:?} is not a card key"))
        .1
        .parse()
        .unwrap_or_else(|err| panic!("{key:?} has no number after the hyphen: {err}"))
}

/// A project's card-key counter.
async fn card_counter(app: &App, admin: &str, project_key: &str) -> i64 {
    let reply = app
        .send(get(&format!("/api/v1/projects/{project_key}"), Some(admin)))
        .await;
    number(&reply.json(), "cardCounter")
}

/// A card's history, oldest first.
async fn history(app: &App, admin: &str, key: &str) -> Vec<Value> {
    let reply = app
        .send(get(&format!("/api/v1/cards/{key}/history"), Some(admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    rows(&reply)
}

/// The one history entry for a field, or a panic naming what was actually there.
fn entry<'a>(history: &'a [Value], field: &str) -> &'a Value {
    let matches: Vec<&Value> = history.iter().filter(|e| e["field"] == field).collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one {field:?} entry, found {}: {history:#?}",
        matches.len(),
    );
    matches[0]
}

// ---------------------------------------------------------------------------
// Key allocation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_card_creation_never_duplicates_a_key() {
    // Two concurrent creates never both get ATLAS-7, driven through the real
    // router — the shape a user actually hits.
    //
    // What this does NOT prove, despite appearances: that the *counter* is what
    // makes it safe. Every write in this process is funnelled through a writer
    // pool of exactly one connection, so whole transactions serialise and no two
    // allocations can interleave here whatever the allocator does. This test
    // passes unchanged against a naive `SELECT counter; UPDATE counter + 1`
    // allocator *and* against `begin()` in place of `BEGIN IMMEDIATE` — both
    // removed at once. It pins the end-to-end behaviour, not the mechanism.
    //
    // `key_allocation_survives_a_second_writer_on_the_same_database_file` is the
    // one that fails when those are broken. Keep both: this one guards the route
    // and the response, that one guards the invariant.
    const TASKS: i64 = 24;

    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let type_id = project.card_type("Story").to_owned();

    let mut handles = Vec::new();
    for i in 0..TASKS {
        // A fresh router per task, over the same pools — which is what two
        // concurrent HTTP requests actually are.
        let router = app.router();
        let cookie = admin.clone();
        let type_id = type_id.clone();
        handles.push(tokio::spawn(async move {
            let response = router
                .oneshot(post(
                    "/api/v1/projects/ATLAS/cards",
                    Some(&cookie),
                    json!({ "typeId": type_id, "summary": format!("Card {i}") }),
                ))
                .await
                .expect("request failed");
            Reply::from(response).await
        }));
    }

    let mut keys = Vec::new();
    for handle in handles {
        let reply = handle.await.expect("task panicked");
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "a concurrent create failed: {}",
            reply.raw_body
        );
        keys.push(reply.key());
    }

    let distinct: HashSet<&String> = keys.iter().collect();
    assert_eq!(
        distinct.len(),
        keys.len(),
        "two cards were given the same key: {keys:?}"
    );

    // And they are exactly ATLAS-1..ATLAS-24 — no gaps, no repeats. A gap would
    // mean a counter increment escaped its transaction.
    let mut numbers: Vec<i64> = keys.iter().map(|k| card_number(k)).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=TASKS).collect::<Vec<_>>());

    app.db.close().await;
}

#[tokio::test]
async fn a_deleted_cards_key_is_never_handed_out_again() {
    // The counter does not rewind. Reusing ATLAS-1 would silently repoint every
    // bookmark, commit message and comment that referenced the original.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    assert_eq!(card_counter(&app, &admin, "ATLAS").await, 0);

    let first = card(&app, &admin, &project, "Story", "Doomed").await;
    assert_eq!(first, "ATLAS-1");
    assert_eq!(card_counter(&app, &admin, "ATLAS").await, 1);

    let reply = app
        .send(delete(&format!("/api/v1/cards/{first}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let second = card(&app, &admin, &project, "Story", "New").await;
    assert_eq!(second, "ATLAS-2", "ATLAS-1 was handed out twice");

    // The counter itself, not just the key that came out of it. Asserting only
    // on the key would let a "count the project's cards" allocator pass — the
    // trash keeps its rows, so the count keeps climbing and the keys look right
    // until a card *leaves* the project, at which point ATLAS-1 is handed out
    // again. This pins the mechanism, so that regression cannot hide here.
    assert_eq!(
        card_counter(&app, &admin, "ATLAS").await,
        2,
        "the project's counter must advance, not be derived from a card count"
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// The changelog
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_field_change_is_written_to_history_with_raw_and_display_values() {
    // TODO.md §D1 and the central non-negotiable: history is written on every
    // field change, in the same transaction. It cannot be reconstructed later.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Original summary").await;

    let admin_id = app.send(get("/api/v1/auth/me", Some(&admin))).await.id();

    // Creation writes no history: the card's initial state *is* the event.
    assert!(
        history(&app, &admin, &key).await.is_empty(),
        "creation should not write a changelog"
    );

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({
                "summary": "Rewritten summary",
                "description": "Now with a body",
                "typeId": project.card_type("Bug"),
                "priorityId": project.priority("High"),
                "assigneeId": admin_id,
                "dueDate": "2026-08-01",
                "estimate": 3.0,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let log = history(&app, &admin, &key).await;
    let fields: HashSet<&str> = log.iter().map(|e| e["field"].as_str().unwrap()).collect();
    assert_eq!(
        fields,
        HashSet::from([
            "summary",
            "description",
            "type",
            "priority",
            "assignee",
            "due_date",
            "estimate",
        ]),
        "one entry per changed field, and nothing else: {}",
        serde_json::to_string_pretty(&log).unwrap()
    );

    // A plain field: raw and display are the same text.
    let summary = entry(&log, "summary");
    assert_eq!(summary["fromValue"], "Original summary");
    assert_eq!(summary["fromDisplay"], "Original summary");
    assert_eq!(summary["toValue"], "Rewritten summary");
    assert_eq!(summary["toDisplay"], "Rewritten summary");
    assert_eq!(summary["authorId"], admin_id.as_str());

    // A reference: the *id* in the raw column, the *name* in the display column.
    // This is the whole point of the two columns, and neither derives from the
    // other after the fact.
    let card_type = entry(&log, "type");
    assert_eq!(card_type["fromValue"], project.card_type("Story"));
    assert_eq!(card_type["fromDisplay"], "Story");
    assert_eq!(card_type["toValue"], project.card_type("Bug"));
    assert_eq!(card_type["toDisplay"], "Bug");

    let assignee = entry(&log, "assignee");
    assert_eq!(assignee["toValue"], admin_id.as_str());
    assert_eq!(
        assignee["toDisplay"], "Administrator",
        "the display column holds the user's name, not their id"
    );

    let priority = entry(&log, "priority");
    assert_eq!(priority["fromValue"], Value::Null, "it had no priority");
    assert_eq!(priority["fromDisplay"], Value::Null);
    assert_eq!(priority["toDisplay"], "High");

    let estimate = entry(&log, "estimate");
    assert_eq!(estimate["toDisplay"], "3", "not \"3.0\"");

    app.db.close().await;
}

#[tokio::test]
async fn a_display_value_survives_its_referent_being_renamed() {
    // The reason `from_display` is written at the time of the change rather than
    // resolved at read time. Storing only the id would make the history tab say
    // "moved to <current name>" — which is a lie about what the user saw.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "statusId": project.status("In Progress") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Rename the status the card just moved into.
    let reply = app
        .send(patch(
            &format!("/api/v1/statuses/{}", project.status("In Progress")),
            Some(&admin),
            json!({ "name": "Doing" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let log = history(&app, &admin, &key).await;
    let status = entry(&log, "status");
    assert_eq!(
        status["toDisplay"], "In Progress",
        "history must say what it said at the time, not what the status is called now"
    );
    // ...while the raw value still points at the live row, so a query for
    // "cards that moved into this status" keeps working across the rename.
    assert_eq!(status["toValue"], project.status("In Progress"));

    app.db.close().await;
}

#[tokio::test]
async fn resending_the_same_value_is_not_a_change() {
    // "I sent the same value again" is not an edit. Bumping updated_at for a
    // no-op would make `updated <= -7d` — the query the job-search follow-up
    // rule is built on — quietly wrong, and it would fill the history tab with
    // rows describing nothing.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let before = app
        .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await
        .json();

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "summary": "Card" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    assert!(
        history(&app, &admin, &key).await.is_empty(),
        "a no-op patch wrote a history row"
    );
    assert_eq!(
        reply.json()["updatedAt"],
        before["updatedAt"],
        "a no-op patch bumped updatedAt"
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Hierarchy: cycles and the depth cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reparent_refuses_to_make_a_card_its_own_ancestor() {
    // ADR 0002's bill: a uniform parent pointer is a graph, and graphs have
    // cycles. Dragging a card into another card is a reparent, so the board
    // hits this path constantly.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let initiative = card(&app, &admin, &project, "Initiative", "Initiative").await;
    let epic = card(&app, &admin, &project, "Epic", "Epic").await;
    let story = card(&app, &admin, &project, "Story", "Story").await;

    let initiative_id = card_id(&app, &admin, &initiative).await;
    let epic_id = card_id(&app, &admin, &epic).await;
    let story_id = card_id(&app, &admin, &story).await;

    // Initiative > Epic > Story.
    for (child, parent) in [(&epic, &initiative_id), (&story, &epic_id)] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{child}/reparent"),
                Some(&admin),
                json!({ "parentId": parent }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // Now try to hang the Initiative under the Story: walking up from the Story
    // reaches the Initiative, so this is a loop.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{initiative}/reparent"),
            Some(&admin),
            json!({ "parentId": story_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "a cycle was accepted: {}",
        reply.raw_body
    );
    assert!(
        reply.json()["detail"].as_str().unwrap().contains("loop"),
        "the error should say what went wrong: {}",
        reply.raw_body
    );

    // The direct case: a card as its own parent.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story}/reparent"),
            Some(&admin),
            json!({ "parentId": story_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // ...and the tree is untouched by either attempt.
    let reply = app
        .send(get(&format!("/api/v1/cards/{initiative}"), Some(&admin)))
        .await;
    assert_eq!(reply.json()["parentId"], Value::Null);

    app.db.close().await;
}

#[tokio::test]
// Long because it builds the whole fixture the two assertions need: a project
// with six rungs, a spine, and a detached branch. The scenario is the test, and
// splitting it into helpers would spread one argument across the file.
#[allow(clippy::too_many_lines)]
async fn the_depth_cap_counts_the_whole_subtree_that_travels_with_the_card() {
    // MAX_DEPTH is 5, and the interesting half of the rule is *what gets
    // counted*. Reparenting moves everything under the card, so the check has to
    // be `depth(new parent) + height(card's subtree)`. Checking the dragged card
    // alone — `depth(new parent) + 1` — would let a deep branch be hung off a
    // parent that only just fits, and the tree would quietly grow past the cap.
    //
    // Both cases below are built so the *level* rule is satisfied, otherwise it
    // would fire first and this would prove nothing about depth.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(&admin),
            json!({ "key": "DEEP", "name": "Deep", "template": "blank" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    // Blank seeds levels 1/0/-1 (Group/Card/Sub-task). Add four rungs above, so
    // a chain longer than the cap is *expressible* — which is exactly what makes
    // the depth cap, rather than the level rule, the thing that refuses it.
    for (level, name) in [(2, "L2"), (3, "L3"), (4, "L4"), (5, "L5")] {
        let reply = app
            .send(post(
                "/api/v1/projects/DEEP/hierarchy-levels",
                Some(&admin),
                json!({ "level": level, "name": name }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

        let reply = app
            .send(post(
                "/api/v1/projects/DEEP/card-types",
                Some(&admin),
                json!({ "name": name, "level": level }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    }

    let types: Vec<(String, String, i64)> = app
        .send(get("/api/v1/projects/DEEP/card-types", Some(&admin)))
        .await
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["id"].as_str().unwrap().to_owned(),
                v["name"].as_str().unwrap().to_owned(),
                v["level"].as_i64().unwrap(),
            )
        })
        .collect();
    let project = Project {
        key: "DEEP".to_owned(),
        types,
        statuses: vec![],
        resolutions: vec![],
        priorities: vec![],
    };

    // A spine three deep: L5 > L4 > L3.
    let l5 = card(&app, &admin, &project, "L5", "L5").await;
    let l4 = card(&app, &admin, &project, "L4", "L4").await;
    let l3 = card(&app, &admin, &project, "L3", "L3").await;

    let l5_id = card_id(&app, &admin, &l5).await;
    let l4_id = card_id(&app, &admin, &l4).await;
    let l3_id = card_id(&app, &admin, &l3).await;

    for (child, parent) in [(&l4, &l5_id), (&l3, &l4_id)] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{child}/reparent"),
                Some(&admin),
                json!({ "parentId": parent }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // A detached branch three tall: L2 > Group > Card.
    let l2 = card(&app, &admin, &project, "L2", "L2").await;
    let group = card(&app, &admin, &project, "Group", "Group").await;
    let inner = card(&app, &admin, &project, "Card", "Card").await;

    let l2_id = card_id(&app, &admin, &l2).await;
    let group_id = card_id(&app, &admin, &group).await;

    for (child, parent) in [(&group, &l2_id), (&inner, &group_id)] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{child}/reparent"),
                Some(&admin),
                json!({ "parentId": parent }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // L3 sits at depth 3 and the branch is 3 tall: 3 + 3 = 6, one past the cap.
    //
    // This is the case that distinguishes the two rules. Counting the dragged
    // card alone gives 3 + 1 = 4 and would wave it through.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{l2}/reparent"),
            Some(&admin),
            json!({ "parentId": l3_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "a 6-deep tree was accepted — the cap counted the card, not its subtree: {}",
        reply.raw_body
    );
    let detail = reply.json()["detail"].as_str().unwrap().to_owned();
    assert!(
        detail.contains('6') && detail.contains('5'),
        "the error should say how deep it would be and what the limit is: {detail}"
    );

    // The refusal left the tree alone.
    let reply = app
        .send(get(&format!("/api/v1/cards/{l2}"), Some(&admin)))
        .await;
    assert_eq!(
        reply.json()["parentId"],
        Value::Null,
        "a refused reparent must not half-apply"
    );

    // L4 sits at depth 2, so the same branch fits there: 2 + 3 = 5, exactly the
    // cap. The cap is a limit, not an off-by-one.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{l2}/reparent"),
            Some(&admin),
            json!({ "parentId": l4_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "depth 5 is the cap and must be allowed: {}",
        reply.raw_body
    );

    // And the whole subtree really did travel: the deepest card is now at 5.
    assert_eq!(
        child_summaries(&app, &admin, &l4).await,
        ["L3", "L2"],
        "L2 is now a child of L4"
    );
    assert_eq!(child_summaries(&app, &admin, &l2).await, ["Group"]);
    assert_eq!(child_summaries(&app, &admin, &group).await, ["Card"]);

    app.db.close().await;
}

#[tokio::test]
async fn a_parent_must_sit_above_its_child_in_the_hierarchy() {
    // ADR 0002's *only* structural rule: parent.level > child.level. Two cards
    // on the same rung are siblings by definition.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let story_a = card(&app, &admin, &project, "Story", "A").await;
    let story_b = card(&app, &admin, &project, "Story", "B").await;
    let epic = card(&app, &admin, &project, "Epic", "Epic").await;

    let story_b_id = card_id(&app, &admin, &story_b).await;
    let epic_id = card_id(&app, &admin, &epic).await;

    // Story under Story: same level.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story_a}/reparent"),
            Some(&admin),
            json!({ "parentId": story_b_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
    assert!(
        reply.json()["detail"]
            .as_str()
            .unwrap()
            .contains("higher level"),
        "{}",
        reply.raw_body
    );

    // Epic under Story: the wrong way round.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{epic}/reparent"),
            Some(&admin),
            json!({ "parentId": story_b_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // Story under Epic: correct.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story_a}/reparent"),
            Some(&admin),
            json!({ "parentId": epic_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn changing_a_cards_type_cannot_break_the_tree_around_it() {
    // A card's level comes from its type, so "make this Story an Epic" is a
    // structural move wearing a field edit's clothes.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let epic = card(&app, &admin, &project, "Epic", "Epic").await;
    let story = card(&app, &admin, &project, "Story", "Story").await;
    let epic_id = card_id(&app, &admin, &epic).await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story}/reparent"),
            Some(&admin),
            json!({ "parentId": epic_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Promoting the child to its parent's level would orphan it.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{story}"),
            Some(&admin),
            json!({ "typeId": project.card_type("Epic") }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "a type change broke the level rule silently: {}",
        reply.raw_body
    );

    // Demoting the parent below its child would too.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{epic}"),
            Some(&admin),
            json!({ "typeId": project.card_type("Sub-task") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // A sideways change at the same level is fine: Story and Bug are both 0.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{story}"),
            Some(&admin),
            json!({ "typeId": project.card_type("Bug") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Resolution — docs/adr §E
// ---------------------------------------------------------------------------

#[tokio::test]
async fn moving_into_a_done_status_sets_a_resolution_and_moving_out_clears_it() {
    // Jira's single most-reported confusion: an issue is resolved iff
    // `resolution IS NOT EMPTY`, independently of reaching a Done status — so a
    // card sits in Done and counts as open in every report and query.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await;
    assert_eq!(reply.json()["resolved"], false);
    assert_eq!(reply.json()["resolutionId"], Value::Null);

    // Into Done, naming no resolution: the project's default is auto-set.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "statusId": project.status("Done") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["resolutionId"],
        project.resolution("Done"),
        "a card in a done status must say why it stopped"
    );
    assert_eq!(reply.json()["resolved"], true);
    assert!(
        reply.json()["resolvedAt"].is_string(),
        "resolvedAt tracks resolutionId"
    );

    // The auto-set resolution is in the changelog, not applied invisibly.
    let log = history(&app, &admin, &key).await;
    let resolution = entry(&log, "resolution");
    assert_eq!(resolution["fromValue"], Value::Null);
    assert_eq!(resolution["toDisplay"], "Done");

    // Back out of Done: the resolution is cleared.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "statusId": project.status("In Progress") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["resolutionId"],
        Value::Null,
        "a reopened card must not still be resolved"
    );
    assert_eq!(reply.json()["resolved"], false);
    assert_eq!(reply.json()["resolvedAt"], Value::Null);

    app.db.close().await;
}

#[tokio::test]
async fn a_resolution_named_in_the_same_request_wins_over_the_default() {
    // The expressive power is worth keeping: Done, Won't Do and Duplicate are
    // genuinely different endings, and status alone cannot say which.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({
                "statusId": project.status("Done"),
                "resolutionId": project.resolution("Won't Do"),
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["resolutionId"],
        project.resolution("Won't Do"),
        "the caller's choice must not be overwritten by the default"
    );

    app.db.close().await;
}

#[tokio::test]
async fn leaving_a_done_status_overrides_a_resolution_sent_in_the_same_request() {
    // "Reopened but still resolved" is not a state anyone means, and honouring
    // both halves of a contradictory request is how the confusion gets back in.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    app.send(patch(
        &format!("/api/v1/cards/{key}"),
        Some(&admin),
        json!({ "statusId": project.status("Done") }),
    ))
    .await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({
                "statusId": project.status("To Do"),
                "resolutionId": project.resolution("Duplicate"),
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["resolutionId"],
        Value::Null,
        "a card out of a done status must be unresolved, whatever the request asked for"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_drag_into_a_done_column_sets_a_resolution_too() {
    // The rule belongs to the transition, not to the endpoint. Every route into
    // Done applies it, or the failure mode is back for whichever route forgot.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{key}/move"),
            Some(&admin),
            json!({ "statusId": project.status("Done") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["resolved"], true);
    assert_eq!(reply.json()["resolutionId"], project.resolution("Done"));

    app.db.close().await;
}

#[tokio::test]
async fn every_terminal_column_of_the_job_search_workflow_resolves_the_card() {
    // Accepted, Rejected and Ghosted are all `done` — the application is over —
    // but they are emphatically not the same outcome. That is what resolutions
    // are for, and it is why three status categories are enough.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "HUNT", "job-search").await;

    for (status, resolution) in [
        ("Accepted", "Accepted"),
        ("Rejected", "Rejected"),
        ("Ghosted", "Ghosted"),
    ] {
        let key = card(&app, &admin, &project, "Application", status).await;

        assert_eq!(
            project.status_category(status),
            "done",
            "{status} must be a terminal column"
        );

        let reply = app
            .send(patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({
                    "statusId": project.status(status),
                    "resolutionId": project.resolution(resolution),
                }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
        assert_eq!(reply.json()["resolved"], true, "{status} left a card open");
        assert_eq!(reply.json()["resolutionId"], project.resolution(resolution));
    }

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Key history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_key_resolves_through_card_key_history() {
    // Without this every bookmark, commit message, branch name, PR title and
    // ATLAS-42 autolink 404s the moment someone tidies up their projects.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let source = project(&app, &admin, "ATLAS", "programming").await;
    let target = project(&app, &admin, "OTHER", "programming").await;

    let old_key = card(&app, &admin, &source, "Story", "Migrant").await;
    assert_eq!(old_key, "ATLAS-1");

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{old_key}"),
            Some(&admin),
            json!({ "projectKey": "OTHER" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let new_key = reply.key();
    assert_eq!(new_key, "OTHER-1", "the card is renumbered in its new home");

    // The old key does not 404, and it does not silently serve the card either:
    // it redirects, so the stale reference heals instead of working forever.
    let reply = app
        .send(get(&format!("/api/v1/cards/{old_key}"), Some(&admin)))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::MOVED_PERMANENTLY,
        "a retired key must redirect, not 404: {}",
        reply.raw_body
    );
    assert_eq!(
        reply.location.as_deref(),
        Some("/api/v1/cards/OTHER-1"),
        "the redirect must point at the card's current key"
    );

    // Following it lands on the card.
    let reply = app.send(get("/api/v1/cards/OTHER-1", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["summary"], "Migrant");

    // A key that never existed is still a 404 — the redirect table is not a
    // catch-all.
    let reply = app.send(get("/api/v1/cards/ATLAS-999", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // And the vacated number is not handed out again: the counter never rewinds.
    let next = card(&app, &admin, &source, "Story", "Newcomer").await;
    assert_eq!(next, "ATLAS-2", "ATLAS-1 was reused after the card moved");

    // The move is in the changelog, with the project and the key both recorded.
    let log = history(&app, &admin, "OTHER-1").await;
    let project_change = entry(&log, "project");
    assert_eq!(project_change["fromDisplay"], "ATLAS");
    assert_eq!(project_change["toDisplay"], "OTHER");
    let key_change = entry(&log, "key");
    assert_eq!(key_change["fromValue"], "ATLAS-1");
    assert_eq!(key_change["toValue"], "OTHER-1");

    let _ = target;
    app.db.close().await;
}

#[tokio::test]
async fn a_moved_card_keeps_its_status_category_rather_than_its_status_name() {
    // Mapping by name would silently reopen a finished card the moment two
    // projects spell their columns differently — which is always.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let source = project(&app, &admin, "ATLAS", "programming").await;
    let target = project(&app, &admin, "HUNT", "job-search").await;

    let key = card(&app, &admin, &source, "Story", "In flight").await;

    // "In Progress" is an in_progress column in ATLAS; job-search has no column
    // by that name at all — its first in_progress column is "Applied".
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "statusId": source.status("In Progress") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "projectKey": "HUNT" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["statusId"],
        target.status("Applied"),
        "the card must land in a column of the same category, not reopen"
    );
    assert_eq!(
        target.status_category("Applied"),
        source.status_category("In Progress")
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Rank
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rank_ordering_survives_the_database_and_sorts_with_a_plain_order_by() {
    // Rank's guarantee is that hex byte order equals string order, so a TEXT
    // column sorts correctly under SQLite's default BINARY collation with no
    // custom collation. Declaring `cards.rank` COLLATE NOCASE would silently
    // break every board; this is what would catch that.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    // Bottom placement, so creation order is board order.
    let mut keys = Vec::new();
    for i in 0..10 {
        keys.push(card(&app, &admin, &project, "Story", &format!("Card {i}")).await);
    }

    assert_eq!(
        board_order(&app, &admin, "ATLAS").await,
        keys,
        "cards created at the bottom must come back in creation order"
    );

    // Drag the last card to the very top, naming the neighbour it landed above.
    let first_id = card_id(&app, &admin, &keys[0]).await;
    let last = keys.last().unwrap().clone();

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{last}/move"),
            Some(&admin),
            json!({ "nextCardId": first_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let mut expected = vec![last.clone()];
    expected.extend(keys[..keys.len() - 1].iter().cloned());
    assert_eq!(board_order(&app, &admin, "ATLAS").await, expected);

    // ...and into the middle, between two named neighbours.
    let after_id = card_id(&app, &admin, &keys[4]).await;
    let before_id = card_id(&app, &admin, &keys[5]).await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{}/move", keys[0]),
            Some(&admin),
            json!({ "previousCardId": after_id, "nextCardId": before_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let order = board_order(&app, &admin, "ATLAS").await;
    let position = |key: &str| order.iter().position(|k| k == key).unwrap();
    assert!(
        position(&keys[4]) < position(&keys[0]) && position(&keys[0]) < position(&keys[5]),
        "the card must land strictly between the two it was dropped between: {order:?}"
    );

    // The ordering the API returns is the database's own `ORDER BY rank`, read
    // back through a raw query — so this asserts the collation, not the handler.
    let ranked: Vec<String> =
        sqlx::query_scalar("SELECT key FROM cards WHERE deleted_at IS NULL ORDER BY rank")
            .fetch_all(app.db.reader())
            .await
            .unwrap();
    assert_eq!(
        ranked, order,
        "a plain ORDER BY rank must agree with the API's ordering"
    );

    // And every rank is distinct: a fractional index lands strictly between its
    // neighbours or fails, never on top of one.
    let ranks: Vec<String> = sqlx::query_scalar("SELECT rank FROM cards")
        .fetch_all(app.db.reader())
        .await
        .unwrap();
    let distinct: HashSet<&String> = ranks.iter().collect();
    assert_eq!(distinct.len(), ranks.len(), "two cards share a rank");

    app.db.close().await;
}

#[tokio::test]
async fn a_drop_next_to_a_card_that_has_moved_is_a_conflict_not_a_guess() {
    // The neighbours name what the user actually saw. If they have moved, the
    // honest answer is "refetch" — putting the card somewhere nobody dropped it
    // is worse than an error.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let a = card(&app, &admin, &project, "Story", "A").await;
    let b = card(&app, &admin, &project, "Story", "B").await;
    let b_id = card_id(&app, &admin, &b).await;

    // B moves to another column behind the client's back.
    app.send(post(
        &format!("/api/v1/cards/{b}/move"),
        Some(&admin),
        json!({ "statusId": project.status("In Progress") }),
    ))
    .await;

    // The client still thinks B is in To Do and drops A next to it.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{a}/move"),
            Some(&admin),
            json!({ "statusId": project.status("To Do"), "nextCardId": b_id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "a stale neighbour must not be silently ignored: {}",
        reply.raw_body
    );
    assert!(
        reply.json()["detail"].as_str().unwrap().contains("refetch"),
        "the error should tell the client what to do: {}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_move_that_names_no_neighbours_lands_at_the_bottom_of_the_column() {
    // A regression, and a quiet one. `Rank::between(None, None)` means "the list
    // is empty" and returns `Rank::first()` — so a move into a column that
    // *already has cards*, with no neighbours named, used to hand out a rank
    // equal to whatever landed there first. Two cards, one rank: `ORDER BY rank`
    // becomes a tie and the board's order goes arbitrary, with nothing failing
    // and nothing logged.
    //
    // Naming no neighbours is a real request — "just put it in this column" is
    // what a keyboard move, a bulk edit and an agent all send.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let a = card(&app, &admin, &project, "Story", "A").await;
    let b = card(&app, &admin, &project, "Story", "B").await;
    let c = card(&app, &admin, &project, "Story", "C").await;

    for key in [&a, &b, &c] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{key}/move"),
                Some(&admin),
                json!({ "statusId": project.status("In Progress") }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // Each arrival goes under the last, so the column reads in arrival order.
    assert_eq!(board_order(&app, &admin, "ATLAS").await, [a, b, c]);

    // And no two cards share a rank — the property the collision broke.
    let ranks: Vec<String> = sqlx::query_scalar("SELECT rank FROM cards")
        .fetch_all(app.db.reader())
        .await
        .unwrap();
    let distinct: HashSet<&String> = ranks.iter().collect();
    assert_eq!(
        distinct.len(),
        ranks.len(),
        "two cards were given the same rank: {ranks:?}"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_new_card_can_be_placed_at_the_top_of_its_column() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let bottom = card(&app, &admin, &project, "Story", "Existing").await;

    let reply = app
        .send(post(
            "/api/v1/projects/ATLAS/cards",
            Some(&admin),
            json!({
                "typeId": project.card_type("Story"),
                "summary": "Urgent",
                "top": true,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let top = reply.key();

    assert_eq!(board_order(&app, &admin, "ATLAS").await, [top, bottom]);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// The nested hierarchy — the whole point of ADR 0002
// ---------------------------------------------------------------------------

#[tokio::test]
// Long because building a real three-level tree *is* the test. Every card here
// is load-bearing for one of the parent-scoped queries below.
#[allow(clippy::too_many_lines)]
async fn a_three_level_nested_hierarchy_can_be_built_and_queried_by_parent() {
    // Collection > Asset > Step, from the 3D template. Not one line of Atlas
    // knows those words: they are rows in `hierarchy_levels` and `card_types`.
    //
    // This is the nested-board feature. A board is a view over a parent's
    // children, so "open the Collection" is the same query as "show the project
    // board", scoped by parent_id instead of by project.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ART", "3d-modeling").await;

    // The template named the rungs, and they are data.
    let reply = app
        .send(get("/api/v1/projects/ART/hierarchy-levels", Some(&admin)))
        .await;
    let levels: Vec<String> = reply
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(levels, ["Collection", "Asset", "Model", "Step"]);

    let collection = card(&app, &admin, &project, "Collection", "Forest Pack").await;
    let collection_id = card_id(&app, &admin, &collection).await;

    // Two assets under the collection...
    let mut asset_ids = Vec::new();
    for name in ["Oak Tree", "Pine Tree"] {
        let reply = app
            .send(post(
                "/api/v1/projects/ART/cards",
                Some(&admin),
                json!({
                    "typeId": project.card_type("Asset"),
                    "summary": name,
                    "parentId": collection_id,
                }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
        asset_ids.push(reply.id());
    }

    // ...a model under the first asset...
    let reply = app
        .send(post(
            "/api/v1/projects/ART/cards",
            Some(&admin),
            json!({
                "typeId": project.card_type("Model"),
                "summary": "Oak high-poly",
                "parentId": asset_ids[0],
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let model_id = reply.id();

    // ...and the steps under the model.
    for name in ["Retopo", "UV unwrap", "Bake"] {
        let reply = app
            .send(post(
                "/api/v1/projects/ART/cards",
                Some(&admin),
                json!({
                    "typeId": project.card_type("Step"),
                    "summary": name,
                    "parentId": model_id,
                }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    }

    // Querying by parent: each of these is a board.
    assert_eq!(
        child_summaries(&app, &admin, &collection).await,
        ["Oak Tree", "Pine Tree"],
        "opening the Collection shows its Assets"
    );

    let oak_key = keys_under(&app, &admin, "ART", &collection_id).await[0].clone();
    assert_eq!(
        child_summaries(&app, &admin, &oak_key).await,
        ["Oak high-poly"],
        "opening an Asset shows its Models"
    );

    // The deepest rung.
    let model_key = keys_under(&app, &admin, "ART", &asset_ids[0]).await[0].clone();
    assert_eq!(
        child_summaries(&app, &admin, &model_key).await,
        ["Retopo", "UV unwrap", "Bake"],
        "opening a Model shows its Steps"
    );

    // The same list endpoint, scoped three ways over one uniform parent_id.
    let reply = app
        .send(get(
            "/api/v1/projects/ART/cards?parentId=none",
            Some(&admin),
        ))
        .await;
    assert_eq!(
        reply.json()["total"],
        1,
        "only the Collection is a root: {}",
        reply.raw_body
    );

    let reply = app
        .send(get(
            &format!("/api/v1/projects/ART/cards?parentId={model_id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.json()["total"], 3, "the Model's three Steps");

    let reply = app
        .send(get("/api/v1/projects/ART/cards", Some(&admin)))
        .await;
    assert_eq!(
        reply.json()["total"],
        7,
        "and every card in the project, at any depth"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_card_created_under_a_parent_obeys_the_level_rule_too() {
    // Creating a card *into* a parent is a card being parented. The guards
    // cannot live only on the reparent path.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ART", "3d-modeling").await;

    let step = card(&app, &admin, &project, "Step", "Retopo").await;
    let step_id = card_id(&app, &admin, &step).await;

    // A Collection (level 2) under a Step (level -1) is upside down.
    let reply = app
        .send(post(
            "/api/v1/projects/ART/cards",
            Some(&admin),
            json!({
                "typeId": project.card_type("Collection"),
                "summary": "Nope",
                "parentId": step_id,
            }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "creation bypassed the level rule: {}",
        reply.raw_body
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Filtering, the trash, and comments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cards_can_be_filtered_by_status_and_assignee_and_are_paginated() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let admin_id = app.send(get("/api/v1/auth/me", Some(&admin))).await.id();

    for i in 0..5 {
        let key = card(&app, &admin, &project, "Story", &format!("Card {i}")).await;
        if i % 2 == 0 {
            app.send(patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "statusId": project.status("In Progress"), "assigneeId": admin_id }),
            ))
            .await;
        }
    }

    assert_eq!(filtered_total(&app, &admin, "ATLAS", "").await, 5);
    assert_eq!(
        filtered_total(
            &app,
            &admin,
            "ATLAS",
            &format!("statusId={}", project.status("In Progress"))
        )
        .await,
        3
    );
    assert_eq!(
        filtered_total(&app, &admin, "ATLAS", &format!("assigneeId={admin_id}")).await,
        3
    );
    assert_eq!(
        filtered_total(
            &app,
            &admin,
            "ATLAS",
            &format!("statusId={}&assigneeId={admin_id}", project.status("To Do"))
        )
        .await,
        0,
        "the filters must be ANDed, not ORed"
    );

    // Pagination: total is the match count, not the page size.
    let reply = app
        .send(get("/api/v1/projects/ATLAS/cards?limit=2", Some(&admin)))
        .await;
    assert_eq!(reply.json()["total"], 5);
    assert_eq!(reply.json()["cards"].as_array().unwrap().len(), 2);
    assert_eq!(reply.json()["limit"], 2);

    // An over-large page is clamped, and the response says so — a client that
    // asked for 10_000 needs to know it got 200 or its pagination stalls.
    let reply = app
        .send(get(
            "/api/v1/projects/ATLAS/cards?limit=10000",
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.json()["limit"], 200);

    app.db.close().await;
}

#[tokio::test]
async fn a_deleted_card_leaves_the_board_and_can_be_restored() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    app.send(post(
        &format!("/api/v1/cards/{key}/comments"),
        Some(&admin),
        json!({ "body": "Worth keeping" }),
    ))
    .await;

    let reply = app
        .send(delete(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(get("/api/v1/projects/ATLAS/cards", Some(&admin)))
        .await;
    assert_eq!(reply.json()["total"], 0, "the trash is not on the board");

    // Soft, so the comment and the history survived to be restored with it.
    let reply = app
        .send(post_empty(
            &format!("/api/v1/cards/{key}/restore"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let reply = app
        .send(get("/api/v1/projects/ATLAS/cards", Some(&admin)))
        .await;
    assert_eq!(reply.json()["total"], 1);

    let reply = app
        .send(get(&format!("/api/v1/cards/{key}/comments"), Some(&admin)))
        .await;
    assert_eq!(
        reply.json().as_array().unwrap().len(),
        1,
        "a soft delete must not take the comments with it"
    );

    // Both the delete and the restore are in the changelog.
    let fields: Vec<String> = history(&app, &admin, &key)
        .await
        .iter()
        .map(|e| e["field"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(fields, ["deleted", "deleted"]);

    app.db.close().await;
}

#[tokio::test]
async fn comments_record_that_they_were_edited() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{key}/comments"),
            Some(&admin),
            json!({ "body": "First thought" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    assert_eq!(
        reply.json()["editedAt"],
        Value::Null,
        "a new comment has not been edited"
    );
    let id = reply.id();

    let reply = app
        .send(patch(
            &format!("/api/v1/comments/{id}"),
            Some(&admin),
            json!({ "body": "Second thought" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["body"], "Second thought");
    assert!(
        reply.json()["editedAt"].is_string(),
        "an edit must be visible: {}",
        reply.raw_body
    );

    // Re-sending the same text is not an edit.
    let reply = app
        .send(patch(
            &format!("/api/v1/comments/{id}"),
            Some(&admin),
            json!({ "body": "Second thought" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Authorisation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn viewers_can_read_the_board_but_change_nothing() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(post(
            "/api/v1/users",
            Some(&admin),
            json!({
                "username": "viewer",
                "password": GOOD_PASSWORD,
                "role": "viewer",
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": "viewer", "password": GOOD_PASSWORD }),
        ))
        .await;
    let viewer = reply.session_cookie().expect("login must set a cookie");

    // Reads are fine.
    for uri in [
        "/api/v1/projects",
        "/api/v1/projects/ATLAS",
        "/api/v1/projects/ATLAS/cards",
        "/api/v1/projects/ATLAS/statuses",
    ] {
        let reply = app.send(get(uri, Some(&viewer))).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}: {}", reply.raw_body);
    }

    // Writes are not.
    let forbidden = [
        post(
            "/api/v1/projects",
            Some(&viewer),
            json!({ "key": "NOPE", "name": "Nope" }),
        ),
        post(
            "/api/v1/projects/ATLAS/cards",
            Some(&viewer),
            json!({ "typeId": project.card_type("Story"), "summary": "Nope" }),
        ),
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&viewer),
            json!({ "summary": "Nope" }),
        ),
        post(
            &format!("/api/v1/cards/{key}/move"),
            Some(&viewer),
            json!({ "statusId": project.status("Done") }),
        ),
        post(
            &format!("/api/v1/cards/{key}/comments"),
            Some(&viewer),
            json!({ "body": "Nope" }),
        ),
        post(
            "/api/v1/projects/ATLAS/statuses",
            Some(&viewer),
            json!({ "name": "Nope", "category": "todo", "position": 9 }),
        ),
        delete(&format!("/api/v1/cards/{key}"), Some(&viewer)),
    ];

    for request in forbidden {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a viewer got through {method} {uri}: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn only_an_admin_can_permanently_delete_a_project() {
    // Archive is reversible and belongs to members; delete is not and does not.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

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

    let member = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": "member", "password": GOOD_PASSWORD }),
        ))
        .await
        .session_cookie()
        .expect("login must set a cookie");

    // A member may archive...
    let reply = app
        .send(post_empty("/api/v1/projects/ATLAS/archive", Some(&member)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(reply.json()["archivedAt"].is_string());

    let reply = app.send(get("/api/v1/projects", Some(&member))).await;
    assert_eq!(
        reply.json().as_array().unwrap().len(),
        0,
        "an archived project is off the default listing"
    );
    let reply = app
        .send(get("/api/v1/projects?includeArchived=true", Some(&member)))
        .await;
    assert_eq!(reply.json().as_array().unwrap().len(), 1);

    // ...and restore.
    let reply = app
        .send(post_empty("/api/v1/projects/ATLAS/restore", Some(&member)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["archivedAt"], Value::Null);

    // But not delete.
    let reply = app
        .send(delete("/api/v1/projects/ATLAS", Some(&member)))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.raw_body);

    // The admin can, and it takes the cards with it — which is why it is the
    // only hard delete in Atlas.
    let reply = app
        .send(delete("/api/v1/projects/ATLAS", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    let reply = app
        .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // The cascade reached every child table rather than leaving orphans. This is
    // what the DEFERRABLE INITIALLY DEFERRED foreign keys in migration 0003 buy:
    // `DELETE FROM projects` cascades into `cards` and into every config table
    // at once, and SQLite fixes no order between them — so with immediate
    // enforcement this would fail whenever `card_types` happened to go first.
    //
    // One literal query rather than a formatted one per table: sqlx 0.9's
    // `SqlSafeStr` bound only admits `&'static str` without `AssertSqlSafe`, and
    // reaching for the escape hatch in a test would be a bad habit to teach the
    // next person reading this file.
    let leftovers: Vec<(String, i64)> = sqlx::query_as(
        "SELECT 'cards', COUNT(*) FROM cards \
         UNION ALL SELECT 'statuses', COUNT(*) FROM statuses \
         UNION ALL SELECT 'card_types', COUNT(*) FROM card_types \
         UNION ALL SELECT 'hierarchy_levels', COUNT(*) FROM hierarchy_levels \
         UNION ALL SELECT 'priorities', COUNT(*) FROM priorities \
         UNION ALL SELECT 'resolutions', COUNT(*) FROM resolutions \
         UNION ALL SELECT 'card_history', COUNT(*) FROM card_history \
         UNION ALL SELECT 'card_key_history', COUNT(*) FROM card_key_history \
         UNION ALL SELECT 'comments', COUNT(*) FROM comments",
    )
    .fetch_all(app.db.reader())
    .await
    .unwrap();

    for (table, count) in leftovers {
        assert_eq!(
            count, 0,
            "{table} still has rows after its project was deleted"
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn every_domain_route_needs_a_session() {
    // The layer wraps the whole /api/v1 nest, so a route added here is gated by
    // default. This is what proves it for the routes Phase 3 added.
    let app = App::new().await;

    let unauthenticated = [
        get("/api/v1/projects", None),
        get("/api/v1/projects/ATLAS", None),
        get("/api/v1/projects/ATLAS/cards", None),
        get("/api/v1/cards/ATLAS-1", None),
        get("/api/v1/cards/ATLAS-1/history", None),
        get("/api/v1/cards/ATLAS-1/children", None),
        get("/api/v1/cards/ATLAS-1/comments", None),
        get("/api/v1/projects/ATLAS/statuses", None),
        get("/api/v1/project-templates", None),
        post(
            "/api/v1/projects",
            None,
            json!({ "key": "NOPE", "name": "Nope" }),
        ),
        post(
            "/api/v1/projects/ATLAS/cards",
            None,
            json!({ "typeId": "x", "summary": "Nope" }),
        ),
        patch("/api/v1/cards/ATLAS-1", None, json!({ "summary": "Nope" })),
        delete("/api/v1/cards/ATLAS-1", None),
    ];

    for request in unauthenticated {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} let an anonymous request through: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn the_forced_reset_gate_covers_the_domain_routes() {
    // The seeded admin can reach exactly three routes until it changes its
    // password. A Phase 3 route must not be a way around that.
    let app = App::new().await;

    let cookie = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": DEFAULT_ADMIN_USERNAME, "password": ADMIN_PASSWORD }),
        ))
        .await
        .session_cookie()
        .expect("login must set a cookie");

    for request in [
        get("/api/v1/projects", Some(&cookie)),
        post(
            "/api/v1/projects",
            Some(&cookie),
            json!({ "key": "NOPE", "name": "Nope" }),
        ),
        get("/api/v1/cards/ATLAS-1", Some(&cookie)),
    ] {
        let uri = request.uri().to_string();
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{uri} was reachable before the forced password change: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[tokio::test]
// Long because it walks an application through all nine columns of the seeded
// workflow. The length is the workflow's, not the test's.
#[allow(clippy::too_many_lines)]
async fn the_job_search_template_seeds_the_requested_workflow_end_to_end() {
    // The domain-neutrality proof, through the real API. If this needed one
    // special case, the model would be wrong.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "HUNT", "job-search").await;

    let names: Vec<&str> = project
        .statuses
        .iter()
        .map(|(_, name, _)| name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Interested",
            "Applied",
            "Phone Screen",
            "Interview",
            "Take-home",
            "Offer",
            "Accepted",
            "Rejected",
            "Ghosted",
        ]
    );

    // Company > Application > Task, and no software assumptions.
    let reply = app.send(get("/api/v1/projects/HUNT", Some(&admin))).await;
    assert_eq!(
        reply.json()["cyclesEnabled"],
        false,
        "a job hunt has no sprints"
    );
    assert_eq!(reply.json()["estimationUnit"], "none");
    assert_eq!(reply.json()["template"], "job-search");

    let company = card(&app, &admin, &project, "Company", "Acme Corp").await;
    let company_id = card_id(&app, &admin, &company).await;

    let reply = app
        .send(post(
            "/api/v1/projects/HUNT/cards",
            Some(&admin),
            json!({
                "typeId": project.card_type("Application"),
                "summary": "Senior Engineer",
                "parentId": company_id,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    let application = reply.key();
    let application_id = reply.id();

    let reply = app
        .send(post(
            "/api/v1/projects/HUNT/cards",
            Some(&admin),
            json!({
                "typeId": project.card_type("Task"),
                "summary": "Tailor CV",
                "parentId": application_id,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    // Walk the application through the whole workflow.
    for status in ["Applied", "Phone Screen", "Interview", "Take-home", "Offer"] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{application}/move"),
                Some(&admin),
                json!({ "statusId": project.status(status) }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{status}: {}", reply.raw_body);
        assert_eq!(
            reply.json()["resolved"],
            false,
            "{status} is not an ending; the card must stay open"
        );
    }

    // ...to an offer accepted.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{application}/move"),
            Some(&admin),
            json!({ "statusId": project.status("Accepted") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["resolved"], true);
    assert_eq!(
        reply.json()["resolutionId"],
        project.resolution("Accepted"),
        "the auto-set resolution must be the one the workflow aims at"
    );

    // Every transition is in the changelog, in order, with readable names.
    let statuses: Vec<String> = history(&app, &admin, &application)
        .await
        .iter()
        .filter(|e| e["field"] == "status")
        .map(|e| e["toDisplay"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        statuses,
        [
            "Applied",
            "Phone Screen",
            "Interview",
            "Take-home",
            "Offer",
            "Accepted",
        ]
    );

    app.db.close().await;
}

#[tokio::test]
async fn every_template_produces_a_project_a_card_can_live_in() {
    // A project with no statuses is a project no card can be created in, and a
    // template that seeds one would only be discovered by a user.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(get("/api/v1/project-templates", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let templates: Vec<String> = reply
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        templates,
        ["programming", "3d-modeling", "job-search", "blank"]
    );

    for (index, template) in templates.iter().enumerate() {
        let key = format!("P{index}");
        let project = project(&app, &admin, &key, template).await;

        assert!(!project.statuses.is_empty(), "{template}: no statuses");
        assert!(
            !project.resolutions.is_empty(),
            "{template}: no resolutions"
        );

        let default_type = project.types.first().map(|(id, ..)| id.clone()).unwrap();
        let reply = app
            .send(post(
                &format!("/api/v1/projects/{key}/cards"),
                Some(&admin),
                json!({ "typeId": default_type, "summary": "Hello" }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "{template}: a card could not be created: {}",
            reply.raw_body
        );

        // And it can be finished — which needs a done status *and* a resolution
        // to auto-set.
        let done = match project
            .statuses
            .iter()
            .find(|(_, _, category)| category == "done")
        {
            Some((id, ..)) => id.clone(),
            None => panic!("{template}: no done status"),
        };

        let reply = app
            .send(post(
                &format!("/api/v1/cards/{}/move", reply.key()),
                Some(&admin),
                json!({ "statusId": done }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{template}: a card could not be finished: {}",
            reply.raw_body
        );
        assert_eq!(reply.json()["resolved"], true, "{template}");
    }

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Resolution: the rule must hold when the *resolution* moves, not only the
// status. Adversarial pass — every test above changes status in the same
// request, so the whole class of "patch the resolution alone" was unguarded.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_card_in_a_done_column_cannot_be_stripped_of_its_resolution() {
    // docs/adr §E's failure mode, reached from the other side. Every existing
    // test drives the rule with a *status* change; this one never touches the
    // status. If the rule is gated on a status transition, the card ends up in
    // the Done column with resolution NULL — which is precisely the Jira bug
    // §E exists to kill: resolved-iff-resolution-set means this card counts as
    // OPEN in every report, filter and `resolution = EMPTY` query while sitting
    // in Done.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "statusId": project.status("Done") }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["resolved"], true);

    // The attack: clear the resolution without moving the card out of Done.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "resolutionId": Value::Null }),
        ))
        .await;

    let card = app
        .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await
        .json();

    assert_eq!(
        card["statusId"],
        project.status("Done"),
        "the card never left Done"
    );
    assert_eq!(
        card["resolved"], true,
        "a card in a done column must always say why it stopped — clearing the resolution while \
         it sits in Done recreates the exact Jira confusion docs/adr §E exists to prevent \
         (request returned {}: {})",
        reply.status, reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_card_that_has_not_reached_a_done_column_cannot_be_handed_a_resolution() {
    // The mirror image. `resolved` is defined as `resolution IS NOT EMPTY`, so a
    // resolution set on a To Do card makes it count as *resolved* everywhere
    // while it sits in the first column — the same divergence as above, pointing
    // the other way.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;
    let key = card(&app, &admin, &project, "Story", "Card").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "resolutionId": project.resolution("Done") }),
        ))
        .await;

    let card = app
        .send(get(&format!("/api/v1/cards/{key}"), Some(&admin)))
        .await
        .json();

    assert_eq!(
        card["statusId"],
        project.status("To Do"),
        "the card never moved"
    );
    assert_eq!(
        card["resolved"], false,
        "a card that has not reached a done column must not be resolved (request returned {}: {})",
        reply.status, reply.raw_body
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// The hierarchy after a cross-project move. Adversarial pass — every reparent
// test builds its tree inside one project, so the one operation that rewrites
// every card type in a subtree at once was never checked against ADR 0002's
// only structural rule.
// ---------------------------------------------------------------------------

/// The `(key, level)` of every card in a project, by walking its card types.
async fn levels_by_key(app: &App, admin: &str, project: &Project) -> Vec<(String, i64)> {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{}/cards", project.key),
            Some(admin),
        ))
        .await;
    reply.json()["cards"]
        .as_array()
        .unwrap_or_else(|| panic!("no cards array: {}", reply.raw_body))
        .iter()
        .map(|c| {
            let type_id = text(c, "typeId");
            let level = project
                .types
                .iter()
                .find(|(id, ..)| *id == type_id)
                .map_or_else(
                    || panic!("card type {type_id} is not in the target project"),
                    |(_, _, level)| *level,
                );
            (text(c, "key"), level)
        })
        .collect()
}

#[tokio::test]
async fn a_cross_project_move_cannot_leave_a_parent_below_its_own_child() {
    // ADR 0002's *only* structural rule is `parent.level > child.level`, and
    // every other door enforces it: `create` checks it, `reparent` checks it,
    // and a type change re-checks it against both the parent and the children.
    //
    // `move_to_project` is the one door that rewrites the type of every card in
    // a subtree at once, and it maps each card by level with a fallback to the
    // target project's *default* type when the target has no rung at that level.
    // The templates make that fallback reachable with no trickery at all:
    // programming seeds Initiative at level 2, and job-search has no level 2 —
    // so an Initiative lands as an Application (level 0) while the Epic beneath
    // it lands as a Company (level 1), and the parent is now *below* its child.
    //
    // The resulting tree is one no API call could have built directly.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let source = project(&app, &admin, "ATLAS", "programming").await;
    let target = project(&app, &admin, "HUNT", "job-search").await;

    let initiative = card(&app, &admin, &source, "Initiative", "Initiative").await;
    let epic = card(&app, &admin, &source, "Epic", "Epic").await;
    let story = card(&app, &admin, &source, "Story", "Story").await;

    let initiative_id = card_id(&app, &admin, &initiative).await;
    let epic_id = card_id(&app, &admin, &epic).await;

    // Initiative(2) > Epic(1) > Story(0) — a legal tree in the source.
    for (child, parent) in [(&epic, &initiative_id), (&story, &epic_id)] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{child}/reparent"),
                Some(&admin),
                json!({ "parentId": parent }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // Move the whole subtree into a project that has no level 2.
    let reply = app
        .send(patch(
            &format!("/api/v1/cards/{initiative}"),
            Some(&admin),
            json!({ "projectKey": "HUNT" }),
        ))
        .await;

    // Whatever the move does — remap, refuse, or flatten — it must not leave a
    // stored tree that violates the rule every other door enforces. Refusing is
    // what Atlas chose, and the message has to name the rung that does not fit,
    // because "add the level to HUNT" is the only action the caller can take.
    if reply.status != StatusCode::OK {
        assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
        let detail = reply.json()["detail"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            detail.contains("level 2") && detail.contains("HUNT"),
            "the refusal must name the rung that does not fit and the project that lacks it: \
             {detail}"
        );

        // ...and it refused *before* changing anything: a half-moved subtree
        // would be worse than either outcome.
        let reply = app
            .send(get(&format!("/api/v1/cards/{initiative}"), Some(&admin)))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
        assert_eq!(
            reply.json()["projectId"],
            app.send(get("/api/v1/projects/ATLAS", Some(&admin)))
                .await
                .json()["id"],
            "the subtree must be left in ATLAS, not half-moved"
        );
        assert_eq!(
            filtered_total(&app, &admin, "HUNT", "").await,
            0,
            "no card should have landed in HUNT"
        );

        app.db.close().await;
        return;
    }

    let levels = levels_by_key(&app, &admin, &target).await;
    let level_of = |key: &str| -> i64 {
        levels
            .iter()
            .find(|(k, _)| k == key)
            .map_or_else(|| panic!("{key} is not in HUNT: {levels:?}"), |(_, l)| *l)
    };

    // The subtree kept its shape (only the root's parent link is cut), so these
    // are the three cards, renumbered.
    let (root, middle, leaf) = ("HUNT-1", "HUNT-2", "HUNT-3");

    assert!(
        level_of(root) > level_of(middle),
        "the moved root sits at level {} and its own child at level {} — a parent must be above \
         its child (ADR 0002). This tree cannot be built by any other API call: {levels:?}",
        level_of(root),
        level_of(middle),
    );
    assert!(
        level_of(middle) > level_of(leaf),
        "level {} parent over a level {} child: {levels:?}",
        level_of(middle),
        level_of(leaf),
    );

    app.db.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn key_allocation_survives_a_second_writer_on_the_same_database_file() {
    // `concurrent_card_creation_never_duplicates_a_key` above hammers the real
    // router, and it is worth having — but it cannot fail for the reason its
    // comment gives. Every write in this process is funnelled through a writer
    // pool of exactly ONE connection, and `begin_write` opens with
    // `BEGIN IMMEDIATE`, so whole transactions serialise and no two allocations
    // can interleave. It passes verbatim against a naive
    // `SELECT counter; UPDATE counter = counter + 1` allocator, even with a
    // yield forced between the read and the write. It tests the pool, not the
    // counter.
    //
    // `db/mod.rs` names the gap in its own docs: the pool split "says nothing
    // about a *second process* touching the same file (a backup tool, the
    // `sqlite3` CLI, a supervised subprocess)". This is that case — two
    // independent `Db` handles, each with its own writer connection, on one
    // file — and it is what `BEGIN IMMEDIATE`, the atomic
    // `UPDATE ... RETURNING`, and the UNIQUE index on `cards.key` are actually
    // there for. Nothing exercised it.
    const PER_WRITER: i64 = 15;

    let temp = TempDb::new();
    let config = temp.config();

    let db_a = Db::connect(&config).await.expect("failed to open writer A");
    db::migrate::run(&db_a).await.expect("failed to migrate");

    // A second handle on the same file: a separate writer connection that the
    // first pool's one-connection limit knows nothing about.
    let db_b = Db::connect(&config).await.expect("failed to open writer B");

    let mut tx = db_a.begin_write().await.expect("failed to begin");
    let project = atlas::domain::project::insert(
        &mut tx,
        &atlas::domain::project::NewProject {
            key: "ATLAS".to_owned(),
            name: "Atlas".to_owned(),
            description: None,
            lead_id: None,
            template: "blank".to_owned(),
            cycles_enabled: false,
            estimation_unit: atlas::domain::EstimationUnit::None,
        },
        chrono::Utc::now(),
    )
    .await
    .expect("failed to insert the project");
    tx.commit().await.expect("failed to commit");

    let mut handles = Vec::new();
    for db in [db_a.clone(), db_b.clone()] {
        let project_id = project.id.clone();
        handles.push(tokio::spawn(async move {
            let mut keys = Vec::new();
            for _ in 0..PER_WRITER {
                let mut tx = db.begin_write().await.expect("failed to begin");
                let key = atlas::domain::project::allocate_card_key(&mut tx, &project_id)
                    .await
                    .expect("failed to allocate");
                // Hold the transaction open across an await, which is exactly
                // where a deferred transaction would lose its snapshot and a
                // read-then-write counter would hand out a number twice.
                tokio::task::yield_now().await;
                tx.commit().await.expect("failed to commit");
                keys.push(key);
            }
            keys
        }));
    }

    let mut keys = Vec::new();
    for handle in handles {
        keys.extend(handle.await.expect("a writer panicked"));
    }

    let distinct: HashSet<&String> = keys.iter().collect();
    assert_eq!(
        distinct.len(),
        keys.len(),
        "two writers were handed the same key: {keys:?}"
    );

    // Exactly ATLAS-1..ATLAS-30, no gaps: a gap means an increment escaped its
    // transaction, and a repeat means the counter was read before it was locked.
    let mut numbers: Vec<i64> = keys.iter().map(|k| card_number(k)).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=PER_WRITER * 2).collect::<Vec<_>>());

    db_a.close().await;
    db_b.close().await;
}

#[tokio::test]
async fn the_database_itself_refuses_a_duplicate_card_key() {
    // The backstop under every allocator argument: even if the counter were
    // wrong, two cards could not share a key — the transaction would roll back
    // instead. Asserting this pins the UNIQUE index, so a migration that dropped
    // it would be caught here rather than by two cards claiming to be ATLAS-1.
    let temp = TempDb::new();
    let db = Db::connect(&temp.config()).await.expect("failed to open");
    db::migrate::run(&db).await.expect("failed to migrate");

    let err = sqlx::query("INSERT INTO cards (id, key, project_id) VALUES ('a', 'ATLAS-1', 'p')")
        .execute(db.writer())
        .await
        .expect_err("the schema accepted a card with no project");
    // The row is rejected for *some* reason (FK/NOT NULL) — what matters is the
    // key index, checked directly below.
    let _ = err;

    let index: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'cards' \
         AND sql IS NOT NULL AND sql LIKE '%key%'",
    )
    .fetch_optional(db.reader())
    .await
    .expect("failed to read the schema");

    let unique_on_key: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_index_list('cards') il \
         JOIN pragma_index_info(il.name) ii \
         WHERE il.\"unique\" = 1 AND ii.name = 'key'",
    )
    .fetch_one(db.reader())
    .await
    .expect("failed to read the index list");

    assert_eq!(
        unique_on_key, 1,
        "cards.key must carry a UNIQUE index — it is the last thing standing between an \
         allocator bug and two cards with the same key (indexes matching 'key': {index:?})"
    );

    db.close().await;
}

#[tokio::test]
async fn reparenting_onto_a_descendant_is_caught_by_the_cycle_check_itself() {
    // `reparent_refuses_to_make_a_card_its_own_ancestor` proves a 3-deep loop is
    // refused, but it cannot prove *which* guard refused it: hanging an
    // Initiative(2) under a Story(0) violates the level rule as well, so the
    // cycle check could be deleted entirely and that test would still pass on
    // the level rule alone.
    //
    // This one isolates the cycle check. Because the level rule forces levels to
    // strictly decrease downwards, a card's descendant always sits *below* it —
    // so every reparent-onto-a-descendant is caught by the level rule too, and
    // the cycle check is unreachable through legal trees. That is worth knowing
    // and worth pinning: the cycle check is defence in depth, and the assertion
    // below is on the *message*, which is the only way to tell which guard fired.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let initiative = card(&app, &admin, &project, "Initiative", "Initiative").await;
    let epic = card(&app, &admin, &project, "Epic", "Epic").await;
    let story = card(&app, &admin, &project, "Story", "Story").await;
    let subtask = card(&app, &admin, &project, "Sub-task", "Sub-task").await;

    let initiative_id = card_id(&app, &admin, &initiative).await;
    let epic_id = card_id(&app, &admin, &epic).await;
    let story_id = card_id(&app, &admin, &story).await;
    let subtask_id = card_id(&app, &admin, &subtask).await;

    // Initiative(2) > Epic(1) > Story(0) > Sub-task(-1): four rungs, all legal.
    for (child, parent) in [
        (&epic, &initiative_id),
        (&story, &epic_id),
        (&subtask, &story_id),
    ] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{child}/reparent"),
                Some(&admin),
                json!({ "parentId": parent }),
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    }

    // Every depth of descendant, including the direct child (the 2-deep case the
    // existing test skips entirely) and the 4-deep leaf.
    for (target, depth) in [
        (&epic_id, "child"),
        (&story_id, "grandchild"),
        (&subtask_id, "leaf"),
    ] {
        let reply = app
            .send(post(
                &format!("/api/v1/cards/{initiative}/reparent"),
                Some(&admin),
                json!({ "parentId": target }),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::CONFLICT,
            "reparenting onto a {depth} was accepted: {}",
            reply.raw_body
        );
        assert!(
            reply.json()["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("loop"),
            "the cycle check must be the guard that fires on a descendant, not the level rule \
             — otherwise deleting it would go unnoticed ({depth}): {}",
            reply.raw_body
        );
    }

    // The tree is exactly as it was: no attempt left a partial write behind.
    let reply = app
        .send(get(&format!("/api/v1/cards/{initiative}"), Some(&admin)))
        .await;
    assert_eq!(reply.json()["parentId"], Value::Null);
    for (child, parent) in [
        (&epic, &initiative_id),
        (&story, &epic_id),
        (&subtask, &story_id),
    ] {
        let reply = app
            .send(get(&format!("/api/v1/cards/{child}"), Some(&admin)))
            .await;
        assert_eq!(
            reply.json()["parentId"],
            parent.as_str(),
            "{child} was moved"
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn a_reparent_is_in_the_changelog_like_every_other_change() {
    // Reparent does not go through `update` — it is its own door, with its own
    // history write. A door with its own history write is a door that can forget
    // to make one, which is the exact failure the "one door" design exists to
    // prevent, so it needs its own proof.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let epic = card(&app, &admin, &project, "Epic", "Epic").await;
    let story = card(&app, &admin, &project, "Story", "Story").await;
    let epic_id = card_id(&app, &admin, &epic).await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story}/reparent"),
            Some(&admin),
            json!({ "parentId": epic_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let log = history(&app, &admin, &story).await;
    let parent = entry(&log, "parent");
    assert_eq!(parent["fromValue"], Value::Null, "it had no parent");
    assert_eq!(
        parent["toValue"],
        epic_id.as_str(),
        "the raw column holds the id"
    );
    assert_eq!(
        parent["toDisplay"], "ATLAS-1",
        "the display column holds the parent's KEY — an id is not something a human can read \
         in a history tab"
    );

    // ...and moving back to the root records the clear, rather than nothing.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story}/reparent"),
            Some(&admin),
            json!({ "parentId": Value::Null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let log = history(&app, &admin, &story).await;
    let clears: Vec<&Value> = log
        .iter()
        .filter(|e| e["field"] == "parent" && e["toValue"] == Value::Null)
        .collect();
    assert_eq!(
        clears.len(),
        1,
        "moving a card back to the root is a change and must be recorded: {}",
        serde_json::to_string_pretty(&log).unwrap()
    );

    // A no-op reparent is not a change, exactly like a no-op patch.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{story}/reparent"),
            Some(&admin),
            json!({ "parentId": Value::Null }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(
        history(&app, &admin, &story).await.len(),
        log.len(),
        "a reparent to the parent the card already has wrote a history row"
    );

    app.db.close().await;
}

#[tokio::test]
async fn every_mutable_column_of_cards_has_a_changelog_field() {
    // The "one door" design says a caller cannot forget to write history,
    // because `update` diffs the row itself. True — but it moves the forgetting
    // one level up: add a column to `cards`, add it to `CardPatch`, and forget
    // the `if` in `diff`, and the field changes silently forever. Nothing fails.
    // No test breaks. History is unreconstructable, so by the time anyone asks
    // the question, the answer is gone — which is the exact failure mode
    // TODO.md §D1 exists to prevent.
    //
    // So this reads the real schema and asserts every column that can change is
    // spoken for. It is deliberately a *schema* test rather than a behavioural
    // one: the point is to fail when a migration adds a column, at which moment
    // someone has to decide whether it belongs in the changelog. Answering "no"
    // means adding it to the list below, which is a code review someone sees.
    use atlas::domain::history::Field;

    let temp = TempDb::new();
    let db = Db::connect(&temp.config()).await.expect("failed to open");
    db::migrate::run(&db).await.expect("failed to migrate");

    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('cards')")
        .fetch_all(db.reader())
        .await
        .expect("failed to read the cards schema");
    assert!(!columns.is_empty(), "pragma returned no columns");

    // The columns that deliberately have no changelog entry, each with the
    // reason it cannot be a "field change".
    let untracked: HashSet<&str> = HashSet::from([
        "id",         // immutable identity
        "creator_id", // a fact about the past
        "created_at", // ditto
        "updated_at", // derived from the change, not a change
    ]);

    // Column -> the logical field name the changelog uses for it. The spellings
    // are asserted against `Field` rather than hardcoded, so a rename of either
    // side has to move both.
    let tracked: Vec<(&str, Field)> = vec![
        ("key", Field::Key),
        ("project_id", Field::Project),
        ("type_id", Field::Type),
        ("parent_id", Field::Parent),
        ("summary", Field::Summary),
        ("description", Field::Description),
        ("status_id", Field::Status),
        ("priority_id", Field::Priority),
        ("assignee_id", Field::Assignee),
        ("reporter_id", Field::Reporter),
        ("resolution_id", Field::Resolution),
        ("estimate", Field::Estimate),
        ("resolved_at", Field::ResolvedAt),
        ("due_date", Field::DueDate),
        ("start_date", Field::StartDate),
        ("rank", Field::Rank),
        ("archived_at", Field::Archived),
        ("deleted_at", Field::Deleted),
    ];

    let spoken_for: HashSet<&str> = tracked
        .iter()
        .map(|(column, _)| *column)
        .chain(untracked.iter().copied())
        .collect();

    for column in &columns {
        assert!(
            spoken_for.contains(column.as_str()),
            "`cards`.`{column}` is new and has no changelog field. Either give it a \
             `history::Field` and an `if` in `card::diff`, or add it to the `untracked` list \
             above with the reason it cannot change. Silently is not one of the options — \
             history cannot be reconstructed later."
        );
    }

    // ...and the reverse: nothing claims a column that no longer exists, which is
    // how this list would rot into a rubber stamp.
    let actual: HashSet<&str> = columns.iter().map(String::as_str).collect();
    for (column, _) in &tracked {
        assert!(
            actual.contains(column),
            "this test tracks `cards`.`{column}`, which the schema no longer has"
        );
    }
    for column in &untracked {
        assert!(
            actual.contains(column),
            "this test excuses `cards`.`{column}`, which the schema no longer has"
        );
    }

    db.close().await;
}

#[tokio::test]
async fn two_cards_dropped_into_the_same_gap_do_not_collide_on_one_rank() {
    // `Rank::between` is deterministic: `between(A, B)` returns the same key
    // every time it is asked. `move_card` reads the two named neighbours and
    // computes a key between them — but moving a card into the gap does not
    // change A or B, so a second drop naming the *same* pair recomputes the
    // *same* key. Two cards, one rank.
    //
    // `neighbour_rank` already rejects a neighbour that has left the column
    // ("that card is not in that column any more; refetch"), on the stated
    // principle that naming two cards is "a statement about what the user
    // actually saw" and a stale statement earns a 409 rather than a guess. A
    // pair that is no longer *adjacent* is exactly as stale — the user could not
    // have seen A directly above B, because C is between them — but it is
    // waved through.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ATLAS", "programming").await;

    let a = card(&app, &admin, &project, "Story", "A").await;
    let b = card(&app, &admin, &project, "Story", "B").await;
    let c = card(&app, &admin, &project, "Story", "C").await;
    let d = card(&app, &admin, &project, "Story", "D").await;

    let a_id = card_id(&app, &admin, &a).await;
    let b_id = card_id(&app, &admin, &b).await;

    // Drop C between A and B. Board is now A, C, B, D.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{c}/move"),
            Some(&admin),
            json!({ "previousCardId": a_id, "nextCardId": b_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let c_rank = text(&reply.json(), "rank");

    // Now drop D naming the same pair. A and B are both still in the column, so
    // the existing staleness check does not fire — but they are not adjacent any
    // more, and the gap the user thinks they are dropping into does not exist.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{d}/move"),
            Some(&admin),
            json!({ "previousCardId": a_id, "nextCardId": b_id }),
        ))
        .await;

    // The honest answer is a 409 naming the card that is in the way, on exactly
    // the principle `neighbour_rank` already applies to a neighbour that left
    // the column. Waving it through hands C and D the same rank.
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "a drop between two cards that are no longer adjacent must be refused, not guessed at \
         — `Rank::between` is deterministic, so guessing hands D the same rank as C ({c_rank}), \
         `ORDER BY rank` becomes a tie, and the board stops showing the user where they put \
         things: {}",
        reply.raw_body
    );
    assert!(
        reply.json()["detail"]
            .as_str()
            .unwrap_or_default()
            .contains(&c),
        "the refusal must name the card that is in the way, so the client knows what it missed: \
         {}",
        reply.raw_body
    );

    // D did not move.
    let after = app
        .send(get(&format!("/api/v1/cards/{d}"), Some(&admin)))
        .await;
    assert_ne!(
        text(&after.json(), "rank"),
        c_rank,
        "D took C's rank anyway"
    );

    // ...and the guard does not break the legitimate drop: naming the pair the
    // user can actually see (A above C) lands D between them, with its own rank.
    let c_id = card_id(&app, &admin, &c).await;
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{d}/move"),
            Some(&admin),
            json!({ "previousCardId": a_id, "nextCardId": c_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let d_rank = text(&reply.json(), "rank");
    assert_ne!(
        d_rank, c_rank,
        "D and C still collided on a legitimate drop"
    );
    assert_eq!(
        board_order(&app, &admin, "ATLAS").await,
        vec![a.clone(), d.clone(), c.clone(), b.clone()],
        "D was dropped between A and C and must appear there"
    );

    // And the invariant the rank test asserts sequentially, restated: no two live
    // cards share a key.
    let ranks: Vec<String> = sqlx::query_scalar("SELECT rank FROM cards WHERE deleted_at IS NULL")
        .fetch_all(app.db.reader())
        .await
        .expect("failed to read ranks");
    let distinct: HashSet<&String> = ranks.iter().collect();
    assert_eq!(
        distinct.len(),
        ranks.len(),
        "two cards share a rank: {ranks:?}"
    );

    app.db.close().await;
}

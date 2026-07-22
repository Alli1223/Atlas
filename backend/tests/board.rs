//! End-to-end board tests, over the real router and a real database.
//!
//! Driven through `tower::ServiceExt::oneshot`, the pattern `tests/aql.rs` and
//! `tests/domain.rs` established: no ports, every middleware layer still runs. The
//! claims pinned here are the ones that would be *design* failures if false —
//!
//! - columns come back in status (`position`) order, and cards in rank order
//!   within a column;
//! - a `parent` scope returns a card's **direct** children only — not its
//!   grandchildren, not its siblings, not the parent;
//! - the `childRollup` mini-map counts children by status category correctly, and
//!   is `null` for a leaf — the feature that lets a card preview its own board
//!   without an N+1 fetch;
//! - an `aql` quick filter narrows the board;
//! - an outsider gets 404 on a project they cannot access;
//! - `swimlane=assignee` partitions the same cards by assignee.

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
        self.set_cookie.iter().find_map(|raw| {
            let prefix = format!("{}=", session::COOKIE_NAME);
            let rest = raw.strip_prefix(&prefix)?;
            let value = rest.split(';').next()?;
            (!value.is_empty()).then(|| value.to_owned())
        })
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

/// The current user's id, from `/auth/me`.
async fn my_id(app: &App, cookie: &str) -> String {
    app.send(get("/api/v1/auth/me", Some(cookie))).await.id()
}

/// Creates a Member user who can log in immediately, and returns `(id, cookie)`.
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
    types: Vec<(String, String)>,
    statuses: Vec<(String, String)>,
    priorities: Vec<(String, String)>,
}

impl Fixture {
    fn type_named(&self, name: &str) -> &str {
        &self
            .types
            .iter()
            .find(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("no type {name}"))
            .0
    }
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

fn id_name_pairs(reply: &Reply) -> Vec<(String, String)> {
    rows(reply)
        .into_iter()
        .map(|v| {
            (
                v["id"].as_str().expect("config id").to_owned(),
                v["name"].as_str().expect("config name").to_owned(),
            )
        })
        .collect()
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
    let types = id_name_pairs(&app.send(get(&cfg("card-types"), Some(admin))).await);
    let statuses = id_name_pairs(&app.send(get(&cfg("statuses"), Some(admin))).await);
    let priorities = id_name_pairs(&app.send(get(&cfg("priorities"), Some(admin))).await);

    Fixture {
        key: key.to_owned(),
        types,
        statuses,
        priorities,
    }
}

/// Creates a card. `body` overrides fields on a default `{ typeId: Story }`.
async fn card(app: &App, cookie: &str, fx: &Fixture, body: Value) -> String {
    let mut object = json!({ "typeId": fx.type_named("Story") });
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

/// Fetches the board and returns the parsed JSON.
async fn board(app: &App, cookie: &str, key: &str, query: &str) -> Value {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{key}/board{query}"),
            Some(cookie),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.json()
}

/// The column names in the order the board returned them.
fn column_names(board: &Value) -> Vec<String> {
    board["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| {
            c["status"]["name"]
                .as_str()
                .expect("status name")
                .to_owned()
        })
        .collect()
}

/// The card keys in the named column, in order.
fn column_cards(board: &Value, status_name: &str) -> Vec<String> {
    board["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .find(|c| c["status"]["name"].as_str() == Some(status_name))
        .unwrap_or_else(|| panic!("no column {status_name}"))["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .map(|c| c["key"].as_str().expect("card key").to_owned())
        .collect()
}

/// Every card key on the board, across all columns.
fn all_cards(board: &Value) -> Vec<String> {
    board["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .flat_map(|c| c["cards"].as_array().expect("cards").iter())
        .map(|c| c["key"].as_str().expect("card key").to_owned())
        .collect()
}

/// One board card by key, wherever it sits.
fn find_card(board: &Value, key: &str) -> Value {
    board["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .flat_map(|c| c["cards"].as_array().expect("cards").iter())
        .find(|c| c["key"].as_str() == Some(key))
        .unwrap_or_else(|| panic!("card {key} not on the board"))
        .clone()
}

// ---------------------------------------------------------------------------
// Columns and rank order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn columns_come_back_in_status_order_and_cards_in_rank_order() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let todo = fx.status("To Do").to_owned();

    // Three cards, each pushed to the *top* of the column, so the last created
    // ranks first: the board order (c, b, a) is not the creation order (a, b, c),
    // which is what makes this a rank-order test rather than an insertion-order
    // one.
    let a = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "a", "statusId": todo, "top": true }),
    )
    .await;
    let b = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "b", "statusId": todo, "top": true }),
    )
    .await;
    let c = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "c", "statusId": todo, "top": true }),
    )
    .await;

    let board = board(&app, &admin, "ATLAS", "").await;

    // Columns in the project's status `position` order.
    assert_eq!(
        column_names(&board),
        vec!["To Do", "In Progress", "In Review", "Blocked", "Done"],
    );

    // Cards in rank order within the To Do column.
    assert_eq!(column_cards(&board, "To Do"), vec![c, b, a]);

    // The other columns are present but empty — a board shows every status.
    assert!(column_cards(&board, "Done").is_empty());
}

// ---------------------------------------------------------------------------
// The parent scope (the nested board)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_parent_scope_returns_only_direct_children() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let epic = fx.type_named("Epic").to_owned();
    let story = fx.type_named("Story").to_owned();
    let subtask = fx.type_named("Sub-task").to_owned();

    // A top-level Epic, two Story children under it, a Sub-task grandchild under
    // one of them, and an unrelated top-level sibling.
    let parent = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "epic", "typeId": epic }),
    )
    .await;
    let parent_id = card_id(&app, &admin, &parent).await;

    let c1 = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "child 1", "typeId": story, "parentId": parent_id }),
    )
    .await;
    let c1_id = card_id(&app, &admin, &c1).await;
    let c2 = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "child 2", "typeId": story, "parentId": parent_id }),
    )
    .await;
    let grandchild = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "grandchild", "typeId": subtask, "parentId": c1_id }),
    )
    .await;
    let sibling = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "sibling", "typeId": epic }),
    )
    .await;

    // The nested board of the Epic: its direct children only.
    let nested = board(&app, &admin, "ATLAS", &format!("?parent={parent}")).await;
    let mut children = all_cards(&nested);
    children.sort();
    let mut expected = vec![c1.clone(), c2.clone()];
    expected.sort();
    assert_eq!(
        children, expected,
        "nested board is exactly the direct children"
    );
    assert!(
        !all_cards(&nested).contains(&grandchild),
        "grandchild leaked in"
    );
    assert!(
        !all_cards(&nested).contains(&parent),
        "the parent is not its own child"
    );
    assert!(
        !all_cards(&nested).contains(&sibling),
        "a sibling leaked in"
    );

    // The top-level board has the roots, and neither the children nor the
    // grandchild.
    let root = board(&app, &admin, "ATLAS", "").await;
    let roots = all_cards(&root);
    assert!(roots.contains(&parent) && roots.contains(&sibling));
    assert!(!roots.contains(&c1) && !roots.contains(&c2) && !roots.contains(&grandchild));
}

/// The card's own id, read from `GET /cards/{key}`.
async fn card_id(app: &App, cookie: &str, key: &str) -> String {
    app.send(get(&format!("/api/v1/cards/{key}"), Some(cookie)))
        .await
        .id()
}

// ---------------------------------------------------------------------------
// The mini-map rollup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn child_rollup_counts_by_category_and_is_null_for_a_leaf() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let fx = project(&app, &admin, "ATLAS").await;

    let epic = fx.type_named("Epic").to_owned();
    let story = fx.type_named("Story").to_owned();
    let todo = fx.status("To Do").to_owned();
    let in_progress = fx.status("In Progress").to_owned();
    let done = fx.status("Done").to_owned();

    // An Epic with three children spanning all three categories.
    let parent = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "epic", "typeId": epic }),
    )
    .await;
    let parent_id = card_id(&app, &admin, &parent).await;
    for status in [&todo, &in_progress, &done] {
        card(
            &app,
            &admin,
            &fx,
            json!({ "summary": "child", "typeId": story, "parentId": parent_id, "statusId": status }),
        )
        .await;
    }

    // A leaf card with no children.
    let leaf = card(&app, &admin, &fx, json!({ "summary": "leaf" })).await;

    let root = board(&app, &admin, "ATLAS", "").await;

    // The Epic's rollup is computed by category — one To Do, one In Progress, one
    // Done — in a single query, not one per card.
    let rollup = find_card(&root, &parent)["childRollup"].clone();
    assert_eq!(rollup["total"], json!(3), "{rollup}");
    assert_eq!(rollup["todo"], json!(1), "{rollup}");
    assert_eq!(rollup["inProgress"], json!(1), "{rollup}");
    assert_eq!(rollup["done"], json!(1), "{rollup}");

    // A leaf has no rollup at all — this is what lets the frontend tell a
    // board-bearing card from an ordinary one.
    assert_eq!(find_card(&root, &leaf)["childRollup"], Value::Null);
}

// ---------------------------------------------------------------------------
// The AQL quick filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_aql_filter_narrows_the_board() {
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

    // No filter: both cards.
    let all = board(&app, &admin, "ATLAS", "").await;
    assert_eq!(all_cards(&all).len(), 2);

    // A quick filter: only the urgent one. The filter rides the AQL layer, so its
    // predicate is ANDed with the board's scope.
    let filtered = board(&app, &admin, "ATLAS", "?aql=priority%20%3E%3D%20High").await;
    assert_eq!(all_cards(&filtered), vec![urgent]);
    assert!(!all_cards(&filtered).contains(&lazy));
}

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_outsider_gets_404_on_a_board_they_cannot_access() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let _fx = project(&app, &admin, "SECRET").await;
    let (_id, outsider) = member(&app, &admin, "outsider").await;

    // The outsider is a Member of the instance but holds no row on SECRET, so the
    // project-access layer answers 404 — not 403, which would confirm the key
    // exists.
    let reply = app
        .send(get("/api/v1/projects/SECRET/board", Some(&outsider)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);
}

// ---------------------------------------------------------------------------
// Swimlanes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn swimlane_assignee_groups_cards_by_assignee() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let admin_id = my_id(&app, &admin).await;
    let (member_id, _member_cookie) = member(&app, &admin, "worker").await;
    let fx = project(&app, &admin, "ATLAS").await;

    let mine = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "mine", "assigneeId": admin_id }),
    )
    .await;
    let theirs = card(
        &app,
        &admin,
        &fx,
        json!({ "summary": "theirs", "assigneeId": member_id }),
    )
    .await;
    let nobody = card(&app, &admin, &fx, json!({ "summary": "nobody" })).await;

    let board = board(&app, &admin, "ATLAS", "?swimlane=assignee").await;

    // The flat columns still hold every card.
    let mut flat = all_cards(&board);
    flat.sort();
    let mut expected = vec![mine.clone(), theirs.clone(), nobody.clone()];
    expected.sort();
    assert_eq!(flat, expected);

    // ...and the swimlanes partition them: one lane per assignee plus Unassigned.
    let lanes = board["swimlanes"].as_array().expect("swimlanes present");
    assert_eq!(lanes.len(), 3, "two assignees and one unassigned lane");

    assert_eq!(lane_cards(lanes, &admin_id), vec![mine]);
    assert_eq!(lane_cards(lanes, &member_id), vec![theirs]);
    // The unassigned lane's key is the empty string.
    assert_eq!(lane_cards(lanes, ""), vec![nobody]);

    // Without a swimlane request, there are no swimlanes at all.
    let plain = board_no_lanes(&app, &admin, "ATLAS").await;
    assert!(plain.get("swimlanes").is_none() || plain["swimlanes"].is_null());
}

/// The card keys in the swimlane with the given key, across its columns.
fn lane_cards(lanes: &[Value], key: &str) -> Vec<String> {
    lanes
        .iter()
        .find(|l| l["key"].as_str() == Some(key))
        .unwrap_or_else(|| panic!("no swimlane keyed {key:?}"))["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .flat_map(|c| c["cards"].as_array().expect("cards").iter())
        .map(|c| c["key"].as_str().expect("card key").to_owned())
        .collect()
}

async fn board_no_lanes(app: &App, cookie: &str, key: &str) -> Value {
    board(app, cookie, key, "").await
}

//! End-to-end tag tests, over the real router, the real middleware stack and a
//! real database.
//!
//! Driven through `tower::ServiceExt::oneshot` — no TCP, no ports, no races, and
//! every layer still runs. The `App` harness and `admin_past_the_gate` are lifted
//! from `tests/domain.rs`, which lifted them from `tests/auth.rs`, as its handoff
//! intended.
//!
//! # What these are for
//!
//! `domain::tag`'s unit tests prove the rules in isolation. These prove the
//! claims that only a database can answer, and that would be *data loss* rather
//! than bugs if they were false:
//!
//! - a name with a space never reaches the database;
//! - merge relinks every card and deletes the source, without duplicating a
//!   `(card, tag)` pair and without orphaning a card that had both;
//! - rename does not orphan;
//! - deleting a tag takes its `card_tags` rows with it and nothing else;
//! - each template seeds its documented preset list;
//! - a global tag cannot exist twice, which `UNIQUE (project_id, name)` alone
//!   does *not* guarantee;
//! - a card cannot be given another project's tag.

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
use std::collections::{HashMap, HashSet};
use tower::ServiceExt;

/// The seeded credentials every test starts from.
const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy.
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

// ---------------------------------------------------------------------------
// Harness — lifted from tests/domain.rs, as its handoff intended.
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

/// A JSON array from a response, or a panic showing what came back instead.
///
/// `expect` rather than `unwrap` throughout these helpers: clippy's
/// `allow-unwrap-in-tests` only covers `#[test]` bodies, and these are ordinary
/// module-level functions.
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

/// A project created from a template, and the default card type to make cards
/// with.
struct Project {
    key: String,
    card_type: String,
}

/// Creates a project from a template.
async fn project(app: &App, admin: &str, key: &str, template: &str) -> Project {
    let reply = app
        .send(post(
            "/api/v1/projects",
            Some(admin),
            json!({ "key": key, "name": key, "template": template }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let types = rows(
        &app.send(get(
            &format!("/api/v1/projects/{key}/card-types"),
            Some(admin),
        ))
        .await,
    );
    let card_type = match types
        .iter()
        .find(|t| t["isDefault"].as_bool() == Some(true))
    {
        Some(t) => text(t, "id"),
        None => panic!("{key} has no default card type"),
    };

    Project {
        key: key.to_owned(),
        card_type,
    }
}

/// Creates a card and returns its key.
async fn card(app: &App, admin: &str, project: &Project, summary: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/cards", project.key),
            Some(admin),
            json!({ "typeId": project.card_type, "summary": summary }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.key()
}

/// Creates a tag in a project and returns its id.
async fn tag(app: &App, admin: &str, project_key: &str, name: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/tags"),
            Some(admin),
            json!({ "name": name }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

/// Puts a tag on a card.
async fn attach(app: &App, admin: &str, card_key: &str, tag_id: &str) {
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card_key}/tags"),
            Some(admin),
            json!({ "tagId": tag_id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
}

/// The names of a card's tags.
async fn card_tags(app: &App, admin: &str, card_key: &str) -> Vec<String> {
    let reply = app
        .send(get(&format!("/api/v1/cards/{card_key}/tags"), Some(admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    rows(&reply).iter().map(|t| text(t, "name")).collect()
}

/// Every tag a project offers, as `name -> usageCount`.
async fn project_tags(app: &App, admin: &str, project_key: &str) -> HashMap<String, i64> {
    let reply = app
        .send(get(
            &format!("/api/v1/projects/{project_key}/tags"),
            Some(admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    rows(&reply)
        .iter()
        .map(|t| {
            (
                text(t, "name"),
                t["usageCount"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("no usageCount in {t}")),
            )
        })
        .collect()
}

/// How many `card_tags` rows exist, in total.
///
/// The orphan detector: every assertion about merge and delete comes down to
/// this number and which cards it is spread across.
async fn card_tag_rows(app: &App) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM card_tags")
        .fetch_one(app.db.reader())
        .await
        .expect("failed to count card_tags")
}

// ---------------------------------------------------------------------------
// The no-spaces rule
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tag_name_with_a_space_is_rejected() {
    // The rule TODO.md Phase 4 states, at the boundary that has to enforce it.
    // Phase 6's grammar has to parse `tag = needs-review` out of a line someone
    // typed; `needs review` makes that line ambiguous forever, and no later
    // migration can un-ambiguate other people's data.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "SPACE", "blank").await;

    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/tags", project.key),
            Some(&admin),
            json!({ "name": "needs review" }),
        ))
        .await;

    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );
    assert_eq!(reply.json()["type"], "urn:atlas:error:validation");
    // The message has to teach the convention, not just refuse.
    assert!(
        reply.raw_body.contains("needs-review"),
        "expected a hyphenated suggestion: {}",
        reply.raw_body
    );

    // And nothing was written.
    assert!(
        !project_tags(&app, &admin, &project.key)
            .await
            .contains_key("needs review")
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_rename_to_a_name_with_a_space_is_rejected_too() {
    // The create path is not the only way a name reaches the column. A rule
    // enforced on one of two write paths is not enforced.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "RENSP", "blank").await;
    let id = tag(&app, &admin, &project.key, "wip").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/tags/{id}"),
            Some(&admin),
            json!({ "name": "work in progress" }),
        ))
        .await;

    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // The original survives untouched.
    assert!(
        project_tags(&app, &admin, &project.key)
            .await
            .contains_key("wip")
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_non_breaking_space_is_rejected_like_any_other_space() {
    // U+00A0 is the one that actually gets through. It is pasted rather than
    // typed, it is invisible in every UI, and it breaks the grammar exactly as a
    // plain space does — so a `== ' '` check would be a rule that looks enforced
    // and is not.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "NBSP", "blank").await;

    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/tags", project.key),
            Some(&admin),
            json!({ "name": "needs\u{00A0}review" }),
        ))
        .await;

    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn merge_relinks_every_card_and_deletes_the_source() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "MERGE", "blank").await;

    let source = tag(&app, &admin, &project.key, "bugfix").await;
    let into = tag(&app, &admin, &project.key, "bug").await;

    let one = card(&app, &admin, &project, "one").await;
    let two = card(&app, &admin, &project, "two").await;
    let three = card(&app, &admin, &project, "three").await;

    attach(&app, &admin, &one, &source).await;
    attach(&app, &admin, &two, &source).await;
    // `three` carries neither, and must be untouched.
    let _ = &three;

    let reply = app
        .send(post(
            &format!("/api/v1/tags/{source}/merge"),
            Some(&admin),
            json!({ "intoTagId": into }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["tag"]["id"], into, "the target survives");
    assert_eq!(reply.json()["relinkedCards"], 2);

    // Every card that had the source now has the target...
    assert_eq!(card_tags(&app, &admin, &one).await, ["bug"]);
    assert_eq!(card_tags(&app, &admin, &two).await, ["bug"]);
    // ...and no card lost its tag on the way. This is the orphan check: had the
    // source been deleted before the relink, the cascade would have taken these
    // rows and both cards would silently be carrying nothing.
    assert!(card_tags(&app, &admin, &three).await.is_empty());

    // The source is gone.
    let names = project_tags(&app, &admin, &project.key).await;
    assert!(!names.contains_key("bugfix"), "the source must not survive");
    assert_eq!(names.get("bug"), Some(&2), "and the target absorbed both");

    // Two rows, not four, not three: exactly one per relinked card.
    assert_eq!(card_tag_rows(&app).await, 2);

    app.db.close().await;
}

#[tokio::test]
async fn merging_does_not_duplicate_a_card_that_carried_both_tags() {
    // The case merge exists for. A card tagged `bug` *and* `bugfix` is precisely
    // why someone reaches for this button, and a plain
    // `UPDATE card_tags SET tag_id = ?` would try to write a (card, bug) row
    // that already exists — a primary-key violation surfacing as a 500 on the
    // one input the feature is for.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "DUP", "blank").await;

    let source = tag(&app, &admin, &project.key, "bugfix").await;
    let into = tag(&app, &admin, &project.key, "bug").await;

    let both = card(&app, &admin, &project, "carries both").await;
    let only_source = card(&app, &admin, &project, "carries the source").await;

    attach(&app, &admin, &both, &source).await;
    attach(&app, &admin, &both, &into).await;
    attach(&app, &admin, &only_source, &source).await;
    assert_eq!(card_tag_rows(&app).await, 3);

    let reply = app
        .send(post(
            &format!("/api/v1/tags/{source}/merge"),
            Some(&admin),
            json!({ "intoTagId": into }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    // Only one card *gained* the target; the other already had it.
    assert_eq!(
        reply.json()["relinkedCards"],
        1,
        "a card that already carried both did not change"
    );

    // One chip each, not two on the card that had both.
    assert_eq!(card_tags(&app, &admin, &both).await, ["bug"]);
    assert_eq!(card_tags(&app, &admin, &only_source).await, ["bug"]);
    assert_eq!(
        card_tag_rows(&app).await,
        2,
        "no duplicate (card, tag) pair"
    );

    // And the usage count is 2, not 3 — it counts cards, and always did.
    assert_eq!(
        project_tags(&app, &admin, &project.key).await.get("bug"),
        Some(&2)
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_tag_cannot_be_merged_into_itself() {
    // Left unguarded, `INSERT OR IGNORE ... SELECT` from the tag into itself
    // would no-op and the delete would then remove the only tag involved —
    // "merge bug into bug" would silently destroy `bug` and every card's chip.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "SELF", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;
    attach(&app, &admin, &one, &id).await;

    let reply = app
        .send(post(
            &format!("/api/v1/tags/{id}/merge"),
            Some(&admin),
            json!({ "intoTagId": id }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // The tag and the card's chip both survive.
    assert_eq!(card_tags(&app, &admin, &one).await, ["bug"]);

    app.db.close().await;
}

#[tokio::test]
async fn tags_from_different_projects_cannot_be_merged() {
    // Merging across projects would retag cards in a project the caller is not
    // looking at — and either hand them a tag scoped to a project they are not
    // in, or drop their tag entirely. Neither is what "merge these two labels"
    // means, so both directions are refused rather than half-implemented.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let alpha = project(&app, &admin, "ALPHA", "blank").await;
    let beta = project(&app, &admin, "BETA", "blank").await;

    let mine = tag(&app, &admin, &alpha.key, "shared").await;
    let theirs = tag(&app, &admin, &beta.key, "shared").await;

    let card_in_beta = card(&app, &admin, &beta, "theirs").await;
    attach(&app, &admin, &card_in_beta, &theirs).await;

    let reply = app
        .send(post(
            &format!("/api/v1/tags/{mine}/merge"),
            Some(&admin),
            json!({ "intoTagId": theirs }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // Both survive, and BETA's card is untouched.
    assert!(
        project_tags(&app, &admin, &alpha.key)
            .await
            .contains_key("shared")
    );
    assert_eq!(card_tags(&app, &admin, &card_in_beta).await, ["shared"]);

    app.db.close().await;
}

#[tokio::test]
async fn merging_a_tag_that_does_not_exist_is_a_404_and_changes_nothing() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "GONE", "blank").await;

    let real = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;
    attach(&app, &admin, &one, &real).await;

    // A missing target must not delete the source on the way to finding out.
    let reply = app
        .send(post(
            &format!("/api/v1/tags/{real}/merge"),
            Some(&admin),
            json!({ "intoTagId": "01890000-0000-7000-8000-000000000000" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    assert_eq!(card_tags(&app, &admin, &one).await, ["bug"]);
    assert_eq!(card_tag_rows(&app).await, 1);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

#[tokio::test]
async fn renaming_a_tag_does_not_orphan_its_cards() {
    // TODO.md: "rename must not orphan cards". It cannot, because `card_tags`
    // references the id — but "cannot by construction" is exactly the sort of
    // claim that quietly stops being true the day someone adds a denormalised
    // cache of tag names, so it is pinned rather than trusted.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "RENAME", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;
    let two = card(&app, &admin, &project, "two").await;
    attach(&app, &admin, &one, &id).await;
    attach(&app, &admin, &two, &id).await;

    let reply = app
        .send(patch(
            &format!("/api/v1/tags/{id}"),
            Some(&admin),
            json!({ "name": "defect", "colour": "magenta" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["name"], "defect");
    assert_eq!(reply.json()["colour"], "magenta");
    assert_eq!(reply.id(), id, "a rename must not mint a new tag");

    // Both cards still carry it, under the new name.
    assert_eq!(card_tags(&app, &admin, &one).await, ["defect"]);
    assert_eq!(card_tags(&app, &admin, &two).await, ["defect"]);
    assert_eq!(card_tag_rows(&app).await, 2, "no row was touched");

    // And the usage count followed the rename rather than resetting.
    let names = project_tags(&app, &admin, &project.key).await;
    assert_eq!(names.get("defect"), Some(&2));
    assert!(!names.contains_key("bug"));

    app.db.close().await;
}

#[tokio::test]
async fn renaming_a_tag_onto_an_existing_name_is_a_conflict() {
    // The UNIQUE index is the real guarantee; this proves it surfaces as a 409
    // that names the fix rather than as an opaque 500.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "CLASH", "blank").await;

    let bug = tag(&app, &admin, &project.key, "bug").await;
    let _defect = tag(&app, &admin, &project.key, "defect").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/tags/{bug}"),
            Some(&admin),
            json!({ "name": "defect" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
    assert!(
        reply.raw_body.contains("Merge"),
        "the 409 should point at merge, which is what the user actually wants: {}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_tag_can_be_recased_without_colliding_with_itself() {
    // Names are COLLATE NOCASE, so `bug` -> `Bug` matches the row being renamed.
    // Without the `except` clause in the taken-check, correcting your own tag's
    // capitalisation would be a 409 against itself.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "RECASE", "blank").await;

    let id = tag(&app, &admin, &project.key, "wip").await;

    let reply = app
        .send(patch(
            &format!("/api/v1/tags/{id}"),
            Some(&admin),
            json!({ "name": "WIP" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json()["name"], "WIP");

    app.db.close().await;
}

#[tokio::test]
async fn one_tag_cannot_exist_twice_in_a_project_under_different_capitalisation() {
    // Tags exist to gather cards together. A set of labels that silently splits
    // `Bug` from `bug` does the exact opposite, and the user only finds out when
    // half their cards are missing from a filter.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "NOCASE", "blank").await;

    let _ = tag(&app, &admin, &project.key, "bug").await;

    let reply = app
        .send(post(
            &format!("/api/v1/projects/{}/tags", project.key),
            Some(&admin),
            json!({ "name": "BUG" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_tag_removes_its_card_tags_rows_and_only_those() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "DEL", "blank").await;

    let doomed = tag(&app, &admin, &project.key, "doomed").await;
    let keeper = tag(&app, &admin, &project.key, "keeper").await;

    let one = card(&app, &admin, &project, "one").await;
    let two = card(&app, &admin, &project, "two").await;
    attach(&app, &admin, &one, &doomed).await;
    attach(&app, &admin, &one, &keeper).await;
    attach(&app, &admin, &two, &doomed).await;
    assert_eq!(card_tag_rows(&app).await, 3);

    let reply = app
        .send(delete(&format!("/api/v1/tags/{doomed}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    // The cascade took both of the doomed tag's rows...
    assert_eq!(card_tag_rows(&app).await, 1);
    // ...and left the other tag's alone.
    assert_eq!(card_tags(&app, &admin, &one).await, ["keeper"]);
    assert!(card_tags(&app, &admin, &two).await.is_empty());

    // The cards themselves are emphatically still here. A cascade that ran the
    // wrong way would be catastrophic and silent.
    let reply = app
        .send(get(&format!("/api/v1/cards/{two}"), Some(&admin)))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "deleting a tag must not delete cards"
    );

    app.db.close().await;
}

#[tokio::test]
async fn deleting_a_project_takes_its_tags_with_it() {
    // `tags.project_id` cascades. Without it, deleting a project would leave its
    // tags behind as unreachable rows that nothing lists and nothing can remove.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "CASC", "blank").await;

    let id = tag(&app, &admin, &project.key, "doomed").await;
    let one = card(&app, &admin, &project, "one").await;
    attach(&app, &admin, &one, &id).await;

    let reply = app
        .send(delete(
            &format!("/api/v1/projects/{}", project.key),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(app.db.reader())
        .await
        .expect("failed to count tags");
    assert_eq!(remaining, 0, "the project's tags went with it");
    assert_eq!(card_tag_rows(&app).await, 0);

    app.db.close().await;
}

#[tokio::test]
async fn deleting_a_tag_that_does_not_exist_is_a_404() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(delete(
            "/api/v1/tags/01890000-0000-7000-8000-000000000000",
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_template_seeds_the_preset_list_todo_md_documents() {
    // The lists were requested by name. This asserts them through the real
    // create-project path and the real API, so a template that silently seeds
    // nothing — or that seeds a list somebody "improved" — cannot pass.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let expected: [(&str, &str, &[&str]); 4] = [
        (
            "PROG",
            "programming",
            &[
                "bug",
                "feature",
                "refactor",
                "tech-debt",
                "docs",
                "testing",
                "ci",
                "security",
                "performance",
                "dependencies",
                "breaking-change",
                "good-first-issue",
                "blocked",
                "needs-review",
                "hotfix",
            ],
        ),
        (
            "MODEL",
            "3d-modeling",
            &[
                "modeling",
                "sculpting",
                "retopo",
                "uv-unwrap",
                "texturing",
                "rigging",
                "animation",
                "lighting",
                "rendering",
                "post-process",
                "reference",
                "wip",
                "client-review",
                "approved",
                "revision",
            ],
        ),
        (
            "JOB",
            "job-search",
            &[
                "applied",
                "phone-screen",
                "technical-interview",
                "onsite",
                "take-home",
                "offer",
                "rejected",
                "ghosted",
                "follow-up",
                "referral",
                "remote",
                "hybrid",
                "onsite-only",
                "contract",
                "permanent",
            ],
        ),
        (
            "GEN",
            "blank",
            &[
                "urgent", "blocked", "waiting", "research", "idea", "question", "admin",
            ],
        ),
    ];

    for (key, template, want) in expected {
        project(&app, &admin, key, template).await;

        let got: HashSet<String> = project_tags(&app, &admin, key).await.into_keys().collect();
        let want: HashSet<String> = want.iter().map(|n| (*n).to_owned()).collect();

        let missing: Vec<_> = want.difference(&got).collect();
        let extra: Vec<_> = got.difference(&want).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{template}: missing {missing:?}, unexpected {extra:?}"
        );
    }

    app.db.close().await;
}

#[tokio::test]
async fn every_seeded_preset_arrives_with_a_renderable_colour_and_no_usage() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // The list the frontend's Tag primitive can paint. A preset with a colour
    // outside it resolves to no CSS variable and renders an invisible chip.
    let renderable: HashSet<&str> = [
        "standard", "grey", "blue", "teal", "green", "lime", "yellow", "orange", "red", "magenta",
        "purple",
    ]
    .into_iter()
    .collect();

    for (key, template) in [
        ("P", "programming"),
        ("M", "3d-modeling"),
        ("J", "job-search"),
        ("B", "blank"),
    ] {
        project(&app, &admin, key, template).await;

        let reply = app
            .send(get(&format!("/api/v1/projects/{key}/tags"), Some(&admin)))
            .await;

        for row in rows(&reply) {
            let name = text(&row, "name");
            let colour = text(&row, "colour");
            assert!(
                renderable.contains(colour.as_str()),
                "{template}: {name:?} has colour {colour:?}, which no chip can render"
            );
            assert_eq!(
                row["usageCount"], 0,
                "{template}: {name:?} is freshly seeded and used by nothing"
            );
        }
    }

    app.db.close().await;
}

#[tokio::test]
async fn the_job_search_stage_tags_progress_grey_to_blue_to_green() {
    // The requested ramp, through the real API. The colour is the only part of a
    // chip read at a glance, and this is the one set where it carries an
    // ordering rather than a category.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    project(&app, &admin, "RAMP", "job-search").await;

    let reply = app
        .send(get("/api/v1/projects/RAMP/tags", Some(&admin)))
        .await;
    let colours: HashMap<String, String> = rows(&reply)
        .iter()
        .map(|t| (text(t, "name"), text(t, "colour")))
        .collect();

    assert_eq!(colours.get("applied").map(String::as_str), Some("grey"));
    assert_eq!(
        colours.get("phone-screen").map(String::as_str),
        Some("blue")
    );
    assert_eq!(
        colours.get("technical-interview").map(String::as_str),
        Some("blue")
    );
    assert_eq!(colours.get("offer").map(String::as_str), Some("green"));
    assert_eq!(colours.get("rejected").map(String::as_str), Some("red"));

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Scope: global tags, and other projects' tags
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_global_tag_cannot_exist_twice() {
    // This is the trap `UNIQUE (project_id, name)` does NOT catch. NULL is never
    // equal to NULL, so two rows (NULL, 'urgent') both satisfy that constraint
    // and SQLite accepts them without complaint. The partial unique index in
    // migration 0004 is what actually refuses the second — and this test is the
    // only thing standing between that index and someone "simplifying" it away.
    let app = App::new().await;

    let mut tx = app.db.begin_write().await.expect("failed to begin");
    atlas::domain::tag::insert(&mut tx, None, "urgent", None, chrono::Utc::now())
        .await
        .expect("the first global tag must insert");
    tx.commit().await.expect("failed to commit");

    let mut tx = app.db.begin_write().await.expect("failed to begin");
    let err = atlas::domain::tag::insert(&mut tx, None, "urgent", None, chrono::Utc::now())
        .await
        .expect_err("a second global 'urgent' must be refused by the database");
    drop(tx);

    assert!(
        format!("{err:?}").to_lowercase().contains("unique"),
        "expected a uniqueness violation, got: {err:?}"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_global_tag_is_offered_by_and_usable_from_every_project() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let alpha = project(&app, &admin, "GA", "blank").await;
    let beta = project(&app, &admin, "GB", "blank").await;

    // Nothing seeds globals and no endpoint creates them yet — see the module
    // note in api::tags. The read and attach paths exist and are exercised here.
    let mut tx = app.db.begin_write().await.expect("failed to begin");
    let global =
        atlas::domain::tag::insert(&mut tx, None, "company-wide", None, chrono::Utc::now())
            .await
            .expect("failed to insert a global tag");
    tx.commit().await.expect("failed to commit");

    // Both projects offer it.
    for key in [&alpha.key, &beta.key] {
        assert!(
            project_tags(&app, &admin, key)
                .await
                .contains_key("company-wide"),
            "{key} must offer the global tag"
        );
    }

    // And a card in either can carry it.
    let in_alpha = card(&app, &admin, &alpha, "alpha card").await;
    let in_beta = card(&app, &admin, &beta, "beta card").await;
    attach(&app, &admin, &in_alpha, &global.id).await;
    attach(&app, &admin, &in_beta, &global.id).await;

    assert_eq!(card_tags(&app, &admin, &in_alpha).await, ["company-wide"]);
    assert_eq!(card_tags(&app, &admin, &in_beta).await, ["company-wide"]);

    // The usage count is per project, not global: the same tag reads 1 in each,
    // not 2 in both.
    assert_eq!(
        project_tags(&app, &admin, &alpha.key)
            .await
            .get("company-wide"),
        Some(&1)
    );
    assert_eq!(
        project_tags(&app, &admin, &beta.key)
            .await
            .get("company-wide"),
        Some(&1)
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_card_cannot_be_given_another_projects_tag() {
    // The rule migration 0004 deliberately does not express: the foreign key
    // says "a tag", not "a tag this card is allowed to have". Without the check
    // in `domain::tag::attach`'s caller, a card in ALPHA could carry a tag
    // belonging to a project its owner has never opened.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let alpha = project(&app, &admin, "XA", "blank").await;
    let beta = project(&app, &admin, "XB", "blank").await;

    let theirs = tag(&app, &admin, &beta.key, "theirs").await;
    let mine = card(&app, &admin, &alpha, "mine").await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{mine}/tags"),
            Some(&admin),
            json!({ "tagId": theirs }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    assert!(card_tags(&app, &admin, &mine).await.is_empty());
    assert_eq!(card_tag_rows(&app).await, 0);

    app.db.close().await;
}

#[tokio::test]
async fn one_projects_tags_are_invisible_to_another() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let alpha = project(&app, &admin, "VA", "blank").await;
    let beta = project(&app, &admin, "VB", "blank").await;

    tag(&app, &admin, &alpha.key, "alpha-only").await;

    assert!(
        project_tags(&app, &admin, &alpha.key)
            .await
            .contains_key("alpha-only")
    );
    assert!(
        !project_tags(&app, &admin, &beta.key)
            .await
            .contains_key("alpha-only"),
        "a project tag is not a global one"
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Attach / detach
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tagging_a_card_twice_is_a_no_op_rather_than_an_error() {
    // A double-click on a chip in the picker is not a mistake worth a 409: the
    // caller's intent is "this card has `bug`", and it does.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "IDEM", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;

    attach(&app, &admin, &one, &id).await;
    attach(&app, &admin, &one, &id).await;

    assert_eq!(card_tags(&app, &admin, &one).await, ["bug"]);
    assert_eq!(card_tag_rows(&app).await, 1);

    app.db.close().await;
}

#[tokio::test]
async fn untagging_a_card_that_never_had_the_tag_is_still_a_204() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "UNTAG", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;

    let reply = app
        .send(delete(
            &format!("/api/v1/cards/{one}/tags/{id}"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    // But a missing *card* is a different question, and gets a different answer.
    let reply = app
        .send(delete(
            &format!("/api/v1/cards/{}-999/tags/{id}", project.key),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_card_key_is_case_insensitive_when_tagging() {
    // Card keys are uppercased on the way in, and every other card route accepts
    // either casing. A tag route that did not would be a URL that works from the
    // board and 404s from a pasted link.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "CASE", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let key = card(&app, &admin, &project, "one").await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{}/tags", key.to_lowercase()),
            Some(&admin),
            json!({ "tagId": id }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(card_tags(&app, &admin, &key).await, ["bug"]);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Usage counts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_soft_deleted_card_stops_counting_towards_a_tags_usage() {
    // A chip reading "2" that opens a board showing one card is worse than no
    // chip at all — and the trash is the one place a count can silently drift
    // from what the board shows.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "TRASH", "blank").await;

    let id = tag(&app, &admin, &project.key, "bug").await;
    let one = card(&app, &admin, &project, "one").await;
    let two = card(&app, &admin, &project, "two").await;
    attach(&app, &admin, &one, &id).await;
    attach(&app, &admin, &two, &id).await;

    assert_eq!(
        project_tags(&app, &admin, &project.key).await.get("bug"),
        Some(&2)
    );

    // A card delete is a *soft* delete and answers 200 with the trashed card —
    // not 204. That is `api::cards`' contract, not an accident.
    let reply = app
        .send(delete(&format!("/api/v1/cards/{two}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    assert_eq!(
        project_tags(&app, &admin, &project.key).await.get("bug"),
        Some(&1),
        "the trash does not count"
    );

    // The join row survives, though — the card is in the trash, not gone, and
    // restoring it must bring its tags back rather than a bare card.
    assert_eq!(card_tag_rows(&app).await, 2);

    app.db.close().await;
}

#[tokio::test]
async fn an_unused_tag_is_listed_with_a_count_of_zero_rather_than_omitted() {
    // The LEFT JOIN's whole purpose: a freshly seeded preset is used by nothing,
    // and it is exactly what the picker most needs to offer.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ZERO", "blank").await;

    let names = project_tags(&app, &admin, &project.key).await;
    assert_eq!(names.get("urgent"), Some(&0));
    assert_eq!(names.len(), 7, "every General preset is listed");

    app.db.close().await;
}

#[tokio::test]
async fn a_tag_whose_only_cards_are_in_the_trash_is_still_offered() {
    // The bug the `ON` clause exists to prevent, and the one an unused-tag test
    // cannot catch.
    //
    // Move `c.deleted_at IS NULL` from the join's ON clause into WHERE and this
    // is what breaks: for a tag with no cards at all the joined row is
    // null-extended, so `c.deleted_at IS NULL` is *true* and the tag survives —
    // which is why the naive test passes either way. But for a tag whose only
    // card is in the trash, the joined row is real and its deleted_at is set, so
    // WHERE drops the row and the tag vanishes from the picker entirely.
    //
    // In product terms: trash your only `hotfix` card and `hotfix` disappears,
    // so you cannot tag anything with it again. Silent, and only reachable
    // through the trash.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ONLYTRASH", "blank").await;

    let id = tag(&app, &admin, &project.key, "hotfix").await;
    let only = card(&app, &admin, &project, "the only one").await;
    attach(&app, &admin, &only, &id).await;

    let reply = app
        .send(delete(&format!("/api/v1/cards/{only}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);

    let names = project_tags(&app, &admin, &project.key).await;
    assert_eq!(
        names.get("hotfix"),
        Some(&0),
        "the tag must still be offered, at zero — not vanish with its last live card"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_global_tag_used_only_in_another_project_is_still_offered_here_at_zero() {
    // The same shape of bug, on the other filter. Move `c.project_id = ?` out of
    // the ON clause and a global tag used only by ALPHA's cards disappears from
    // BETA's picker — because BETA's query joins ALPHA's real rows and then
    // filters them out, leaving nothing to null-extend.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let alpha = project(&app, &admin, "OA", "blank").await;
    let beta = project(&app, &admin, "OB", "blank").await;

    let mut tx = app.db.begin_write().await.expect("failed to begin");
    let global =
        atlas::domain::tag::insert(&mut tx, None, "cross-cutting", None, chrono::Utc::now())
            .await
            .expect("failed to insert a global tag");
    tx.commit().await.expect("failed to commit");

    // Used in ALPHA only.
    let in_alpha = card(&app, &admin, &alpha, "alpha card").await;
    attach(&app, &admin, &in_alpha, &global.id).await;

    assert_eq!(
        project_tags(&app, &admin, &alpha.key)
            .await
            .get("cross-cutting"),
        Some(&1)
    );
    assert_eq!(
        project_tags(&app, &admin, &beta.key)
            .await
            .get("cross-cutting"),
        Some(&0),
        "BETA must still be offered the global tag, counted at zero for BETA"
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Authorisation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tag_routes_require_a_session() {
    // The whole /api/v1 tree is gated by one layer, so this is really a test
    // that the tag routes were mounted inside it rather than beside it.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let project = project(&app, &admin, "ANON", "blank").await;
    let id = tag(&app, &admin, &project.key, "bug").await;
    let key = card(&app, &admin, &project, "one").await;

    for request in [
        get("/api/v1/projects/ANON/tags", None),
        post("/api/v1/projects/ANON/tags", None, json!({ "name": "x" })),
        patch(&format!("/api/v1/tags/{id}"), None, json!({ "name": "x" })),
        delete(&format!("/api/v1/tags/{id}"), None),
        post(
            &format!("/api/v1/tags/{id}/merge"),
            None,
            json!({ "intoTagId": id }),
        ),
        get(&format!("/api/v1/cards/{key}/tags"), None),
        post(
            &format!("/api/v1/cards/{key}/tags"),
            None,
            json!({ "tagId": id }),
        ),
        delete(&format!("/api/v1/cards/{key}/tags/{id}"), None),
    ] {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "an anonymous request reached a tag route: {}",
            reply.raw_body
        );
    }

    app.db.close().await;
}

//! End-to-end tests for the GitHub integration's HTTP surface, over the real
//! router, middleware stack, and database.
//!
//! These cover the paths that do **not** need to reach `api.github.com`: the
//! guards that fire before any outbound call (no repo linked, unknown or
//! non-GitHub credential, a card with no links). The happy paths that create a
//! real branch are exercised manually against a real PAT + repo — the same
//! posture as the credential-validation tests, which also call GitHub for real.
//!
//! The project-access gating of every route (an outsider gets a 404) is proven
//! exhaustively by `every_project_scoped_route_refuses_an_outsider` in
//! `tests/project_access.rs`; here the caller is always the instance admin, who is
//! past the gate, so the assertions are about the handler logic itself.

use atlas::api::{self, AppState};
use atlas::auth::seed::{self, DEFAULT_ADMIN_USERNAME};
use atlas::auth::session;
use atlas::config::{Config, SecretString};
use atlas::db::{self, Db};
use atlas::integrations::github::store::{self, NewProjectRepo};
use atlas::secrets::Secret;
use atlas::secrets::vault::Vault;
use atlas::test_support::{TempDb, now, sign_github_webhook};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "Admin";
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";
const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A migrated, seeded database with the vault live, and the router over it.
struct App {
    db: Db,
    config: Config,
    _temp: TempDb,
}

impl App {
    async fn new() -> Self {
        let temp = TempDb::new();
        let config = Config {
            master_key: Some(SecretString::new(MASTER_KEY)),
            ..temp.config()
        };
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

    fn id(&self) -> String {
        self.json()["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id in: {}", self.raw_body))
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

fn put(uri: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    request(Method::PUT, uri, cookie, Some(body))
}

fn delete(uri: &str, cookie: Option<&str>) -> Request<Body> {
    request(Method::DELETE, uri, cookie, None)
}

/// Signs the admin in and past the forced-reset gate; returns the session cookie.
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

/// Creates a project and returns the id of its first card type.
async fn create_project(app: &App, admin: &str, key: &str) -> String {
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
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.json()[0]["id"]
        .as_str()
        .expect("a card type")
        .to_owned()
}

/// Creates a card and returns its key.
async fn create_card(app: &App, admin: &str, project_key: &str, type_id: &str) -> String {
    let reply = app
        .send(post(
            &format!("/api/v1/projects/{project_key}/cards"),
            Some(admin),
            json!({ "typeId": type_id, "summary": "Add login" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.json()["key"].as_str().expect("a card key").to_owned()
}

/// Stores a credential and returns its id.
async fn store_credential(app: &App, admin: &str, provider: &str, secret: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(admin),
            json!({ "provider": provider, "label": "test", "secret": secret }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unlinked_project_reports_no_repo_and_cannot_be_unlinked() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_project(&app, &admin, "ATLAS").await;

    let reply = app
        .send(get("/api/v1/projects/ATLAS/repo", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    // Unlinking when nothing is linked is a 404, not a silent success.
    let reply = app
        .send(delete("/api/v1/projects/ATLAS/repo", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_fresh_cards_git_links_are_an_empty_list() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    let reply = app
        .send(get(
            &format!("/api/v1/cards/{card}/git-links"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json(), json!([]), "a fresh card has no git links");

    app.db.close().await;
}

#[tokio::test]
async fn creating_a_branch_without_a_linked_repo_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    // No repo is linked to the project, so there is nothing to branch on — a 409
    // that never reaches GitHub, rather than a 500.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card}/branch"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn linking_rejects_an_unknown_or_non_github_credential() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    create_project(&app, &admin, "ATLAS").await;

    // An id that names no credential at all.
    let reply = app
        .send(put(
            "/api/v1/projects/ATLAS/repo",
            Some(&admin),
            json!({ "credentialId": "no-such-id", "owner": "octocat", "repo": "hello" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // A real credential, but for the wrong provider: it cannot drive a repo link,
    // and this is caught before any GitHub call.
    let gemini = store_credential(&app, &admin, "gemini", "ya29.not-a-github-token").await;
    let reply = app
        .send(put(
            "/api/v1/projects/ATLAS/repo",
            Some(&admin),
            json!({ "credentialId": gemini, "owner": "octocat", "repo": "hello" }),
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

#[tokio::test]
async fn the_repo_picker_rejects_a_missing_or_non_github_credential() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(get("/api/v1/credentials/no-such-id/repos", Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    // A gemini credential is not a GitHub credential, so the picker treats it as
    // absent rather than calling GitHub with it.
    let gemini = store_credential(&app, &admin, "gemini", "ya29.not-a-github-token").await;
    let reply = app
        .send(get(
            &format!("/api/v1/credentials/{gemini}/repos"),
            Some(&admin),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Webhook receiver
// ---------------------------------------------------------------------------

/// The project's id (the DTO carries it; the store queries key on it).
async fn project_id(app: &App, admin: &str, key: &str) -> String {
    app.send(get(&format!("/api/v1/projects/{key}"), Some(admin)))
        .await
        .id()
}

/// Links a repo to a project with a known webhook secret, straight in the DB — bypassing the
/// live `get_repo` call the API link path makes, which these hermetic tests cannot reach.
async fn link_repo_with_secret(app: &App, project_id: &str, repo_id: i64, secret: &str) {
    let vault = Vault::from_config(&app.config).expect("the vault is live in these tests");
    let mut tx = app.db.begin_write().await.expect("begin a write");
    let repo = store::upsert_project_repo(
        &mut tx,
        &NewProjectRepo {
            project_id,
            credential_id: None,
            owner: "octocat",
            repo: "hello",
            repo_id,
            default_branch: "main",
            branch_prefix: "feature",
        },
        now(),
    )
    .await
    .expect("link the repo");
    let sealed = vault
        .seal_for(&repo.id, &Secret::new(secret.to_owned()))
        .expect("seal the webhook secret");
    store::set_webhook(&mut tx, &repo.id, 999, &sealed, now())
        .await
        .expect("store the webhook binding");
    tx.commit().await.expect("commit the webhook link");
}

/// A raw, unauthenticated POST to the webhook receiver with GitHub's headers.
fn webhook_request(body: &str, event: &str, signature: &str) -> Request<Body> {
    webhook_request_with_delivery(body, event, signature, "test-delivery")
}

/// The same, with an explicit delivery id — for exercising the replay guard, which keys on it.
fn webhook_request_with_delivery(
    body: &str,
    event: &str,
    signature: &str,
    delivery_id: &str,
) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/webhooks/github")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-github-event", event)
        .header("x-hub-signature-256", signature)
        .header("x-github-delivery", delivery_id)
        .body(Body::from(body.to_owned()))
        .expect("failed to build the webhook request")
}

/// The category (`todo`/`in_progress`/`done`) of a card's current status, via the API.
async fn card_status_category(app: &App, admin: &str, card_key: &str) -> String {
    let card = app
        .send(get(&format!("/api/v1/cards/{card_key}"), Some(admin)))
        .await;
    let status_id = card.json()["statusId"]
        .as_str()
        .expect("the card has a status id")
        .to_owned();
    let project = card_key.rsplit_once('-').map_or(card_key, |(p, _)| p);
    let statuses = app
        .send(get(
            &format!("/api/v1/projects/{project}/statuses"),
            Some(admin),
        ))
        .await;
    statuses
        .json()
        .as_array()
        .expect("statuses are an array")
        .iter()
        .find(|status| status["id"] == status_id.as_str())
        .and_then(|status| status["category"].as_str())
        .expect("the card's status belongs to the project")
        .to_owned()
}

#[tokio::test]
async fn a_signed_push_applies_a_smart_commit() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await; // ATLAS-1
    let pid = project_id(&app, &admin, "ATLAS").await;

    let secret = "a-per-repo-webhook-secret";
    link_repo_with_secret(&app, &pid, 12345, secret).await;

    let body = json!({
        "repository": { "id": 12345 },
        "ref": "refs/heads/feature/ATLAS-1-add-login",
        "commits": [ { "id": "abc", "message": format!("{card} #done"), "url": "https://x/abc" } ]
    })
    .to_string();
    let signature = sign_github_webhook(secret.as_bytes(), body.as_bytes());

    let reply = app.send(webhook_request(&body, "push", &signature)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);

    // `#done` moved the card into a Done-category status.
    assert_eq!(card_status_category(&app, &admin, &card).await, "done");

    app.db.close().await;
}

#[tokio::test]
async fn a_delivery_signed_with_the_wrong_secret_is_refused_and_changes_nothing() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;
    let pid = project_id(&app, &admin, "ATLAS").await;
    link_repo_with_secret(&app, &pid, 12345, "the-real-secret").await;

    let body = json!({
        "repository": { "id": 12345 },
        "ref": "refs/heads/x",
        "commits": [ { "id": "abc", "message": format!("{card} #done"), "url": "" } ]
    })
    .to_string();
    // Right shape, signed under a key the server does not hold.
    let forged = sign_github_webhook(b"attacker-guess", body.as_bytes());

    let reply = app.send(webhook_request(&body, "push", &forged)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.raw_body);
    assert_ne!(
        card_status_category(&app, &admin, &card).await,
        "done",
        "an unverified delivery must never move a card"
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_merged_pull_request_moves_the_card_to_done() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await; // ATLAS-1
    let pid = project_id(&app, &admin, "ATLAS").await;

    let secret = "s";
    link_repo_with_secret(&app, &pid, 12345, secret).await;

    let body = json!({
        "repository": { "id": 12345 },
        "action": "closed",
        "pull_request": {
            "number": 7, "title": "Add login", "html_url": "https://x/7",
            "merged": true, "head": { "ref": format!("feature/{card}-add-login") }
        }
    })
    .to_string();
    let signature = sign_github_webhook(secret.as_bytes(), body.as_bytes());

    let reply = app
        .send(webhook_request(&body, "pull_request", &signature))
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);
    assert_eq!(card_status_category(&app, &admin, &card).await, "done");

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Create PR from card
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creating_a_pr_without_a_linked_repo_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card}/pr"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn creating_a_pr_before_a_branch_exists_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;
    let pid = project_id(&app, &admin, "ATLAS").await;
    link_repo_with_secret(&app, &pid, 12345, "s").await;

    // A repo is linked, but no branch has been created from this card yet.
    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card}/pr"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_pr_already_recorded_against_the_card_is_returned_without_calling_github() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await; // ATLAS-1
    let pid = project_id(&app, &admin, "ATLAS").await;
    let secret = "s";
    link_repo_with_secret(&app, &pid, 12345, secret).await;

    // Record a "pr" git link the same way the webhook receiver would — no outbound GitHub
    // call is made by this. If POST /pr subsequently tried to reach api.github.com, this
    // hermetic test would hang or fail rather than pass.
    let body = json!({
        "repository": { "id": 12345 },
        "action": "opened",
        "pull_request": {
            "number": 9, "title": "Add login", "html_url": "https://x/9",
            "merged": false, "head": { "ref": format!("feature/{card}-add-login") }
        }
    })
    .to_string();
    let signature = sign_github_webhook(secret.as_bytes(), body.as_bytes());
    let reply = app
        .send(webhook_request(&body, "pull_request", &signature))
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);

    let reply = app
        .send(post(
            &format!("/api/v1/cards/{card}/pr"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    let recorded = reply.json();
    assert_eq!(recorded["kind"], "pr");
    assert_eq!(recorded["reference"], "9");
    assert_eq!(recorded["url"], "https://x/9");

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Webhook replay guard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_redelivered_webhook_is_acknowledged_but_not_reprocessed() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await; // ATLAS-1
    let pid = project_id(&app, &admin, "ATLAS").await;
    let secret = "s";
    link_repo_with_secret(&app, &pid, 12345, secret).await;

    // `#comment` has no natural idempotence of its own (unlike a transition, which no-ops
    // once already at the target status) — a second application would visibly add a second
    // comment, which is exactly the observable the replay guard exists to prevent.
    let body = json!({
        "repository": { "id": 12345 },
        "ref": "refs/heads/x",
        "commits": [ { "id": "abc", "message": format!("{card} #comment reviewed"), "url": "" } ]
    })
    .to_string();
    let signature = sign_github_webhook(secret.as_bytes(), body.as_bytes());

    let first = app
        .send(webhook_request_with_delivery(
            &body,
            "push",
            &signature,
            "delivery-abc",
        ))
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.raw_body);

    // The exact same delivery, redelivered — same id, same signature, same body.
    let second = app
        .send(webhook_request_with_delivery(
            &body,
            "push",
            &signature,
            "delivery-abc",
        ))
        .await;
    assert_eq!(
        second.status,
        StatusCode::ACCEPTED,
        "a redelivery is still acknowledged, not rejected: {}",
        second.raw_body
    );

    let comments = app
        .send(get(&format!("/api/v1/cards/{card}/comments"), Some(&admin)))
        .await;
    assert_eq!(comments.status, StatusCode::OK, "{}", comments.raw_body);
    assert_eq!(
        comments
            .json()
            .as_array()
            .expect("comments are an array")
            .len(),
        1,
        "the redelivery must not have added a second comment"
    );

    app.db.close().await;
}

#[tokio::test]
async fn two_deliveries_with_different_ids_are_both_processed() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;
    let pid = project_id(&app, &admin, "ATLAS").await;
    let secret = "s";
    link_repo_with_secret(&app, &pid, 12345, secret).await;

    let body = |n: u32| {
        json!({
            "repository": { "id": 12345 },
            "ref": "refs/heads/x",
            "commits": [ { "id": n.to_string(), "message": format!("{card} #comment note {n}"), "url": "" } ]
        })
        .to_string()
    };

    for (n, delivery_id) in [(1u32, "delivery-1"), (2, "delivery-2")] {
        let payload = body(n);
        let signature = sign_github_webhook(secret.as_bytes(), payload.as_bytes());
        let reply = app
            .send(webhook_request_with_delivery(
                &payload,
                "push",
                &signature,
                delivery_id,
            ))
            .await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.raw_body);
    }

    let comments = app
        .send(get(&format!("/api/v1/cards/{card}/comments"), Some(&admin)))
        .await;
    assert_eq!(
        comments
            .json()
            .as_array()
            .expect("comments are an array")
            .len(),
        2,
        "two genuinely distinct deliveries must both be processed"
    );

    app.db.close().await;
}

// ---------------------------------------------------------------------------
// Card activity (live commits + CI status)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn card_activity_without_a_linked_repo_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;

    let reply = app
        .send(get(&format!("/api/v1/cards/{card}/activity"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn card_activity_before_a_branch_exists_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let type_id = create_project(&app, &admin, "ATLAS").await;
    let card = create_card(&app, &admin, "ATLAS", &type_id).await;
    let pid = project_id(&app, &admin, "ATLAS").await;
    link_repo_with_secret(&app, &pid, 12345, "s").await;

    // A repo is linked, but no branch has been created from this card yet — nothing to ask
    // GitHub for commits or CI status about.
    let reply = app
        .send(get(&format!("/api/v1/cards/{card}/activity"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    app.db.close().await;
}

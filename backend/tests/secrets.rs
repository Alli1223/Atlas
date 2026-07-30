//! End-to-end secrets-vault tests, over the real router, the real middleware
//! stack, real encryption, and a real database.
//!
//! Driven through `tower::ServiceExt::oneshot` — no TCP, no ports — so every layer
//! runs: the CSRF origin check, the session gate, the project-access layer (which
//! stands aside for these instance-admin routes), and the handler's own
//! `RequireAdmin`. The `App` harness is the one `tests/tags.rs` and `tests/domain.rs`
//! share, extended with a master key so the vault is live.
//!
//! # What these prove that the unit tests cannot
//!
//! `crate::secrets`' unit tests prove the crypto in isolation. These prove the
//! security properties *through the whole stack*, and each is a property whose
//! failure would be a credential leak rather than a bug:
//!
//! - the plaintext is never in a create/list/validate response, and never stored
//!   as cleartext in the row;
//! - a credential sealed under one master key cannot be read under another —
//!   the at-rest guarantee, end to end;
//! - a ciphertext tampered with in the database is detected on decrypt;
//! - a ciphertext moved to another row will not decrypt (AAD binding), end to end;
//! - the vault is admin-only, and refuses to store when no master key is set;
//! - `last_four` reveals four characters and no more.

use atlas::api::{self, AppState};
use atlas::auth::seed::{self, DEFAULT_ADMIN_USERNAME};
use atlas::auth::session;
use atlas::config::{Config, SecretString};
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The seeded admin credentials every test starts from.
const ADMIN_PASSWORD: &str = "Admin";

/// A password that satisfies the policy, for the admin and any extra users.
const GOOD_PASSWORD: &str = "a perfectly fine passphrase";

/// Two valid base64 master keys. HKDF makes the length irrelevant; these decode
/// to arbitrary bytes and derive two independent vault keys.
const MASTER_KEY_A: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";
const MASTER_KEY_B: &str = "YS10b3RhbGx5LWRpZmZlcmVudC0zMi1ieXRlLW1hc3Rlcmtl";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A migrated, seeded database and a master key, with the router over them.
struct App {
    db: Db,
    config: Config,
    _temp: TempDb,
}

impl App {
    /// An instance with the vault live (master key A set).
    async fn new() -> Self {
        Self::with_master_key(Some(MASTER_KEY_A)).await
    }

    /// An instance with a chosen master key, or none at all.
    async fn with_master_key(key: Option<&str>) -> Self {
        let temp = TempDb::new();
        let config = Config {
            master_key: key.map(SecretString::new),
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

    /// A fresh router over the current config. Rebuilt per request because
    /// `oneshot` consumes it.
    fn router(&self) -> Router {
        api::router(AppState::new(self.db.clone(), self.config.clone()))
    }

    /// A router over the same database but a different master key — for the
    /// key-rotation / wrong-key tests. The session table is shared, so a cookie
    /// obtained under one key still authenticates under the other.
    fn router_with_master_key(&self, key: Option<&str>) -> Router {
        let config = Config {
            master_key: key.map(SecretString::new),
            ..self.config.clone()
        };
        api::router(AppState::new(self.db.clone(), config))
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        Self::send_to(self.router(), request).await
    }

    async fn send_to(router: Router, request: Request<Body>) -> Reply {
        let response = router.oneshot(request).await.expect("request failed");
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

/// Creates a non-admin member and returns their session cookie.
async fn member_cookie(app: &App, admin: &str, username: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/users",
            Some(admin),
            json!({
                "username": username,
                "password": GOOD_PASSWORD,
                "role": "member",
                "mustChangePassword": false,
            }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    let reply = app
        .send(post(
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": GOOD_PASSWORD }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    reply.session_cookie().expect("member login sets a cookie")
}

/// Stores a credential and returns its id.
async fn store(app: &App, admin: &str, provider: &str, label: &str, secret: &str) -> String {
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(admin),
            json!({ "provider": provider, "label": label, "secret": secret }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);
    reply.id()
}

/// The raw `(ciphertext, nonce)` BLOBs of a stored row, read straight from the DB.
async fn stored_blobs(db: &Db, id: &str) -> (Vec<u8>, Vec<u8>) {
    sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>(
        "SELECT ciphertext, nonce FROM api_credentials WHERE id = ?",
    )
    .bind(id)
    .fetch_one(db.reader())
    .await
    .expect("the row must exist")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_admin_can_store_list_and_delete_a_credential() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // Empty to begin with.
    let reply = app.send(get("/api/v1/credentials", Some(&admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert_eq!(reply.json().as_array().expect("array").len(), 0);

    let id = store(&app, &admin, "github", "work PAT", "ghp_1234567890abcd").await;

    let reply = app.send(get("/api/v1/credentials", Some(&admin))).await;
    let rows = reply.json();
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"], id);
    assert_eq!(row["provider"], "github");
    assert_eq!(row["label"], "work PAT");
    assert_eq!(row["lastFour"], "abcd");
    assert_eq!(row["status"], "unchecked");
    assert_eq!(row["scopes"], json!([]));

    // Delete it.
    let reply = app
        .send(delete(&format!("/api/v1/credentials/{id}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.raw_body);

    let reply = app.send(get("/api/v1/credentials", Some(&admin))).await;
    assert_eq!(reply.json().as_array().expect("array").len(), 0);

    // Deleting again is a 404.
    let reply = app
        .send(delete(&format!("/api/v1/credentials/{id}"), Some(&admin)))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn the_secret_never_appears_in_a_response_nor_as_cleartext_in_the_row() {
    // The property the whole module exists for. A distinctive plaintext, hunted
    // for in every place it could leak: the create response, the list response,
    // and the stored bytes.
    let secret = "ghp_UNIQUEsecretVALUE0987654321";
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let create = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "github", "label": "leak-check", "secret": secret }),
        ))
        .await;
    assert_eq!(create.status, StatusCode::CREATED, "{}", create.raw_body);
    assert!(
        !create.raw_body.contains(secret),
        "the secret leaked into the create response: {}",
        create.raw_body
    );
    let id = create.id();

    let list = app.send(get("/api/v1/credentials", Some(&admin))).await;
    assert!(
        !list.raw_body.contains(secret),
        "the secret leaked into the list response: {}",
        list.raw_body
    );

    // And it is genuinely encrypted at rest: the plaintext bytes appear nowhere
    // in the stored ciphertext, and every stored text column is free of it too.
    let (ciphertext, _nonce) = stored_blobs(&app.db, &id).await;
    assert!(
        !ciphertext
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()),
        "the plaintext is present in the stored ciphertext"
    );

    let row_text: String = sqlx::query_scalar(
        "SELECT provider || '|' || label || '|' || last_four || '|' || status \
         FROM api_credentials WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(app.db.reader())
    .await
    .expect("row exists");
    assert!(
        !row_text.contains(secret),
        "the plaintext is present in a text column: {row_text}"
    );

    app.db.close().await;
}

#[tokio::test]
async fn last_four_is_the_last_four_characters_and_nothing_more() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    store(
        &app,
        &admin,
        "anthropic",
        "claude",
        "sk-ant-api03-XYZ-tail9WXYZ",
    )
    .await;

    let reply = app.send(get("/api/v1/credentials", Some(&admin))).await;
    let rows = reply.json();
    let last_four = rows.as_array().expect("array")[0]["lastFour"]
        .as_str()
        .expect("lastFour is a string")
        .to_owned();
    assert_eq!(last_four, "WXYZ");
    assert_eq!(last_four.chars().count(), 4);

    app.db.close().await;
}

#[tokio::test]
async fn the_vault_is_admin_only() {
    // The routes are Unscoped in the project-access layer, so the guard that
    // stops a member is the handler's RequireAdmin — a 403, on every verb.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = store(&app, &admin, "github", "admins-only", "ghp_secrettoken1234").await;
    let member = member_cookie(&app, &admin, "member1").await;

    for request in [
        get("/api/v1/credentials", Some(&member)),
        post(
            "/api/v1/credentials",
            Some(&member),
            json!({ "provider": "gemini", "label": "sneaky", "secret": "ya29.sneakysecret" }),
        ),
        delete(&format!("/api/v1/credentials/{id}"), Some(&member)),
        post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&member),
            json!({}),
        ),
    ] {
        let reply = app.send(request).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "a member must be forbidden: {}",
            reply.raw_body
        );
    }

    // And an anonymous caller gets 401, not 403.
    let reply = app.send(get("/api/v1/credentials", None)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn a_duplicate_provider_and_label_is_a_conflict() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    store(&app, &admin, "github", "dup", "ghp_firsttoken1234").await;

    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "github", "label": "dup", "secret": "ghp_secondtoken5678" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);

    // The same label under a *different* provider is fine — the uniqueness is per
    // (provider, label).
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "gemini", "label": "dup", "secret": "ya29.differentsecret" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn validation_updates_metadata_without_ever_returning_the_secret() {
    // This exercises the provider-agnostic decrypt → probe → persist plumbing, so it
    // targets a provider whose probe is still the no-op (`gemini`): `github` now
    // routes to a real validator that would reach the network.
    let secret = "ya29.VALIDATIONsecret5555";
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = store(&app, &admin, "gemini", "to-validate", secret).await;

    let reply = app
        .send(post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw_body);
    assert!(
        !reply.raw_body.contains(secret),
        "validate leaked the secret: {}",
        reply.raw_body
    );

    // The no-op probe stamps last_validated_at even though it reports 'unchecked':
    // the decrypt → probe → persist path ran end to end.
    let body = reply.json();
    assert!(
        !body["lastValidatedAt"].is_null(),
        "last_validated_at must be set: {body}"
    );
    assert_eq!(body["status"], "unchecked");

    app.db.close().await;
}

#[tokio::test]
async fn a_credential_sealed_under_one_key_cannot_be_read_under_another() {
    // The at-rest guarantee, proved through the whole stack: seal under master key
    // A, then serve the SAME database with master key B and try to open it. The
    // AEAD authentication fails, so validate is a 500 — a stolen database plus the
    // wrong key is useless. (Session cookies do not depend on the vault, so the
    // admin's cookie still authenticates under key B.)
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = store(&app, &admin, "github", "wrong-key", "ghp_sealedundera1234").await;

    // Under the correct key, validate succeeds.
    let ok = App::send_to(
        app.router_with_master_key(Some(MASTER_KEY_A)),
        post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&admin),
            json!({}),
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.raw_body);

    // Under the wrong key, it cannot be decrypted: 500, and nothing leaks.
    let wrong = App::send_to(
        app.router_with_master_key(Some(MASTER_KEY_B)),
        post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&admin),
            json!({}),
        ),
    )
    .await;
    assert_eq!(
        wrong.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        wrong.raw_body
    );
    assert!(!wrong.raw_body.contains("ghp_"), "{}", wrong.raw_body);

    app.db.close().await;
}

#[tokio::test]
async fn tampering_with_the_stored_ciphertext_is_detected() {
    // The Poly1305 tag, end to end: flip one byte of the stored ciphertext and the
    // next decrypt must fail rather than return mangled bytes.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let id = store(&app, &admin, "github", "tamper", "ghp_tamperme12345678").await;

    let (mut ciphertext, _nonce) = stored_blobs(&app.db, &id).await;
    ciphertext[0] ^= 0x01;
    sqlx::query("UPDATE api_credentials SET ciphertext = ? WHERE id = ?")
        .bind(&ciphertext)
        .bind(&id)
        .execute(app.db.writer())
        .await
        .expect("the tampering update must apply");

    let reply = app
        .send(post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a tampered ciphertext must not decrypt: {}",
        reply.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn a_ciphertext_moved_to_another_row_will_not_decrypt() {
    // The AAD binding, proved through the whole stack. Each row's ciphertext is
    // bound to that row's id. Copy row TWO's sealed bytes onto row ONE and row
    // ONE can no longer be opened — the tag was computed over row TWO's id, which
    // is not the id row ONE decrypts with. Without AAD binding this swap would
    // silently succeed and hand row ONE row TWO's secret.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    let one = store(&app, &admin, "github", "one", "ghp_secretNUMBERone11").await;
    let two = store(&app, &admin, "anthropic", "two", "sk-ant-secretNUMBERtwo22").await;

    // Sanity: row ONE opens fine before the swap.
    let before = app
        .send(post(
            &format!("/api/v1/credentials/{one}/validate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.raw_body);

    let (two_ct, two_nonce) = stored_blobs(&app.db, &two).await;
    sqlx::query("UPDATE api_credentials SET ciphertext = ?, nonce = ? WHERE id = ?")
        .bind(&two_ct)
        .bind(&two_nonce)
        .bind(&one)
        .execute(app.db.writer())
        .await
        .expect("the swap must apply");

    let after = app
        .send(post(
            &format!("/api/v1/credentials/{one}/validate"),
            Some(&admin),
            json!({}),
        ))
        .await;
    assert_eq!(
        after.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a ciphertext bound to another row must not decrypt here: {}",
        after.raw_body
    );

    app.db.close().await;
}

#[tokio::test]
async fn without_a_master_key_the_vault_refuses_to_store() {
    // A dev instance with no ATLAS_MASTER_KEY: there is no key to encrypt with, so
    // create refuses (500) rather than storing a secret it cannot protect. Listing
    // still works — it needs no key — proving the refusal is specific to writes.
    let app = App::with_master_key(None).await;
    let admin = admin_past_the_gate(&app).await;

    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "github", "label": "nokey", "secret": "ghp_wouldbestored123" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        reply.raw_body
    );
    assert!(!reply.raw_body.contains("ghp_"), "{}", reply.raw_body);

    // Nothing was written.
    let list = app.send(get("/api/v1/credentials", Some(&admin))).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.raw_body);
    assert_eq!(list.json().as_array().expect("array").len(), 0);

    app.db.close().await;
}

#[tokio::test]
async fn an_invalid_label_or_secret_is_a_422() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    // Empty label.
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "github", "label": "   ", "secret": "ghp_fine1234567890" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // Empty secret.
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "github", "label": "ok", "secret": "" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );

    // Unknown provider.
    let reply = app
        .send(post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "gitlab", "label": "ok", "secret": "ghp_fine1234567890" }),
        ))
        .await;
    assert!(
        reply.status == StatusCode::UNPROCESSABLE_ENTITY || reply.status == StatusCode::BAD_REQUEST,
        "unknown provider must be rejected: {} {}",
        reply.status,
        reply.raw_body
    );

    app.db.close().await;
}

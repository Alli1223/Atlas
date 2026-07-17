//! Server-side sessions, and the cookie that carries them.
//!
//! # Why this is hand-rolled
//!
//! `tower-sessions` is the obvious candidate and cannot be used: its SQLite
//! store (`tower-sessions-sqlx-store` 0.15.0, last touched 2025-01) pins
//! `sqlx ^0.8` *and* `tower-sessions-core ^0.14`, while `tower-sessions` 0.15
//! pins core `=0.15.0`. The two cannot co-resolve, and neither works against
//! sqlx 0.9. See `docs/research/rust-stack.md`. The store below is the ~100
//! lines that replace it.
//!
//! # The token and the row are not the same value
//!
//! The cookie carries 256 bits of CSPRNG output. The database stores the
//! **SHA-256 of that token**, and nothing else. So:
//!
//! - a dump of `sessions` yields no usable credential — inverting SHA-256 is the
//!   attacker's problem, exactly as with `password_hash`;
//! - a session id shown in the UI (`GET /auth/sessions`) is the digest, which is
//!   safe to display for the same reason;
//! - there is no fast hash-vs-password tradeoff to make: the input is already
//!   256 uniform bits, so a memory-hard KDF would only add latency to every
//!   authenticated request while buying nothing against a brute-forcer who
//!   cannot enumerate 2^256 anyway.

use std::time::Duration as StdDuration;

use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use subtle::ConstantTimeEq;
use utoipa::ToSchema;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

/// The session cookie's name.
pub const COOKIE_NAME: &str = "atlas_session";

/// Session token entropy, in bytes. 256 bits.
const TOKEN_BYTES: usize = 32;

/// How long a session may live, however active it is.
///
/// Idle timeout alone is not enough: a session that is refreshed every day would
/// live forever, so a stolen cookie would too.
pub const ABSOLUTE_MAX_AGE: Duration = Duration::days(30);

/// How long a session survives without a request.
pub const IDLE_TIMEOUT: Duration = Duration::days(7);

/// How stale `last_seen_at` must be before a request rewrites it.
///
/// Without this, every authenticated request is a write, and the single-writer
/// pool turns a page of parallel API calls into a queue. A minute of drift on a
/// seven-day idle window is not worth that.
const TOUCH_INTERVAL: Duration = Duration::minutes(1);

/// A row of `sessions`.
///
/// `id` is the digest, never the token — see the module docs.
#[derive(Debug, Clone, FromRow)]
pub struct Session {
    /// SHA-256 hex digest of the session token.
    pub id: String,
    /// The owning user.
    pub user_id: String,
    /// When the session was created. Fixes the absolute expiry.
    pub created_at: DateTime<Utc>,
    /// When the session was last used. Drives the idle window.
    pub last_seen_at: DateTime<Utc>,
    /// The absolute expiry, set at creation and never extended.
    pub expires_at: DateTime<Utc>,
    /// The creating client's `User-Agent`, for the session list.
    pub user_agent: Option<String>,
    /// The creating client's IP, for the session list.
    pub ip: Option<String>,
}

impl Session {
    /// Whether the session is still usable at `now`.
    ///
    /// Both windows must hold: the absolute cap *and* the idle timeout.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at && now < self.last_seen_at + IDLE_TIMEOUT
    }
}

/// A session as the API describes it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    /// The session's id — the token's digest, safe to show and to revoke by.
    pub id: String,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last used.
    pub last_seen_at: DateTime<Utc>,
    /// When the session expires regardless of activity.
    pub expires_at: DateTime<Utc>,
    /// The creating client's `User-Agent`, if it sent one.
    pub user_agent: Option<String>,
    /// The creating client's IP, if it could be determined.
    pub ip: Option<String>,
    /// Whether this is the session making the request — so the UI can say
    /// "this device" and warn before revoking it.
    pub current: bool,
}

impl SessionDto {
    /// Renders a session, marking whether it is `current_id`.
    pub fn from_session(session: &Session, current_id: &str) -> Self {
        Self {
            id: session.id.clone(),
            created_at: session.created_at,
            last_seen_at: session.last_seen_at,
            expires_at: session.expires_at,
            user_agent: session.user_agent.clone(),
            ip: session.ip.clone(),
            current: session.id == current_id,
        }
    }
}

/// A freshly minted session and the token that addresses it.
///
/// The token is returned exactly once, here, and is never recoverable from the
/// database afterwards.
#[derive(Debug)]
pub struct IssuedSession {
    /// The stored row.
    pub session: Session,
    /// The secret to put in the cookie.
    pub token: String,
}

/// Generates a session token: 256 bits of OS entropy, base64url, unpadded.
///
/// `OsRng` here is argon2's re-export (`rand_core` 0.6). Atlas does not depend on
/// `rand` at all, and adding it for this would mean two incompatible `OsRng`
/// types in the tree for no benefit — this one is the operating system's
/// CSPRNG either way.
fn new_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};

    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The database id for a token: its SHA-256, hex-encoded.
fn digest(token: &str) -> String {
    let hash = Sha256::digest(token.as_bytes());
    // Lowercase hex; 64 characters.
    hash.iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write as _;
            // Writing to a String is infallible.
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Creates a session for `user_id`.
pub async fn create(
    db: &Db,
    user_id: &str,
    user_agent: Option<&str>,
    ip: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<IssuedSession> {
    let token = new_token();
    let id = digest(&token);
    let expires_at = now + ABSOLUTE_MAX_AGE;

    sqlx::query(
        "INSERT INTO sessions (id, user_id, created_at, last_seen_at, expires_at, user_agent, ip) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(to_sql_timestamp(now))
    .bind(to_sql_timestamp(now))
    .bind(to_sql_timestamp(expires_at))
    .bind(user_agent)
    .bind(ip)
    .execute(db.writer())
    .await?;

    Ok(IssuedSession {
        session: Session {
            id,
            user_id: user_id.to_owned(),
            created_at: now,
            last_seen_at: now,
            expires_at,
            user_agent: user_agent.map(ToOwned::to_owned),
            ip: ip.map(ToOwned::to_owned),
        },
        token,
    })
}

/// Loads the session a token addresses, if it exists and is still valid.
///
/// An expired session is deleted on the way past: the row is dead, the client
/// will keep presenting it until the cookie's own `Max-Age` lapses, and cleaning
/// it up here means the table does not need a scheduled sweep to stay small.
pub async fn load(db: &Db, token: &str, now: DateTime<Utc>) -> AppResult<Option<Session>> {
    let id = digest(token);

    let Some(session) = sqlx::query_as::<_, Session>(
        "SELECT id, user_id, created_at, last_seen_at, expires_at, user_agent, ip \
         FROM sessions WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(db.reader())
    .await?
    else {
        return Ok(None);
    };

    // Belt and braces. The index lookup above already matched, and it is not
    // constant time — but what it compared is a SHA-256 digest, and no amount of
    // timing on a B-tree descent yields a preimage of one. This re-check is here
    // so that the *comparison of a secret-derived value* is constant time by
    // construction, and stays that way if the lookup above is ever changed to
    // something where timing does matter.
    if !bool::from(session.id.as_bytes().ct_eq(id.as_bytes())) {
        return Ok(None);
    }

    if !session.is_valid_at(now) {
        delete(db, &session.id).await?;
        return Ok(None);
    }

    Ok(Some(session))
}

/// Refreshes a session's idle window, at most once per [`TOUCH_INTERVAL`].
///
/// Returns the instant `last_seen_at` now holds, so the caller does not have to
/// guess whether the write happened.
pub async fn touch(db: &Db, session: &Session, now: DateTime<Utc>) -> AppResult<DateTime<Utc>> {
    if now - session.last_seen_at < TOUCH_INTERVAL {
        return Ok(session.last_seen_at);
    }

    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now))
        .bind(&session.id)
        .execute(db.writer())
        .await?;

    Ok(now)
}

/// Every session for a user, newest first.
pub async fn list_for_user(db: &Db, user_id: &str) -> AppResult<Vec<Session>> {
    Ok(sqlx::query_as::<_, Session>(
        "SELECT id, user_id, created_at, last_seen_at, expires_at, user_agent, ip \
         FROM sessions WHERE user_id = ? ORDER BY last_seen_at DESC",
    )
    .bind(user_id)
    .fetch_all(db.reader())
    .await?)
}

/// Revokes one session by id.
pub async fn delete(db: &Db, id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Revokes one session, but only if it belongs to `user_id`.
///
/// The ownership check is in the `WHERE` clause rather than a separate read:
/// a fetch-then-delete would be two statements to get wrong, and this way a
/// request to revoke someone else's session is indistinguishable from a request
/// to revoke one that does not exist — which is what it should be.
pub async fn delete_owned(db: &Db, id: &str, user_id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Revokes every session for a user.
///
/// Used on password change and on deactivation — the two moments where "log this
/// account out everywhere, now" is the whole point of having server-side
/// sessions at all.
pub async fn delete_all_for_user(tx: &mut sqlx::SqliteConnection, user_id: &str) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes sessions whose absolute expiry has passed.
///
/// Housekeeping only. The authoritative check is [`Session::is_valid_at`], which
/// runs in Rust on every request; this just stops the table growing without
/// bound. Comparing timestamps as text is sound here because
/// [`to_sql_timestamp`] is fixed-width.
pub async fn purge_expired(db: &Db, now: DateTime<Utc>) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
        .bind(to_sql_timestamp(now))
        .execute(db.writer())
        .await?;
    Ok(result.rows_affected())
}

/// Builds the session cookie.
///
/// The attributes, and why each one:
///
/// - **`HttpOnly`** — JavaScript cannot read it, so an XSS cannot exfiltrate the
///   session. This is the entire reason Atlas does not put a token in
///   `localStorage`.
/// - **`SameSite=Lax`** — the browser does not attach the cookie to a cross-site
///   `POST`, which kills classic CSRF outright. `Strict` would additionally
///   break following a link into Atlas from anywhere else (email, GitHub, a
///   card link in Slack) by logging the user out on arrival.
/// - **`Secure`** — set from `secure`, which is `true` in prod and `false` in
///   dev. It cannot be unconditionally true: dev serves plain HTTP on
///   localhost, and a `Secure` cookie is silently dropped there, which presents
///   as "login does nothing" with no error anywhere.
/// - **`Path=/`** — the API and the SPA share an origin in the single-binary
///   deploy.
/// - **`Max-Age`** — [`ABSOLUTE_MAX_AGE`], matching the row's own cap. The
///   *idle* window is enforced server-side rather than by the cookie, because
///   refreshing a cookie's `Max-Age` means re-sending `Set-Cookie` on every
///   response, and the server has to enforce it anyway (a client can keep
///   sending an expired cookie for as long as it likes).
///
/// No signing or encryption: `PrivateCookieJar` would encrypt a value that is
/// already 256 uniform random bits and carries no information. It would buy
/// nothing and would add a key to `AppState` that must be persisted across
/// restarts or every session dies on deploy.
pub fn cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, token))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(
            time::Duration::try_from(StdDuration::from_secs(
                ABSOLUTE_MAX_AGE.num_seconds().unsigned_abs(),
            ))
            .unwrap_or(time::Duration::DAY * 30),
        )
        .build()
}

/// Builds the cookie that removes the session cookie.
///
/// The attributes must match [`cookie`]'s `Path` (and `Domain`, if one were
/// ever set) or the browser adds a *second* cookie instead of replacing the
/// first, and the user stays logged in with a cookie nothing can reach.
pub fn removal_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::ZERO)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::role::Role;
    use crate::auth::user::{self, NewUser};
    use crate::db::migrate;
    use crate::test_support::TempDb;

    async fn db_with_user() -> (Db, TempDb, String) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let user = user::insert(
            &mut tx,
            &NewUser {
                username: "someone".to_owned(),
                email: None,
                display_name: "Someone".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, user.id)
    }

    #[test]
    fn tokens_are_256_bits_and_never_repeat() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        // base64url of 32 bytes, unpadded.
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), TOKEN_BYTES);
        assert!(!a.contains('='), "the token must be URL-safe and unpadded");
        assert!(!a.contains('+') && !a.contains('/'), "{a}");
    }

    #[test]
    fn the_digest_is_sha256_hex_and_is_not_the_token() {
        let token = new_token();
        let id = digest(&token);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, token);
        // Deterministic, or a session could never be found twice.
        assert_eq!(digest(&token), id);
        // A known vector, so a swap to a different hash is caught.
        assert_eq!(
            digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn the_stored_row_never_contains_the_token() {
        // The claim the module docs make, asserted against the real table: if
        // someone "simplifies" create() to store the token, this fails.
        let (db, _temp, user_id) = db_with_user().await;
        let issued = create(
            &db,
            &user_id,
            Some("curl"),
            Some("10.0.0.1"),
            crate::auth::now(),
        )
        .await
        .unwrap();

        let stored: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_all(db.reader())
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_ne!(stored[0], issued.token);
        assert_eq!(stored[0], digest(&issued.token));

        db.close().await;
    }

    #[tokio::test]
    async fn a_session_loads_by_its_token_and_only_its_token() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let issued = create(&db, &user_id, None, None, now).await.unwrap();

        let loaded = load(&db, &issued.token, now).await.unwrap().unwrap();
        assert_eq!(loaded.id, issued.session.id);
        assert_eq!(loaded.user_id, user_id);

        // A different token, and the digest itself, must both fail: presenting
        // the id from `GET /auth/sessions` must not be a login.
        assert!(load(&db, &new_token(), now).await.unwrap().is_none());
        assert!(load(&db, &issued.session.id, now).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn a_session_dies_at_the_idle_timeout() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let issued = create(&db, &user_id, None, None, now).await.unwrap();

        // Just inside the window.
        let inside = now + IDLE_TIMEOUT - Duration::minutes(1);
        assert!(load(&db, &issued.token, inside).await.unwrap().is_some());

        // Just outside it.
        let outside = now + IDLE_TIMEOUT + Duration::minutes(1);
        assert!(load(&db, &issued.token, outside).await.unwrap().is_none());

        // ...and the dead row was cleaned up rather than left to rot.
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        db.close().await;
    }

    #[tokio::test]
    async fn a_session_dies_at_the_absolute_cap_however_active_it_is() {
        // The case idle timeout alone misses: a session used every day forever.
        let (db, _temp, user_id) = db_with_user().await;
        let start = crate::auth::now();
        let issued = create(&db, &user_id, None, None, start).await.unwrap();

        // Keep it warm right up to the cap.
        let mut session = issued.session.clone();
        let mut at = start;
        for _ in 0..30 {
            at += Duration::days(1);
            let seen = touch(&db, &session, at).await.unwrap();
            session.last_seen_at = seen;
            assert_eq!(seen, at, "a day is well past the touch interval");
        }

        // Active every single day, and still dead at 30 days.
        let past_cap = start + ABSOLUTE_MAX_AGE + Duration::minutes(1);
        assert!(load(&db, &issued.token, past_cap).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn touch_is_throttled_so_reads_do_not_become_writes() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let issued = create(&db, &user_id, None, None, now).await.unwrap();

        // Within the interval: no write, and the reported instant is unchanged.
        let seen = touch(&db, &issued.session, now + Duration::seconds(5))
            .await
            .unwrap();
        assert_eq!(seen, issued.session.last_seen_at);
        let stored = load(&db, &issued.token, now).await.unwrap().unwrap();
        assert_eq!(stored.last_seen_at, issued.session.last_seen_at);

        // Past it: the write happens.
        let later = now + TOUCH_INTERVAL + Duration::seconds(1);
        let seen = touch(&db, &issued.session, later).await.unwrap();
        assert_eq!(seen, later);
        let stored = load(&db, &issued.token, later).await.unwrap().unwrap();
        assert_eq!(stored.last_seen_at, later);

        db.close().await;
    }

    #[tokio::test]
    async fn a_revoked_session_stops_loading_immediately() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let issued = create(&db, &user_id, None, None, now).await.unwrap();

        assert!(delete(&db, &issued.session.id).await.unwrap());
        assert!(load(&db, &issued.token, now).await.unwrap().is_none());
        // Revoking twice is not an error, but does report that nothing happened.
        assert!(!delete(&db, &issued.session.id).await.unwrap());

        db.close().await;
    }

    #[tokio::test]
    async fn delete_owned_refuses_someone_elses_session() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let issued = create(&db, &user_id, None, None, now).await.unwrap();

        assert!(
            !delete_owned(&db, &issued.session.id, "a-different-user")
                .await
                .unwrap()
        );
        assert!(
            load(&db, &issued.token, now).await.unwrap().is_some(),
            "the session must survive another user's revoke attempt"
        );

        assert!(
            delete_owned(&db, &issued.session.id, &user_id)
                .await
                .unwrap()
        );
        assert!(load(&db, &issued.token, now).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn deleting_all_sessions_for_a_user_logs_every_device_out() {
        let (db, _temp, user_id) = db_with_user().await;
        let now = crate::auth::now();
        let a = create(&db, &user_id, Some("laptop"), None, now)
            .await
            .unwrap();
        let b = create(&db, &user_id, Some("phone"), None, now)
            .await
            .unwrap();

        let mut tx = db.begin_write().await.unwrap();
        assert_eq!(delete_all_for_user(&mut tx, &user_id).await.unwrap(), 2);
        tx.commit().await.unwrap();

        assert!(load(&db, &a.token, now).await.unwrap().is_none());
        assert!(load(&db, &b.token, now).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn sessions_die_with_their_user() {
        // The ON DELETE CASCADE. Atlas never hard-deletes a user, but if it ever
        // did, orphan sessions pointing at nobody would be worse.
        let (db, _temp, user_id) = db_with_user().await;
        create(&db, &user_id, None, None, crate::auth::now())
            .await
            .unwrap();

        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(&user_id)
            .execute(db.writer())
            .await
            .unwrap();

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        db.close().await;
    }

    #[tokio::test]
    async fn purge_removes_only_the_expired() {
        let (db, _temp, user_id) = db_with_user().await;
        let old = crate::auth::now() - ABSOLUTE_MAX_AGE - Duration::days(1);
        let fresh = crate::auth::now();
        create(&db, &user_id, Some("old"), None, old).await.unwrap();
        let live = create(&db, &user_id, Some("fresh"), None, fresh)
            .await
            .unwrap();

        assert_eq!(purge_expired(&db, fresh).await.unwrap(), 1);
        assert!(load(&db, &live.token, fresh).await.unwrap().is_some());

        db.close().await;
    }

    #[test]
    fn the_cookie_carries_every_attribute_that_makes_it_safe() {
        let c = cookie("a-token".to_owned(), true);
        assert_eq!(c.name(), COOKIE_NAME);
        assert_eq!(c.value(), "a-token");
        assert_eq!(
            c.http_only(),
            Some(true),
            "an XSS must not be able to read it"
        );
        assert_eq!(c.secure(), Some(true));
        assert_eq!(
            c.same_site(),
            Some(SameSite::Lax),
            "Lax is what blocks CSRF"
        );
        assert_eq!(c.path(), Some("/"));
        assert_eq!(c.max_age(), Some(time::Duration::days(30)));
    }

    #[test]
    fn the_cookie_is_not_secure_in_dev_but_is_everything_else() {
        // A Secure cookie on http://localhost is silently dropped by the
        // browser, which presents as "login does nothing".
        let c = cookie("a-token".to_owned(), false);
        assert_eq!(c.secure(), Some(false));
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Lax));
    }

    #[test]
    fn the_removal_cookie_matches_the_real_one_so_it_replaces_it() {
        // A removal cookie with a different Path adds a second cookie instead of
        // clearing the first, and the user stays logged in forever.
        let real = cookie("a-token".to_owned(), true);
        let removal = removal_cookie(true);
        assert_eq!(removal.name(), real.name());
        assert_eq!(removal.path(), real.path());
        assert_eq!(removal.same_site(), real.same_site());
        assert_eq!(removal.value(), "");
        assert_eq!(removal.max_age(), Some(time::Duration::ZERO));
    }
}

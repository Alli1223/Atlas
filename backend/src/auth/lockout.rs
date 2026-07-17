//! Login failure counters and lockout, per username and per IP.
//!
//! # The shape of the defence
//!
//! Two independent counters guard a login:
//!
//! - **per username** — stops an attacker grinding one account's password from
//!   a botnet, where every request comes from a different address;
//! - **per IP** — stops one host spraying one password across every account,
//!   where no single username ever accumulates failures.
//!
//! Neither catches the other's case, which is why there are two.
//!
//! # Counters exist for usernames that do not
//!
//! A counter is created for *any* attempted username, existing or not. Skipping
//! the bookkeeping for unknown usernames would be the obvious optimisation and
//! would reintroduce the very oracle that
//! [`crate::auth::password::verify_dummy`] exists to close: "this username locks
//! out, therefore it exists".

use chrono::{DateTime, Duration, Utc};
use sqlx::FromRow;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

/// Failures within [`WINDOW`] before the key locks.
pub const MAX_FAILURES: i64 = 10;

/// How far back failures count.
///
/// Outside it the counter resets, so a typo last month never contributes to
/// today's lockout.
pub const WINDOW: Duration = Duration::minutes(15);

/// The first lock's duration. Doubles per failure beyond [`MAX_FAILURES`].
pub const BASE_LOCK: Duration = Duration::minutes(15);

/// The longest a key can be locked.
///
/// Unbounded doubling would turn a burst of failures into a permanent
/// self-inflicted denial of service — an attacker who cannot get in can still
/// lock the real user out forever, which is a worse outcome than a slow
/// password grind against Argon2id.
pub const MAX_LOCK: Duration = Duration::hours(24);

/// A row of `login_attempts`.
#[derive(Debug, Clone, FromRow)]
pub struct Attempts {
    /// `user:<lowercased username>` or `ip:<address>`.
    pub key: String,
    /// Failures inside the current window.
    pub failures: i64,
    /// When the current window opened.
    pub first_failure_at: DateTime<Utc>,
    /// When the lock lifts, if the key is locked.
    pub locked_until: Option<DateTime<Utc>>,
}

/// The counter key for a username.
///
/// Lowercased, because usernames are case-insensitive: without this, `Admin`,
/// `admin` and `ADMIN` would be three counters for one account and the threshold
/// would be three times what it says.
pub fn user_key(username: &str) -> String {
    format!("user:{}", username.to_lowercase())
}

/// The counter key for an IP.
pub fn ip_key(ip: &str) -> String {
    format!("ip:{ip}")
}

/// How long `key` is locked for at `now`, if it is locked.
pub async fn locked_for(db: &Db, key: &str, now: DateTime<Utc>) -> AppResult<Option<Duration>> {
    let Some(attempts) = fetch(db, key).await? else {
        return Ok(None);
    };

    match attempts.locked_until {
        Some(until) if until > now => Ok(Some(until - now)),
        _ => Ok(None),
    }
}

/// Records a failure against `key` and returns the resulting state.
///
/// Locks the key once [`MAX_FAILURES`] failures land inside [`WINDOW`], for
/// [`BASE_LOCK`] doubled once per failure beyond the threshold and capped at
/// [`MAX_LOCK`]. The backoff means a determined attacker's tenth guess costs 15
/// minutes and their fifteenth costs eight hours, while a human who mistypes
/// twice and gets it right on the third try never notices any of this.
pub async fn record_failure(db: &Db, key: &str, now: DateTime<Utc>) -> AppResult<Attempts> {
    let mut tx = db.begin_write().await?;

    let existing = sqlx::query_as::<_, Attempts>(
        "SELECT key, failures, first_failure_at, locked_until FROM login_attempts WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;

    // A fresh window when there is no row, when the old window has lapsed, or
    // when a lock has expired. That last case matters: after serving a lock the
    // key starts from zero, so the next honest attempt is not instantly locked
    // again by the failures that caused the first lock.
    let start_fresh = match &existing {
        None => true,
        Some(a) => match a.locked_until {
            Some(until) => until <= now,
            None => now - a.first_failure_at >= WINDOW,
        },
    };

    let (failures, first_failure_at) = if start_fresh {
        (1, now)
    } else {
        let a = existing
            .as_ref()
            .expect("start_fresh is true when there is no row");
        (a.failures + 1, a.first_failure_at)
    };

    let locked_until = (failures >= MAX_FAILURES).then(|| now + lock_duration(failures));

    sqlx::query(
        "INSERT INTO login_attempts (key, failures, first_failure_at, locked_until) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET \
           failures = excluded.failures, \
           first_failure_at = excluded.first_failure_at, \
           locked_until = excluded.locked_until",
    )
    .bind(key)
    .bind(failures)
    .bind(to_sql_timestamp(first_failure_at))
    .bind(locked_until.map(to_sql_timestamp))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Attempts {
        key: key.to_owned(),
        failures,
        first_failure_at,
        locked_until,
    })
}

/// Clears a key's counter. Called on every successful login.
pub async fn clear(db: &Db, key: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM login_attempts WHERE key = ?")
        .bind(key)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// How long a lock lasts for a given failure count.
///
/// [`BASE_LOCK`] at the threshold, doubling per extra failure, capped at
/// [`MAX_LOCK`]. The shift is bounded before it is taken: `1i64 << 64` is
/// undefined-ish (it panics in debug and wraps in release), and an attacker
/// controls the failure count.
fn lock_duration(failures: i64) -> Duration {
    let over = (failures - MAX_FAILURES).clamp(0, 32);
    let multiplier = 1i64 << over;

    BASE_LOCK
        .checked_mul(i32::try_from(multiplier).unwrap_or(i32::MAX))
        .unwrap_or(MAX_LOCK)
        .min(MAX_LOCK)
}

async fn fetch(db: &Db, key: &str) -> AppResult<Option<Attempts>> {
    Ok(sqlx::query_as::<_, Attempts>(
        "SELECT key, failures, first_failure_at, locked_until FROM login_attempts WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(db.reader())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;
    use crate::test_support::TempDb;

    async fn db() -> (Db, TempDb) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        (db, temp)
    }

    #[test]
    fn keys_are_namespaced_and_usernames_are_folded() {
        // Without the fold, Admin/admin/ADMIN are three counters for one
        // account and the real threshold is 30, not 10.
        assert_eq!(user_key("Admin"), "user:admin");
        assert_eq!(user_key("ADMIN"), "user:admin");
        assert_eq!(user_key("admin"), "user:admin");
        // The namespaces must not be able to collide with each other.
        assert_ne!(user_key("10.0.0.1"), ip_key("10.0.0.1"));
    }

    #[tokio::test]
    async fn nine_failures_do_not_lock_and_the_tenth_does() {
        let (db, _temp) = db().await;
        let now = crate::auth::now();
        let key = user_key("someone");

        for i in 1..MAX_FAILURES {
            let state = record_failure(&db, &key, now).await.unwrap();
            assert_eq!(state.failures, i);
            assert!(
                locked_for(&db, &key, now).await.unwrap().is_none(),
                "locked after only {i} failures"
            );
        }

        let state = record_failure(&db, &key, now).await.unwrap();
        assert_eq!(state.failures, MAX_FAILURES);
        let remaining = locked_for(&db, &key, now).await.unwrap().unwrap();
        assert_eq!(remaining, BASE_LOCK);

        db.close().await;
    }

    #[tokio::test]
    async fn the_lock_expires_on_its_own() {
        let (db, _temp) = db().await;
        let now = crate::auth::now();
        let key = user_key("someone");

        for _ in 0..MAX_FAILURES {
            record_failure(&db, &key, now).await.unwrap();
        }
        assert!(locked_for(&db, &key, now).await.unwrap().is_some());

        // Still locked one minute before it lifts...
        let almost = now + BASE_LOCK - Duration::minutes(1);
        assert!(locked_for(&db, &key, almost).await.unwrap().is_some());

        // ...and free one minute after.
        let after = now + BASE_LOCK + Duration::minutes(1);
        assert!(locked_for(&db, &key, after).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn a_served_lock_resets_the_counter_rather_than_relocking_instantly() {
        // Without this, the eleventh failure — the first honest attempt after
        // the lock lifts — would re-lock the account on a single typo.
        let (db, _temp) = db().await;
        let now = crate::auth::now();
        let key = user_key("someone");

        for _ in 0..MAX_FAILURES {
            record_failure(&db, &key, now).await.unwrap();
        }

        let after = now + BASE_LOCK + Duration::minutes(1);
        let state = record_failure(&db, &key, after).await.unwrap();
        assert_eq!(state.failures, 1, "the window must restart after a lock");
        assert!(locked_for(&db, &key, after).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn failures_outside_the_window_are_forgiven() {
        let (db, _temp) = db().await;
        let now = crate::auth::now();
        let key = user_key("someone");

        // Nine failures, then a long quiet period.
        for _ in 0..(MAX_FAILURES - 1) {
            record_failure(&db, &key, now).await.unwrap();
        }

        let much_later = now + WINDOW + Duration::minutes(1);
        let state = record_failure(&db, &key, much_later).await.unwrap();
        assert_eq!(
            state.failures, 1,
            "an old failure must not contribute to today's lockout"
        );
        assert!(locked_for(&db, &key, much_later).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn a_successful_login_clears_the_counter() {
        let (db, _temp) = db().await;
        let now = crate::auth::now();
        let key = user_key("someone");

        for _ in 0..3 {
            record_failure(&db, &key, now).await.unwrap();
        }
        clear(&db, &key).await.unwrap();

        // Back to zero: nine more failures must still not lock.
        for _ in 0..(MAX_FAILURES - 1) {
            record_failure(&db, &key, now).await.unwrap();
        }
        assert!(locked_for(&db, &key, now).await.unwrap().is_none());

        db.close().await;
    }

    #[tokio::test]
    async fn the_two_counters_are_independent() {
        let (db, _temp) = db().await;
        let now = crate::auth::now();

        for _ in 0..MAX_FAILURES {
            record_failure(&db, &user_key("someone"), now)
                .await
                .unwrap();
        }

        assert!(
            locked_for(&db, &user_key("someone"), now)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            locked_for(&db, &ip_key("10.0.0.1"), now)
                .await
                .unwrap()
                .is_none(),
            "locking a username must not lock an address"
        );

        db.close().await;
    }

    #[test]
    fn the_lock_doubles_per_failure_and_is_capped() {
        assert_eq!(lock_duration(MAX_FAILURES), BASE_LOCK);
        assert_eq!(lock_duration(MAX_FAILURES + 1), BASE_LOCK * 2);
        assert_eq!(lock_duration(MAX_FAILURES + 2), BASE_LOCK * 4);
        assert_eq!(
            lock_duration(MAX_FAILURES + 5),
            MAX_LOCK.min(BASE_LOCK * 32)
        );

        // The cap. An unbounded lock would let an attacker who cannot get in
        // still lock the real user out permanently.
        assert_eq!(lock_duration(1_000), MAX_LOCK);
        assert_eq!(lock_duration(i64::MAX), MAX_LOCK);
        // ...and the shift must not overflow on the way there.
        assert!(lock_duration(i64::MAX) <= MAX_LOCK);
    }

    #[tokio::test]
    async fn the_backoff_is_visible_through_the_store() {
        let (db, _temp) = db().await;
        let mut now = crate::auth::now();
        let key = user_key("someone");

        for _ in 0..MAX_FAILURES {
            record_failure(&db, &key, now).await.unwrap();
        }
        assert_eq!(
            locked_for(&db, &key, now).await.unwrap().unwrap(),
            BASE_LOCK
        );

        // Keep failing while locked: the lock gets longer, not shorter.
        now += Duration::minutes(1);
        record_failure(&db, &key, now).await.unwrap();
        assert_eq!(
            locked_for(&db, &key, now).await.unwrap().unwrap(),
            BASE_LOCK * 2
        );

        db.close().await;
    }
}

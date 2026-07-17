//! The auth audit log.
//!
//! Every authentication-relevant thing that happens gets a row: who, what, from
//! where, when. This is the record that answers "was that login me?" after the
//! fact, and no amount of `tracing` output substitutes for it — logs rotate, and
//! they are not queryable from the product.
//!
//! # Recording must never fail a request
//!
//! [`record`] logs and swallows its own errors. A login that succeeded must not
//! be reported to the user as failed because the audit insert hit a busy
//! database — and, more importantly, a failed *audit write* must not be a way to
//! make a failed *login* look like a server error.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;

/// What happened.
///
/// A closed enum rather than free strings at the call sites: the column is free
/// text (later phases add kinds without a migration), but the kinds *this* phase
/// emits should be greppable and impossible to typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A login succeeded.
    LoginSucceeded,
    /// A login failed — wrong password, unknown user, or a deactivated account.
    LoginFailed,
    /// A login was refused because the username or address is locked out.
    LoginLockedOut,
    /// A username or address crossed the failure threshold.
    LockedOut,
    /// A user logged out.
    LoggedOut,
    /// A user changed their own password.
    PasswordChanged,
    /// A user revoked one of their own sessions.
    SessionRevoked,
    /// An admin created a user.
    UserCreated,
    /// An admin edited a user.
    UserUpdated,
    /// An admin deactivated a user.
    UserDeactivated,
    /// The default admin account was seeded into an empty instance.
    DefaultAdminSeeded,
}

impl Kind {
    /// The kind's spelling in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginSucceeded => "login_succeeded",
            Self::LoginFailed => "login_failed",
            Self::LoginLockedOut => "login_locked_out",
            Self::LockedOut => "locked_out",
            Self::LoggedOut => "logged_out",
            Self::PasswordChanged => "password_changed",
            Self::SessionRevoked => "session_revoked",
            Self::UserCreated => "user_created",
            Self::UserUpdated => "user_updated",
            Self::UserDeactivated => "user_deactivated",
            Self::DefaultAdminSeeded => "default_admin_seeded",
        }
    }
}

/// Where a request came from.
#[derive(Debug, Clone, Default)]
pub struct Client {
    /// The client's IP, if one could be determined.
    pub ip: Option<String>,
    /// The client's `User-Agent`, if it sent one.
    pub user_agent: Option<String>,
}

/// Appends an event.
///
/// `user_id` is `None` when the event cannot be attributed — a failed login for
/// a username that does not exist is exactly that, and is exactly the event
/// worth having.
///
/// `detail` is free text for a human reading the log. **It must never contain a
/// password, a token, or a hash**: this table is not treated as secret, and the
/// point of hashing everything else is lost if the plaintext is one join away.
pub async fn record(
    db: &Db,
    kind: Kind,
    user_id: Option<&str>,
    client: &Client,
    detail: Option<&str>,
    now: DateTime<Utc>,
) {
    let result = sqlx::query(
        "INSERT INTO auth_events (id, user_id, kind, ip, user_agent, created_at, detail) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(user_id)
    .bind(kind.as_str())
    .bind(client.ip.as_deref())
    .bind(client.user_agent.as_deref())
    .bind(to_sql_timestamp(now))
    .bind(detail)
    .execute(db.writer())
    .await;

    if let Err(err) = result {
        // Swallowed on purpose — see the module docs. Loud in the log, because
        // an audit log that has silently stopped recording is worse than none.
        tracing::error!(
            error = ?err,
            kind = kind.as_str(),
            "failed to record an auth event"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::role::Role;
    use crate::auth::user::{self, NewUser};
    use crate::db::migrate;
    use crate::test_support::TempDb;

    async fn db() -> (Db, TempDb) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        (db, temp)
    }

    #[test]
    fn every_kind_has_a_distinct_spelling() {
        let kinds = [
            Kind::LoginSucceeded,
            Kind::LoginFailed,
            Kind::LoginLockedOut,
            Kind::LockedOut,
            Kind::LoggedOut,
            Kind::PasswordChanged,
            Kind::SessionRevoked,
            Kind::UserCreated,
            Kind::UserUpdated,
            Kind::UserDeactivated,
            Kind::DefaultAdminSeeded,
        ];
        let mut spellings: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();
        assert_eq!(spellings.len(), count, "two kinds share a spelling");
    }

    #[tokio::test]
    async fn an_unattributable_failure_is_still_recorded() {
        // The reason user_id is nullable: a failed login for a username that
        // does not exist has no user to point at, and is the single most
        // interesting row in the table.
        let (db, _temp) = db().await;
        let client = Client {
            ip: Some("10.0.0.1".to_owned()),
            user_agent: Some("curl/8".to_owned()),
        };

        record(
            &db,
            Kind::LoginFailed,
            None,
            &client,
            Some("unknown username"),
            crate::auth::now(),
        )
        .await;

        let (kind, user_id, ip, detail): (String, Option<String>, Option<String>, Option<String>) =
            sqlx::query_as("SELECT kind, user_id, ip, detail FROM auth_events")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(kind, "login_failed");
        assert_eq!(user_id, None);
        assert_eq!(ip, Some("10.0.0.1".to_owned()));
        assert_eq!(detail, Some("unknown username".to_owned()));

        db.close().await;
    }

    #[tokio::test]
    async fn a_recording_failure_never_propagates() {
        // The contract: a broken audit write must not turn a successful login
        // into a 500. Close the pools first, so the insert genuinely fails.
        let (db, _temp) = db().await;
        db.close().await;

        // No panic, no error: the signature has nowhere to put one.
        record(
            &db,
            Kind::LoginSucceeded,
            None,
            &Client::default(),
            None,
            crate::auth::now(),
        )
        .await;
    }

    #[tokio::test]
    async fn events_survive_their_user_being_deleted() {
        // ON DELETE SET NULL, not CASCADE. If a user were ever hard-deleted,
        // cascading would erase the audit trail of what they did — which is the
        // one thing an audit trail must survive.
        let (db, _temp) = db().await;

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

        record(
            &db,
            Kind::LoginSucceeded,
            Some(&user.id),
            &Client::default(),
            None,
            crate::auth::now(),
        )
        .await;

        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(&user.id)
            .execute(db.writer())
            .await
            .unwrap();

        let (count, user_id): (i64, Option<String>) =
            sqlx::query_as("SELECT COUNT(*), MAX(user_id) FROM auth_events")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(count, 1, "the event must outlive the user");
        assert_eq!(
            user_id, None,
            "and its user_id must be nulled, not dangling"
        );

        db.close().await;
    }
}

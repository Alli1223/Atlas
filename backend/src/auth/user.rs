//! The `users` row, its API representation, and the queries over both.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::role::Role;
use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

/// A row of `users`, exactly as stored.
///
/// **Deliberately not `Serialize`.** This type holds `password_hash`, and the
/// single most common way a hash reaches a client is a handler that returns the
/// entity because it happened to be `Serialize`. Making that a compile error is
/// worth the [`UserDto`] boilerplate: `Json(user)` does not compile, and no
/// amount of code review is needed to keep it that way.
#[derive(Debug, Clone, FromRow)]
pub struct User {
    /// UUID v7, as text.
    pub id: String,
    /// Unique, compared case-insensitively (the column is `COLLATE NOCASE`).
    pub username: String,
    /// Optional: a self-hosted instance need not know an address.
    pub email: Option<String>,
    /// What the UI shows.
    pub display_name: String,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// The Argon2id PHC string. Never leaves the process.
    pub password_hash: String,
    /// Instance-wide role.
    pub role: Role,
    /// Deactivated users keep their rows — cards reference them — but cannot log in.
    pub is_active: bool,
    /// While true, the forced-reset gate blocks almost every route.
    pub must_change_password: bool,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the account last changed.
    pub updated_at: DateTime<Utc>,
    /// When the account last logged in successfully. `None` until it has.
    pub last_login_at: Option<DateTime<Utc>>,
}

/// A user as the API describes it.
///
/// This is the [`User`] type minus `password_hash`, and that subtraction is the
/// entire point: a field that does not exist on the DTO cannot be leaked by a
/// handler, by a nested response, or by a field someone adds to the row later.
///
/// `camelCase` because the consumer is a TypeScript client generated from the
/// OpenAPI document (see `docs/research/rust-stack.md` §8).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserDto {
    /// UUID v7, as text.
    pub id: String,
    /// The login name.
    pub username: String,
    /// Optional email address.
    pub email: Option<String>,
    /// What the UI shows.
    pub display_name: String,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// Instance-wide role.
    pub role: Role,
    /// Whether the account can log in.
    pub is_active: bool,
    /// Whether the client must send the user to the change-password screen.
    pub must_change_password: bool,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the account last changed.
    pub updated_at: DateTime<Utc>,
    /// When the account last logged in successfully.
    pub last_login_at: Option<DateTime<Utc>>,
}

impl From<&User> for UserDto {
    fn from(user: &User) -> Self {
        Self {
            id: user.id.clone(),
            username: user.username.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            avatar_url: user.avatar_url.clone(),
            role: user.role,
            is_active: user.is_active,
            must_change_password: user.must_change_password,
            created_at: user.created_at,
            updated_at: user.updated_at,
            last_login_at: user.last_login_at,
        }
    }
}

impl From<User> for UserDto {
    fn from(user: User) -> Self {
        Self::from(&user)
    }
}

/// Every column of `users`, as a macro rather than a `const`.
///
/// A macro so that `concat!` can splice it: `concat!` takes literals only, and
/// a `const &str` is not one. The payoff is that every query below is a
/// `&'static str`, which satisfies sqlx 0.9's `SqlSafeStr` bound *without*
/// `AssertSqlSafe` — so the absence of `AssertSqlSafe` in this module is a real
/// signal that no SQL here is assembled at runtime.
macro_rules! user_columns {
    () => {
        "id, username, email, display_name, avatar_url, password_hash, role, is_active, \
         must_change_password, created_at, updated_at, last_login_at"
    };
}

/// A new account, ready to insert.
#[derive(Debug)]
pub struct NewUser {
    /// The login name. Must be unique, case-insensitively.
    pub username: String,
    /// Optional email address.
    pub email: Option<String>,
    /// What the UI shows.
    pub display_name: String,
    /// An Argon2id PHC string — hash before you get here.
    pub password_hash: String,
    /// Instance-wide role.
    pub role: Role,
    /// Whether the forced-reset gate should apply immediately.
    pub must_change_password: bool,
}

/// Finds a user by username, case-insensitively.
///
/// The case-insensitivity is the column's `COLLATE NOCASE`, not a `LOWER()` call
/// here: putting it in the column definition makes the `UNIQUE` index
/// case-insensitive too — so `Admin` and `admin` cannot both exist — *and* keeps
/// this lookup on the index instead of scanning.
pub async fn find_by_username(db: &Db, username: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE username = ?"
    ))
    .bind(username)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a user by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Every user, ordered for display.
pub async fn list(db: &Db) -> AppResult<Vec<User>> {
    Ok(sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users ORDER BY display_name, username"
    ))
    .fetch_all(db.reader())
    .await?)
}

/// Whether any user exists at all.
///
/// `EXISTS` rather than `COUNT(*)`: the seeder only asks "is this instance
/// empty", and `EXISTS` stops at the first row.
pub async fn any_exist(db: &Db) -> AppResult<bool> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(db.reader())
        .await?;
    Ok(exists)
}

/// Whether any user exists, read inside an open transaction.
pub async fn any_exist_tx(tx: &mut sqlx::SqliteConnection) -> AppResult<bool> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await?;
    Ok(exists)
}

/// Inserts a user and returns it.
///
/// Takes a transaction rather than the pool so that "check unique, then insert"
/// and "count admins, then demote" are atomic. `Db::begin_write` issues
/// `BEGIN IMMEDIATE`, so the write lock is already held when the check runs and
/// no other writer can slip in between.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    new: &NewUser,
    now: DateTime<Utc>,
) -> AppResult<User> {
    // UUID v7 rather than v4: it is time-ordered, so ids sort by creation and
    // the primary-key index stays append-mostly instead of writing into a random
    // page on every insert.
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO users (id, username, email, display_name, avatar_url, password_hash, \
         role, is_active, must_change_password, created_at, updated_at, last_login_at) \
         VALUES (?, ?, ?, ?, NULL, ?, ?, 1, ?, ?, ?, NULL)",
    )
    .bind(&id)
    .bind(&new.username)
    .bind(&new.email)
    .bind(&new.display_name)
    .bind(&new.password_hash)
    .bind(new.role)
    .bind(new.must_change_password)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    Ok(sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE id = ?"
    ))
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?)
}

/// Reads a user inside an open transaction, so a check and its write see the
/// same snapshot.
pub async fn find_by_id_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        user_columns!(),
        " FROM users WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Whether a username is already taken, case-insensitively.
///
/// `excluding_id` is the user being edited, so renaming someone to their own
/// current name is not a conflict with themselves.
pub async fn username_taken(
    tx: &mut sqlx::SqliteConnection,
    username: &str,
    excluding_id: Option<&str>,
) -> AppResult<bool> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE username = ? AND (? IS NULL OR id != ?))",
    )
    .bind(username)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(taken)
}

/// Whether an email is already taken, case-insensitively.
pub async fn email_taken(
    tx: &mut sqlx::SqliteConnection,
    email: &str,
    excluding_id: Option<&str>,
) -> AppResult<bool> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE email = ? AND (? IS NULL OR id != ?))",
    )
    .bind(email)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(taken)
}

/// How many active admins there are.
///
/// The guard against locking every human out of the instance: an edit that would
/// take this to zero is refused.
pub async fn active_admin_count(tx: &mut sqlx::SqliteConnection) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND is_active = 1")
            .fetch_one(&mut *tx)
            .await?,
    )
}

/// Records a successful login.
pub async fn touch_last_login(db: &Db, id: &str, now: DateTime<Utc>) -> AppResult<()> {
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now))
        .bind(id)
        .execute(db.writer())
        .await?;
    Ok(())
}

/// Replaces a user's password and clears the forced-reset flag.
pub async fn set_password(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    password_hash: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0, updated_at = ? WHERE id = ?",
    )
    .bind(password_hash)
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// The fields a `PATCH /users/{id}` may change.
///
/// `Option<Option<T>>` on the nullable fields distinguishes the three cases JSON
/// can express — absent (leave alone), `null` (clear), and a value (set) — which
/// a plain `Option` collapses into two. See `docs/research/rust-stack.md` §8.
#[derive(Debug, Default)]
pub struct UserPatch {
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub email: Option<Option<String>>,
    /// The display name, if it is changing.
    pub display_name: Option<String>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub avatar_url: Option<Option<String>>,
    /// The role, if it is changing.
    pub role: Option<Role>,
    /// Activation, if it is changing.
    pub is_active: Option<bool>,
    /// The forced-reset flag, if it is changing.
    pub must_change_password: Option<bool>,
}

impl UserPatch {
    /// Whether this patch would change anything.
    pub fn is_empty(&self) -> bool {
        self.email.is_none()
            && self.display_name.is_none()
            && self.avatar_url.is_none()
            && self.role.is_none()
            && self.is_active.is_none()
            && self.must_change_password.is_none()
    }
}

/// Applies a patch.
///
/// One fixed statement writes every column on every call: `COALESCE(?, column)`
/// means "leave it alone when the parameter is NULL". The alternative — building
/// a `SET` list from whichever fields happen to be present — assembles SQL from
/// a runtime shape, which is the habit that produces injection bugs even where
/// this particular instance would have been safe.
///
/// The nullable columns cannot use `COALESCE`, because there NULL is a value the
/// caller may legitimately mean. They get a `CASE WHEN <should-write> THEN
/// <value> ELSE <column> END` and an explicit flag instead.
pub async fn apply_patch(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    patch: &UserPatch,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE users SET \
           email                = CASE WHEN ? THEN ? ELSE email END, \
           display_name         = COALESCE(?, display_name), \
           avatar_url           = CASE WHEN ? THEN ? ELSE avatar_url END, \
           role                 = COALESCE(?, role), \
           is_active            = COALESCE(?, is_active), \
           must_change_password = COALESCE(?, must_change_password), \
           updated_at           = ? \
         WHERE id = ?",
    )
    .bind(patch.email.is_some())
    .bind(patch.email.clone().flatten())
    .bind(patch.display_name.clone())
    .bind(patch.avatar_url.is_some())
    .bind(patch.avatar_url.clone().flatten())
    .bind(patch.role)
    .bind(patch.is_active)
    .bind(patch.must_change_password)
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Deactivates a user. Atlas never hard-deletes: cards reference their author.
pub async fn deactivate(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET is_active = 0, updated_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
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

    async fn insert_user(db: &Db, username: &str, role: Role) -> User {
        let mut tx = db.begin_write().await.unwrap();
        let user = insert(
            &mut tx,
            &NewUser {
                username: username.to_owned(),
                email: None,
                display_name: username.to_owned(),
                password_hash: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g".to_owned(),
                role,
                must_change_password: false,
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        user
    }

    #[test]
    fn the_dto_cannot_carry_a_password_hash() {
        // The structural guarantee, asserted on the wire format rather than the
        // type: if someone adds `password_hash` to UserDto, this fails.
        let user = User {
            id: "u1".to_owned(),
            username: "someone".to_owned(),
            email: Some("a@b.test".to_owned()),
            display_name: "Someone".to_owned(),
            avatar_url: None,
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$SALTSALT$HASHHASH".to_owned(),
            role: Role::Member,
            is_active: true,
            must_change_password: false,
            created_at: crate::auth::now(),
            updated_at: crate::auth::now(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&UserDto::from(&user)).unwrap();
        assert!(!json.contains("argon2"), "{json}");
        assert!(!json.contains("HASHHASH"), "{json}");
        assert!(!json.contains("passwordHash"), "{json}");
        assert!(!json.contains("password_hash"), "{json}");
        // ...while still carrying what the UI needs.
        assert!(json.contains("\"username\":\"someone\""), "{json}");
        assert!(json.contains("\"mustChangePassword\":false"), "{json}");
    }

    #[tokio::test]
    async fn a_user_round_trips_through_the_database() {
        let (db, _temp) = db().await;
        let created = insert_user(&db, "someone", Role::Member).await;

        let found = find_by_id(&db, &created.id).await.unwrap().unwrap();
        assert_eq!(found.username, "someone");
        assert_eq!(found.role, Role::Member);
        assert!(found.is_active);
        assert!(!found.must_change_password);
        assert!(found.last_login_at.is_none());
        // The timestamp survived the TEXT round-trip intact, to the microsecond
        // the storage format keeps.
        assert_eq!(found.created_at, created.created_at);

        db.close().await;
    }

    #[tokio::test]
    async fn username_lookup_is_case_insensitive() {
        // This is the database's COLLATE NOCASE doing the work. If someone drops
        // it from the migration, "Admin" stops finding "admin" and a second
        // "ADMIN" account becomes creatable.
        let (db, _temp) = db().await;
        insert_user(&db, "Admin", Role::Admin).await;

        for spelling in ["Admin", "admin", "ADMIN", "aDmIn"] {
            assert!(
                find_by_username(&db, spelling).await.unwrap().is_some(),
                "{spelling} did not find the Admin account"
            );
        }

        db.close().await;
    }

    #[tokio::test]
    async fn usernames_are_unique_case_insensitively() {
        let (db, _temp) = db().await;
        insert_user(&db, "someone", Role::Member).await;

        let mut tx = db.begin_write().await.unwrap();
        assert!(username_taken(&mut tx, "SOMEONE", None).await.unwrap());
        assert!(!username_taken(&mut tx, "someone-else", None).await.unwrap());
        tx.rollback().await.unwrap();

        // ...and the database refuses the duplicate even if the check is skipped.
        let mut tx = db.begin_write().await.unwrap();
        let result = insert(
            &mut tx,
            &NewUser {
                username: "SOMEONE".to_owned(),
                email: None,
                display_name: "Clash".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            crate::auth::now(),
        )
        .await;
        assert!(
            result.is_err(),
            "the UNIQUE index must reject a case variant"
        );
        tx.rollback().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn username_taken_can_exclude_the_user_being_edited() {
        let (db, _temp) = db().await;
        let user = insert_user(&db, "someone", Role::Member).await;

        let mut tx = db.begin_write().await.unwrap();
        // Editing yourself must not collide with yourself.
        assert!(
            !username_taken(&mut tx, "someone", Some(&user.id))
                .await
                .unwrap()
        );
        assert!(username_taken(&mut tx, "someone", None).await.unwrap());
        tx.rollback().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn a_patch_leaves_absent_fields_alone_and_clears_explicit_nulls() {
        let (db, _temp) = db().await;
        let user = insert_user(&db, "someone", Role::Member).await;

        // Give it an email to clear later.
        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &user.id,
            &UserPatch {
                email: Some(Some("a@b.test".to_owned())),
                ..UserPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            find_by_id(&db, &user.id).await.unwrap().unwrap().email,
            Some("a@b.test".to_owned())
        );

        // An absent email must not disturb it, while a present role does change.
        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &user.id,
            &UserPatch {
                role: Some(Role::Viewer),
                ..UserPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let after = find_by_id(&db, &user.id).await.unwrap().unwrap();
        assert_eq!(after.email, Some("a@b.test".to_owned()), "absent != null");
        assert_eq!(after.role, Role::Viewer);

        // An explicit null clears it. This is the case a plain Option cannot
        // express, and the reason for Option<Option<_>>.
        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &user.id,
            &UserPatch {
                email: Some(None),
                ..UserPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            find_by_id(&db, &user.id).await.unwrap().unwrap().email,
            None
        );

        db.close().await;
    }

    #[tokio::test]
    async fn the_database_rejects_a_role_outside_the_three() {
        // The CHECK constraint, independently of Role's Decode impl.
        let (db, _temp) = db().await;
        let err = sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, role, \
             created_at, updated_at) VALUES ('x', 'x', 'x', 'x', 'superuser', 'now', 'now')",
        )
        .execute(db.writer())
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");

        db.close().await;
    }

    #[tokio::test]
    async fn any_exist_reports_the_empty_instance() {
        let (db, _temp) = db().await;
        assert!(!any_exist(&db).await.unwrap());
        insert_user(&db, "someone", Role::Member).await;
        assert!(any_exist(&db).await.unwrap());
        db.close().await;
    }

    #[tokio::test]
    async fn active_admins_are_counted_for_the_lockout_guard() {
        let (db, _temp) = db().await;
        let admin1 = insert_user(&db, "admin1", Role::Admin).await;
        insert_user(&db, "admin2", Role::Admin).await;
        let member = insert_user(&db, "member", Role::Member).await;

        let mut tx = db.begin_write().await.unwrap();
        assert_eq!(
            active_admin_count(&mut tx).await.unwrap(),
            2,
            "members do not count"
        );

        // Deactivating an admin drops the count — an *inactive* admin cannot
        // unlock anyone, so it must not satisfy the guard.
        deactivate(&mut tx, &admin1.id, crate::auth::now())
            .await
            .unwrap();
        assert_eq!(active_admin_count(&mut tx).await.unwrap(), 1);

        // Promoting a member raises it.
        apply_patch(
            &mut tx,
            &member.id,
            &UserPatch {
                role: Some(Role::Admin),
                ..UserPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        assert_eq!(active_admin_count(&mut tx).await.unwrap(), 2);
        tx.rollback().await.unwrap();

        db.close().await;
    }
}

//! The default `Admin` account.

use crate::auth::events::{self, Client, Kind};
use crate::auth::now;
use crate::auth::password::{self, DEFAULT_ADMIN_PASSWORD};
use crate::auth::role::Role;
use crate::auth::user::{self, NewUser};
use crate::db::Db;
use crate::error::AppResult;

/// The seeded account's username.
pub const DEFAULT_ADMIN_USERNAME: &str = "Admin";

/// The seeded account's display name.
const DEFAULT_ADMIN_DISPLAY_NAME: &str = "Administrator";

/// Creates the default admin, but **only into a completely empty instance**.
///
/// # The idempotency rule, and why it is "any user" rather than "no Admin"
///
/// The condition is `users` being empty — not "there is no account called
/// Admin". The difference is the whole point. An operator who creates their own
/// account and then deletes `Admin` has *said* they do not want it; a seeder
/// keyed on "is there an Admin" would helpfully recreate it, with the password
/// `Admin`, on the very next restart. Silently, forever. Keying on emptiness
/// means the seed happens exactly once in an instance's life, at first boot.
///
/// # Why this is not part of migration 0002
///
/// The password has to be hashed, and a migration is SQL. Baking a precomputed
/// hash into the migration would ship one fixed salt to every Atlas installation
/// in the world — the password is `Admin` and public either way, but a shared
/// salt is a bad habit to encode in the schema, and `must_change_password`
/// deserves a real Argon2id hash behind it rather than a constant.
///
/// Returns whether an account was created.
pub async fn ensure_default_admin(db: &Db) -> AppResult<bool> {
    // Cheap check first, so the common path (every boot after the first) costs
    // one indexed read and does not take the write lock or burn 50ms of Argon2.
    if user::any_exist(db).await? {
        return Ok(false);
    }

    // Hash outside the transaction: this is ~50ms of deliberate CPU and the
    // writer pool has exactly one connection. Holding it for the duration would
    // block every other write in the process for no reason.
    let password_hash = password::hash(DEFAULT_ADMIN_PASSWORD.to_owned()).await?;

    let now = now();
    let mut tx = db.begin_write().await?;

    // Re-check inside the transaction. `Db::begin_write` issues BEGIN IMMEDIATE,
    // so the write lock is held from here — two Atlas processes racing to boot
    // against the same file cannot both pass this check and create two admins.
    if user::any_exist_tx(&mut tx).await? {
        tx.rollback().await?;
        return Ok(false);
    }

    let admin = user::insert(
        &mut tx,
        &NewUser {
            username: DEFAULT_ADMIN_USERNAME.to_owned(),
            email: None,
            display_name: DEFAULT_ADMIN_DISPLAY_NAME.to_owned(),
            password_hash,
            role: Role::Admin,
            // The forced-reset gate. Until this is cleared, this account can
            // reach exactly three routes.
            must_change_password: true,
        },
        now,
    )
    .await?;

    tx.commit().await?;

    // Deliberately loud, and deliberately not a debug log. An instance running
    // with `Admin`/`Admin` is a live vulnerability from this second until
    // somebody changes it, and the operator must not be able to miss it.
    //
    // One `warn!` per line, with no structured fields: this is a banner meant to
    // be read by a human watching a terminal on first boot. A field would be
    // appended to the line by the pretty formatter and break the box.
    tracing::warn!("===============================================================");
    tracing::warn!("  Atlas created the default administrator account because this");
    tracing::warn!("  instance had no users.");
    tracing::warn!(
        "      username: {DEFAULT_ADMIN_USERNAME}    password: {DEFAULT_ADMIN_PASSWORD}"
    );
    tracing::warn!("  These credentials are PUBLIC. Sign in and change the password");
    tracing::warn!("  immediately — every route is refused until you do.");
    tracing::warn!("===============================================================");

    events::record(
        db,
        Kind::DefaultAdminSeeded,
        Some(&admin.id),
        &Client::default(),
        Some("default administrator seeded into an empty instance"),
        now,
    )
    .await;

    Ok(true)
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

    async fn usernames(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT username FROM users ORDER BY username")
            .fetch_all(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_empty_instance_gets_an_admin_that_must_change_its_password() {
        let (db, _temp) = db().await;

        assert!(ensure_default_admin(&db).await.unwrap());

        let admin = user::find_by_username(&db, DEFAULT_ADMIN_USERNAME)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admin.username, "Admin");
        assert_eq!(admin.role, Role::Admin);
        assert!(admin.is_active);
        assert!(
            admin.must_change_password,
            "the seeded admin must be locked to the reset screen"
        );

        // The password really is `Admin`, and it really is Argon2id — the point
        // of the seed is that the operator can actually sign in.
        assert!(admin.password_hash.starts_with("$argon2id$"));
        assert!(
            password::verify(
                DEFAULT_ADMIN_PASSWORD.to_owned(),
                admin.password_hash.clone()
            )
            .await
            .unwrap()
        );
        assert!(
            !password::verify("something else".to_owned(), admin.password_hash)
                .await
                .unwrap()
        );

        db.close().await;
    }

    #[tokio::test]
    async fn seeding_is_idempotent_across_restarts() {
        // This runs on every boot. Three boots must not be three admins.
        let (db, _temp) = db().await;

        assert!(ensure_default_admin(&db).await.unwrap());
        assert!(!ensure_default_admin(&db).await.unwrap());
        assert!(!ensure_default_admin(&db).await.unwrap());

        assert_eq!(usernames(&db).await, ["Admin"]);

        db.close().await;
    }

    #[tokio::test]
    async fn a_seeded_admin_that_changed_its_password_is_not_reseeded() {
        // The realistic second boot: the operator has done what they were told.
        // Re-running the seeder must not touch the account.
        let (db, _temp) = db().await;
        ensure_default_admin(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let admin = user::find_by_username(&db, DEFAULT_ADMIN_USERNAME)
            .await
            .unwrap()
            .unwrap();
        user::set_password(&mut tx, &admin.id, "$argon2id$fake", now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(!ensure_default_admin(&db).await.unwrap());

        let after = user::find_by_username(&db, DEFAULT_ADMIN_USERNAME)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.password_hash, "$argon2id$fake",
            "the seeder overwrote a real password"
        );
        assert!(
            !after.must_change_password,
            "the seeder re-locked the account"
        );

        db.close().await;
    }

    #[tokio::test]
    async fn deleting_admin_does_not_resurrect_it() {
        // The headline of the idempotency rule. An operator who made their own
        // account and deleted Admin has said what they want; a seeder keyed on
        // "is there an Admin" would put `Admin`/`Admin` back on the next reboot.
        let (db, _temp) = db().await;
        ensure_default_admin(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        user::insert(
            &mut tx,
            &NewUser {
                username: "alastair".to_owned(),
                email: None,
                display_name: "Alastair".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Admin,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        sqlx::query("DELETE FROM users WHERE username = 'Admin'")
            .execute(db.writer())
            .await
            .unwrap();

        assert!(!ensure_default_admin(&db).await.unwrap());
        assert_eq!(
            usernames(&db).await,
            ["alastair"],
            "Admin came back from the dead"
        );

        db.close().await;
    }

    #[tokio::test]
    async fn an_instance_with_any_user_is_never_seeded() {
        // Not even a viewer-only instance with no admin at all: "empty" means
        // empty. Recovering a lost admin is an operator task with a CLI, not
        // something that should happen by surprise on a reboot.
        let (db, _temp) = db().await;

        let mut tx = db.begin_write().await.unwrap();
        user::insert(
            &mut tx,
            &NewUser {
                username: "viewer".to_owned(),
                email: None,
                display_name: "Viewer".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Viewer,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert!(!ensure_default_admin(&db).await.unwrap());
        assert_eq!(usernames(&db).await, ["viewer"]);

        db.close().await;
    }

    #[tokio::test]
    async fn the_seed_is_audited() {
        let (db, _temp) = db().await;
        ensure_default_admin(&db).await.unwrap();

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM auth_events")
            .fetch_all(db.reader())
            .await
            .unwrap();
        assert_eq!(kinds, ["default_admin_seeded"]);

        db.close().await;
    }

    #[tokio::test]
    async fn concurrent_boots_create_exactly_one_admin() {
        // Two processes against one database file is a real deployment (a
        // rolling restart). The BEGIN IMMEDIATE re-check inside the transaction
        // is what makes this safe; without it both pass the cheap check and the
        // UNIQUE index turns the loser into a 500 at boot.
        let (db, _temp) = db().await;

        let (a, b) = tokio::join!(ensure_default_admin(&db), ensure_default_admin(&db));
        let created = [a.unwrap(), b.unwrap()];

        assert_eq!(
            created.iter().filter(|c| **c).count(),
            1,
            "exactly one of the two boots should have created the admin"
        );
        assert_eq!(usernames(&db).await, ["Admin"]);

        db.close().await;
    }

    #[tokio::test]
    async fn the_seeded_password_is_rejected_as_a_replacement_for_itself() {
        // The other half of the requirement: seeding `Admin` is fine, keeping it
        // is not. The policy owns this rule, so it holds for any account.
        assert!(password::validate(DEFAULT_ADMIN_PASSWORD, DEFAULT_ADMIN_USERNAME).is_err());
        assert!(password::validate("admin", "Admin").is_err());
        assert!(password::validate("ADMIN", "Admin").is_err());
    }
}

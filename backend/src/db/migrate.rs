//! Embedded schema migrations.
//!
//! Migrations are compiled into the binary by [`sqlx::migrate!`], so a release
//! build carries its own schema and needs no files on disk at runtime — which is
//! what makes the single-binary deploy in Phase 20 possible.

use anyhow::Context;
use sqlx::migrate::Migrator;

use crate::db::Db;

/// Every migration under `backend/migrations`, embedded at compile time.
///
/// The path is resolved relative to `CARGO_MANIFEST_DIR`. Adding a `.sql` file
/// there is enough; nothing needs registering.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Applies all outstanding migrations.
///
/// Runs on the **writer** pool: migrations are writes, and the reader pool is
/// opened read-only. Must complete before the server starts serving.
pub async fn run(db: &Db) -> anyhow::Result<()> {
    MIGRATOR
        .run(db.writer())
        .await
        .context("failed to apply database migrations")?;

    tracing::info!(
        migrations = MIGRATOR.iter().len(),
        "database migrations applied"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::test_support::TempDb;

    #[tokio::test]
    async fn migrations_create_the_meta_table() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        run(&db).await.unwrap();

        let name: String = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_atlas_meta'",
        )
        .fetch_one(db.reader())
        .await
        .unwrap();
        assert_eq!(name, "_atlas_meta");

        db.close().await;
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        // The migrator must be safe to run on every boot, which is exactly how
        // `main` calls it.
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();

        run(&db).await.unwrap();
        run(&db).await.unwrap();
        run(&db).await.unwrap();

        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(applied, i64::try_from(MIGRATOR.iter().len()).unwrap());

        db.close().await;
    }

    #[tokio::test]
    async fn the_schema_version_is_recorded() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        run(&db).await.unwrap();

        let value: String =
            sqlx::query_scalar("SELECT value FROM _atlas_meta WHERE key = 'schema_version'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(value, "1");

        db.close().await;
    }

    #[test]
    fn there_is_at_least_one_migration() {
        // Guards against the embedding silently resolving to an empty directory.
        assert!(MIGRATOR.iter().len() >= 1);
    }
}

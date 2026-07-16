//! SQLite access: a writer pool of exactly one, and a reader pool of N.

pub mod migrate;

use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::config::Config;

/// The statement every write transaction begins with.
///
/// See [`Db::begin_write`] for why this is not `BEGIN`.
const BEGIN_IMMEDIATE: &str = "BEGIN IMMEDIATE";

/// Handle to the Atlas database.
///
/// # Why two pools
///
/// SQLite in WAL mode allows **one writer at a time**, concurrent with N
/// readers. The split is not an optimisation — it exists to make a specific
/// deadlock impossible.
///
/// sqlx's [`sqlx::Pool::begin`] emits `BEGIN DEFERRED`. A deferred transaction
/// starts as a *reader* and only tries to take the write lock on its first
/// write. If another connection wrote in between, that upgrade fails with
/// `SQLITE_BUSY_SNAPSHOT` — and crucially `busy_timeout` does **not** retry it,
/// because the transaction's snapshot is already stale; no amount of waiting can
/// make it succeed. The transaction has to be rolled back and replayed.
///
/// Two mitigations, applied together (see `docs/research/corrections.md` #11):
///
/// 1. **A writer pool of exactly one connection.** Every write in this process
///    is serialised through it, so two of *our own* connections can never race
///    to upgrade.
/// 2. **`BEGIN IMMEDIATE` on every write transaction** ([`Db::begin_write`]),
///    which takes the write lock up front — where `busy_timeout` *does* apply.
///
/// The pool split alone is not enough, because it says nothing about a *second
/// process* touching the same file (a backup tool, the `sqlite3` CLI, a
/// supervised subprocess). `BEGIN IMMEDIATE` alone is not enough either, because
/// a larger writer pool would just convert in-process serialisation into
/// `SQLITE_BUSY`. Hence both.
///
/// The reader pool additionally opens `read_only`, so a stray write routed to it
/// fails loudly at the point of the mistake instead of quietly contending for
/// the write lock.
#[derive(Debug, Clone)]
pub struct Db {
    writer: SqlitePool,
    reader: SqlitePool,
}

impl Db {
    /// Opens both pools against the configured database, creating it if needed.
    ///
    /// The writer is opened first: it is what creates the file and puts it into
    /// WAL mode, which the read-only reader cannot do for itself.
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let url = config.database_url.as_str();

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            // Keep the single writer warm: reconnecting costs a WAL handshake.
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(writer_options(url)?)
            .await
            .with_context(|| format!("failed to open the database for writing at {url}"))?;

        let reader = SqlitePoolOptions::new()
            .max_connections(config.reader_pool_size)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(reader_options(url)?)
            .await
            .with_context(|| format!("failed to open the database for reading at {url}"))?;

        tracing::debug!(
            url,
            reader_pool_size = config.reader_pool_size,
            "database pools opened"
        );

        Ok(Self { writer, reader })
    }

    /// The read-only pool. Every `SELECT` belongs here.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// The single-connection write pool.
    ///
    /// Safe for standalone statements: SQLite wraps a bare `INSERT`/`UPDATE` in
    /// an implicit transaction that takes the write lock immediately, so there
    /// is no deferred upgrade to lose. Anything that reads and then writes must
    /// go through [`Db::begin_write`] instead.
    pub fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// Begins a write transaction with `BEGIN IMMEDIATE`.
    ///
    /// **Use this for every write transaction.** `pool.begin()` would emit
    /// `BEGIN DEFERRED` and expose the `SQLITE_BUSY_SNAPSHOT` upgrade race
    /// described on [`Db`].
    pub async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
        self.writer.begin_with(BEGIN_IMMEDIATE).await
    }

    /// Checks that both pools can actually round-trip a query.
    ///
    /// Deliberately hits the writer as well as the reader: they are separate
    /// connections with separate failure modes, and a health check that only
    /// proves reads work would report green while every write fails.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.reader).await?;
        sqlx::query("SELECT 1").execute(&self.writer).await?;
        Ok(())
    }

    /// Closes both pools, waiting for in-flight statements to finish.
    pub async fn close(&self) {
        self.writer.close().await;
        self.reader.close().await;
    }
}

/// Connection options shared by both pools.
///
/// Note what is *not* here: `journal_mode`. It is a property of the database
/// file, not of a connection, and sqlx deliberately leaves it unset by default
/// so that opening a connection cannot silently flip a database into or out of
/// WAL. Only the writer sets it — see [`reader_options`].
fn base_options(url: &str) -> Result<SqliteConnectOptions, sqlx::Error> {
    Ok(SqliteConnectOptions::from_str(url)?
        // sqlx already defaults this ON; being explicit means a future default
        // change cannot silently disable referential integrity.
        .foreign_keys(true)
        // The right WAL trade-off: a power loss can cost the last few committed
        // transactions, but cannot corrupt the database. FULL costs an fsync per
        // commit for a durability guarantee a self-hosted board does not need.
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        // Negative = KiB, so this is 64 MiB of page cache per connection.
        .pragma("cache_size", "-64000")
        .pragma("temp_store", "MEMORY"))
}

/// Writer options: creates the database and establishes WAL mode.
fn writer_options(url: &str) -> Result<SqliteConnectOptions, sqlx::Error> {
    Ok(base_options(url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // Run a bounded `PRAGMA optimize` on close so the planner keeps decent
        // statistics without a scheduled job.
        .optimize_on_close(true, Some(400)))
}

/// Reader options: read-only, and inheriting the journal mode from the file.
///
/// Setting `journal_mode` here would be a bug. `PRAGMA journal_mode = WAL` is a
/// *write*, and these connections are opened `SQLITE_OPEN_READONLY`. A database
/// already in WAL applies WAL to every connection that does not ask for
/// something else, so omitting it is both correct and necessary.
fn reader_options(url: &str) -> Result<SqliteConnectOptions, sqlx::Error> {
    // `read_only` wins over `create_if_missing` in sqlx's flag computation, so
    // a missing file surfaces as an error here rather than as an empty database.
    Ok(base_options(url)?.read_only(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;
    use crate::test_support::TempDb;

    #[tokio::test]
    async fn connect_creates_the_database_and_both_pools_work() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        db.ping().await.unwrap();
        db.close().await;
    }

    #[tokio::test]
    async fn the_database_is_actually_in_wal_mode() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();

        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        db.close().await;
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_on_both_pools() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();

        for pool in [db.reader(), db.writer()] {
            let on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(on, 1);
        }

        db.close().await;
    }

    #[tokio::test]
    async fn the_writer_pool_has_exactly_one_connection() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        assert_eq!(db.writer().options().get_max_connections(), 1);
        db.close().await;
    }

    #[tokio::test]
    async fn the_reader_pool_rejects_writes() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        // This is the whole point of opening the readers read-only: a write
        // routed to the wrong pool must fail here, loudly, not contend silently.
        let err = sqlx::query("INSERT INTO _atlas_meta (key, value) VALUES ('x', 'y')")
            .execute(db.reader())
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "expected a readonly error, got: {err}"
        );

        db.close().await;
    }

    #[tokio::test]
    async fn begin_write_commits_and_rolls_back() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        // Commit.
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("INSERT INTO _atlas_meta (key, value) VALUES ('committed', '1')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        // Roll back.
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("INSERT INTO _atlas_meta (key, value) VALUES ('rolled_back', '1')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT key FROM _atlas_meta WHERE key IN ('committed', 'rolled_back')",
        )
        .fetch_all(db.reader())
        .await
        .unwrap();
        assert_eq!(keys, ["committed"]);

        db.close().await;
    }

    #[tokio::test]
    async fn a_write_is_visible_to_the_reader_pool() {
        // Guards the two-pool design: readers must see committed writes made on
        // the other pool's connection.
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("INSERT INTO _atlas_meta (key, value) VALUES ('visible', 'yes')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let value: String =
            sqlx::query_scalar("SELECT value FROM _atlas_meta WHERE key = 'visible'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(value, "yes");

        db.close().await;
    }
}

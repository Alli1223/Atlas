//! Backups (`TODO.md` Phase 20): the database by `VACUUM INTO`, the uploads tree as a
//! `.tar.gz`, on a schedule and on demand — and an offline restore.
//!
//! # Why `VACUUM INTO`
//!
//! Copying `atlas.db` while Atlas runs copies a WAL-mode database mid-write: the main file
//! without the `-wal` beside it is a database from some moment in the past, or a corrupt one.
//! `VACUUM INTO` writes a consistent, compacted, self-contained copy from one read
//! transaction, while writers carry on. It is SQLite's own answer to "back up a live database".
//!
//! # Layout
//!
//! `{data_dir}/backups/atlas-{YYYYMMDDTHHMMSSZ}.db` and, beside it,
//! `atlas-{stamp}-uploads.tar.gz`. A backup is the pair; [`prune`] keeps the newest
//! `ATLAS_BACKUP_KEEP` of them.
//!
//! # Restore is offline
//!
//! `atlas restore <name>` replaces the database (removing any `-wal`/`-shm` beside it) and
//! the uploads tree. It refuses while the database is in use — restoring under a running
//! server would hand its open connections a file swapped out from under them.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, Utc};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::Serialize;
use utoipa::ToSchema;

use crate::config::Config;
use crate::db::Db;
use crate::error::{AppError, AppResult};

const PREFIX: &str = "atlas-";
const STAMP: &str = "%Y%m%dT%H%M%SZ";
const UPLOADS_SUFFIX: &str = "-uploads.tar.gz";

/// One backup.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    /// `atlas-20261101T020000Z` — what `restore` and the download routes take.
    pub name: String,
    /// When it was taken.
    pub created_at: DateTime<Utc>,
    /// Size of the database copy.
    pub database_bytes: u64,
    /// Size of the uploads archive, if one was written.
    pub uploads_bytes: Option<u64>,
}

/// The database file a `sqlite://` URL names, or `None` for an in-memory database.
pub fn database_path(config: &Config) -> Option<PathBuf> {
    let url = config.database_url.as_str();
    let path = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    (!path.is_empty() && path != ":memory:").then(|| PathBuf::from(path))
}

/// Validates a backup name, so a route parameter can never become a path traversal.
pub fn check_name(name: &str) -> AppResult<NaiveDateTime> {
    name.strip_prefix(PREFIX)
        .and_then(|stamp| NaiveDateTime::parse_from_str(stamp, STAMP).ok())
        .ok_or(AppError::NotFound)
}

/// The two files of a backup.
pub fn files(config: &Config, name: &str) -> AppResult<(PathBuf, PathBuf)> {
    check_name(name)?;
    let dir = config.backup_dir();
    Ok((
        dir.join(format!("{name}.db")),
        dir.join(format!("{name}{UPLOADS_SUFFIX}")),
    ))
}

fn archive_uploads(uploads: &Path, target: &Path) -> std::io::Result<()> {
    let file = fs::File::create(target)?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    // Symlinks are archived as links, never followed: a link out of uploads/ must not drag
    // the rest of the filesystem into a backup.
    builder.follow_symlinks(false);
    if uploads.is_dir() {
        builder.append_dir_all("uploads", uploads)?;
    }
    builder.into_inner()?.finish()?.sync_all()
}

/// Takes a backup now.
pub async fn run(db: &Db, config: &Config, now: DateTime<Utc>) -> AppResult<BackupInfo> {
    let dir = config.backup_dir();
    fs::create_dir_all(&dir).map_err(AppError::internal)?;
    let name = format!("{PREFIX}{}", now.format(STAMP));
    let (database, uploads) = files(config, &name)?;
    if database.exists() {
        return Err(AppError::Conflict(
            "A backup was already taken this second.".to_owned(),
        ));
    }
    let target = database
        .to_str()
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("a non-UTF-8 backup path")))?
        .to_owned();
    sqlx::query("VACUUM INTO ?")
        .bind(&target)
        .execute(db.reader())
        .await?;

    let source = crate::storage::root(&config.data_dir);
    let archive = uploads.clone();
    tokio::task::spawn_blocking(move || archive_uploads(&source, &archive))
        .await
        .map_err(AppError::internal)?
        .map_err(AppError::internal)?;

    let info = describe(config, &name)?;
    if let Err(err) = prune(config, config.backup_keep) {
        tracing::warn!(error = %err, "failed to prune old backups");
    }
    tracing::info!(backup = %info.name, bytes = info.database_bytes, "backup written");
    Ok(info)
}

fn describe(config: &Config, name: &str) -> AppResult<BackupInfo> {
    let stamp = check_name(name)?;
    let (database, uploads) = files(config, name)?;
    let database_bytes = fs::metadata(&database)
        .map_err(|_| AppError::NotFound)?
        .len();
    Ok(BackupInfo {
        name: name.to_owned(),
        created_at: stamp.and_utc(),
        database_bytes,
        uploads_bytes: fs::metadata(&uploads).ok().map(|m| m.len()),
    })
}

/// Every backup, newest first.
pub fn list(config: &Config) -> AppResult<Vec<BackupInfo>> {
    let Ok(entries) = fs::read_dir(config.backup_dir()) else {
        return Ok(Vec::new());
    };
    let mut backups: Vec<BackupInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let name = file.strip_suffix(".db")?;
            describe(config, name).ok()
        })
        .collect();
    backups.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(backups)
}

/// Deletes all but the newest `keep` backups. `keep == 0` keeps everything.
pub fn prune(config: &Config, keep: usize) -> AppResult<usize> {
    if keep == 0 {
        return Ok(0);
    }
    let mut removed = 0;
    for old in list(config)?.into_iter().skip(keep) {
        let (database, uploads) = files(config, &old.name)?;
        let _ = fs::remove_file(uploads);
        fs::remove_file(database).map_err(AppError::internal)?;
        removed += 1;
    }
    Ok(removed)
}

/// Whether the newest backup is older than the configured interval (so one is due).
pub fn due(config: &Config, now: DateTime<Utc>) -> AppResult<bool> {
    if config.backup_interval_hours == 0 {
        return Ok(false);
    }
    let interval = chrono::Duration::hours(
        i64::try_from(config.backup_interval_hours).unwrap_or(i64::MAX / 3_600_000),
    );
    Ok(list(config)?
        .first()
        .is_none_or(|newest| now - newest.created_at >= interval))
}

/// Restores a backup over the configured database and uploads. **Offline only**: the caller
/// (the `atlas restore` subcommand) runs it with no server attached to the database.
pub async fn restore(config: &Config, name: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let (database, uploads) = files(config, name).map_err(|_| {
        anyhow::anyhow!("{name:?} is not a backup name (expected atlas-YYYYMMDDTHHMMSSZ)")
    })?;
    anyhow::ensure!(database.exists(), "no backup at {}", database.display());
    let live = database_path(config).context("the configured database is in-memory")?;

    // Refuse while another process holds the database.
    if live.exists() {
        anyhow::ensure!(
            !in_use(&live).await,
            "{} is in use — stop Atlas before restoring",
            live.display()
        );
    }

    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", live.display()));
        if side.exists() {
            fs::remove_file(&side).with_context(|| format!("removing {}", side.display()))?;
        }
    }
    if let Some(parent) = live.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&database, &live)
        .with_context(|| format!("copying {} to {}", database.display(), live.display()))?;

    if uploads.exists() {
        let root = crate::storage::root(&config.data_dir);
        if root.exists() {
            let aside = config.data_dir.join(format!("uploads.before-{name}"));
            fs::rename(&root, &aside)
                .with_context(|| format!("moving the current uploads to {}", aside.display()))?;
        }
        let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(&uploads)?));
        // `unpack` refuses entries that would land outside the target directory.
        archive
            .unpack(&config.data_dir)
            .with_context(|| format!("extracting {}", uploads.display()))?;
    }
    Ok(())
}

/// Whether another connection holds the database open.
///
/// In WAL mode every open connection keeps a SHARED lock on the database file, so an
/// exclusive-locking-mode connection cannot take its EXCLUSIVE lock while a server is
/// attached — even an idle one.
pub async fn in_use(path: &Path) -> bool {
    use sqlx::ConnectOptions;
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .locking_mode(sqlx::sqlite::SqliteLockingMode::Exclusive)
        .busy_timeout(std::time::Duration::from_millis(200));
    let Ok(mut conn) = options.connect().await else {
        return true;
    };
    let locked = sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut conn)
        .await
        .is_ok();
    let _ = sqlx::query("ROLLBACK").execute(&mut conn).await;
    // Closed explicitly, and awaited: a dropped connection closes later on its worker
    // thread, and one still open while the file is replaced could write over the restore.
    let _ = sqlx::Connection::close(conn).await;
    !locked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_traverse() {
        assert!(check_name("atlas-20261101T020000Z").is_ok());
        assert!(check_name("../etc/passwd").is_err());
        assert!(check_name("atlas-20261101T020000Z/../../x").is_err());
    }

    #[tokio::test]
    async fn a_database_a_server_holds_is_in_use() {
        let temp = crate::test_support::TempDb::new();
        let config = temp.config();
        let db = Db::connect(&config).await.expect("connect");
        crate::db::migrate::run(&db).await.expect("migrate");
        let path = database_path(&config).expect("a file database");
        assert!(in_use(&path).await, "an open server holds it");
        db.close().await;
        assert!(!in_use(&path).await, "a closed one does not");
    }

    #[test]
    fn the_database_path_comes_from_the_url() {
        let mut config = Config {
            database_url: "sqlite://data/atlas.db?mode=rwc".to_owned(),
            ..Config::default()
        };
        assert_eq!(database_path(&config), Some(PathBuf::from("data/atlas.db")));
        config.database_url = "sqlite::memory:".to_owned();
        assert_eq!(database_path(&config), None);
    }
}

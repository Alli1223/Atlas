//! Helpers for tests.
//!
//! This module is compiled unconditionally rather than under `#[cfg(test)]`,
//! because integration tests in `backend/tests/` link against the library as an
//! ordinary dependency — they never see `cfg(test)` items. Gating it behind a
//! Cargo feature instead would mean the crate dev-depending on itself, which
//! builds the library twice. The cost of the current approach is a couple of
//! hundred bytes of dead code in the binary; the benefit is that unit tests and
//! integration tests share one definition of "a throwaway database".

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::config::Config;

/// A throwaway on-disk SQLite database in a private temporary directory,
/// removed when the value is dropped.
///
/// # Why a file and not `:memory:`
///
/// Atlas opens two pools, and the reader pool is a *separate connection*. Each
/// `sqlite::memory:` connection gets its own private database, so an in-memory
/// test would give the reader an empty database and the writer a different one —
/// the tests would pass or fail for reasons unrelated to the code. A temp file
/// also exercises the real WAL path, which is the thing worth testing.
#[derive(Debug)]
pub struct TempDb {
    dir: PathBuf,
}

impl TempDb {
    /// Creates a fresh temporary directory to hold the database.
    ///
    /// # Panics
    ///
    /// If the temporary directory cannot be created. Tests cannot proceed anyway.
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("atlas-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|err| panic!("failed to create temp dir {}: {err}", dir.display()));
        Self { dir }
    }

    /// The temporary directory backing this database.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path to the database file.
    pub fn path(&self) -> PathBuf {
        self.dir.join("atlas.db")
    }

    /// A [`Config`] pointing at this database, with otherwise safe test defaults.
    pub fn config(&self) -> Config {
        Config {
            database_url: format!("sqlite://{}", self.path().display()),
            data_dir: self.dir.clone(),
            workspace_dir: self.dir.join("workspaces"),
            // Small but >1, so reader concurrency is still exercised.
            reader_pool_size: 2,
            ..Config::default()
        }
    }
}

impl Default for TempDb {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Best-effort: a leaked temp dir must never fail a test run, and the WAL
        // and SHM sidecars go with the directory.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

//! Preparing a project's cloned repository for an agent run.
//!
//! [`agent::runner::RunRequest`](super::runner::RunRequest)'s `working_dir` needs a stable,
//! on-disk git checkout — stable because `--resume` is CWD-scoped
//! (`docs/research/claude-code-cli.md`), on-disk because Claude Code reads and writes real
//! files there. [`prepare`] is where that directory comes from: cloned on first use, then
//! fetched and hard-reset to a clean copy of the default branch on every use after that, so
//! an agent run never starts against whatever a previous one left behind.
//!
//! # One workspace per project, not per card or session
//!
//! `TODO.md`'s own naming (`~/.atlas/workspaces/{project}`) is followed literally here.
//! Concurrent agent runs against the same project sharing one workspace is a real hazard
//! this module does not guard against — that is what Phase 13's later "concurrency cap;
//! queue when saturated" item is for, matching how Atlas's SQLite pool already serializes
//! writers rather than trying to make concurrent writes safe.
//!
//! # Credentials never touch argv or the stored git config
//!
//! The PAT is passed to `git` via the `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` environment
//! variables (a documented mechanism since git 2.31, the same one GitHub Actions' own
//! checkout action uses), setting a one-shot `http.extraheader` for that invocation only.
//! Two things this avoids: an embedded-credential URL sitting in argv (world-readable via
//! `ps` on most systems, unlike a child's environment) for the life of the process, and that
//! same URL being written into the clone's `.git/config` as the `origin` remote, where it
//! would sit in cleartext on disk indefinitely and outlive the credential's own rotation.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::process::Command;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::integrations::github::store;
use crate::secrets::vault::Vault;
use crate::secrets::{self, Provider};

/// Where a project's workspace lives under the configured root.
#[must_use]
pub fn workspace_path(root: &Path, project_id: &str) -> PathBuf {
    root.join(project_id)
}

/// Clones (first use) or refreshes (every use after) a project's linked repo to a clean copy
/// of its default branch, and returns the path an agent run should use as its working
/// directory.
///
/// # Errors
///
/// - [`AppError::Conflict`] if the project has no repository linked, or its link's
///   credential no longer exists (deleted since linking) — the same "not set up for this"
///   answer `card_activity`/`create_branch` give elsewhere in the GitHub integration.
/// - [`AppError::Conflict`] if cloning a **new** workspace would push `workspace_dir` over
///   `quota_mb`. Refreshing an existing one is exempt: a fetch+reset does not itself grow
///   disk usage the way a fresh clone does.
/// - [`AppError::internal`] if `git` itself fails.
pub async fn prepare(
    db: &Db,
    vault: &Vault,
    root: &Path,
    quota_mb: u64,
    project_id: &str,
) -> AppResult<PathBuf> {
    let Some(repo) = store::find_project_repo(db, project_id).await? else {
        return Err(AppError::Conflict(
            "this project has no repository linked".to_owned(),
        ));
    };
    let Some(credential_id) = &repo.credential_id else {
        return Err(AppError::Conflict(
            "this project's repo link has no credential".to_owned(),
        ));
    };
    let credential = secrets::find_by_id(db, credential_id)
        .await?
        .filter(|c| c.provider == Provider::Github)
        .ok_or_else(|| {
            AppError::Conflict(
                "the credential this project's repo was linked with no longer exists".to_owned(),
            )
        })?;
    let token = vault.open(&credential)?;
    let auth_header = format!(
        "Authorization: Basic {}",
        BASE64.encode(format!("x-access-token:{}", token.expose()))
    );
    let url = format!("https://github.com/{}/{}.git", repo.owner, repo.repo);

    let path = workspace_path(root, project_id);

    if path.join(".git").is_dir() {
        refresh(&path, &url, &repo.default_branch, &auth_header).await?;
    } else {
        ensure_quota(root, quota_mb)?;
        clone(&path, &url, &repo.default_branch, &auth_header).await?;
    }

    Ok(path)
}

async fn clone(path: &Path, url: &str, default_branch: &str, auth_header: &str) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(AppError::internal)?;
    }
    run_git(
        None,
        [
            "clone",
            "--branch",
            default_branch,
            "--single-branch",
            url,
            &path.to_string_lossy(),
        ],
        Some(auth_header),
    )
    .await
}

async fn refresh(path: &Path, url: &str, default_branch: &str, auth_header: &str) -> AppResult<()> {
    run_git(
        Some(path),
        ["fetch", url, default_branch],
        Some(auth_header),
    )
    .await?;
    run_git(Some(path), ["reset", "--hard", "FETCH_HEAD"], None).await?;
    // Untracked files (new files an earlier agent run created but never committed) survive
    // `reset --hard`, which only touches tracked content — leaving them would let one run's
    // scratch files bleed into the next. `-x` is deliberately omitted: it would also remove
    // gitignored build caches (`target/`, `node_modules/`), punishing every run with a
    // from-scratch rebuild for a hazard `-fd` alone already closes.
    run_git(Some(path), ["clean", "-fd"], None).await
}

/// Runs `git` to completion, treating a non-zero exit as an error.
///
/// `auth_header`, when given, is passed as a one-shot `http.extraheader` via the
/// `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` environment mechanism — see the module doc for
/// why this, and not an embedded-credential URL, is what carries the secret.
async fn run_git<I, S>(cwd: Option<&Path>, args: I, auth_header: Option<&str>) -> AppResult<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(header) = auth_header {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraheader")
            .env("GIT_CONFIG_VALUE_0", header);
    }

    let output = command.output().await.map_err(AppError::internal)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::internal(anyhow::anyhow!("git failed: {stderr}")));
    }
    Ok(())
}

/// Refuses to grow the workspace root past `quota_mb` by starting a brand new clone.
///
/// Does not walk into `.git` internals for a byte-exact figure — a fast, approximate
/// directory walk is enough to catch runaway growth across many cloned projects, which is
/// the actual risk this guards against, not precise accounting.
fn ensure_quota(root: &Path, quota_mb: u64) -> AppResult<()> {
    let used_mb = directory_size(root) / (1024 * 1024);
    if used_mb >= quota_mb {
        return Err(AppError::Conflict(format!(
            "the workspace directory is at its {quota_mb} MB quota ({used_mb} MB used) — \
             free up space before cloning another repository"
        )));
    }
    Ok(())
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            total += directory_size(&entry.path());
        } else {
            total += metadata.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("atlas-workspace-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn workspace_path_is_keyed_by_project_id_under_the_root() {
        let root = Path::new("/var/atlas/workspaces");
        assert_eq!(
            workspace_path(root, "proj-1"),
            PathBuf::from("/var/atlas/workspaces/proj-1")
        );
    }

    #[test]
    fn an_empty_directory_measures_as_zero() {
        let dir = TempDir::new();
        assert_eq!(directory_size(&dir.0), 0);
    }

    #[test]
    fn directory_size_sums_nested_files() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join("a.txt"), b"12345").unwrap();
        std::fs::create_dir(dir.0.join("nested")).unwrap();
        std::fs::write(dir.0.join("nested/b.txt"), b"1234567890").unwrap();
        assert_eq!(directory_size(&dir.0), 15);
    }

    #[test]
    fn a_missing_directory_measures_as_zero_rather_than_erroring() {
        // ensure_quota must not fail just because nothing has been cloned yet.
        assert_eq!(directory_size(Path::new("/does/not/exist")), 0);
    }

    #[test]
    fn quota_refuses_a_new_clone_once_the_root_is_at_capacity() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join("big.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        // 2MB used, 1MB quota.
        let err = ensure_quota(&dir.0, 1).unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn quota_allows_a_new_clone_comfortably_under_capacity() {
        let dir = TempDir::new();
        std::fs::write(dir.0.join("small.bin"), vec![0u8; 1024]).unwrap();
        ensure_quota(&dir.0, 10_000).unwrap();
    }

    #[tokio::test]
    async fn run_git_surfaces_stderr_on_a_non_zero_exit() {
        let err = run_git(
            None,
            ["clone", "--branch", "x", "not-a-real-remote-url"],
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)), "{err:?}");
    }

    #[tokio::test]
    async fn run_git_succeeds_on_an_ordinary_local_command() {
        let dir = TempDir::new();
        run_git(None, ["init", &dir.0.to_string_lossy()], None)
            .await
            .unwrap();
        assert!(dir.0.join(".git").is_dir());
    }
}

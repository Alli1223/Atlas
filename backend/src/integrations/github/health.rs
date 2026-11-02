//! Repo link health (`TODO.md` Phase 12): find a broken link before a user does.
//!
//! Every linked repo is probed periodically with its own credential. The verdict lands on the
//! link (`project_repos.sync_status`) where the project settings page and the card dev panel
//! show it. A rename is not a breakage: GitHub redirects the old name, the response carries
//! the same immutable `repo_id` under its new `owner/name`, and Atlas follows it.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::api::AppState;
use crate::db::Db;
use crate::error::AppResult;
use crate::integrations::github::client::{GithubClient, RepoProbe};
use crate::integrations::github::store::{self, ProjectRepo};
use crate::scheduler;
use crate::secrets::{self, Provider};

/// How often every link is checked.
const INTERVAL: Duration = Duration::from_hours(1);

/// Probes one link and records the verdict. `client` is `None` when the link has no usable
/// credential. Returns the status written.
pub async fn check(
    db: &Db,
    client: Option<&GithubClient>,
    repo: &ProjectRepo,
    now: DateTime<Utc>,
) -> AppResult<&'static str> {
    let Some(client) = client else {
        store::set_sync_status(
            db,
            &repo.id,
            "no_credential",
            Some("The GitHub credential this link used was deleted. Relink the repository."),
            now,
        )
        .await?;
        return Ok("no_credential");
    };
    let (status, detail) = match client.probe_repo(&repo.repo_ref()).await {
        Ok(RepoProbe::Found(found)) if found.id == repo.repo_id => {
            let current = format!("{}/{}", repo.owner, repo.repo);
            let detail = if found.full_name.eq_ignore_ascii_case(&current) {
                None
            } else if let Some((owner, name)) = found.full_name.split_once('/') {
                store::follow_rename(db, &repo.id, owner, name, now).await?;
                Some(format!("Renamed on GitHub from {current}; followed."))
            } else {
                None
            };
            ("ok", detail)
        }
        Ok(RepoProbe::Found(other)) => (
            "not_found",
            Some(format!(
                "{}/{} now names a different repository ({}); the linked one was deleted or \
                 moved. Relink the project.",
                repo.owner, repo.repo, other.full_name
            )),
        ),
        Ok(RepoProbe::Unauthorized) => (
            "auth_failed",
            Some(
                "GitHub rejected the token. It was revoked or expired; replace the credential."
                    .to_owned(),
            ),
        ),
        Ok(RepoProbe::NotFound) => (
            "not_found",
            Some(format!(
                "{}/{} is gone, or the token can no longer see it.",
                repo.owner, repo.repo
            )),
        ),
        Err(err) => {
            // A blip is not a broken link: record it as `error` only if the link had no
            // verdict yet, so a good link is not flagged by one failed request.
            tracing::warn!(error = %err, repo = %format!("{}/{}", repo.owner, repo.repo), "repo health probe failed");
            if repo.sync_status == "unchecked" {
                (
                    "error",
                    Some("GitHub could not be reached to check this link.".to_owned()),
                )
            } else {
                return Ok("skipped");
            }
        }
    };
    store::set_sync_status(db, &repo.id, status, detail.as_deref(), now).await?;
    Ok(status)
}

/// Checks every linked repo.
pub async fn check_all(state: &AppState) -> AppResult<()> {
    let Some(vault) = state.vault.as_deref() else {
        return Ok(());
    };
    for repo in store::list_project_repos(&state.db).await? {
        let credential = match &repo.credential_id {
            Some(id) => secrets::find_by_id(&state.db, id)
                .await?
                .filter(|c| c.provider == Provider::Github),
            None => None,
        };
        let client = match credential {
            Some(credential) => Some(GithubClient::new(vault.open(&credential)?)?),
            None => None,
        };
        if let Err(err) = check(&state.db, client.as_ref(), &repo, crate::auth::now()).await {
            tracing::warn!(error = %err, "recording repo health failed");
        }
    }
    Ok(())
}

/// The [`scheduler::Job`] `main` arms at startup.
#[must_use]
pub fn job(state: AppState) -> scheduler::Job {
    scheduler::Job {
        name: "github-repo-health",
        interval: INTERVAL,
        run: Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                if let Err(err) = check_all(&state).await {
                    tracing::warn!(error = %err, "repo health pass failed");
                }
            })
        }),
    }
}

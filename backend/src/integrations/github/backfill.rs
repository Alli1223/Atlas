//! Backfilling git-links for cards whose branch/PR predates a project's repo link — created
//! directly on GitHub, or before Atlas ever knew the project had a repo.
//!
//! `TODO.md` Phase 12's "sync status + backfill" bullet. Runs once, right after a successful
//! `PUT /projects/{key}/repo` ([`crate::api::github::link_project_repo`]) — after that, any
//! installed webhook and [`crate::integrations::github::poll`]'s fallback both keep git-links
//! live, so there is nothing ongoing for this module to do.
//!
//! Scoped to the repo's first page of most-recently-updated PRs
//! ([`crate::integrations::github::client::GithubClient::list_prs`]'s own doc explains why) —
//! a backfill's job is to seed what is realistically still relevant, not replay a repo's
//! entire history.

use crate::api::AppState;
use crate::domain::card;
use crate::integrations::github::client::GithubClient;
use crate::integrations::github::store::ProjectRepo;
use crate::integrations::github::{poll, smart_commit};

/// How many of the repo's most-recently-updated PRs to check. One API call regardless — see
/// [`GithubClient::list_prs`].
const BACKFILL_PAGE_SIZE: u32 = 50;

/// Scans a newly-linked repo's recent PRs for ones whose branch names a card in this project,
/// and records them via [`poll::record_pr`] — the same "upsert the link, move to Done on a
/// merge" step the webhook receiver and the poll fallback both use.
///
/// Best-effort: called after the link itself has already succeeded, so a failure here must
/// not undo it — logged and swallowed, the same posture
/// [`crate::api::github::install_webhook`] takes for the same reason.
pub async fn backfill(state: &AppState, client: &GithubClient, repo: &ProjectRepo) {
    let repo_ref = repo.repo_ref();
    let prs = match client.list_prs(&repo_ref, BACKFILL_PAGE_SIZE).await {
        Ok(prs) => prs,
        Err(err) => {
            tracing::warn!(error = %err, repo = %repo_ref, "backfill: failed to list pull requests");
            return;
        }
    };

    for pr in prs {
        let Some(key) = smart_commit::key_in_branch(&pr.branch) else {
            continue;
        };
        let card = match card::find_by_key(&state.db, &key).await {
            Ok(Some(card)) if card.project_id == repo.project_id => card,
            // No such card, or the key belongs to a different project (a branch that just
            // happens to look like ATLAS-42 while this repo is linked to a different
            // project) — neither is this backfill's business.
            Ok(_) => continue,
            Err(err) => {
                tracing::warn!(error = %err, key = %key, "backfill: failed to look up a card");
                continue;
            }
        };

        if let Err(err) = poll::record_pr(state, &card, &pr).await {
            tracing::warn!(
                error = %err,
                card = %card.key,
                pr = pr.number,
                "backfill: failed to record a pull request"
            );
        }
    }
}

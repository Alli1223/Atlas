//! GitHub integration: link a project to a repo, drive the card→branch→PR flow,
//! receive webhooks, and act on smart commits (`TODO.md` Phase 12).
//!
//! # Layout
//!
//! - [`client`] — the one module that names `reqwest`. The [`client::GithubClient`]
//!   wraps the REST calls Atlas makes to `api.github.com`, and the module also
//!   holds the *pure* response-interpretation functions (token-expiry parsing, the
//!   `GET /user` classification, the CI rollup, PR merge-state) that carry all the
//!   logic worth testing — tested directly, with no network.
//! - [`validator`] — the [`crate::secrets::Validator`] for `Provider::Github`,
//!   routed to from [`crate::secrets::vault::default_validator`].
//! - [`webhook`] — HMAC-SHA256 signature verification over the raw body (the only
//!   thing standing between the unauthenticated receiver and card mutation) and
//!   the event payload types.
//! - [`smart_commit`] — the `ATLAS-42 #done #comment … #time 2h` parser and its
//!   application against the workflow engine.
//! - [`branch`] — turning a card into a sanitised git branch name.
//! - [`store`] — the `project_repos` / `card_git_links` / `card_worklogs` rows and
//!   their queries.
//! - [`poll`] — the poll fallback for a repo with no installed webhook: periodically
//!   re-checks a card's open PR links and applies the merge → Done transition the
//!   webhook receiver would have. Driven by [`crate::scheduler`].
//! - [`backfill`] — runs once, right after a repo is linked: seeds git-links for cards
//!   whose PR predates the link, sharing [`poll::record_pr`] with the poll fallback.
//!
//! # SSRF posture
//!
//! Every outbound call Atlas makes is to one fixed host, `api.github.com`, built
//! from a compile-time constant ([`client::GITHUB_API_BASE`]). No URL derived from
//! a webhook body, a repo payload, or any other attacker-influenced input is ever
//! fetched. Keeping it that way is the SSRF control: there is no code path that
//! turns remote data into an outbound request target. When Phase 12's "remote
//! links" or avatar-proxying land, they must not break that invariant — a URL that
//! came from GitHub is still attacker-influenced and must be validated against
//! internal ranges before it is fetched.

pub mod backfill;
pub mod branch;
pub mod client;
pub mod poll;
pub mod smart_commit;
pub mod store;
pub mod validator;
pub mod webhook;

use std::fmt;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::error::{AppError, AppResult};

/// A repository, addressed the way every GitHub REST path wants it: `{owner}/{repo}`.
///
/// Deliberately *not* keyed on the token owner's login. A GitHub App has no
/// `/user`, so any design that models "the repo of the authenticated user" is a
/// migration blocker — everything keys on `owner`/`repo` (and, in the database, on
/// the immutable `repo.id`), never on who the credential belongs to. See
/// `docs/research/github-api.md` §11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// The owning user or organisation login.
    pub owner: String,
    /// The repository name.
    pub repo: String,
}

impl RepoRef {
    /// Builds a repo reference.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
        }
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// Updates a repo link's stored health from the outcome of a live call Atlas just made
/// against it — the step shared by [`poll::poll_repo`] and
/// [`crate::api::github::card_activity`], the two places a call happens against an
/// already-linked repo outside a user-initiated action. (Linking itself always resets health
/// optimistically; see `store::upsert_project_repo`.)
///
/// `outcome` is `None` for success. A failure that is not one of ours (no
/// [`client::GithubApiError`] to recover a status from) or is transient — a 5xx, a timeout, a
/// secondary rate limit — leaves the stored status alone: none of those mean the *link* is
/// broken, only that this one call did not land.
pub async fn record_link_health(
    db: &Db,
    repo: &store::ProjectRepo,
    outcome: Option<&AppError>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let Some(err) = outcome else {
        return store::mark_repo_link_ok(db, &repo.id, now).await;
    };

    let reason = match client::github_status(err) {
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            "GitHub rejected the stored credential — it may have been revoked, or has lost \
             access to this repository."
        }
        Some(StatusCode::NOT_FOUND) => {
            "GitHub reports this repository no longer exists at this owner and name — it may \
             have been renamed or deleted."
        }
        _ => return Ok(()),
    };
    store::mark_repo_link_broken(db, &repo.id, reason, now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, migrate};
    use crate::domain::EstimationUnit;
    use crate::domain::project::{self, NewProject};
    use crate::integrations::github::client::GithubApiError;
    use crate::integrations::github::store::{LinkStatus, NewProjectRepo};
    use crate::test_support::TempDb;

    async fn fixture() -> (Db, TempDb, store::ProjectRepo) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let project = project::insert(
            &mut tx,
            &NewProject {
                key: "ATLAS".to_owned(),
                name: "Atlas".to_owned(),
                description: None,
                lead_id: None,
                template: "blank".to_owned(),
                cycles_enabled: false,
                estimation_unit: EstimationUnit::None,
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        let repo = store::upsert_project_repo(
            &mut tx,
            &NewProjectRepo {
                project_id: &project.id,
                credential_id: None,
                owner: "octocat",
                repo: "hello",
                repo_id: 42,
                default_branch: "main",
                branch_prefix: "feature",
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, repo)
    }

    fn github_error(status: StatusCode) -> AppError {
        AppError::internal(GithubApiError {
            status,
            body: String::new(),
        })
    }

    #[tokio::test]
    async fn a_401_or_403_marks_the_link_broken_as_a_credential_problem() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let (db, _temp, repo) = fixture().await;
            let err = github_error(status);
            record_link_health(&db, &repo, Some(&err), crate::auth::now())
                .await
                .unwrap();

            let found = store::find_project_repo(&db, &repo.project_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.link_status, LinkStatus::Broken);
            assert!(found.link_error.unwrap().contains("credential"));
        }
    }

    #[tokio::test]
    async fn a_404_marks_the_link_broken_as_a_gone_repo() {
        let (db, _temp, repo) = fixture().await;
        let err = github_error(StatusCode::NOT_FOUND);
        record_link_health(&db, &repo, Some(&err), crate::auth::now())
            .await
            .unwrap();

        let found = store::find_project_repo(&db, &repo.project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.link_status, LinkStatus::Broken);
        assert!(found.link_error.unwrap().contains("renamed or deleted"));
    }

    #[tokio::test]
    async fn a_transient_status_leaves_the_stored_health_alone() {
        let (db, _temp, repo) = fixture().await;
        // Break it first, on a real link problem...
        record_link_health(
            &db,
            &repo,
            Some(&github_error(StatusCode::NOT_FOUND)),
            crate::auth::now(),
        )
        .await
        .unwrap();

        // ...then a 500 (GitHub having a bad day) must not overwrite that verdict, and must
        // not clear it either — a transient error says nothing about whether the *link* is
        // still broken.
        let transient = github_error(StatusCode::INTERNAL_SERVER_ERROR);
        record_link_health(&db, &repo, Some(&transient), crate::auth::now())
            .await
            .unwrap();

        let found = store::find_project_repo(&db, &repo.project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            found.link_status,
            LinkStatus::Broken,
            "a transient error must not clear it"
        );
    }

    #[tokio::test]
    async fn an_error_with_no_recoverable_github_status_leaves_the_stored_health_alone() {
        let (db, _temp, repo) = fixture().await;
        let not_ours = AppError::internal(anyhow::anyhow!("the vault is not configured"));
        record_link_health(&db, &repo, Some(&not_ours), crate::auth::now())
            .await
            .unwrap();

        let found = store::find_project_repo(&db, &repo.project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.link_status, LinkStatus::Ok);
    }

    #[tokio::test]
    async fn success_marks_a_previously_broken_link_ok_again() {
        let (db, _temp, repo) = fixture().await;
        record_link_health(
            &db,
            &repo,
            Some(&github_error(StatusCode::NOT_FOUND)),
            crate::auth::now(),
        )
        .await
        .unwrap();

        record_link_health(&db, &repo, None, crate::auth::now())
            .await
            .unwrap();

        let found = store::find_project_repo(&db, &repo.project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.link_status, LinkStatus::Ok);
        assert_eq!(found.link_error, None);
    }
}

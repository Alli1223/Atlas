//! Poll fallback: catches a PR merging when Atlas has no way to hear about it from a webhook.
//!
//! `TODO.md` Phase 12's own framing: "poll fallback when no public webhook URL". Deliberately
//! narrow — this is not a general resync. CI status, reviews, and mergeable state are already
//! fetched live whenever a card-detail page is open (`GET /cards/{key}/activity`); the actual
//! gap a missing webhook leaves is the *automated* side effect nobody is around to trigger by
//! hand: nothing ever moves a merged card to Done. That is the one thing [`poll_all`] closes,
//! by re-checking every card whose stored PR link is still `open` and applying the same merge
//! → Done step [`crate::api::webhooks::receive`] would have.
//!
//! Auto-transitioning to In Progress on a PR's *open* action is **not** reproduced here — that
//! is a one-time signal the webhook payload's `action` field carries (`opened`/`reopened`
//! specifically, never `synchronize` or any other action on an already-open PR) that a bare
//! "what is this PR's state right now" poll snapshot cannot distinguish from "still open since
//! last time". Forcing every already-open PR's card to In Progress on every tick would fight a
//! human who deliberately moved it back — see [`crate::integrations::github::smart_commit::move_to_category`]
//! for why the merge case does not have this problem: it is idempotent (a no-op once the card
//! is already Done), so re-applying it on every tick for an unchanged merged PR is safe in a
//! way re-applying "just opened" is not.
//!
//! Driven by [`crate::scheduler`] via [`job`], on the repos [`store::list_unwebhooked_project_repos`]
//! names — see that function's doc for why the trigger is the per-repo `webhook_id` column
//! rather than the instance-wide `ATLAS_PUBLIC_URL` setting alone.

use std::sync::Arc;
use std::time::Duration;

use crate::api::AppState;
use crate::auth::now;
use crate::domain::StatusCategory;
use crate::domain::card::{self, Card};
use crate::error::AppResult;
use crate::integrations::github::RepoRef;
use crate::integrations::github::client::{GithubClient, PrState, PrSummary};
use crate::integrations::github::store::{self, CardGitLink, NewCardGitLink, ProjectRepo};
use crate::scheduler;
use crate::secrets::vault::Vault;
use crate::secrets::{self, Provider};

/// How often a pass runs. Not (yet) configurable — `TODO.md`'s config surface for this
/// integration does not otherwise expose polling intervals, and this is cheap enough (a
/// handful of DB reads plus, at most, one GitHub call per open PR link) to default to
/// something short rather than add a setting nobody has asked to tune yet.
const POLL_INTERVAL: Duration = Duration::from_mins(5);

/// The [`scheduler::Job`] `main` arms at startup.
#[must_use]
pub fn job(state: AppState) -> scheduler::Job {
    scheduler::Job {
        name: "github-poll-fallback",
        interval: POLL_INTERVAL,
        run: Arc::new(move || {
            let state = state.clone();
            Box::pin(async move { poll_all(&state).await })
        }),
    }
}

/// Runs one poll pass over every linked repo with no installed webhook.
///
/// Best-effort throughout: a failure for one repo or one card is logged and does not stop the
/// rest of the pass — a single stale credential or a transient GitHub error must not block
/// every other project's poll.
pub async fn poll_all(state: &AppState) {
    let Some(vault) = state.vault.as_deref() else {
        // No vault configured at all (a dev instance with no ATLAS_MASTER_KEY) — nothing here
        // could open a credential anyway.
        return;
    };

    let repos = match store::list_unwebhooked_project_repos(&state.db).await {
        Ok(repos) => repos,
        Err(err) => {
            tracing::warn!(error = %err, "poll fallback: failed to list linked repos");
            return;
        }
    };

    for repo in repos {
        if let Err(err) = poll_repo(state, vault, &repo).await {
            tracing::warn!(
                error = %err,
                repo = %format!("{}/{}", repo.owner, repo.repo),
                "poll fallback: failed to poll a repo"
            );
        }
    }
}

async fn poll_repo(state: &AppState, vault: &Vault, repo: &ProjectRepo) -> AppResult<()> {
    let Some(credential_id) = &repo.credential_id else {
        // Linked, but its credential was later deleted — nothing to poll with. The same
        // "inert until re-pointed" state `agent::workspace::prepare` treats as a conflict for
        // an interactive caller; here there is no caller to tell, so it is simply skipped.
        return Ok(());
    };
    let Some(credential) = secrets::find_by_id(&state.db, credential_id).await? else {
        return Ok(());
    };
    if credential.provider != Provider::Github {
        return Ok(());
    }

    let client = GithubClient::new(vault.open(&credential)?)?;
    let repo_ref = repo.repo_ref();

    let links = store::list_open_pr_links(&state.db, &repo.project_id).await?;
    for link in links {
        if let Err(err) = poll_pr_link(state, &client, &repo_ref, &link).await {
            tracing::warn!(
                error = %err,
                repo = %format!("{}/{}", repo.owner, repo.repo),
                pr = %link.git_ref,
                "poll fallback: failed to poll a pull request"
            );
        }
    }
    Ok(())
}

async fn poll_pr_link(
    state: &AppState,
    client: &GithubClient,
    repo_ref: &RepoRef,
    link: &CardGitLink,
) -> AppResult<()> {
    let Ok(number) = link.git_ref.parse::<i64>() else {
        return Ok(());
    };
    let Some(card) = card::find_by_id(&state.db, &link.card_id).await? else {
        return Ok(());
    };

    let pr = client.pr(repo_ref, number).await?;
    if pr.state == PrState::Open {
        // Nothing changed since it was last recorded — this is the common case on every tick
        // for a PR still awaiting review, and needs no write.
        return Ok(());
    }

    apply_terminal_state(state, &card, &pr).await
}

/// Records a PR's merge/close outcome, and — only on a merge — moves the card to Done.
///
/// [`crate::integrations::github::smart_commit::move_to_category`] is a no-op once the card is
/// already in the target category, which is what makes it safe to call on every tick for a
/// card whose merge was already applied on an earlier pass — see the module doc for why the
/// open→In-Progress transition does not get the same treatment.
async fn apply_terminal_state(state: &AppState, card: &Card, pr: &PrSummary) -> AppResult<()> {
    let state_str = match pr.state {
        PrState::Merged => "merged",
        PrState::Closed => "closed",
        PrState::Open => return Ok(()),
    };
    let number = pr.number.to_string();

    let mut tx = state.db.begin_write().await?;
    store::upsert_card_git_link(
        &mut tx,
        &NewCardGitLink {
            card_id: &card.id,
            kind: "pr",
            git_ref: &number,
            url: Some(&pr.html_url),
            state: Some(state_str),
            meta: Some(&pr.title),
        },
        now(),
    )
    .await?;
    tx.commit().await?;

    if pr.state == PrState::Merged {
        crate::integrations::github::smart_commit::move_to_category(
            &state.db,
            card,
            StatusCategory::Done,
            &card.creator_id,
            now(),
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, user};
    use crate::db::{Db, migrate};
    use crate::domain::card::{NewCard, Placement};
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    fn a_pr(number: i64, state: PrState) -> PrSummary {
        PrSummary {
            number,
            title: "Add login".to_owned(),
            html_url: format!("https://x/{number}"),
            state,
        }
    }

    /// A real project and card — `apply_terminal_state` reads `card.project_id`/`creator_id`
    /// and moves the card through the workflow engine, so a bare row is not enough.
    async fn fixture() -> (AppState, TempDb, Card) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let creator = user::insert(
            &mut tx,
            &user::NewUser {
                username: "pm".to_owned(),
                email: None,
                display_name: "PM".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();
        let project = template::create_project(
            &mut tx,
            Template::Programming,
            "ATLAS",
            "Atlas",
            None,
            None,
            now(),
        )
        .await
        .unwrap();
        let type_id: String = sqlx::query_scalar(
            "SELECT id FROM card_types WHERE project_id = ? ORDER BY level DESC, name LIMIT 1",
        )
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let created = card::create(
            &mut tx,
            &project,
            &NewCard {
                type_id,
                parent_id: None,
                summary: "Add login".to_owned(),
                description: None,
                status_id: None,
                priority_id: None,
                assignee_id: None,
                reporter_id: None,
                due_date: None,
                start_date: None,
                estimate: None,
                placement: Placement::Bottom,
            },
            &creator.id,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let state = AppState::new(db, temp.config());
        (state, temp, created)
    }

    #[tokio::test]
    async fn a_merged_pr_records_the_state_and_moves_the_card_to_done() {
        let (state, _temp, card) = fixture().await;
        assert!(
            !card.is_resolved(),
            "sanity: a fresh card is not already done"
        );

        apply_terminal_state(&state, &card, &a_pr(7, PrState::Merged))
            .await
            .unwrap();

        let links = store::list_card_git_links(&state.db, &card.id)
            .await
            .unwrap();
        let pr_link = links.iter().find(|l| l.kind == "pr").unwrap();
        assert_eq!(pr_link.state.as_deref(), Some("merged"));

        let updated = card::find_by_id(&state.db, &card.id)
            .await
            .unwrap()
            .unwrap();
        assert!(updated.is_resolved(), "the card must now be Done");
    }

    #[tokio::test]
    async fn reapplying_a_merge_the_card_is_already_in_is_a_safe_no_op() {
        let (state, _temp, card) = fixture().await;

        apply_terminal_state(&state, &card, &a_pr(7, PrState::Merged))
            .await
            .unwrap();
        let once_done = card::find_by_id(&state.db, &card.id)
            .await
            .unwrap()
            .unwrap();

        // Simulates the next poll tick seeing the same still-merged PR again.
        apply_terminal_state(&state, &once_done, &a_pr(7, PrState::Merged))
            .await
            .unwrap();
        let after_second_pass = card::find_by_id(&state.db, &card.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            after_second_pass.status_id, once_done.status_id,
            "the card must stay exactly where the first merge already put it"
        );
        assert!(after_second_pass.is_resolved());
    }

    #[tokio::test]
    async fn a_closed_unmerged_pr_records_its_state_but_does_not_move_the_card() {
        let (state, _temp, card) = fixture().await;

        apply_terminal_state(&state, &card, &a_pr(9, PrState::Closed))
            .await
            .unwrap();

        let links = store::list_card_git_links(&state.db, &card.id)
            .await
            .unwrap();
        let pr_link = links.iter().find(|l| l.kind == "pr").unwrap();
        assert_eq!(pr_link.state.as_deref(), Some("closed"));

        let updated = card::find_by_id(&state.db, &card.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !updated.is_resolved(),
            "closing without merging must not resolve the card"
        );
    }

    #[tokio::test]
    async fn a_still_open_pr_is_a_complete_no_op() {
        let (state, _temp, card) = fixture().await;

        apply_terminal_state(&state, &card, &a_pr(11, PrState::Open))
            .await
            .unwrap();

        let links = store::list_card_git_links(&state.db, &card.id)
            .await
            .unwrap();
        assert!(
            links.is_empty(),
            "an unchanged-open PR must not even write a link, {links:?}"
        );
    }
}

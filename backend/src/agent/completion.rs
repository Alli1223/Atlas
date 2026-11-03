//! What happens once a Claude Code run finishes: a transcript-summary comment, the run's own
//! workspace changes committed to a branch and opened as a PR (when there are any), and the
//! card moved into the project's configured completion category.
//!
//! Called from [`crate::agent::orchestrator`]'s detached drain task, after the session's
//! outcome is already recorded — so, like persisting the transcript and revoking the
//! capability, this is best-effort: a failure here is logged, never surfaced, because there is
//! no request left to answer by then. Each of the three actions runs independently of the
//! others' success — a comment failing to post must not also suppress the card move.
//!
//! # Why this runs unconditionally, not only when the agent asked for it
//!
//! The agent could have called `atlas_move_card`/`atlas_add_comment` itself mid-run via the
//! MCP tools, but nothing requires it to, and a run that forgot (or was denied the tool)
//! should not leave a finished card looking untouched. This runs after every `Completed`/
//! `CompletedWithDenials` session regardless of what the agent itself did or didn't call —
//! `move_to_category` is idempotent either way (a no-op if the card is already there).
//!
//! # Why a push, not just a branch
//!
//! [`crate::agent::workspace::prepare`]'s clone credential is one-shot and never persisted to
//! the workspace's git config (see its module doc) — the agent's own Bash tool has no
//! credential to push with. Whatever it wrote stays local until this step commits and pushes
//! it, reusing [`crate::agent::workspace::resolve_repo_auth`]'s exact credential path. Skipped
//! entirely (silently, not an error) when the run made no changes or commits, or when the
//! project has no repo linked at all — a card with no repo already got a 409 rather than ever
//! reaching here as a run.
//!
//! # Where the destination category comes from
//!
//! Per-project, not a constant — see `migrations/0017_agent_completion_category.sql` and
//! `domain::project::Project::agent_completion_category`. "First status in that category" is
//! the same substitution [`crate::integrations::github::smart_commit::move_to_category`]
//! already makes for a smart commit's `#done`, reused here rather than duplicated.

use chrono::{DateTime, Utc};

use crate::agent::workspace;
use crate::db::Db;
use crate::domain::agent_session::{AgentSession, AgentSessionStatus};
use crate::domain::card::{self as card_domain, Card};
use crate::domain::{comment, project};
use crate::error::AppResult;
use crate::integrations::github::client::{GithubClient, PrState};
use crate::integrations::github::store::{self, NewCardGitLink};
use crate::integrations::github::{branch, smart_commit};
use crate::secrets::vault::Vault;

/// The outcome fields [`run`] needs — a narrow, public view of
/// [`crate::agent::orchestrator`]'s own outcome type, which stays private to that module.
#[derive(Debug)]
pub struct Summary<'a> {
    pub status: AgentSessionStatus,
    pub result_text: Option<&'a str>,
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    /// The project's configured destination (`agent_settings.destination_status_id`), when the
    /// caller has one. `None` falls back to the project's `agent_completion_category`.
    pub destination_status_id: Option<&'a str>,
}

/// Runs every on-completion action for a session that ended `Completed` or
/// `CompletedWithDenials`. A no-op for every other status.
///
/// # Why `started_by` gates all of it
///
/// The comment's author, the card move's actor — both need *someone* to attribute the action
/// to, the same way [`crate::agent::mcp::tools`] refuses a tool call with no actor. A session
/// with no `started_by` does not happen in production (`api::agent_sessions::start_agent_session`
/// always passes the caller's id) — this is an honest "nothing to attribute to" skip, not a
/// workaround for a real gap.
pub async fn run(
    db: &Db,
    vault: &Vault,
    working_dir: &std::path::Path,
    session: &AgentSession,
    card: &Card,
    summary: &Summary<'_>,
    now: DateTime<Utc>,
) {
    // Every write this makes is tagged `agent:<session>`, the same as a tool call the agent
    // made itself via MCP — so history, the activity feed and automation can all tell this
    // run's own completion actions from its starter's.
    let actor_tag = format!("agent:{}", session.id);
    crate::activity::with_actor(actor_tag, async {
        if let (Some(actor), Some(text)) = (session.started_by.as_deref(), summary.result_text) {
            for line in text.lines() {
                if let Some(request) = line.trim().strip_prefix("ATLAS_BLOCKED_BY:")
                    && let Some((key, reason)) = request.split_once('|')
                    && let Err(err) =
                        crate::domain::automation::block(db, card, key.trim(), reason.trim(), actor)
                            .await
                {
                    tracing::warn!(error = %err, "could not record worker's reported blocker");
                }
            }
        }
        if !matches!(
            summary.status,
            AgentSessionStatus::Completed | AgentSessionStatus::CompletedWithDenials
        ) {
            return;
        }
        let Some(actor) = session.started_by.as_deref() else {
            tracing::debug!(
                session = %session.id,
                "a finished session has no started_by; skipping its on-completion actions"
            );
            return;
        };

        post_comment(db, card, actor, &session.provider, summary, now).await;
        attach_branch_and_pr(db, vault, working_dir, session, card, now).await;
        let automation = crate::domain::automation::info(db, &card.id).await;
        match automation {
            Ok(info) if info.blocked_by.iter().any(|dependency| !dependency.done) => {
                if let Err(err) =
                    crate::domain::automation::move_named(db, &card.id, "Blocked", actor).await
                {
                    tracing::warn!(error = %err, "could not keep ticket in blocked lane");
                }
            }
            Ok(info) if info.assignment.is_some() => {
                if let Err(err) =
                    crate::domain::automation::move_named(db, &card.id, "In Review", actor).await
                {
                    tracing::warn!(error = %err, "could not move agent ticket into review");
                }
            }
            _ => move_card(db, card, actor, summary.destination_status_id, now).await,
        }
    })
    .await;
}

/// Posts a transcript-summary comment, attributed to whoever started the run.
async fn post_comment(
    db: &Db,
    card: &Card,
    actor: &str,
    provider: &str,
    summary: &Summary<'_>,
    now: DateTime<Utc>,
) {
    let body = comment_body(summary, provider);
    let result: AppResult<()> = async {
        let mut tx = db.begin_write().await?;
        comment::insert(&mut tx, &card.id, actor, &body, now).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(
            card = %card.key,
            error = ?err,
            "failed to post an agent run's completion comment"
        );
    }
}

fn comment_body(summary: &Summary<'_>, provider: &str) -> String {
    let label = match provider {
        "codex" => "Codex",
        "antigravity" => "Antigravity",
        _ => "Claude Code",
    };
    let heading = match summary.status {
        AgentSessionStatus::CompletedWithDenials => {
            format!("**{label} finished** (some tool calls were denied)")
        }
        _ => format!("**{label} finished**"),
    };
    let mut body = heading;

    if let Some(text) = summary.result_text.map(str::trim).filter(|t| !t.is_empty()) {
        body.push_str("\n\n");
        body.push_str(text);
    }

    let mut meta = Vec::new();
    if let Some(turns) = summary.num_turns {
        meta.push(format!("{turns} turn{}", if turns == 1 { "" } else { "s" }));
    }
    if let Some(cost) = summary.total_cost_usd {
        meta.push(format!("${cost:.2}"));
    }
    if !meta.is_empty() {
        body.push_str("\n\n_");
        body.push_str(&meta.join(" · "));
        body.push('_');
    }

    body
}

/// Commits and pushes the run's workspace changes (if any) to a new branch, and opens a PR for
/// it. Does nothing — not even an error — when the project has no repo linked, or the run left
/// the workspace exactly as it found it.
async fn attach_branch_and_pr(
    db: &Db,
    vault: &Vault,
    working_dir: &std::path::Path,
    session: &AgentSession,
    card: &Card,
    now: DateTime<Utc>,
) {
    let result: AppResult<()> = async {
        // No repo linked, or its credential is gone: nothing to attach to. Not this step's
        // error to raise — the comment and the card move still happen.
        let Ok(auth) = workspace::resolve_repo_auth(db, vault, &card.project_id).await else {
            let branch_name = format!("{}-{}", branch::branch_name("agent", &card.key, &card.summary), session.id);
            if workspace::save_local_branch(working_dir, &branch_name, &card.summary).await? {
                let mut tx = db.begin_write().await?;
                store::upsert_card_git_link(&mut tx, &NewCardGitLink {
                    card_id: &card.id, kind: "branch", git_ref: &branch_name, url: None, state: None, meta: None,
                }, now).await?;
                if let Some(actor) = session.started_by.as_deref() {
                    let body = format!("Changes saved on local branch `{branch_name}` in the checkout at `{}`. Review this branch before merging it into the source repository.", working_dir.display());
                    comment::insert(&mut tx, &card.id, actor, &body, now).await?;
                }
                tx.commit().await?;
            }
            return Ok(());
        };

        // A rerun must get its own branch rather than colliding with an earlier run's PR.
        let branch_name = format!(
            "{}-{}",
            branch::branch_name(&auth.repo.branch_prefix, &card.key, &card.summary),
            session.id
        );
        let message = format!(
            "{}\n\nAutomated commit from a Claude Code run on {}.",
            card.summary, card.key
        );
        let pushed = workspace::commit_and_push(working_dir, &auth, &branch_name, &message).await?;
        if !pushed {
            return Ok(());
        }

        let repo_ref = auth.repo.repo_ref();
        let client = GithubClient::new(auth.token.clone())?;
        let branch_url = format!(
            "https://github.com/{}/{}/tree/{branch_name}",
            repo_ref.owner, repo_ref.repo
        );

        let mut tx = db.begin_write().await?;
        store::upsert_card_git_link(
            &mut tx,
            &NewCardGitLink {
                card_id: &card.id,
                kind: "branch",
                git_ref: &branch_name,
                url: Some(&branch_url),
                state: None,
                meta: None,
            },
            now,
        )
        .await?;
        tx.commit().await?;

        let pr = client
            .create_pr(
                &repo_ref,
                &branch_name,
                &auth.repo.default_branch,
                &format!("{}: {}", card.key, card.summary),
                Some(&format!(
                    "Automated PR from a Claude Code run on {}.",
                    card.key
                )),
            )
            .await?;

        let state_str = match pr.state {
            PrState::Open => "open",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        };
        let mut tx = db.begin_write().await?;
        store::upsert_card_git_link(
            &mut tx,
            &NewCardGitLink {
                card_id: &card.id,
                kind: "pr",
                git_ref: &pr.number.to_string(),
                url: Some(&pr.html_url),
                state: Some(state_str),
                meta: None,
            },
            now,
        )
        .await?;
        tx.commit().await?;

        Ok(())
    }
    .await;

    if let Err(err) = result {
        tracing::warn!(
            card = %card.key,
            error = ?err,
            "failed to attach a branch/PR after an agent run"
        );
    }
}

/// Moves the card to the caller's explicit destination status when it gave one (a project's
/// `agent_settings.destination_status_id`, defaulting to "In Review" — see `agent::dispatch`),
/// else the first status of the project's configured completion category, for callers with no
/// such setting to consult (the Auto board coordinator).
async fn move_card(
    db: &Db,
    card: &Card,
    actor: &str,
    destination_status_id: Option<&str>,
    now: DateTime<Utc>,
) {
    let result: AppResult<()> = async {
        if let Some(destination) = destination_status_id {
            if destination != card.status_id {
                let mut tx = db.begin_write().await?;
                card_domain::update(
                    &mut tx,
                    card,
                    &card_domain::CardPatch {
                        status_id: Some(destination.to_owned()),
                        ..card_domain::CardPatch::default()
                    },
                    Some(actor),
                    now,
                )
                .await?;
                tx.commit().await?;
            }
            return Ok(());
        }
        let Some(project) = project::find_by_id(db, &card.project_id).await? else {
            return Ok(());
        };
        smart_commit::move_to_category(db, card, project.agent_completion_category, actor, now)
            .await?;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(
            card = %card.key,
            error = ?err,
            "failed to move a card on run completion"
        );
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::project::ProjectPatch;
    use crate::domain::template::{self, Template};
    use crate::domain::{StatusCategory, config};
    use crate::secrets::Crypto;
    use crate::test_support::TempDb;

    async fn fixture() -> (Db, TempDb, Card) {
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
                type_id: type_id.clone(),
                parent_id: None,
                summary: "Fix the thing".to_owned(),
                description: Some("Do the needful.".to_owned()),
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

        (db, temp, created)
    }

    fn test_vault() -> Vault {
        Vault::new(
            Crypto::from_master_b64("dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=").unwrap(),
        )
    }

    fn session(card_id: &str, started_by: Option<&str>) -> AgentSession {
        AgentSession {
            provider: "claude".to_owned(),
            id: Uuid::new_v4().to_string(),
            card_id: card_id.to_owned(),
            claude_session_id: Some(Uuid::new_v4().to_string()),
            status: AgentSessionStatus::Completed,
            prompt: "do it".to_owned(),
            result_text: None,
            total_cost_usd: None,
            num_turns: None,
            error_message: None,
            started_by: started_by.map(ToOwned::to_owned),
            started_at: now(),
            ended_at: None,
            created_at: now(),
            updated_at: now(),
            queued: false,
            usage: None,
            duration_ms: None,
            branch: None,
            rerun_of: None,
        }
    }

    async fn comment_count(db: &Db, card_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE card_id = ?")
            .bind(card_id)
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_completed_run_posts_a_comment_attributed_to_the_starter() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let sess = session(&card.id, Some(&card.creator_id));

        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::Completed,
                result_text: Some("all done"),
                total_cost_usd: Some(0.12),
                num_turns: Some(3),
                destination_status_id: None,
            },
            now(),
        )
        .await;

        assert_eq!(comment_count(&db, &card.id).await, 1);
        let (author_id, body): (String, String) =
            sqlx::query_as("SELECT author_id, body FROM comments WHERE card_id = ?")
                .bind(&card.id)
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(author_id, card.creator_id);
        assert!(body.contains("all done"), "{body}");
        assert!(body.contains("3 turns"), "{body}");
        assert!(body.contains("$0.12"), "{body}");
    }

    #[tokio::test]
    async fn completed_with_denials_notes_it_in_the_comment() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let sess = session(&card.id, Some(&card.creator_id));

        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::CompletedWithDenials,
                result_text: None,
                total_cost_usd: None,
                num_turns: None,
                destination_status_id: None,
            },
            now(),
        )
        .await;

        let body: String = sqlx::query_scalar("SELECT body FROM comments WHERE card_id = ?")
            .bind(&card.id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert!(body.contains("some tool calls were denied"), "{body}");
    }

    #[tokio::test]
    async fn a_failed_session_gets_no_on_completion_actions() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let sess = session(&card.id, Some(&card.creator_id));
        let status_before = card.status_id.clone();

        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::Failed,
                result_text: None,
                total_cost_usd: None,
                num_turns: None,
                destination_status_id: None,
            },
            now(),
        )
        .await;

        assert_eq!(comment_count(&db, &card.id).await, 0);
        let status_after: String = sqlx::query_scalar("SELECT status_id FROM cards WHERE id = ?")
            .bind(&card.id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(status_after, status_before);
    }

    #[tokio::test]
    async fn no_started_by_skips_every_action() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let sess = session(&card.id, None);

        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::Completed,
                result_text: Some("done"),
                total_cost_usd: None,
                num_turns: None,
                destination_status_id: None,
            },
            now(),
        )
        .await;

        assert_eq!(comment_count(&db, &card.id).await, 0);
    }

    #[tokio::test]
    async fn the_card_moves_to_the_projects_configured_category() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();

        let mut tx = db.begin_write().await.unwrap();
        project::apply_patch(
            &mut tx,
            &card.project_id,
            &ProjectPatch {
                agent_completion_category: Some(StatusCategory::Done),
                ..ProjectPatch::default()
            },
            now(),
        )
        .await
        .unwrap();
        let done =
            config::first_status_in_category_tx(&mut tx, &card.project_id, StatusCategory::Done)
                .await
                .unwrap()
                .expect("the Programming template has a Done status");
        tx.commit().await.unwrap();

        let sess = session(&card.id, Some(&card.creator_id));
        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::Completed,
                result_text: None,
                total_cost_usd: None,
                num_turns: None,
                destination_status_id: None,
            },
            now(),
        )
        .await;

        let status_after: String = sqlx::query_scalar("SELECT status_id FROM cards WHERE id = ?")
            .bind(&card.id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(status_after, done.id);
    }

    #[tokio::test]
    async fn with_no_repo_linked_the_comment_and_move_still_happen() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let sess = session(&card.id, Some(&card.creator_id));

        run(
            &db,
            &vault,
            std::env::temp_dir().as_path(),
            &sess,
            &card,
            &Summary {
                status: AgentSessionStatus::Completed,
                result_text: Some("done"),
                total_cost_usd: None,
                num_turns: None,
                destination_status_id: None,
            },
            now(),
        )
        .await;

        assert_eq!(comment_count(&db, &card.id).await, 1);
        let links = store::list_card_git_links(&db, &card.id).await.unwrap();
        assert!(links.is_empty(), "no repo is linked, so nothing to attach");
    }

    #[tokio::test]
    async fn completion_uses_the_cards_current_status_instead_of_the_start_snapshot() {
        let (db, _temp, original) = fixture().await;
        let mut tx = db.begin_write().await.unwrap();
        project::apply_patch(
            &mut tx,
            &original.project_id,
            &ProjectPatch {
                agent_completion_category: Some(StatusCategory::Todo),
                ..ProjectPatch::default()
            },
            now(),
        )
        .await
        .unwrap();
        let progress = config::first_status_in_category_tx(
            &mut tx,
            &original.project_id,
            StatusCategory::InProgress,
        )
        .await
        .unwrap()
        .unwrap();
        card::update(
            &mut tx,
            &original,
            &card::CardPatch {
                status_id: Some(progress.id),
                ..card::CardPatch::default()
            },
            Some(&original.creator_id),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        move_card(&db, &original, &original.creator_id, None, now()).await;

        let current = card::find_by_id(&db, &original.id).await.unwrap().unwrap();
        assert_eq!(current.status_id, original.status_id);
    }
}

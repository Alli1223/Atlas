//! Wiring [`crate::agent::runner`], [`crate::agent::workspace`], and
//! [`crate::domain::agent_session`] into one "run Claude Code against this card" operation.
//!
//! [`start`] is deliberately not itself an HTTP handler — [`crate::api::agent_sessions`] is
//! that thin layer — so this stays testable against a fake [`AgentRunner`]/
//! [`WorkspacePreparer`] and a real (temporary) database, with no real subprocess, GitHub
//! call, or API cost, matching every other increment in this phase.
//!
//! # Why the drain is a detached task, not something the caller awaits
//!
//! A run can take minutes. [`start`] returns as soon as the session is recorded as
//! `running`; a `tokio::spawn`ed task owns draining [`RunHandle::events`] to its terminal
//! `result` event and calling [`agent_session::finish`] — the same "return once queued,
//! finish in the background" shape [`crate::agent::runner`]'s own stderr-drain task already
//! uses. A caller sees progress by polling `GET /agent-sessions/{id}` (or, later, subscribing
//! over WebSocket); nothing here blocks a request thread for the run's lifetime.
//!
//! # What happens if Atlas restarts mid-run
//!
//! The detached task dies with the process, and the session is left `running` forever with
//! no error recorded — a known, narrow gap (worth a stale-session sweep on startup later),
//! not silent data loss: the CLI's own child process is not orphaned regardless
//! ([`crate::agent::runner`]'s `KillOnDrop` + process-group kill), so nothing keeps spending
//! after Atlas is gone.

use crate::agent::claude_code::{self, Event, Outcome, ResultEvent};
use crate::agent::runner::{AgentRunner, RunEvent, RunHandle, RunLimits, RunRequest};
use crate::agent::workspace::WorkspacePreparer;
use crate::auth::now;
use crate::db::Db;
use crate::domain::agent_session::{
    self, AgentSession, AgentSessionStatus, NewAgentSession, SessionOutcome,
};
use crate::domain::card::Card;
use crate::error::AppResult;
use crate::secrets::vault::Vault;

/// What to run, and for whom. The API handler builds this from a card and its own defaults —
/// nothing here reads a `Card` field itself beyond `project_id`/`id`, so a caller cannot
/// forget to pass the prompt it actually means to send.
#[derive(Debug)]
pub struct StartRequest<'a> {
    pub card: &'a Card,
    pub prompt: String,
    pub allowed_tools: Vec<String>,
    pub limits: RunLimits,
    pub started_by: Option<&'a str>,
}

/// Prepares the card's project workspace, spawns the run, and records it as `running`.
///
/// Returns as soon as the session row exists. See the module doc for what happens after
/// that.
pub async fn start(
    db: &Db,
    vault: &Vault,
    runner: &dyn AgentRunner,
    preparer: &dyn WorkspacePreparer,
    request: StartRequest<'_>,
) -> AppResult<AgentSession> {
    let working_dir = preparer
        .prepare(db, vault, &request.card.project_id)
        .await?;

    let handle = runner
        .spawn(RunRequest {
            prompt: request.prompt.clone(),
            working_dir,
            resume_session_id: None,
            allowed_tools: request.allowed_tools,
            limits: request.limits,
        })
        .await?;

    let mut tx = db.begin_write().await?;
    let session = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id: &request.card.id,
            claude_session_id: &handle.session_id,
            prompt: &request.prompt,
            started_by: request.started_by,
        },
        now(),
    )
    .await?;
    tx.commit().await?;

    spawn_drain(db.clone(), session.clone(), handle);

    Ok(session)
}

/// Drains a run's events to its terminal `result` event (if any) and records the outcome.
///
/// Best-effort past the run itself finishing: a failure recording the outcome is logged, not
/// surfaced — there is no request left to answer by the time a detached task's own write
/// fails.
fn spawn_drain(db: Db, session: AgentSession, handle: RunHandle) {
    tokio::spawn(async move {
        let result_event = drain_to_result(handle).await;
        let outcome = outcome_for(result_event.as_ref());

        if let Err(err) = finish(&db, &session, &outcome).await {
            tracing::error!(
                session = %session.id,
                error = %err,
                "failed to record an agent session's outcome"
            );
        }
    });
}

/// Reads every event until the channel closes, keeping the last (only) `result` event seen.
///
/// Takes the whole [`RunHandle`], not just its `events` receiver, and never touches
/// `handle.cancel` — on purpose. An `async fn`'s generator transform drops a value as soon as
/// it is provably dead, which is *before* the lexical end of scope; extracting only
/// `handle.events` here would leave `handle.cancel` dead on arrival and drop its
/// `oneshot::Sender` at the very first `.await` below. `agent::runner`'s reader task cannot
/// tell that apart from an explicit [`RunHandle::cancel`] — a dropped sender and a sent signal
/// look identical to its `tokio::select!` — so the run would be killed and its events
/// truncated before this had read any of them. Keeping the whole handle alive keeps the
/// sender alive for exactly as long as a real caller holding it would.
async fn drain_to_result(mut handle: RunHandle) -> Option<ResultEvent> {
    let mut result_event = None;
    while let Some(event) = handle.events.recv().await {
        if let RunEvent::Parsed(event) = event
            && let Event::Result(result) = *event
        {
            result_event = Some(result);
        }
    }
    result_event
}

/// The status/outcome fields to record, as owned values — kept alive in the caller's scope so
/// [`SessionOutcome`]'s borrows have somewhere to point.
struct OutcomeFields {
    status: AgentSessionStatus,
    result_text: Option<String>,
    total_cost_usd: Option<f64>,
    num_turns: Option<i64>,
    error_message: Option<String>,
}

fn outcome_for(result: Option<&ResultEvent>) -> OutcomeFields {
    let Some(result) = result else {
        // The channel closed with no result event at all: the process died or was killed
        // before producing one (a crash, an OOM kill, an early `RunHandle::cancel`).
        return OutcomeFields {
            status: AgentSessionStatus::Failed,
            result_text: None,
            total_cost_usd: None,
            num_turns: None,
            error_message: Some("the run ended with no result event".to_owned()),
        };
    };

    let status = match claude_code::outcome(result) {
        Outcome::Completed => AgentSessionStatus::Completed,
        Outcome::CompletedWithDenials => AgentSessionStatus::CompletedWithDenials,
        Outcome::LimitReached => AgentSessionStatus::LimitReached,
        Outcome::Failed => AgentSessionStatus::Failed,
    };
    OutcomeFields {
        status,
        result_text: result.result.clone(),
        total_cost_usd: Some(result.total_cost_usd),
        num_turns: Some(i64::from(result.num_turns)),
        error_message: (!result.errors.is_empty()).then(|| result.errors.join("; ")),
    }
}

async fn finish(db: &Db, session: &AgentSession, outcome: &OutcomeFields) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    agent_session::finish(
        &mut tx,
        session,
        &SessionOutcome {
            status: outcome.status,
            result_text: outcome.result_text.as_deref(),
            total_cost_usd: outcome.total_cost_usd,
            num_turns: outcome.num_turns,
            error_message: outcome.error_message.as_deref(),
        },
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tokio::time::{Duration, sleep, timeout};
    use uuid::Uuid;

    use super::*;
    use crate::agent::BoxFuture;
    use crate::agent::runner::LocalRunner;
    use crate::auth::{Role, user};
    use crate::db::migrate;
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::template::{self, Template};
    use crate::secrets::Crypto;
    use crate::test_support::TempDb;

    /// A private temporary directory, removed on drop — mirrors
    /// `agent::runner`'s test helper of the same name.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("atlas-orchestrator-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A [`WorkspacePreparer`] that hands back a fixed local directory without touching `db`,
    /// `vault`, or a network — proof that `start` needs nothing more from the workspace layer
    /// than "some directory to run in", which is exactly the seam this module exists to draw.
    struct FixedWorkspace(PathBuf);

    impl WorkspacePreparer for FixedWorkspace {
        fn prepare<'a>(
            &'a self,
            _db: &'a Db,
            _vault: &'a Vault,
            _project_id: &'a str,
        ) -> BoxFuture<'a, AppResult<PathBuf>> {
            let path = self.0.clone();
            Box::pin(async move { Ok(path) })
        }
    }

    fn test_vault() -> Vault {
        Vault::new(
            Crypto::from_master_b64("dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=").unwrap(),
        )
    }

    fn fake_program(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-claude");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh\n{body}").unwrap();
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

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

    fn request<'a>(card: &'a Card, prompt: &str) -> StartRequest<'a> {
        StartRequest {
            card,
            prompt: prompt.to_owned(),
            allowed_tools: vec!["Read".to_owned()],
            limits: RunLimits {
                max_turns: 10,
                max_budget_usd: 1.0,
            },
            started_by: None,
        }
    }

    #[tokio::test]
    async fn starting_a_run_records_a_running_session_with_the_clis_session_id() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let program = fake_program(&scripts.0, "sleep 60");
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            request(&card, "Fix the thing\n\nDo the needful."),
        )
        .await
        .unwrap();

        assert_eq!(session.status, AgentSessionStatus::Running);
        assert_eq!(session.card_id, card.id);
        assert_eq!(session.prompt, "Fix the thing\n\nDo the needful.");
        assert!(session.claude_session_id.is_some());

        let stored = agent_session::find_by_id(&db, &session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, session.id);
    }

    #[tokio::test]
    async fn a_clean_run_finishes_completed_with_its_result_text_and_cost() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let program = fake_program(
            &scripts.0,
            r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"session_id":"whatever","result":"all done","total_cost_usd":0.12,"terminal_reason":"completed"}'"#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());

        let session = start(&db, &vault, &runner, &preparer, request(&card, "do it"))
            .await
            .unwrap();

        let finished = wait_for_finish(&db, &session.id).await;
        assert_eq!(finished.status, AgentSessionStatus::Completed);
        assert_eq!(finished.result_text.as_deref(), Some("all done"));
        assert_eq!(finished.total_cost_usd, Some(0.12));
        assert_eq!(finished.num_turns, Some(2));
        assert!(finished.error_message.is_none());
        assert!(finished.ended_at.is_some());
    }

    #[tokio::test]
    async fn a_run_hitting_max_turns_finishes_limit_reached_not_failed() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let program = fake_program(
            &scripts.0,
            r#"echo '{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":10,"session_id":"whatever","total_cost_usd":0.5,"errors":["Reached maximum number of turns (10)"],"terminal_reason":"max_turns"}'"#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());

        let session = start(&db, &vault, &runner, &preparer, request(&card, "do it"))
            .await
            .unwrap();

        let finished = wait_for_finish(&db, &session.id).await;
        assert_eq!(finished.status, AgentSessionStatus::LimitReached);
        assert!(finished.result_text.is_none());
        assert_eq!(
            finished.error_message.as_deref(),
            Some("Reached maximum number of turns (10)")
        );
    }

    #[tokio::test]
    async fn a_process_that_dies_with_no_result_event_finishes_failed() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        // Exits immediately, having said nothing on stdout.
        let program = fake_program(&scripts.0, "exit 1");
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());

        let session = start(&db, &vault, &runner, &preparer, request(&card, "do it"))
            .await
            .unwrap();

        let finished = wait_for_finish(&db, &session.id).await;
        assert_eq!(finished.status, AgentSessionStatus::Failed);
        assert_eq!(
            finished.error_message.as_deref(),
            Some("the run ended with no result event")
        );
    }

    /// Polls until the detached drain task has recorded a terminal outcome, or panics after a
    /// generous timeout — the drain happens on its own `tokio::spawn`ed task, so there is no
    /// handle to `.await` directly from a test.
    async fn wait_for_finish(db: &Db, session_id: &str) -> AgentSession {
        timeout(Duration::from_secs(5), async {
            loop {
                let session = agent_session::find_by_id(db, session_id)
                    .await
                    .unwrap()
                    .unwrap();
                if !session.status.is_running() {
                    return session;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the session must reach a terminal status")
    }
}

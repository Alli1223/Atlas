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
//!
//! # Transcript persistence
//!
//! Every raw line the CLI wrote — not just the terminal `result` — is written to
//! [`crate::domain::agent_session_transcript`] once the run ends, in one transaction, rather
//! than one row per line as they arrive: a run's lines live only in memory for its duration
//! (bounded by how long a run itself is), and batching means a transcript write can never
//! race the outcome write that follows it. A restart mid-run loses the in-flight lines the
//! same way it loses the outcome (see above) — there is nothing to batch-write yet.
//!
//! # Cancelling a run
//!
//! [`RunHandle::cancel`] already exists and is tested — what was missing was a way for an API
//! request (which has no reference to the `RunHandle` a detached `spawn_drain` task owns) to
//! reach it. [`CancelRegistry`] closes that gap: [`start`] registers a `oneshot::Sender` under
//! the new session's id, and [`cancel`] looks it up and fires it. The registry is in-memory
//! only, held by [`crate::api::AppState`] and passed in — the same "take what you need as a
//! parameter, never reach into `AppState` yourself" shape `start`'s own `db`/`vault`/`runner`/
//! `preparer` parameters already establish — so it does not survive a restart, which is fine:
//! nothing does yet (see above).
//!
//! Cancellation always wins the race against a natural finish once requested: if the drain
//! loop ever saw the cancel signal, the session's terminal status is `Cancelled` regardless of
//! whether a genuine `result` event also happened to arrive (a real possibility — the process
//! can finish in the same instant it is asked to stop) — it reflects what the caller asked
//! for, not a coin flip over which event the drain loop's `select!` happened to observe first.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

use crate::agent::claude_code::{self, Event, Outcome, ResultEvent};
use crate::agent::runner::{AgentRunner, RunEvent, RunHandle, RunLimits, RunRequest};
use crate::agent::workspace::WorkspacePreparer;
use crate::auth::now;
use crate::db::Db;
use crate::domain::agent_session::{
    self, AgentSession, AgentSessionStatus, NewAgentSession, SessionOutcome,
};
use crate::domain::agent_session_transcript;
use crate::domain::card::Card;
use crate::error::{AppError, AppResult};
use crate::secrets::vault::Vault;

/// Live runs' cancel signals, keyed by agent session id. See the module doc.
pub type CancelRegistry = Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>;

/// A poisoned lock still holds a perfectly usable `HashMap` — the panic that poisoned it
/// happened in an unrelated request's critical section, not in this one, and refusing to look
/// at cancel registrations forever after is a worse outcome than reading through the
/// poisoning. Shared by [`start`] and [`cancel`], the only two lockers.
fn lock(
    registry: &CancelRegistry,
) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<()>>> {
    registry.lock().unwrap_or_else(PoisonError::into_inner)
}

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
    registry: &CancelRegistry,
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

    let (cancel_tx, cancel_rx) = oneshot::channel();
    lock(registry).insert(session.id.clone(), cancel_tx);

    spawn_drain(
        db.clone(),
        session.clone(),
        handle,
        registry.clone(),
        cancel_rx,
    );

    Ok(session)
}

/// Requests cancellation of a running session. See the module doc for the semantics.
///
/// Only *requests* it — cancellation itself happens on the drain task, so this returns as soon
/// as the signal is queued, not once the session has actually reached `cancelled`. A caller
/// polls `GET /agent-sessions/{id}` to see it land.
///
/// # Errors
///
/// [`AppError::NotFound`] if there is no such session. [`AppError::Conflict`] if it is not
/// currently `running` — including the case where it finished naturally in the race between
/// whatever last told the caller it was running and this call, which is not distinguishable
/// from "was never running" and does not need to be.
pub async fn cancel(db: &Db, registry: &CancelRegistry, session_id: &str) -> AppResult<()> {
    let session = agent_session::find_by_id(db, session_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !session.status.is_running() {
        return Err(AppError::Conflict(format!(
            "this session already finished as {}",
            session.status
        )));
    }

    let sender = lock(registry).remove(session_id);
    let Some(sender) = sender else {
        // Genuinely `running` in the database, but no live `RunHandle` is reachable — Atlas
        // restarted since it started (the registry is in-memory only) or the drain task is in
        // the narrow window between finishing and its own `finish()` write landing. Either
        // way there is nothing left here to signal.
        return Err(AppError::Conflict(
            "this session is not currently reachable to cancel".to_owned(),
        ));
    };
    let _ = sender.send(());
    Ok(())
}

/// Drains a run's events to its terminal `result` event (if any), persists every raw line as
/// the session's transcript, and records the outcome.
///
/// Best-effort past the run itself finishing: a failure persisting the transcript or
/// recording the outcome is logged, not surfaced — there is no request left to answer by the
/// time a detached task's own write fails.
fn spawn_drain(
    db: Db,
    session: AgentSession,
    handle: RunHandle,
    registry: CancelRegistry,
    cancel_rx: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let drained = drain_to_result(handle, cancel_rx).await;

        // The run is over one way or another; nothing can cancel it any more, so its entry
        // (if the session was not already cancelled, which removes it itself) has no further
        // reason to exist.
        lock(&registry).remove(&session.id);

        if let Err(err) = persist_transcript(&db, &session.id, &drained.lines).await {
            tracing::error!(
                session = %session.id,
                error = %err,
                "failed to persist an agent session's transcript"
            );
        }

        let outcome = outcome_for_drain(&drained);
        if let Err(err) = finish(&db, &session, &outcome).await {
            tracing::error!(
                session = %session.id,
                error = %err,
                "failed to record an agent session's outcome"
            );
        }
    });
}

/// What draining a run's events to completion produced: every raw line, in arrival order, the
/// terminal `result` event if the run reached one, and whether an external [`cancel`] request
/// landed during the drain.
struct Drained {
    lines: Vec<String>,
    result: Option<ResultEvent>,
    cancelled: bool,
}

/// Reads every event until the channel closes, collecting each raw line and keeping the last
/// (only) `result` event seen — and, once `cancel_rx` fires, calling [`RunHandle::cancel`] and
/// continuing to drain (rather than stopping immediately), so whatever the process managed to
/// say before it actually died still ends up in the transcript.
///
/// Takes the whole [`RunHandle`], not just its `events` receiver, and never touches
/// `handle.cancel` from anywhere but the `select!` branch below — on purpose. An `async fn`'s
/// generator transform drops a value as soon as it is provably dead, which is *before* the
/// lexical end of scope; extracting only `handle.events` here would leave `handle.cancel` dead
/// on arrival and drop its `oneshot::Sender` at the very first `.await` below. `agent::runner`'s
/// reader task cannot tell that apart from an explicit [`RunHandle::cancel`] — a dropped sender
/// and a sent signal look identical to its `tokio::select!` — so the run would be killed and
/// its events truncated before this had read any of them. Keeping the whole handle alive keeps
/// the sender alive for exactly as long as a real caller holding it would.
async fn drain_to_result(mut handle: RunHandle, mut cancel_rx: oneshot::Receiver<()>) -> Drained {
    let mut lines = Vec::new();
    let mut result_event = None;
    let mut cancelled = false;
    loop {
        tokio::select! {
            biased;
            // `if !cancelled` drops this branch from the select entirely once it has already
            // fired once — a resolved `oneshot::Receiver` is safe to poll again (it just keeps
            // reporting the same outcome), but there is nothing left to do the second time,
            // and the guard makes that explicit rather than relying on that being harmless.
            _ = &mut cancel_rx, if !cancelled => {
                cancelled = true;
                handle.cancel();
            }
            event = handle.events.recv() => {
                let Some(event) = event else { break };
                match event {
                    RunEvent::Parsed(line, event) => {
                        lines.push(line);
                        if let Event::Result(result) = *event {
                            result_event = Some(result);
                        }
                    }
                    RunEvent::Unparseable(line) => lines.push(line),
                }
            }
        }
    }
    Drained {
        lines,
        result: result_event,
        cancelled,
    }
}

/// Writes a run's whole transcript in one transaction — see the module doc for why this is
/// batched rather than appended line-by-line as events arrive.
async fn persist_transcript(db: &Db, session_id: &str, lines: &[String]) -> AppResult<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin_write().await?;
    for (seq, line) in lines.iter().enumerate() {
        let seq =
            i64::try_from(seq).expect("a transcript will never remotely approach i64::MAX lines");
        agent_session_transcript::append(&mut tx, session_id, seq, line, now()).await?;
    }
    tx.commit().await?;
    Ok(())
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

/// The outcome to record for a drained run, folding in [`cancel`] having won the race — see
/// the module doc for why a deliberate cancel always overrides whatever [`outcome_for`] would
/// otherwise have derived from the events actually seen.
fn outcome_for_drain(drained: &Drained) -> OutcomeFields {
    if drained.cancelled {
        let result = drained.result.as_ref();
        return OutcomeFields {
            status: AgentSessionStatus::Cancelled,
            // A result event can still have arrived even on a cancelled run — the process was
            // already finishing when the cancel signal landed — in which case its cost/turns
            // are real numbers worth keeping, even though the session did not run to a normal
            // completion.
            result_text: None,
            total_cost_usd: result.map(|r| r.total_cost_usd),
            num_turns: result.map(|r| i64::from(r.num_turns)),
            error_message: None,
        };
    }
    outcome_for(drained.result.as_ref())
}

fn outcome_for(result: Option<&ResultEvent>) -> OutcomeFields {
    let Some(result) = result else {
        // The channel closed with no result event at all: the process died or was killed
        // before producing one (a crash, an OOM kill).
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
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
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
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
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
    async fn every_raw_line_is_persisted_to_the_transcript_in_arrival_order() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let program = fake_program(
            &scripts.0,
            r#"
            echo '{"type":"system","subtype":"init","session_id":"whatever"}'
            echo 'this is not json'
            echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"whatever","total_cost_usd":0.01,"terminal_reason":"completed"}'
            "#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
        .await
        .unwrap();
        wait_for_finish(&db, &session.id).await;

        let lines = agent_session_transcript::list_for_session(&db, &session.id)
            .await
            .unwrap();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0].seq, 0);
        assert!(lines[0].line.contains(r#""subtype":"init""#));
        assert_eq!(lines[1].seq, 1);
        assert_eq!(lines[1].line, "this is not json");
        assert_eq!(lines[2].seq, 2);
        assert!(lines[2].line.contains(r#""type":"result""#));
    }

    #[tokio::test]
    async fn cancelling_a_running_session_finishes_it_cancelled() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        // Never produces a result event on its own — only cancellation ends this run.
        let program = fake_program(&scripts.0, "sleep 60");
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
        .await
        .unwrap();
        assert_eq!(session.status, AgentSessionStatus::Running);

        cancel(&db, &registry, &session.id).await.unwrap();

        let finished = wait_for_finish(&db, &session.id).await;
        assert_eq!(finished.status, AgentSessionStatus::Cancelled);
        assert!(finished.error_message.is_none());
        assert!(finished.ended_at.is_some());

        // The registry entry is gone either way once the run is over — nothing left to
        // double-cancel.
        assert!(!lock(&registry).contains_key(&session.id));
    }

    #[tokio::test]
    async fn cancelling_a_session_that_already_finished_is_a_conflict() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let program = fake_program(
            &scripts.0,
            r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"whatever","total_cost_usd":0.01,"terminal_reason":"completed"}'"#,
        );
        let runner = LocalRunner::with_program(program.to_string_lossy());
        let preparer = FixedWorkspace(std::env::temp_dir());
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
        .await
        .unwrap();
        wait_for_finish(&db, &session.id).await;

        let err = cancel(&db, &registry, &session.id).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn cancelling_an_unknown_session_is_not_found() {
        let (db, _temp, _card) = fixture().await;
        let registry = CancelRegistry::default();

        let err = cancel(&db, &registry, "no-such-session").await.unwrap_err();
        assert!(matches!(err, AppError::NotFound), "{err:?}");
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
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
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
        let registry = CancelRegistry::default();

        let session = start(
            &db,
            &vault,
            &runner,
            &preparer,
            &registry,
            request(&card, "do it"),
        )
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

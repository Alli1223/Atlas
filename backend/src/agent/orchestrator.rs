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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

use crate::agent::claude_code::{self, Event, Outcome, ResultEvent};
use crate::agent::mcp;
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
use crate::secrets::Secret;
use crate::secrets::vault::Vault;

/// The last lines of every live run, for a viewer who opens the session mid-run (the
/// WebSocket only carries lines from the moment it subscribed). Keyed by agent session id;
/// an entry exists only while its run is executing.
pub type LiveBuffers = Arc<Mutex<HashMap<String, VecDeque<String>>>>;

/// How many lines a live buffer keeps.
pub const LIVE_BUFFER_LINES: usize = 500;

/// A snapshot of a live run's buffered lines, or `None` if it is not executing here.
pub fn live_lines(buffers: &LiveBuffers, session_id: &str) -> Option<Vec<String>> {
    buffers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(session_id)
        .map(|lines| lines.iter().cloned().collect())
}

/// Per-run knobs beyond the prompt and tools, all defaulting to the conservative choice.
#[derive(Debug, Default)]
pub struct RunOptions {
    /// `--permission-mode`; `None` = [`RunRequest::default_permission_mode`].
    pub permission_mode: Option<String>,
    /// `--disallowedTools`.
    pub disallowed_tools: Vec<String>,
    /// Bill this Anthropic key instead of the machine's Claude login.
    pub api_key: Option<Secret<String>>,
    /// Continue an earlier CLI session instead of starting one (`--resume`).
    pub resume_session_id: Option<String>,
    /// The Atlas session this one re-runs or continues.
    pub rerun_of: Option<String>,
}

/// What happens around a run beyond recording it: streaming each line live, and the
/// completion actions (summary comment, moving the card, telling the starter).
#[derive(Debug, Clone)]
pub struct AfterRun {
    /// Where live lines and the finish event go.
    pub hub: crate::activity::Hub,
    /// Where late joiners read the lines so far.
    pub live: LiveBuffers,
    /// The card's project, for scoping the live frames.
    pub project_id: String,
    /// Post the result as a comment on the card.
    pub post_summary: bool,
    /// Move the card here when the run completes; `None` = leave it.
    pub destination_status_id: Option<String>,
}

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
    /// The database URL the spawned `atlas mcp` child should open, passed into its
    /// `--mcp-config` environment so it does not depend on inheriting one. The handler takes
    /// it from `config.database_url`.
    pub database_url: String,
    /// Everything else, defaulting to the conservative choice.
    pub options: RunOptions,
    /// Live streaming and completion actions; `None` = record only.
    pub after: Option<AfterRun>,
}

/// What [`launch_prepared`] needs to spawn a run whose session row already exists.
#[derive(Debug)]
pub struct Launch {
    pub prompt: String,
    pub claude_session_id: String,
    pub allowed_tools: Vec<String>,
    pub limits: RunLimits,
    pub database_url: String,
    pub options: RunOptions,
    pub after: Option<AfterRun>,
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

    // The session row and its capability token must exist *before* the run is spawned: the
    // token rides in the run's `--mcp-config`, and it is keyed on the session id. So the CLI
    // session id is generated here rather than scraped back from the runner, letting the whole
    // record — row, capability, mcp config — be built up front and the run spawned last. A
    // continued run keeps the CLI session it resumes.
    let claude_session_id = request
        .options
        .resume_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut tx = db.begin_write().await?;
    let session = agent_session::insert(
        &mut tx,
        &NewAgentSession {
            card_id: &request.card.id,
            claude_session_id: &claude_session_id,
            prompt: &request.prompt,
            started_by: request.started_by,
        },
        now(),
    )
    .await?;
    if let Some(rerun_of) = &request.options.rerun_of {
        agent_session::set_rerun_of(&mut tx, &session.id, rerun_of).await?;
    }
    let session = agent_session::find_by_id_tx(&mut tx, &session.id)
        .await?
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("the session just inserted is missing"))
        })?;
    tx.commit().await?;

    launch_prepared(
        db,
        runner,
        registry,
        &session,
        working_dir,
        Launch {
            prompt: request.prompt,
            claude_session_id,
            allowed_tools: request.allowed_tools,
            limits: request.limits,
            database_url: request.database_url,
            options: request.options,
            after: request.after,
        },
    )
    .await?;
    Ok(session)
}

/// Spawns a run for a session that already has a row and a prepared workspace: mints its MCP
/// capability, spawns the CLI, registers it for cancellation, and starts draining it. Used by
/// [`start`] and by the queue dispatcher for a session that waited for a free slot.
pub async fn launch_prepared(
    db: &Db,
    runner: &dyn AgentRunner,
    registry: &CancelRegistry,
    session: &AgentSession,
    working_dir: PathBuf,
    launch: Launch,
) -> AppResult<()> {
    let minted = {
        let mut tx = db.begin_write().await?;
        let minted = mcp::capability::mint(&mut tx, &session.id, now()).await?;
        tx.commit().await?;
        minted
    };

    // The child MCP server is this very binary in `mcp` mode; falling back to `atlas` on PATH
    // keeps a test or an unusual deployment working if the exe path cannot be read.
    let atlas_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "atlas".to_owned());
    let mcp_config =
        mcp::mcp_config_json(&atlas_exe, &minted.token, &launch.database_url).to_string();

    // The run may use its own allowlist *and* Atlas's tools.
    let mut allowed_tools = launch.allowed_tools;
    allowed_tools.extend(mcp::ALLOWED_TOOLS.iter().map(|t| (*t).to_owned()));

    let resuming = launch.options.resume_session_id.is_some();
    let handle = match runner
        .spawn(RunRequest {
            prompt: launch.prompt.clone(),
            working_dir: working_dir.clone(),
            resume_session_id: launch.options.resume_session_id.clone(),
            session_id: (!resuming).then(|| launch.claude_session_id.clone()),
            allowed_tools,
            mcp_config: Some(mcp_config),
            permission_mode: launch
                .options
                .permission_mode
                .clone()
                .unwrap_or_else(RunRequest::default_permission_mode),
            limits: launch.limits,
            disallowed_tools: launch.options.disallowed_tools.clone(),
            api_key: launch.options.api_key.clone(),
        })
        .await
    {
        Ok(handle) => handle,
        // The row exists but the run never started: record it as failed and revoke its
        // capability, rather than leaving a `running` session that will never drain.
        Err(err) => {
            fail_unstarted(db, session, &err).await;
            return Err(err);
        }
    };

    let (cancel_tx, cancel_rx) = oneshot::channel();
    lock(registry).insert(session.id.clone(), cancel_tx);

    spawn_drain(
        db.clone(),
        session.clone(),
        handle,
        registry.clone(),
        cancel_rx,
        launch.after,
        working_dir,
    );

    Ok(())
}

/// Marks a session that failed to spawn as `failed` and revokes its capability. Best-effort:
/// a bookkeeping failure here must not mask the spawn error the caller is about to see.
pub async fn fail_unstarted(db: &Db, session: &AgentSession, err: &AppError) {
    let outcome = OutcomeFields {
        status: AgentSessionStatus::Failed,
        result_text: None,
        total_cost_usd: None,
        num_turns: None,
        error_message: Some(format!("the run could not be started: {err}")),
    };
    if let Err(e) = finish(db, session, &outcome).await {
        tracing::error!(session = %session.id, error = ?e, "failed to record an unstarted session");
    }
    revoke_capability(db, &session.id).await;
}

/// Revokes a session's MCP capability. Best-effort: the capability is already dead the moment
/// the session leaves `running` (see `mcp::capability::resolve`), so this is durable cleanup,
/// not the live guard.
async fn revoke_capability(db: &Db, session_id: &str) {
    let result: AppResult<()> = async {
        let mut tx = db.begin_write().await?;
        mcp::capability::revoke(&mut tx, session_id).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        tracing::warn!(session = %session_id, error = ?err, "failed to revoke an agent capability");
    }
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
    after: Option<AfterRun>,
    working_dir: PathBuf,
) {
    tokio::spawn(async move {
        if let Some(after) = &after {
            lock_live(&after.live).insert(session.id.clone(), VecDeque::new());
        }
        let drained = drain_to_result(handle, cancel_rx, &session.id, after.as_ref()).await;

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

        // The run is over: revoke its MCP capability so the token cannot be reused. Ordered
        // after `finish` so the session is already non-`running` — a tool call racing this is
        // refused by the status check regardless.
        revoke_capability(&db, &session.id).await;

        record_details(&db, &session.id, &drained, &working_dir).await;
        if let Some(after) = &after {
            lock_live(&after.live).remove(&session.id);
            completion_actions(&db, &session, &outcome, after).await;
        }
    });
}

fn lock_live(live: &LiveBuffers) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<String>>> {
    live.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The workspace's current branch, if it is a git checkout. Best-effort and bounded.
async fn current_branch(dir: &std::path::Path) -> Option<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let branch = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !branch.is_empty() && branch != "HEAD").then_some(branch)
}

/// Records a finished run's usage, duration and branch. Best-effort, like the outcome.
async fn record_details(
    db: &Db,
    session_id: &str,
    drained: &Drained,
    working_dir: &std::path::Path,
) {
    let usage = drained
        .result
        .as_ref()
        .map(|r| serde_json::json!({ "usage": r.usage, "modelUsage": r.model_usage }).to_string());
    let duration = drained
        .result
        .as_ref()
        .and_then(|r| r.duration_ms)
        .and_then(|d| i64::try_from(d).ok());
    let branch = current_branch(working_dir).await;
    if let Err(err) = agent_session::record_details(
        db,
        session_id,
        usage.as_deref(),
        duration,
        branch.as_deref(),
    )
    .await
    {
        tracing::warn!(session = %session_id, error = %err, "failed to record run details");
    }
}

/// The summary comment a finished run posts: its status, its result, and the numbers.
fn summary_body(outcome: &OutcomeFields, branch: Option<&str>) -> String {
    let mut body = format!("🤖 **Claude Code {}**", outcome.status);
    if let Some(text) = outcome
        .result_text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    {
        let text: String = text.chars().take(8000).collect();
        body.push_str("\n\n");
        body.push_str(&text);
    }
    if let Some(error) = &outcome.error_message {
        body.push_str("\n\n> ");
        body.push_str(error);
    }
    let mut facts = Vec::new();
    if let Some(cost) = outcome.total_cost_usd {
        facts.push(format!("${cost:.2}"));
    }
    if let Some(turns) = outcome.num_turns {
        facts.push(format!("{turns} turns"));
    }
    if let Some(branch) = branch {
        facts.push(format!("branch `{branch}`"));
    }
    if !facts.is_empty() {
        body.push_str("\n\n_");
        body.push_str(&facts.join(" · "));
        body.push('_');
    }
    body
}

/// What happens when a run ends: the result as a comment, the card moved to the project's
/// destination (only on a clean completion), and the starter told. Every write is attributed
/// to the starter with an `agent:<session>` actor tag. Best-effort throughout: the run is
/// already recorded, and one failed follow-up must not hide the others.
async fn completion_actions(
    db: &Db,
    session: &AgentSession,
    outcome: &OutcomeFields,
    after: &AfterRun,
) {
    let Ok(Some(fresh)) = agent_session::find_by_id(db, &session.id).await else {
        return;
    };
    let Ok(Some(card)) = crate::domain::card::find_by_id(db, &session.card_id).await else {
        return;
    };
    let completed = outcome.status == AgentSessionStatus::Completed;
    let actor = format!("agent:{}", session.id);

    if after.post_summary
        && let Some(author) = &session.started_by
    {
        let body = summary_body(outcome, fresh.branch.as_deref());
        let result: AppResult<()> = crate::activity::with_actor(actor.clone(), async {
            let mut tx = db.begin_write().await?;
            crate::domain::comment::insert(&mut tx, &card.id, author, &body, now()).await?;
            tx.commit().await?;
            Ok(())
        })
        .await;
        if let Err(err) = result {
            tracing::warn!(session = %session.id, error = %err, "failed to post the run summary");
        }
    }

    if completed
        && let Some(destination) = &after.destination_status_id
        && *destination != card.status_id
    {
        let result: AppResult<()> = crate::activity::with_actor(actor.clone(), async {
            let mut tx = db.begin_write().await?;
            crate::domain::card::update(
                &mut tx,
                &card,
                &crate::domain::card::CardPatch {
                    status_id: Some(destination.clone()),
                    ..crate::domain::card::CardPatch::default()
                },
                session.started_by.as_deref(),
                now(),
            )
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await;
        if let Err(err) = result {
            // A workflow may refuse the move (a validator, a missing field). The run still
            // succeeded; the card simply stays where it is, and the reason is logged.
            tracing::info!(session = %session.id, error = %err, "the completed run's card could not be moved");
        }
    }

    if let Some(starter) = &session.started_by {
        let title = if completed {
            format!("Claude finished {}", card.key)
        } else {
            format!("Claude run on {} ended: {}", card.key, outcome.status)
        };
        let notice = crate::notify::Notice {
            user_id: starter.clone(),
            kind: crate::notify::Kind::Agent,
            card_id: Some(card.id.clone()),
            project_id: Some(after.project_id.clone()),
            actor_id: None,
            title,
            body: outcome
                .result_text
                .as_ref()
                .map(|t| t.chars().take(280).collect()),
            link: Some(format!("/cards/{}", card.key)),
        };
        if let Err(err) = crate::notify::deliver(db, &after.hub, notice).await {
            tracing::warn!(session = %session.id, error = %err, "failed to notify the run's starter");
        }
    }

    after.hub.publish(crate::activity::HubMessage {
        project_id: Some(after.project_id.clone()),
        user_id: None,
        payload: serde_json::json!({
            "type": "agent.finished",
            "sessionId": session.id,
            "cardKey": card.key,
            "status": outcome.status,
        }),
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
async fn drain_to_result(
    mut handle: RunHandle,
    mut cancel_rx: oneshot::Receiver<()>,
    session_id: &str,
    after: Option<&AfterRun>,
) -> Drained {
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
                let line = match event {
                    RunEvent::Parsed(line, event) => {
                        if let Event::Result(result) = *event {
                            result_event = Some(result);
                        }
                        line
                    }
                    RunEvent::Unparseable(line) => line,
                };
                if let Some(after) = after {
                    publish_line(after, session_id, &line);
                }
                lines.push(line);
            }
        }
    }
    Drained {
        lines,
        result: result_event,
        cancelled,
    }
}

/// Streams one line live: into the late-joiner buffer, and onto the hub for open sockets.
fn publish_line(after: &AfterRun, session_id: &str, line: &str) {
    {
        let mut buffers = lock_live(&after.live);
        if let Some(buffer) = buffers.get_mut(session_id) {
            if buffer.len() >= LIVE_BUFFER_LINES {
                buffer.pop_front();
            }
            buffer.push_back(line.to_owned());
        }
    }
    after.hub.publish(crate::activity::HubMessage {
        project_id: Some(after.project_id.clone()),
        user_id: None,
        payload: serde_json::json!({ "type": "agent.event", "sessionId": session_id, "line": line }),
    });
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
            database_url: "sqlite::memory:".to_owned(),
            options: RunOptions::default(),
            after: None,
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

    async fn capability_rows(db: &Db, session_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_capabilities WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_run_is_spawned_with_the_atlas_mcp_config_and_tools() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        let args_file = scripts.0.join("args.txt");
        // The fake CLI records the exact argv it was spawned with, then completes.
        let program = fake_program(
            &scripts.0,
            &format!(
                "printf '%s\\n' \"$@\" > {}\necho '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"session_id\":\"x\",\"result\":\"ok\",\"total_cost_usd\":0.0,\"terminal_reason\":\"completed\"}}'",
                args_file.display()
            ),
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
            request(&card, "go"),
        )
        .await
        .unwrap();
        wait_for_finish(&db, &session.id).await;

        let argv = std::fs::read_to_string(&args_file).expect("the fake CLI recorded its argv");
        assert!(argv.contains("--mcp-config"), "no --mcp-config in: {argv}");
        assert!(
            argv.contains("--strict-mcp-config"),
            "no --strict-mcp-config: {argv}"
        );
        assert!(
            argv.contains("mcp__atlas__atlas_move_card"),
            "the atlas tools were not allowlisted: {argv}"
        );
        // The CLI session id we generated is the one it was told to use.
        let cli_id = session.claude_session_id.as_deref().unwrap();
        assert!(
            argv.contains(cli_id),
            "the pre-generated session id was not passed: {argv}"
        );
    }

    #[tokio::test]
    async fn a_finished_runs_capability_is_revoked() {
        let (db, _temp, card) = fixture().await;
        let vault = test_vault();
        let scripts = TempDir::new();
        // Sleep before completing, so the capability is observably present *during* the run —
        // making this prove both that start() minted it and that the drain revokes it.
        let program = fake_program(
            &scripts.0,
            r#"sleep 0.3; echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"session_id":"x","result":"ok","total_cost_usd":0.0,"terminal_reason":"completed"}'"#,
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
            request(&card, "go"),
        )
        .await
        .unwrap();
        assert_eq!(
            capability_rows(&db, &session.id).await,
            1,
            "the capability must exist while the run is live"
        );

        wait_for_finish(&db, &session.id).await;
        timeout(Duration::from_secs(5), async {
            loop {
                if capability_rows(&db, &session.id).await == 0 {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the capability must be revoked after the run finishes");
    }
}

//! Starting runs under a project's settings and the instance's concurrency cap.
//!
//! [`submit`] is what "Run with Claude" calls. It reads the project's
//! [`crate::domain::agent_settings`], and either launches the run now or — when
//! `ATLAS_AGENT_MAX_CONCURRENT` runs are already executing — records it as a queued session
//! carrying its launch parameters. [`dispatch_queued`] (run by a job every few seconds) launches
//! queued sessions oldest-first as slots free up. The queue is a column on `agent_sessions`, so
//! it survives a restart.
//!
//! [`sweep_stale`] runs once at startup: a session left executing by a previous process lost
//! its drain task with that process and would otherwise read `running` forever.

use serde::{Deserialize, Serialize};

use crate::agent::orchestrator::{self, AfterRun, Launch, RunOptions, StartRequest};
use crate::agent::runner::RunLimits;
use crate::api::AppState;
use crate::domain::agent_session::{self, AgentSession, NewAgentSession};
use crate::domain::agent_settings::{self, AgentSettings};
use crate::domain::card::Card;
use crate::error::{AppError, AppResult};
use crate::secrets::{self, Provider, Secret};

/// The launch parameters stored on a queued session. No secret is stored: an API key is
/// re-opened from the vault by credential id when the run actually launches.
#[derive(Debug, Serialize, Deserialize)]
struct StoredLaunch {
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    permission_mode: String,
    max_turns: u32,
    max_budget_usd: f64,
    api_credential_id: Option<String>,
    resume_session_id: Option<String>,
}

/// What to run.
#[derive(Debug)]
pub struct Submission<'a> {
    /// The card.
    pub card: &'a Card,
    /// The prompt.
    pub prompt: String,
    /// Who asked.
    pub started_by: &'a str,
    /// The session this re-runs or continues.
    pub rerun_of: Option<String>,
    /// Continue this CLI session (`--resume`) rather than starting fresh.
    pub resume_session_id: Option<String>,
}

/// The destination a completed run moves its card to: the project's setting, else a status
/// called "In Review", else none.
async fn destination(
    state: &AppState,
    project_id: &str,
    settings: &AgentSettings,
) -> AppResult<Option<String>> {
    if let Some(id) = &settings.destination_status_id {
        return Ok(Some(id.clone()));
    }
    Ok(sqlx::query_scalar(
        "SELECT id FROM statuses WHERE project_id = ? AND name = 'In Review' COLLATE NOCASE",
    )
    .bind(project_id)
    .fetch_optional(state.db.reader())
    .await?)
}

async fn after_run(
    state: &AppState,
    project_id: &str,
    settings: &AgentSettings,
) -> AppResult<AfterRun> {
    Ok(AfterRun {
        hub: state.hub.clone(),
        live: state.agent_live.clone(),
        project_id: project_id.to_owned(),
        post_summary: settings.post_summary,
        destination_status_id: destination(state, project_id, settings).await?,
    })
}

/// Opens the API key a project bills, if it opted in.
async fn api_key(
    state: &AppState,
    credential_id: Option<&str>,
) -> AppResult<Option<Secret<String>>> {
    let Some(id) = credential_id else {
        return Ok(None);
    };
    let vault = state
        .vault
        .as_deref()
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the vault is not configured")))?;
    let credential = secrets::find_by_id(&state.db, id)
        .await?
        .filter(|c| c.provider == Provider::Anthropic)
        .ok_or_else(|| {
            AppError::Conflict(
                "The project's Anthropic API key is gone. Pick another in its agent settings."
                    .to_owned(),
            )
        })?;
    Ok(Some(vault.open(&credential)?))
}

fn stored(settings: &AgentSettings, resume: Option<String>) -> StoredLaunch {
    StoredLaunch {
        allowed_tools: settings.effective_allowed_tools(),
        disallowed_tools: settings.disallowed_tools.clone(),
        permission_mode: settings.permission_mode.clone(),
        max_turns: settings.max_turns,
        max_budget_usd: settings.max_budget_usd,
        api_credential_id: settings
            .use_api_key
            .then(|| settings.api_credential_id.clone())
            .flatten(),
        resume_session_id: resume,
    }
}

/// Starts a run now, or queues it if the instance is at its concurrency cap.
pub async fn submit(state: &AppState, submission: Submission<'_>) -> AppResult<AgentSession> {
    let vault = state.vault.as_deref().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!(
            "the secrets vault is not configured: set ATLAS_MASTER_KEY"
        ))
    })?;
    let card = submission.card;
    let settings = agent_settings::get(&state.db, &card.project_id).await?;
    let launch = stored(&settings, submission.resume_session_id.clone());

    let executing = agent_session::executing_count(&state.db).await?;
    let cap = i64::try_from(state.config.agent_max_concurrent).unwrap_or(i64::MAX);
    if executing >= cap {
        let claude_session_id = submission
            .resume_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut tx = state.db.begin_write().await?;
        let session = agent_session::insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card.id,
                claude_session_id: &claude_session_id,
                prompt: &submission.prompt,
                started_by: Some(submission.started_by),
            },
            crate::auth::now(),
        )
        .await?;
        agent_session::mark_queued(
            &mut tx,
            &session.id,
            &serde_json::to_string(&launch).map_err(AppError::internal)?,
        )
        .await?;
        if let Some(rerun_of) = &submission.rerun_of {
            agent_session::set_rerun_of(&mut tx, &session.id, rerun_of).await?;
        }
        tx.commit().await?;
        return agent_session::find_by_id(&state.db, &session.id)
            .await?
            .ok_or(AppError::NotFound);
    }

    let api_key = api_key(state, launch.api_credential_id.as_deref()).await?;
    let after = after_run(state, &card.project_id, &settings).await?;
    orchestrator::start(
        &state.db,
        vault,
        state.agent_runner.as_ref(),
        state.workspace_preparer.as_ref(),
        &state.cancel_registry,
        StartRequest {
            card,
            prompt: submission.prompt,
            allowed_tools: launch.allowed_tools,
            limits: RunLimits {
                max_turns: launch.max_turns,
                max_budget_usd: launch.max_budget_usd,
            },
            started_by: Some(submission.started_by),
            database_url: state.config.database_url.clone(),
            options: RunOptions {
                permission_mode: Some(launch.permission_mode),
                disallowed_tools: launch.disallowed_tools,
                api_key,
                resume_session_id: launch.resume_session_id,
                rerun_of: submission.rerun_of,
            },
            after: Some(after),
        },
    )
    .await
}

/// Launches queued sessions, oldest first, while there are free slots. Returns how many.
pub async fn dispatch_queued(state: &AppState) -> AppResult<usize> {
    let Some(vault) = state.vault.as_deref() else {
        return Ok(0);
    };
    let cap = i64::try_from(state.config.agent_max_concurrent).unwrap_or(i64::MAX);
    let mut launched = 0;
    while agent_session::executing_count(&state.db).await? < cap {
        let Some((session, raw)) = agent_session::claim_queued(&state.db).await? else {
            break;
        };
        let result = async {
            let launch: StoredLaunch = serde_json::from_str(&raw).map_err(AppError::internal)?;
            let card = crate::domain::card::find_by_id(&state.db, &session.card_id)
                .await?
                .ok_or(AppError::NotFound)?;
            let settings = agent_settings::get(&state.db, &card.project_id).await?;
            let working_dir = state
                .workspace_preparer
                .prepare(&state.db, vault, &card.project_id)
                .await?;
            let api_key = api_key(state, launch.api_credential_id.as_deref()).await?;
            let after = after_run(state, &card.project_id, &settings).await?;
            orchestrator::launch_prepared(
                &state.db,
                state.agent_runner.as_ref(),
                &state.cancel_registry,
                &session,
                working_dir,
                Launch {
                    prompt: session.prompt.clone(),
                    claude_session_id: session.claude_session_id.clone().unwrap_or_default(),
                    allowed_tools: launch.allowed_tools,
                    limits: RunLimits {
                        max_turns: launch.max_turns,
                        max_budget_usd: launch.max_budget_usd,
                    },
                    database_url: state.config.database_url.clone(),
                    options: RunOptions {
                        permission_mode: Some(launch.permission_mode),
                        disallowed_tools: launch.disallowed_tools,
                        api_key,
                        resume_session_id: launch.resume_session_id,
                        rerun_of: None,
                    },
                    after: Some(after),
                },
            )
            .await
        }
        .await;
        match result {
            Ok(()) => launched += 1,
            Err(err) => {
                // `launch_prepared` already failed a spawn error itself; anything earlier
                // (a missing repo, a gone key) is recorded here.
                if let Ok(Some(current)) = agent_session::find_by_id(&state.db, &session.id).await
                    && current.status.is_running()
                {
                    orchestrator::fail_unstarted(&state.db, &current, &err).await;
                }
                tracing::warn!(session = %session.id, error = %err, "failed to launch a queued run");
            }
        }
    }
    Ok(launched)
}

/// Fails every run a previous process left executing. Call once, at startup.
pub async fn sweep_stale(state: &AppState) -> AppResult<usize> {
    let swept = agent_session::sweep_stale(&state.db, crate::auth::now()).await?;
    if !swept.is_empty() {
        tracing::warn!(
            count = swept.len(),
            "marked runs left executing by a restart as failed"
        );
    }
    Ok(swept.len())
}

/// Cancels a queued session (it has no process to signal).
pub async fn cancel_queued(state: &AppState, session: &AgentSession) -> AppResult<()> {
    let mut tx = state.db.begin_write().await?;
    agent_session::finish(
        &mut tx,
        session,
        &agent_session::SessionOutcome {
            status: agent_session::AgentSessionStatus::Cancelled,
            result_text: None,
            total_cost_usd: None,
            num_turns: None,
            error_message: Some("Cancelled while queued."),
        },
        crate::auth::now(),
    )
    .await?;
    sqlx::query("UPDATE agent_sessions SET queued = 0 WHERE id = ?")
        .bind(&session.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

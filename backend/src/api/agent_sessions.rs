//! "Run Claude Code against this card": starting a run, and reading a card's session history.
//!
//! The actual wiring — preparing the workspace, spawning the CLI, persisting the session and
//! its eventual outcome — lives in [`crate::agent::orchestrator`]; this module is the thin
//! HTTP layer over it plus the wire-facing prompt Atlas builds from a card, matching this
//! phase's own naming for the feature (`TODO.md`'s "card → task binding").
//!
//! # Scoping
//!
//! Starting or cancelling a run costs real money (or stops it costing more) and pushes real
//! commits, so both are `Card(Member)`/`AgentSession(Member)` like every other card-mutating
//! action (`POST /cards/{key}/branch`, `/pr`). Reading a session — whether the list for a
//! card, one by id, or its transcript — is `Viewer`, like reading anything else about a card.
//!
//! # On completion
//!
//! A run posts its result as a comment, moves its card to the project's configured destination
//! (default: "In Review" — `TODO.md` Phase 13 flagged "move to the backlog" as an unusual home
//! for finished work, so the destination is a per-project setting with backlog selectable),
//! records the branch it left its work on, and notifies whoever started it. See
//! [`crate::agent::orchestrator`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::agent::dispatch::{self, Submission};
use crate::agent::orchestrator;
use crate::agent::providers::AgentProvider;
use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::agent_session::{self, AgentSession};
use crate::domain::agent_session_transcript::{self, TranscriptLine};
use crate::domain::agent_settings::{self, AgentSettings};
use crate::domain::card;
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /cards/{key}/agent-sessions`. The project's [`AgentSettings`] (tools,
/// permission mode, turns/budget, destination, API-key opt-in) drive everything else — this
/// is the one per-run choice the caller still makes, since the same card can be run with
/// whichever CLI is actually signed in.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartAgentSessionRequest {
    #[serde(default)]
    pub provider: AgentProvider,
}

/// Builds the prompt Atlas sends: the card's summary, and its description if it has one and
/// it is not blank.
///
/// Not reconstructed later from the card — [`crate::domain::agent_session::AgentSession`]
/// stores the exact string sent, since a card can be edited after the fact and a session's
/// record of what it was actually asked must not drift with it.
fn build_prompt(summary: &str, description: Option<&str>) -> String {
    match description.map(str::trim) {
        Some(description) if !description.is_empty() => format!("{summary}\n\n{description}"),
        _ => summary.to_owned(),
    }
}

/// Starts a Claude Code run against a card: the prompt is the card's summary and description,
/// sent into a clean, up-to-date checkout of the card's project repo.
#[utoipa::path(
    post,
    path = "/cards/{key}/agent-sessions",
    tag = "agent-sessions",
    request_body = Option<StartAgentSessionRequest>,
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 201, description = "The run, recorded as running (and `queued` when the instance is at its concurrency cap)", body = AgentSession),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "The card's project has no linked repo, or its credential is gone", body = Problem),
        (status = 500, description = "The vault is not configured, or the CLI failed to spawn", body = Problem),
    )
)]
async fn start_agent_session(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    request: Option<Json<StartAgentSessionRequest>>,
) -> AppResult<(StatusCode, Json<AgentSession>)> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    if crate::domain::automation::info(&state.db, &card.id)
        .await?
        .blocked_by
        .iter()
        .any(|dependency| !dependency.done)
    {
        return Err(AppError::Conflict(
            "This ticket has unfinished blocking dependencies. Finish them before starting work."
                .to_owned(),
        ));
    }
    let prompt = build_prompt(&card.summary, card.description.as_deref());
    let session = dispatch::submit(
        &state,
        Submission {
            provider: request
                .map(|Json(request)| request.provider)
                .unwrap_or_default(),
            card: &card,
            prompt,
            started_by: current.id(),
            rerun_of: None,
            resume_session_id: None,
        },
    )
    .await?;

    tracing::info!(card = %card.key, session = %session.id, queued = session.queued, "started a Claude Code run against a card");
    Ok((StatusCode::CREATED, Json(session)))
}

/// A card's Claude Code sessions, most recent first.
#[utoipa::path(
    get,
    path = "/cards/{key}/agent-sessions",
    tag = "agent-sessions",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 200, description = "The card's sessions, most recent first", body = Vec<AgentSession>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_card_agent_sessions(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<AgentSession>>> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let sessions = agent_session::list_for_card(&state.db, &card.id).await?;
    Ok(Json(sessions))
}

/// One session, by id — for polling a run's status until it finishes.
#[utoipa::path(
    get,
    path = "/agent-sessions/{id}",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session id")),
    responses(
        (status = 200, description = "The session", body = AgentSession),
        (status = 404, description = "No such session", body = Problem),
    )
)]
async fn get_agent_session(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<AgentSession>> {
    let session = agent_session::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(session))
}

/// A session's full `stream-json` transcript, in arrival order.
#[utoipa::path(
    get,
    path = "/agent-sessions/{id}/transcript",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session id")),
    responses(
        (status = 200, description = "The session's transcript lines, in arrival order", body = Vec<TranscriptLine>),
        (status = 404, description = "No such session", body = Problem),
    )
)]
async fn get_agent_session_transcript(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<TranscriptLine>>> {
    let session = agent_session::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    // While a run executes its lines are only in memory (the transcript is written once, at
    // the end); a viewer opening it mid-run gets the live buffer, then follows `/ws`.
    if session.status.is_running()
        && let Some(live) = orchestrator::live_lines(&state.agent_live, &id)
    {
        let at = now();
        return Ok(Json(
            live.into_iter()
                .enumerate()
                .map(|(seq, line)| TranscriptLine {
                    id: format!("live-{seq}"),
                    session_id: id.clone(),
                    seq: i64::try_from(seq).unwrap_or(i64::MAX),
                    line,
                    created_at: at,
                })
                .collect(),
        ));
    }
    let lines = agent_session_transcript::list_for_session(&state.db, &id).await?;
    Ok(Json(lines))
}

/// Requests cancellation of a running session.
///
/// Answers as soon as the cancel signal is queued — the run itself may take a moment longer
/// to actually stop (its process group has to be killed and the drain task has to notice).
/// Poll `GET /agent-sessions/{id}` to see the session actually reach `cancelled`.
#[utoipa::path(
    post,
    path = "/agent-sessions/{id}/cancel",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session id")),
    responses(
        (status = 202, description = "Cancellation requested"),
        (status = 404, description = "No such session", body = Problem),
        (status = 409, description = "The session is not currently running", body = Problem),
    )
)]
async fn cancel_agent_session(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let session = agent_session::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if session.queued && session.status.is_running() {
        dispatch::cancel_queued(&state, &session).await?;
    } else {
        orchestrator::cancel(&state.db, &state.cancel_registry, &id).await?;
    }
    tracing::info!(session = %id, "requested cancellation of a Claude Code run");
    Ok(StatusCode::ACCEPTED)
}

/// The body of `POST /agent-sessions/{id}/continue`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContinueRequest {
    /// What to tell the agent next.
    pub prompt: String,
}

/// Runs a finished session's prompt again, from scratch.
#[utoipa::path(
    post,
    path = "/agent-sessions/{id}/rerun",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session to re-run")),
    responses(
        (status = 201, description = "The new run", body = AgentSession),
        (status = 404, description = "No such session", body = Problem),
        (status = 409, description = "It is still running", body = Problem),
    )
)]
async fn rerun_agent_session(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<(StatusCode, Json<AgentSession>)> {
    let previous = finished(&state, &id).await?;
    let card = card::find_by_id(&state.db, &previous.card_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let session = dispatch::submit(
        &state,
        Submission {
            provider: previous.provider.parse().unwrap_or_default(),
            card: &card,
            prompt: previous.prompt.clone(),
            started_by: member.0.id(),
            rerun_of: Some(previous.id.clone()),
            resume_session_id: None,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(session)))
}

/// Continues a finished session's conversation with a follow-up prompt (`--resume`).
#[utoipa::path(
    post,
    path = "/agent-sessions/{id}/continue",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session to continue")),
    request_body = ContinueRequest,
    responses(
        (status = 201, description = "The continuation run", body = AgentSession),
        (status = 409, description = "It is still running", body = Problem),
        (status = 422, description = "An empty prompt", body = Problem),
    )
)]
async fn continue_agent_session(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<ContinueRequest>,
) -> AppResult<(StatusCode, Json<AgentSession>)> {
    let prompt = body.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > 32 * 1024 {
        return Err(AppError::Validation(
            "Say what the agent should do next.".to_owned(),
        ));
    }
    let previous = finished(&state, &id).await?;
    let resume = previous
        .claude_session_id
        .clone()
        .ok_or_else(|| AppError::Conflict("That run has no CLI session to continue.".to_owned()))?;
    let card = card::find_by_id(&state.db, &previous.card_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let session = dispatch::submit(
        &state,
        Submission {
            provider: previous.provider.parse().unwrap_or_default(),
            card: &card,
            prompt: prompt.to_owned(),
            started_by: member.0.id(),
            rerun_of: Some(previous.id.clone()),
            resume_session_id: Some(resume),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(session)))
}

async fn finished(state: &AppState, id: &str) -> AppResult<AgentSession> {
    let session = agent_session::find_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if session.status.is_running() {
        return Err(AppError::Conflict(
            "That run is still in progress.".to_owned(),
        ));
    }
    Ok(session)
}

/// What a run changed in its workspace, for review before opening a PR.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDiff {
    /// The branch the workspace is on.
    pub branch: Option<String>,
    /// `git status --porcelain`.
    pub status: String,
    /// Commits on this branch not on the default branch (`git log --oneline`).
    pub commits: String,
    /// The full patch: committed since the default branch, plus anything uncommitted.
    pub diff: String,
    /// Whether `diff` was cut at the size limit.
    pub truncated: bool,
}

/// The largest diff returned, in bytes.
const MAX_DIFF: usize = 1024 * 1024;

async fn git(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The diff a run left in its project's workspace, against the remote default branch.
#[utoipa::path(
    get,
    path = "/agent-sessions/{id}/diff",
    tag = "agent-sessions",
    params(("id" = String, Path, description = "The session")),
    responses(
        (status = 200, description = "Status, commits and patch", body = WorkspaceDiff),
        (status = 404, description = "No such session, or no workspace on disk", body = Problem),
    )
)]
async fn get_agent_session_diff(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<WorkspaceDiff>> {
    let session = agent_session::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let target = card::find_by_id(&state.db, &session.card_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let dir =
        crate::agent::workspace::workspace_path(&state.config.workspace_dir, &target.project_id);
    if !dir.join(".git").exists() {
        return Err(AppError::NotFound);
    }
    let base = git(
        &dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    .map_or_else(|| "origin/HEAD".to_owned(), |b| b.trim().to_owned());
    let range = format!("{base}...HEAD");
    let status = git(&dir, &["status", "--porcelain"])
        .await
        .unwrap_or_default();
    let commits = git(
        &dir,
        &["log", "--oneline", "--no-color", &format!("{base}..HEAD")],
    )
    .await
    .unwrap_or_default();
    let mut diff = git(&dir, &["diff", "--no-color", &range])
        .await
        .unwrap_or_default();
    diff.push_str(
        &git(&dir, &["diff", "--no-color", "HEAD"])
            .await
            .unwrap_or_default(),
    );
    let truncated = diff.len() > MAX_DIFF;
    if truncated {
        let mut cut = MAX_DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
    }
    Ok(Json(WorkspaceDiff {
        branch: session
            .branch
            .clone()
            .or(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await
                .map(|b| b.trim().to_owned())),
        status,
        commits,
        diff,
        truncated,
    }))
}

/// The body of `PUT /projects/{key}/agent-settings`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSettingsRequest {
    /// The settings.
    pub settings: AgentSettings,
    /// Required to choose `bypassPermissions` or API-key billing.
    #[serde(default)]
    pub acknowledge_risk: bool,
}

/// A project's Claude Code settings.
#[utoipa::path(
    get,
    path = "/projects/{key}/agent-settings",
    tag = "agent-sessions",
    params(("key" = String, Path, description = "The project key")),
    responses((status = 200, description = "The settings, defaults filled in", body = AgentSettings))
)]
async fn get_agent_settings(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<AgentSettings>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(agent_settings::get(&state.db, &project.id).await?))
}

/// Replaces a project's Claude Code settings.
#[utoipa::path(
    put,
    path = "/projects/{key}/agent-settings",
    tag = "agent-sessions",
    params(("key" = String, Path, description = "The project key")),
    request_body = AgentSettingsRequest,
    responses(
        (status = 200, description = "Saved", body = AgentSettings),
        (status = 422, description = "Invalid, or a risky choice without acknowledgeRisk", body = Problem),
    )
)]
async fn put_agent_settings(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<AgentSettingsRequest>,
) -> AppResult<Json<AgentSettings>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    agent_settings::validate(&body.settings, body.acknowledge_risk)?;
    if let Some(status) = &body.settings.destination_status_id {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM statuses WHERE id = ?")
                .bind(status)
                .fetch_optional(state.db.reader())
                .await?;
        if owner.as_deref() != Some(project.id.as_str()) {
            return Err(AppError::Validation(
                "The destination must be one of this project's statuses.".to_owned(),
            ));
        }
    }
    if let Some(credential) = &body.settings.api_credential_id {
        let found = crate::secrets::find_by_id(&state.db, credential).await?;
        if !found.is_some_and(|c| c.provider == crate::secrets::Provider::Anthropic) {
            return Err(AppError::Validation(
                "Pick an Anthropic API key from the vault.".to_owned(),
            ));
        }
    }
    agent_settings::put(&state.db, &project.id, &body.settings, now()).await?;
    Ok(Json(agent_settings::get(&state.db, &project.id).await?))
}

/// Assembles the routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_agent_session, list_card_agent_sessions))
        .routes(routes!(get_agent_session))
        .routes(routes!(get_agent_session_transcript))
        .routes(routes!(cancel_agent_session))
        .routes(routes!(rerun_agent_session))
        .routes(routes!(continue_agent_session))
        .routes(routes!(get_agent_session_diff))
        .routes(routes!(get_agent_settings, put_agent_settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_is_the_summary_alone_when_there_is_no_description() {
        assert_eq!(build_prompt("Fix the thing", None), "Fix the thing");
    }

    #[test]
    fn the_prompt_is_the_summary_alone_when_the_description_is_blank() {
        assert_eq!(
            build_prompt("Fix the thing", Some("   \n  ")),
            "Fix the thing"
        );
    }

    #[test]
    fn the_prompt_joins_summary_and_description_with_a_blank_line() {
        assert_eq!(
            build_prompt("Fix the thing", Some("Do the needful.")),
            "Fix the thing\n\nDo the needful."
        );
    }

    #[test]
    fn the_description_is_trimmed_before_joining() {
        assert_eq!(
            build_prompt("Fix the thing", Some("  Do the needful.  \n")),
            "Fix the thing\n\nDo the needful."
        );
    }
}

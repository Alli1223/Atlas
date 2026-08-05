//! "Run Claude Code against this card": starting a run, and reading a card's session history.
//!
//! The actual wiring — preparing the workspace, spawning the CLI, persisting the session and
//! its eventual outcome — lives in [`crate::agent::orchestrator`]; this module is the thin
//! HTTP layer over it plus the wire-facing prompt Atlas builds from a card, matching this
//! phase's own naming for the feature (`TODO.md`'s "card → task binding").
//!
//! # Scoping
//!
//! Starting a run costs real money and pushes real commits, so it is `Card(Member)` like
//! every other card-mutating action (`POST /cards/{key}/branch`, `/pr`). Reading a session —
//! whether the list for a card or one by id — is `Viewer`, like reading anything else about a
//! card.
//!
//! # What is not here yet
//!
//! Cancelling a run over the API, and the on-completion actions (move the card, attach the
//! PR) — both later increments in `TODO.md` Phase 13. [`crate::agent::runner::RunHandle`]
//! already supports cancellation; nothing in Atlas's HTTP surface reaches it yet.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::agent::orchestrator::{self, StartRequest};
use crate::agent::runner::RunLimits;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::domain::agent_session::{self, AgentSession};
use crate::domain::card;
use crate::error::{AppError, AppResult, Problem};

/// Turns per-run — enough for a genuinely multi-step task without an unbounded bill. Not yet
/// per-project configurable; `TODO.md`'s "permission mode per project" bullet covers making
/// this (and the tool allowlist below) a setting rather than a constant.
const DEFAULT_MAX_TURNS: u32 = 50;

/// Spend cap per run, in USD. Required by the CLI itself — see `agent::runner`'s module doc —
/// and picked here as a number small enough that a runaway loop is a nuisance, not a bill.
const DEFAULT_MAX_BUDGET_USD: f64 = 5.0;

/// The tools a run may use. Deliberately not `bypassPermissions` or an unrestricted
/// `--allowedTools`: this is the least permissive set that can still actually do the work a
/// card describes (read/edit/write files, run shell commands, search the tree) inside its own
/// cloned workspace. `TODO.md`'s permission-mode bullet is what makes this configurable.
const DEFAULT_ALLOWED_TOOLS: &[&str] = &["Read", "Edit", "Write", "Bash", "Grep", "Glob"];

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
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 201, description = "The run, recorded as running", body = AgentSession),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "The card's project has no linked repo, or its credential is gone", body = Problem),
        (status = 500, description = "The vault is not configured, or the CLI failed to spawn", body = Problem),
    )
)]
async fn start_agent_session(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<(StatusCode, Json<AgentSession>)> {
    let vault = state.vault.as_deref().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!(
            "the secrets vault is not configured: set ATLAS_MASTER_KEY"
        ))
    })?;

    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let prompt = build_prompt(&card.summary, card.description.as_deref());

    let session = orchestrator::start(
        &state.db,
        vault,
        state.agent_runner.as_ref(),
        state.workspace_preparer.as_ref(),
        StartRequest {
            card: &card,
            prompt,
            allowed_tools: DEFAULT_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            limits: RunLimits {
                max_turns: DEFAULT_MAX_TURNS,
                max_budget_usd: DEFAULT_MAX_BUDGET_USD,
            },
            started_by: Some(current.id()),
        },
    )
    .await?;

    tracing::info!(card = %card.key, session = %session.id, "started a Claude Code run against a card");
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

/// Assembles the routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_agent_session, list_card_agent_sessions))
        .routes(routes!(get_agent_session))
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

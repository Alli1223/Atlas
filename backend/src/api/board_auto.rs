//! Project owners start/stop a coordinator; ticket members manage dependency references.
use crate::{
    agent::{
        board_auto::{self, AutoRun, Context},
        providers::AgentProvider,
    },
    api::AppState,
    auth::CurrentUser,
    domain::{
        automation::{self, CardAutomation},
        card, project,
    },
    error::{AppError, AppResult, Problem},
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutoRequest {
    #[serde(default)]
    pub provider: AgentProvider,
    #[serde(default = "default_batch")]
    pub max_tasks: i64,
}
fn default_batch() -> i64 {
    3
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DependencyRequest {
    pub blocked_by: String,
    pub reason: String,
}

#[utoipa::path(get, path = "/projects/{key}/auto", tag = "board-auto", params(("key" = String, Path)), responses((status = 200, body = Option<AutoRun>), (status = 404, body = Problem)))]
async fn get_auto(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> AppResult<Json<Option<AutoRun>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(board_auto::latest(&state.db, &project.id).await?))
}
#[utoipa::path(post, path = "/projects/{key}/auto", tag = "board-auto", params(("key" = String, Path)), request_body = AutoRequest, responses((status = 201, body = AutoRun), (status = 409, body = Problem)))]
async fn start_auto(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    Json(request): Json<AutoRequest>,
) -> AppResult<(StatusCode, Json<AutoRun>)> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let vault = state.vault.clone().ok_or_else(|| {
        AppError::Conflict("Configure the secret vault before starting agents.".to_owned())
    })?;
    let ctx = Context {
        db: state.db,
        config: state.config,
        vault,
        runner: state.agent_runner,
        workspace: state.workspace_preparer,
        registry: state.cancel_registry,
    };
    let run = board_auto::start(
        ctx,
        project,
        current.user.id.clone(),
        request.provider,
        request.max_tasks,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(run)))
}
#[utoipa::path(post, path = "/projects/{key}/auto/stop", tag = "board-auto", params(("key" = String, Path)), responses((status = 204), (status = 404, body = Problem)))]
async fn stop_auto(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> AppResult<StatusCode> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    board_auto::stop(&state.db, &project.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(get, path = "/cards/{key}/automation", tag = "board-auto", params(("key" = String, Path)), responses((status = 200, body = CardAutomation), (status = 404, body = Problem)))]
async fn get_card_automation(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> AppResult<Json<CardAutomation>> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(automation::info(&state.db, &card.id).await?))
}
#[utoipa::path(post, path = "/cards/{key}/dependencies", tag = "board-auto", params(("key" = String, Path)), request_body = DependencyRequest, responses((status = 200, body = CardAutomation), (status = 409, body = Problem)))]
async fn add_dependency(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    Json(request): Json<DependencyRequest>,
) -> AppResult<Json<CardAutomation>> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    automation::block(
        &state.db,
        &card,
        &request.blocked_by,
        &request.reason,
        &current.user.id,
    )
    .await?;
    Ok(Json(automation::info(&state.db, &card.id).await?))
}
#[utoipa::path(delete, path = "/cards/{key}/dependencies/{blockerKey}", tag = "board-auto", params(("key" = String, Path), ("blockerKey" = String, Path)), responses((status = 204), (status = 404, body = Problem)))]
async fn remove_dependency(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((key, blocker_key)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let blocker = card::find_by_key(&state.db, &blocker_key)
        .await?
        .ok_or(AppError::NotFound)?;
    if blocker.project_id != card.project_id {
        return Err(AppError::NotFound);
    }
    let mut tx = state.db.begin_write().await?;
    sqlx::query("DELETE FROM card_dependencies WHERE card_id = ? AND blocked_by_id = ?")
        .bind(&card.id)
        .bind(&blocker.id)
        .execute(&mut *tx)
        .await?;
    crate::domain::comment::insert(
        &mut tx,
        &card.id,
        &current.user.id,
        &format!("Removed dependency on {}.", blocker.key),
        crate::auth::now(),
    )
    .await?;
    tx.commit().await?;
    if automation::info(&state.db, &card.id)
        .await?
        .blocked_by
        .iter()
        .all(|dependency| dependency.done)
    {
        let name: String = sqlx::query_scalar("SELECT name FROM statuses WHERE id = ?")
            .bind(&card.status_id)
            .fetch_one(state.db.reader())
            .await?;
        if name.eq_ignore_ascii_case("Blocked") {
            crate::integrations::github::smart_commit::move_to_category(
                &state.db,
                &card,
                crate::domain::StatusCategory::Todo,
                &current.user.id,
                crate::auth::now(),
            )
            .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/projects/{key}/auto/activity", tag = "board-auto", params(("key" = String, Path)), responses((status = 200, body = Vec<board_auto::Activity>)))]
async fn get_activity(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<board_auto::Activity>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let events = if let Some(run) = board_auto::latest(&state.db, &project.id).await? {
        board_auto::activities(&state.db, &run.id).await?
    } else {
        Vec::new()
    };
    Ok(Json(events))
}
#[utoipa::path(get, path = "/agent-usage", tag = "board-auto", responses((status = 200, body = Vec<crate::agent::usage::ProviderUsage>)))]
async fn get_usage(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<Json<Vec<crate::agent::usage::ProviderUsage>>> {
    Ok(Json(
        crate::agent::usage::summary(&state.db, &current.user.id).await?,
    ))
}

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_activity))
        .routes(routes!(get_usage))
        .routes(routes!(get_auto, start_auto))
        .routes(routes!(stop_auto))
        .routes(routes!(get_card_automation))
        .routes(routes!(add_dependency))
        .routes(routes!(remove_dependency))
}

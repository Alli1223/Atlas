//! `/api/v1/projects/{key}/board` — the board data — and the saved-board CRUD.
//!
//! The board-data endpoint is the feature; the saved-board routes are a thin
//! bookmark around it. See [`crate::domain::board`] for the computation and the
//! two properties that shape it (the filter reuses AQL's access scoping; the
//! mini-map rollup is one query, never N+1).

// See the note at the top of `api::cards`: the `params(...)` macro expands to a
// qualified path in a sibling item, so the lint is silenced at module scope.
#![allow(unused_qualifications)]

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{AppState, projects};
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::board::{self, Board, BoardData, BoardPatch, BoardScope, NewBoard, Swimlane};
use crate::domain::card;
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// Query parameters for `GET /projects/{key}/board`.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct BoardQuery {
    /// A card **key** to render the children of — the nested board. Omit for the
    /// project's top-level cards.
    #[serde(default)]
    pub parent: Option<String>,
    /// An AQL quick filter, combined with `AND` onto the board's scope. Omit for
    /// no filter.
    #[serde(default)]
    pub aql: Option<String>,
    /// Row grouping: `none` (default), `assignee` or `parent`.
    #[serde(default)]
    pub swimlane: Option<String>,
}

/// The board: columns of cards, with the mini-map rollup on every card.
#[utoipa::path(
    get,
    path = "/projects/{key}/board",
    tag = "boards",
    params(("key" = String, Path, description = "The project key"), BoardQuery),
    responses(
        (status = 200, description = "The board data", body = BoardData),
        (status = 400, description = "The AQL filter does not parse or type-check", body = Problem),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project, or no such parent card in it", body = Problem),
        (status = 422, description = "Unknown swimlane value", body = Problem),
    )
)]
async fn get_board(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    Query(query): Query<BoardQuery>,
) -> AppResult<Json<BoardData>> {
    let project = projects::by_key(&state.db, &key).await?;

    // A parent scope names a card by key; it must exist and live in *this*
    // project, or the board is 404 rather than silently the top level.
    let scope = match query.parent.as_deref() {
        None => BoardScope::Root,
        Some(parent_key) => {
            let card = card::find_by_key(&state.db, &parent_key.to_ascii_uppercase())
                .await?
                .filter(|c| c.project_id == project.id)
                .ok_or(AppError::NotFound)?;
            BoardScope::Child {
                id: card.id,
                key: card.key,
            }
        }
    };

    let swimlane = Swimlane::from_query(query.swimlane.as_deref())?;

    let data = board::build(
        &state.db,
        &current.user,
        &project,
        &scope,
        query.aql.as_deref(),
        swimlane,
        now(),
    )
    .await?;

    Ok(Json(data))
}

// ---------------------------------------------------------------------------
// Saved board configuration — thin CRUD
// ---------------------------------------------------------------------------

/// The body of `POST /projects/{key}/boards`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBoardRequest {
    /// The display name, unique per project.
    #[schema(example = "My board")]
    pub name: String,
    /// The card whose children this board renders, or `null`/omitted for the top
    /// level.
    #[serde(default)]
    pub default_parent_id: Option<String>,
    /// The saved AQL quick filter, or omitted for none.
    #[serde(default)]
    pub aql_filter: Option<String>,
    /// Row grouping. Defaults to `none`.
    #[serde(default)]
    pub swimlane: Option<String>,
    /// Per-status WIP limits, a JSON object `{statusId: max}`. Defaults to `{}`.
    #[serde(default)]
    pub wip_limits: Option<serde_json::Value>,
}

/// The body of `PATCH /boards/{id}`.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBoardRequest {
    /// The new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub default_parent_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub aql_filter: Option<Option<String>>,
    /// The new swimlane mode.
    #[serde(default)]
    pub swimlane: Option<String>,
    /// The new WIP limits.
    #[serde(default)]
    pub wip_limits: Option<serde_json::Value>,
}

/// Validates a swimlane string, echoing it back. A saved board stores the string;
/// the board-data endpoint parses it at render time.
fn validate_swimlane(value: &str) -> AppResult<String> {
    Swimlane::from_query(Some(value)).map(|_| value.to_owned())
}

/// Every saved board of a project.
#[utoipa::path(
    get,
    path = "/projects/{key}/boards",
    tag = "boards",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's saved boards, by name", body = Vec<Board>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_boards(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Board>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(board::list_boards(&state.db, &project.id).await?))
}

/// Saves a new board.
#[utoipa::path(
    post,
    path = "/projects/{key}/boards",
    tag = "boards",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateBoardRequest,
    responses(
        (status = 201, description = "Created", body = Board),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot save boards", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "A board of that name already exists", body = Problem),
        (status = 422, description = "The name or swimlane is invalid", body = Problem),
    )
)]
async fn create_board(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateBoardRequest>,
) -> AppResult<(StatusCode, Json<Board>)> {
    let name = board::validate_board_name(&body.name)?;
    let swimlane = match body.swimlane.as_deref() {
        Some(value) => validate_swimlane(value)?,
        None => "none".to_owned(),
    };
    let wip_limits = body.wip_limits.unwrap_or_else(|| serde_json::json!({}));

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    if board::board_name_taken(&mut tx, &project.id, &name, None).await? {
        return Err(AppError::Conflict(format!(
            "This project already has a board called {name:?}."
        )));
    }

    let created = board::insert_board(
        &mut tx,
        &project.id,
        &NewBoard {
            name,
            default_parent_id: body.default_parent_id,
            aql_filter: body.aql_filter,
            swimlane,
            wip_limits,
        },
        now,
    )
    .await?;

    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// One saved board.
#[utoipa::path(
    get,
    path = "/boards/{id}",
    tag = "boards",
    params(("id" = String, Path, description = "The board's id")),
    responses(
        (status = 200, description = "The board", body = Board),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such board", body = Problem),
    )
)]
async fn get_saved_board(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Board>> {
    Ok(Json(
        board::find_board(&state.db, &id)
            .await?
            .ok_or(AppError::NotFound)?,
    ))
}

/// Edits a saved board.
#[utoipa::path(
    patch,
    path = "/boards/{id}",
    tag = "boards",
    params(("id" = String, Path, description = "The board's id")),
    request_body = UpdateBoardRequest,
    responses(
        (status = 200, description = "Updated", body = Board),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot edit boards", body = Problem),
        (status = 404, description = "No such board", body = Problem),
        (status = 409, description = "A board of that name already exists", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn update_board(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateBoardRequest>,
) -> AppResult<Json<Board>> {
    let patch = BoardPatch {
        name: body
            .name
            .as_deref()
            .map(board::validate_board_name)
            .transpose()?,
        default_parent_id: body.default_parent_id,
        aql_filter: body.aql_filter,
        swimlane: body
            .swimlane
            .as_deref()
            .map(validate_swimlane)
            .transpose()?,
        wip_limits: body.wip_limits,
    };

    if patch.is_empty() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = board::find_board_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(name) = &patch.name
        && board::board_name_taken(&mut tx, &target.project_id, name, Some(&target.id)).await?
    {
        return Err(AppError::Conflict(format!(
            "This project already has a board called {name:?}."
        )));
    }

    let updated = board::apply_board_patch(&mut tx, &target.id, &patch, now).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// Deletes a saved board.
#[utoipa::path(
    delete,
    path = "/boards/{id}",
    tag = "boards",
    params(("id" = String, Path, description = "The board's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot delete boards", body = Problem),
        (status = 404, description = "No such board", body = Problem),
    )
)]
async fn delete_board(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;

    let target = board::find_board_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    board::delete_board(&mut tx, &target.id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The board routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_board))
        .routes(routes!(list_boards, create_board))
        .routes(routes!(get_saved_board, update_board, delete_board))
}

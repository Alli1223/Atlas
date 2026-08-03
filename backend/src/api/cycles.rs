//! `/api/v1/projects/{key}/cycles`, `/api/v1/cycles/{id}`, and a card's cycle
//! membership (`/api/v1/cards/{key}/cycle`).
//!
//! Read is Viewer; creating, patching, and every state-machine action (start,
//! complete, reopen) is Member — a cycle is sprint planning, not structural
//! project configuration, the same tier as a saved board. See
//! [`crate::domain::cycle`] for the state machine itself; this module is thin
//! glue plus the DTOs the wire needs that the domain row does not already
//! carry.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{AppState, projects};
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::card;
use crate::domain::config;
use crate::domain::cycle::{self, CarryTo, Cycle, CyclePatch, NewCycle};
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /projects/{key}/cycles`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCycleRequest {
    /// The cycle's name, e.g. `"Sprint 14"`.
    pub name: String,
    /// An optional sprint goal.
    #[serde(default)]
    pub goal: Option<String>,
}

/// The body of `PATCH /cycles/{id}`. At least one field must be present.
#[allow(clippy::option_option)]
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCycleRequest {
    /// A new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves the goal alone, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub goal: Option<Option<String>>,
}

/// The body of `POST /cycles/{id}/start`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartCycleRequest {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

/// The body of `POST /cycles/{id}/reopen`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReopenCycleRequest {
    /// The replanned end date. The start date is kept as it was.
    pub end_date: NaiveDate,
}

/// Where a completing cycle's incomplete cards go — the wire shape of
/// [`CarryTo`].
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum CarryToRequest {
    /// Out of any cycle, back to the plain backlog.
    Backlog,
    /// An existing cycle in the same project.
    #[serde(rename_all = "camelCase")]
    ExistingCycle {
        /// The target cycle's id.
        cycle_id: String,
    },
    /// A brand new cycle, created for this purpose.
    NewCycle {
        /// The new cycle's name.
        name: String,
    },
}

impl CarryToRequest {
    fn as_domain(&self) -> CarryTo<'_> {
        match self {
            Self::Backlog => CarryTo::Backlog,
            Self::ExistingCycle { cycle_id } => CarryTo::ExistingCycle(cycle_id),
            Self::NewCycle { name } => CarryTo::NewCycle(name),
        }
    }
}

/// The body of `POST /cycles/{id}/complete`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteCycleRequest {
    pub carry_to: CarryToRequest,
}

/// The body of `POST /cards/{key}/cycle`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddCardToCycleRequest {
    /// The cycle to add this card to. Must belong to the card's project.
    pub cycle_id: String,
}

// ---------------------------------------------------------------------------
// Cycles
// ---------------------------------------------------------------------------

/// A project's cycles: active first, then future, then closed.
#[utoipa::path(
    get,
    path = "/projects/{key}/cycles",
    tag = "cycles",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's cycles", body = Vec<Cycle>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_cycles(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Cycle>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(cycle::list_for_project(&state.db, &project.id).await?))
}

/// Creates a cycle. Requires the project to have cycles enabled.
#[utoipa::path(
    post,
    path = "/projects/{key}/cycles",
    tag = "cycles",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateCycleRequest,
    responses(
        (status = 201, description = "Created", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot create cycles", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 422, description = "The name is invalid, or cycles are not enabled", body = Problem),
    )
)]
async fn create_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateCycleRequest>,
) -> AppResult<(StatusCode, Json<Cycle>)> {
    let name = config::validate_name(&body.name)?;

    let mut tx = state.db.begin_write().await?;
    let project = projects::by_key(&state.db, &key).await?;
    let created = cycle::insert(
        &mut tx,
        &project,
        &NewCycle {
            project_id: &project.id,
            name: &name,
            goal: body.goal.as_deref(),
        },
        now(),
    )
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Renames a cycle and/or edits its goal. Legal in any state.
#[utoipa::path(
    patch,
    path = "/cycles/{id}",
    tag = "cycles",
    params(("id" = String, Path, description = "The cycle id")),
    request_body = UpdateCycleRequest,
    responses(
        (status = 200, description = "Updated", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot edit cycles", body = Problem),
        (status = 404, description = "No such cycle", body = Problem),
        (status = 422, description = "The name is invalid, or nothing was sent", body = Problem),
    )
)]
async fn update_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateCycleRequest>,
) -> AppResult<Json<Cycle>> {
    if body.name.is_none() && body.goal.is_none() {
        return Err(AppError::Validation(
            "The request changed nothing. Send a name or goal.".to_owned(),
        ));
    }
    let name = body
        .name
        .as_deref()
        .map(config::validate_name)
        .transpose()?;

    let mut tx = state.db.begin_write().await?;
    let existing = cycle::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updated = cycle::apply_patch(
        &mut tx,
        &existing,
        &CyclePatch {
            name,
            goal: body.goal,
        },
        now(),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// Starts a cycle: `future -> active`.
#[utoipa::path(
    post,
    path = "/cycles/{id}/start",
    tag = "cycles",
    params(("id" = String, Path, description = "The cycle id")),
    request_body = StartCycleRequest,
    responses(
        (status = 200, description = "Started", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot start cycles", body = Problem),
        (status = 404, description = "No such cycle", body = Problem),
        (status = 409, description = "Not a future cycle, or another cycle is already active", body = Problem),
        (status = 422, description = "The end date is before the start date", body = Problem),
    )
)]
async fn start_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<StartCycleRequest>,
) -> AppResult<Json<Cycle>> {
    let mut tx = state.db.begin_write().await?;
    let existing = cycle::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let started = cycle::start(&mut tx, &existing, body.start_date, body.end_date, now()).await?;
    tx.commit().await?;

    Ok(Json(started))
}

/// Completes a cycle: `active -> closed`, carrying any incomplete cards as directed.
#[utoipa::path(
    post,
    path = "/cycles/{id}/complete",
    tag = "cycles",
    params(("id" = String, Path, description = "The cycle id")),
    request_body = CompleteCycleRequest,
    responses(
        (status = 200, description = "Completed", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot complete cycles", body = Problem),
        (status = 404, description = "No such cycle", body = Problem),
        (status = 409, description = "Not an active cycle", body = Problem),
        (status = 422, description = "The carry-to cycle does not exist in this project, or is closed", body = Problem),
    )
)]
async fn complete_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<CompleteCycleRequest>,
) -> AppResult<Json<Cycle>> {
    let mut tx = state.db.begin_write().await?;
    let existing = cycle::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let closed = cycle::complete(&mut tx, &existing, &body.carry_to.as_domain(), now()).await?;
    tx.commit().await?;

    Ok(Json(closed))
}

/// Reopens a closed cycle: `closed -> active`, replanning its end date.
#[utoipa::path(
    post,
    path = "/cycles/{id}/reopen",
    tag = "cycles",
    params(("id" = String, Path, description = "The cycle id")),
    request_body = ReopenCycleRequest,
    responses(
        (status = 200, description = "Reopened", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot reopen cycles", body = Problem),
        (status = 404, description = "No such cycle", body = Problem),
        (status = 409, description = "Not a closed cycle, or another cycle is already active", body = Problem),
    )
)]
async fn reopen_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<ReopenCycleRequest>,
) -> AppResult<Json<Cycle>> {
    let mut tx = state.db.begin_write().await?;
    let existing = cycle::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let reopened = cycle::reopen(&mut tx, &existing, body.end_date, now()).await?;
    tx.commit().await?;

    Ok(Json(reopened))
}

// ---------------------------------------------------------------------------
// A card's cycle membership
// ---------------------------------------------------------------------------

/// The cycle a card currently belongs to, if any.
#[utoipa::path(
    get,
    path = "/cards/{key}/cycle",
    tag = "cycles",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's current cycle", body = Cycle),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card, or it is not in a cycle", body = Problem),
    )
)]
async fn card_cycle(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Cycle>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let current = cycle::current_cycle_for_card(&state.db, &target.id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(current))
}

/// Adds a card to a cycle (or refreshes its membership, if it was previously removed).
#[utoipa::path(
    post,
    path = "/cards/{key}/cycle",
    tag = "cycles",
    params(("key" = String, Path, description = "The card key")),
    request_body = AddCardToCycleRequest,
    responses(
        (status = 204, description = "Added"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change cycle membership", body = Problem),
        (status = 404, description = "No such card, or no such cycle", body = Problem),
        (status = 409, description = "The cycle is closed", body = Problem),
    )
)]
async fn add_card_to_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<AddCardToCycleRequest>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let target_cycle = cycle::find_by_id_tx(&mut tx, &body.cycle_id)
        .await?
        .filter(|c| c.project_id == target.project_id)
        .ok_or(AppError::NotFound)?;

    cycle::add_card(&mut tx, &target.id, &target_cycle.id, now()).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Removes a card from its current cycle. A no-op (still `204`) if it was not in one.
#[utoipa::path(
    delete,
    path = "/cards/{key}/cycle",
    tag = "cycles",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 204, description = "Removed, or already not in a cycle"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change cycle membership", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn remove_card_from_cycle(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(current) = cycle::current_cycle_for_card(&state.db, &target.id).await? {
        cycle::remove_card(&mut tx, &target.id, &current.id, now()).await?;
    }
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The cycle routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_cycles, create_cycle))
        .routes(routes!(update_cycle))
        .routes(routes!(start_cycle))
        .routes(routes!(complete_cycle))
        .routes(routes!(reopen_cycle))
        .routes(routes!(
            card_cycle,
            add_card_to_cycle,
            remove_card_from_cycle
        ))
}

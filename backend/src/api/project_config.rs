//! `/api/v1/projects/{key}/…` — the per-project configuration surface.
//!
//! Five near-identical CRUD sets, and their sameness is the point. In Jira each
//! of these would arrive through its own scheme layer with its own admin screen
//! and its own three-level indirection; here a status belongs to a project, so
//! the whole config surface is five tables and one shape. See docs/adr/0003.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{AppState, projects};
use crate::auth::CurrentUser;
use crate::auth::extract::RequireMember;
use crate::domain::StatusCategory;
use crate::domain::config::{self, CardType, HierarchyLevel, Priority, Resolution, Status};
use crate::error::{AppResult, Problem};

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// The body of `POST /projects/{key}/hierarchy-levels`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateHierarchyLevelRequest {
    /// Higher is further up the tree. May be negative.
    pub level: i64,
    /// What this project calls the rung: `Epic`, `Asset`, `Company`.
    pub name: String,
}

/// The body of `PATCH /hierarchy-levels/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameRequest {
    /// The new name.
    pub name: String,
}

/// The body of `POST /projects/{key}/card-types`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCardTypeRequest {
    /// `Story`, `Asset`, `Application`.
    pub name: String,
    /// The hierarchy rung. Must be a level the project has defined.
    pub level: i64,
    /// An icon identifier the frontend resolves.
    #[serde(default)]
    pub icon: Option<String>,
    /// A hex colour.
    #[serde(default)]
    pub colour: Option<String>,
    /// Whether new cards default to this type.
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// The body of `PATCH /card-types/{id}`.
///
/// `level` is absent deliberately: moving a type between rungs would invalidate
/// the `parent.level > child.level` rule for every existing card of that type at
/// once, with no way to tell the user which cards had just become illegal.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCardTypeRequest {
    /// The name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub icon: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub colour: Option<Option<String>>,
    /// Whether new cards default to this type.
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// The body of `POST /projects/{key}/statuses`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateStatusRequest {
    /// `In Review`, `Phone Screen`, `Retopo`.
    pub name: String,
    /// One of the three buckets. There are three and there will be three.
    pub category: StatusCategory,
    /// Board order, left to right.
    pub position: i64,
}

/// The body of `PATCH /statuses/{id}`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateStatusRequest {
    /// The name.
    #[serde(default)]
    pub name: Option<String>,
    /// The category. Changing this changes what every card in the status means,
    /// and Atlas does not retroactively rewrite their resolutions — history says
    /// what happened.
    #[serde(default)]
    pub category: Option<StatusCategory>,
    /// Board order.
    #[serde(default)]
    pub position: Option<i64>,
}

/// The body of `POST /projects/{key}/priorities`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePriorityRequest {
    /// `Highest`, `Dream Job`, `Critical`.
    pub name: String,
    /// **Lower is more urgent.** Rank 1 is the most urgent.
    pub rank: i64,
    /// An icon identifier.
    #[serde(default)]
    pub icon: Option<String>,
    /// A hex colour.
    #[serde(default)]
    pub colour: Option<String>,
}

/// The body of `PATCH /priorities/{id}`.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePriorityRequest {
    /// The name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub icon: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub colour: Option<Option<String>>,
    /// Lower is more urgent.
    #[serde(default)]
    pub rank: Option<i64>,
}

/// The body of `POST /projects/{key}/resolutions`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResolutionRequest {
    /// `Done`, `Won't Do`, `Ghosted`.
    pub name: String,
    /// Display order. **Position 1 is what a move into a done status auto-sets**
    /// when the client did not name one.
    pub position: i64,
}

/// The body of `PATCH /resolutions/{id}`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateResolutionRequest {
    /// The name.
    #[serde(default)]
    pub name: Option<String>,
    /// Display order. Position 1 is the auto-set default.
    #[serde(default)]
    pub position: Option<i64>,
}

// ---------------------------------------------------------------------------
// hierarchy_levels
// ---------------------------------------------------------------------------

/// A project's hierarchy levels, deepest last.
#[utoipa::path(
    get,
    path = "/projects/{key}/hierarchy-levels",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's hierarchy", body = Vec<HierarchyLevel>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_hierarchy_levels(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<HierarchyLevel>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(config::levels(&state.db, &project.id).await?))
}

/// Adds a hierarchy level.
#[utoipa::path(
    post,
    path = "/projects/{key}/hierarchy-levels",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateHierarchyLevelRequest,
    responses(
        (status = 201, description = "Created", body = HierarchyLevel),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The project already has a level with that number", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn create_hierarchy_level(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateHierarchyLevelRequest>,
) -> AppResult<(StatusCode, Json<HierarchyLevel>)> {
    let name = config::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let mut tx = state.db.begin_write().await?;
    let created = config::insert_level(&mut tx, &project.id, body.level, &name).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Renames a hierarchy level.
#[utoipa::path(
    patch,
    path = "/hierarchy-levels/{id}",
    tag = "project-config",
    params(("id" = String, Path, description = "The level's id")),
    request_body = RenameRequest,
    responses(
        (status = 200, description = "Renamed", body = HierarchyLevel),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such level", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn update_hierarchy_level(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<RenameRequest>,
) -> AppResult<Json<HierarchyLevel>> {
    let name = config::validate_name(&body.name)?;

    let mut tx = state.db.begin_write().await?;
    let updated = config::rename_level(&mut tx, &id, &name).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// card_types
// ---------------------------------------------------------------------------

/// A project's card types.
#[utoipa::path(
    get,
    path = "/projects/{key}/card-types",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's card types", body = Vec<CardType>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_card_types(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardType>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(config::card_types(&state.db, &project.id).await?))
}

/// Adds a card type.
#[utoipa::path(
    post,
    path = "/projects/{key}/card-types",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateCardTypeRequest,
    responses(
        (status = 201, description = "Created", body = CardType),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken, or the level does not exist", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn create_card_type(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateCardTypeRequest>,
) -> AppResult<(StatusCode, Json<CardType>)> {
    let name = config::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let mut tx = state.db.begin_write().await?;
    let created = config::insert_card_type(
        &mut tx,
        &project.id,
        &name,
        body.icon.as_deref(),
        body.colour.as_deref(),
        body.level,
        body.is_default.unwrap_or(false),
    )
    .await?;
    // A type at a level the project has not defined violates the composite
    // foreign key, which is DEFERRABLE — so the failure surfaces here, at commit.
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a card type.
#[utoipa::path(
    patch,
    path = "/card-types/{id}",
    tag = "project-config",
    params(("id" = String, Path, description = "The card type's id")),
    request_body = UpdateCardTypeRequest,
    responses(
        (status = 200, description = "Updated", body = CardType),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such card type", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn update_card_type(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateCardTypeRequest>,
) -> AppResult<Json<CardType>> {
    let name = body
        .name
        .as_deref()
        .map(config::validate_name)
        .transpose()?;

    let mut tx = state.db.begin_write().await?;
    let updated = config::update_card_type(
        &mut tx,
        &id,
        name.as_deref(),
        body.icon.as_ref().map(|i| i.as_deref()),
        body.colour.as_ref().map(|c| c.as_deref()),
        body.is_default,
    )
    .await?;
    tx.commit().await?;

    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// statuses
// ---------------------------------------------------------------------------

/// A project's statuses, in board order.
#[utoipa::path(
    get,
    path = "/projects/{key}/statuses",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's statuses", body = Vec<Status>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_statuses(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Status>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(config::statuses(&state.db, &project.id).await?))
}

/// Adds a status.
#[utoipa::path(
    post,
    path = "/projects/{key}/statuses",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateStatusRequest,
    responses(
        (status = 201, description = "Created", body = Status),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "The name or category is invalid", body = Problem),
    )
)]
async fn create_status(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateStatusRequest>,
) -> AppResult<(StatusCode, Json<Status>)> {
    let name = config::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let mut tx = state.db.begin_write().await?;
    let created =
        config::insert_status(&mut tx, &project.id, &name, body.category, body.position).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a status.
#[utoipa::path(
    patch,
    path = "/statuses/{id}",
    tag = "project-config",
    params(("id" = String, Path, description = "The status's id")),
    request_body = UpdateStatusRequest,
    responses(
        (status = 200, description = "Updated", body = Status),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such status", body = Problem),
        (status = 422, description = "The name or category is invalid", body = Problem),
    )
)]
async fn update_status(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateStatusRequest>,
) -> AppResult<Json<Status>> {
    let name = body
        .name
        .as_deref()
        .map(config::validate_name)
        .transpose()?;

    let mut tx = state.db.begin_write().await?;
    let updated =
        config::update_status(&mut tx, &id, name.as_deref(), body.category, body.position).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// priorities
// ---------------------------------------------------------------------------

/// A project's priorities, most urgent first.
#[utoipa::path(
    get,
    path = "/projects/{key}/priorities",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's priorities", body = Vec<Priority>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_priorities(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Priority>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(config::priorities(&state.db, &project.id).await?))
}

/// Adds a priority.
#[utoipa::path(
    post,
    path = "/projects/{key}/priorities",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreatePriorityRequest,
    responses(
        (status = 201, description = "Created", body = Priority),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn create_priority(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreatePriorityRequest>,
) -> AppResult<(StatusCode, Json<Priority>)> {
    let name = config::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let mut tx = state.db.begin_write().await?;
    let created = config::insert_priority(
        &mut tx,
        &project.id,
        &name,
        body.icon.as_deref(),
        body.colour.as_deref(),
        body.rank,
    )
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a priority.
#[utoipa::path(
    patch,
    path = "/priorities/{id}",
    tag = "project-config",
    params(("id" = String, Path, description = "The priority's id")),
    request_body = UpdatePriorityRequest,
    responses(
        (status = 200, description = "Updated", body = Priority),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such priority", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn update_priority(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdatePriorityRequest>,
) -> AppResult<Json<Priority>> {
    let name = body
        .name
        .as_deref()
        .map(config::validate_name)
        .transpose()?;

    let mut tx = state.db.begin_write().await?;
    let updated = config::update_priority(
        &mut tx,
        &id,
        name.as_deref(),
        body.icon.as_ref().map(|i| i.as_deref()),
        body.colour.as_ref().map(|c| c.as_deref()),
        body.rank,
    )
    .await?;
    tx.commit().await?;

    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// resolutions
// ---------------------------------------------------------------------------

/// A project's resolutions.
#[utoipa::path(
    get,
    path = "/projects/{key}/resolutions",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's resolutions", body = Vec<Resolution>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_resolutions(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Resolution>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(config::resolutions(&state.db, &project.id).await?))
}

/// Adds a resolution.
#[utoipa::path(
    post,
    path = "/projects/{key}/resolutions",
    tag = "project-config",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateResolutionRequest,
    responses(
        (status = 201, description = "Created", body = Resolution),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn create_resolution(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateResolutionRequest>,
) -> AppResult<(StatusCode, Json<Resolution>)> {
    let name = config::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let mut tx = state.db.begin_write().await?;
    let created = config::insert_resolution(&mut tx, &project.id, &name, body.position).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a resolution.
#[utoipa::path(
    patch,
    path = "/resolutions/{id}",
    tag = "project-config",
    params(("id" = String, Path, description = "The resolution's id")),
    request_body = UpdateResolutionRequest,
    responses(
        (status = 200, description = "Updated", body = Resolution),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot change configuration", body = Problem),
        (status = 404, description = "No such resolution", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn update_resolution(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateResolutionRequest>,
) -> AppResult<Json<Resolution>> {
    let name = body
        .name
        .as_deref()
        .map(config::validate_name)
        .transpose()?;

    let mut tx = state.db.begin_write().await?;
    let updated = config::update_resolution(&mut tx, &id, name.as_deref(), body.position).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// The per-project configuration routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_hierarchy_levels, create_hierarchy_level))
        .routes(routes!(update_hierarchy_level))
        .routes(routes!(list_card_types, create_card_type))
        .routes(routes!(update_card_type))
        .routes(routes!(list_statuses, create_status))
        .routes(routes!(update_status))
        .routes(routes!(list_priorities, create_priority))
        .routes(routes!(update_priority))
        .routes(routes!(list_resolutions, create_resolution))
        .routes(routes!(update_resolution))
}

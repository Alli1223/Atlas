//! The card's collaboration surface: watchers, worklogs, card links, remote links,
//! components, and milestones.
//!
//! Attachments live in [`crate::api::attachments`]; they stream bytes and want their own
//! module.
//!
//! Every route here is classified in [`crate::auth::project_access::SCOPES`], so the project
//! gate has already decided the caller may reach the card or project named in the path
//! before a handler runs. The one place a handler checks access itself is linking: the
//! *other* card arrives in the body, which the gate cannot see.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::api::serde_ext::double_option;
use crate::auth::extract::RequireMember;
use crate::auth::role::Role;
use crate::auth::{CurrentUser, now};
use crate::domain::card::{self, Card};
use crate::domain::links::{self, CardLink, LinkType, RemoteLink, Watcher};
use crate::domain::member::{self, ProjectRole};
use crate::domain::planning::{self, Component, Milestone, MilestonePatch, MilestoneStatus};
use crate::domain::project::{self, Project};
use crate::domain::worklog::{self, NewWorklog, Worklog, WorklogSummary};
use crate::error::{AppError, AppResult, Problem};

/// Loads a card by key (case-insensitive) or 404s.
async fn load_card(state: &AppState, key: &str) -> AppResult<Card> {
    card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)
}

/// Loads a project by key or 404s.
async fn load_project(state: &AppState, key: &str) -> AppResult<Project> {
    project::find_by_key(&state.db, key)
        .await?
        .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// Watchers
// ---------------------------------------------------------------------------

/// A card's watchers.
#[utoipa::path(
    get,
    path = "/cards/{key}/watchers",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Who is watching", body = Vec<Watcher>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_watchers(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Watcher>>> {
    let target = load_card(&state, &key).await?;
    Ok(Json(links::watchers(&state.db, &target.id).await?))
}

/// Starts watching a card — always as the caller. Anyone who can see a card may watch it.
#[utoipa::path(
    post,
    path = "/cards/{key}/watchers",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Watching; the updated list", body = Vec<Watcher>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn watch_card(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Watcher>>> {
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    links::watch(&mut tx, &target.id, current.id(), now()).await?;
    tx.commit().await?;
    Ok(Json(links::watchers(&state.db, &target.id).await?))
}

/// Stops someone watching a card: yourself, or anyone if you are an instance admin.
#[utoipa::path(
    delete,
    path = "/cards/{key}/watchers/{userId}",
    tag = "collaboration",
    params(
        ("key" = String, Path, description = "The card key"),
        ("userId" = String, Path, description = "The watcher"),
    ),
    responses(
        (status = 204, description = "No longer watching"),
        (status = 403, description = "Not yourself, and not an admin", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn unwatch_card(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((key, user_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    if user_id != current.id() && !current.has_role(Role::Admin) {
        return Err(AppError::Forbidden);
    }
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    links::unwatch(&mut tx, &target.id, &user_id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Worklogs
// ---------------------------------------------------------------------------

/// The body of `POST /cards/{key}/worklogs`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogWorkRequest {
    /// `2w 3d 4h 30m`, or a bare number of minutes.
    pub duration: String,
    /// What the time was spent on.
    #[serde(default)]
    pub note: Option<String>,
    /// When the work happened. Defaults to now.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
}

/// The body of `PATCH /worklogs/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditWorklogRequest {
    /// A new duration.
    #[serde(default)]
    pub duration: Option<String>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub note: Option<Option<String>>,
    /// When the work happened.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
}

/// A card's logged time, with the total.
#[utoipa::path(
    get,
    path = "/cards/{key}/worklogs",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Every worklog, and the total", body = WorklogSummary),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_worklogs(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<WorklogSummary>> {
    let target = load_card(&state, &key).await?;
    Ok(Json(worklog::summary(&state.db, &target.id).await?))
}

/// Logs time against a card.
#[utoipa::path(
    post,
    path = "/cards/{key}/worklogs",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    request_body = LogWorkRequest,
    responses(
        (status = 201, description = "Logged", body = Worklog),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "Not a duration", body = Problem),
    )
)]
async fn log_work(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<LogWorkRequest>,
) -> AppResult<(StatusCode, Json<Worklog>)> {
    let minutes = worklog::parse_duration(&body.duration)?;
    let note = worklog::validate_note(body.note.as_deref())?;
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    let logged = worklog::insert(
        &mut tx,
        &NewWorklog {
            card_id: &target.id,
            author_id: Some(member.0.id()),
            minutes,
            note,
            source: "manual",
            started_at: body.started_at,
        },
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(logged)))
}

/// Who may change a worklog: its author, or an instance admin.
fn may_edit_worklog(current: &CurrentUser, log: &Worklog) -> bool {
    log.author_id.as_deref() == Some(current.id()) || current.has_role(Role::Admin)
}

/// Edits a worklog. The author, or an instance admin.
#[utoipa::path(
    patch,
    path = "/worklogs/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The worklog")),
    request_body = EditWorklogRequest,
    responses(
        (status = 200, description = "Edited", body = Worklog),
        (status = 403, description = "Not yours", body = Problem),
        (status = 404, description = "No such worklog", body = Problem),
    )
)]
async fn edit_worklog(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<EditWorklogRequest>,
) -> AppResult<Json<Worklog>> {
    let minutes = body
        .duration
        .as_deref()
        .map(worklog::parse_duration)
        .transpose()?;
    let note = body
        .note
        .map(|n| worklog::validate_note(n.as_deref()))
        .transpose()?;
    let mut tx = state.db.begin_write().await?;
    let current = worklog::find_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_edit_worklog(&member.0, &current) {
        return Err(AppError::Forbidden);
    }
    let updated = worklog::update(&mut tx, &current, minutes, note, body.started_at, now()).await?;
    tx.commit().await?;
    Ok(Json(updated))
}

/// Deletes a worklog. The author, or an instance admin.
#[utoipa::path(
    delete,
    path = "/worklogs/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The worklog")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not yours", body = Problem),
        (status = 404, description = "No such worklog", body = Problem),
    )
)]
async fn delete_worklog(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let current = worklog::find_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !may_edit_worklog(&member.0, &current) {
        return Err(AppError::Forbidden);
    }
    worklog::delete(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Card links
// ---------------------------------------------------------------------------

/// The body of `POST /cards/{key}/links`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkCardRequest {
    /// The other card's key.
    pub target_key: String,
    /// How this card relates to it.
    pub link_type: LinkType,
}

/// A card's links to other cards.
#[utoipa::path(
    get,
    path = "/cards/{key}/links",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Outbound links (inverses are materialised)", body = Vec<CardLink>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_links(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardLink>>> {
    let target = load_card(&state, &key).await?;
    let all = links::list_card_links(&state.db, &target.id).await?;
    // A link to a card in a project the caller cannot see must not reveal that card's key
    // or summary — so the list is filtered per linked card's project, not just the source's.
    let mut visible = Vec::with_capacity(all.len());
    for link in all {
        if let Some(other) = card::find_by_id(&state.db, &link.card_id).await?
            && let Some(project) = project::find_by_id(&state.db, &other.project_id).await?
            && member::effective_role(&state.db, &project, &current.user)
                .await?
                .is_some()
        {
            visible.push(link);
        }
    }
    Ok(Json(visible))
}

/// Links this card to another. You must be able to edit both.
#[utoipa::path(
    post,
    path = "/cards/{key}/links",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    request_body = LinkCardRequest,
    responses(
        (status = 200, description = "Linked; this card's links", body = Vec<CardLink>),
        (status = 404, description = "No such card (either end)", body = Problem),
        (status = 422, description = "A card cannot link to itself", body = Problem),
    )
)]
async fn link_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<LinkCardRequest>,
) -> AppResult<Json<Vec<CardLink>>> {
    let source = load_card(&state, &key).await?;
    let other = load_card(&state, &body.target_key).await?;
    if other.is_deleted() {
        return Err(AppError::NotFound);
    }
    // The gate checked the source; the body named the target, which it cannot see. The
    // inverse row lands on the target card, so linking is a write to both ends.
    let other_project = project::find_by_id(&state.db, &other.project_id)
        .await?
        .ok_or(AppError::NotFound)?;
    member::require(
        &state.db,
        &other_project,
        &member.0.user,
        ProjectRole::Member,
    )
    .await?;

    let mut tx = state.db.begin_write().await?;
    links::link_cards(&mut tx, &source.id, &other.id, body.link_type, now()).await?;
    tx.commit().await?;
    Ok(Json(links::list_card_links(&state.db, &source.id).await?))
}

/// Removes a link (and its inverse).
#[utoipa::path(
    delete,
    path = "/cards/{key}/links/{linkId}",
    tag = "collaboration",
    params(
        ("key" = String, Path, description = "The card key"),
        ("linkId" = String, Path, description = "The link, as listed on this card"),
    ),
    responses(
        (status = 204, description = "Unlinked"),
        (status = 404, description = "No such card or link", body = Problem),
    )
)]
async fn unlink_card(
    State(state): State<AppState>,
    _member: RequireMember,
    Path((key, link_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let source = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    if !links::unlink_cards(&mut tx, &source.id, &link_id).await? {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Remote links
// ---------------------------------------------------------------------------

/// The body of `POST /cards/{key}/remote-links`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddRemoteLinkRequest {
    /// An `http(s)` URL.
    pub url: String,
    /// What to show instead of the URL. Defaults to the URL.
    #[serde(default)]
    pub title: Option<String>,
    /// An optional icon name.
    #[serde(default)]
    pub icon: Option<String>,
}

/// A card's remote (web) links.
#[utoipa::path(
    get,
    path = "/cards/{key}/remote-links",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's web links", body = Vec<RemoteLink>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_remote_links(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<RemoteLink>>> {
    let target = load_card(&state, &key).await?;
    Ok(Json(links::list_remote(&state.db, &target.id).await?))
}

/// Adds a web link to a card. Only `http`/`https` URLs are accepted.
#[utoipa::path(
    post,
    path = "/cards/{key}/remote-links",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    request_body = AddRemoteLinkRequest,
    responses(
        (status = 201, description = "Added", body = RemoteLink),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "Not an http(s) URL", body = Problem),
    )
)]
async fn add_remote_link(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<AddRemoteLinkRequest>,
) -> AppResult<(StatusCode, Json<RemoteLink>)> {
    let url = links::validate_url(&body.url)?;
    let title = links::validate_title(body.title.as_deref(), &url)?;
    let icon = body
        .icon
        .as_deref()
        .map(str::trim)
        .filter(|i| !i.is_empty() && i.len() <= 64 && i.bytes().all(|b| b.is_ascii_graphic()));
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    let added = links::add_remote(
        &mut tx,
        &target.id,
        &url,
        &title,
        icon,
        Some(member.0.id()),
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(added)))
}

/// Removes a web link from a card.
#[utoipa::path(
    delete,
    path = "/cards/{key}/remote-links/{linkId}",
    tag = "collaboration",
    params(
        ("key" = String, Path, description = "The card key"),
        ("linkId" = String, Path, description = "The remote link"),
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "No such card or link", body = Problem),
    )
)]
async fn remove_remote_link(
    State(state): State<AppState>,
    _member: RequireMember,
    Path((key, link_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    if !links::remove_remote(&mut tx, &target.id, &link_id).await? {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// The body of `POST /projects/{key}/components`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateComponentRequest {
    /// Unique within the project.
    pub name: String,
    /// What the area covers.
    #[serde(default)]
    pub description: Option<String>,
    /// The area's lead.
    #[serde(default)]
    pub lead_id: Option<String>,
}

/// The body of `PATCH /components/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateComponentRequest {
    /// A new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub lead_id: Option<Option<String>>,
}

/// The body of `PUT /cards/{key}/components`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetCardComponentsRequest {
    /// The complete set; replaces whatever was there.
    pub component_ids: Vec<String>,
}

async fn check_lead(state: &AppState, lead_id: Option<&str>) -> AppResult<()> {
    if let Some(id) = lead_id {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(state.db.reader())
            .await?;
        if exists.is_none() {
            return Err(AppError::Validation(format!("{id:?} is not a user.")));
        }
    }
    Ok(())
}

/// A project's components.
#[utoipa::path(
    get,
    path = "/projects/{key}/components",
    tag = "collaboration",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Components, by name", body = Vec<Component>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_components(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Component>>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        planning::list_components(&state.db, &project.id).await?,
    ))
}

/// Creates a component.
#[utoipa::path(
    post,
    path = "/projects/{key}/components",
    tag = "collaboration",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateComponentRequest,
    responses(
        (status = 201, description = "Created", body = Component),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "Invalid name or lead", body = Problem),
    )
)]
async fn create_component(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateComponentRequest>,
) -> AppResult<(StatusCode, Json<Component>)> {
    let name = planning::validate_name(&body.name)?;
    let description = planning::validate_description(body.description.as_deref())?;
    check_lead(&state, body.lead_id.as_deref()).await?;
    let project = load_project(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    let created = planning::insert_component(
        &mut tx,
        &project.id,
        &name,
        description.as_deref(),
        body.lead_id.as_deref(),
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a component.
#[utoipa::path(
    patch,
    path = "/components/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The component")),
    request_body = UpdateComponentRequest,
    responses(
        (status = 200, description = "Edited", body = Component),
        (status = 404, description = "No such component", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
    )
)]
async fn update_component(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateComponentRequest>,
) -> AppResult<Json<Component>> {
    let name = body
        .name
        .as_deref()
        .map(planning::validate_name)
        .transpose()?;
    let description = body
        .description
        .map(|d| planning::validate_description(d.as_deref()))
        .transpose()?;
    if let Some(Some(lead)) = &body.lead_id {
        check_lead(&state, Some(lead)).await?;
    }
    let mut tx = state.db.begin_write().await?;
    let current = planning::find_component_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updated = planning::update_component(
        &mut tx,
        &current,
        name.as_deref(),
        description,
        body.lead_id,
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(updated))
}

/// Deletes a component. Its cards lose the membership, nothing else.
#[utoipa::path(
    delete,
    path = "/components/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The component")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such component", body = Problem),
    )
)]
async fn delete_component(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    planning::find_component_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    planning::delete_component(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The components a card is in.
#[utoipa::path(
    get,
    path = "/cards/{key}/components",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's components", body = Vec<Component>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_card_components(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Component>>> {
    let target = load_card(&state, &key).await?;
    Ok(Json(
        planning::card_components(&state.db, &target.id).await?,
    ))
}

/// Replaces the set of components a card is in.
#[utoipa::path(
    put,
    path = "/cards/{key}/components",
    tag = "collaboration",
    params(("key" = String, Path, description = "The card key")),
    request_body = SetCardComponentsRequest,
    responses(
        (status = 200, description = "The card's components now", body = Vec<Component>),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "A component of another project", body = Problem),
    )
)]
async fn set_card_components(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<SetCardComponentsRequest>,
) -> AppResult<Json<Vec<Component>>> {
    let target = load_card(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    planning::set_card_components(&mut tx, &target.id, &target.project_id, &body.component_ids)
        .await?;
    tx.commit().await?;
    Ok(Json(
        planning::card_components(&state.db, &target.id).await?,
    ))
}

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

/// The body of `POST /projects/{key}/milestones` and `PATCH /milestones/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MilestoneRequest {
    /// Required on create.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub release_date: Option<Option<NaiveDate>>,
    /// `planned`, `released` (stamps `releasedAt`), `archived`. Edit only.
    #[serde(default)]
    pub status: Option<MilestoneStatus>,
}

impl MilestoneRequest {
    fn into_patch(self) -> AppResult<MilestonePatch> {
        Ok(MilestonePatch {
            name: self
                .name
                .as_deref()
                .map(planning::validate_name)
                .transpose()?,
            description: self
                .description
                .map(|d| planning::validate_description(d.as_deref()))
                .transpose()?,
            start_date: self.start_date,
            release_date: self.release_date,
            status: self.status,
        })
    }
}

/// A project's milestones, with progress.
#[utoipa::path(
    get,
    path = "/projects/{key}/milestones",
    tag = "collaboration",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Planned (by date), released, archived", body = Vec<Milestone>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_milestones(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Milestone>>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        planning::list_milestones(&state.db, &project.id).await?,
    ))
}

/// Creates a milestone.
#[utoipa::path(
    post,
    path = "/projects/{key}/milestones",
    tag = "collaboration",
    params(("key" = String, Path, description = "The project key")),
    request_body = MilestoneRequest,
    responses(
        (status = 201, description = "Created", body = Milestone),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "Invalid name or dates", body = Problem),
    )
)]
async fn create_milestone(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<MilestoneRequest>,
) -> AppResult<(StatusCode, Json<Milestone>)> {
    let patch = body.into_patch()?;
    let name = patch
        .name
        .clone()
        .ok_or_else(|| AppError::Validation("A milestone needs a name.".to_owned()))?;
    let project = load_project(&state, &key).await?;
    let mut tx = state.db.begin_write().await?;
    let created = planning::insert_milestone(&mut tx, &project.id, &name, &patch, now()).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a milestone, including releasing or archiving it.
#[utoipa::path(
    patch,
    path = "/milestones/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The milestone")),
    request_body = MilestoneRequest,
    responses(
        (status = 200, description = "Edited", body = Milestone),
        (status = 404, description = "No such milestone", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
    )
)]
async fn update_milestone(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<MilestoneRequest>,
) -> AppResult<Json<Milestone>> {
    let patch = body.into_patch()?;
    let mut tx = state.db.begin_write().await?;
    let current = planning::find_milestone_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updated = planning::update_milestone(&mut tx, &current, &patch, now()).await?;
    tx.commit().await?;
    Ok(Json(updated))
}

/// Deletes a milestone. Its cards are un-scheduled, not deleted.
#[utoipa::path(
    delete,
    path = "/milestones/{id}",
    tag = "collaboration",
    params(("id" = String, Path, description = "The milestone")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such milestone", body = Problem),
    )
)]
async fn delete_milestone(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    planning::find_milestone_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    planning::delete_milestone(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The collaboration routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_watchers, watch_card))
        .routes(routes!(unwatch_card))
        .routes(routes!(list_worklogs, log_work))
        .routes(routes!(edit_worklog, delete_worklog))
        .routes(routes!(list_links, link_card))
        .routes(routes!(unlink_card))
        .routes(routes!(list_remote_links, add_remote_link))
        .routes(routes!(remove_remote_link))
        .routes(routes!(list_components, create_component))
        .routes(routes!(update_component, delete_component))
        .routes(routes!(list_card_components, set_card_components))
        .routes(routes!(list_milestones, create_milestone))
        .routes(routes!(update_milestone, delete_milestone))
}

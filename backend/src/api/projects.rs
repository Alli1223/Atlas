//! `/api/v1/projects` — projects and their lifecycle.

// See the note at the top of `api::cards`: `params(<IntoParams type>)` expands
// to a qualified path in a sibling item that carries the handler signature's
// span, so the lint has to be silenced at module scope to reach it.
#![allow(unused_qualifications)]

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::api::serde_ext::double_option;
use crate::auth::extract::{RequireAdmin, RequireMember};
use crate::auth::{now, user};
use crate::db::Db;
use crate::domain::EstimationUnit;
use crate::domain::member::{self, ProjectRole};
use crate::domain::project::{self, Project, ProjectDto, ProjectPatch};
use crate::domain::template::{self, Template};
use crate::error::{AppError, AppResult, Problem};

/// Loads a project by key, or 404s.
///
/// The key lookup is case-insensitive because the column is `COLLATE NOCASE`, so
/// `/projects/atlas` and `/projects/ATLAS` are the same project — which is what
/// anyone typing a URL expects.
pub(crate) async fn by_key(db: &Db, key: &str) -> AppResult<Project> {
    project::find_by_key(db, key)
        .await?
        .ok_or(AppError::NotFound)
}

/// Query parameters for `GET /projects`.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListProjectsQuery {
    /// Whether archived projects are included. Defaults to false.
    #[serde(default)]
    pub include_archived: Option<bool>,
}

/// The body of `POST /projects`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectRequest {
    /// The card-key prefix: `ATLAS` in `ATLAS-123`. Uppercased on the way in.
    #[schema(example = "ATLAS")]
    pub key: String,
    /// The human name.
    pub name: String,
    /// Optional markdown description.
    #[serde(default)]
    pub description: Option<String>,
    /// The project lead's user id.
    #[serde(default)]
    pub lead_id: Option<String>,
    /// Which template seeds the project's levels, types, statuses, priorities
    /// and resolutions. Defaults to `blank`.
    #[serde(default)]
    pub template: Template,
}

/// The body of `PATCH /projects/{key}`.
///
/// `key` is not here on purpose: renaming a project key would invalidate every
/// card key under it. That is a bulk move, not a field edit.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProjectRequest {
    /// The human name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub lead_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub avatar_url: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub cover_image_url: Option<Option<String>>,
    /// Whether cycles are on for this project.
    #[serde(default)]
    pub cycles_enabled: Option<bool>,
    /// How the project's single `estimate` field is interpreted.
    #[serde(default)]
    pub estimation_unit: Option<EstimationUnit>,
}

/// One template, as the picker describes it.
#[derive(Debug, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemplateDto {
    /// The wire spelling: `programming`, `3d-modeling`, `job-search`, `blank`.
    pub id: String,
    /// A one-line description.
    pub description: String,
    /// What this template calls its hierarchy rungs, deepest last.
    pub levels: Vec<String>,
    /// The workflow this template seeds, in board order.
    pub statuses: Vec<String>,
}

/// Every project the caller can reach.
///
/// # This filters; it does not refuse
///
/// A project the caller has no access to is simply **absent**, and this is the
/// one route [`crate::auth::project_access`] classifies `SelfFiltered` for
/// exactly that reason. A 403 on a list would be a bug twice over: it would turn
/// "here is your work" into "you are not allowed to have work", and it would
/// confirm to an outsider that there is something there to be refused.
///
/// The filtering is [`project::list_for`], which takes the viewer and has no
/// unscoped sibling that could be called by mistake.
#[utoipa::path(
    get,
    path = "/projects",
    tag = "projects",
    params(ListProjectsQuery),
    responses(
        (status = 200, description = "Every project the caller can reach, by name", body = Vec<ProjectDto>),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn list_projects(
    State(state): State<AppState>,
    current: crate::auth::CurrentUser,
    Query(query): Query<ListProjectsQuery>,
) -> AppResult<Json<Vec<ProjectDto>>> {
    let projects = project::list_for(
        &state.db,
        &current.user,
        query.include_archived.unwrap_or(false),
    )
    .await?;
    Ok(Json(projects.iter().map(ProjectDto::from).collect()))
}

/// The templates a project can be created from.
///
/// Ahead of Phase 18's wizard, and deliberately: the template list is data the
/// client needs to render a picker, and it is the same data the domain seeds
/// from. Two copies of it would disagree.
#[utoipa::path(
    get,
    path = "/project-templates",
    tag = "projects",
    responses(
        (status = 200, description = "Every project template", body = Vec<TemplateDto>),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn list_templates(_current: crate::auth::CurrentUser) -> Json<Vec<TemplateDto>> {
    Json(
        Template::all()
            .into_iter()
            .map(|template| TemplateDto {
                id: template.as_str().to_owned(),
                description: template.description().to_owned(),
                levels: template
                    .levels()
                    .iter()
                    .map(|(_, name)| (*name).to_owned())
                    .collect(),
                statuses: template
                    .statuses()
                    .iter()
                    .map(|(name, ..)| (*name).to_owned())
                    .collect(),
            })
            .collect(),
    )
}

/// Creates a project, seeded from a template.
#[utoipa::path(
    post,
    path = "/projects",
    tag = "projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 201, description = "Created, with every config row its template calls for", body = ProjectDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot create projects", body = Problem),
        (status = 409, description = "The key is taken", body = Problem),
        (status = 422, description = "The key or name is invalid", body = Problem),
    )
)]
async fn create_project(
    State(state): State<AppState>,
    member: RequireMember,
    Json(body): Json<CreateProjectRequest>,
) -> AppResult<(StatusCode, Json<ProjectDto>)> {
    let key = project::validate_key(&body.key)?;
    let name = project::validate_name(&body.name)?;
    let description = body
        .description
        .as_deref()
        .map(project::validate_description)
        .transpose()?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    // Checked inside the transaction so the check and the insert cannot be
    // separated by another writer. The UNIQUE index is the real guarantee; this
    // turns its 500 into a 409 that says what to fix.
    if project::key_taken(&mut tx, &key, None).await? {
        return Err(AppError::Conflict(format!(
            "The project key {key:?} is already taken."
        )));
    }

    // Checked rather than left to the foreign key: `projects.lead_id` REFERENCES
    // users (id), so an id naming nobody is a "FOREIGN KEY constraint failed"
    // that reaches the caller as a 500 they can do nothing with. The FK is still
    // the guarantee; this is the same argument `members::add_member` makes about
    // its own `userId`, and the two routes take the same kind of input.
    let lead_id = match body.lead_id.as_deref() {
        Some(id) => {
            user::find_by_id_tx(&mut tx, id)
                .await?
                .ok_or_else(|| AppError::Validation(format!("No user with id {id:?}.")))?
                .id
        }
        None => member.0.id().to_owned(),
    };
    let lead_id = lead_id.as_str();

    // The project and all ~25 of its config rows, or none of them. A project
    // with no statuses is a project no card can be created in.
    let created = template::create_project(
        &mut tx,
        body.template,
        &key,
        &name,
        description.as_deref(),
        Some(lead_id),
        now,
    )
    .await?;

    // The creator owns what they created — in the same transaction, because a
    // project whose owner row failed to land is a project its own creator cannot
    // reach. Default deny means there is no second chance here: nothing but an
    // instance admin could repair it.
    //
    // The lead gets a row too. They are an implicit owner via `projects.lead_id`
    // regardless (rule 2 of `domain::member::resolve`), so this grants nothing
    // new — it makes the member list *honest*, so that `GET .../members` shows
    // the person running the project rather than an empty list with a footnote.
    // `insert_or_ignore` because the two are the same person in the common case.
    member::insert_or_ignore(
        &mut tx,
        &created.id,
        member.0.id(),
        ProjectRole::Owner,
        Some(member.0.id()),
        now,
    )
    .await?;
    member::insert_or_ignore(
        &mut tx,
        &created.id,
        lead_id,
        ProjectRole::Owner,
        Some(member.0.id()),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(ProjectDto::from(&created))))
}

/// One project.
#[utoipa::path(
    get,
    path = "/projects/{key}",
    tag = "projects",
    params(("key" = String, Path, description = "The project key, e.g. ATLAS")),
    responses(
        (status = 200, description = "The project", body = ProjectDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn get_project(
    State(state): State<AppState>,
    _current: crate::auth::CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<ProjectDto>> {
    Ok(Json(ProjectDto::from(&by_key(&state.db, &key).await?)))
}

/// Edits a project.
#[utoipa::path(
    patch,
    path = "/projects/{key}",
    tag = "projects",
    params(("key" = String, Path, description = "The project key")),
    request_body = UpdateProjectRequest,
    responses(
        (status = 200, description = "Updated", body = ProjectDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot edit projects", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn update_project(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<UpdateProjectRequest>,
) -> AppResult<Json<ProjectDto>> {
    let patch = ProjectPatch {
        name: body
            .name
            .as_deref()
            .map(project::validate_name)
            .transpose()?,
        description: body
            .description
            .map(|d| d.as_deref().map(project::validate_description).transpose())
            .transpose()?,
        lead_id: body.lead_id,
        avatar_url: body.avatar_url,
        cover_image_url: body.cover_image_url,
        cycles_enabled: body.cycles_enabled,
        estimation_unit: body.estimation_unit,
    };

    if patch.is_empty() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    // Naming a lead **is** a grant of ownership — rule 2 of `member::resolve`
    // makes the lead an owner with no row at all. So it gets the same treatment
    // the same act gets in `create_project`: the user is checked (a bad id is a
    // 422, not the foreign key's 500), and they are written an explicit row.
    let new_lead = match patch.lead_id.as_ref().and_then(Option::as_ref) {
        Some(id) => Some(
            user::find_by_id_tx(&mut tx, id)
                .await?
                .ok_or_else(|| AppError::Validation(format!("No user with id {id:?}.")))?,
        ),
        None => None,
    };

    project::apply_patch(&mut tx, &target.id, &patch, now).await?;

    // Why the row, when the lead is already an owner by rule: because
    // `member::list` lists *rows*, so without it the member list — the one place
    // an owner audits who can reach the project — would not mention the person
    // who just became its owner. `create_project` writes this row for exactly
    // that reason ("it makes the member list honest"), and an act that grants
    // ownership must not be honest in one route and silent in the other.
    //
    // `insert_or_ignore`, because the new lead is very often already a member:
    // promoting them is the owner's business, and this must not quietly demote a
    // grant somebody made on purpose, nor fail because one exists.
    if let Some(lead) = &new_lead {
        member::insert_or_ignore(
            &mut tx,
            &target.id,
            &lead.id,
            ProjectRole::Owner,
            Some(member.0.id()),
            now,
        )
        .await?;
    }

    let updated = project::find_by_id_tx(&mut tx, &target.id)
        .await?
        .ok_or(AppError::NotFound)?;

    tx.commit().await?;

    Ok(Json(ProjectDto::from(&updated)))
}

/// Archives a project.
///
/// The reversible answer to "get this off my screen", and the one almost every
/// caller of `DELETE` actually wants. Nothing is destroyed and every card key
/// still resolves.
#[utoipa::path(
    post,
    path = "/projects/{key}/archive",
    tag = "projects",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Archived", body = ProjectDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot archive projects", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn archive_project(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
) -> AppResult<Json<ProjectDto>> {
    set_archived(&state, &key, true).await
}

/// Brings an archived project back.
#[utoipa::path(
    post,
    path = "/projects/{key}/restore",
    tag = "projects",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Restored", body = ProjectDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot restore projects", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn restore_project(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
) -> AppResult<Json<ProjectDto>> {
    set_archived(&state, &key, false).await
}

async fn set_archived(state: &AppState, key: &str, archived: bool) -> AppResult<Json<ProjectDto>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = project::find_by_key_tx(&mut tx, key)
        .await?
        .ok_or(AppError::NotFound)?;

    project::set_archived(&mut tx, &target.id, archived, now).await?;

    let updated = project::find_by_id_tx(&mut tx, &target.id)
        .await?
        .ok_or(AppError::NotFound)?;

    tx.commit().await?;

    Ok(Json(ProjectDto::from(&updated)))
}

/// Permanently deletes a project and everything in it.
///
/// # Why this is admin-only, and why it exists at all
///
/// It is the only hard delete in Atlas. Cards are soft-deleted, users are
/// deactivated, comments are the sole other exception — because everything else
/// is referenced by something that outlives it. A project is the one thing whose
/// removal is sometimes genuinely meant: a typo'd key, a test project, an import
/// gone wrong.
///
/// Every card, comment, history row, config row and key redirect goes with it,
/// and no bookmark or commit message that ever mentioned one of those keys will
/// resolve again. That is why it is Admin and archive is Member: the difference
/// between the two is not permission, it is reversibility.
#[utoipa::path(
    delete,
    path = "/projects/{key}",
    tag = "projects",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 204, description = "Deleted, with every card, comment and history row in it"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn delete_project(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(key): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;

    let target = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    project::delete(&mut tx, &target.id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The `/projects` routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{key}`, never `:key` — the 0.7 syntax is a runtime panic.
        .routes(routes!(list_projects, create_project))
        .routes(routes!(list_templates))
        .routes(routes!(get_project, update_project, delete_project))
        .routes(routes!(archive_project))
        .routes(routes!(restore_project))
}

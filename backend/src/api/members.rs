//! `/api/v1/projects/{key}/members` — per-project access.
//!
//! # Where the authorisation is
//!
//! **Not in this file.** Every route here is classified `Scope::Project(Owner)`
//! (or `Viewer`, for the list) in [`crate::auth::project_access::SCOPES`], and
//! the layer has already refused anyone who may not be here by the time a handler
//! below runs. A reader looking for the `if !is_owner { return 403 }` will not
//! find one, and that is the design — see that module's header.
//!
//! What *is* here is the one rule the layer cannot express: the last owner of a
//! project cannot be removed or demoted.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::{AppState, projects};
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now, user};
use crate::domain::member::{self, ProjectMemberDto, ProjectRole};
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /projects/{key}/members`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMemberRequest {
    /// Who to grant access to.
    pub user_id: String,
    /// What to grant them. Capped by their instance role on the way out — see
    /// [`crate::domain::member::ProjectRole::capped_by`].
    pub role: ProjectRole,
}

/// The body of `PATCH /projects/{key}/members/{userId}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMemberRequest {
    /// The new role.
    pub role: ProjectRole,
}

/// Who has been granted access to a project.
///
/// # Why an instance admin is not in this list
///
/// Admins own every project by rule and hold no row, so listing them here would
/// make every project's member list a copy of the user list. This is the *grant*
/// list — "who has been given access to this" — not "everyone who could open it".
/// The project's lead appears only if they also hold a row; they are an owner
/// either way.
#[utoipa::path(
    get,
    path = "/projects/{key}/members",
    tag = "project-members",
    params(("key" = String, Path, description = "The project key, e.g. ATLAS")),
    responses(
        (status = 200, description = "The project's members, by display name", body = Vec<ProjectMemberDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project, or no access to it", body = Problem),
    )
)]
async fn list_members(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<ProjectMemberDto>>> {
    let project = projects::by_key(&state.db, &key).await?;
    let members = member::list(&state.db, &project.id).await?;
    Ok(Json(members.iter().map(ProjectMemberDto::from).collect()))
}

/// Grants someone access to a project.
#[utoipa::path(
    post,
    path = "/projects/{key}/members",
    tag = "project-members",
    params(("key" = String, Path, description = "The project key")),
    request_body = AddMemberRequest,
    responses(
        (status = 201, description = "Granted", body = ProjectMemberDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an owner of this project", body = Problem),
        (status = 404, description = "No such project, or no access to it", body = Problem),
        (status = 409, description = "They are already a member", body = Problem),
        (status = 422, description = "No such user", body = Problem),
    )
)]
async fn add_member(
    State(state): State<AppState>,
    member_of_instance: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<AddMemberRequest>,
) -> AppResult<(StatusCode, Json<ProjectMemberDto>)> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    // Checked rather than left to the foreign key: "FOREIGN KEY constraint
    // failed" is a 500 the caller cannot act on, and this is a 422 that says
    // what is wrong. The FK is still the guarantee.
    let target = user::find_by_id_tx(&mut tx, &body.user_id)
        .await?
        .ok_or_else(|| AppError::Validation(format!("No user with id {:?}.", body.user_id)))?;

    // Checked inside the transaction so the check and the insert cannot be
    // separated by another writer. The PRIMARY KEY is the real guarantee; this
    // turns its 500 into a 409 that says what to do instead.
    if member::find_role_tx(&mut tx, &project.id, &target.id)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(format!(
            "{:?} is already a member of {}. PATCH their membership to change their role.",
            target.username, project.key
        )));
    }

    member::insert(
        &mut tx,
        &project.id,
        &target.id,
        body.role,
        Some(member_of_instance.0.id()),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(dto(&state, &project.id, &target.id).await?),
    ))
}

/// Changes what someone may do on a project.
#[utoipa::path(
    patch,
    path = "/projects/{key}/members/{userId}",
    tag = "project-members",
    params(
        ("key" = String, Path, description = "The project key"),
        ("userId" = String, Path, description = "The member's user id"),
    ),
    request_body = UpdateMemberRequest,
    responses(
        (status = 200, description = "Updated", body = ProjectMemberDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an owner of this project", body = Problem),
        (status = 404, description = "No such project or member", body = Problem),
        (status = 409, description = "This is the project's only owner", body = Problem),
    )
)]
async fn update_member(
    State(state): State<AppState>,
    _member: RequireMember,
    Path((key, user_id)): Path<(String, String)>,
    Json(body): Json<UpdateMemberRequest>,
) -> AppResult<Json<ProjectMemberDto>> {
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    let target = user::find_by_id_tx(&mut tx, &user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let current_role = member::find_role_tx(&mut tx, &project.id, &user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Only a grant that actually confers ownership can be the last one — see
    // `member::grants_ownership`. An `owner` row held by an instance Viewer
    // resolves to `viewer`, so demoting it takes nothing away and must not be
    // refused as though it did.
    if member::grants_ownership(current_role, &target)
        && !member::grants_ownership(body.role, &target)
    {
        guard_last_owner(&mut tx, &project.id, &project.key).await?;
    }

    if !member::update_role(&mut tx, &project.id, &user_id, body.role).await? {
        return Err(AppError::NotFound);
    }

    tx.commit().await?;

    Ok(Json(dto(&state, &project.id, &user_id).await?))
}

/// Revokes someone's access to a project.
#[utoipa::path(
    delete,
    path = "/projects/{key}/members/{userId}",
    tag = "project-members",
    params(
        ("key" = String, Path, description = "The project key"),
        ("userId" = String, Path, description = "The member's user id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an owner of this project", body = Problem),
        (status = 404, description = "No such project or member", body = Problem),
        (status = 409, description = "This is the project's only owner", body = Problem),
    )
)]
async fn remove_member(
    State(state): State<AppState>,
    _member: RequireMember,
    Path((key, user_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    let target = user::find_by_id_tx(&mut tx, &user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let current_role = member::find_role_tx(&mut tx, &project.id, &user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if member::grants_ownership(current_role, &target) {
        guard_last_owner(&mut tx, &project.id, &project.key).await?;
    }

    if !member::delete(&mut tx, &project.id, &user_id).await? {
        return Err(AppError::NotFound);
    }

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Refuses to leave a project's member list with no owner in it.
///
/// The exact shape of the last-active-admin guard in [`crate::api::users`], and
/// for the same reason one level down: a project whose member list contains
/// nobody who can manage it can only be repaired by an instance admin, and
/// "escalate to an admin" is precisely the outcome that guard exists to avoid.
///
/// Called *inside* the write transaction, before the change, so the count it
/// reads cannot move underneath it — `begin_write`'s `BEGIN IMMEDIATE` holds the
/// write lock for the whole transaction.
///
/// # Why it counts rows rather than owners
///
/// Instance admins own every project implicitly, and so does the lead, so it is
/// fair to ask what this protects. It protects against the *combination*:
/// `PATCH /projects/{key}` can set `lead_id` to null, so a member list with no
/// owner is one field edit away from a project with no owner at all. Counting
/// the lead here would make the guard depend on a column the same API can clear a
/// moment later. See `domain::member::owner_count`.
async fn guard_last_owner(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    project_key: &str,
) -> AppResult<()> {
    if member::owner_count(&mut *tx, project_id).await? <= 1 {
        return Err(AppError::Conflict(format!(
            "This is the only owner of {project_key}. Make someone else an owner first, or \
             nobody will be able to manage this project."
        )));
    }
    Ok(())
}

/// Re-reads one member for the response.
///
/// Reads after the commit rather than building a DTO from the request: the DTO
/// carries `effectiveRole`, which depends on the target's instance role and on
/// whether they lead the project — neither of which the request said anything
/// about, and both of which the caller needs to see. A hand-assembled DTO here
/// would be a second copy of `member::resolve`'s rules.
async fn dto(state: &AppState, project_id: &str, user_id: &str) -> AppResult<ProjectMemberDto> {
    member::list(&state.db, project_id)
        .await?
        .iter()
        .find(|row| row.user_id == user_id)
        .map(ProjectMemberDto::from)
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!(
                "the project member just written is missing"
            ))
        })
}

/// The project-member routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{key}`, never `:key` — the 0.7 syntax is a runtime panic.
        .routes(routes!(list_members, add_member))
        .routes(routes!(update_member, remove_member))
}

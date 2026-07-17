//! `/api/v1/cards/{key}/comments` and `/api/v1/comments/{id}`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::role::Role;
use crate::auth::{CurrentUser, now};
use crate::domain::card;
use crate::domain::comment::{self, Comment};
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /cards/{key}/comments` and `PATCH /comments/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommentBodyRequest {
    /// Markdown **source**. Rendered and sanitised by the client at read time,
    /// never stored as HTML.
    pub body: String,
}

/// Every comment on a card, oldest first.
#[utoipa::path(
    get,
    path = "/cards/{key}/comments",
    tag = "comments",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's comments", body = Vec<Comment>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_comments(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Comment>>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(comment::list(&state.db, &target.id).await?))
}

/// Posts a comment.
#[utoipa::path(
    post,
    path = "/cards/{key}/comments",
    tag = "comments",
    params(("key" = String, Path, description = "The card key")),
    request_body = CommentBodyRequest,
    responses(
        (status = 201, description = "Posted", body = Comment),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot comment", body = Problem),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "The comment is empty or too long", body = Problem),
    )
)]
async fn create_comment(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CommentBodyRequest>,
) -> AppResult<(StatusCode, Json<Comment>)> {
    let text = comment::validate_body(&body.body)?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let created = comment::insert(&mut tx, &target.id, member.0.id(), &text, now).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// Edits a comment.
///
/// **Authors only, admins included nowhere.** Editing puts words in someone's
/// mouth under their name, and there is no administrative reason to do that —
/// the remedy for a comment an admin objects to is deletion, which is visible.
/// This is the one place an admin deliberately has *less* power than the rules
/// elsewhere would suggest.
#[utoipa::path(
    patch,
    path = "/comments/{id}",
    tag = "comments",
    params(("id" = String, Path, description = "The comment's id")),
    request_body = CommentBodyRequest,
    responses(
        (status = 200, description = "Edited, and marked as edited", body = Comment),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not the author", body = Problem),
        (status = 404, description = "No such comment", body = Problem),
        (status = 422, description = "The comment is empty or too long", body = Problem),
    )
)]
async fn update_comment(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<CommentBodyRequest>,
) -> AppResult<Json<Comment>> {
    let text = comment::validate_body(&body.body)?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = comment::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if target.author_id != member.0.id() {
        return Err(AppError::Forbidden);
    }

    let updated = comment::update(&mut tx, &target, &text, now).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// Deletes a comment.
///
/// The author, or an admin. An admin can remove a comment they did not write —
/// unlike editing — because taking words away is not the same as putting words
/// in, and moderation has to be possible on a self-hosted instance with no other
/// recourse.
#[utoipa::path(
    delete,
    path = "/comments/{id}",
    tag = "comments",
    params(("id" = String, Path, description = "The comment's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not the author, and not an admin", body = Problem),
        (status = 404, description = "No such comment", body = Problem),
    )
)]
async fn delete_comment(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;

    let target = comment::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if target.author_id != member.0.id() && !member.0.has_role(Role::Admin) {
        return Err(AppError::Forbidden);
    }

    comment::delete(&mut tx, &id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The comment routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_comments, create_comment))
        .routes(routes!(update_comment, delete_comment))
}

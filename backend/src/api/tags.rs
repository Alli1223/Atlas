//! `/api/v1/projects/{key}/tags`, `/api/v1/tags/{id}` and
//! `/api/v1/cards/{key}/tags` — free-text labels.
//!
//! The domain rules live in [`crate::domain::tag`]; this module is the HTTP
//! shape over them, as `api::project_config` is over `domain::config`.

// See the note at the top of `api::cards`: `params(<IntoParams type>)` expands
// to a qualified path in a sibling item that carries the handler signature's
// span, so the lint has to be silenced at module scope to reach it.
#![allow(unused_qualifications)]

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{AppState, projects};
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::db::Db;
use crate::domain::card::Card;
use crate::domain::tag::{self, Tag, TagColour, TagPatch, TagUsage};
use crate::error::{AppError, AppResult, Problem};

/// Loads a card by key, or 404s. Case-insensitive on the key.
///
/// A private copy of `api::cards`' helper of the same name rather than a shared
/// one: that module's is deliberately private, and reaching into it to save four
/// lines would couple two route modules for no gain.
async fn card_by_key(db: &Db, key: &str) -> AppResult<Card> {
    crate::domain::card::find_by_key(db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)
}

/// Loads a tag by id, or 404s.
async fn tag_by_id(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Tag> {
    tag::find_tx(&mut *tx, id).await?.ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// The body of `POST /projects/{key}/tags`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTagRequest {
    /// The label. Must not contain spaces.
    #[schema(example = "good-first-issue")]
    pub name: String,
    /// An ADS accent name. Omit for the neutral chip.
    #[serde(default)]
    pub colour: Option<TagColour>,
}

/// The body of `PATCH /tags/{id}`.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTagRequest {
    /// The new label. Must not contain spaces.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it back to the neutral chip, a value sets
    /// it.
    #[serde(default, deserialize_with = "double_option")]
    pub colour: Option<Option<TagColour>>,
}

/// The body of `POST /cards/{key}/tags`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachTagRequest {
    /// The tag to put on the card. Must be the card's project's, or global.
    pub tag_id: String,
}

/// The body of `POST /tags/{id}/merge`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MergeTagRequest {
    /// The tag that survives. Every card carrying the merged tag ends up
    /// carrying this one.
    pub into_tag_id: String,
}

/// What a merge did.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeTagResponse {
    /// The surviving tag.
    pub tag: Tag,
    /// How many cards gained it. Cards that already carried both are not
    /// counted — nothing changed for them.
    pub relinked_cards: u64,
}

// ---------------------------------------------------------------------------
// Project-scoped
// ---------------------------------------------------------------------------

/// Every tag this project can offer, with usage counts.
///
/// Includes global tags (`projectId: null`), because from a project's point of
/// view a global tag is simply one more tag it can use — and the picker would
/// have to concatenate two lists in exactly the same order if this returned one.
///
/// `usageCount` is scoped to **this** project's live cards. A global tag has a
/// different count in every project, and a soft-deleted card is not one you can
/// navigate to.
#[utoipa::path(
    get,
    path = "/projects/{key}/tags",
    tag = "tags",
    params(("key" = String, Path, description = "The project key, e.g. ATLAS")),
    responses(
        (status = 200, description = "The project's tags and every global one, by name", body = Vec<TagUsage>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_tags(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<TagUsage>>> {
    let project = projects::by_key(&state.db, &key).await?;
    Ok(Json(tag::list_for_project(&state.db, &project.id).await?))
}

/// Creates a tag in a project.
///
/// This is the create-on-the-fly path: the picker calls it when what someone
/// typed matches nothing, so it has to be cheap and it has to say clearly why it
/// refused. A tag is not an admin action — that is most of the point of Phase 4 —
/// so it needs Member, like a comment, not Admin, like a status.
#[utoipa::path(
    post,
    path = "/projects/{key}/tags",
    tag = "tags",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateTagRequest,
    responses(
        (status = 201, description = "Created", body = Tag),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot create tags", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken, here or globally", body = Problem),
        (status = 422, description = "The name is invalid — a space, most likely", body = Problem),
    )
)]
async fn create_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateTagRequest>,
) -> AppResult<(StatusCode, Json<Tag>)> {
    let name = tag::validate_name(&body.name)?;
    let project = projects::by_key(&state.db, &key).await?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    // Checked inside the transaction so the check and the insert cannot be
    // separated by another writer. The UNIQUE index is the real guarantee; this
    // turns its 500 into a 409 that says what to fix.
    if tag::name_taken_tx(&mut tx, Some(&project.id), &name, None).await? {
        return Err(AppError::Conflict(format!(
            "The tag {name:?} already exists in {}.",
            project.key
        )));
    }

    // A global tag of the same name is also a collision, but a different one:
    // the project tag would be legal (the UNIQUE index scopes by project), and
    // the result would be two identical-looking chips in one picker with no way
    // to tell them apart. Refused for the user's sake rather than the index's.
    if tag::name_taken_tx(&mut tx, None, &name, None).await? {
        return Err(AppError::Conflict(format!(
            "{name:?} is already a global tag, usable from every project."
        )));
    }

    let created = tag::insert(&mut tx, Some(&project.id), &name, body.colour, now).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

// ---------------------------------------------------------------------------
// Tag-scoped
// ---------------------------------------------------------------------------

/// Renames and/or recolours a tag.
///
/// Renaming cannot orphan a card: `card_tags` references the tag's **id**, and
/// the id does not change. See [`crate::domain::tag::update`].
#[utoipa::path(
    patch,
    path = "/tags/{id}",
    tag = "tags",
    params(("id" = String, Path, description = "The tag's id")),
    request_body = UpdateTagRequest,
    responses(
        (status = 200, description = "Updated. Every card carrying it still carries it", body = Tag),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot edit tags", body = Problem),
        (status = 404, description = "No such tag", body = Problem),
        (status = 409, description = "The new name is taken", body = Problem),
        (status = 422, description = "The name is invalid, or nothing was sent", body = Problem),
    )
)]
async fn update_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateTagRequest>,
) -> AppResult<Json<Tag>> {
    let patch = TagPatch {
        name: body.name.as_deref().map(tag::validate_name).transpose()?,
        colour: body.colour,
    };

    if patch.is_empty() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    let mut tx = state.db.begin_write().await?;
    let target = tag_by_id(&mut tx, &id).await?;

    if let Some(name) = &patch.name {
        // `except` is the tag itself, so recasing `bug` to `Bug` is a rename and
        // not a collision with the row being renamed.
        if tag::name_taken_tx(&mut tx, target.project_id.as_deref(), name, Some(&id)).await? {
            return Err(AppError::Conflict(format!(
                "The tag {name:?} already exists. Merge the two instead of renaming."
            )));
        }
    }

    let updated = tag::update(&mut tx, &id, &patch).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// Deletes a tag, taking it off every card that carried it.
///
/// A hard delete, and deliberately so: a tag carries no history and nothing
/// references it by name, so there is nothing to preserve and nothing to break.
/// Restoring one is retyping seven characters.
#[utoipa::path(
    delete,
    path = "/tags/{id}",
    tag = "tags",
    params(("id" = String, Path, description = "The tag's id")),
    responses(
        (status = 204, description = "Deleted, and removed from every card"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot delete tags", body = Problem),
        (status = 404, description = "No such tag", body = Problem),
    )
)]
async fn delete_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    tag::delete(&mut tx, &id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Merges one tag into another.
///
/// Every card carrying `{id}` ends up carrying `intoTagId`, and `{id}` stops
/// existing. Cards carrying both already are left alone rather than duplicated —
/// which is the case the whole operation exists for. See
/// [`crate::domain::tag::merge`].
#[utoipa::path(
    post,
    path = "/tags/{id}/merge",
    tag = "tags",
    params(("id" = String, Path, description = "The tag to merge away. It will not exist afterwards")),
    request_body = MergeTagRequest,
    responses(
        (status = 200, description = "Merged. Every card that had the source now has the target", body = MergeTagResponse),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot merge tags", body = Problem),
        (status = 404, description = "No such tag", body = Problem),
        (status = 422, description = "The tags are the same one, or are in different scopes", body = Problem),
    )
)]
async fn merge_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<MergeTagRequest>,
) -> AppResult<Json<MergeTagResponse>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let relinked_cards = tag::merge(&mut tx, &id, &body.into_tag_id, now).await?;
    let survivor = tag_by_id(&mut tx, &body.into_tag_id).await?;

    tx.commit().await?;

    Ok(Json(MergeTagResponse {
        tag: survivor,
        relinked_cards,
    }))
}

// ---------------------------------------------------------------------------
// Card-scoped
// ---------------------------------------------------------------------------

/// The tags on a card.
#[utoipa::path(
    get,
    path = "/cards/{key}/tags",
    tag = "tags",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 200, description = "The card's tags, by name", body = Vec<Tag>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_card_tags(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Tag>>> {
    let card = card_by_key(&state.db, &key).await?;
    Ok(Json(tag::list_for_card(&state.db, &card.id).await?))
}

/// Puts a tag on a card.
///
/// Idempotent: tagging a card that already has the tag is a 200, not a 409. The
/// caller's intent is "this card has `bug`", and it does.
///
/// Returns the card's tags rather than the one attached, so the client re-renders
/// from one authoritative answer instead of splicing a chip into a list it hopes
/// is still current.
#[utoipa::path(
    post,
    path = "/cards/{key}/tags",
    tag = "tags",
    params(("key" = String, Path, description = "The card key")),
    request_body = AttachTagRequest,
    responses(
        (status = 200, description = "The card's tags, by name", body = Vec<Tag>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot tag cards", body = Problem),
        (status = 404, description = "No such card, or no such tag", body = Problem),
        (status = 422, description = "The tag belongs to a different project", body = Problem),
    )
)]
async fn attach_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<AttachTagRequest>,
) -> AppResult<Json<Vec<Tag>>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let card = crate::domain::card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let target = tag_by_id(&mut tx, &body.tag_id).await?;

    // The rule migration 0004 cannot express — see its closing note. The foreign
    // key says "a tag", not "a tag this card is allowed to have", so without
    // this a card in ATLAS could be given a tag belonging to a project its owner
    // has never heard of.
    if !tag::usable_from(&target, &card.project_id) {
        return Err(AppError::Validation(format!(
            "The tag {:?} belongs to another project. A card can only carry its own \
             project's tags, or a global one.",
            target.name
        )));
    }

    tag::attach(&mut tx, &card.id, &target.id, now).await?;
    let tags = tag::list_for_card_tx(&mut tx, &card.id).await?;

    tx.commit().await?;

    Ok(Json(tags))
}

/// Takes a tag off a card.
///
/// Idempotent in the same direction as [`attach_tag`]: a 204 whether or not the
/// card had the tag, because "this card does not have `bug`" is true either way.
/// A missing *card* is still a 404 — that is a different question, and answering
/// it with a cheerful 204 would hide a client bug.
#[utoipa::path(
    delete,
    path = "/cards/{key}/tags/{tagId}",
    tag = "tags",
    params(
        ("key" = String, Path, description = "The card key"),
        ("tagId" = String, Path, description = "The tag's id"),
    ),
    responses(
        (status = 204, description = "The card does not carry the tag"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot untag cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn detach_tag(
    State(state): State<AppState>,
    _member: RequireMember,
    Path((key, tag_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;

    let card = crate::domain::card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    tag::detach(&mut tx, &card.id, &tag_id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The tag routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{key}`, never `:key` — the 0.7 syntax is a runtime panic.
        .routes(routes!(list_tags, create_tag))
        .routes(routes!(update_tag, delete_tag))
        .routes(routes!(merge_tag))
        .routes(routes!(list_card_tags, attach_tag))
        .routes(routes!(detach_tag))
}

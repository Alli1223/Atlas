//! `/api/v1/cards` and `/api/v1/projects/{key}/cards`.

// `params(ListCardsQuery)` in `#[utoipa::path]` expands to a fully-qualified
// reference to the extractor's type, as a *sibling item* of the handler — but
// carrying the handler signature's span. So the workspace's
// `unused_qualifications` lint fires on a line nobody wrote, and an `#[allow]`
// on the handler cannot reach the generated item.
//
// The alternatives are worse: dropping `params(...)` would silence it by
// deleting the query parameters from the OpenAPI document (which is what
// generates the frontend's typed client), and restating every parameter
// longhand in the attribute would duplicate the struct below and drift from it.
// Scoped to this module rather than relaxed workspace-wide.
#![allow(unused_qualifications)]

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{API_V1_PREFIX, AppState, projects};
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::db::Db;
use crate::domain::card::{
    self, Card, CardDto, CardFilter, CardPatch, Drop, KeyLookup, NewCard, ParentFilter, Placement,
};
use crate::domain::history::{self, HistoryEntry};
use crate::domain::member::{self, ProjectRole};
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// The `parentId` value that means "only the top level".
///
/// A sentinel rather than a separate parameter because `?parentId=none` and
/// `?parentId=<id>` are the same question — which slice of the tree — and card
/// ids are UUIDs, so no card can ever be called `none`.
const ROOT_SENTINEL: &str = "none";

/// Loads a card by key, or 404s. Case-insensitive on the key.
async fn by_key(db: &Db, key: &str) -> AppResult<Card> {
    card::find_by_key(db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)
}

/// Query parameters for `GET /projects/{key}/cards`.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListCardsQuery {
    /// A card id to list the children of — **this is the nested board** — or
    /// `none` for the project's top level. Omit for every card at any depth.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Only cards in this status.
    #[serde(default)]
    pub status_id: Option<String>,
    /// Only cards assigned to this user.
    #[serde(default)]
    pub assignee_id: Option<String>,
    /// Whether archived cards are included. The trash never is.
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Page size. Capped at 200; defaults to 50.
    #[serde(default)]
    pub limit: Option<i64>,
    /// How many to skip.
    #[serde(default)]
    pub offset: Option<i64>,
}

/// A page of cards.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardPageDto {
    /// The cards on this page, in rank order.
    pub cards: Vec<CardDto>,
    /// How many cards match the filter in total, ignoring the page.
    pub total: i64,
    /// The page size actually applied, after clamping.
    pub limit: i64,
    /// The offset actually applied.
    pub offset: i64,
}

/// The body of `POST /projects/{key}/cards`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCardRequest {
    /// The card type. Must belong to the project; fixes the card's level.
    pub type_id: String,
    /// The parent, if this card is being created inside another card's board.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// The one-line title.
    pub summary: String,
    /// Markdown source.
    #[serde(default)]
    pub description: Option<String>,
    /// The status. Defaults to the project's first column.
    #[serde(default)]
    pub status_id: Option<String>,
    /// The priority.
    #[serde(default)]
    pub priority_id: Option<String>,
    /// Who is doing it.
    #[serde(default)]
    pub assignee_id: Option<String>,
    /// Who asked for it. Defaults to the creator.
    #[serde(default)]
    pub reporter_id: Option<String>,
    /// The due date, `YYYY-MM-DD`.
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
    /// The start date, `YYYY-MM-DD`.
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    /// The estimate, in the project's estimation unit.
    #[serde(default)]
    pub estimate: Option<f64>,
    /// Whether the card lands at the top of its column rather than the bottom.
    #[serde(default)]
    pub top: Option<bool>,
}

/// The body of `PATCH /cards/{key}`.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCardRequest {
    /// The one-line title.
    #[serde(default)]
    pub summary: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// The card type. Re-checked against the hierarchy, since type fixes level.
    #[serde(default)]
    pub type_id: Option<String>,
    /// The status. Drives the resolution rules.
    #[serde(default)]
    pub status_id: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub priority_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub assignee_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub reporter_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    ///
    /// Overridden when the same request moves the card out of a done status:
    /// "reopened but still resolved" is not a state anyone means.
    #[serde(default, deserialize_with = "double_option")]
    pub resolution_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub due_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub estimate: Option<Option<f64>>,
    /// Archive or unarchive.
    #[serde(default)]
    pub archived: Option<bool>,
    /// Move the card — **and everything under it** — to another project.
    ///
    /// This is a large operation wearing a field edit's clothes, and it is here
    /// rather than on its own endpoint because "change this card's project" is
    /// what a client is actually saying. It mints a new key for every card in
    /// the subtree, leaves a permanent redirect behind for each old key, and
    /// remaps type, status, priority and resolution to the target project's
    /// equivalents. Applied *before* any other field in the same request, so a
    /// `statusId` sent alongside it is resolved against the new project.
    #[serde(default)]
    pub project_key: Option<String>,
}

/// The body of `POST /cards/{key}/move`.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveCardRequest {
    /// The target column. Omit to reorder within the current one.
    #[serde(default)]
    pub status_id: Option<String>,
    /// The card immediately **above** the drop point. Omit for the top.
    #[serde(default)]
    pub previous_card_id: Option<String>,
    /// The card immediately **below** the drop point. Omit for the bottom.
    #[serde(default)]
    pub next_card_id: Option<String>,
}

/// The body of `POST /cards/{key}/reparent`.
#[allow(clippy::option_option)]
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReparentRequest {
    /// The new parent's card id, or `null` to move the card to the top level.
    ///
    /// Required — `double_option` is what lets an absent field be told apart
    /// from an explicit `null` here, and the two mean very different things: one
    /// is a client bug, the other is "make this a root".
    #[serde(default, deserialize_with = "double_option")]
    pub parent_id: Option<Option<String>>,
}

/// A page of a project's cards.
#[utoipa::path(
    get,
    path = "/projects/{key}/cards",
    tag = "cards",
    params(("key" = String, Path, description = "The project key"), ListCardsQuery),
    responses(
        (status = 200, description = "A page of cards, in rank order", body = CardPageDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_cards(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(query): Query<ListCardsQuery>,
) -> AppResult<Json<CardPageDto>> {
    let project = projects::by_key(&state.db, &key).await?;

    let parent = match query.parent_id.as_deref() {
        None => ParentFilter::Any,
        Some(ROOT_SENTINEL) => ParentFilter::Root,
        Some(id) => ParentFilter::Card(id.to_owned()),
    };

    let filter = CardFilter {
        parent,
        status_id: query.status_id,
        assignee_id: query.assignee_id,
        include_archived: query.include_archived.unwrap_or(false),
        limit: query.limit.unwrap_or(card::DEFAULT_PAGE_SIZE),
        offset: query.offset.unwrap_or(0),
    };

    let page = card::list(&state.db, &project.id, &filter).await?;

    Ok(Json(CardPageDto {
        cards: page.cards.iter().map(CardDto::from).collect(),
        total: page.total,
        // The clamped values, not the requested ones: a client that asked for
        // 10_000 needs to know it got 200, or its pagination silently stalls.
        limit: filter.limit.clamp(1, card::MAX_PAGE_SIZE),
        offset: filter.offset.max(0),
    }))
}

/// Creates a card.
#[utoipa::path(
    post,
    path = "/projects/{key}/cards",
    tag = "cards",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateCardRequest,
    responses(
        (status = 201, description = "Created, with a key that is never reused", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot create cards", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The parent is illegal, or the project has no statuses", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn create_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateCardRequest>,
) -> AppResult<(StatusCode, Json<CardDto>)> {
    let summary = card::validate_summary(&body.summary)?;
    let description = body
        .description
        .as_deref()
        .map(card::validate_description)
        .transpose()?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    let created = card::create(
        &mut tx,
        &project,
        &NewCard {
            type_id: body.type_id,
            parent_id: body.parent_id,
            summary,
            description,
            status_id: body.status_id,
            priority_id: body.priority_id,
            assignee_id: body.assignee_id,
            reporter_id: body.reporter_id,
            due_date: body.due_date,
            start_date: body.start_date,
            estimate: body.estimate,
            placement: if body.top.unwrap_or(false) {
                Placement::Top
            } else {
                Placement::Bottom
            },
        },
        member.0.id(),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(CardDto::from(&created))))
}

/// One card, following a retired key to wherever the card lives now.
///
/// # The redirect
///
/// A key that has been retired — the card moved projects and was renumbered —
/// answers **301** with a `Location` pointing at the card's current key, rather
/// than serving the card under its old name. Both halves of that matter:
///
/// - Serving it would mean the old key silently keeps working forever, so
///   nothing in the wild ever updates and the redirect table grows without ever
///   paying off.
/// - 404ing it would break every bookmark, commit message, branch name, PR title
///   and `ATLAS-42` autolink that ever mentioned the card — which is the entire
///   reason `card_key_history` exists.
#[utoipa::path(
    get,
    path = "/cards/{key}",
    tag = "cards",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-123")),
    responses(
        (status = 200, description = "The card", body = CardDto),
        (status = 301, description = "The key is retired; Location has the card's current key"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card, now or ever", body = Problem),
    )
)]
async fn get_card(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Response> {
    let lookup = card::resolve_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    match lookup {
        KeyLookup::Current(card) => Ok(Json(CardDto::from(&*card)).into_response()),
        KeyLookup::Moved(card) => {
            let location = format!("{API_V1_PREFIX}/cards/{}", card.key);
            Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .map_err(AppError::internal)
        }
    }
}

/// Edits a card.
///
/// Every field change is diffed and written to `card_history` in the same
/// transaction — see [`crate::domain::card::update`].
#[utoipa::path(
    patch,
    path = "/cards/{key}",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    request_body = UpdateCardRequest,
    responses(
        (status = 200, description = "Updated, with history rows for whatever moved", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot edit cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "The type change breaks the hierarchy, or the project has no resolutions", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn update_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<UpdateCardRequest>,
) -> AppResult<Json<CardDto>> {
    let patch = CardPatch {
        summary: body
            .summary
            .as_deref()
            .map(card::validate_summary)
            .transpose()?,
        description: body
            .description
            .map(|d| d.as_deref().map(card::validate_description).transpose())
            .transpose()?,
        type_id: body.type_id,
        status_id: body.status_id,
        priority_id: body.priority_id,
        assignee_id: body.assignee_id,
        reporter_id: body.reporter_id,
        resolution_id: body.resolution_id,
        due_date: body.due_date,
        start_date: body.start_date,
        estimate: body.estimate,
        rank: None,
        archived: body.archived,
    };

    if patch.is_empty() && body.project_key.is_none() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let mut target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    // The project move goes first, so any status or type in the same request is
    // resolved against the project the card is landing in rather than the one it
    // is leaving.
    if let Some(project_key) = &body.project_key {
        let destination = project::find_by_key_tx(&mut tx, project_key)
            .await?
            .ok_or_else(|| AppError::Validation(format!("No project with key {project_key:?}.")))?;

        // **The one access check in Atlas that is not in the layer**, and the
        // reason is structural rather than an oversight:
        // `crate::auth::project_access` decides on the project named by the
        // *path*, and this destination is named by the *body*. A layer that read
        // request bodies would have to buffer and re-deserialise every one of
        // them, guess at each route's schema, and stay in step with it forever.
        //
        // So it is here — and it is the whole of why this route is worth
        // attention: `PATCH /cards/{key}` with a `projectKey` is a write to a
        // project the caller never named in the URL. Without this, project
        // Member on any one project would be a licence to inject cards into
        // every other one.
        //
        // 404 for no access, 403 for not enough of it, exactly as
        // `member::require` does everywhere else — a caller who cannot see the
        // destination must not learn it exists by being told they may not write
        // to it.
        member::require(&state.db, &destination, &member.0.user, ProjectRole::Member).await?;

        target =
            card::move_to_project(&mut tx, &target, &destination, Some(member.0.id()), now).await?;
    }

    let updated = if patch.is_empty() {
        target
    } else {
        card::update(&mut tx, &target, &patch, Some(member.0.id()), now).await?
    };

    tx.commit().await?;

    Ok(Json(CardDto::from(&updated)))
}

/// Moves a card to the trash.
#[utoipa::path(
    delete,
    path = "/cards/{key}",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Moved to the trash; restorable, and its key stays burned", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot delete cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn delete_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
) -> AppResult<Json<CardDto>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let deleted = card::soft_delete(&mut tx, &target, Some(member.0.id()), now).await?;
    tx.commit().await?;

    Ok(Json(CardDto::from(&deleted)))
}

/// Brings a card back out of the trash.
#[utoipa::path(
    post,
    path = "/cards/{key}/restore",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Restored", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot restore cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn restore_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
) -> AppResult<Json<CardDto>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let restored = card::restore(&mut tx, &target, Some(member.0.id()), now).await?;
    tx.commit().await?;

    Ok(Json(CardDto::from(&restored)))
}

/// The drag-and-drop endpoint: move a card to a column and a place in it.
///
/// The drop point is named as the two cards it landed between, not as an index.
/// An index is stale the moment the client sends it; the neighbours are a
/// statement about what the user actually saw, and if they have moved the answer
/// is a 409 telling the client to refetch rather than a card silently landing
/// somewhere nobody dropped it.
#[utoipa::path(
    post,
    path = "/cards/{key}/move",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    request_body = MoveCardRequest,
    responses(
        (status = 200, description = "Moved; a drop into a done column also sets a resolution", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot move cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "The named neighbours have moved; refetch the board", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn move_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<MoveCardRequest>,
) -> AppResult<Json<CardDto>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let moved = card::move_card(
        &mut tx,
        &target,
        &Drop {
            status_id: body.status_id,
            previous_card_id: body.previous_card_id,
            next_card_id: body.next_card_id,
        },
        Some(member.0.id()),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok(Json(CardDto::from(&moved)))
}

/// Moves a card, and everything under it, to a new parent or to the top level.
///
/// Dragging a card onto another card is this endpoint. The four guards — same
/// project, `parent.level > child.level`, no cycles, and the depth cap — live in
/// [`crate::domain::hierarchy::check_reparent`] and each answers 409 with a
/// sentence saying which rule was hit.
#[utoipa::path(
    post,
    path = "/cards/{key}/reparent",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    request_body = ReparentRequest,
    responses(
        (status = 200, description = "Reparented", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot reparent cards", body = Problem),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "That would make a loop, break the level rule, or nest too deep", body = Problem),
        (status = 422, description = "parentId was not sent", body = Problem),
    )
)]
async fn reparent_card(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<ReparentRequest>,
) -> AppResult<Json<CardDto>> {
    // Absent and null are different requests, and only one of them is one.
    let parent_id = body.parent_id.ok_or_else(|| {
        AppError::Validation(
            "Send parentId: a card id to nest this card under, or null to move it to the top \
             level."
                .to_owned(),
        )
    })?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let reparented = card::reparent(
        &mut tx,
        &target,
        parent_id.as_deref(),
        Some(member.0.id()),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok(Json(CardDto::from(&reparented)))
}

/// A card's children. **This is the nested board.**
///
/// The same data a project's top-level board renders, scoped by `parent_id`
/// instead of by project — which is the whole of the nested-board feature, and
/// the reason ADR 0002 chose a uniform parent pointer.
#[utoipa::path(
    get,
    path = "/cards/{key}/children",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's children, in rank order", body = Vec<CardDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn card_children(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardDto>>> {
    let target = by_key(&state.db, &key).await?;
    let children = card::children(&state.db, &target.id).await?;
    Ok(Json(children.iter().map(CardDto::from).collect()))
}

/// A card's changelog, oldest first.
#[utoipa::path(
    get,
    path = "/cards/{key}/history",
    tag = "cards",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Every field change, with both raw and display values", body = Vec<HistoryEntry>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn card_history(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<HistoryEntry>>> {
    let target = by_key(&state.db, &key).await?;
    Ok(Json(history::list(&state.db, &target.id).await?))
}

/// The card routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_cards, create_card))
        .routes(routes!(get_card, update_card, delete_card))
        .routes(routes!(move_card))
        .routes(routes!(reparent_card))
        .routes(routes!(restore_card))
        .routes(routes!(card_children))
        .routes(routes!(card_history))
}

//! `/api/v1/search`, `/api/v1/search/validate`, and `/api/v1/filters` — the AQL
//! surface.
//!
//! The query language itself is [`crate::aql`]; this module is the HTTP shape
//! over it, exactly as [`crate::api::project_config`] is over
//! [`crate::domain::config`]. Two properties are load-bearing and enforced here:
//!
//! - **Results are scoped to the caller's accessible projects.** That scoping is
//!   compiled into every query by [`crate::aql`] (the accessible-projects
//!   predicate), so these handlers do not — and must not — re-implement it. A
//!   search cannot be a way to read cards in a project you cannot see.
//! - **Filters are personal.** Every filter route resolves the filter and checks
//!   `owner_id` against the caller, answering 404 (not 403) for someone else's,
//!   so the id space is not an existence oracle.

// See the note at the top of `api::cards`: the `params(...)` macro expands to a
// qualified path in a sibling item, so the lint is silenced at module scope.
#![allow(unused_qualifications)]

use axum::extract::{Path, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::api::serde_ext::double_option;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::card::{CardDto, MAX_PAGE_SIZE};
use crate::domain::filter::{self, Filter, FilterPatch};
use crate::error::{AppError, AppResult, Problem};

/// The largest `pageSize` a search will honour.
const MAX_SEARCH_PAGE_SIZE: i64 = MAX_PAGE_SIZE;

/// The default `pageSize`.
const DEFAULT_SEARCH_PAGE_SIZE: i64 = 50;

// ---------------------------------------------------------------------------
// Request / response bodies
// ---------------------------------------------------------------------------

/// The body of `POST /search`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchRequest {
    /// The AQL query. An empty string matches every card the caller can see.
    #[schema(example = "status = Done AND assignee = currentUser() ORDER BY updated DESC")]
    pub aql: String,
    /// The 1-based page number. Defaults to 1.
    #[serde(default)]
    pub page: Option<i64>,
    /// The page size, capped at 200. Defaults to 50.
    #[serde(default)]
    pub page_size: Option<i64>,
    /// An `ORDER BY` clause to append, e.g. `priority DESC`. A convenience for a
    /// UI that keeps sorting separate from the predicate; embedding `ORDER BY` in
    /// `aql` works too. Do not supply both.
    #[serde(default)]
    pub order_by: Option<String>,
}

/// A page of search results, with the normalised query echoed back.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// The cards on this page, in the query's order.
    pub cards: Vec<CardDto>,
    /// How many cards matched in total.
    pub total: i64,
    /// The 1-based page number returned.
    pub page: i64,
    /// The page size used.
    pub page_size: i64,
    /// The query, re-rendered canonically. This is what the basic⇄advanced
    /// editor round-trips against.
    pub query: String,
}

/// The body of `POST /search/validate`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateRequest {
    /// The AQL to check. Parsed and type-checked, never run.
    pub aql: String,
}

/// A parse/type error, with the span the frontend underlines.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidationError {
    /// The human-readable message, without the column.
    pub message: String,
    /// The byte offset the span starts at, or `null` for a whole-query problem.
    pub start: Option<usize>,
    /// One past the last byte of the span.
    pub end: Option<usize>,
    /// The 1-based character column, for a caret.
    pub column: Option<usize>,
}

/// The result of validating a query.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResponse {
    /// Whether the query is valid.
    pub ok: bool,
    /// The normalised query, present when `ok`.
    pub query: Option<String>,
    /// The error, present when not `ok`.
    pub error: Option<ValidationError>,
    /// Every field a query can name — an autocomplete hint for the editor.
    pub fields: Vec<String>,
    /// Every function the language offers.
    pub functions: Vec<String>,
}

/// The body of `POST /filters`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateFilterRequest {
    /// The display name, unique per owner.
    #[schema(example = "My open bugs")]
    pub name: String,
    /// An optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// The AQL. Checked for syntax and type errors before it is saved.
    pub aql: String,
}

/// The body of `PATCH /filters/{id}`.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFilterRequest {
    /// The new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// The new AQL. Re-checked before it is saved.
    #[serde(default)]
    pub aql: Option<String>,
}

/// Pagination query for `GET /filters/{id}/results`.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ResultsQuery {
    /// The 1-based page number.
    #[serde(default)]
    pub page: Option<i64>,
    /// The page size.
    #[serde(default)]
    pub page_size: Option<i64>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Turns a page number and size into a `(limit, offset)` pair.
fn window(page: Option<i64>, page_size: Option<i64>) -> (i64, i64, i64, i64) {
    let page = page.unwrap_or(1).max(1);
    let size = page_size
        .unwrap_or(DEFAULT_SEARCH_PAGE_SIZE)
        .clamp(1, MAX_SEARCH_PAGE_SIZE);
    let offset = (page - 1) * size;
    (size, offset, page, size)
}

/// Loads a filter the caller owns, or 404s.
///
/// The ownership check is the access control: a filter belongs to one person, so
/// someone else's is answered 404 rather than 403 — its existence is not the
/// caller's business.
async fn owned_filter(state: &AppState, current: &CurrentUser, id: &str) -> AppResult<Filter> {
    let filter = filter::find_by_id(&state.db, id)
        .await?
        .filter(|f| f.owner_id == current.id())
        .ok_or(AppError::NotFound)?;
    Ok(filter)
}

/// Composes a source string from a query and an optional appended `ORDER BY`.
fn compose(aql: &str, order_by: Option<&str>) -> String {
    match order_by {
        Some(order) if !order.trim().is_empty() => format!("{aql} ORDER BY {order}"),
        _ => aql.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Runs an AQL query and returns a page of matching cards.
///
/// The result set is always scoped to the projects the caller can access — the
/// scoping is compiled into the query by [`crate::aql`], not applied here.
#[utoipa::path(
    post,
    path = "/search",
    tag = "search",
    request_body = SearchRequest,
    responses(
        (status = 200, description = "A page of matching cards and the normalised query", body = SearchResponse),
        (status = 400, description = "The query does not parse or type-check", body = Problem),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn search(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<SearchRequest>,
) -> AppResult<Json<SearchResponse>> {
    let (limit, offset, page, page_size) = window(body.page, body.page_size);
    let source = compose(&body.aql, body.order_by.as_deref());

    let results =
        crate::aql::search(&state.db, &current.user, now(), &source, limit, offset).await?;

    Ok(Json(SearchResponse {
        cards: results.cards.iter().map(CardDto::from).collect(),
        total: results.total,
        page,
        page_size,
        query: results.normalized,
    }))
}

/// Parses and type-checks a query without running it.
///
/// Returns the error with its span so the editor can underline the offending
/// token, or the normalised query plus the field and function vocabulary as an
/// autocomplete hint. Never touches the database beyond the caller's own
/// identity — a filter reference is left unexpanded, so this is cheap.
#[utoipa::path(
    post,
    path = "/search/validate",
    tag = "search",
    request_body = ValidateRequest,
    responses(
        (status = 200, description = "Validation result: ok with a normalised query, or an error with a span", body = ValidateResponse),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn validate(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<ValidateRequest>,
) -> AppResult<Json<ValidateResponse>> {
    let ctx = crate::aql::context(&current.user, now(), 1, 0);
    let _ = &state;

    let response = match crate::aql::check(&body.aql, &ctx) {
        Ok(query) => ValidateResponse {
            ok: true,
            query: Some(crate::aql::normalize(&query)),
            error: None,
            fields: field_hints(),
            functions: function_hints(),
        },
        Err(err) => ValidateResponse {
            ok: false,
            query: None,
            error: Some(ValidationError {
                message: err.message.clone(),
                start: err.span.map(|s| s.start),
                end: err.span.map(|s| s.end),
                column: err.span.map(|s| s.column_in(&body.aql)),
            }),
            fields: field_hints(),
            functions: function_hints(),
        },
    };

    Ok(Json(response))
}

fn field_hints() -> Vec<String> {
    [
        "project",
        "type",
        "status",
        "statusCategory",
        "priority",
        "assignee",
        "reporter",
        "creator",
        "resolution",
        "parent",
        "created",
        "updated",
        "due",
        "resolved",
        "started",
        "summary",
        "description",
        "text",
        "labels",
        "key",
        "estimate",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn function_hints() -> Vec<String> {
    [
        "currentUser()",
        "now()",
        "startOfDay()",
        "startOfWeek()",
        "startOfMonth()",
        "startOfYear()",
        "endOfDay()",
        "endOfWeek()",
        "endOfMonth()",
        "endOfYear()",
        "membersOf()",
        "watchedCards()",
        "linkedCards()",
        "cardHistory()",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// Every filter the caller owns.
#[utoipa::path(
    get,
    path = "/filters",
    tag = "search",
    responses(
        (status = 200, description = "The caller's saved filters, by name", body = Vec<Filter>),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn list_filters(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<Json<Vec<Filter>>> {
    Ok(Json(filter::list_for_owner(&state.db, current.id()).await?))
}

/// Saves a new filter.
///
/// The AQL is parsed and type-checked before it is stored, so a saved filter is
/// always one that will compile when it is run or referenced — which is what lets
/// filter composition trust the bodies it inlines.
#[utoipa::path(
    post,
    path = "/filters",
    tag = "search",
    request_body = CreateFilterRequest,
    responses(
        (status = 201, description = "Created", body = Filter),
        (status = 400, description = "The AQL does not parse or type-check", body = Problem),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "A read-only account cannot save filters", body = Problem),
        (status = 409, description = "A filter of that name already exists", body = Problem),
        (status = 422, description = "The name, description or AQL is invalid", body = Problem),
    )
)]
async fn create_filter(
    State(state): State<AppState>,
    member: RequireMember,
    Json(body): Json<CreateFilterRequest>,
) -> AppResult<(StatusCode, Json<Filter>)> {
    let current = member.0;
    let name = filter::validate_name(&body.name)?;
    let description = body
        .description
        .as_deref()
        .map(filter::validate_description)
        .transpose()?;
    filter::validate_aql_length(&body.aql)?;

    // Type-check now so a broken filter can never be saved.
    let ctx = crate::aql::context(&current.user, now(), 1, 0);
    crate::aql::check(&body.aql, &ctx)
        .map_err(|err| AppError::BadRequest(err.render(&body.aql)))?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    if filter::name_taken(&mut tx, current.id(), &name, None).await? {
        return Err(AppError::Conflict(format!(
            "You already have a filter called {name:?}."
        )));
    }

    let created = filter::insert(
        &mut tx,
        current.id(),
        &name,
        description.as_deref(),
        &body.aql,
        now,
    )
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// One filter the caller owns.
#[utoipa::path(
    get,
    path = "/filters/{id}",
    tag = "search",
    params(("id" = String, Path, description = "The filter's id")),
    responses(
        (status = 200, description = "The filter", body = Filter),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such filter of yours", body = Problem),
    )
)]
async fn get_filter(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Filter>> {
    Ok(Json(owned_filter(&state, &current, &id).await?))
}

/// Edits a filter.
#[utoipa::path(
    patch,
    path = "/filters/{id}",
    tag = "search",
    params(("id" = String, Path, description = "The filter's id")),
    request_body = UpdateFilterRequest,
    responses(
        (status = 200, description = "Updated", body = Filter),
        (status = 400, description = "The new AQL does not parse or type-check", body = Problem),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "A read-only account cannot edit filters", body = Problem),
        (status = 404, description = "No such filter of yours", body = Problem),
        (status = 409, description = "A filter of that name already exists", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn update_filter(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateFilterRequest>,
) -> AppResult<Json<Filter>> {
    let current = member.0;

    let patch = FilterPatch {
        name: body
            .name
            .as_deref()
            .map(filter::validate_name)
            .transpose()?,
        description: body
            .description
            .map(|d| d.as_deref().map(filter::validate_description).transpose())
            .transpose()?,
        aql: body.aql.clone(),
    };

    if patch.is_empty() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    if let Some(aql) = &patch.aql {
        filter::validate_aql_length(aql)?;
        let ctx = crate::aql::context(&current.user, now(), 1, 0);
        crate::aql::check(aql, &ctx).map_err(|err| AppError::BadRequest(err.render(aql)))?;
    }

    let now = now();
    let mut tx = state.db.begin_write().await?;

    // Ownership check inside the write transaction, the same shape the tag and
    // project routes use: load, verify, then mutate.
    let target = filter::find_by_id_tx(&mut tx, &id)
        .await?
        .filter(|f| f.owner_id == current.id())
        .ok_or(AppError::NotFound)?;

    if let Some(name) = &patch.name
        && filter::name_taken(&mut tx, current.id(), name, Some(&target.id)).await?
    {
        return Err(AppError::Conflict(format!(
            "You already have a filter called {name:?}."
        )));
    }

    let updated = filter::apply_patch(&mut tx, &target.id, &patch, now).await?;
    tx.commit().await?;

    Ok(Json(updated))
}

/// Deletes a filter.
#[utoipa::path(
    delete,
    path = "/filters/{id}",
    tag = "search",
    params(("id" = String, Path, description = "The filter's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "A read-only account cannot delete filters", body = Problem),
        (status = 404, description = "No such filter of yours", body = Problem),
    )
)]
async fn delete_filter(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let current = member.0;
    let mut tx = state.db.begin_write().await?;

    let target = filter::find_by_id_tx(&mut tx, &id)
        .await?
        .filter(|f| f.owner_id == current.id())
        .ok_or(AppError::NotFound)?;

    filter::delete(&mut tx, &target.id).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Runs a saved filter and returns a page of results.
///
/// The same accessibility scoping as `POST /search`: even the filter's owner
/// sees only cards in projects they can access.
#[utoipa::path(
    get,
    path = "/filters/{id}/results",
    tag = "search",
    params(
        ("id" = String, Path, description = "The filter's id"),
        ResultsQuery,
    ),
    responses(
        (status = 200, description = "A page of the filter's results", body = SearchResponse),
        (status = 400, description = "The filter's AQL no longer compiles", body = Problem),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such filter of yours", body = Problem),
    )
)]
async fn filter_results(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    AxumQuery(query): AxumQuery<ResultsQuery>,
) -> AppResult<Json<SearchResponse>> {
    let target = owned_filter(&state, &current, &id).await?;
    let (limit, offset, page, page_size) = window(query.page, query.page_size);

    let results =
        crate::aql::search(&state.db, &current.user, now(), &target.aql, limit, offset).await?;

    Ok(Json(SearchResponse {
        cards: results.cards.iter().map(CardDto::from).collect(),
        total: results.total,
        page,
        page_size,
        query: results.normalized,
    }))
}

/// The search and filter routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{id}`, never `:id`.
        .routes(routes!(search))
        .routes(routes!(validate))
        .routes(routes!(list_filters, create_filter))
        .routes(routes!(get_filter, update_filter, delete_filter))
        .routes(routes!(filter_results))
}

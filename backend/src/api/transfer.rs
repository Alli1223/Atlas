//! Moving work in bulk (`TODO.md` Phase 18): bulk operations with preview, CSV import and
//! export, the project bundle (JSON export / import / duplicate), card templates, and the
//! trash.
//!
//! Every endpoint that names cards one by one (bulk) or by query (CSV export) checks access
//! per card, the same way a direct request would; the rest are project-scoped through
//! [`crate::auth::project_access::SCOPES`].

// See the note at the top of `api::cards`: `params(<IntoParams type>)` expands to a qualified
// path carrying the handler signature's span, so the lint is silenced at module scope.
#![allow(unused_qualifications)]

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::activity::HubMessage;
use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::bulk::{self, Operation, Report};
use crate::domain::bundle::{self, Bundle};
use crate::domain::card::{self, CardDto};
use crate::domain::card_template::{self, CardTemplate, TemplateInput};
use crate::domain::csv_import::{self, Imported, Plan, Target};
use crate::domain::member::{self, ProjectRole};
use crate::domain::project::{self, ProjectDto};
use crate::error::{AppError, AppResult, Problem};

/// The largest body an import endpoint accepts (a 5 MB CSV grows when JSON-escaped).
const IMPORT_BODY_LIMIT: usize = 12 * 1024 * 1024;

/// The most cards one CSV export writes.
const MAX_EXPORT: i64 = 10_000;

/// The body of `POST /bulk`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkRequest {
    /// Card keys, at most 1000.
    pub keys: Vec<String>,
    /// What to do to each.
    pub operation: Operation,
    /// `true` (the default) reports what would happen and saves nothing.
    #[serde(default = "yes")]
    pub preview: bool,
    /// An id the caller chooses; progress frames on the live channel carry it.
    #[serde(default)]
    pub request_id: Option<String>,
}

const fn yes() -> bool {
    true
}

/// The body of the CSV import endpoints.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CsvRequest {
    /// The file's text.
    pub csv: String,
    /// One target per column, overriding the suggested mapping. Absent = suggested.
    #[serde(default)]
    pub mapping: Option<Vec<Target>>,
}

/// `GET /export/csv` parameters.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ExportCsvParams {
    /// The AQL query whose results are exported.
    pub aql: String,
}

/// `GET /projects/{key}/export` parameters.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ExportBundleParams {
    /// `false` exports configuration only — a template. Default `true`.
    #[serde(default)]
    pub include_cards: Option<bool>,
}

/// The body of `POST /projects/import`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportBundleRequest {
    /// The bundle, as exported.
    pub bundle: Bundle,
    /// The new project's key.
    pub key: String,
    /// The new project's name. Absent = the bundle's.
    #[serde(default)]
    pub name: Option<String>,
    /// `false` imports the configuration only. Default `true`.
    #[serde(default = "yes")]
    pub include_cards: bool,
}

/// The body of `POST /projects/{key}/duplicate`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DuplicateRequest {
    /// The new project's key.
    pub key: String,
    /// The new project's name.
    pub name: String,
    /// `false` (the default) copies configuration only — "copy config from project".
    #[serde(default)]
    pub include_cards: bool,
}

/// Bulk-edits, transitions, deletes, restores, archives, moves, tags or untags cards.
///
/// Preview first (the default): every card goes through the real code inside a transaction
/// that is then rolled back, so the report is the domain's own answer. Apply runs each card
/// in its own transaction. Progress frames (`{"type":"bulkProgress"}`) go to the caller's
/// live channel every 50 cards.
#[utoipa::path(
    post,
    path = "/bulk",
    tag = "bulk",
    request_body = BulkRequest,
    responses(
        (status = 200, description = "What happened (or would) to each card", body = Report),
        (status = 422, description = "No cards, or more than 1000", body = Problem),
    )
)]
async fn run_bulk(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<BulkRequest>,
) -> AppResult<Json<Report>> {
    let hub = state.hub.clone();
    let user_id = current.user.id.clone();
    let request_id = body.request_id.clone();
    let preview = body.preview;
    let report = bulk::run(
        &state.db,
        &current.user,
        &body.keys,
        &body.operation,
        preview,
        now(),
        |done, total| {
            hub.publish(HubMessage {
                project_id: None,
                user_id: Some(user_id.clone()),
                payload: json!({
                    "type": "bulkProgress",
                    "requestId": request_id,
                    "preview": preview,
                    "done": done,
                    "total": total,
                }),
            });
        },
    )
    .await?;
    Ok(Json(report))
}

/// Parses a CSV and reports the mapping, the cards it would create, and every problem.
#[utoipa::path(
    post,
    path = "/projects/{key}/import/csv/preview",
    tag = "import-export",
    params(("key" = String, Path, description = "The project key")),
    request_body = CsvRequest,
    responses(
        (status = 200, description = "The plan; nothing is saved", body = Plan),
        (status = 422, description = "Not CSV, too big, or no Summary column", body = Problem),
    )
)]
async fn preview_csv(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Json(body): Json<CsvRequest>,
) -> AppResult<Json<Plan>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        csv_import::plan(&state.db, &project, &body.csv, body.mapping).await?,
    ))
}

/// Imports a CSV: every card or none. A plan with any problem is returned with a 422.
#[utoipa::path(
    post,
    path = "/projects/{key}/import/csv",
    tag = "import-export",
    params(("key" = String, Path, description = "The project key")),
    request_body = CsvRequest,
    responses(
        (status = 201, description = "Imported; the new keys, in file order", body = Imported),
        (status = 422, description = "The plan had problems; nothing was saved", body = Plan),
    )
)]
async fn import_csv(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    Json(body): Json<CsvRequest>,
) -> AppResult<Response> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    match csv_import::import(
        &state.db,
        &project,
        &body.csv,
        body.mapping,
        &current.user.id,
        now(),
    )
    .await?
    {
        Ok(imported) => Ok((StatusCode::CREATED, Json(imported)).into_response()),
        Err(plan) => Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(plan)).into_response()),
    }
}

/// Exports an AQL query's results as CSV, in the shape the importer reads.
///
/// Only cards the caller can see match, exactly as in `/search`.
#[utoipa::path(
    get,
    path = "/export/csv",
    tag = "import-export",
    params(ExportCsvParams),
    responses(
        (status = 200, description = "A CSV file", content_type = "text/csv", body = String),
        (status = 400, description = "The query does not parse", body = Problem),
    )
)]
async fn export_csv(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ExportCsvParams>,
) -> AppResult<Response> {
    let results =
        crate::aql::search(&state.db, &current.user, now(), &params.aql, MAX_EXPORT, 0).await?;
    let text = csv_import::export(&state.db, &results.cards).await?;
    let mut response = text.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"atlas-export.csv\""),
    );
    Ok(response)
}

/// Exports a project as one JSON bundle — configuration, and (by default) its cards.
#[utoipa::path(
    get,
    path = "/projects/{key}/export",
    tag = "import-export",
    params(("key" = String, Path, description = "The project key"), ExportBundleParams),
    responses(
        (status = 200, description = "The bundle", body = Bundle),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn export_bundle(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(params): Query<ExportBundleParams>,
) -> AppResult<Json<Bundle>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        bundle::export(
            &state.db,
            &project,
            params.include_cards.unwrap_or(true),
            now(),
        )
        .await?,
    ))
}

/// Creates a project from a bundle. The caller owns it.
#[utoipa::path(
    post,
    path = "/projects/import",
    tag = "import-export",
    request_body = ImportBundleRequest,
    responses(
        (status = 201, description = "Created", body = ProjectDto),
        (status = 403, description = "Viewers cannot create projects", body = Problem),
        (status = 409, description = "The key is taken", body = Problem),
        (status = 422, description = "Not a valid bundle", body = Problem),
    )
)]
async fn import_bundle(
    State(state): State<AppState>,
    member: RequireMember,
    Json(body): Json<ImportBundleRequest>,
) -> AppResult<(StatusCode, Json<ProjectDto>)> {
    let name = body
        .name
        .clone()
        .unwrap_or_else(|| body.bundle.project.name.clone());
    let created = bundle::import(
        &state.db,
        &body.bundle,
        &body.key,
        &name,
        body.include_cards,
        &member.0.user,
        now(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ProjectDto::from(&created))))
}

/// Creates a new project from this one: its configuration (the default — "copy config from
/// project"), or a full copy with its cards.
#[utoipa::path(
    post,
    path = "/projects/{key}/duplicate",
    tag = "import-export",
    params(("key" = String, Path, description = "The project to copy")),
    request_body = DuplicateRequest,
    responses(
        (status = 201, description = "Created", body = ProjectDto),
        (status = 403, description = "Viewers cannot create projects", body = Problem),
        (status = 409, description = "The key is taken", body = Problem),
    )
)]
async fn duplicate_project(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<DuplicateRequest>,
) -> AppResult<(StatusCode, Json<ProjectDto>)> {
    let source = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let now = now();
    let exported = bundle::export(&state.db, &source, body.include_cards, now).await?;
    let created = bundle::import(
        &state.db,
        &exported,
        &body.key,
        &body.name,
        body.include_cards,
        &member.0.user,
        now,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ProjectDto::from(&created))))
}

/// A project's card templates.
#[utoipa::path(
    get,
    path = "/projects/{key}/card-templates",
    tag = "card-templates",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Templates, by name", body = Vec<CardTemplate>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_templates(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardTemplate>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(card_template::list(&state.db, &project.id).await?))
}

/// Creates a card template.
#[utoipa::path(
    post,
    path = "/projects/{key}/card-templates",
    tag = "card-templates",
    params(("key" = String, Path, description = "The project key")),
    request_body = TemplateInput,
    responses(
        (status = 201, description = "Created", body = CardTemplate),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "An invalid template", body = Problem),
    )
)]
async fn create_template(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Json(body): Json<TemplateInput>,
) -> AppResult<(StatusCode, Json<CardTemplate>)> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    card_template::validate(&mut tx, &project.id, &body).await?;
    let id = card_template::insert(&mut tx, &project.id, &body, now()).await?;
    tx.commit().await?;
    let created = card_template::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Replaces a card template.
#[utoipa::path(
    put,
    path = "/card-templates/{id}",
    tag = "card-templates",
    params(("id" = String, Path, description = "The template")),
    request_body = TemplateInput,
    responses(
        (status = 200, description = "Replaced", body = CardTemplate),
        (status = 404, description = "No such template", body = Problem),
    )
)]
async fn replace_template(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<TemplateInput>,
) -> AppResult<Json<CardTemplate>> {
    let existing = card_template::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    card_template::validate(&mut tx, &existing.project_id, &body).await?;
    card_template::update(&mut tx, &id, &body, now()).await?;
    tx.commit().await?;
    Ok(Json(
        card_template::find(&state.db, &id)
            .await?
            .ok_or(AppError::NotFound)?,
    ))
}

/// Deletes a card template.
#[utoipa::path(
    delete,
    path = "/card-templates/{id}",
    tag = "card-templates",
    params(("id" = String, Path, description = "The template")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such template", body = Problem),
    )
)]
async fn delete_template(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    card_template::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    card_template::delete(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A project's trash: soft-deleted cards, most recently deleted first. Restore one with
/// `POST /cards/{key}/restore`, or many with a bulk `restore`.
#[utoipa::path(
    get,
    path = "/projects/{key}/trash",
    tag = "cards",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Deleted cards", body = Vec<CardDto>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_trash(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardDto>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    // The gate already demanded Viewer; restoring needs Member, which the restore checks.
    member::require(&state.db, &project, &current.user, ProjectRole::Viewer).await?;
    Ok(Json(
        card::trash(&state.db, &project.id)
            .await?
            .iter()
            .map(CardDto::from)
            .collect(),
    ))
}

/// The routes.
pub fn routes() -> OpenApiRouter<AppState> {
    let imports = OpenApiRouter::new()
        .routes(routes!(preview_csv))
        .routes(routes!(import_csv))
        .routes(routes!(import_bundle))
        .layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT));
    OpenApiRouter::new()
        .routes(routes!(run_bulk))
        .routes(routes!(export_csv))
        .routes(routes!(export_bundle))
        .routes(routes!(duplicate_project))
        .routes(routes!(list_templates, create_template))
        .routes(routes!(replace_template, delete_template))
        .routes(routes!(list_trash))
        .merge(imports)
}

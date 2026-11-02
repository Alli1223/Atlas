//! Reports (`/projects/{key}/reports/*`, `/cycles/{id}/burndown|report`, `/reports/*`) and
//! dashboards (`/dashboards`).
//!
//! Project reports are Viewer-scoped by the gate. The AQL-driven ones (`aggregate`,
//! `two-dimensional`) are not project-scoped at the route: the query is compiled with the
//! caller's access, exactly as `/search` is, so a gadget can never count a card its viewer
//! cannot see. Dashboards are personal (owner-checked in the handler, like saved filters);
//! a shared dashboard shows its layout to everyone and its data with each viewer's access.

#![allow(unused_qualifications)] // utoipa's params(IntoParams) expansion; see api::cards

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::api::AppState;
use crate::auth::{CurrentUser, now, to_sql_timestamp};
use crate::domain::project::{self, Project};
use crate::error::{AppError, AppResult, Problem};
use crate::reports::{
    self, AgentCosts, Aggregate, Burndown, ControlChart, CycleReport, DailyChart, Metric,
    TwoDimensional, Velocity,
};

/// `?days=` for the day-indexed reports.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DaysQuery {
    /// How many days back (default 30, at most 730).
    #[serde(default)]
    pub days: Option<i64>,
}

/// The body of `POST /reports/aggregate`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregateRequest {
    /// Which cards. Empty = everything the caller can see.
    #[serde(default)]
    pub aql: String,
    /// Group by one of the groupable fields; absent = one total.
    #[serde(default)]
    pub group_by: Option<String>,
    /// Count cards, or sum their estimates.
    #[serde(default)]
    pub metric: Metric,
}

/// The body of `POST /reports/two-dimensional`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TwoDimensionalRequest {
    /// Which cards.
    #[serde(default)]
    pub aql: String,
    /// The column field.
    pub x: String,
    /// The row field.
    pub y: String,
}

async fn load_project(state: &AppState, key: &str) -> AppResult<Project> {
    project::find_by_key(&state.db, key)
        .await?
        .ok_or(AppError::NotFound)
}

async fn cycle_project(state: &AppState, cycle_id: &str) -> AppResult<Project> {
    let project_id: String = sqlx::query_scalar("SELECT project_id FROM cycles WHERE id = ?")
        .bind(cycle_id)
        .fetch_optional(state.db.reader())
        .await?
        .ok_or(AppError::NotFound)?;
    project::find_by_id(&state.db, &project_id)
        .await?
        .ok_or(AppError::NotFound)
}

/// Cumulative flow: cards per status per day.
#[utoipa::path(get, path = "/projects/{key}/reports/cumulative-flow", tag = "reports",
    params(("key" = String, Path, description = "The project key"), DaysQuery),
    responses((status = 200, description = "Daily counts per status", body = DailyChart),
              (status = 404, description = "No such project", body = Problem)))]
async fn cumulative_flow(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<DaysQuery>,
) -> AppResult<Json<DailyChart>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        reports::cumulative_flow(&state.db, &project, now(), q.days.unwrap_or(30)).await?,
    ))
}

/// Created vs resolved per day, with running totals.
#[utoipa::path(get, path = "/projects/{key}/reports/created-vs-resolved", tag = "reports",
    params(("key" = String, Path, description = "The project key"), DaysQuery),
    responses((status = 200, description = "Daily created/resolved", body = DailyChart)))]
async fn created_vs_resolved(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<DaysQuery>,
) -> AppResult<Json<DailyChart>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        reports::created_vs_resolved(&state.db, &project, now(), q.days.unwrap_or(30)).await?,
    ))
}

/// The average age of unresolved cards, per day.
#[utoipa::path(get, path = "/projects/{key}/reports/average-age", tag = "reports",
    params(("key" = String, Path, description = "The project key"), DaysQuery),
    responses((status = 200, description = "Average age in days", body = DailyChart)))]
async fn average_age(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<DaysQuery>,
) -> AppResult<Json<DailyChart>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        reports::average_age(&state.db, &project, now(), q.days.unwrap_or(30)).await?,
    ))
}

/// The control chart: cycle time per resolved card.
#[utoipa::path(get, path = "/projects/{key}/reports/control-chart", tag = "reports",
    params(("key" = String, Path, description = "The project key"), DaysQuery),
    responses((status = 200, description = "Cycle times with a rolling average", body = ControlChart)))]
async fn control_chart(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<DaysQuery>,
) -> AppResult<Json<ControlChart>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(
        reports::control_chart(&state.db, &project, now(), q.days.unwrap_or(90)).await?,
    ))
}

/// Velocity over the last closed cycles.
#[utoipa::path(get, path = "/projects/{key}/reports/velocity", tag = "reports",
    params(("key" = String, Path, description = "The project key")),
    responses((status = 200, description = "Committed vs completed per cycle", body = Velocity),
              (status = 422, description = "Cycles are off for this project", body = Problem)))]
async fn velocity(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Velocity>> {
    let project = load_project(&state, &key).await?;
    Ok(Json(reports::velocity(&state.db, &project, 7).await?))
}

/// A cycle's burndown and burnup. Count-based when the project's estimation is `none`.
#[utoipa::path(get, path = "/cycles/{id}/burndown", tag = "reports",
    params(("id" = String, Path, description = "The cycle")),
    responses((status = 200, description = "Remaining, scope, completed, ideal", body = Burndown),
              (status = 422, description = "Cycles off, or not started", body = Problem)))]
async fn burndown(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Burndown>> {
    let project = cycle_project(&state, &id).await?;
    Ok(Json(
        reports::burndown(&state.db, &project, &id, now()).await?,
    ))
}

/// A cycle's report: committed vs added vs removed vs completed.
#[utoipa::path(get, path = "/cycles/{id}/report", tag = "reports",
    params(("id" = String, Path, description = "The cycle")),
    responses((status = 200, description = "The cycle report", body = CycleReport)))]
async fn cycle_report(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<CycleReport>> {
    let project = cycle_project(&state, &id).await?;
    Ok(Json(reports::cycle_report(&state.db, &project, &id).await?))
}

/// Groups the cards an AQL query finds — the pie, bar and stat gadgets.
#[utoipa::path(post, path = "/reports/aggregate", tag = "reports",
    request_body = AggregateRequest,
    responses((status = 200, description = "Buckets, largest first", body = Aggregate),
              (status = 400, description = "Bad AQL", body = Problem),
              (status = 422, description = "Not a groupable field", body = Problem)))]
async fn aggregate(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<AggregateRequest>,
) -> AppResult<Json<Aggregate>> {
    Ok(Json(
        reports::aggregate(
            &state.db,
            &current.user,
            now(),
            &body.aql,
            body.group_by.as_deref(),
            body.metric,
        )
        .await?,
    ))
}

/// Two-dimensional filter statistics.
#[utoipa::path(post, path = "/reports/two-dimensional", tag = "reports",
    request_body = TwoDimensionalRequest,
    responses((status = 200, description = "A count matrix", body = TwoDimensional),
              (status = 422, description = "Not a groupable field", body = Problem)))]
async fn two_dimensional(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<TwoDimensionalRequest>,
) -> AppResult<Json<TwoDimensional>> {
    Ok(Json(
        reports::two_dimensional(&state.db, &current.user, now(), &body.aql, &body.x, &body.y)
            .await?,
    ))
}

/// Claude and Gemini spend across the projects the caller can see.
#[utoipa::path(get, path = "/reports/agent-costs", tag = "reports",
    params(DaysQuery),
    responses((status = 200, description = "Spend by provider, project, card and day", body = AgentCosts)))]
async fn agent_costs(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(q): Query<DaysQuery>,
) -> AppResult<Json<AgentCosts>> {
    let visible: Vec<String> = project::list_for(&state.db, &current.user, true)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    Ok(Json(
        reports::agent_costs(&state.db, &visible, now(), q.days.unwrap_or(30)).await?,
    ))
}

// ---------------------------------------------------------------------------
// Dashboards
// ---------------------------------------------------------------------------

/// The gadget kinds — four renderers over AQL, plus the built-in reports.
pub const GADGET_KINDS: &[&str] = &[
    "table",
    "pie",
    "bar",
    "stat",
    "twoDimensional",
    "cumulativeFlow",
    "createdVsResolved",
    "averageAge",
    "controlChart",
    "burndown",
    "velocity",
    "agentCosts",
];

/// One gadget on a dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Gadget {
    /// Stable within the dashboard (the client generates it).
    pub id: String,
    /// One of [`GADGET_KINDS`].
    pub kind: String,
    /// Its heading.
    pub title: String,
    /// Kind-specific: `{ aql, groupBy, metric }`, `{ projectKey, days }`, `{ cycleId }`, ….
    #[schema(value_type = Object)]
    #[serde(default)]
    pub config: Value,
    /// Grid column (0–11).
    pub x: i64,
    /// Grid row.
    pub y: i64,
    /// Width in columns (1–12).
    pub w: i64,
    /// Height in rows (1–12).
    pub h: i64,
}

/// A dashboard.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    /// UUID v7.
    pub id: String,
    /// Who owns it.
    pub owner_id: String,
    /// Its name.
    pub name: String,
    /// Readable by every signed-in user.
    pub shared: bool,
    /// The gadgets.
    pub gadgets: Vec<Gadget>,
    /// Whether the caller may edit it.
    pub editable: bool,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// The body of a dashboard create or replace.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardRequest {
    /// Its name.
    pub name: String,
    /// Share it.
    #[serde(default)]
    pub shared: bool,
    /// The gadgets.
    #[serde(default)]
    pub gadgets: Vec<Gadget>,
}

#[derive(Debug, FromRow)]
struct DashboardRow {
    id: String,
    owner_id: String,
    name: String,
    shared: bool,
    gadgets: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn to_dashboard(row: DashboardRow, viewer: &str) -> AppResult<Dashboard> {
    Ok(Dashboard {
        gadgets: serde_json::from_str(&row.gadgets).map_err(AppError::internal)?,
        editable: row.owner_id == viewer,
        id: row.id,
        owner_id: row.owner_id,
        name: row.name,
        shared: row.shared,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn validate(body: &DashboardRequest) -> AppResult<String> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::Validation(
            "A dashboard name must be 1 to 100 characters.".to_owned(),
        ));
    }
    if body.gadgets.len() > 40 {
        return Err(AppError::Validation(
            "A dashboard may hold at most 40 gadgets.".to_owned(),
        ));
    }
    for gadget in &body.gadgets {
        if !GADGET_KINDS.contains(&gadget.kind.as_str()) {
            return Err(AppError::Validation(format!(
                "{:?} is not a gadget kind.",
                gadget.kind
            )));
        }
        if !(0..12).contains(&gadget.x)
            || !(1..=12).contains(&gadget.w)
            || !(1..=12).contains(&gadget.h)
            || gadget.y < 0
            || gadget.x + gadget.w > 12
        {
            return Err(AppError::Validation(format!(
                "Gadget {:?} does not fit the 12-column grid.",
                gadget.title
            )));
        }
        if gadget.title.chars().count() > 120 || gadget.config.to_string().len() > 8 * 1024 {
            return Err(AppError::Validation(
                "A gadget's title or config is too large.".to_owned(),
            ));
        }
    }
    Ok(name.to_owned())
}

async fn find_visible(state: &AppState, id: &str, viewer: &str) -> AppResult<Dashboard> {
    let row = sqlx::query_as::<_, DashboardRow>(
        "SELECT id, owner_id, name, shared, gadgets, created_at, updated_at FROM dashboards \
          WHERE id = ? AND (owner_id = ? OR shared = 1)",
    )
    .bind(id)
    .bind(viewer)
    .fetch_optional(state.db.reader())
    .await?
    .ok_or(AppError::NotFound)?;
    to_dashboard(row, viewer)
}

/// Your dashboards, and everyone's shared ones.
#[utoipa::path(get, path = "/dashboards", tag = "reports",
    responses((status = 200, description = "Dashboards, by name", body = Vec<Dashboard>)))]
async fn list_dashboards(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<Json<Vec<Dashboard>>> {
    let rows = sqlx::query_as::<_, DashboardRow>(
        "SELECT id, owner_id, name, shared, gadgets, created_at, updated_at FROM dashboards \
          WHERE owner_id = ? OR shared = 1 ORDER BY name",
    )
    .bind(current.id())
    .fetch_all(state.db.reader())
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| to_dashboard(r, current.id()))
            .collect::<AppResult<_>>()?,
    ))
}

/// Creates a dashboard.
#[utoipa::path(post, path = "/dashboards", tag = "reports",
    request_body = DashboardRequest,
    responses((status = 201, description = "Created", body = Dashboard),
              (status = 422, description = "Invalid layout", body = Problem)))]
async fn create_dashboard(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<DashboardRequest>,
) -> AppResult<(StatusCode, Json<Dashboard>)> {
    let name = validate(&body)?;
    let id = Uuid::now_v7().to_string();
    let at = to_sql_timestamp(now());
    let mut tx = state.db.begin_write().await?;
    sqlx::query("INSERT INTO dashboards (id, owner_id, name, shared, gadgets, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(current.id())
        .bind(&name)
        .bind(body.shared)
        .bind(serde_json::to_string(&body.gadgets).map_err(AppError::internal)?)
        .bind(&at)
        .bind(&at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(find_visible(&state, &id, current.id()).await?),
    ))
}

/// One dashboard (yours, or a shared one).
#[utoipa::path(get, path = "/dashboards/{id}", tag = "reports",
    params(("id" = String, Path, description = "The dashboard")),
    responses((status = 200, description = "The dashboard", body = Dashboard),
              (status = 404, description = "No such dashboard you can see", body = Problem)))]
async fn get_dashboard(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Dashboard>> {
    Ok(Json(find_visible(&state, &id, current.id()).await?))
}

/// Replaces a dashboard. Yours only.
#[utoipa::path(put, path = "/dashboards/{id}", tag = "reports",
    params(("id" = String, Path, description = "The dashboard")),
    request_body = DashboardRequest,
    responses((status = 200, description = "Saved", body = Dashboard),
              (status = 404, description = "No such dashboard of yours", body = Problem)))]
async fn replace_dashboard(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<DashboardRequest>,
) -> AppResult<Json<Dashboard>> {
    let name = validate(&body)?;
    let mut tx = state.db.begin_write().await?;
    let done = sqlx::query("UPDATE dashboards SET name = ?, shared = ?, gadgets = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(&name)
        .bind(body.shared)
        .bind(serde_json::to_string(&body.gadgets).map_err(AppError::internal)?)
        .bind(to_sql_timestamp(now()))
        .bind(&id)
        .bind(current.id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(find_visible(&state, &id, current.id()).await?))
}

/// Deletes a dashboard. Yours only.
#[utoipa::path(delete, path = "/dashboards/{id}", tag = "reports",
    params(("id" = String, Path, description = "The dashboard")),
    responses((status = 204, description = "Deleted"),
              (status = 404, description = "No such dashboard of yours", body = Problem)))]
async fn delete_dashboard(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let done = sqlx::query("DELETE FROM dashboards WHERE id = ? AND owner_id = ?")
        .bind(&id)
        .bind(current.id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The report and dashboard routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(cumulative_flow))
        .routes(routes!(created_vs_resolved))
        .routes(routes!(average_age))
        .routes(routes!(control_chart))
        .routes(routes!(velocity))
        .routes(routes!(burndown))
        .routes(routes!(cycle_report))
        .routes(routes!(aggregate))
        .routes(routes!(two_dimensional))
        .routes(routes!(agent_costs))
        .routes(routes!(list_dashboards, create_dashboard))
        .routes(routes!(get_dashboard, replace_dashboard, delete_dashboard))
}

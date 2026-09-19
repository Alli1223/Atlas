//! `/api/v1/cycles/{id}/burndown`.
//!
//! Read-only, Viewer-scoped like the cycle it hangs off — a report is not something only a
//! Member should see. See [`crate::domain::report`] for the computation.

use axum::extract::{Path, State};
use axum::response::Json;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::domain::cycle;
use crate::domain::project;
use crate::domain::report::{self, Burndown, BurndownMetric};
use crate::error::{AppError, AppResult, Problem};

/// A cycle's burndown: one point per day it has been snapshotted.
///
/// Count-based rather than estimate-based whenever the cycle's project has no estimation
/// field configured (`estimationUnit = "none"`) — `metric` in the response says which, so the
/// client renders the right axis label without having to ask the project separately.
#[utoipa::path(
    get,
    path = "/cycles/{id}/burndown",
    tag = "reports",
    params(("id" = String, Path, description = "The cycle id")),
    responses(
        (status = 200, description = "The cycle's burndown", body = Burndown),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such cycle", body = Problem),
    )
)]
async fn get_burndown(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Burndown>> {
    let cycle = cycle::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let project = project::find_by_id(&state.db, &cycle.project_id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("cycle's project is missing")))?;

    let metric = BurndownMetric::for_project(project.estimation_unit);
    let burndown = report::burndown_for_cycle(&state.db, &cycle.id, metric).await?;

    Ok(Json(burndown))
}

/// The report routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_burndown))
}

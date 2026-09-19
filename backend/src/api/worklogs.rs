//! `/api/v1/cards/{key}/worklogs` — the direct time-logging path.
//!
//! A smart commit's `#time 2h` directive
//! ([`crate::integrations::github::smart_commit`]) writes the same
//! `card_worklogs` rows through the same [`crate::domain::worklog`] functions;
//! this module is the other way a worklog gets created — a form, not a commit
//! message — and the only way any of them are read back.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::domain::card;
use crate::domain::worklog::{self, NewWorklog, Worklog};
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /cards/{key}/worklogs`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogTimeRequest {
    /// A duration string: whitespace-separated `2w`/`3d`/`4h`/`30m` tokens, e.g.
    /// `"2h 30m"`. Every token must be a duration — there is no trailing note
    /// to fall back to here, unlike a smart commit's `#time` directive.
    #[schema(example = "2h 30m")]
    pub duration: String,
    /// An optional note.
    #[serde(default)]
    pub note: Option<String>,
}

/// A card's worklogs plus their total, so the client never has to sum the list
/// itself (and can't get it wrong doing so).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorklogsResponse {
    /// Newest first.
    pub entries: Vec<Worklog>,
    /// The sum of every entry's minutes.
    pub total_minutes: i64,
}

/// Every worklog on a card, newest first, plus the running total.
#[utoipa::path(
    get,
    path = "/cards/{key}/worklogs",
    tag = "worklogs",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's worklogs", body = WorklogsResponse),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_worklogs(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<WorklogsResponse>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let entries = worklog::list_for_card(&state.db, &target.id).await?;
    let total_minutes = worklog::total_minutes_for_card(&state.db, &target.id).await?;

    Ok(Json(WorklogsResponse {
        entries,
        total_minutes,
    }))
}

/// Logs time against a card directly — the manual counterpart to a smart
/// commit's `#time` directive.
#[utoipa::path(
    post,
    path = "/cards/{key}/worklogs",
    tag = "worklogs",
    params(("key" = String, Path, description = "The card key")),
    request_body = LogTimeRequest,
    responses(
        (status = 201, description = "Logged", body = Worklog),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot log time", body = Problem),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "The duration is empty, malformed, or not positive", body = Problem),
    )
)]
async fn log_time(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<LogTimeRequest>,
) -> AppResult<(StatusCode, Json<Worklog>)> {
    let minutes = worklog::parse_duration(&body.duration).ok_or_else(|| {
        AppError::Validation(
            "the duration must be one or more w/d/h/m tokens, e.g. \"2h 30m\", summing to more \
             than zero minutes"
                .to_owned(),
        )
    })?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let logged = worklog::insert(
        &mut tx,
        &NewWorklog {
            card_id: &target.id,
            author_id: Some(member.0.id()),
            minutes,
            note: body.note.as_deref(),
            source: "manual",
        },
        now,
    )
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(logged)))
}

/// The worklog routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_worklogs, log_time))
}

//! `/notifications` — the caller's bell and preferences — and `/admin/smtp`, outbound email.
//!
//! Everything under `/notifications` acts on the caller's own rows only: there is no `{userId}`
//! anywhere, so there is nothing to forge. Marking someone else's notification read 404s.

// See the note at the top of `api::cards`: `params(<IntoParams type>)` expands to a qualified
// path carrying the handler signature's span, so the lint is silenced at module scope.
#![allow(unused_qualifications)]

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireAdmin;
use crate::auth::{CurrentUser, now};
use crate::error::{AppError, AppResult, Problem};
use crate::notify::mail::{self, SmtpSettings};
use crate::notify::{self, Notification, Preferences};

/// The query of `GET /notifications`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Only unread ones.
    #[serde(default)]
    pub unread: Option<bool>,
    /// At most this many (1–200, default 50).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// The bell: recent notifications and the unread count.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    /// Newest first.
    pub items: Vec<Notification>,
    /// How many are unread in total.
    pub unread: i64,
}

/// The body of `POST /admin/smtp/test`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestEmailRequest {
    /// Where to send the test. Defaults to the caller's own address.
    #[serde(default)]
    pub to: Option<String>,
}

/// The caller's notifications.
#[utoipa::path(
    get,
    path = "/notifications",
    tag = "notifications",
    params(ListQuery),
    responses(
        (status = 200, description = "The bell", body = Inbox),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn inbox(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<ListQuery>,
) -> AppResult<Json<Inbox>> {
    let items = notify::list(
        &state.db,
        current.id(),
        query.unread.unwrap_or(false),
        query.limit.unwrap_or(50),
    )
    .await?;
    let unread = notify::unread_count(&state.db, current.id()).await?;
    Ok(Json(Inbox { items, unread }))
}

/// Marks one notification read.
#[utoipa::path(
    post,
    path = "/notifications/{id}/read",
    tag = "notifications",
    params(("id" = String, Path, description = "The notification")),
    responses(
        (status = 204, description = "Read"),
        (status = 404, description = "No such notification of yours", body = Problem),
    )
)]
async fn read_one(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    if notify::mark_read(&state.db, current.id(), &id, now()).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

/// Marks every notification read.
#[utoipa::path(
    post,
    path = "/notifications/read-all",
    tag = "notifications",
    responses((status = 204, description = "All read"))
)]
async fn read_all(State(state): State<AppState>, current: CurrentUser) -> AppResult<StatusCode> {
    notify::mark_all_read(&state.db, current.id(), now()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The caller's notification preferences.
#[utoipa::path(
    get,
    path = "/notifications/preferences",
    tag = "notifications",
    responses((status = 200, description = "Preferences, defaults filled in", body = Preferences))
)]
async fn get_preferences(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<Json<Preferences>> {
    Ok(Json(notify::preferences(&state.db, current.id()).await?))
}

/// Replaces the caller's notification preferences.
#[utoipa::path(
    put,
    path = "/notifications/preferences",
    tag = "notifications",
    request_body = Preferences,
    responses(
        (status = 200, description = "Saved", body = Preferences),
        (status = 422, description = "Email chosen with no SMTP configured", body = Problem),
    )
)]
async fn put_preferences(
    State(state): State<AppState>,
    current: CurrentUser,
    Json(body): Json<Preferences>,
) -> AppResult<Json<Preferences>> {
    let configured = mail::settings(&state.db).await?.is_some();
    notify::require_mail_for(&body, configured)?;
    notify::set_preferences(&state.db, current.id(), &body).await?;
    Ok(Json(notify::preferences(&state.db, current.id()).await?))
}

/// The SMTP settings. Admin only. The password is a vault credential and never appears here.
#[utoipa::path(
    get,
    path = "/admin/smtp",
    tag = "admin",
    responses(
        (status = 200, description = "The settings, or null when email is off", body = Option<SmtpSettings>),
        (status = 403, description = "Not an admin", body = Problem),
    )
)]
async fn get_smtp(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> AppResult<Json<Option<SmtpSettings>>> {
    Ok(Json(mail::settings(&state.db).await?))
}

/// Saves the SMTP settings.
#[utoipa::path(
    put,
    path = "/admin/smtp",
    tag = "admin",
    request_body = SmtpSettings,
    responses(
        (status = 200, description = "Saved", body = SmtpSettings),
        (status = 422, description = "Invalid host, address, or credential", body = Problem),
    )
)]
async fn put_smtp(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(body): Json<SmtpSettings>,
) -> AppResult<Json<SmtpSettings>> {
    mail::save(&state.db, &body, now()).await?;
    mail::settings(&state.db).await?.map(Json).ok_or_else(|| {
        AppError::internal(anyhow::anyhow!("the SMTP settings just saved are missing"))
    })
}

/// Turns email off.
#[utoipa::path(
    delete,
    path = "/admin/smtp",
    tag = "admin",
    responses((status = 204, description = "Email is off"))
)]
async fn delete_smtp(State(state): State<AppState>, _admin: RequireAdmin) -> AppResult<StatusCode> {
    mail::clear(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends a test email with the saved settings.
#[utoipa::path(
    post,
    path = "/admin/smtp/test",
    tag = "admin",
    request_body = TestEmailRequest,
    responses(
        (status = 204, description = "Sent"),
        (status = 422, description = "Not configured, or the server refused", body = Problem),
    )
)]
async fn test_smtp(
    State(state): State<AppState>,
    admin: RequireAdmin,
    Json(body): Json<TestEmailRequest>,
) -> AppResult<StatusCode> {
    let to = body
        .to
        .or_else(|| admin.0.user.email.clone())
        .ok_or_else(|| AppError::Validation("Give an address to send the test to.".to_owned()))?;
    mail::send(
        &state.db,
        state.vault.as_deref(),
        &to,
        "Atlas test email",
        "If you can read this, Atlas can send email.",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The notification and SMTP routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(inbox))
        .routes(routes!(read_one))
        .routes(routes!(read_all))
        .routes(routes!(get_preferences, put_preferences))
        .routes(routes!(get_smtp, put_smtp, delete_smtp))
        .routes(routes!(test_smtp))
}

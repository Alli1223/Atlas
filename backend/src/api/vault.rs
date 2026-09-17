//! `/api/v1/admin/credentials` — the secrets vault's settings surface.
//!
//! Instance-level, not per-project: a self-hosted single instance has one
//! GitHub PAT to hold, not one per project (Phase 12 links a *project* to a
//! *repo*, which is a separate, much cheaper row). Admin only, like the rest
//! of `api::admin`.
//!
//! The domain rules live in [`crate::vault`]; this module is the HTTP shape
//! over them.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::events::{self, Kind};
use crate::auth::extract::{ClientInfo, RequireAdmin};
use crate::auth::now;
use crate::error::{AppError, AppResult, Problem};
use crate::vault::credential::{self, Provider, RedactedCredential};
use crate::vault::github;

/// The body of `PUT /admin/credentials`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutCredentialRequest {
    /// Which provider this credential is for.
    pub provider: Provider,
    /// A human label. Adding with the same `(provider, label)` as an existing
    /// row replaces it rather than creating a second one.
    #[schema(example = "default")]
    pub label: String,
    /// The plaintext credential — a GitHub PAT, for the one provider today.
    /// Never stored, logged, or echoed back: sealed immediately and discarded.
    #[schema(example = "ghp_...")]
    pub secret: String,
}

/// Lists every stored credential. Metadata only — see [`RedactedCredential`].
#[utoipa::path(
    get,
    path = "/admin/credentials",
    tag = "admin",
    responses(
        (status = 200, description = "Every stored credential, newest first", body = Vec<RedactedCredential>),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
    )
)]
async fn list_credentials(
    _: RequireAdmin,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<RedactedCredential>>> {
    let rows = credential::list(&state.db).await?;
    Ok(Json(
        rows.iter().map(credential::Credential::redacted).collect(),
    ))
}

/// Adds a credential, or replaces the existing one with the same
/// `(provider, label)`.
#[utoipa::path(
    put,
    path = "/admin/credentials",
    tag = "admin",
    request_body = PutCredentialRequest,
    responses(
        (status = 200, description = "Stored — unchecked until validated", body = RedactedCredential),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
        (status = 422, description = "The secret is implausibly short or long", body = Problem),
    )
)]
async fn put_credential(
    RequireAdmin(current): RequireAdmin,
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Json(body): Json<PutCredentialRequest>,
) -> AppResult<Json<RedactedCredential>> {
    let saved = credential::put(
        &state.db,
        &state.vault,
        body.provider,
        &body.label,
        &body.secret,
        current.id(),
        now(),
    )
    .await?;

    events::record(
        &state.db,
        Kind::CredentialPut,
        Some(current.id()),
        &client,
        Some(&format!(
            "{} credential {:?} stored",
            saved.provider, saved.label
        )),
        now(),
    )
    .await;

    Ok(Json(saved.redacted()))
}

/// Runs a credential's validation probe against its provider.
///
/// The only route that decrypts a stored secret — see
/// [`crate::vault::credential::Credential::decrypt`].
#[utoipa::path(
    post,
    path = "/admin/credentials/{id}/validate",
    tag = "admin",
    params(("id" = String, Path, description = "The credential's id")),
    responses(
        (status = 200, description = "Probe ran — check `status` for the outcome", body = RedactedCredential),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
        (status = 404, description = "No such credential", body = Problem),
        (status = 502, description = "The probe itself failed (network, timeout)", body = Problem),
    )
)]
async fn validate_credential(
    RequireAdmin(current): RequireAdmin,
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Path(id): Path<String>,
) -> AppResult<Json<RedactedCredential>> {
    let stored = credential::find(&state.db, &id).await?;
    let plaintext = stored.decrypt(&state.vault).map_err(AppError::internal)?;

    let result = match stored.provider {
        Provider::GitHub => github::validate(&plaintext).await.map_err(|err| {
            AppError::internal(anyhow::anyhow!("GitHub validation probe failed: {err}"))
        })?,
    };

    let now = now();
    credential::record_validation(&state.db, &id, &result, now).await?;

    events::record(
        &state.db,
        Kind::CredentialValidated,
        Some(current.id()),
        &client,
        Some(&format!(
            "{} credential {:?} validated: {}",
            stored.provider,
            stored.label,
            result.status.as_str()
        )),
        now,
    )
    .await;

    let refreshed = credential::find(&state.db, &id).await?;
    Ok(Json(refreshed.redacted()))
}

/// Deletes a stored credential.
#[utoipa::path(
    delete,
    path = "/admin/credentials/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "The credential's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not authenticated", body = Problem),
        (status = 403, description = "Forbidden — admins only", body = Problem),
        (status = 404, description = "No such credential", body = Problem),
    )
)]
async fn delete_credential(
    RequireAdmin(current): RequireAdmin,
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let existing = credential::find(&state.db, &id).await?;
    credential::delete(&state.db, &id).await?;

    events::record(
        &state.db,
        Kind::CredentialDeleted,
        Some(current.id()),
        &client,
        Some(&format!(
            "{} credential {:?} deleted",
            existing.provider, existing.label
        )),
        now(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// The vault routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_credentials, put_credential))
        .routes(routes!(validate_credential))
        .routes(routes!(delete_credential))
}

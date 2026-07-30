//! `/api/v1/credentials` — the secrets vault's HTTP surface. **Admin only, and
//! never returns a secret.**
//!
//! # Where the authorisation is
//!
//! In the [`RequireAdmin`] in every handler signature — a route that names it
//! cannot forget to check, and a route that does not is visibly not admin-only.
//! These routes are *instance settings*, not project-scoped: there is no project a
//! GitHub token belongs to. They are classified [`crate::auth::project_access::Scope::Unscoped`]
//! in `SCOPES`, with the justification recorded there.
//!
//! # The one rule that dominates this file
//!
//! **No handler returns the plaintext, ever.** Create and list respond with
//! [`CredentialDto`] — a type with no field for the secret, the ciphertext, or the
//! nonce, so a secret cannot leak through it by any edit short of adding a field
//! and a decrypt call, both loud in review. The plaintext is decrypted in exactly
//! one place (`validate`, to hand it to the provider probe) and never serialised.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireAdmin;
use crate::auth::now;
use crate::error::{AppError, AppResult, Problem};
use crate::secrets::vault::{Vault, default_validator};
use crate::secrets::{self, CredentialDto, NewCredential, Provider, Secret};

/// Longest accepted label, in characters.
const MAX_LABEL: usize = 128;

/// Longest accepted secret, in characters. Comfortably above any real API key or
/// PAT (GitHub PATs are ~40–255, JWT-shaped keys a few hundred) while still
/// bounding what a single field can carry.
const MAX_SECRET: usize = 8192;

/// The body of `POST /credentials`.
///
/// `secret` deserialises straight into a [`Secret`], so the plaintext is redacted
/// in this struct's `Debug` from the moment it exists — a `tracing` line that logs
/// the request body cannot leak it. `#[schema(value_type = String)]` tells OpenAPI
/// it is a string on the wire without teaching the schema anything about the
/// wrapper.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCredentialRequest {
    /// Which integration the secret is for.
    pub provider: Provider,
    /// A human label, unique within the provider.
    pub label: String,
    /// The secret itself — a PAT or API key. Never stored or returned in
    /// cleartext.
    #[schema(value_type = String, example = "ghp_your_token_here")]
    pub secret: Secret<String>,
}

/// Every stored credential, as metadata.
#[utoipa::path(
    get,
    path = "/credentials",
    tag = "credentials",
    responses(
        (status = 200, description = "Every credential, as metadata — never the secret", body = Vec<CredentialDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
    )
)]
async fn list_credentials(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> AppResult<Json<Vec<CredentialDto>>> {
    let now = now();
    let credentials = secrets::list(&state.db).await?;
    Ok(Json(
        credentials
            .iter()
            .map(|credential| CredentialDto::from_row(credential, now))
            .collect(),
    ))
}

/// Stores a new credential, encrypting the secret at rest.
#[utoipa::path(
    post,
    path = "/credentials",
    tag = "credentials",
    request_body = CreateCredentialRequest,
    responses(
        (status = 201, description = "Stored; the response is metadata only", body = CredentialDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 409, description = "A credential with this provider and label already exists", body = Problem),
        (status = 422, description = "The label or secret is invalid", body = Problem),
        (status = 500, description = "The secrets vault is not configured (no ATLAS_MASTER_KEY)", body = Problem),
    )
)]
async fn create_credential(
    State(state): State<AppState>,
    admin: RequireAdmin,
    Json(body): Json<CreateCredentialRequest>,
) -> AppResult<(StatusCode, Json<CredentialDto>)> {
    let vault = require_vault(&state)?;

    let label = validate_label(&body.label)?;
    let secret = validate_secret(&body.secret)?;
    let last_four = secrets::last_four(secret.expose());

    // The id is minted here so the ciphertext can be bound to it before the row
    // exists — the AAD and the primary key must be the same value.
    let id = secrets::new_id();
    let sealed = vault.seal_for(&id, &secret)?;

    let now = now();
    let mut tx = state.db.begin_write().await?;

    // Checked inside the transaction so the check and the insert cannot be
    // separated by another writer. The UNIQUE index is the real guarantee; this
    // turns its 500 into a 409 that says what to fix.
    if secrets::label_taken(&mut tx, body.provider, &label).await? {
        return Err(AppError::Conflict(format!(
            "A {} credential labelled {label:?} already exists. Pick another label, or delete the \
             existing one first.",
            body.provider
        )));
    }

    let credential = secrets::insert(
        &mut tx,
        &id,
        &NewCredential {
            provider: body.provider,
            label,
            sealed,
            last_four,
            created_by: admin.0.id().to_owned(),
        },
        now,
    )
    .await?;

    tx.commit().await?;

    // Audit the write. The secret is a `Secret`, so even a careless edit that
    // added it to this line would render `[REDACTED]` — but it is simply not
    // here.
    tracing::info!(
        provider = %credential.provider,
        credential_id = %credential.id,
        actor = %admin.0.id(),
        "stored an API credential"
    );

    Ok((
        StatusCode::CREATED,
        Json(CredentialDto::from_row(&credential, now)),
    ))
}

/// Deletes a credential.
#[utoipa::path(
    delete,
    path = "/credentials/{id}",
    tag = "credentials",
    params(("id" = String, Path, description = "The credential's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such credential", body = Problem),
    )
)]
async fn delete_credential(
    State(state): State<AppState>,
    admin: RequireAdmin,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let removed = secrets::delete(&mut tx, &id).await?;
    tx.commit().await?;

    if !removed {
        return Err(AppError::NotFound);
    }

    tracing::info!(
        credential_id = %id,
        actor = %admin.0.id(),
        "deleted an API credential"
    );

    Ok(StatusCode::NO_CONTENT)
}

/// Validates a credential against its provider, on demand.
///
/// Decrypts the secret, runs the provider probe ([`crate::secrets::Validator`]),
/// and records the outcome — status, discovered scopes, discovered expiry, and
/// `last_validated_at`. The response is the updated metadata; the secret is opened
/// only to hand it to the probe and is never serialised.
///
/// Today every provider uses the no-op probe, which reports `unchecked` — the
/// endpoint plumbing (decrypt → probe → persist) works end to end; the GitHub and
/// Gemini agents drop their real probes into [`default_validator`].
#[utoipa::path(
    post,
    path = "/credentials/{id}/validate",
    tag = "credentials",
    params(("id" = String, Path, description = "The credential's id")),
    responses(
        (status = 200, description = "Validated; the updated metadata", body = CredentialDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such credential", body = Problem),
        (status = 500, description = "The secrets vault is not configured, or the secret could not be decrypted", body = Problem),
    )
)]
async fn validate_credential(
    State(state): State<AppState>,
    admin: RequireAdmin,
    Path(id): Path<String>,
) -> AppResult<Json<CredentialDto>> {
    let vault = require_vault(&state)?;
    let now = now();

    let credential = secrets::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    // The one decrypt in the API layer. The plaintext lives in a `Secret`, is
    // handed to the probe by reference, and is dropped (and zeroed) at the end of
    // this function without ever being serialised.
    let secret = vault.open(&credential)?;
    let outcome = default_validator(credential.provider)
        .validate(&secret)
        .await?;

    let mut tx = state.db.begin_write().await?;
    secrets::apply_validation(&mut tx, &id, &outcome, now).await?;
    let updated = secrets::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    tracing::info!(
        provider = %updated.provider,
        credential_id = %updated.id,
        status = %outcome.status,
        actor = %admin.0.id(),
        "validated an API credential"
    );

    Ok(Json(CredentialDto::from_row(&updated, now)))
}

/// The configured vault, or a 500 that names the missing variable.
///
/// The vault is absent only when `ATLAS_MASTER_KEY` is unset — which
/// `Config::validate` already forbids in prod, so this can fire only on a dev
/// instance that has not set one. A 500 (opaque body, cause logged) rather than a
/// 422: it is a deployment omission, not a bad request.
fn require_vault(state: &AppState) -> AppResult<&Vault> {
    state.vault.as_deref().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!(
            "the secrets vault is not configured: set ATLAS_MASTER_KEY (openssl rand -base64 32) \
             and restart"
        ))
    })
}

/// Checks a label: trimmed, non-empty, bounded, no control characters.
fn validate_label(label: &str) -> AppResult<String> {
    let label = label.trim();

    if label.is_empty() {
        return Err(AppError::Validation("Label must not be empty.".to_owned()));
    }
    if label.chars().count() > MAX_LABEL {
        return Err(AppError::Validation(format!(
            "Label must be at most {MAX_LABEL} characters long."
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Label must not contain control characters.".to_owned(),
        ));
    }

    Ok(label.to_owned())
}

/// Checks a secret and returns it trimmed of surrounding whitespace.
///
/// Surrounding whitespace is stripped because a pasted token routinely carries a
/// trailing newline, and no provider key has meaningful leading or trailing
/// spaces — an interior space (an SMTP passphrase, say) is preserved. A control
/// character *inside* the value is rejected: it is almost certainly a paste
/// artefact, and it is a log-injection vector if the value ever reached a log
/// (which it must not, but defence in depth).
fn validate_secret(secret: &Secret<String>) -> AppResult<Secret<String>> {
    let trimmed = secret.expose().trim();

    if trimmed.is_empty() {
        return Err(AppError::Validation("Secret must not be empty.".to_owned()));
    }
    if trimmed.chars().count() > MAX_SECRET {
        return Err(AppError::Validation(format!(
            "Secret must be at most {MAX_SECRET} characters long."
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Secret must not contain control characters.".to_owned(),
        ));
    }

    Ok(Secret::new(trimmed.to_owned()))
}

/// The `/credentials` routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{id}`, never `:id`.
        .routes(routes!(list_credentials, create_credential))
        .routes(routes!(delete_credential))
        .routes(routes!(validate_credential))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_trimmed_bounded_and_reject_control_characters() {
        assert_eq!(validate_label("  work PAT  ").unwrap(), "work PAT");
        assert!(validate_label("").is_err());
        assert!(validate_label("   ").is_err());
        assert!(validate_label("bad\nlabel").is_err());
        assert!(validate_label(&"a".repeat(MAX_LABEL + 1)).is_err());
        assert!(validate_label(&"a".repeat(MAX_LABEL)).is_ok());
    }

    #[test]
    fn secrets_are_trimmed_but_interior_spaces_survive() {
        let secret = validate_secret(&Secret::new("  ghp_token\n".to_owned())).unwrap();
        assert_eq!(secret.expose(), "ghp_token");

        let smtp = validate_secret(&Secret::new("pass with spaces".to_owned())).unwrap();
        assert_eq!(smtp.expose(), "pass with spaces");
    }

    #[test]
    fn empty_or_control_bearing_secrets_are_rejected() {
        assert!(validate_secret(&Secret::new(String::new())).is_err());
        assert!(validate_secret(&Secret::new("   ".to_owned())).is_err());
        assert!(validate_secret(&Secret::new("tok\u{0}en".to_owned())).is_err());
        assert!(
            validate_secret(&Secret::new(format!("{}x", "a".repeat(MAX_SECRET))).clone()).is_err()
        );
    }

    #[test]
    fn the_request_debug_never_reveals_the_secret() {
        // The secret field is a `Secret`, so even a `tracing::debug!(?body)` on the
        // whole request body cannot leak it.
        let body: CreateCredentialRequest = serde_json::from_str(
            r#"{"provider":"github","label":"work","secret":"ghp_supersecrettoken"}"#,
        )
        .unwrap();
        let rendered = format!("{body:?}");
        assert!(!rendered.contains("supersecrettoken"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
        assert_eq!(body.provider, Provider::Github);
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        assert!(
            serde_json::from_str::<CreateCredentialRequest>(
                r#"{"provider":"github","label":"x","secret":"y","isAdmin":true}"#,
            )
            .is_err()
        );
    }
}

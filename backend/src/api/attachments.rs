//! Attachments on a card, and the caller's avatar.
//!
//! # Uploads are raw bodies, not multipart
//!
//! `POST /cards/{key}/attachments?filename=model.blend` with the file as the request body and
//! its type as `Content-Type`. That is exactly what `fetch(url, { body: file })` sends from a
//! drop or a clipboard paste, it streams straight to disk with no form parser in between, and
//! it needs no multipart dependency. The filename is a query parameter because it is metadata,
//! not content.
//!
//! # Downloads cannot become pages on Atlas's origin
//!
//! Only passive formats (`storage::is_inline_safe`: PNG/JPEG/GIF/WebP/PDF) are ever served
//! `inline` with their real type; everything else — `.html`, `.svg`, `.js` — goes out as
//! `application/octet-stream` with `Content-Disposition: attachment`. Every response also
//! carries `X-Content-Type-Options: nosniff` and a `sandbox` CSP, so even a mislabelled file
//! cannot run script with the viewer's session.

// See the note at the top of `api::cards`: `params(<IntoParams type>)` expands to a qualified
// path carrying the handler signature's span, so the lint is silenced at module scope.
#![allow(unused_qualifications)]

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tokio_util::io::ReaderStream;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::role::Role;
use crate::auth::{CurrentUser, UserDto, now, to_sql_timestamp, user};
use crate::domain::attachment::{self, Attachment, AttachmentVersion, Slot, Upload};
use crate::domain::card;
use crate::error::{AppError, AppResult, Problem};
use crate::storage::{self, Area};

/// Largest accepted avatar.
const MAX_AVATAR_BYTES: u64 = 2 * 1024 * 1024;

/// The query of `POST /cards/{key}/attachments`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UploadQuery {
    /// The file's name. Display only — it is never used to build a path. Re-using a name on
    /// the same card adds a new version.
    pub filename: String,
}

/// The query of `GET /attachments/{id}/content`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContentQuery {
    /// Which version. Defaults to the newest.
    #[serde(default)]
    pub version: Option<i64>,
    /// Force a download even for a previewable type.
    #[serde(default)]
    pub download: Option<bool>,
}

/// A card's attachments, newest activity first.
#[utoipa::path(
    get,
    path = "/cards/{key}/attachments",
    tag = "attachments",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's attachments", body = Vec<Attachment>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn list_attachments(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Attachment>>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(attachment::list(&state.db, &target.id).await?))
}

/// Uploads a file to a card. The body is the file; `Content-Type` is its type.
///
/// Uploading a name the card already has adds a version rather than a second attachment.
#[utoipa::path(
    post,
    path = "/cards/{key}/attachments",
    tag = "attachments",
    params(("key" = String, Path, description = "The card key"), UploadQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "The file's bytes"),
    responses(
        (status = 201, description = "Stored (as a new attachment or a new version)", body = Attachment),
        (status = 404, description = "No such card", body = Problem),
        (status = 413, description = "Larger than ATLAS_MAX_UPLOAD_MB", body = Problem),
        (status = 422, description = "Larger than ATLAS_MAX_UPLOAD_MB", body = Problem),
    )
)]
async fn upload_attachment(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> AppResult<(StatusCode, Json<Attachment>)> {
    let filename = storage::clean_filename(&query.filename);
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let content_type = storage::content_type_for(declared, &filename);
    let limit = state.config.max_upload_bytes();

    // Refuse on the declared length before reading a byte, when there is one.
    if let Some(length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        && length > limit
    {
        return Err(AppError::Validation(format!(
            "The file is larger than the {} MB upload limit.",
            state.config.max_upload_mb
        )));
    }

    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    // Reserve the slot (which attachment, which version) in a short read, write the bytes
    // with no transaction open — a 100 MB upload must not hold SQLite's single writer — then
    // record the rows. A concurrent upload of the same name can race for the version number;
    // the UNIQUE (attachment_id, version) constraint turns the loser into an error rather than
    // a silent overwrite, and its orphaned file is removed.
    let slot: Slot = {
        let mut tx = state.db.begin_write().await?;
        let slot = attachment::reserve(&mut tx, &target.id, &filename).await?;
        tx.commit().await?;
        slot
    };
    let path = storage::path_for(
        &state.config.data_dir,
        Area::Attachments,
        &slot.attachment_id,
        Some(slot.version),
    )?;
    let stored = storage::write_stream(body, &path, limit).await?;

    let recorded = async {
        let mut tx = state.db.begin_write().await?;
        let row = attachment::record(
            &mut tx,
            &slot,
            &Upload {
                card_id: &target.id,
                filename: &filename,
                content_type: &content_type,
                size_bytes: i64::try_from(stored.size).unwrap_or(i64::MAX),
                sha256: &stored.sha256,
                uploaded_by: Some(member.0.id()),
            },
            now(),
        )
        .await?;
        tx.commit().await?;
        AppResult::Ok(row)
    }
    .await;

    match recorded {
        Ok(row) => Ok((StatusCode::CREATED, Json(row))),
        Err(err) => {
            storage::remove(&path).await;
            match err {
                AppError::Internal(inner)
                    if inner
                        .downcast_ref::<sqlx::Error>()
                        .and_then(|e| e.as_database_error())
                        .is_some_and(sqlx::error::DatabaseError::is_unique_violation) =>
                {
                    Err(AppError::Conflict(
                        "Another upload of this file landed at the same moment. Try again."
                            .to_owned(),
                    ))
                }
                other => Err(other),
            }
        }
    }
}

/// Every version of an attachment, newest first.
#[utoipa::path(
    get,
    path = "/attachments/{id}/versions",
    tag = "attachments",
    params(("id" = String, Path, description = "The attachment")),
    responses(
        (status = 200, description = "Its versions", body = Vec<AttachmentVersion>),
        (status = 404, description = "No such attachment", body = Problem),
    )
)]
async fn list_versions(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<AttachmentVersion>>> {
    Ok(Json(attachment::versions(&state.db, &id).await?))
}

/// Downloads (or previews) an attachment's bytes.
#[utoipa::path(
    get,
    path = "/attachments/{id}/content",
    tag = "attachments",
    params(("id" = String, Path, description = "The attachment"), ContentQuery),
    responses(
        (status = 200, description = "The file", content_type = "application/octet-stream"),
        (status = 404, description = "No such attachment or version", body = Problem),
    )
)]
async fn download_attachment(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<ContentQuery>,
) -> AppResult<Response> {
    let found = attachment::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let version = attachment::version(&state.db, &id, query.version)
        .await?
        .ok_or(AppError::NotFound)?;
    let path = storage::path_for(
        &state.config.data_dir,
        Area::Attachments,
        &id,
        Some(version.version),
    )?;
    let inline = !query.download.unwrap_or(false) && storage::is_inline_safe(&version.content_type);
    serve_file(&path, &found.filename, &version.content_type, inline).await
}

/// Streams a stored file with the hardening headers described in the module doc.
async fn serve_file(
    path: &std::path::Path,
    filename: &str,
    content_type: &str,
    inline: bool,
) -> AppResult<Response> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(AppError::NotFound),
        Err(err) => return Err(AppError::internal(err)),
    };
    let length = file.metadata().await.map_err(AppError::internal)?.len();
    let served_type = if inline {
        content_type
    } else {
        "application/octet-stream"
    };
    // RFC 6266: an ASCII fallback plus the exact UTF-8 name.
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = filename
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let disposition = format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    );

    let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(served_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "sandbox; default-src 'none'; img-src 'self'; style-src 'unsafe-inline'",
        ),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(response)
}

/// Deletes an attachment and every version of it. The uploader, or an instance admin.
#[utoipa::path(
    delete,
    path = "/attachments/{id}",
    tag = "attachments",
    params(("id" = String, Path, description = "The attachment")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Not the uploader, and not an admin", body = Problem),
        (status = 404, description = "No such attachment", body = Problem),
    )
)]
async fn delete_attachment(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let found = attachment::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if found.created_by.as_deref() != Some(member.0.id()) && !member.0.has_role(Role::Admin) {
        return Err(AppError::Forbidden);
    }
    let mut tx = state.db.begin_write().await?;
    attachment::delete(&mut tx, &id).await?;
    tx.commit().await?;
    storage::remove(&storage::path_for(
        &state.config.data_dir,
        Area::Attachments,
        &id,
        None,
    )?)
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Avatars
// ---------------------------------------------------------------------------

/// Sets the caller's avatar. The body is a PNG, JPEG, GIF or WebP image of at most 2 MB.
///
/// The type is sniffed from the bytes, not taken from `Content-Type`, so an avatar can only
/// ever be an image — it is served inline to every other user.
#[utoipa::path(
    put,
    path = "/auth/me/avatar",
    tag = "auth",
    request_body(content = Vec<u8>, content_type = "image/png", description = "The image"),
    responses(
        (status = 200, description = "Set; the updated account", body = UserDto),
        (status = 422, description = "Not an image, or too large", body = Problem),
    )
)]
async fn set_avatar(
    State(state): State<AppState>,
    current: CurrentUser,
    body: Body,
) -> AppResult<Json<UserDto>> {
    let path = storage::path_for(&state.config.data_dir, Area::Avatars, current.id(), None)?;
    let temp = path.with_extension("incoming");
    storage::write_stream(body, &temp, MAX_AVATAR_BYTES).await?;
    let head = {
        let bytes = tokio::fs::read(&temp).await.map_err(AppError::internal)?;
        storage::sniff_image(&bytes)
    };
    if head.is_none() {
        storage::remove(&temp).await;
        return Err(AppError::Validation(
            "An avatar must be a PNG, JPEG, GIF or WebP image.".to_owned(),
        ));
    }
    tokio::fs::rename(&temp, &path)
        .await
        .map_err(AppError::internal)?;

    let stamp = now();
    let url = format!(
        "/api/v1/users/{}/avatar?v={}",
        current.id(),
        stamp.timestamp()
    );
    let mut tx = state.db.begin_write().await?;
    sqlx::query("UPDATE users SET avatar_url = ?, updated_at = ? WHERE id = ?")
        .bind(&url)
        .bind(to_sql_timestamp(stamp))
        .bind(current.id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let refreshed = user::find_by_id(&state.db, current.id())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(UserDto::from(&refreshed)))
}

/// Removes the caller's avatar.
#[utoipa::path(
    delete,
    path = "/auth/me/avatar",
    tag = "auth",
    responses((status = 204, description = "Removed"))
)]
async fn clear_avatar(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    sqlx::query("UPDATE users SET avatar_url = NULL, updated_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now()))
        .bind(current.id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    storage::remove(&storage::path_for(
        &state.config.data_dir,
        Area::Avatars,
        current.id(),
        None,
    )?)
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// A user's avatar image. Any signed-in user may see any avatar.
#[utoipa::path(
    get,
    path = "/users/{id}/avatar",
    tag = "users",
    params(("id" = String, Path, description = "The user")),
    responses(
        (status = 200, description = "The image", content_type = "image/png"),
        (status = 404, description = "No avatar", body = Problem),
    )
)]
async fn get_avatar(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let path = storage::path_for(&state.config.data_dir, Area::Avatars, &id, None)?;
    let head = match tokio::fs::read(&path).await {
        Ok(bytes) => storage::sniff_image(&bytes[..bytes.len().min(16)]),
        Err(_) => return Err(AppError::NotFound),
    };
    let content_type = head.ok_or(AppError::NotFound)?;
    serve_file(&path, "avatar", content_type, true).await
}

/// The attachment and avatar routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_attachments, upload_attachment))
        .routes(routes!(list_versions))
        .routes(routes!(download_attachment))
        .routes(routes!(delete_attachment))
        .routes(routes!(set_avatar, clear_avatar))
        .routes(routes!(get_avatar))
}

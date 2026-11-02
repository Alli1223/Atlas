//! `attachments` and `attachment_versions`: files on a card, with history.
//!
//! An attachment is a *name on a card*; its bytes are a stack of versions. Uploading a file
//! whose (cleaned) name matches an existing attachment on the same card adds a version rather
//! than a second attachment — `model.blend` v1, v2, v3 instead of `model_v3_final.blend`.
//! Every version stays downloadable; the attachment points at the newest.
//!
//! The bytes are on disk (see [`crate::storage`]); this module owns only the rows.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// An attachment as listed on a card: its newest version's metadata inline.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// UUID v7.
    pub id: String,
    /// The card.
    pub card_id: String,
    /// The display name. Unique per card.
    pub filename: String,
    /// The newest version number.
    pub current_version: i64,
    /// The newest version's content type.
    pub content_type: String,
    /// The newest version's size.
    pub size_bytes: i64,
    /// Who first attached it.
    pub created_by: Option<String>,
    /// When it was first attached.
    pub created_at: DateTime<Utc>,
    /// When the newest version landed.
    pub updated_at: DateTime<Utc>,
}

/// One version of an attachment.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentVersion {
    /// UUID v7.
    pub id: String,
    /// The attachment.
    pub attachment_id: String,
    /// 1, 2, 3…
    pub version: i64,
    /// What the uploader declared (normalised).
    pub content_type: String,
    /// Bytes.
    pub size_bytes: i64,
    /// Hex SHA-256 — lets a client tell an identical re-upload from a real change.
    pub sha256: String,
    /// Who uploaded this version.
    pub uploaded_by: Option<String>,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// A card's attachments, newest activity first.
pub async fn list(db: &Db, card_id: &str) -> AppResult<Vec<Attachment>> {
    Ok(sqlx::query_as::<_, Attachment>(
        "SELECT a.id, a.card_id, a.filename, a.current_version, \
                v.content_type, v.size_bytes, a.created_by, a.created_at, a.updated_at \
           FROM attachments a \
           JOIN attachment_versions v ON v.attachment_id = a.id AND v.version = a.current_version \
          WHERE a.card_id = ? ORDER BY a.updated_at DESC, a.id",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds an attachment.
pub async fn find(db: &Db, id: &str) -> AppResult<Option<Attachment>> {
    Ok(sqlx::query_as::<_, Attachment>(
        "SELECT a.id, a.card_id, a.filename, a.current_version, \
                v.content_type, v.size_bytes, a.created_by, a.created_at, a.updated_at \
           FROM attachments a \
           JOIN attachment_versions v ON v.attachment_id = a.id AND v.version = a.current_version \
          WHERE a.id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

async fn find_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Attachment>> {
    Ok(sqlx::query_as::<_, Attachment>(
        "SELECT a.id, a.card_id, a.filename, a.current_version, \
                v.content_type, v.size_bytes, a.created_by, a.created_at, a.updated_at \
           FROM attachments a \
           JOIN attachment_versions v ON v.attachment_id = a.id AND v.version = a.current_version \
          WHERE a.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Every version of an attachment, newest first.
pub async fn versions(db: &Db, attachment_id: &str) -> AppResult<Vec<AttachmentVersion>> {
    Ok(sqlx::query_as::<_, AttachmentVersion>(
        "SELECT id, attachment_id, version, content_type, size_bytes, sha256, uploaded_by, \
                created_at FROM attachment_versions WHERE attachment_id = ? \
          ORDER BY version DESC",
    )
    .bind(attachment_id)
    .fetch_all(db.reader())
    .await?)
}

/// One version, or the newest when `version` is `None`.
pub async fn version(
    db: &Db,
    attachment_id: &str,
    version: Option<i64>,
) -> AppResult<Option<AttachmentVersion>> {
    Ok(sqlx::query_as::<_, AttachmentVersion>(
        "SELECT v.id, v.attachment_id, v.version, v.content_type, v.size_bytes, v.sha256, \
                v.uploaded_by, v.created_at \
           FROM attachment_versions v JOIN attachments a ON a.id = v.attachment_id \
          WHERE v.attachment_id = ? AND v.version = COALESCE(?, a.current_version)",
    )
    .bind(attachment_id)
    .bind(version)
    .fetch_optional(db.reader())
    .await?)
}

/// The slot an upload is about to fill: which attachment, and which version number.
#[derive(Debug, Clone)]
pub struct Slot {
    /// The attachment id (new or existing).
    pub attachment_id: String,
    /// The version the upload will become.
    pub version: i64,
    /// Whether this creates the attachment rather than versioning it.
    pub is_new: bool,
}

/// Decides where an upload named `filename` lands on a card, inside the write transaction.
///
/// Called *before* the bytes are written, so the storage path is known; [`record`] then
/// commits the rows once the file is safely on disk.
pub async fn reserve(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    filename: &str,
) -> AppResult<Slot> {
    let existing: Option<(String, i64)> = sqlx::query_as(
        "SELECT id, current_version FROM attachments WHERE card_id = ? AND filename = ?",
    )
    .bind(card_id)
    .bind(filename)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(match existing {
        Some((attachment_id, current)) => Slot {
            attachment_id,
            version: current + 1,
            is_new: false,
        },
        None => Slot {
            attachment_id: Uuid::now_v7().to_string(),
            version: 1,
            is_new: true,
        },
    })
}

/// What an upload wrote to disk.
#[derive(Debug)]
pub struct Upload<'a> {
    /// The card.
    pub card_id: &'a str,
    /// Display name, already cleaned.
    pub filename: &'a str,
    /// Normalised content type.
    pub content_type: &'a str,
    /// Bytes on disk.
    pub size_bytes: i64,
    /// Hex SHA-256 of them.
    pub sha256: &'a str,
    /// The uploader.
    pub uploaded_by: Option<&'a str>,
}

/// Records an upload into its reserved slot.
pub async fn record(
    tx: &mut sqlx::SqliteConnection,
    slot: &Slot,
    upload: &Upload<'_>,
    now: DateTime<Utc>,
) -> AppResult<Attachment> {
    let timestamp = to_sql_timestamp(now);
    if slot.is_new {
        sqlx::query(
            "INSERT INTO attachments (id, card_id, filename, current_version, created_by, \
             created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&slot.attachment_id)
        .bind(upload.card_id)
        .bind(upload.filename)
        .bind(slot.version)
        .bind(upload.uploaded_by)
        .bind(&timestamp)
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("UPDATE attachments SET current_version = ?, updated_at = ? WHERE id = ?")
            .bind(slot.version)
            .bind(&timestamp)
            .bind(&slot.attachment_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO attachment_versions (id, attachment_id, version, content_type, size_bytes, \
         sha256, uploaded_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&slot.attachment_id)
    .bind(slot.version)
    .bind(upload.content_type)
    .bind(upload.size_bytes)
    .bind(upload.sha256)
    .bind(upload.uploaded_by)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_tx(&mut *tx, &slot.attachment_id)
        .await?
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("the attachment just recorded is missing"))
        })
}

/// Deletes an attachment and every version row. The caller removes the files.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM attachments WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

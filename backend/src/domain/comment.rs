//! The `comments` row.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// Longest accepted comment body, in characters.
pub const MAX_BODY: usize = 64 * 1024;

/// A comment on a card.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    /// UUID v7, as text.
    pub id: String,
    /// The card it is on.
    pub card_id: String,
    /// Who wrote it.
    pub author_id: String,
    /// Markdown **source**.
    ///
    /// Never rendered HTML — the client renders and sanitises at read time. A
    /// stored-HTML comment field is a stored-XSS hole with a scheduler attached:
    /// every reader of the card runs whatever the writer put there, forever, and
    /// tightening the sanitiser later cannot retroactively clean what is already
    /// in the table.
    pub body: String,
    /// When it was posted.
    pub created_at: DateTime<Utc>,
    /// When the row last changed.
    pub updated_at: DateTime<Utc>,
    /// When the body was last edited. `None` = never.
    ///
    /// Distinct from `updated_at` on purpose: this is what the UI needs to show
    /// "(edited)" honestly, and it must not be set by a write that did not touch
    /// the text.
    pub edited_at: Option<DateTime<Utc>>,
}

/// Checks a comment body. Markdown, so only the length is bounded.
pub fn validate_body(body: &str) -> AppResult<String> {
    let body = body.trim();

    if body.is_empty() {
        return Err(AppError::Validation(
            "A comment must not be empty.".to_owned(),
        ));
    }
    if body.chars().count() > MAX_BODY {
        return Err(AppError::Validation(format!(
            "A comment must be at most {MAX_BODY} characters long."
        )));
    }

    Ok(body.to_owned())
}

/// Every comment on a card, oldest first.
pub async fn list(db: &Db, card_id: &str) -> AppResult<Vec<Comment>> {
    Ok(sqlx::query_as::<_, Comment>(
        "SELECT id, card_id, author_id, body, created_at, updated_at, edited_at \
         FROM comments WHERE card_id = ? ORDER BY created_at, id",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a comment by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Comment>> {
    Ok(sqlx::query_as::<_, Comment>(
        "SELECT id, card_id, author_id, body, created_at, updated_at, edited_at \
         FROM comments WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a comment by id inside an open transaction.
pub async fn find_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Comment>> {
    Ok(sqlx::query_as::<_, Comment>(
        "SELECT id, card_id, author_id, body, created_at, updated_at, edited_at \
         FROM comments WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Posts a comment.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    author_id: &str,
    body: &str,
    now: DateTime<Utc>,
) -> AppResult<Comment> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO comments (id, card_id, author_id, body, created_at, updated_at, edited_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&id)
    .bind(card_id)
    .bind(author_id)
    .bind(body)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the comment just inserted is missing")))
}

/// Edits a comment's body.
///
/// Sets `edited_at`, which is what makes "(edited)" honest. Re-submitting the
/// identical text is not an edit and does not set it — the same principle as
/// [`crate::domain::card::update`] not bumping `updated_at` for a no-op.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    comment: &Comment,
    body: &str,
    now: DateTime<Utc>,
) -> AppResult<Comment> {
    if comment.body == body {
        return Ok(comment.clone());
    }

    let timestamp = to_sql_timestamp(now);
    sqlx::query("UPDATE comments SET body = ?, updated_at = ?, edited_at = ? WHERE id = ?")
        .bind(body)
        .bind(&timestamp)
        .bind(&timestamp)
        .bind(&comment.id)
        .execute(&mut *tx)
        .await?;

    find_by_id_tx(&mut *tx, &comment.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the comment just edited is missing")))
}

/// Deletes a comment.
///
/// Hard, unlike a card. A comment is not referenced by anything — no history
/// row, no link, no key that leaked into a commit message — so there is nothing
/// for a tombstone to protect, and a deleted comment that lingers as "[deleted]"
/// is a worse answer than one that is gone.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM comments WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_are_trimmed_bounded_and_may_contain_markdown() {
        assert_eq!(validate_body("  Looks good  ").unwrap(), "Looks good");
        assert!(validate_body("# Heading\n\n- a\n- b\n\n```rs\nlet x = 1;\n```").is_ok());

        assert!(validate_body("").is_err());
        assert!(validate_body("   \n\t ").is_err());
        assert!(validate_body(&"a".repeat(MAX_BODY + 1)).is_err());
        assert!(validate_body(&"a".repeat(MAX_BODY)).is_ok());
    }
}

//! Card-to-card links (`card_links`), remote links (`remote_links`), and watchers.
//!
//! Three small aggregates that hang off a card and share a shape: a list, an add, a remove.
//!
//! # Card links materialise their inverse
//!
//! `A blocks B` is written as two rows — `(A, B, blocks)` and `(B, A, is_blocked_by)` — so
//! "show this card's links" is one indexed scan from either end and AQL's `linkedCards()` is
//! not a special case. Both rows go in and come out together, inside one transaction; a half
//! link (one direction only) is a state this module cannot produce.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

// ---------------------------------------------------------------------------
// Card links
// ---------------------------------------------------------------------------

/// The kinds of link a card can have to another, each with its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    /// This card blocks the other.
    Blocks,
    /// This card is blocked by the other.
    IsBlockedBy,
    /// Symmetric.
    RelatesTo,
    /// This card duplicates the other.
    Duplicates,
    /// This card is duplicated by the other.
    IsDuplicatedBy,
    /// This card was cloned from the other.
    Clones,
    /// This card is cloned by the other.
    IsClonedBy,
    /// This card causes the other.
    Causes,
    /// This card is caused by the other.
    IsCausedBy,
}

impl LinkType {
    /// Every link type.
    pub const ALL: [Self; 9] = [
        Self::Blocks,
        Self::IsBlockedBy,
        Self::RelatesTo,
        Self::Duplicates,
        Self::IsDuplicatedBy,
        Self::Clones,
        Self::IsClonedBy,
        Self::Causes,
        Self::IsCausedBy,
    ];

    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::IsBlockedBy => "is_blocked_by",
            Self::RelatesTo => "relates_to",
            Self::Duplicates => "duplicates",
            Self::IsDuplicatedBy => "is_duplicated_by",
            Self::Clones => "clones",
            Self::IsClonedBy => "is_cloned_by",
            Self::Causes => "causes",
            Self::IsCausedBy => "is_caused_by",
        }
    }

    /// Parses the stored spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }

    /// The link the *other* card holds back to this one.
    #[must_use]
    pub fn inverse(self) -> Self {
        match self {
            Self::Blocks => Self::IsBlockedBy,
            Self::IsBlockedBy => Self::Blocks,
            Self::RelatesTo => Self::RelatesTo,
            Self::Duplicates => Self::IsDuplicatedBy,
            Self::IsDuplicatedBy => Self::Duplicates,
            Self::Clones => Self::IsClonedBy,
            Self::IsClonedBy => Self::Clones,
            Self::Causes => Self::IsCausedBy,
            Self::IsCausedBy => Self::Causes,
        }
    }

    /// How a human reads it: "blocks", "is blocked by".
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::IsBlockedBy => "is blocked by",
            Self::RelatesTo => "relates to",
            Self::Duplicates => "duplicates",
            Self::IsDuplicatedBy => "is duplicated by",
            Self::Clones => "clones",
            Self::IsClonedBy => "is cloned by",
            Self::Causes => "causes",
            Self::IsCausedBy => "is caused by",
        }
    }
}

/// One outbound link, joined with the card it points at.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardLink {
    /// The row id of this direction.
    pub id: String,
    /// `blocks`, `is_blocked_by`, ….
    pub link_type: String,
    /// The other card's id.
    pub card_id: String,
    /// The other card's key.
    pub card_key: String,
    /// The other card's summary.
    pub card_summary: String,
    /// The other card's status name.
    pub status_name: String,
    /// The other card's status category.
    pub status_category: String,
    /// When the link was made.
    pub created_at: DateTime<Utc>,
}

/// A card's links, outbound direction only (the inverse rows make that complete).
pub async fn list_card_links(db: &Db, card_id: &str) -> AppResult<Vec<CardLink>> {
    Ok(sqlx::query_as::<_, CardLink>(
        "SELECT l.id, l.link_type, c.id AS card_id, c.key AS card_key, \
                c.summary AS card_summary, s.name AS status_name, s.category AS status_category, \
                l.created_at \
           FROM card_links l \
           JOIN cards c ON c.id = l.to_card_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE l.from_card_id = ? AND c.deleted_at IS NULL \
          ORDER BY l.link_type, c.key",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Links `from` to `to`, and `to` back to `from` with the inverse type.
///
/// Idempotent: linking the same pair the same way twice leaves one pair of rows.
pub async fn link_cards(
    tx: &mut sqlx::SqliteConnection,
    from_card_id: &str,
    to_card_id: &str,
    link_type: LinkType,
    now: DateTime<Utc>,
) -> AppResult<String> {
    if from_card_id == to_card_id {
        return Err(AppError::Validation(
            "A card cannot be linked to itself.".to_owned(),
        ));
    }
    let timestamp = to_sql_timestamp(now);
    for (from, to, kind) in [
        (from_card_id, to_card_id, link_type),
        (to_card_id, from_card_id, link_type.inverse()),
    ] {
        sqlx::query(
            "INSERT INTO card_links (id, from_card_id, to_card_id, link_type, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (from_card_id, to_card_id, link_type) DO NOTHING",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(from)
        .bind(to)
        .bind(kind.as_str())
        .bind(&timestamp)
        .execute(&mut *tx)
        .await?;
    }
    let id: String = sqlx::query_scalar(
        "SELECT id FROM card_links WHERE from_card_id = ? AND to_card_id = ? AND link_type = ?",
    )
    .bind(from_card_id)
    .bind(to_card_id)
    .bind(link_type.as_str())
    .fetch_one(&mut *tx)
    .await?;
    Ok(id)
}

/// Removes a link by the id of either direction, taking its inverse with it.
///
/// Returns `false` if `link_id` is not one of `card_id`'s outbound links.
pub async fn unlink_cards(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    link_id: &str,
) -> AppResult<bool> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT from_card_id, to_card_id, link_type FROM card_links \
         WHERE id = ? AND from_card_id = ?",
    )
    .bind(link_id)
    .bind(card_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((from, to, kind)) = row else {
        return Ok(false);
    };
    let inverse = LinkType::parse(&kind).map_or(kind.clone(), |k| k.inverse().as_str().to_owned());
    sqlx::query(
        "DELETE FROM card_links WHERE (from_card_id = ? AND to_card_id = ? AND link_type = ?) \
            OR (from_card_id = ? AND to_card_id = ? AND link_type = ?)",
    )
    .bind(&from)
    .bind(&to)
    .bind(&kind)
    .bind(&to)
    .bind(&from)
    .bind(&inverse)
    .execute(&mut *tx)
    .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Remote links
// ---------------------------------------------------------------------------

/// Longest accepted URL.
pub const MAX_URL: usize = 2048;
/// Longest accepted title.
pub const MAX_TITLE: usize = 255;

/// A URL attached to a card.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoteLink {
    /// UUID v7.
    pub id: String,
    /// The card.
    pub card_id: String,
    /// An `http(s)` URL.
    pub url: String,
    /// What to show instead of the URL.
    pub title: String,
    /// An optional icon name or URL.
    pub icon: Option<String>,
    /// Who added it.
    pub created_by: Option<String>,
    /// When it was added.
    pub created_at: DateTime<Utc>,
}

/// Checks a remote link's URL: `http` or `https` only.
///
/// The scheme allowlist is the whole point. A `javascript:` URL rendered as an `<a href>` is
/// stored XSS that fires on click; `data:` and `file:` are the same idea with different
/// payloads. Nothing but the two web schemes can be a legitimate remote link.
pub fn validate_url(url: &str) -> AppResult<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().count() > MAX_URL {
        return Err(AppError::Validation(format!(
            "A link must be a URL of at most {MAX_URL} characters."
        )));
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or_else(|| {
            AppError::Validation("A link must start with http:// or https://.".to_owned())
        })?;
    if rest.is_empty() || rest.starts_with('/') || url.chars().any(char::is_control) {
        return Err(AppError::Validation(format!("{url:?} is not a valid URL.")));
    }
    Ok(url.to_owned())
}

/// Checks a remote link's title; an empty title falls back to the URL.
pub fn validate_title(title: Option<&str>, url: &str) -> AppResult<String> {
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(url);
    if title.chars().count() > MAX_TITLE {
        return Err(AppError::Validation(format!(
            "A link title must be at most {MAX_TITLE} characters long."
        )));
    }
    Ok(title.to_owned())
}

/// A card's remote links, oldest first.
pub async fn list_remote(db: &Db, card_id: &str) -> AppResult<Vec<RemoteLink>> {
    Ok(sqlx::query_as::<_, RemoteLink>(
        "SELECT id, card_id, url, title, icon, created_by, created_at FROM remote_links \
         WHERE card_id = ? ORDER BY created_at, id",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Adds a remote link. The URL and title must already be validated.
pub async fn add_remote(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    url: &str,
    title: &str,
    icon: Option<&str>,
    created_by: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<RemoteLink> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO remote_links (id, card_id, url, title, icon, created_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(card_id)
    .bind(url)
    .bind(title)
    .bind(icon)
    .bind(created_by)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    sqlx::query_as::<_, RemoteLink>(
        "SELECT id, card_id, url, title, icon, created_by, created_at FROM remote_links \
         WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await
    .map_err(Into::into)
}

/// Removes a remote link from a card. `false` if it was not on that card.
pub async fn remove_remote(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    id: &str,
) -> AppResult<bool> {
    let done = sqlx::query("DELETE FROM remote_links WHERE id = ? AND card_id = ?")
        .bind(id)
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    Ok(done.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// Watchers
// ---------------------------------------------------------------------------

/// Someone watching a card.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Watcher {
    /// The user's id.
    pub user_id: String,
    /// Their username.
    pub username: String,
    /// Their display name.
    pub display_name: String,
    /// Since when.
    pub created_at: DateTime<Utc>,
}

/// A card's watchers, in the order they started watching.
pub async fn watchers(db: &Db, card_id: &str) -> AppResult<Vec<Watcher>> {
    Ok(sqlx::query_as::<_, Watcher>(
        "SELECT w.user_id, u.username, u.display_name, w.created_at \
           FROM watchers w JOIN users u ON u.id = w.user_id \
          WHERE w.card_id = ? ORDER BY w.created_at, u.username",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// The ids of a card's watchers, inside a transaction — the notification fan-out's view.
pub async fn watcher_ids(db: &Db, card_id: &str) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM watchers WHERE card_id = ?")
            .bind(card_id)
            .fetch_all(db.reader())
            .await?,
    )
}

/// Starts watching. Idempotent.
pub async fn watch(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    user_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO watchers (card_id, user_id, created_at) VALUES (?, ?, ?) \
         ON CONFLICT (card_id, user_id) DO NOTHING",
    )
    .bind(card_id)
    .bind(user_id)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Stops watching. Idempotent.
pub async fn unwatch(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM watchers WHERE card_id = ? AND user_id = ?")
        .bind(card_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_link_type_round_trips_and_inverts_back() {
        for kind in LinkType::ALL {
            assert_eq!(LinkType::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.inverse().inverse(), kind);
        }
        assert_eq!(LinkType::RelatesTo.inverse(), LinkType::RelatesTo);
    }

    #[test]
    fn only_web_urls_are_links() {
        assert!(validate_url("https://example.com/a?b=c").is_ok());
        assert!(validate_url("  http://localhost:3000  ").is_ok());
        for bad in [
            "",
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "data:text/html,<script>",
            "file:///etc/passwd",
            "ftp://example.com",
            "https://",
            "https:///path",
            "example.com",
            "https://a\nb",
        ] {
            assert!(validate_url(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_missing_title_falls_back_to_the_url() {
        assert_eq!(validate_title(None, "https://x.y").unwrap(), "https://x.y");
        assert_eq!(
            validate_title(Some("  "), "https://x.y").unwrap(),
            "https://x.y"
        );
        assert_eq!(
            validate_title(Some(" Spec "), "https://x.y").unwrap(),
            "Spec"
        );
        assert!(validate_title(Some(&"a".repeat(MAX_TITLE + 1)), "https://x.y").is_err());
    }
}

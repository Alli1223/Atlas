//! Components and milestones: a project's areas, and the dates it is working towards.
//!
//! Both are plain per-project lists, named uniquely within the project (case-insensitively —
//! the columns are `COLLATE NOCASE`). A component is many-to-many with cards; a milestone is
//! single-valued (`cards.milestone_id`), because `milestone` is one of AQL's six
//! history-indexed fields and history needs one "from" and one "to".
//!
//! Neither concept assumes software. "Frontend"/"Backend" are components, so are
//! "Rigging"/"Texturing" and "Recruiters"/"Direct applications"; "v1.2" is a milestone, so is
//! "Client delivery #3".

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// Longest accepted component or milestone name.
pub const MAX_NAME: usize = 80;
/// Longest accepted description.
pub const MAX_DESCRIPTION: usize = 4 * 1024;

/// Checks a component/milestone name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "A name must be between 1 and {MAX_NAME} characters long."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "A name must not contain control characters.".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

/// Checks an optional description.
pub fn validate_description(description: Option<&str>) -> AppResult<Option<String>> {
    let Some(text) = description.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > MAX_DESCRIPTION {
        return Err(AppError::Validation(format!(
            "A description must be at most {MAX_DESCRIPTION} characters long."
        )));
    }
    Ok(Some(text.to_owned()))
}

/// Maps a UNIQUE violation on `(project_id, name)` to a 409 the caller can read.
fn name_conflict(err: sqlx::Error, kind: &str, name: &str) -> AppError {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => AppError::Conflict(format!(
            "This project already has a {kind} called {name:?}."
        )),
        _ => err.into(),
    }
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// A named area of a project.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    /// UUID v7.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// Unique within the project, case-insensitively.
    pub name: String,
    /// What the area covers.
    pub description: Option<String>,
    /// The area's lead.
    pub lead_id: Option<String>,
    /// How many live cards are in it.
    pub card_count: i64,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// A project's components, by name.
pub async fn list_components(db: &Db, project_id: &str) -> AppResult<Vec<Component>> {
    Ok(sqlx::query_as::<_, Component>(
        "SELECT c.id, c.project_id, c.name, c.description, c.lead_id, \
           (SELECT COUNT(*) FROM card_components cc JOIN cards k ON k.id = cc.card_id \
             WHERE cc.component_id = c.id AND k.deleted_at IS NULL) AS card_count, \
           c.created_at, c.updated_at FROM components c \
         WHERE c.project_id = ? ORDER BY c.name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a component inside a transaction.
pub async fn find_component_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Component>> {
    Ok(sqlx::query_as::<_, Component>(
        "SELECT c.id, c.project_id, c.name, c.description, c.lead_id, \
           (SELECT COUNT(*) FROM card_components cc JOIN cards k ON k.id = cc.card_id \
             WHERE cc.component_id = c.id AND k.deleted_at IS NULL) AS card_count, \
           c.created_at, c.updated_at FROM components c WHERE c.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Creates a component.
pub async fn insert_component(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    description: Option<&str>,
    lead_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Component> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "INSERT INTO components (id, project_id, name, description, lead_id, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(description)
    .bind(lead_id)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_conflict(err, "component", name))?;
    find_component_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the component just made is missing")))
}

/// Renames / redescribes / re-leads a component.
#[allow(clippy::option_option)]
pub async fn update_component(
    tx: &mut sqlx::SqliteConnection,
    current: &Component,
    name: Option<&str>,
    description: Option<Option<String>>,
    lead_id: Option<Option<String>>,
    now: DateTime<Utc>,
) -> AppResult<Component> {
    let name = name.unwrap_or(&current.name);
    let description = description.unwrap_or_else(|| current.description.clone());
    let lead_id = lead_id.unwrap_or_else(|| current.lead_id.clone());
    sqlx::query(
        "UPDATE components SET name = ?, description = ?, lead_id = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(name)
    .bind(&description)
    .bind(&lead_id)
    .bind(to_sql_timestamp(now))
    .bind(&current.id)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_conflict(err, "component", name))?;
    find_component_tx(&mut *tx, &current.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the component just edited is missing")))
}

/// Deletes a component. Its cards keep everything else; only the membership goes.
pub async fn delete_component(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM components WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The components a card is in.
pub async fn card_components(db: &Db, card_id: &str) -> AppResult<Vec<Component>> {
    Ok(sqlx::query_as::<_, Component>(
        "SELECT c.id, c.project_id, c.name, c.description, c.lead_id, \
           (SELECT COUNT(*) FROM card_components cc2 JOIN cards k ON k.id = cc2.card_id \
             WHERE cc2.component_id = c.id AND k.deleted_at IS NULL) AS card_count, \
           c.created_at, c.updated_at \
           FROM components c JOIN card_components cc ON cc.component_id = c.id \
          WHERE cc.card_id = ? ORDER BY c.name",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Replaces the set of components a card is in. Every id must belong to `project_id`.
pub async fn set_card_components(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    project_id: &str,
    component_ids: &[String],
) -> AppResult<()> {
    for id in component_ids {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM components WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if owner.as_deref() != Some(project_id) {
            return Err(AppError::Validation(format!(
                "{id:?} is not a component of this card's project."
            )));
        }
    }
    sqlx::query("DELETE FROM card_components WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;
    for id in component_ids {
        sqlx::query(
            "INSERT INTO card_components (card_id, component_id) VALUES (?, ?) \
             ON CONFLICT DO NOTHING",
        )
        .bind(card_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

/// Where a milestone is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    /// Still being worked towards.
    Planned,
    /// Shipped / delivered / done.
    Released,
    /// Out of the way, but kept for history.
    Archived,
}

impl MilestoneStatus {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Released => "released",
            Self::Archived => "archived",
        }
    }
}

/// A date a project is working towards.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    /// UUID v7.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// Unique within the project.
    pub name: String,
    /// What it is.
    pub description: Option<String>,
    /// When work towards it starts.
    pub start_date: Option<NaiveDate>,
    /// When it is due.
    pub release_date: Option<NaiveDate>,
    /// `planned`, `released`, `archived`.
    pub status: String,
    /// When it was released.
    pub released_at: Option<DateTime<Utc>>,
    /// Live cards scheduled for it.
    pub card_count: i64,
    /// …of which resolved.
    pub done_count: i64,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// A project's milestones: planned by date, then released, then archived.
pub async fn list_milestones(db: &Db, project_id: &str) -> AppResult<Vec<Milestone>> {
    Ok(sqlx::query_as::<_, Milestone>(
        "SELECT m.id, m.project_id, m.name, m.description, m.start_date, m.release_date, \
                m.status, m.released_at, \
                (SELECT COUNT(*) FROM cards k WHERE k.milestone_id = m.id \
                   AND k.deleted_at IS NULL) AS card_count, \
                (SELECT COUNT(*) FROM cards k WHERE k.milestone_id = m.id \
                   AND k.deleted_at IS NULL AND k.resolution_id IS NOT NULL) AS done_count, \
                m.created_at, m.updated_at \
           FROM milestones m WHERE m.project_id = ? \
          ORDER BY CASE m.status WHEN 'planned' THEN 0 WHEN 'released' THEN 1 ELSE 2 END, \
                   m.release_date IS NULL, m.release_date, m.name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a milestone inside a transaction.
pub async fn find_milestone_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Milestone>> {
    Ok(sqlx::query_as::<_, Milestone>(
        "SELECT m.id, m.project_id, m.name, m.description, m.start_date, m.release_date, \
                m.status, m.released_at, \
                (SELECT COUNT(*) FROM cards k WHERE k.milestone_id = m.id \
                   AND k.deleted_at IS NULL) AS card_count, \
                (SELECT COUNT(*) FROM cards k WHERE k.milestone_id = m.id \
                   AND k.deleted_at IS NULL AND k.resolution_id IS NOT NULL) AS done_count, \
                m.created_at, m.updated_at \
           FROM milestones m WHERE m.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// The fields of a milestone a create or edit may set.
#[allow(clippy::option_option)]
#[derive(Debug, Default)]
pub struct MilestonePatch {
    /// Already validated.
    pub name: Option<String>,
    /// Absent leaves it, `None` clears it.
    pub description: Option<Option<String>>,
    /// Absent leaves it, `None` clears it.
    pub start_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `None` clears it.
    pub release_date: Option<Option<NaiveDate>>,
    /// A status change. `released` stamps `released_at`; leaving `released` clears it.
    pub status: Option<MilestoneStatus>,
}

/// Creates a milestone.
pub async fn insert_milestone(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    patch: &MilestonePatch,
    now: DateTime<Utc>,
) -> AppResult<Milestone> {
    check_dates(patch.start_date.flatten(), patch.release_date.flatten())?;
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "INSERT INTO milestones (id, project_id, name, description, start_date, release_date, \
         status, released_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'planned', NULL, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(patch.description.clone().flatten())
    .bind(patch.start_date.flatten())
    .bind(patch.release_date.flatten())
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_conflict(err, "milestone", name))?;
    find_milestone_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the milestone just made is missing")))
}

fn check_dates(start: Option<NaiveDate>, release: Option<NaiveDate>) -> AppResult<()> {
    if let (Some(start), Some(release)) = (start, release)
        && release < start
    {
        return Err(AppError::Validation(
            "A milestone's release date must not be before its start date.".to_owned(),
        ));
    }
    Ok(())
}

/// Edits a milestone.
pub async fn update_milestone(
    tx: &mut sqlx::SqliteConnection,
    current: &Milestone,
    patch: &MilestonePatch,
    now: DateTime<Utc>,
) -> AppResult<Milestone> {
    let name = patch.name.clone().unwrap_or_else(|| current.name.clone());
    let description = patch
        .description
        .clone()
        .unwrap_or_else(|| current.description.clone());
    let start_date = patch.start_date.unwrap_or(current.start_date);
    let release_date = patch.release_date.unwrap_or(current.release_date);
    check_dates(start_date, release_date)?;
    let status = patch
        .status
        .map_or(current.status.clone(), |s| s.as_str().to_owned());
    let released_at = match (status.as_str(), current.released_at) {
        ("released", Some(at)) => Some(at),
        ("released", None) => Some(now),
        // Archiving a released milestone keeps when it shipped.
        ("archived", at) => at,
        _ => None,
    };
    sqlx::query(
        "UPDATE milestones SET name = ?, description = ?, start_date = ?, release_date = ?, \
         status = ?, released_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&description)
    .bind(start_date)
    .bind(release_date)
    .bind(&status)
    .bind(released_at.map(to_sql_timestamp))
    .bind(to_sql_timestamp(now))
    .bind(&current.id)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_conflict(err, "milestone", &name))?;
    find_milestone_tx(&mut *tx, &current.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the milestone just edited is missing")))
}

/// Deletes a milestone. Its cards are un-scheduled (`ON DELETE SET NULL`), not deleted.
pub async fn delete_milestone(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM milestones WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_bounded_and_printable() {
        assert_eq!(validate_name("  Rigging ").unwrap(), "Rigging");
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
        assert!(validate_name("a\u{0007}b").is_err());
    }

    #[test]
    fn a_release_date_cannot_precede_the_start() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
        assert!(check_dates(d("2026-01-01"), d("2026-02-01")).is_ok());
        assert!(check_dates(d("2026-02-01"), d("2026-01-01")).is_err());
        assert!(check_dates(None, d("2026-01-01")).is_ok());
    }
}

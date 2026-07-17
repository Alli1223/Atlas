//! `card_history` — the changelog.
//!
//! # Why this module exists at all
//!
//! History is **unreconstructable retroactively**. Every other table can be
//! backfilled, recomputed or migrated; this one cannot, because the information
//! it holds only exists at the instant of the change. If the row is not written
//! now, "when did this move to Done?" has no answer, forever. That is why
//! `TODO.md` §D lists the changelog as one of three things to build *with* the
//! card table rather than after it, and why this is not a listener, a queue, or
//! a background job: it is written in the same transaction as the change, by
//! [`crate::domain::card::update`], which diffs the row so a handler cannot
//! forget.
//!
//! # Raw and display, and why both
//!
//! Each row carries the id **and** the name-as-it-was:
//!
//! - `from_value` / `to_value` — the id. Stable across renames; what a query
//!   matches on.
//! - `from_display` / `to_display` — the name at the time. What a human reads.
//!
//! Neither derives from the other after the fact. Keep only ids and the history
//! tab renders "assignee → ?" the day someone is deactivated. Keep only names
//! and `status CHANGED TO "Done"` silently stops matching the day someone
//! renames the status. Jira stores both, and this is the reason.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

/// A card field that gets a changelog entry.
///
/// The spellings here are the **logical** field names — `status`, not
/// `status_id`. That is deliberate: this is the vocabulary Phase 6's
/// `status CHANGED FROM "In Progress" TO "Done" AFTER -7d` will match against,
/// and `status_id CHANGED` is not a query anyone would write.
///
/// A closed enum rather than free strings at the call sites, for the same reason
/// [`crate::auth::events::Kind`] is one: the column is free text, but a typo in
/// a field name silently breaks every query that filters on it and nothing ever
/// tells you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// The one-line title.
    Summary,
    /// The markdown body.
    Description,
    /// The card type.
    Type,
    /// The parent card. Written by a reparent.
    Parent,
    /// The project. Written by a cross-project move, alongside `Key`.
    Project,
    /// The card key. Written by a cross-project move.
    Key,
    /// The workflow status.
    Status,
    /// The priority.
    Priority,
    /// Who is doing it.
    Assignee,
    /// Who asked for it.
    Reporter,
    /// Why it stopped. The load-bearing one — see docs/adr §E.
    Resolution,
    /// When it stopped.
    ResolvedAt,
    /// The due date.
    DueDate,
    /// The start date.
    StartDate,
    /// The estimate.
    Estimate,
    /// The board sort key.
    Rank,
    /// Archival.
    Archived,
    /// Soft deletion — the trash.
    Deleted,
}

impl Field {
    /// The field's spelling in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Description => "description",
            Self::Type => "type",
            Self::Parent => "parent",
            Self::Project => "project",
            Self::Key => "key",
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Assignee => "assignee",
            Self::Reporter => "reporter",
            Self::Resolution => "resolution",
            Self::ResolvedAt => "resolved_at",
            Self::DueDate => "due_date",
            Self::StartDate => "start_date",
            Self::Estimate => "estimate",
            Self::Rank => "rank",
            Self::Archived => "archived",
            Self::Deleted => "deleted",
        }
    }
}

/// One field's before and after, ready to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Which field moved.
    pub field: Field,
    /// The old id / raw value.
    pub from_value: Option<String>,
    /// The old value as a human saw it, at the time.
    pub from_display: Option<String>,
    /// The new id / raw value.
    pub to_value: Option<String>,
    /// The new value as a human sees it now.
    pub to_display: Option<String>,
}

impl Change {
    /// A change whose raw value *is* what a human reads: a summary, a date, a
    /// number. Both columns get the same string.
    ///
    /// Storing the display copy even here, where it is redundant today, is
    /// deliberate: it means every consumer reads `to_display` unconditionally
    /// rather than branching on which fields happen to be references, and the
    /// branch is what would rot the first time a field changes shape.
    pub fn plain(field: Field, from: Option<String>, to: Option<String>) -> Self {
        Self {
            field,
            from_display: from.clone(),
            from_value: from,
            to_display: to.clone(),
            to_value: to,
        }
    }

    /// A change to a referenced entity: the id in `*_value`, the name in
    /// `*_display`.
    pub fn reference(
        field: Field,
        from: Option<(String, String)>,
        to: Option<(String, String)>,
    ) -> Self {
        let (from_value, from_display) = split(from);
        let (to_value, to_display) = split(to);
        Self {
            field,
            from_value,
            from_display,
            to_value,
            to_display,
        }
    }
}

fn split(pair: Option<(String, String)>) -> (Option<String>, Option<String>) {
    match pair {
        Some((value, display)) => (Some(value), Some(display)),
        None => (None, None),
    }
}

/// A row of `card_history`, as the API describes it.
///
/// Both raw and display are on the wire. The client renders `*Display` and
/// filters on `*Value`; sending only one would push the choice this table exists
/// to avoid onto every consumer.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// UUID v7, as text.
    pub id: String,
    /// Which card.
    pub card_id: String,
    /// Who did it. `None` for an automation or an agent.
    pub author_id: Option<String>,
    /// When.
    pub created_at: DateTime<Utc>,
    /// The logical field name: `status`, `assignee`.
    pub field: String,
    /// The old id / raw value.
    pub from_value: Option<String>,
    /// The old value as a human saw it, at the time.
    pub from_display: Option<String>,
    /// The new id / raw value.
    pub to_value: Option<String>,
    /// The new value as a human saw it, at the time.
    pub to_display: Option<String>,
}

/// Appends changes to a card's history, inside the caller's transaction.
///
/// Takes a transaction and **not** the pool, and returns a `Result` it does not
/// swallow — both are deliberate departures from
/// [`crate::auth::events::record`], which does the opposite. The difference is
/// what the two logs are for. An auth event is a security observation *about* a
/// request: losing one must not fail the login it describes. A history row is
/// part of the change itself: a status transition that is not in the changelog
/// did not really happen as far as every report, query and audit is concerned,
/// so if this write fails the transition must roll back with it.
pub async fn record(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    author_id: Option<&str>,
    changes: &[Change],
    now: DateTime<Utc>,
) -> AppResult<()> {
    let timestamp = to_sql_timestamp(now);

    for change in changes {
        sqlx::query(
            "INSERT INTO card_history (id, card_id, author_id, created_at, field, \
             from_value, from_display, to_value, to_display) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(card_id)
        .bind(author_id)
        .bind(&timestamp)
        .bind(change.field.as_str())
        .bind(&change.from_value)
        .bind(&change.from_display)
        .bind(&change.to_value)
        .bind(&change.to_display)
        .execute(&mut *tx)
        .await?;
    }

    Ok(())
}

/// A card's history, oldest first.
pub async fn list(db: &Db, card_id: &str) -> AppResult<Vec<HistoryEntry>> {
    Ok(sqlx::query_as::<_, HistoryEntry>(
        "SELECT id, card_id, author_id, created_at, field, from_value, from_display, \
         to_value, to_display FROM card_history \
         WHERE card_id = ? ORDER BY created_at, id",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_a_distinct_spelling() {
        // Two fields sharing a spelling would silently merge their histories,
        // and every query over either would return both.
        let fields = [
            Field::Summary,
            Field::Description,
            Field::Type,
            Field::Parent,
            Field::Project,
            Field::Key,
            Field::Status,
            Field::Priority,
            Field::Assignee,
            Field::Reporter,
            Field::Resolution,
            Field::ResolvedAt,
            Field::DueDate,
            Field::StartDate,
            Field::Estimate,
            Field::Rank,
            Field::Archived,
            Field::Deleted,
        ];
        let mut spellings: Vec<&str> = fields.iter().map(|f| f.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();
        assert_eq!(spellings.len(), count, "two fields share a spelling");
    }

    #[test]
    fn the_six_fields_phase_six_queries_are_spelled_logically() {
        // `status CHANGED FROM "In Progress"`, not `status_id CHANGED`. TODO.md
        // §6 scopes WAS/CHANGED to these; if a spelling drifts, every one of
        // those queries silently matches nothing.
        assert_eq!(Field::Status.as_str(), "status");
        assert_eq!(Field::Assignee.as_str(), "assignee");
        assert_eq!(Field::Priority.as_str(), "priority");
        assert_eq!(Field::Reporter.as_str(), "reporter");
        assert_eq!(Field::Resolution.as_str(), "resolution");
    }

    #[test]
    fn a_plain_change_stores_the_same_text_in_both_columns() {
        let change = Change::plain(
            Field::Summary,
            Some("Old".to_owned()),
            Some("New".to_owned()),
        );
        assert_eq!(change.from_value.as_deref(), Some("Old"));
        assert_eq!(change.from_display.as_deref(), Some("Old"));
        assert_eq!(change.to_value.as_deref(), Some("New"));
        assert_eq!(change.to_display.as_deref(), Some("New"));
    }

    #[test]
    fn a_reference_change_keeps_the_id_and_the_name_apart() {
        // The whole point of the two columns: the id survives a rename, the name
        // survives a deletion.
        let change = Change::reference(
            Field::Status,
            Some(("s1".to_owned(), "To Do".to_owned())),
            Some(("s2".to_owned(), "Done".to_owned())),
        );
        assert_eq!(change.from_value.as_deref(), Some("s1"));
        assert_eq!(change.from_display.as_deref(), Some("To Do"));
        assert_eq!(change.to_value.as_deref(), Some("s2"));
        assert_eq!(change.to_display.as_deref(), Some("Done"));
    }

    #[test]
    fn clearing_a_reference_nulls_both_columns() {
        let change = Change::reference(
            Field::Assignee,
            Some(("u1".to_owned(), "Alastair".to_owned())),
            None,
        );
        assert_eq!(change.to_value, None);
        assert_eq!(change.to_display, None);
    }
}

//! The `cards` row and every mutation of it.
//!
//! # The one function that matters: [`update`]
//!
//! Everything that changes a card goes through it, and it **diffs the row and
//! writes the changelog itself**. That is not tidiness; it is the only design
//! that survives contact with a growing codebase.
//!
//! `TODO.md` is explicit: history is written on every field change, in the same
//! transaction as the change — "not a listener, not later". The alternatives all
//! fail in the same way:
//!
//! - *Each handler writes its own history rows.* Handler number nine forgets
//!   one field. Nothing breaks, no test fails, and the changelog is quietly
//!   wrong from that day on. Nobody finds out until someone asks the question
//!   history existed to answer.
//! - *An event listener writes them afterwards.* Now the change and its record
//!   are in different transactions, so a crash between them loses the record
//!   permanently — and history, unlike everything else, cannot be recomputed.
//!
//! So there is one door. Hand it the old row and a [`CardPatch`]; it works out
//! what actually moved, applies the resolution rules, writes the row, and writes
//! the history — atomically. A caller *cannot* forget, because there is nothing
//! to remember.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::history::{self, Change, Field};
use crate::domain::project::{self, Project};
use crate::domain::workflow::{self, Outcome, Transition};
use crate::domain::{config, hierarchy};
use crate::error::{AppError, AppResult};
use crate::rank::Rank;

/// Longest accepted card summary, in characters.
pub const MAX_SUMMARY: usize = 255;

/// Longest accepted card description, in characters.
pub const MAX_DESCRIPTION: usize = 64 * 1024;

/// The most cards one `GET .../cards` will return.
pub const MAX_PAGE_SIZE: i64 = 200;

/// The default page size.
pub const DEFAULT_PAGE_SIZE: i64 = 50;

// The display lookups the changelog needs. `&'static str`, so they satisfy sqlx
// 0.9's `SqlSafeStr` bound without `AssertSqlSafe`.
const TYPE_NAME_SQL: &str = "SELECT name FROM card_types WHERE id = ?";
const STATUS_NAME_SQL: &str = "SELECT name FROM statuses WHERE id = ?";
const PRIORITY_NAME_SQL: &str = "SELECT name FROM priorities WHERE id = ?";
const RESOLUTION_NAME_SQL: &str = "SELECT name FROM resolutions WHERE id = ?";
const USER_NAME_SQL: &str = "SELECT display_name FROM users WHERE id = ?";
const CARD_KEY_SQL: &str = "SELECT key FROM cards WHERE id = ?";

/// A row of `cards`, exactly as stored.
#[derive(Debug, Clone, FromRow)]
pub struct Card {
    /// UUID v7, as text.
    pub id: String,
    /// `ATLAS-123`. Unique across the instance.
    pub key: String,
    /// The owning project.
    pub project_id: String,
    /// The card type, which is also what fixes the card's hierarchy level.
    pub type_id: String,
    /// The parent card. `None` = a root of the project's tree.
    ///
    /// This one nullable column is the whole nested-board feature.
    pub parent_id: Option<String>,
    /// The one-line title.
    pub summary: String,
    /// Markdown **source**. Never rendered HTML.
    pub description: Option<String>,
    /// The workflow status.
    pub status_id: String,
    /// The priority.
    pub priority_id: Option<String>,
    /// Who is doing it.
    pub assignee_id: Option<String>,
    /// Who asked for it.
    pub reporter_id: Option<String>,
    /// Who created it. Immutable.
    pub creator_id: String,
    /// Why it stopped. **A card is resolved iff this is set** — see docs/adr §E.
    pub resolution_id: Option<String>,
    /// When it was resolved. Kept in lockstep with `resolution_id`.
    pub resolved_at: Option<DateTime<Utc>>,
    /// The due date.
    pub due_date: Option<NaiveDate>,
    /// The start date.
    pub start_date: Option<NaiveDate>,
    /// The estimate, interpreted through the project's `estimation_unit`.
    pub estimate: Option<f64>,
    /// The lexicographic board sort key.
    pub rank: Rank,
    /// When it was archived. `None` = live.
    pub archived_at: Option<DateTime<Utc>>,
    /// When it was moved to the trash. `None` = not deleted.
    pub deleted_at: Option<DateTime<Utc>>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl Card {
    /// Whether the card is resolved.
    ///
    /// **This, and never "is it in a Done status".** The two are kept in sync by
    /// [`update`], but this is the definition — the one Jira uses everywhere and
    /// then fails to maintain, which is the confusion docs/adr §E exists to kill.
    pub fn is_resolved(&self) -> bool {
        self.resolution_id.is_some()
    }

    /// Whether the card is in the trash.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// A card as the API describes it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardDto {
    /// UUID v7, as text.
    pub id: String,
    /// `ATLAS-123`.
    #[schema(example = "ATLAS-123")]
    pub key: String,
    /// The owning project.
    pub project_id: String,
    /// The card type.
    pub type_id: String,
    /// The parent card, if any.
    pub parent_id: Option<String>,
    /// The one-line title.
    pub summary: String,
    /// Markdown source. The client renders and sanitises it.
    pub description: Option<String>,
    /// The workflow status.
    pub status_id: String,
    /// The priority.
    pub priority_id: Option<String>,
    /// Who is doing it.
    pub assignee_id: Option<String>,
    /// Who asked for it.
    pub reporter_id: Option<String>,
    /// Who created it.
    pub creator_id: String,
    /// Why it stopped.
    pub resolution_id: Option<String>,
    /// Whether the card is resolved. Derived, and sent explicitly so no client
    /// has to re-derive the rule that docs/adr §E exists to centralise.
    pub resolved: bool,
    /// When it was resolved.
    pub resolved_at: Option<DateTime<Utc>>,
    /// The due date.
    pub due_date: Option<NaiveDate>,
    /// The start date.
    pub start_date: Option<NaiveDate>,
    /// The estimate.
    pub estimate: Option<f64>,
    /// The board sort key.
    ///
    /// `value_type = String` because [`Rank`] is `#[serde(transparent)]` over a
    /// `String` and has no `ToSchema` impl of its own — it lives in `rank.rs`,
    /// which predates the OpenAPI surface. This is what it actually serialises
    /// as, so the generated TypeScript client gets the truth.
    #[schema(value_type = String, example = "8000")]
    pub rank: Rank,
    /// When it was archived.
    pub archived_at: Option<DateTime<Utc>>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl From<&Card> for CardDto {
    fn from(card: &Card) -> Self {
        Self {
            id: card.id.clone(),
            key: card.key.clone(),
            project_id: card.project_id.clone(),
            type_id: card.type_id.clone(),
            parent_id: card.parent_id.clone(),
            summary: card.summary.clone(),
            description: card.description.clone(),
            status_id: card.status_id.clone(),
            priority_id: card.priority_id.clone(),
            assignee_id: card.assignee_id.clone(),
            reporter_id: card.reporter_id.clone(),
            creator_id: card.creator_id.clone(),
            resolution_id: card.resolution_id.clone(),
            resolved: card.is_resolved(),
            resolved_at: card.resolved_at,
            due_date: card.due_date,
            start_date: card.start_date,
            estimate: card.estimate,
            rank: card.rank.clone(),
            archived_at: card.archived_at,
            created_at: card.created_at,
            updated_at: card.updated_at,
        }
    }
}

impl From<Card> for CardDto {
    fn from(card: Card) -> Self {
        Self::from(&card)
    }
}

/// Every column of `cards`.
macro_rules! card_columns {
    () => {
        "id, key, project_id, type_id, parent_id, summary, description, status_id, \
         priority_id, assignee_id, reporter_id, creator_id, resolution_id, resolved_at, \
         due_date, start_date, estimate, rank, archived_at, deleted_at, created_at, updated_at"
    };
}

/// The filter shared by the list query and its count.
///
/// One fixed statement with every predicate always present and neutralised by a
/// parameter, rather than a `WHERE` clause assembled from whichever filters the
/// caller supplied. Same reasoning as [`crate::auth::user::apply_patch`]:
/// building SQL from a runtime shape is the habit that produces injection bugs,
/// even in the instances where it would have been safe.
macro_rules! card_filter_where {
    () => {
        " WHERE project_id = ? \
            AND deleted_at IS NULL \
            AND (? OR archived_at IS NULL) \
            AND (? = 'any' \
                 OR (? = 'root' AND parent_id IS NULL) \
                 OR (? = 'card' AND parent_id = ?)) \
            AND (? IS NULL OR status_id = ?) \
            AND (? IS NULL OR assignee_id = ?)"
    };
}

/// Which cards of a project a listing wants, by parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ParentFilter {
    /// Every card, at any depth. The flat project view.
    #[default]
    Any,
    /// Only the roots — the project's top-level board.
    Root,
    /// Only the children of one card: **this is the nested board**.
    Card(String),
}

impl ParentFilter {
    /// The mode string the fixed statement branches on.
    fn mode(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Root => "root",
            Self::Card(_) => "card",
        }
    }

    /// The parent id, when there is one.
    fn id(&self) -> Option<&str> {
        match self {
            Self::Card(id) => Some(id),
            _ => None,
        }
    }
}

/// What `GET /projects/{key}/cards` is asking for.
#[derive(Debug, Clone)]
pub struct CardFilter {
    /// Which slice of the tree.
    pub parent: ParentFilter,
    /// Only this status.
    pub status_id: Option<String>,
    /// Only this assignee.
    pub assignee_id: Option<String>,
    /// Whether archived cards are included. The trash never is.
    pub include_archived: bool,
    /// Page size, capped at [`MAX_PAGE_SIZE`].
    pub limit: i64,
    /// How many to skip.
    pub offset: i64,
}

impl Default for CardFilter {
    fn default() -> Self {
        Self {
            parent: ParentFilter::Any,
            status_id: None,
            assignee_id: None,
            include_archived: false,
            limit: DEFAULT_PAGE_SIZE,
            offset: 0,
        }
    }
}

/// A page of cards, and how many there are in total.
#[derive(Debug)]
pub struct CardPage {
    /// The cards on this page, in rank order.
    pub cards: Vec<Card>,
    /// How many cards match the filter, ignoring the page.
    pub total: i64,
}

/// Where a new card lands in its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// The top of the column. Where a freshly-triaged card belongs.
    Top,
    /// The bottom of the column. Where a backlog item belongs.
    #[default]
    Bottom,
}

/// A new card, ready to insert.
#[derive(Debug)]
pub struct NewCard {
    /// The card type. Must belong to the project, and fixes the card's level.
    pub type_id: String,
    /// The parent, if this card is being created inside another card's board.
    pub parent_id: Option<String>,
    /// The one-line title.
    pub summary: String,
    /// Markdown source.
    pub description: Option<String>,
    /// The status. `None` = the project's first column.
    pub status_id: Option<String>,
    /// The priority.
    pub priority_id: Option<String>,
    /// Who is doing it.
    pub assignee_id: Option<String>,
    /// Who asked for it. `None` = the creator.
    pub reporter_id: Option<String>,
    /// The due date.
    pub due_date: Option<NaiveDate>,
    /// The start date.
    pub start_date: Option<NaiveDate>,
    /// The estimate.
    pub estimate: Option<f64>,
    /// Top or bottom of the column.
    pub placement: Placement,
}

/// The fields [`update`] may change.
///
/// `Option<Option<T>>` on the nullable fields keeps absent (leave alone) and
/// `null` (clear) distinct.
///
/// # What is deliberately not here
///
/// - **`parent_id`.** Reparenting has four guards on it (same project, level
///   ordering, cycles, depth) and it would be a bypass if a plain field edit
///   could set it. [`reparent`] is the only door, and it is the only door
///   *because* it is not a field here.
/// - **`key`, `project_id`, `creator_id`.** The first two move together and only
///   via [`move_to_project`], which has to mint a new key and leave a redirect
///   behind. The third is a fact about the past.
/// - **`resolved_at`.** Derived from `resolution_id` by [`update`] itself; a
///   caller that could set it independently could make a card resolved at a time
///   it was not resolved.
#[allow(clippy::option_option)]
#[derive(Debug, Default)]
pub struct CardPatch {
    /// The one-line title.
    pub summary: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub description: Option<Option<String>>,
    /// The card type. Re-checked against the hierarchy, since type fixes level.
    pub type_id: Option<String>,
    /// The status. Drives the resolution rules.
    pub status_id: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub priority_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub assignee_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub reporter_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub resolution_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub due_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub start_date: Option<Option<NaiveDate>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub estimate: Option<Option<f64>>,
    /// The board sort key. Set by [`move_card`], not by hand.
    pub rank: Option<Rank>,
    /// Archive or unarchive.
    pub archived: Option<bool>,
}

impl CardPatch {
    /// Whether this patch names any field at all.
    ///
    /// Naming a field is not the same as changing it — `{"summary": "same"}` is
    /// not empty but produces no history. [`update`] decides that by diffing.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.description.is_none()
            && self.type_id.is_none()
            && self.status_id.is_none()
            && self.priority_id.is_none()
            && self.assignee_id.is_none()
            && self.reporter_id.is_none()
            && self.resolution_id.is_none()
            && self.due_date.is_none()
            && self.start_date.is_none()
            && self.estimate.is_none()
            && self.rank.is_none()
            && self.archived.is_none()
    }
}

/// Checks a card summary.
pub fn validate_summary(summary: &str) -> AppResult<String> {
    let summary = summary.trim();

    if summary.is_empty() {
        return Err(AppError::Validation(
            "Summary must not be empty.".to_owned(),
        ));
    }
    if summary.chars().count() > MAX_SUMMARY {
        return Err(AppError::Validation(format!(
            "Summary must be at most {MAX_SUMMARY} characters long."
        )));
    }
    // A summary is one line, everywhere it is rendered: board card, breadcrumb,
    // history row, notification subject, agent prompt.
    if summary.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Summary must be a single line, with no control characters.".to_owned(),
        ));
    }

    Ok(summary.to_owned())
}

/// Checks a card description. Markdown, so only the length is bounded.
pub fn validate_description(description: &str) -> AppResult<String> {
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(AppError::Validation(format!(
            "Description must be at most {MAX_DESCRIPTION} characters long."
        )));
    }
    Ok(description.to_owned())
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Finds a card by key.
pub async fn find_by_key(db: &Db, key: &str) -> AppResult<Option<Card>> {
    Ok(sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards WHERE key = ?"
    ))
    .bind(key)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a card by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Card>> {
    Ok(sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a card by id inside an open transaction.
pub async fn find_by_id_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Card>> {
    Ok(sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Finds a card by key inside an open transaction.
pub async fn find_by_key_tx(tx: &mut sqlx::SqliteConnection, key: &str) -> AppResult<Option<Card>> {
    Ok(sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards WHERE key = ?"
    ))
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?)
}

/// How a key resolved.
#[derive(Debug)]
pub enum KeyLookup {
    /// The key is the card's current one.
    Current(Box<Card>),
    /// The key is retired: the card has moved and has a new key.
    ///
    /// The caller is expected to redirect rather than serve the card, so that a
    /// stale bookmark heals itself instead of silently working forever.
    Moved(Box<Card>),
}

/// Resolves a card key, following a retired key to the card that owns it now.
///
/// # Why `card_key_history` exists
///
/// A card key leaks everywhere by design: bookmarks, commit messages, branch
/// names, PR titles, comments, `ATLAS-42` autolinks, and Claude Code prompts.
/// Moving a card between projects has to renumber it — the counter is
/// per-project — so without a redirect table, one tidy-up afternoon 404s every
/// one of those references, permanently and silently.
///
/// The rows never expire. A redirect that stops working after a year is a
/// redirect that was not worth writing.
pub async fn resolve_key(db: &Db, key: &str) -> AppResult<Option<KeyLookup>> {
    if let Some(card) = find_by_key(db, key).await? {
        return Ok(Some(KeyLookup::Current(Box::new(card))));
    }

    let card_id: Option<String> =
        sqlx::query_scalar("SELECT card_id FROM card_key_history WHERE old_key = ?")
            .bind(key)
            .fetch_optional(db.reader())
            .await?;

    let Some(card_id) = card_id else {
        return Ok(None);
    };

    // The FK is ON DELETE CASCADE, so a redirect cannot outlive its card — but a
    // `None` here would be a dangling row rather than a 404 for the client.
    Ok(find_by_id(db, &card_id)
        .await?
        .map(|card| KeyLookup::Moved(Box::new(card))))
}

/// A page of a project's cards, in rank order.
pub async fn list(db: &Db, project_id: &str, filter: &CardFilter) -> AppResult<CardPage> {
    let limit = filter.limit.clamp(1, MAX_PAGE_SIZE);
    let offset = filter.offset.max(0);

    let cards = sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards",
        card_filter_where!(),
        " ORDER BY rank, key LIMIT ? OFFSET ?"
    ))
    .bind(project_id)
    .bind(filter.include_archived)
    .bind(filter.parent.mode())
    .bind(filter.parent.mode())
    .bind(filter.parent.mode())
    .bind(filter.parent.id())
    .bind(filter.status_id.as_deref())
    .bind(filter.status_id.as_deref())
    .bind(filter.assignee_id.as_deref())
    .bind(filter.assignee_id.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(db.reader())
    .await?;

    let total: i64 =
        sqlx::query_scalar(concat!("SELECT COUNT(*) FROM cards", card_filter_where!()))
            .bind(project_id)
            .bind(filter.include_archived)
            .bind(filter.parent.mode())
            .bind(filter.parent.mode())
            .bind(filter.parent.mode())
            .bind(filter.parent.id())
            .bind(filter.status_id.as_deref())
            .bind(filter.status_id.as_deref())
            .bind(filter.assignee_id.as_deref())
            .bind(filter.assignee_id.as_deref())
            .fetch_one(db.reader())
            .await?;

    Ok(CardPage { cards, total })
}

/// A card's children, in rank order. **This is a nested board.**
pub async fn children(db: &Db, parent_id: &str) -> AppResult<Vec<Card>> {
    Ok(sqlx::query_as::<_, Card>(concat!(
        "SELECT ",
        card_columns!(),
        " FROM cards WHERE parent_id = ? AND deleted_at IS NULL ORDER BY rank, key"
    ))
    .bind(parent_id)
    .fetch_all(db.reader())
    .await?)
}

// ---------------------------------------------------------------------------
// Creating
// ---------------------------------------------------------------------------

/// Creates a card.
///
/// The key is allocated inside the caller's transaction by
/// [`crate::domain::project::allocate_card_key`], which is what makes two
/// concurrent creates unable to both get `ATLAS-7`.
///
/// **No history rows are written.** Creation is not a change: the card's initial
/// state *is* the event, and `created_at` + `creator_id` record it. A changelog
/// that opened with eighteen "field set from nothing" rows would bury the first
/// real change under noise. Jira does the same.
// Long, but straight-line: validate each reference, then insert. Splitting the
// validations into a helper would mean passing five loaded rows back out to the
// caller that needs them, which is more moving parts than it removes.
#[allow(clippy::too_many_lines)]
pub async fn create(
    tx: &mut sqlx::SqliteConnection,
    project: &Project,
    new: &NewCard,
    creator_id: &str,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    let card_type = config::find_card_type_tx(&mut *tx, &project.id, &new.type_id)
        .await?
        .ok_or_else(|| {
            AppError::Validation(format!(
                "{:?} is not a card type of project {}.",
                new.type_id, project.key
            ))
        })?;

    let status = match &new.status_id {
        Some(status_id) => config::find_status_tx(&mut *tx, &project.id, status_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{status_id:?} is not a status of project {}.",
                    project.key
                ))
            })?,
        None => config::first_status_tx(&mut *tx, &project.id)
            .await?
            .ok_or_else(|| {
                AppError::Conflict(format!(
                    "Project {} has no statuses, so a card has nowhere to go. Add a status first.",
                    project.key
                ))
            })?,
    };

    if let Some(priority_id) = &new.priority_id {
        config::find_priority_tx(&mut *tx, &project.id, priority_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{priority_id:?} is not a priority of project {}.",
                    project.key
                ))
            })?;
    }

    // Same rule as `update`: creation must refuse a user reference that names
    // nobody, or the FK turns a bad request into a 500.
    if let Some(assignee_id) = &new.assignee_id {
        check_user_exists(&mut *tx, "assigneeId", assignee_id).await?;
    }
    if let Some(reporter_id) = &new.reporter_id {
        check_user_exists(&mut *tx, "reporterId", reporter_id).await?;
    }

    // The hierarchy guards apply to creation exactly as they do to a reparent —
    // a card created *into* a parent is a card being parented. A cycle is
    // impossible here (the card does not exist yet), but the level rule and the
    // depth cap are not.
    if let Some(parent_id) = &new.parent_id {
        let parent = find_by_id_tx(&mut *tx, parent_id)
            .await?
            .ok_or_else(|| AppError::Validation(format!("No card with id {parent_id:?}.")))?;

        if parent.project_id != project.id {
            return Err(AppError::Conflict(format!(
                "{} is in another project.",
                parent.key
            )));
        }

        let parent_level = hierarchy::level_of(&mut *tx, &parent.id).await?;
        if parent_level <= card_type.level {
            return Err(AppError::Conflict(format!(
                "{} sits at hierarchy level {parent_level}, and a {} sits at level {}. A parent \
                 must be at a higher level than its child.",
                parent.key, card_type.name, card_type.level
            )));
        }

        let depth = hierarchy::depth_of(&mut *tx, &parent.id).await? + 1;
        if depth > hierarchy::MAX_DEPTH {
            return Err(AppError::Conflict(format!(
                "That would nest cards {depth} levels deep; the limit is {}.",
                hierarchy::MAX_DEPTH
            )));
        }
    }

    let rank = rank_for_placement(&mut *tx, &project.id, &status.id, new.placement).await?;
    let key = project::allocate_card_key(&mut *tx, &project.id).await?;
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    // A card created directly into a done column is resolved on arrival — the
    // same rule a transition would apply, so that "created as Done" and "created
    // then moved to Done" do not disagree about what resolved means.
    let (resolution_id, resolved_at) = if status.category.is_done() {
        let resolution = config::default_resolution_tx(&mut *tx, &project.id)
            .await?
            .ok_or_else(|| no_resolutions(project, &status.name))?;
        (Some(resolution.id), Some(timestamp.clone()))
    } else {
        (None, None)
    };

    sqlx::query(
        "INSERT INTO cards (id, key, project_id, type_id, parent_id, summary, description, \
         status_id, priority_id, assignee_id, reporter_id, creator_id, resolution_id, \
         resolved_at, due_date, start_date, estimate, rank, archived_at, deleted_at, \
         created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&key)
    .bind(&project.id)
    .bind(&new.type_id)
    .bind(&new.parent_id)
    .bind(&new.summary)
    .bind(&new.description)
    .bind(&status.id)
    .bind(&new.priority_id)
    .bind(&new.assignee_id)
    .bind(new.reporter_id.as_deref().unwrap_or(creator_id))
    .bind(creator_id)
    .bind(&resolution_id)
    .bind(&resolved_at)
    .bind(new.due_date)
    .bind(new.start_date)
    .bind(new.estimate)
    .bind(&rank)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just inserted is missing")))
}

fn no_resolutions(project: &Project, status_name: &str) -> AppError {
    AppError::Conflict(format!(
        "Project {} has no resolutions, so a card cannot be moved to {status_name:?} — a card in \
         a done status must say why it stopped. Add a resolution to the project first.",
        project.key
    ))
}

/// The rank for a card arriving at the top or bottom of a column.
async fn rank_for_placement(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    status_id: &str,
    placement: Placement,
) -> AppResult<Rank> {
    // Both arms are literals, so `sql` is `&'static str` and satisfies
    // `SqlSafeStr` without `AssertSqlSafe`. Nothing is formatted.
    let sql = match placement {
        Placement::Top => {
            "SELECT rank FROM cards WHERE project_id = ? AND status_id = ? \
             AND deleted_at IS NULL ORDER BY rank LIMIT 1"
        }
        Placement::Bottom => {
            "SELECT rank FROM cards WHERE project_id = ? AND status_id = ? \
             AND deleted_at IS NULL ORDER BY rank DESC LIMIT 1"
        }
    };

    let neighbour: Option<Rank> = sqlx::query_scalar(sql)
        .bind(project_id)
        .bind(status_id)
        .fetch_optional(&mut *tx)
        .await?;

    Ok(match (placement, neighbour) {
        (_, None) => Rank::first(),
        (Placement::Top, Some(first)) => Rank::between(None, Some(&first))?,
        (Placement::Bottom, Some(last)) => Rank::between(Some(&last), None)?,
    })
}

// ---------------------------------------------------------------------------
// Updating — the one door
// ---------------------------------------------------------------------------

/// Applies a patch to a card, writing the changelog for whatever actually moved.
///
/// This is the only way a card's fields change. See the module docs for why
/// there is exactly one.
///
/// # What it does, in order
///
/// 1. Validates that every referenced id belongs to the card's project. A
///    foreign key says "a status"; it does not say "a status *of this project*",
///    so without this a board could be handed another project's column.
/// 2. Re-checks the hierarchy if the **type** changed — type fixes level, so
///    changing it can invalidate a parent or child relationship that was legal a
///    moment ago.
/// 3. Applies the resolution rules (docs/adr §E).
/// 4. Diffs old against new, resolving a display name for every reference.
/// 5. Writes the row and the history rows, in the caller's transaction.
///
/// A patch that names fields but changes nothing writes nothing at all — not
/// even `updated_at`. "I sent the same value again" is not an edit.
/// The workflow context of an [`update`] that is a transition.
///
/// Carried only by [`execute_transition`], which pins a *specific* transition
/// (chosen by the user from a screen) and may carry a comment they typed on it.
/// A plain [`update`] passes `None`: a status change there auto-resolves whatever
/// legal transition reaches the target, which is what a board drag or a bare
/// `PATCH statusId` means.
struct TransitionCtx {
    /// The transition the caller pinned, already verified by
    /// [`workflow::verify_transition`].
    explicit: Option<Transition>,
    /// A comment entered on the transition screen, added as the first
    /// post-function.
    comment: Option<String>,
}

pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    patch: &CardPatch,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    update_inner(tx, current, patch, author_id, now, None).await
}

/// Executes a **named** transition on a card: the `POST /cards/{key}/transitions/{id}`
/// path.
///
/// Distinct from a board drag only in that the caller pinned *which* transition
/// (there may be several to the same status, with different post-functions or
/// screens) and may pass a `comment` typed on the transition's screen. The status
/// change, the validators, and the post-functions all run inside `tx`, so a
/// post-function that fails rolls the whole move back.
pub async fn execute_transition(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    transition: &Transition,
    mut patch: CardPatch,
    comment: Option<&str>,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    // Verify up front: belongs to this card's workflow, offered from where the
    // card sits, and not hidden by a condition. A hidden transition is rejected
    // here exactly as if it did not exist.
    workflow::verify_transition(&mut *tx, card, transition, author_id).await?;

    // The target is the transition's, never the caller's — the caller named a
    // transition, not a status.
    patch.status_id = Some(transition.to_status_id.clone());

    update_inner(
        tx,
        card,
        &patch,
        author_id,
        now,
        Some(TransitionCtx {
            explicit: Some(transition.clone()),
            comment: comment.map(ToOwned::to_owned),
        }),
    )
    .await
}

// One `if let` per patchable field, which is what a patch *is*. Splitting it to
// satisfy a line count would scatter the field rules across helpers that each
// have one caller, and `create` above carries the same allow for the same
// reason.
#[allow(clippy::too_many_lines)]
async fn update_inner(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    patch: &CardPatch,
    author_id: Option<&str>,
    now: DateTime<Utc>,
    tctx: Option<TransitionCtx>,
) -> AppResult<Card> {
    let project = project::find_by_id_tx(&mut *tx, &current.project_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let mut next = current.clone();

    if let Some(summary) = &patch.summary {
        next.summary = summary.clone();
    }
    if let Some(description) = &patch.description {
        next.description = description.clone();
    }
    if let Some(type_id) = &patch.type_id {
        config::find_card_type_tx(&mut *tx, &project.id, type_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{type_id:?} is not a card type of project {}.",
                    project.key
                ))
            })?;
        next.type_id = type_id.clone();
    }
    if let Some(status_id) = &patch.status_id {
        config::find_status_tx(&mut *tx, &project.id, status_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{status_id:?} is not a status of project {}.",
                    project.key
                ))
            })?;
        next.status_id = status_id.clone();
    }
    if let Some(priority_id) = &patch.priority_id {
        if let Some(priority_id) = priority_id {
            config::find_priority_tx(&mut *tx, &project.id, priority_id)
                .await?
                .ok_or_else(|| {
                    AppError::Validation(format!(
                        "{priority_id:?} is not a priority of project {}.",
                        project.key
                    ))
                })?;
        }
        next.priority_id = priority_id.clone();
    }
    if let Some(resolution_id) = &patch.resolution_id {
        if let Some(resolution_id) = resolution_id {
            config::find_resolution_tx(&mut *tx, &project.id, resolution_id)
                .await?
                .ok_or_else(|| {
                    AppError::Validation(format!(
                        "{resolution_id:?} is not a resolution of project {}.",
                        project.key
                    ))
                })?;
        }
        next.resolution_id = resolution_id.clone();
    }
    if let Some(assignee_id) = &patch.assignee_id {
        if let Some(id) = assignee_id {
            check_user_exists(&mut *tx, "assigneeId", id).await?;
        }
        next.assignee_id = assignee_id.clone();
    }
    if let Some(reporter_id) = &patch.reporter_id {
        if let Some(id) = reporter_id {
            check_user_exists(&mut *tx, "reporterId", id).await?;
        }
        next.reporter_id = reporter_id.clone();
    }
    if let Some(due_date) = patch.due_date {
        next.due_date = due_date;
    }
    if let Some(start_date) = patch.start_date {
        next.start_date = start_date;
    }
    if let Some(estimate) = patch.estimate {
        next.estimate = estimate;
    }
    if let Some(rank) = &patch.rank {
        next.rank = rank.clone();
    }
    if let Some(archived) = patch.archived {
        next.archived_at = match (archived, current.archived_at) {
            (true, Some(at)) => Some(at),
            (true, None) => Some(now),
            (false, _) => None,
        };
    }

    if next.type_id != current.type_id {
        check_type_change_against_hierarchy(&mut *tx, current, &next.type_id).await?;
    }

    // --- the workflow execution contract (see domain::workflow) --------------
    //
    // A status change is a *transition*, and a transition must be legal. The
    // check happens here, once, so both a board drag and a bare `PATCH statusId`
    // are covered — there is no second door.
    let explicit = tctx.as_ref().and_then(|c| c.explicit.clone());
    let entered_comment = tctx.as_ref().and_then(|c| c.comment.clone());

    let transition: Option<Transition> = match explicit {
        // The execute endpoint pinned and already verified this one.
        Some(transition) => Some(transition),
        // A status change with no pinned transition auto-resolves: conditions
        // decide whether *any* legal edge reaches the target. A permissive
        // (default or absent) workflow yields `None` and behaves exactly as the
        // pre-workflow code did.
        None if next.status_id != current.status_id => {
            match workflow::resolve_transition(&mut *tx, current, &next.status_id, author_id).await?
            {
                Outcome::Permissive => None,
                Outcome::Via(transition) => Some(*transition),
            }
        }
        None => None,
    };

    if let Some(transition) = &transition {
        // Validators run against the card the user is submitting — before any
        // post-function touches it. A failure is a 422 and stops here: the status
        // does not change and no post-function runs.
        workflow::run_validators(&mut *tx, transition, &next).await?;

        // Field post-functions (SetResolution, AssignTo, UpdateField) fold into
        // `next` so they land in the same row and the same changelog as the
        // status change. SetResolution only *sets* the field; the resolution
        // rules below reconcile it against the landing status.
        workflow::apply_field_post_functions(&mut *tx, transition, &mut next, author_id).await?;
    }

    apply_resolution_rules(&mut *tx, &project, current, &mut next, now).await?;

    let changes = diff(&mut *tx, current, &next).await?;
    let moved = !changes.is_empty();

    // Nothing moved: no write, no `updated_at` bump, no history. Bumping
    // `updated_at` for a no-op edit would make "updated <= -7d" — the query the
    // job-search follow-up rule is built on (TODO.md Phase 15) — quietly wrong.
    //
    // But taking a *transition* is a deliberate action, not a field edit, and its
    // post-functions and the comment the user typed on the screen must still run
    // even when no field moved. A self-loop "comment" transition — a global edge
    // back to the card's own status — exists for exactly that: it fires an event
    // and records a comment without moving the card. Gating the deferred
    // post-functions on `moved` silently swallowed all three (the FireEvent, the
    // AddComment, and the screen comment). So the *write and changelog* are gated
    // on an actual field change; the *deferred post-functions* are gated on a
    // transition having been taken.
    if moved {
        write(&mut *tx, &next, now).await?;
        history::record(&mut *tx, &current.id, author_id, &changes, now).await?;
    }

    // Deferred post-functions (add comment, fire event) run last — after the write
    // and the history when there was one — but still inside `tx`, so if one fails
    // the whole transition rolls back and the card did not move. They neither read
    // nor need the just-written row; both sinks reference the card by id.
    if let Some(transition) = &transition {
        workflow::run_deferred_post_functions(
            &mut *tx,
            transition,
            &next,
            author_id,
            entered_comment.as_deref(),
            now,
        )
        .await?;
    }

    if !moved {
        // The card row itself is unchanged; any post-function above added a
        // comment or an event, neither of which is a card field.
        return Ok(current.clone());
    }

    find_by_id_tx(&mut *tx, &current.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just updated is missing")))
}

/// Refuses a type change that would break the tree around the card.
///
/// A card's hierarchy level comes from its type, so "make this Story an Epic" is
/// a structural move wearing a field edit's clothes. It can orphan the card from
/// its parent (an Epic cannot live under an Epic) or from its children.
/// Refuses a user reference that names nobody.
///
/// `assignee_id` and `reporter_id` are the only two card fields that point
/// *outside* the project, so none of the "is this a status/priority/type of this
/// project" checks reach them. That left the column's foreign key as the sole
/// guard — and a raw FK violation arrives as [`AppError::Internal`], so
/// `PATCH {"assigneeId": "no-such-user"}` answered **500, incident logged** to
/// what is plainly a bad request, telling the caller nothing about which field
/// was wrong and burying a real 500 in the noise if one ever happened here.
///
/// Deactivated users pass deliberately. Accounts are never hard-deleted
/// precisely *because* cards reference them, and a card assigned to someone who
/// has since left must keep saying so — this checks existence, not eligibility.
async fn check_user_exists(
    tx: &mut sqlx::SqliteConnection,
    field: &str,
    user_id: &str,
) -> AppResult<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;

    if exists.is_none() {
        return Err(AppError::Validation(format!(
            "{field} {user_id:?} is not a user of this instance."
        )));
    }

    Ok(())
}

async fn check_type_change_against_hierarchy(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    new_type_id: &str,
) -> AppResult<()> {
    let new_level: i64 = sqlx::query_scalar("SELECT level FROM card_types WHERE id = ?")
        .bind(new_type_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(parent_id) = &current.parent_id {
        let parent_level = hierarchy::level_of(&mut *tx, parent_id).await?;
        if parent_level <= new_level {
            return Err(AppError::Conflict(format!(
                "{} would sit at hierarchy level {new_level}, which is not below its parent's \
                 level {parent_level}. Move it out from under its parent first.",
                current.key
            )));
        }
    }

    let deepest_child: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(ct.level) FROM cards c JOIN card_types ct ON c.type_id = ct.id \
         WHERE c.parent_id = ? AND c.deleted_at IS NULL",
    )
    .bind(&current.id)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(child_level) = deepest_child
        && child_level >= new_level
    {
        return Err(AppError::Conflict(format!(
            "{} would sit at hierarchy level {new_level}, which is not above its children's level \
             {child_level}. Move its children out first.",
            current.key
        )));
    }

    Ok(())
}

/// Keeps resolution and status honest about each other. **docs/adr §E.**
///
/// # The failure this exists to kill
///
/// In Jira an issue is resolved iff `resolution IS NOT EMPTY`, which is
/// *independent* of reaching a Done status. Setting it is a workflow
/// post-function someone has to remember to add. When they don't — and the
/// default workflows don't — a card sits in the Done column and counts as open
/// in every report, every filter and every `resolution = EMPTY` query. It is
/// Jira's single most-reported confusion, and it is entirely self-inflicted.
///
/// The expressive power is worth keeping: "Done" and "Won't Do" and "Duplicate"
/// are genuinely different endings, and status alone cannot say which. So Atlas
/// keeps the field and removes the footgun by driving it from the transition:
///
/// - **In a done category** → there is a resolution. The caller's choice if it
///   named one in the same patch, otherwise whatever the card already said,
///   otherwise the project's default. A project with no resolutions at all is
///   refused loudly rather than silently producing the exact Jira bug this
///   function exists to prevent.
/// - **Not in a done category** → there is no resolution. This overrides an
///   explicitly supplied one, deliberately: "reopened but still resolved" is not
///   a state anyone means, and honouring both halves of a contradictory request
///   is how the confusion gets back in.
///
/// # Why the *landing* status and not the transition
///
/// This was gated on `next.status_id != current.status_id` — the rule only ran
/// when the status moved. That left the entire class of patches that move the
/// **resolution** and not the status unguarded, in both directions:
///
/// - `{"resolutionId": null}` on a card sitting in Done → Done column, no
///   resolution: the §E bug verbatim, reached by the one route nobody tested.
/// - `{"resolutionId": "..."}` on a card in To Do → resolved, in the first
///   column, counting as closed in every report.
///
/// The invariant is a property of the row, not of the edit that produced it, so
/// it is enforced against the status the card is *landing in* regardless of
/// whether that status moved. `resolved_at` then tracks `resolution_id` the same
/// way, so "`resolved_at` is set iff `resolution_id` is set" holds on every path.
async fn apply_resolution_rules(
    tx: &mut sqlx::SqliteConnection,
    project: &Project,
    current: &Card,
    next: &mut Card,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let (now_done, status_name) = category_and_name_of(&mut *tx, &next.status_id).await?;

    if now_done {
        if next.resolution_id.is_none() {
            // The card must say why it stopped. Its own existing resolution
            // first: a bare request to clear it is a contradiction, and falling
            // straight to the default would silently *restate the outcome* —
            // "Won't Do" quietly becoming "Done" is a worse answer than either
            // honouring or ignoring the request. The default is for the card
            // that has just arrived in Done and has nothing to say yet.
            next.resolution_id = if let Some(resolution_id) = &current.resolution_id {
                Some(resolution_id.clone())
            } else {
                let resolution = config::default_resolution_tx(&mut *tx, &project.id)
                    .await?
                    .ok_or_else(|| no_resolutions(project, &status_name))?;
                Some(resolution.id)
            };
        }
    } else {
        next.resolution_id = None;
    }

    // The invariant, applied on every path rather than only the transition:
    // resolved_at is set exactly when resolution_id is.
    if next.resolution_id.is_none() {
        next.resolved_at = None;
    } else if next.resolution_id != current.resolution_id {
        next.resolved_at = Some(now);
    }

    Ok(())
}

/// Whether a status is done, and what it is called.
async fn category_and_name_of(
    tx: &mut sqlx::SqliteConnection,
    status_id: &str,
) -> AppResult<(bool, String)> {
    let status = config::status_by_id_tx(tx, status_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((status.category.is_done(), status.name))
}

/// Works out what actually moved, and what a human should read for each.
///
/// Every reference resolves a display name here, at the moment of the change,
/// because that is the only moment it is knowable. See [`crate::domain::history`].
// One `if` per tracked field, and that is the point: the list of fields that get
// a changelog entry is meant to be readable top to bottom, so that adding a
// column to `cards` and forgetting to track it is a visible omission rather than
// a missing line in a macro. Length here is field count, not complexity.
#[allow(clippy::too_many_lines)]
async fn diff(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    next: &Card,
) -> AppResult<Vec<Change>> {
    let mut changes = Vec::new();

    if current.summary != next.summary {
        changes.push(Change::plain(
            Field::Summary,
            Some(current.summary.clone()),
            Some(next.summary.clone()),
        ));
    }

    if current.description != next.description {
        changes.push(Change::plain(
            Field::Description,
            current.description.clone(),
            next.description.clone(),
        ));
    }

    if current.type_id != next.type_id {
        changes.push(Change::reference(
            Field::Type,
            reference(&mut *tx, TYPE_NAME_SQL, Some(&current.type_id)).await?,
            reference(&mut *tx, TYPE_NAME_SQL, Some(&next.type_id)).await?,
        ));
    }

    if current.status_id != next.status_id {
        changes.push(Change::reference(
            Field::Status,
            reference(&mut *tx, STATUS_NAME_SQL, Some(&current.status_id)).await?,
            reference(&mut *tx, STATUS_NAME_SQL, Some(&next.status_id)).await?,
        ));
    }

    if current.priority_id != next.priority_id {
        changes.push(Change::reference(
            Field::Priority,
            reference(&mut *tx, PRIORITY_NAME_SQL, current.priority_id.as_deref()).await?,
            reference(&mut *tx, PRIORITY_NAME_SQL, next.priority_id.as_deref()).await?,
        ));
    }

    if current.assignee_id != next.assignee_id {
        changes.push(Change::reference(
            Field::Assignee,
            reference(&mut *tx, USER_NAME_SQL, current.assignee_id.as_deref()).await?,
            reference(&mut *tx, USER_NAME_SQL, next.assignee_id.as_deref()).await?,
        ));
    }

    if current.reporter_id != next.reporter_id {
        changes.push(Change::reference(
            Field::Reporter,
            reference(&mut *tx, USER_NAME_SQL, current.reporter_id.as_deref()).await?,
            reference(&mut *tx, USER_NAME_SQL, next.reporter_id.as_deref()).await?,
        ));
    }

    if current.resolution_id != next.resolution_id {
        changes.push(Change::reference(
            Field::Resolution,
            reference(
                &mut *tx,
                RESOLUTION_NAME_SQL,
                current.resolution_id.as_deref(),
            )
            .await?,
            reference(&mut *tx, RESOLUTION_NAME_SQL, next.resolution_id.as_deref()).await?,
        ));
    }

    if current.resolved_at != next.resolved_at {
        changes.push(Change::plain(
            Field::ResolvedAt,
            current.resolved_at.map(to_sql_timestamp),
            next.resolved_at.map(to_sql_timestamp),
        ));
    }

    if current.due_date != next.due_date {
        changes.push(Change::plain(
            Field::DueDate,
            current.due_date.map(|d| d.to_string()),
            next.due_date.map(|d| d.to_string()),
        ));
    }

    if current.start_date != next.start_date {
        changes.push(Change::plain(
            Field::StartDate,
            current.start_date.map(|d| d.to_string()),
            next.start_date.map(|d| d.to_string()),
        ));
    }

    if current.estimate != next.estimate {
        changes.push(Change::plain(
            Field::Estimate,
            current.estimate.map(format_estimate),
            next.estimate.map(format_estimate),
        ));
    }

    if current.rank != next.rank {
        // A rank is machine noise, so the display side says what a human cares
        // about — the direction — rather than a hex string. `to_value` keeps the
        // real key so the change is still reconstructable.
        let direction = if next.rank > current.rank {
            "Ranked lower"
        } else {
            "Ranked higher"
        };
        changes.push(Change {
            field: Field::Rank,
            from_value: Some(current.rank.to_string()),
            from_display: None,
            to_value: Some(next.rank.to_string()),
            to_display: Some(direction.to_owned()),
        });
    }

    if current.archived_at != next.archived_at {
        changes.push(Change::plain(
            Field::Archived,
            current.archived_at.map(to_sql_timestamp),
            next.archived_at.map(to_sql_timestamp),
        ));
    }

    if current.deleted_at != next.deleted_at {
        changes.push(Change::plain(
            Field::Deleted,
            current.deleted_at.map(to_sql_timestamp),
            next.deleted_at.map(to_sql_timestamp),
        ));
    }

    Ok(changes)
}

/// Formats an estimate the way a human wrote it: `3`, not `3`.
fn format_estimate(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// Resolves a reference to `(id, display name)`.
///
/// A referent that has vanished still gets a row, with the id standing in for
/// the name. Losing the change because the thing it pointed at is gone would be
/// exactly backwards: that is when history matters most.
async fn reference(
    tx: &mut sqlx::SqliteConnection,
    sql: &'static str,
    id: Option<&str>,
) -> AppResult<Option<(String, String)>> {
    let Some(id) = id else {
        return Ok(None);
    };

    let name: Option<String> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

    Ok(Some((id.to_owned(), name.unwrap_or_else(|| id.to_owned()))))
}

/// Writes every mutable column of a card. The single UPDATE behind [`update`].
async fn write(tx: &mut sqlx::SqliteConnection, card: &Card, now: DateTime<Utc>) -> AppResult<()> {
    sqlx::query(
        "UPDATE cards SET \
           type_id       = ?, \
           summary       = ?, \
           description   = ?, \
           status_id     = ?, \
           priority_id   = ?, \
           assignee_id   = ?, \
           reporter_id   = ?, \
           resolution_id = ?, \
           resolved_at   = ?, \
           due_date      = ?, \
           start_date    = ?, \
           estimate      = ?, \
           rank          = ?, \
           archived_at   = ?, \
           deleted_at    = ?, \
           updated_at    = ? \
         WHERE id = ?",
    )
    .bind(&card.type_id)
    .bind(&card.summary)
    .bind(&card.description)
    .bind(&card.status_id)
    .bind(&card.priority_id)
    .bind(&card.assignee_id)
    .bind(&card.reporter_id)
    .bind(&card.resolution_id)
    .bind(card.resolved_at.map(to_sql_timestamp))
    .bind(card.due_date)
    .bind(card.start_date)
    .bind(card.estimate)
    .bind(&card.rank)
    .bind(card.archived_at.map(to_sql_timestamp))
    .bind(card.deleted_at.map(to_sql_timestamp))
    .bind(to_sql_timestamp(now))
    .bind(&card.id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The board operations
// ---------------------------------------------------------------------------

/// Where a card is being dropped.
///
/// The neighbours are named rather than a position computed, because a position
/// is stale the moment the client sends it. Naming the two cards it was dropped
/// between is a statement about what the user actually saw, and
/// [`crate::rank::Rank::between`] turns it into a key — or a 409 saying the
/// neighbours have moved, which is the truth.
#[derive(Debug, Default)]
pub struct Drop {
    /// The target column. `None` keeps the card where it is.
    pub status_id: Option<String>,
    /// The card immediately **above** the drop point, if any.
    pub previous_card_id: Option<String>,
    /// The card immediately **below** the drop point, if any.
    pub next_card_id: Option<String>,
}

/// The drag-and-drop endpoint's whole job: move a card to a column and a place.
///
/// Delegates to [`update`], which is the point — a drag is a status change and a
/// rank change, so it gets the resolution rules and the changelog for free
/// rather than reimplementing them. Dropping a card into the Done column sets a
/// resolution because *every* route into Done does.
pub async fn move_card(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    drop: &Drop,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    let status_id = drop
        .status_id
        .clone()
        .unwrap_or_else(|| card.status_id.clone());

    let previous =
        neighbour_rank(&mut *tx, card, &status_id, drop.previous_card_id.as_deref()).await?;
    let next = neighbour_rank(&mut *tx, card, &status_id, drop.next_card_id.as_deref()).await?;

    // Neither neighbour named: the caller is saying "put it in this column"
    // without saying where, so it goes to the bottom.
    //
    // `Rank::between(None, None)` would be wrong here, and silently so: it means
    // "the list is empty" and returns `Rank::first()`. In a column that already
    // has cards that is a *collision* — the new rank equals the rank of whatever
    // landed there first, `ORDER BY rank` becomes a tie, and the board's order
    // turns arbitrary. Asking for the bottom of the column handles the
    // genuinely-empty case identically (there is no last card, so it returns
    // `Rank::first()` anyway) and the populated case correctly.
    let rank = if previous.is_none() && next.is_none() {
        rank_for_placement(&mut *tx, &card.project_id, &status_id, Placement::Bottom).await?
    } else {
        // `between` fails when the named neighbours are equal or out of order,
        // which means the client's board is stale. That is a 409 telling it to
        // refetch — `From<RankError> for AppError` already maps it.
        check_gap_is_still_a_gap(&mut *tx, card, &status_id, previous.as_ref(), next.as_ref())
            .await?;
        Rank::between(previous.as_ref(), next.as_ref())?
    };

    update(
        &mut *tx,
        card,
        &CardPatch {
            status_id: Some(status_id),
            rank: Some(rank),
            ..CardPatch::default()
        },
        author_id,
        now,
    )
    .await
}

/// Refuses a drop into a gap that is not a gap any more.
///
/// # Why naming two live cards is not enough
///
/// [`neighbour_rank`] already refuses a neighbour that has left the column. That
/// catches the obvious staleness and misses the more common kind: both cards are
/// still right there, and something has moved *between* them.
///
/// It matters because [`Rank::between`] is **deterministic**. `between(A, B)`
/// returns the same key every time, and dropping a card into the gap does not
/// change A or B — so a second drop naming the same pair recomputes the same
/// key, and two cards end up sharing a rank. `ORDER BY rank` becomes a tie, and
/// the board stops showing the user where they put things. No concurrency is
/// needed to reach it: two ordinary drags naming the same pair will do, which is
/// exactly what a client with a board a few seconds old will send.
///
/// So the check is the one the [`Drop`] doc claims: the pair is a statement
/// about what the user actually saw, and if they could not have seen it — there
/// is a card in the gap — it earns a 409 rather than a guess. The bounds are the
/// open interval, with an absent end meaning "the edge of the column", which is
/// why the head and tail drops are covered by the same query: `between(A, None)`
/// is just as deterministic as `between(A, B)`.
async fn check_gap_is_still_a_gap(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    status_id: &str,
    previous: Option<&Rank>,
    next: Option<&Rank>,
) -> AppResult<()> {
    // One fixed statement with both bounds always present and neutralised by a
    // parameter, rather than a `WHERE` assembled from which ends were named —
    // same reasoning as `card_filter_where!`.
    let intruder: Option<String> = sqlx::query_scalar(
        "SELECT key FROM cards \
          WHERE project_id = ? AND status_id = ? AND deleted_at IS NULL \
            AND id != ? \
            AND (? IS NULL OR rank > ?) \
            AND (? IS NULL OR rank < ?) \
          ORDER BY rank LIMIT 1",
    )
    .bind(&card.project_id)
    .bind(status_id)
    .bind(&card.id)
    .bind(previous.map(Rank::as_str))
    .bind(previous.map(Rank::as_str))
    .bind(next.map(Rank::as_str))
    .bind(next.map(Rank::as_str))
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(key) = intruder {
        return Err(AppError::Conflict(format!(
            "{key} is between those two cards now; refetch the board and try again."
        )));
    }

    Ok(())
}

/// The rank of a named neighbour, checked to be where the client thinks it is.
async fn neighbour_rank(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    status_id: &str,
    neighbour_id: Option<&str>,
) -> AppResult<Option<Rank>> {
    let Some(neighbour_id) = neighbour_id else {
        return Ok(None);
    };

    if neighbour_id == card.id {
        return Err(AppError::Validation(
            "A card cannot be dropped next to itself.".to_owned(),
        ));
    }

    let neighbour = find_by_id_tx(&mut *tx, neighbour_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("That card no longer exists; refetch the board.".to_owned())
        })?;

    // The neighbour has to actually be in the column the card is landing in. If
    // it is not, the client is looking at a board somebody else has already
    // changed, and computing a rank from it would put the card somewhere the
    // user did not drop it.
    if neighbour.project_id != card.project_id || neighbour.status_id != status_id {
        return Err(AppError::Conflict(format!(
            "{} is not in that column any more; refetch the board and try again.",
            neighbour.key
        )));
    }

    Ok(Some(neighbour.rank))
}

/// Moves a card (and everything under it) to a new parent, or to the root.
///
/// The four guards live in [`crate::domain::hierarchy::check_reparent`]; this
/// function is what writes the result and its history. Reparenting is not a
/// field edit and deliberately not reachable through [`CardPatch`] — dragging a
/// card onto another card comes through here, and there is no other door.
pub async fn reparent(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    new_parent_id: Option<&str>,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    if new_parent_id == card.parent_id.as_deref() {
        return Ok(card.clone());
    }

    if let Some(new_parent_id) = new_parent_id {
        let new_parent = find_by_id_tx(&mut *tx, new_parent_id)
            .await?
            .ok_or_else(|| AppError::Validation(format!("No card with id {new_parent_id:?}.")))?;
        hierarchy::check_reparent(&mut *tx, card, &new_parent).await?;
    }

    let change = Change::reference(
        Field::Parent,
        reference(&mut *tx, CARD_KEY_SQL, card.parent_id.as_deref()).await?,
        reference(&mut *tx, CARD_KEY_SQL, new_parent_id).await?,
    );

    sqlx::query("UPDATE cards SET parent_id = ?, updated_at = ? WHERE id = ?")
        .bind(new_parent_id)
        .bind(to_sql_timestamp(now))
        .bind(&card.id)
        .execute(&mut *tx)
        .await?;

    history::record(&mut *tx, &card.id, author_id, &[change], now).await?;

    find_by_id_tx(&mut *tx, &card.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just reparented is missing")))
}

/// Moves a card to the trash.
///
/// Soft, always. A hard delete would take the card's history, its comments and
/// every inbound link with it, and "restore" is the whole reason the trash
/// exists. The key stays burned either way — see
/// [`crate::domain::project::allocate_card_key`].
pub async fn soft_delete(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    if card.is_deleted() {
        return Ok(card.clone());
    }

    let mut next = card.clone();
    next.deleted_at = Some(now);

    let changes = diff(&mut *tx, card, &next).await?;
    write(&mut *tx, &next, now).await?;
    history::record(&mut *tx, &card.id, author_id, &changes, now).await?;

    find_by_id_tx(&mut *tx, &card.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just deleted is missing")))
}

/// Brings a card back out of the trash.
pub async fn restore(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    if !card.is_deleted() {
        return Ok(card.clone());
    }

    let mut next = card.clone();
    next.deleted_at = None;

    let changes = diff(&mut *tx, card, &next).await?;
    write(&mut *tx, &next, now).await?;
    history::record(&mut *tx, &card.id, author_id, &changes, now).await?;

    find_by_id_tx(&mut *tx, &card.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just restored is missing")))
}

// ---------------------------------------------------------------------------
// Moving between projects — the reason card_key_history exists
// ---------------------------------------------------------------------------

/// Moves a card and its whole subtree into another project.
///
/// # Why the subtree travels
///
/// A parent in one project and a child in another is a tree that spans projects,
/// and then every board scope, breadcrumb and roll-up has to decide what that
/// means. It means nothing useful, so the tree moves as a unit and the moved
/// card's own parent link is cut — its parent stays behind.
///
/// # What each card gets
///
/// A new key, because the counter is per-project — and the old key goes into
/// `card_key_history`, which is the whole reason that table exists. Every id it
/// referenced is per-project too, so each is remapped by *meaning* rather than
/// dropped:
///
/// | field | mapped by | if there is no match |
/// |---|---|---|
/// | type | same hierarchy level | **refused** — see below |
/// | status | same **category** | the target's first status |
/// | priority | same name | cleared |
/// | resolution | same name | cleared |
///
/// Mapping status by category rather than by name is the important one: a card
/// that was in progress stays in progress. Matching on name would silently
/// reopen a finished card the moment two projects spell their columns
/// differently — which is always.
///
/// # Why a missing level is refused rather than defaulted
///
/// The type fallback used to be "the target's default type", which quietly
/// ignores the one thing the type decides: the card's rung. The templates make
/// that reachable with no trickery — programming seeds Initiative at level 2 and
/// job-search has no level 2 — so an Initiative would land as an Application
/// (level 0) while the Epic beneath it landed as a Company (level 1), leaving a
/// **parent below its own child**. That is ADR 0002's only structural rule, and
/// every other door (`create`, [`reparent`], a type change) enforces it; this
/// one silently produced a tree none of them would have accepted.
///
/// Refusing is also the honest answer on the merits: a job hunt has no rung for
/// an Initiative, which is exactly what "no level 2" says. Flattening it into an
/// Application would not be a move, it would be a quiet reinterpretation of what
/// the card is. The 409 names the level and the card, so the fix — add the rung,
/// or move the cards individually — is the caller's to choose.
pub async fn move_to_project(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    target: &Project,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Card> {
    if card.project_id == target.id {
        return Ok(card.clone());
    }

    let source = project::find_by_id_tx(&mut *tx, &card.project_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Root first, so a card is always moved before its children.
    let subtree = hierarchy::descendant_ids(&mut *tx, &card.id).await?;

    for (index, card_id) in subtree.iter().enumerate() {
        let current = find_by_id_tx(&mut *tx, card_id)
            .await?
            .ok_or(AppError::NotFound)?;

        // Only the root's parent link is cut: the links *inside* the subtree are
        // between cards that are all moving, so they stay valid.
        let detach_parent = index == 0;

        move_one(
            &mut *tx,
            &current,
            &source,
            target,
            detach_parent,
            author_id,
            now,
        )
        .await?;
    }

    find_by_id_tx(&mut *tx, &card.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the card just moved is missing")))
}

/// Moves one card of a subtree. See [`move_to_project`].
// Four independent remappings (type, status, priority, resolution), each a
// lookup with a documented fallback, then one write. The length is the four
// tables; factoring each into its own function would scatter the fallback policy
// that the doc comment on `move_to_project` describes as a single table.
#[allow(clippy::too_many_lines)]
async fn move_one(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    source: &Project,
    target: &Project,
    detach_parent: bool,
    author_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let level = hierarchy::level_of(&mut *tx, &current.id).await?;

    // The level is not negotiable: it is what keeps a parent above its child.
    // Falling back to the target's default type here would drop the card onto
    // whatever rung that type happens to sit on — see `move_to_project`.
    let card_type = config::card_type_at_level_tx(&mut *tx, &target.id, level)
        .await?
        .ok_or_else(|| {
            AppError::Conflict(format!(
                "{} sits at hierarchy level {level}, and project {} has no card type at that \
                 level. Add one to {}, or move the cards one rung at a time.",
                current.key, target.key, target.key
            ))
        })?;

    let old_status = config::status_by_id_tx(&mut *tx, &current.status_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let status =
        match config::first_status_in_category_tx(&mut *tx, &target.id, old_status.category).await?
        {
            Some(status) => status,
            None => config::first_status_tx(&mut *tx, &target.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!(
                        "Project {} has no statuses, so nothing can be moved into it.",
                        target.key
                    ))
                })?,
        };

    let priority = match &current.priority_id {
        Some(priority_id) => {
            let name: Option<String> = sqlx::query_scalar(PRIORITY_NAME_SQL)
                .bind(priority_id)
                .fetch_optional(&mut *tx)
                .await?;
            match name {
                Some(name) => config::priority_by_name_tx(&mut *tx, &target.id, &name).await?,
                None => None,
            }
        }
        None => None,
    };

    // A resolution only survives if the card is still landing in a done column
    // *and* the target project has a resolution by that name. Anything else and
    // the card arrives unresolved, which is the honest answer.
    let resolution = if status.category.is_done() {
        match &current.resolution_id {
            Some(resolution_id) => {
                let name: Option<String> = sqlx::query_scalar(RESOLUTION_NAME_SQL)
                    .bind(resolution_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                match name {
                    Some(name) => {
                        match config::resolution_by_name_tx(&mut *tx, &target.id, &name).await? {
                            Some(resolution) => Some(resolution),
                            None => config::default_resolution_tx(&mut *tx, &target.id).await?,
                        }
                    }
                    None => config::default_resolution_tx(&mut *tx, &target.id).await?,
                }
            }
            None => config::default_resolution_tx(&mut *tx, &target.id).await?,
        }
    } else {
        None
    };

    let old_key = current.key.clone();
    let new_key = project::allocate_card_key(&mut *tx, &target.id).await?;
    let rank = rank_for_placement(&mut *tx, &target.id, &status.id, Placement::Bottom).await?;
    let timestamp = to_sql_timestamp(now);

    // The redirect, written before the key changes so a failure here cannot
    // leave a renamed card with no way back to its old name.
    sqlx::query(
        "INSERT INTO card_key_history (id, card_id, old_key, moved_at) VALUES (?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&current.id)
    .bind(&old_key)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    let mut next = current.clone();
    next.project_id = target.id.clone();
    next.key = new_key.clone();
    next.type_id = card_type.id.clone();
    next.status_id = status.id.clone();
    next.priority_id = priority.as_ref().map(|p| p.id.clone());
    next.resolution_id = resolution.as_ref().map(|r| r.id.clone());
    next.resolved_at = if next.resolution_id.is_some() {
        current.resolved_at.or(Some(now))
    } else {
        None
    };
    next.rank = rank;
    if detach_parent {
        next.parent_id = None;
    }

    // The history for everything except the project and the key: `diff` already
    // knows how to describe a type, status, priority or resolution change, and
    // the display names it resolves are still the *old* ones at this point,
    // which is exactly right.
    let mut changes = diff(&mut *tx, current, &next).await?;
    changes.push(Change::reference(
        Field::Project,
        Some((source.id.clone(), source.key.clone())),
        Some((target.id.clone(), target.key.clone())),
    ));
    changes.push(Change::plain(
        Field::Key,
        Some(old_key),
        Some(new_key.clone()),
    ));
    if detach_parent && current.parent_id.is_some() {
        changes.push(Change::reference(
            Field::Parent,
            reference(&mut *tx, CARD_KEY_SQL, current.parent_id.as_deref()).await?,
            None,
        ));
    }

    sqlx::query(
        "UPDATE cards SET project_id = ?, key = ?, parent_id = ?, type_id = ?, status_id = ?, \
         priority_id = ?, resolution_id = ?, resolved_at = ?, rank = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&next.project_id)
    .bind(&next.key)
    .bind(&next.parent_id)
    .bind(&next.type_id)
    .bind(&next.status_id)
    .bind(&next.priority_id)
    .bind(&next.resolution_id)
    .bind(next.resolved_at.map(to_sql_timestamp))
    .bind(&next.rank)
    .bind(&timestamp)
    .bind(&current.id)
    .execute(&mut *tx)
    .await?;

    history::record(&mut *tx, &current.id, author_id, &changes, now).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_trimmed_bounded_and_single_line() {
        assert_eq!(validate_summary("  Add login  ").unwrap(), "Add login");
        assert!(validate_summary("").is_err());
        assert!(validate_summary("   ").is_err());
        assert!(validate_summary(&"a".repeat(MAX_SUMMARY + 1)).is_err());
        assert!(validate_summary(&"a".repeat(MAX_SUMMARY)).is_ok());

        // A summary is rendered on one line everywhere it appears — board card,
        // breadcrumb, history row, notification subject, agent prompt.
        assert!(validate_summary("Add\nlogin").is_err());
        assert!(validate_summary("Add\tlogin").is_err());
        assert!(validate_summary("Add login\u{0}").is_err());
    }

    #[test]
    fn descriptions_are_markdown_so_only_length_is_checked() {
        // Newlines and markup are the point of the field.
        assert!(validate_description("# Heading\n\n- a\n- b").is_ok());
        assert!(validate_description(&"a".repeat(MAX_DESCRIPTION)).is_ok());
        assert!(validate_description(&"a".repeat(MAX_DESCRIPTION + 1)).is_err());
    }

    #[test]
    fn estimates_render_without_a_pointless_decimal() {
        assert_eq!(format_estimate(3.0), "3");
        assert_eq!(format_estimate(0.0), "0");
        assert_eq!(format_estimate(2.5), "2.5");
        assert_eq!(format_estimate(-1.0), "-1");
    }

    #[test]
    fn an_empty_patch_is_recognised_as_naming_nothing() {
        assert!(CardPatch::default().is_empty());
        assert!(
            !CardPatch {
                summary: Some("x".to_owned()),
                ..CardPatch::default()
            }
            .is_empty()
        );
        // Naming a nullable field with an explicit null is still naming it.
        assert!(
            !CardPatch {
                assignee_id: Some(None),
                ..CardPatch::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn the_parent_filter_maps_to_the_mode_the_fixed_statement_branches_on() {
        assert_eq!(ParentFilter::Any.mode(), "any");
        assert_eq!(ParentFilter::Any.id(), None);
        assert_eq!(ParentFilter::Root.mode(), "root");
        assert_eq!(ParentFilter::Root.id(), None);
        assert_eq!(ParentFilter::Card("c1".to_owned()).mode(), "card");
        assert_eq!(ParentFilter::Card("c1".to_owned()).id(), Some("c1"));
    }
}

//! The board: a **view** over cards, grouped into columns by status.
//!
//! A board is not a container. It is the project's cards, read on demand and
//! bucketed into one column per status — and, because hierarchy is a uniform
//! `parent_id` (docs/adr/0002), the *same* computation scoped to one card's
//! children is the nested board. `GET /projects/{key}/board` is that computation;
//! [`build`] is its engine. The saved-config CRUD lower down is a thin bookmark
//! around it (the `boards` table, migration 0008).
//!
//! # Two properties this module is built around
//!
//! ## The filter reuses AQL, so it cannot leak across the access boundary
//!
//! The optional quick filter is not re-implemented here. [`board_source`] composes
//! the board's scope (`project = …`, `parent = …`) and the caller's filter into a
//! single AQL string and hands it to [`crate::aql::search`], which ANDs its
//! always-on accessible-projects predicate onto every query it compiles. So board
//! data is scoped exactly as `POST /search` is, by the same code, and a filter can
//! never read a card the caller could not already see. The parent scope is just an
//! extra `parent_id` predicate expressed in that same AQL.
//!
//! ## The mini-map rollup is ONE query, never N+1
//!
//! Every card that contains a board renders a miniature of it — the child
//! distribution by status category. Fetching that per card would be a query per
//! card on every board render. [`child_rollups`] instead computes the rollup for
//! *every* in-scope parent in a single `GROUP BY` over children joined to their
//! status category, and [`build`] looks each card up in the resulting map. A leaf
//! card is simply absent from the map and gets `null`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::auth::user::User;
use crate::db::Db;
use crate::domain::StatusCategory;
use crate::domain::config::{self, Status};
use crate::domain::project::Project;
use crate::domain::tag::Tag;
use crate::error::{AppError, AppResult};
use crate::rank::Rank;

/// The most cards one board render will fetch.
///
/// A board shows every card in scope at once (there are no column pages), so this
/// is a safety cap rather than a page size. It is the same clamp
/// [`crate::aql::compile`] already applies to any query's `LIMIT`, stated here so
/// the intent is visible at the call site.
const BOARD_CARD_CAP: i64 = 500;

// ---------------------------------------------------------------------------
// Scope and grouping
// ---------------------------------------------------------------------------

/// Which slice of the tree a board renders.
#[derive(Debug, Clone)]
pub enum BoardScope {
    /// The project's top-level cards (`parent_id IS NULL`).
    Root,
    /// One card's **direct** children — the nested board.
    Child {
        /// The parent card's id, for the rollup/tag/swimlane scope queries.
        id: String,
        /// The parent card's key, for the AQL `parent = …` scope.
        key: String,
    },
}

impl BoardScope {
    /// The `(mode, parent_id)` pair the fixed scope queries branch on. `mode` is
    /// `'root'` or `'child'`; `parent_id` is the card id for a child scope and
    /// `None` for the root, neutralised by the mode either way.
    fn mode_and_id(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Root => ("root", None),
            Self::Child { id, .. } => ("child", Some(id.as_str())),
        }
    }
}

/// How a board groups its rows into swimlanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Swimlane {
    /// No swimlanes: the flat set of columns only.
    #[default]
    None,
    /// One lane per assignee, plus an "Unassigned" lane.
    Assignee,
    /// One lane per parent card, plus a "No parent" lane.
    Parent,
}

impl Swimlane {
    /// Parses the `swimlane` query parameter. Absent means [`Swimlane::None`].
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] naming the three legal values for anything else.
    pub fn from_query(value: Option<&str>) -> AppResult<Self> {
        match value {
            None | Some("none" | "") => Ok(Self::None),
            Some("assignee") => Ok(Self::Assignee),
            Some("parent") => Ok(Self::Parent),
            Some(other) => Err(AppError::Validation(format!(
                "{other:?} is not a swimlane; use none, assignee or parent."
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// The response shape (the frontend builds against this)
// ---------------------------------------------------------------------------

/// A card's children summarised by status category — the mini-board preview.
///
/// `null` on a card with no children; present with real counts otherwise.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChildRollup {
    /// Total live children.
    pub total: i64,
    /// Children in a To Do status.
    pub todo: i64,
    /// Children in an In Progress status.
    pub in_progress: i64,
    /// Children in a Done status.
    pub done: i64,
}

/// One card as the board renders it: the summary fields plus the mini-map rollup.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardCard {
    /// UUID v7, as text.
    pub id: String,
    /// `ATLAS-123`.
    pub key: String,
    /// The one-line title.
    pub summary: String,
    /// The card type.
    pub type_id: String,
    /// The parent card, if any. Present so a nested-board client and the parent
    /// swimlane can group without a second fetch.
    pub parent_id: Option<String>,
    /// The workflow status — which column the card is in.
    pub status_id: String,
    /// The priority.
    pub priority_id: Option<String>,
    /// Who is doing it.
    pub assignee_id: Option<String>,
    /// Who asked for it.
    pub reporter_id: Option<String>,
    /// The board sort key. Cards come back in this order within a column.
    #[schema(value_type = String, example = "8000")]
    pub rank: Rank,
    /// The estimate, in the project's estimation unit.
    pub estimate: Option<f64>,
    /// The card's tags, by name.
    pub tags: Vec<Tag>,
    /// The mini-board rollup, or `null` for a leaf card.
    pub child_rollup: Option<ChildRollup>,
}

/// A column's status header: enough for the frontend to label and colour it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardColumnStatus {
    /// The status id.
    pub id: String,
    /// The status name.
    pub name: String,
    /// Which of the three categories it falls into.
    pub category: StatusCategory,
}

/// One board column: a status and the cards in it, in rank order.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardColumn {
    /// The column's status.
    pub status: BoardColumnStatus,
    /// The cards in this column, in rank order.
    pub cards: Vec<BoardCard>,
}

/// One swimlane: a labelled partition of the board's cards into the same columns.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardSwimlane {
    /// The grouping key — an assignee id, a parent card id, or `""` for the
    /// unassigned / no-parent catch-all.
    pub key: String,
    /// The human label for the lane.
    pub label: String,
    /// The same columns as the flat board, holding only this lane's cards.
    pub columns: Vec<BoardColumn>,
}

/// The board: one column per project status, and optionally the same cards
/// partitioned into swimlanes.
///
/// `columns` is always the full, ungrouped board. When a swimlane grouping is
/// requested, `swimlanes` additionally partitions the same cards; the frontend
/// renders one or the other.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardData {
    /// Every project status as a column, in `position` order, with its cards.
    pub columns: Vec<BoardColumn>,
    /// The swimlane partition, present only when a grouping was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swimlanes: Option<Vec<BoardSwimlane>>,
}

// ---------------------------------------------------------------------------
// Building the board
// ---------------------------------------------------------------------------

/// Builds the board for a scope, filter and swimlane grouping.
///
/// Four reads, none of them per-card: the project's statuses (the columns), the
/// in-scope filtered cards (via AQL, which supplies the access scoping), the
/// child rollups (one `GROUP BY`), and the cards' tags (one query). The swimlane
/// grouping adds one more read for the lane labels.
///
/// # Errors
///
/// [`AppError::BadRequest`] if the AQL filter does not parse or type-check;
/// otherwise a database error.
pub async fn build(
    db: &Db,
    viewer: &User,
    project: &Project,
    scope: &BoardScope,
    aql_filter: Option<&str>,
    swimlane: Swimlane,
    now: DateTime<Utc>,
) -> AppResult<BoardData> {
    let statuses = config::statuses(db, &project.id).await?;

    // The in-scope, filtered cards. The scope and the caller's filter ride the
    // AQL layer together, so the accessible-projects predicate is applied to both.
    let source = board_source(project, scope, aql_filter)?;
    let results = crate::aql::search(db, viewer, now, &source, BOARD_CARD_CAP, 0).await?;

    // The two batch reads that keep this off the N+1 path.
    let rollups = child_rollups(db, project, scope).await?;
    let tags = card_tags(db, project, scope).await?;

    let cards: Vec<BoardCard> = results
        .cards
        .iter()
        .map(|card| BoardCard {
            id: card.id.clone(),
            key: card.key.clone(),
            summary: card.summary.clone(),
            type_id: card.type_id.clone(),
            parent_id: card.parent_id.clone(),
            status_id: card.status_id.clone(),
            priority_id: card.priority_id.clone(),
            assignee_id: card.assignee_id.clone(),
            reporter_id: card.reporter_id.clone(),
            rank: card.rank.clone(),
            estimate: card.estimate,
            tags: tags.get(&card.id).cloned().unwrap_or_default(),
            child_rollup: rollups.get(&card.id).cloned(),
        })
        .collect();

    let columns = bucket(&statuses, &cards, |_| true);

    let swimlanes = match swimlane {
        Swimlane::None => None,
        Swimlane::Assignee => {
            Some(assignee_swimlanes(db, project, scope, &statuses, &cards).await?)
        }
        Swimlane::Parent => Some(parent_swimlanes(db, project, scope, &statuses, &cards).await?),
    };

    Ok(BoardData { columns, swimlanes })
}

/// Composes the AQL the board runs: its scope, combined with `AND` onto the
/// caller's filter.
///
/// The scope is expressed *as AQL* (`project = "KEY"`, `parent = "KEY"` or
/// `parent IS EMPTY`) so it rides the same compiler — and the same access
/// predicate — as the filter. The filter's own `ORDER BY` is dropped: a board
/// defines its own order (rank), and the query's ordering is never the board's.
///
/// The project and card keys are interpolated into a string that is then *parsed*
/// by AQL, which builds SQL only from its closed grammar and binds every value —
/// so this composition cannot inject SQL even though it formats text (the whole
/// point of `crate::aql`).
fn board_source(
    project: &Project,
    scope: &BoardScope,
    aql_filter: Option<&str>,
) -> AppResult<String> {
    let mut parts = vec![format!("project = \"{}\"", aql_escape(&project.key))];

    match scope {
        BoardScope::Root => parts.push("parent IS EMPTY".to_owned()),
        BoardScope::Child { key, .. } => {
            parts.push(format!("parent = \"{}\"", aql_escape(key)));
        }
    }

    if let Some(filter) = aql_filter {
        let trimmed = filter.trim();
        if !trimmed.is_empty() {
            let parsed = crate::aql::parse(trimmed)
                .map_err(|err| AppError::BadRequest(err.render(trimmed)))?;
            // Predicate only — a board's order is its own, never the filter's.
            if let Some(node) = parsed.predicate {
                let predicate_only = crate::aql::Query {
                    predicate: Some(node),
                    order_by: Vec::new(),
                };
                parts.push(format!("({})", crate::aql::normalize(&predicate_only)));
            }
        }
    }

    Ok(parts.join(" AND "))
}

/// Escapes a value for an AQL double-quoted string, the same way
/// [`crate::aql::normalize`] does.
fn aql_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Buckets cards into one column per status, preserving their (rank) order.
///
/// `include` selects which cards land here — `|_| true` for the flat board, and a
/// per-lane predicate for a swimlane. A card whose status is not among the
/// project's statuses is dropped rather than inventing a column for it.
fn bucket(
    statuses: &[Status],
    cards: &[BoardCard],
    mut include: impl FnMut(&BoardCard) -> bool,
) -> Vec<BoardColumn> {
    let index: HashMap<&str, usize> = statuses
        .iter()
        .enumerate()
        .map(|(i, status)| (status.id.as_str(), i))
        .collect();

    let mut columns: Vec<BoardColumn> = statuses
        .iter()
        .map(|status| BoardColumn {
            status: BoardColumnStatus {
                id: status.id.clone(),
                name: status.name.clone(),
                category: status.category,
            },
            cards: Vec::new(),
        })
        .collect();

    for card in cards {
        if !include(card) {
            continue;
        }
        if let Some(&i) = index.get(card.status_id.as_str()) {
            columns[i].cards.push(card.clone());
        }
    }

    columns
}

/// A row of the rollup `GROUP BY`.
#[derive(Debug, FromRow)]
struct RollupRow {
    parent_id: String,
    category: StatusCategory,
    n: i64,
}

/// The child rollup for **every** in-scope parent, in one query.
///
/// This is the query the mini-map depends on: a single `GROUP BY` over the
/// children of every board card, joined to their status category, rather than a
/// query per card. The scope matches the board's — top-level cards, or one card's
/// children — so it computes exactly (a superset of) the cards the board shows,
/// and [`build`] looks each up by id.
async fn child_rollups(
    db: &Db,
    project: &Project,
    scope: &BoardScope,
) -> AppResult<HashMap<String, ChildRollup>> {
    let (mode, parent_id) = scope.mode_and_id();

    let rows = sqlx::query_as::<_, RollupRow>(
        "SELECT parent.id AS parent_id, child_status.category AS category, COUNT(*) AS n \
         FROM cards parent \
         JOIN cards child ON child.parent_id = parent.id AND child.deleted_at IS NULL \
         JOIN statuses child_status ON child_status.id = child.status_id \
         WHERE parent.project_id = ? AND parent.deleted_at IS NULL \
           AND ((? = 'root' AND parent.parent_id IS NULL) \
             OR (? = 'child' AND parent.parent_id = ?)) \
         GROUP BY parent.id, child_status.category",
    )
    .bind(&project.id)
    .bind(mode)
    .bind(mode)
    .bind(parent_id)
    .fetch_all(db.reader())
    .await?;

    let mut map: HashMap<String, ChildRollup> = HashMap::new();
    for row in rows {
        let rollup = map.entry(row.parent_id).or_insert(ChildRollup {
            total: 0,
            todo: 0,
            in_progress: 0,
            done: 0,
        });
        rollup.total += row.n;
        match row.category {
            StatusCategory::Todo => rollup.todo += row.n,
            StatusCategory::InProgress => rollup.in_progress += row.n,
            StatusCategory::Done => rollup.done += row.n,
        }
    }
    Ok(map)
}

/// A tag joined to the card that carries it.
#[derive(Debug, FromRow)]
struct CardTagRow {
    card_id: String,
    #[sqlx(flatten)]
    tag: Tag,
}

/// Every in-scope card's tags, in one query, grouped by card.
///
/// Same shape as the rollup: one query scoped to the board rather than one per
/// card. A card with no tags is simply absent from the map.
async fn card_tags(
    db: &Db,
    project: &Project,
    scope: &BoardScope,
) -> AppResult<HashMap<String, Vec<Tag>>> {
    let (mode, parent_id) = scope.mode_and_id();

    let rows = sqlx::query_as::<_, CardTagRow>(
        "SELECT ct.card_id AS card_id, \
                t.id AS id, t.project_id AS project_id, t.name AS name, \
                t.colour AS colour, t.created_at AS created_at \
         FROM card_tags ct \
         JOIN tags t ON t.id = ct.tag_id \
         WHERE ct.card_id IN ( \
             SELECT id FROM cards \
              WHERE project_id = ? AND deleted_at IS NULL \
                AND ((? = 'root' AND parent_id IS NULL) \
                  OR (? = 'child' AND parent_id = ?)) \
         ) \
         ORDER BY t.name",
    )
    .bind(&project.id)
    .bind(mode)
    .bind(mode)
    .bind(parent_id)
    .fetch_all(db.reader())
    .await?;

    let mut map: HashMap<String, Vec<Tag>> = HashMap::new();
    for row in rows {
        map.entry(row.card_id).or_default().push(row.tag);
    }
    Ok(map)
}

/// A distinct assignee among the in-scope cards.
#[derive(Debug, FromRow)]
struct LaneUser {
    id: String,
    display_name: String,
}

/// One swimlane per assignee that has a card on the (filtered) board, plus an
/// "Unassigned" lane when any card has no assignee.
///
/// The display names come from one query over the in-scope cards' assignees; the
/// lanes themselves are then partitioned from the already-loaded board cards, so
/// an assignee whose only cards the filter removed produces no empty lane.
async fn assignee_swimlanes(
    db: &Db,
    project: &Project,
    scope: &BoardScope,
    statuses: &[Status],
    cards: &[BoardCard],
) -> AppResult<Vec<BoardSwimlane>> {
    let (mode, parent_id) = scope.mode_and_id();

    let users = sqlx::query_as::<_, LaneUser>(
        "SELECT DISTINCT u.id AS id, u.display_name AS display_name \
         FROM cards c \
         JOIN users u ON u.id = c.assignee_id \
         WHERE c.project_id = ? AND c.deleted_at IS NULL \
           AND ((? = 'root' AND c.parent_id IS NULL) \
             OR (? = 'child' AND c.parent_id = ?)) \
         ORDER BY u.display_name",
    )
    .bind(&project.id)
    .bind(mode)
    .bind(mode)
    .bind(parent_id)
    .fetch_all(db.reader())
    .await?;

    let present: HashSet<&str> = cards
        .iter()
        .filter_map(|c| c.assignee_id.as_deref())
        .collect();

    let mut lanes = Vec::new();
    for user in users {
        if !present.contains(user.id.as_str()) {
            continue;
        }
        let uid = user.id.clone();
        let columns = bucket(statuses, cards, |card| {
            card.assignee_id.as_deref() == Some(uid.as_str())
        });
        lanes.push(BoardSwimlane {
            key: user.id,
            label: user.display_name,
            columns,
        });
    }

    if cards.iter().any(|c| c.assignee_id.is_none()) {
        let columns = bucket(statuses, cards, |card| card.assignee_id.is_none());
        lanes.push(BoardSwimlane {
            key: String::new(),
            label: "Unassigned".to_owned(),
            columns,
        });
    }

    Ok(lanes)
}

/// A distinct parent among the in-scope cards.
#[derive(Debug, FromRow)]
struct LaneParent {
    id: String,
    summary: String,
}

/// One swimlane per parent card that has a card on the (filtered) board, plus a
/// "No parent" lane. Grouped from the loaded cards' `parent_id`; the query only
/// supplies the lane labels (the parents' summaries).
///
/// For the scopes the endpoint exposes today this is deliberately narrow: a root
/// board's cards are all parentless (one "No parent" lane), and a child board's
/// cards all share the one parent (one lane). It is written generically so it is
/// still correct if a broader scope is ever added.
async fn parent_swimlanes(
    db: &Db,
    project: &Project,
    scope: &BoardScope,
    statuses: &[Status],
    cards: &[BoardCard],
) -> AppResult<Vec<BoardSwimlane>> {
    let (mode, parent_id) = scope.mode_and_id();

    let parents = sqlx::query_as::<_, LaneParent>(
        "SELECT DISTINCT p.id AS id, p.summary AS summary \
         FROM cards c \
         JOIN cards p ON p.id = c.parent_id \
         WHERE c.project_id = ? AND c.deleted_at IS NULL \
           AND ((? = 'root' AND c.parent_id IS NULL) \
             OR (? = 'child' AND c.parent_id = ?)) \
         ORDER BY p.rank, p.key",
    )
    .bind(&project.id)
    .bind(mode)
    .bind(mode)
    .bind(parent_id)
    .fetch_all(db.reader())
    .await?;

    let present: HashSet<&str> = cards
        .iter()
        .filter_map(|c| c.parent_id.as_deref())
        .collect();

    let mut lanes = Vec::new();
    for parent in parents {
        if !present.contains(parent.id.as_str()) {
            continue;
        }
        let pid = parent.id.clone();
        let columns = bucket(statuses, cards, |card| {
            card.parent_id.as_deref() == Some(pid.as_str())
        });
        lanes.push(BoardSwimlane {
            key: parent.id,
            label: parent.summary,
            columns,
        });
    }

    if cards.iter().any(|c| c.parent_id.is_none()) {
        let columns = bucket(statuses, cards, |card| card.parent_id.is_none());
        lanes.push(BoardSwimlane {
            key: String::new(),
            label: "No parent".to_owned(),
            columns,
        });
    }

    Ok(lanes)
}

// ---------------------------------------------------------------------------
// Saved board configuration — the thin CRUD around the view
// ---------------------------------------------------------------------------

/// The longest a saved board name may be.
pub const MAX_BOARD_NAME: usize = 120;

/// A saved board configuration.
///
/// `wip_limits` is a real JSON object in the API, but sqlx's `json` feature is not
/// enabled in this workspace, so the column is read as text through [`BoardRow`]
/// and parsed here rather than decoded by a `#[sqlx(json)]` field.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// The display name, unique per project.
    pub name: String,
    /// The card whose children this board renders, or `None` for the top level.
    pub default_parent_id: Option<String>,
    /// The saved AQL quick filter, or `None`.
    pub aql_filter: Option<String>,
    /// `none`, `assignee` or `parent`.
    pub swimlane: String,
    /// Per-status WIP limits, a JSON object `{status_id: max}`.
    #[schema(value_type = Object)]
    pub wip_limits: serde_json::Value,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// The `boards` row exactly as stored, with `wip_limits` still text.
#[derive(Debug, FromRow)]
struct BoardRow {
    id: String,
    project_id: String,
    name: String,
    default_parent_id: Option<String>,
    aql_filter: Option<String>,
    swimlane: String,
    wip_limits: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<BoardRow> for Board {
    fn from(row: BoardRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            name: row.name,
            default_parent_id: row.default_parent_id,
            aql_filter: row.aql_filter,
            swimlane: row.swimlane,
            // The column is only ever written by `serde_json::to_string`, so a
            // parse failure means the row was tampered with; an empty object is
            // the safe, non-panicking reading.
            wip_limits: serde_json::from_str(&row.wip_limits)
                .unwrap_or_else(|_| serde_json::json!({})),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// The columns of `boards`, in the order [`BoardRow`]'s `FromRow` expects.
macro_rules! board_columns {
    () => {
        "id, project_id, name, default_parent_id, aql_filter, swimlane, wip_limits, \
         created_at, updated_at"
    };
}

/// Checks a saved board name, returning it trimmed.
///
/// # Errors
///
/// [`AppError::Validation`] for an empty or over-long name.
pub fn validate_board_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Board name must not be empty.".to_owned(),
        ));
    }
    if name.chars().count() > MAX_BOARD_NAME {
        return Err(AppError::Validation(format!(
            "Board name must be at most {MAX_BOARD_NAME} characters long."
        )));
    }
    Ok(name.to_owned())
}

/// Every saved board of a project, by name.
pub async fn list_boards(db: &Db, project_id: &str) -> AppResult<Vec<Board>> {
    let rows = sqlx::query_as::<_, BoardRow>(concat!(
        "SELECT ",
        board_columns!(),
        " FROM boards WHERE project_id = ? ORDER BY name"
    ))
    .bind(project_id)
    .fetch_all(db.reader())
    .await?;
    Ok(rows.into_iter().map(Board::from).collect())
}

/// Finds a saved board by id.
pub async fn find_board(db: &Db, id: &str) -> AppResult<Option<Board>> {
    Ok(sqlx::query_as::<_, BoardRow>(concat!(
        "SELECT ",
        board_columns!(),
        " FROM boards WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?
    .map(Board::from))
}

/// Finds a saved board by id inside a transaction.
pub async fn find_board_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Board>> {
    Ok(sqlx::query_as::<_, BoardRow>(concat!(
        "SELECT ",
        board_columns!(),
        " FROM boards WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .map(Board::from))
}

/// Whether a project already has a board of this name — the uniqueness check the
/// `UNIQUE (project_id, name)` constraint backs, surfaced as a friendly 409
/// rather than a raw constraint 500. `exclude` skips one board's own id on edit.
pub async fn board_name_taken(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    exclude: Option<&str>,
) -> AppResult<bool> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT id FROM boards WHERE project_id = ? AND name = ? COLLATE NOCASE \
           AND (? IS NULL OR id != ?)",
    )
    .bind(project_id)
    .bind(name)
    .bind(exclude)
    .bind(exclude)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(found.is_some())
}

/// A new saved board, ready to insert.
#[derive(Debug)]
pub struct NewBoard {
    /// The display name, already validated.
    pub name: String,
    /// The card whose children this board renders, or `None`.
    pub default_parent_id: Option<String>,
    /// The saved AQL filter, or `None`.
    pub aql_filter: Option<String>,
    /// `none`, `assignee` or `parent`.
    pub swimlane: String,
    /// Per-status WIP limits.
    pub wip_limits: serde_json::Value,
}

/// Inserts a saved board.
pub async fn insert_board(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    new: &NewBoard,
    now: DateTime<Utc>,
) -> AppResult<Board> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    let wip = serde_json::to_string(&new.wip_limits).unwrap_or_else(|_| "{}".to_owned());

    sqlx::query(
        "INSERT INTO boards \
           (id, project_id, name, default_parent_id, aql_filter, swimlane, wip_limits, \
            created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(&new.name)
    .bind(&new.default_parent_id)
    .bind(&new.aql_filter)
    .bind(&new.swimlane)
    .bind(&wip)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_board_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the board just inserted is missing")))
}

/// The fields [`apply_board_patch`] may change.
///
/// `Option<Option<T>>` on the nullable fields keeps absent (leave alone) and
/// `null` (clear) distinct.
#[allow(clippy::option_option)]
#[derive(Debug, Default)]
pub struct BoardPatch {
    /// The new name.
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub default_parent_id: Option<Option<String>>,
    /// Absent leaves it, `null` clears it, a value sets it.
    pub aql_filter: Option<Option<String>>,
    /// The new swimlane mode.
    pub swimlane: Option<String>,
    /// The new WIP limits.
    pub wip_limits: Option<serde_json::Value>,
}

impl BoardPatch {
    /// Whether the patch names any field.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.default_parent_id.is_none()
            && self.aql_filter.is_none()
            && self.swimlane.is_none()
            && self.wip_limits.is_none()
    }
}

/// Applies a patch to a saved board. Every column is written with `COALESCE`
/// against a sentinel, so an absent field is left exactly as it was.
pub async fn apply_board_patch(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    patch: &BoardPatch,
    now: DateTime<Utc>,
) -> AppResult<Board> {
    let current = find_board_tx(&mut *tx, id)
        .await?
        .ok_or(AppError::NotFound)?;

    let name = patch.name.clone().unwrap_or(current.name);
    let default_parent_id = match &patch.default_parent_id {
        Some(value) => value.clone(),
        None => current.default_parent_id,
    };
    let aql_filter = match &patch.aql_filter {
        Some(value) => value.clone(),
        None => current.aql_filter,
    };
    let swimlane = patch.swimlane.clone().unwrap_or(current.swimlane);
    let wip_value = patch.wip_limits.clone().unwrap_or(current.wip_limits);
    let wip = serde_json::to_string(&wip_value).unwrap_or_else(|_| "{}".to_owned());

    sqlx::query(
        "UPDATE boards SET \
           name = ?, default_parent_id = ?, aql_filter = ?, swimlane = ?, \
           wip_limits = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&name)
    .bind(&default_parent_id)
    .bind(&aql_filter)
    .bind(&swimlane)
    .bind(&wip)
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;

    find_board_tx(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the board just updated is missing")))
}

/// Deletes a saved board.
pub async fn delete_board(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM boards WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swimlane_parses_the_three_modes_and_rejects_others() {
        assert_eq!(Swimlane::from_query(None).unwrap(), Swimlane::None);
        assert_eq!(Swimlane::from_query(Some("none")).unwrap(), Swimlane::None);
        assert_eq!(
            Swimlane::from_query(Some("assignee")).unwrap(),
            Swimlane::Assignee
        );
        assert_eq!(
            Swimlane::from_query(Some("parent")).unwrap(),
            Swimlane::Parent
        );
        assert!(Swimlane::from_query(Some("epic")).is_err());
    }

    #[test]
    fn the_root_source_scopes_by_project_and_null_parent() {
        let project = sample_project();
        let source = board_source(&project, &BoardScope::Root, None).unwrap();
        assert_eq!(source, "project = \"ATLAS\" AND parent IS EMPTY");
    }

    #[test]
    fn a_child_source_scopes_by_the_parent_key() {
        let project = sample_project();
        let scope = BoardScope::Child {
            id: "card-id".to_owned(),
            key: "ATLAS-5".to_owned(),
        };
        let source = board_source(&project, &scope, None).unwrap();
        assert_eq!(source, "project = \"ATLAS\" AND parent = \"ATLAS-5\"");
    }

    #[test]
    fn a_filter_is_anded_onto_the_scope_and_its_order_by_is_dropped() {
        let project = sample_project();
        let source = board_source(
            &project,
            &BoardScope::Root,
            Some("assignee = currentUser() ORDER BY created DESC"),
        )
        .unwrap();
        // The scope and the filter predicate, ANDed; no ORDER BY survives.
        assert!(source.starts_with("project = \"ATLAS\" AND parent IS EMPTY AND ("));
        assert!(source.contains("assignee = currentUser()"), "{source}");
        assert!(!source.contains("ORDER BY"), "{source}");
    }

    #[test]
    fn an_empty_filter_adds_nothing() {
        let project = sample_project();
        let source = board_source(&project, &BoardScope::Root, Some("   ")).unwrap();
        assert_eq!(source, "project = \"ATLAS\" AND parent IS EMPTY");
    }

    #[test]
    fn a_broken_filter_is_a_bad_request() {
        let project = sample_project();
        let err = board_source(&project, &BoardScope::Root, Some("status =")).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    fn sample_project() -> Project {
        Project {
            id: "p1".to_owned(),
            key: "ATLAS".to_owned(),
            name: "Atlas".to_owned(),
            description: None,
            lead_id: None,
            avatar_url: None,
            cover_image_url: None,
            template: "programming".to_owned(),
            card_counter: 0,
            cycles_enabled: false,
            estimation_unit: crate::domain::EstimationUnit::None,
            archived_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

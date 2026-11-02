//! The activity feed: who changed what, turned into events that live sync, notifications and
//! automation all consume.
//!
//! # Why the feed tails the database instead of handlers publishing
//!
//! A card can change through the REST API, a board drag, a workflow post-function, a GitHub
//! webhook, a smart commit, the scheduler, an automation rule — and the `atlas mcp`
//! *subprocess* an agent drives, which shares the database but not this process's memory.
//! Publishing from each of those would be one forgotten call away from a silent gap, and the
//! subprocess could not do it at all. The changelog, comment and card tables already record
//! every one of those writes, in the same transaction as the change; [`tick`] reads what is
//! new since a persisted cursor and fans it out. The database is the bus.
//!
//! # Actor attribution
//!
//! [`with_actor`] runs a future with a task-local actor tag (`automation:<rule>:<depth>`,
//! `agent:<session>`); every changelog, comment and card row written inside it records the
//! tag in its `actor` column. That is how the automation engine knows not to react to its own
//! changes past a depth, without threading an argument through every domain function.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio::sync::broadcast;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

tokio::task_local! {
    static ACTOR: String;
}

/// Runs `future` with every write inside it attributed to `actor`.
pub async fn with_actor<F: Future>(actor: String, future: F) -> F::Output {
    ACTOR.scope(actor, future).await
}

/// The actor tag of the current task, if one is set.
pub fn current_actor() -> Option<String> {
    ACTOR.try_with(Clone::clone).ok()
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One field's before and after, as the changelog recorded it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    /// The logical field name (`status`, `assignee`, `cf:<id>`).
    pub field: String,
    /// Raw before (an id for references).
    pub from_value: Option<String>,
    /// Human before.
    pub from_display: Option<String>,
    /// Raw after.
    pub to_value: Option<String>,
    /// Human after.
    pub to_display: Option<String>,
}

/// What happened.
#[derive(Debug, Clone)]
pub enum EventKind {
    /// A card was created.
    CardCreated,
    /// One or more fields changed in one write.
    CardUpdated {
        /// The changes, as recorded.
        changes: Vec<FieldChange>,
    },
    /// A comment was posted.
    CommentAdded {
        /// The comment's id.
        comment_id: String,
        /// Its markdown source.
        body: String,
    },
}

impl EventKind {
    /// The wire name: `card.created`, `card.updated`, `comment.added`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::CardCreated => "card.created",
            Self::CardUpdated { .. } => "card.updated",
            Self::CommentAdded { .. } => "comment.added",
        }
    }
}

/// The card an event is about, as it is now.
#[derive(Debug, Clone, FromRow)]
pub struct CardRef {
    /// Card id.
    pub id: String,
    /// Card key.
    pub key: String,
    /// One-line summary.
    pub summary: String,
    /// Description source.
    pub description: Option<String>,
    /// Owning project.
    pub project_id: String,
    /// Owning project's key.
    pub project_key: String,
    /// Current assignee.
    pub assignee_id: Option<String>,
    /// Current reporter.
    pub reporter_id: Option<String>,
    /// Current status name.
    pub status_name: String,
    /// Card type id.
    pub type_id: String,
}

/// One thing that happened to one card.
#[derive(Debug, Clone)]
pub struct Event {
    /// What.
    pub kind: EventKind,
    /// To which card, as it is now.
    pub card: CardRef,
    /// The person it is attributed to.
    pub author_id: Option<String>,
    /// What made it, when not a person by hand (`automation:…`, `agent:…`).
    pub actor: Option<String>,
    /// When.
    pub at: DateTime<Utc>,
}

impl Event {
    /// The change to `field` in this event, if it is an update that touched it.
    pub fn change(&self, field: &str) -> Option<&FieldChange> {
        match &self.kind {
            EventKind::CardUpdated { changes } => changes.iter().find(|c| c.field == field),
            _ => None,
        }
    }

    /// The automation depth that produced this event: 0 for a human or agent.
    pub fn automation_depth(&self) -> u32 {
        self.actor
            .as_deref()
            .and_then(|a| a.strip_prefix("automation:"))
            .and_then(|rest| rest.rsplit(':').next())
            .and_then(|d| d.parse().ok())
            .unwrap_or(0)
    }

    /// The rule that produced this event, if one did.
    pub fn automation_rule(&self) -> Option<&str> {
        self.actor
            .as_deref()
            .and_then(|a| a.strip_prefix("automation:"))
            .and_then(|rest| rest.split(':').next())
    }

    /// The JSON pushed to live-sync subscribers.
    pub fn to_live_json(&self) -> Value {
        let fields: Vec<&str> = match &self.kind {
            EventKind::CardUpdated { changes } => {
                changes.iter().map(|c| c.field.as_str()).collect()
            }
            _ => Vec::new(),
        };
        json!({
            "type": self.kind.name(),
            "projectId": self.card.project_id,
            "projectKey": self.card.project_key,
            "cardId": self.card.id,
            "cardKey": self.card.key,
            "authorId": self.author_id,
            "fields": fields,
            "at": self.at,
        })
    }
}

// ---------------------------------------------------------------------------
// The hub
// ---------------------------------------------------------------------------

/// A message on the live hub. `project_id` and `user_id` say who may receive it.
#[derive(Debug, Clone)]
pub struct HubMessage {
    /// Only subscribers who can see this project receive it. `None` = not project-bound.
    pub project_id: Option<String>,
    /// Only this user receives it. `None` = anyone (subject to `project_id`).
    pub user_id: Option<String>,
    /// The JSON frame sent as-is.
    pub payload: Value,
}

/// The in-process broadcast every WebSocket subscribes to.
///
/// Lossy by design: a subscriber that falls more than [`HUB_CAPACITY`] messages behind gets a
/// `Lagged` error and the frontend refetches — live sync is a cache-invalidation hint, never
/// the source of truth.
#[derive(Debug, Clone)]
pub struct Hub {
    sender: broadcast::Sender<Arc<HubMessage>>,
}

/// How many messages a slow subscriber may fall behind before it is told to resync.
pub const HUB_CAPACITY: usize = 1024;

impl Default for Hub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(HUB_CAPACITY);
        Self { sender }
    }
}

impl Hub {
    /// Publishes a message. Having no subscribers is not an error.
    pub fn publish(&self, message: HubMessage) {
        let _ = self.sender.send(Arc::new(message));
    }

    /// A new subscription.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<HubMessage>> {
        self.sender.subscribe()
    }
}

// ---------------------------------------------------------------------------
// The pump
// ---------------------------------------------------------------------------

/// Rows read per stream per tick. A burst larger than this drains over several ticks.
const BATCH: i64 = 500;

/// How far each stream has been read, by rowid.
#[derive(Debug, FromRow)]
#[allow(clippy::struct_field_names)] // the columns' names; renaming them would only obscure that
struct Cursor {
    card_rowid: i64,
    history_rowid: i64,
    comment_rowid: i64,
}

#[derive(Debug, FromRow)]
struct CreatedRow {
    rowid: i64,
    id: String,
    creator_id: String,
    actor: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct HistoryRow {
    rowid: i64,
    card_id: String,
    author_id: Option<String>,
    actor: Option<String>,
    created_at: DateTime<Utc>,
    field: String,
    from_value: Option<String>,
    from_display: Option<String>,
    to_value: Option<String>,
    to_display: Option<String>,
}

#[derive(Debug, FromRow)]
struct CommentRow {
    rowid: i64,
    id: String,
    card_id: String,
    author_id: String,
    actor: Option<String>,
    body: String,
    created_at: DateTime<Utc>,
}

async fn card_ref(
    db: &Db,
    cache: &mut HashMap<String, Option<CardRef>>,
    id: &str,
) -> AppResult<Option<CardRef>> {
    if let Some(found) = cache.get(id) {
        return Ok(found.clone());
    }
    let found = sqlx::query_as::<_, CardRef>(
        "SELECT c.id, c.key, c.summary, c.description, c.project_id, p.key AS project_key, \
                c.assignee_id, c.reporter_id, s.name AS status_name, c.type_id \
           FROM cards c JOIN projects p ON p.id = c.project_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE c.id = ? AND c.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?;
    cache.insert(id.to_owned(), found.clone());
    Ok(found)
}

/// The card as it was when it was created.
///
/// [`card_ref`] reads the card *now*, and by the time the pump runs the card may already have
/// moved on — reassigned, re-described — in the same second. A creation event built from the
/// current row would then announce the *later* assignee as the creator's choice. The changelog
/// knows better: the first change to a field records what it was changed *from*, which is its
/// value at creation.
async fn as_created(db: &Db, mut card: CardRef) -> AppResult<CardRef> {
    let first = |field: &'static str| {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT from_value FROM card_history WHERE card_id = ? AND field = ? \
             ORDER BY rowid LIMIT 1",
        )
        .bind(card.id.clone())
        .bind(field)
        .fetch_optional(db.reader())
    };
    if let Some(initial) = first("assignee").await? {
        card.assignee_id = initial;
    }
    if let Some(initial) = first("description").await? {
        card.description = initial;
    }
    Ok(card)
}

/// Reads everything new since the cursor, advances it, and returns the events in order.
///
/// Cards in the trash produce no events. The cursor is advanced past them anyway: an event
/// about a deleted card has no one to tell.
// Three streams read, grouped, merged and the cursor advanced, in one place: splitting it would
// separate the reads from the cursor write that must reflect exactly those reads.
#[allow(clippy::too_many_lines)]
pub async fn read_new(db: &Db) -> AppResult<Vec<Event>> {
    let cursor = sqlx::query_as::<_, Cursor>(
        "SELECT card_rowid, history_rowid, comment_rowid FROM activity_cursor WHERE id = 1",
    )
    .fetch_one(db.reader())
    .await?;

    let created = sqlx::query_as::<_, CreatedRow>(
        "SELECT rowid, id, creator_id, actor, created_at FROM cards \
          WHERE rowid > ? ORDER BY rowid LIMIT ?",
    )
    .bind(cursor.card_rowid)
    .bind(BATCH)
    .fetch_all(db.reader())
    .await?;
    let history = sqlx::query_as::<_, HistoryRow>(
        "SELECT rowid, card_id, author_id, actor, created_at, field, from_value, from_display, \
                to_value, to_display FROM card_history \
          WHERE rowid > ? ORDER BY rowid LIMIT ?",
    )
    .bind(cursor.history_rowid)
    .bind(BATCH)
    .fetch_all(db.reader())
    .await?;
    let comments = sqlx::query_as::<_, CommentRow>(
        "SELECT rowid, id, card_id, author_id, actor, body, created_at FROM comments \
          WHERE rowid > ? ORDER BY rowid LIMIT ?",
    )
    .bind(cursor.comment_rowid)
    .bind(BATCH)
    .fetch_all(db.reader())
    .await?;

    let next = Cursor {
        card_rowid: created.last().map_or(cursor.card_rowid, |r| r.rowid),
        history_rowid: history.last().map_or(cursor.history_rowid, |r| r.rowid),
        comment_rowid: comments.last().map_or(cursor.comment_rowid, |r| r.rowid),
    };

    let mut cache: HashMap<String, Option<CardRef>> = HashMap::new();
    // (at, stream order, event) — sorted by time, then creations before their own edits.
    let mut events: Vec<(DateTime<Utc>, u8, i64, Event)> = Vec::new();

    for row in created {
        if let Some(card) = card_ref(db, &mut cache, &row.id).await? {
            let card = as_created(db, card).await?;
            events.push((
                row.created_at,
                0,
                row.rowid,
                Event {
                    kind: EventKind::CardCreated,
                    card,
                    author_id: Some(row.creator_id),
                    actor: row.actor,
                    at: row.created_at,
                },
            ));
        }
    }

    // Consecutive changelog rows from one write (same card, instant, author, actor) are one
    // event with several changes — a board drag that also sets the resolution is one move.
    let mut group: Option<(HistoryRow, Vec<FieldChange>)> = None;
    let mut groups: Vec<(HistoryRow, Vec<FieldChange>)> = Vec::new();
    for row in history {
        let change = FieldChange {
            field: row.field.clone(),
            from_value: row.from_value.clone(),
            from_display: row.from_display.clone(),
            to_value: row.to_value.clone(),
            to_display: row.to_display.clone(),
        };
        match &mut group {
            Some((head, changes))
                if head.card_id == row.card_id
                    && head.created_at == row.created_at
                    && head.author_id == row.author_id
                    && head.actor == row.actor =>
            {
                changes.push(change);
            }
            _ => {
                if let Some(done) = group.take() {
                    groups.push(done);
                }
                group = Some((row, vec![change]));
            }
        }
    }
    groups.extend(group);
    for (head, changes) in groups {
        if let Some(card) = card_ref(db, &mut cache, &head.card_id).await? {
            events.push((
                head.created_at,
                1,
                head.rowid,
                Event {
                    kind: EventKind::CardUpdated { changes },
                    card,
                    author_id: head.author_id,
                    actor: head.actor,
                    at: head.created_at,
                },
            ));
        }
    }

    for row in comments {
        if let Some(card) = card_ref(db, &mut cache, &row.card_id).await? {
            events.push((
                row.created_at,
                2,
                row.rowid,
                Event {
                    kind: EventKind::CommentAdded {
                        comment_id: row.id,
                        body: row.body,
                    },
                    card,
                    author_id: Some(row.author_id),
                    actor: row.actor,
                    at: row.created_at,
                },
            ));
        }
    }

    events.sort_by_key(|e| (e.0, e.1, e.2));

    let mut tx = db.begin_write().await?;
    sqlx::query(
        "UPDATE activity_cursor SET card_rowid = ?, history_rowid = ?, comment_rowid = ?, \
         updated_at = ? WHERE id = 1",
    )
    .bind(next.card_rowid)
    .bind(next.history_rowid)
    .bind(next.comment_rowid)
    .bind(to_sql_timestamp(Utc::now()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(events.into_iter().map(|(_, _, _, event)| event).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(actor: Option<&str>) -> Event {
        Event {
            kind: EventKind::CardCreated,
            card: CardRef {
                id: "c".into(),
                key: "A-1".into(),
                summary: "s".into(),
                description: None,
                project_id: "p".into(),
                project_key: "A".into(),
                assignee_id: None,
                reporter_id: None,
                status_name: "To Do".into(),
                type_id: "t".into(),
            },
            author_id: None,
            actor: actor.map(str::to_owned),
            at: Utc::now(),
        }
    }

    #[test]
    fn the_actor_tag_carries_the_rule_and_its_depth() {
        assert_eq!(event(None).automation_depth(), 0);
        assert_eq!(event(Some("agent:s1")).automation_depth(), 0);
        let e = event(Some("automation:rule-1:3"));
        assert_eq!(e.automation_depth(), 3);
        assert_eq!(e.automation_rule(), Some("rule-1"));
    }

    #[tokio::test]
    async fn the_actor_is_visible_only_inside_its_scope() {
        assert_eq!(current_actor(), None);
        let inside = with_actor("automation:r:1".into(), async { current_actor() }).await;
        assert_eq!(inside.as_deref(), Some("automation:r:1"));
        assert_eq!(current_actor(), None);
    }

    #[test]
    fn a_hub_with_no_subscribers_does_not_fail() {
        Hub::default().publish(HubMessage {
            project_id: None,
            user_id: None,
            payload: json!({}),
        });
    }
}

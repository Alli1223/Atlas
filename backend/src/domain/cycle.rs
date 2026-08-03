//! Cycles: a project's sprints/iterations, and which cards belong to them.
//!
//! # The state machine
//!
//! `future -> active -> closed`, and **closed is not terminal** — a closed cycle can be
//! [`reopen`]ed back to active (`docs/research/corrections.md` #7, Jira's own "Reopen
//! sprint" action). Starting requires both a start and an end date, together, never one
//! alone; reopening replans the end date rather than reusing the original.
//!
//! At most one cycle may be active per project at a time — Jira's "parallel sprints" is an
//! opt-in most instances never turn on, and the research corrected an earlier draft that
//! would have scoped it per-board, which does not map onto Atlas's model anyway (a board
//! *is* a project here, not a separate entity spanning several). [`start`] and [`reopen`]
//! both enforce this; nothing about the schema forecloses lifting it later.
//!
//! # Scope tracking
//!
//! [`card_cycle`] is a many-to-many join with history, not a simple FK on cards — a card
//! passes through several cycles over its life (carried over on [`complete`]), and a closed
//! cycle must keep the membership it had for commitment/scope-creep reporting. See the
//! migration's own comment for the exact shape.
//!
//! # What this module deliberately does not do yet
//!
//! [`complete`]'s carry-over removes an incomplete card from the closing cycle and, per the
//! caller's choice, adds it to another. [`reopen`] does **not** try to automatically restore
//! those carried-away cards back into scope — Jira's own behaviour here ("restores completed
//! and incomplete items, except ones moved into an active cycle in the interim") has enough
//! edge cases around concurrent moves to deserve its own careful pass; for now, reopening a
//! cycle only flips its state and replans its end date, and an admin who wants a carried-away
//! card back adds it by hand with [`add_card`].
//!
//! [`cycle_snapshot`] rows (commitment/completion/burndown data) are not written by this
//! module at all yet — that lands with the reporting work this data exists for.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, SqliteConnection, Type};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

use super::project::Project;

/// A cycle's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CycleState {
    /// Created, not yet started. Dates are unset.
    Future,
    /// Running. Exactly one cycle per project may be in this state.
    Active,
    /// Finished. Not terminal — see [`reopen`].
    Closed,
}

impl CycleState {
    /// The state's database and JSON spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Future => "future",
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for CycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a cycle state could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown cycle state {0:?}")]
pub struct CycleStateError(String);

impl FromStr for CycleState {
    type Err = CycleStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "future" => Ok(Self::Future),
            "active" => Ok(Self::Active),
            "closed" => Ok(Self::Closed),
            other => Err(CycleStateError(other.to_owned())),
        }
    }
}

// The same sqlx shape as `StatusCategory`/`EstimationUnit`: stored as TEXT, validated on
// read. The CHECK constraint in the migration and this Decode impl are two independent
// guards against a state that means nothing.

impl Type<Sqlite> for CycleState {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for CycleState {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CycleState {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// A row of `cycles`.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Cycle {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    pub name: String,
    pub goal: Option<String>,
    /// `None` until started.
    pub start_date: Option<NaiveDate>,
    /// `None` until started; replanned on [`reopen`].
    pub end_date: Option<NaiveDate>,
    pub state: CycleState,
    /// Display order within the project's cycle list.
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A new cycle, ready to insert. Always created `future`, with no dates.
#[derive(Debug)]
pub struct NewCycle<'a> {
    pub project_id: &'a str,
    pub name: &'a str,
    pub goal: Option<&'a str>,
}

/// Where a cycle's incomplete cards go when it [`complete`]s.
#[derive(Debug)]
pub enum CarryTo<'a> {
    /// Out of any cycle — back to the plain backlog.
    Backlog,
    /// An existing `future` or `active` cycle in the same project.
    ExistingCycle(&'a str),
    /// A brand new `future` cycle, created for this purpose.
    NewCycle(&'a str),
}

macro_rules! cycle_columns {
    () => {
        "id, project_id, name, goal, start_date, end_date, state, position, created_at, updated_at"
    };
}

/// Inserts a cycle. Requires `project.cycles_enabled` — a project that has not turned
/// cycles on has no home for one.
pub async fn insert(
    tx: &mut SqliteConnection,
    project: &Project,
    new: &NewCycle<'_>,
    now: DateTime<Utc>,
) -> AppResult<Cycle> {
    if !project.cycles_enabled {
        return Err(AppError::Validation(
            "this project does not have cycles enabled".to_owned(),
        ));
    }

    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    let position: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM cycles WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO cycles (id, project_id, name, goal, state, position, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'future', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&project.id)
    .bind(new.name)
    .bind(new.goal)
    .bind(position)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the cycle just inserted is missing")))
}

/// Finds a cycle by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Cycle>> {
    Ok(sqlx::query_as::<_, Cycle>(concat!(
        "SELECT ",
        cycle_columns!(),
        " FROM cycles WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a cycle by id inside an open transaction.
pub async fn find_by_id_tx(tx: &mut SqliteConnection, id: &str) -> AppResult<Option<Cycle>> {
    Ok(sqlx::query_as::<_, Cycle>(concat!(
        "SELECT ",
        cycle_columns!(),
        " FROM cycles WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Every cycle of a project, state then display order.
pub async fn list_for_project(db: &Db, project_id: &str) -> AppResult<Vec<Cycle>> {
    Ok(sqlx::query_as::<_, Cycle>(concat!(
        "SELECT ",
        cycle_columns!(),
        " FROM cycles WHERE project_id = ? \
         ORDER BY CASE state WHEN 'active' THEN 0 WHEN 'future' THEN 1 ELSE 2 END, position"
    ))
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// The fields a patch may carry. `None` leaves a field alone; `goal: Some(None)` clears it.
#[derive(Debug, Default)]
pub struct CyclePatch {
    pub name: Option<String>,
    pub goal: Option<Option<String>>,
}

/// Renames a cycle and/or edits its goal. Legal in any state — even a closed cycle's name
/// was presumably a typo someone wants to fix.
pub async fn apply_patch(
    tx: &mut SqliteConnection,
    cycle: &Cycle,
    patch: &CyclePatch,
    now: DateTime<Utc>,
) -> AppResult<Cycle> {
    sqlx::query(
        "UPDATE cycles SET name = COALESCE(?, name), goal = CASE WHEN ? THEN ? ELSE goal END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&patch.name)
    .bind(patch.goal.is_some())
    .bind(patch.goal.as_ref().and_then(|g| g.as_deref()))
    .bind(to_sql_timestamp(now))
    .bind(&cycle.id)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &cycle.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the cycle just patched is missing")))
}

/// Starts a cycle: `future -> active`. Requires both dates, `end >= start`, and no other
/// active cycle in the project. Every card currently in scope is marked
/// `in_scope_at_start` — the commitment baseline scope-creep is measured against.
pub async fn start(
    tx: &mut SqliteConnection,
    cycle: &Cycle,
    start_date: NaiveDate,
    end_date: NaiveDate,
    now: DateTime<Utc>,
) -> AppResult<Cycle> {
    if cycle.state != CycleState::Future {
        return Err(AppError::Conflict(format!(
            "only a future cycle can be started (this one is {})",
            cycle.state
        )));
    }
    if end_date < start_date {
        return Err(AppError::Validation(
            "a cycle's end date cannot be before its start date".to_owned(),
        ));
    }
    ensure_no_other_active(tx, &cycle.project_id, &cycle.id).await?;

    sqlx::query(
        "UPDATE cycles SET state = 'active', start_date = ?, end_date = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(start_date)
    .bind(end_date)
    .bind(to_sql_timestamp(now))
    .bind(&cycle.id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE card_cycle SET in_scope_at_start = 1 WHERE cycle_id = ? AND removed_at IS NULL",
    )
    .bind(&cycle.id)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &cycle.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the cycle just started is missing")))
}

/// Completes a cycle: `active -> closed`. Every still-incomplete card (its status is not in
/// the `done` category) leaves this cycle's scope and, per `carry_to`, either goes nowhere
/// (the backlog), joins an existing cycle, or seeds a brand new one.
pub async fn complete(
    tx: &mut SqliteConnection,
    cycle: &Cycle,
    carry_to: &CarryTo<'_>,
    now: DateTime<Utc>,
) -> AppResult<Cycle> {
    if cycle.state != CycleState::Active {
        return Err(AppError::Conflict(format!(
            "only an active cycle can be completed (this one is {})",
            cycle.state
        )));
    }

    let incomplete: Vec<String> = sqlx::query_scalar(
        "SELECT cc.card_id FROM card_cycle cc \
           JOIN cards c ON c.id = cc.card_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE cc.cycle_id = ? AND cc.removed_at IS NULL AND s.category != 'done'",
    )
    .bind(&cycle.id)
    .fetch_all(&mut *tx)
    .await?;

    if !incomplete.is_empty() {
        let timestamp = to_sql_timestamp(now);
        sqlx::query(
            "UPDATE card_cycle SET removed_at = ? WHERE cycle_id = ? AND card_id IN (\
               SELECT value FROM json_each(?))",
        )
        .bind(&timestamp)
        .bind(&cycle.id)
        .bind(serde_json::to_string(&incomplete).map_err(AppError::internal)?)
        .execute(&mut *tx)
        .await?;

        let target_cycle_id = match carry_to {
            CarryTo::Backlog => None,
            CarryTo::ExistingCycle(id) => {
                let target = find_by_id_tx(&mut *tx, id)
                    .await?
                    .filter(|c| c.project_id == cycle.project_id)
                    .ok_or_else(|| {
                        AppError::Validation(
                            "no such cycle in this project to carry into".to_owned(),
                        )
                    })?;
                if target.state == CycleState::Closed {
                    return Err(AppError::Validation(
                        "cannot carry cards into a closed cycle".to_owned(),
                    ));
                }
                Some(target.id)
            }
            CarryTo::NewCycle(name) => {
                let project = super::project::find_by_id_tx(&mut *tx, &cycle.project_id)
                    .await?
                    .ok_or_else(|| {
                        AppError::internal(anyhow::anyhow!("the cycle's project is missing"))
                    })?;
                let created = insert(
                    &mut *tx,
                    &project,
                    &NewCycle {
                        project_id: &cycle.project_id,
                        name,
                        goal: None,
                    },
                    now,
                )
                .await?;
                Some(created.id)
            }
        };

        if let Some(target_cycle_id) = target_cycle_id {
            for card_id in &incomplete {
                add_card(tx, card_id, &target_cycle_id, now).await?;
            }
        }
    }

    sqlx::query("UPDATE cycles SET state = 'closed', updated_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now))
        .bind(&cycle.id)
        .execute(&mut *tx)
        .await?;

    find_by_id_tx(&mut *tx, &cycle.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the cycle just completed is missing")))
}

/// Reopens a cycle: `closed -> active`, with a replanned end date. See the module docs for
/// what this deliberately does not attempt (automatic carried-card restoration).
pub async fn reopen(
    tx: &mut SqliteConnection,
    cycle: &Cycle,
    new_end_date: NaiveDate,
    now: DateTime<Utc>,
) -> AppResult<Cycle> {
    if cycle.state != CycleState::Closed {
        return Err(AppError::Conflict(format!(
            "only a closed cycle can be reopened (this one is {})",
            cycle.state
        )));
    }
    ensure_no_other_active(tx, &cycle.project_id, &cycle.id).await?;

    sqlx::query("UPDATE cycles SET state = 'active', end_date = ?, updated_at = ? WHERE id = ?")
        .bind(new_end_date)
        .bind(to_sql_timestamp(now))
        .bind(&cycle.id)
        .execute(&mut *tx)
        .await?;

    find_by_id_tx(&mut *tx, &cycle.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the cycle just reopened is missing")))
}

/// Refuses if the project already has an active cycle other than `excluding_id`.
async fn ensure_no_other_active(
    tx: &mut SqliteConnection,
    project_id: &str,
    excluding_id: &str,
) -> AppResult<()> {
    let already_active: Option<String> = sqlx::query_scalar(
        "SELECT id FROM cycles WHERE project_id = ? AND state = 'active' AND id != ? LIMIT 1",
    )
    .bind(project_id)
    .bind(excluding_id)
    .fetch_optional(&mut *tx)
    .await?;

    if already_active.is_some() {
        return Err(AppError::Conflict(
            "another cycle in this project is already active; parallel cycles are not \
             supported yet"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Adds a card to a cycle, or — if it was previously removed — re-adds it as a fresh
/// membership event. Always `in_scope_at_start = 0`: that flag means "was here when the
/// cycle *started*", never "is here right now", so an add always starts false regardless of
/// the cycle's current state — [`start`] is the only place it becomes true.
pub async fn add_card(
    tx: &mut SqliteConnection,
    card_id: &str,
    cycle_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let Some(cycle) = find_by_id_tx(&mut *tx, cycle_id).await? else {
        return Err(AppError::NotFound);
    };
    if cycle.state == CycleState::Closed {
        return Err(AppError::Conflict(
            "cannot add a card to a closed cycle".to_owned(),
        ));
    }

    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "INSERT INTO card_cycle (card_id, cycle_id, added_at, removed_at, in_scope_at_start) \
         VALUES (?, ?, ?, NULL, 0) \
         ON CONFLICT (card_id, cycle_id) DO UPDATE SET \
            added_at = excluded.added_at, removed_at = NULL, in_scope_at_start = 0",
    )
    .bind(card_id)
    .bind(cycle_id)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Removes a card from a cycle. Returns whether it was actually a member.
pub async fn remove_card(
    tx: &mut SqliteConnection,
    card_id: &str,
    cycle_id: &str,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    let result = sqlx::query(
        "UPDATE card_cycle SET removed_at = ? \
          WHERE card_id = ? AND cycle_id = ? AND removed_at IS NULL",
    )
    .bind(to_sql_timestamp(now))
    .bind(card_id)
    .bind(cycle_id)
    .execute(&mut *tx)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// A card's current cycle (the one with no `removed_at`), if it is in one. A card belongs
/// to at most one *current* cycle even though it may have passed through several over time.
pub async fn current_cycle_for_card(db: &Db, card_id: &str) -> AppResult<Option<Cycle>> {
    Ok(sqlx::query_as::<_, Cycle>(concat!(
        "SELECT ",
        "c.id, c.project_id, c.name, c.goal, c.start_date, c.end_date, c.state, c.position, \
         c.created_at, c.updated_at",
        " FROM cycles c JOIN card_cycle cc ON cc.cycle_id = c.id \
         WHERE cc.card_id = ? AND cc.removed_at IS NULL"
    ))
    .bind(card_id)
    .fetch_optional(db.reader())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::StatusCategory as ConfigStatusCategory;
    use crate::domain::card::{self, CardPatch, NewCard, Placement};
    use crate::domain::config;
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    /// A migrated database with a `Programming` project (cycles enabled), one card type,
    /// and a seeded user to attribute cards to.
    async fn fixture() -> (Db, TempDb, Project, String, String) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let creator = user::insert(
            &mut tx,
            &user::NewUser {
                username: "pm".to_owned(),
                email: None,
                display_name: "PM".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();
        let project = template::create_project(
            &mut tx,
            Template::Programming,
            "ATLAS",
            "Atlas",
            None,
            None,
            now(),
        )
        .await
        .unwrap();
        let type_id: String = sqlx::query_scalar(
            "SELECT id FROM card_types WHERE project_id = ? ORDER BY level DESC, name LIMIT 1",
        )
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, project, creator.id, type_id)
    }

    /// A card in the fixture's project, in its default (todo) status.
    async fn make_card(db: &Db, project: &Project, type_id: &str, creator_id: &str) -> card::Card {
        let mut tx = db.begin_write().await.unwrap();
        let created = card::create(
            &mut tx,
            project,
            &NewCard {
                type_id: type_id.to_owned(),
                parent_id: None,
                summary: "A card".to_owned(),
                description: None,
                status_id: None,
                priority_id: None,
                assignee_id: None,
                reporter_id: None,
                due_date: None,
                start_date: None,
                estimate: None,
                placement: Placement::Bottom,
            },
            creator_id,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        created
    }

    /// Moves a card to the project's first Done-category status.
    async fn mark_done(db: &Db, project: &Project, card: &card::Card, creator_id: &str) {
        let mut tx = db.begin_write().await.unwrap();
        let done =
            config::first_status_in_category_tx(&mut tx, &project.id, ConfigStatusCategory::Done)
                .await
                .unwrap()
                .expect("the Programming template has a Done status");
        card::update(
            &mut tx,
            card,
            &CardPatch {
                status_id: Some(done.id),
                ..CardPatch::default()
            },
            Some(creator_id),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn insert_cycle(db: &Db, project: &Project, name: &str) -> Cycle {
        let mut tx = db.begin_write().await.unwrap();
        let cycle = insert(
            &mut tx,
            project,
            &NewCycle {
                project_id: &project.id,
                name,
                goal: None,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        cycle
    }

    #[tokio::test]
    async fn a_cycle_requires_the_project_to_have_cycles_enabled() {
        let (db, _temp, _project, _creator, _type_id) = fixture().await;
        let mut tx = db.begin_write().await.unwrap();
        let blank = template::create_project(
            &mut tx,
            Template::Blank,
            "OTHER",
            "Other",
            None,
            None,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(!blank.cycles_enabled);

        let mut tx = db.begin_write().await.unwrap();
        let err = insert(
            &mut tx,
            &blank,
            &NewCycle {
                project_id: &blank.id,
                name: "Sprint 1",
                goal: None,
            },
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn starting_requires_a_future_cycle_both_dates_and_end_not_before_start() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;

        let start_date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let end_date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let err = start(&mut tx, &cycle, start_date, end_date, now())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "{err:?}");
        tx.rollback().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let started = start(&mut tx, &cycle, start_date, end_date.max(start_date), now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(started.state, CycleState::Active);
        assert_eq!(started.start_date, Some(start_date));

        // Starting an already-active cycle is a conflict, not a silent no-op.
        let mut tx = db.begin_write().await.unwrap();
        let err = start(&mut tx, &started, start_date, start_date, now())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn only_one_cycle_may_be_active_per_project_at_once() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let a = insert_cycle(&db, &project, "Sprint A").await;
        let b = insert_cycle(&db, &project, "Sprint B").await;
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut tx = db.begin_write().await.unwrap();
        start(&mut tx, &a, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let err = start(&mut tx, &b, d, d, now()).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn starting_marks_current_members_in_scope_but_a_later_add_is_not() {
        let (db, _temp, project, creator, type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let early = make_card(&db, &project, &type_id, &creator).await;

        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &early.id, &cycle.id, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut tx = db.begin_write().await.unwrap();
        start(&mut tx, &cycle, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();

        let late = make_card(&db, &project, &type_id, &creator).await;
        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &late.id, &cycle.id, now()).await.unwrap();
        tx.commit().await.unwrap();

        let early_flag: i64 =
            sqlx::query_scalar("SELECT in_scope_at_start FROM card_cycle WHERE card_id = ?")
                .bind(&early.id)
                .fetch_one(db.reader())
                .await
                .unwrap();
        let late_flag: i64 =
            sqlx::query_scalar("SELECT in_scope_at_start FROM card_cycle WHERE card_id = ?")
                .bind(&late.id)
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(
            early_flag, 1,
            "present before start must be marked in-scope"
        );
        assert_eq!(late_flag, 0, "added after start must not be");
    }

    #[tokio::test]
    async fn completing_leaves_done_cards_in_place_and_drops_incomplete_ones_to_the_backlog() {
        let (db, _temp, project, creator, type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let done_card = make_card(&db, &project, &type_id, &creator).await;
        let todo_card = make_card(&db, &project, &type_id, &creator).await;

        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &done_card.id, &cycle.id, now())
            .await
            .unwrap();
        add_card(&mut tx, &todo_card.id, &cycle.id, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        mark_done(&db, &project, &done_card, &creator).await;

        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let cycle = start(&mut tx, &cycle, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let closed = complete(&mut tx, &cycle, &CarryTo::Backlog, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(closed.state, CycleState::Closed);

        assert!(
            current_cycle_for_card(&db, &done_card.id)
                .await
                .unwrap()
                .is_some(),
            "a done card stays in the closed cycle's record"
        );
        assert!(
            current_cycle_for_card(&db, &todo_card.id)
                .await
                .unwrap()
                .is_none(),
            "an incomplete card carried to the backlog has no current cycle"
        );
    }

    #[tokio::test]
    async fn completing_can_carry_incomplete_cards_into_an_existing_or_a_brand_new_cycle() {
        let (db, _temp, project, creator, type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let target = insert_cycle(&db, &project, "Sprint 2").await;
        let card_a = make_card(&db, &project, &type_id, &creator).await;
        let card_b = make_card(&db, &project, &type_id, &creator).await;

        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &card_a.id, &cycle.id, now())
            .await
            .unwrap();
        add_card(&mut tx, &card_b.id, &cycle.id, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let cycle = start(&mut tx, &cycle, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        complete(&mut tx, &cycle, &CarryTo::ExistingCycle(&target.id), now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let a_cycle = current_cycle_for_card(&db, &card_a.id).await.unwrap();
        assert_eq!(a_cycle.map(|c| c.id), Some(target.id.clone()));

        // A second cycle exercising CarryTo::NewCycle, independently.
        let cycle2 = insert_cycle(&db, &project, "Sprint 3").await;
        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &card_b.id, &cycle2.id, now())
            .await
            .unwrap();
        remove_card(&mut tx, &card_b.id, &target.id, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let cycle2 = start(&mut tx, &cycle2, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        complete(&mut tx, &cycle2, &CarryTo::NewCycle("Sprint 4"), now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let b_cycle = current_cycle_for_card(&db, &card_b.id).await.unwrap();
        assert_eq!(b_cycle.as_ref().map(|c| c.name.as_str()), Some("Sprint 4"));
        assert_eq!(b_cycle.unwrap().state, CycleState::Future);
    }

    #[tokio::test]
    async fn only_an_active_cycle_can_be_completed() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let mut tx = db.begin_write().await.unwrap();
        let err = complete(&mut tx, &cycle, &CarryTo::Backlog, now())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn reopening_a_closed_cycle_replans_the_end_date_but_keeps_the_start_date() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let start_date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let end_date = NaiveDate::from_ymd_opt(2026, 1, 14).unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let cycle = start(&mut tx, &cycle, start_date, end_date, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let closed = complete(&mut tx, &cycle, &CarryTo::Backlog, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let new_end = NaiveDate::from_ymd_opt(2026, 1, 21).unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let reopened = reopen(&mut tx, &closed, new_end, now()).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(reopened.state, CycleState::Active);
        assert_eq!(
            reopened.start_date,
            Some(start_date),
            "start date is not replanned"
        );
        assert_eq!(reopened.end_date, Some(new_end));
    }

    #[tokio::test]
    async fn reopening_still_respects_the_single_active_cycle_rule() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let a = insert_cycle(&db, &project, "Sprint A").await;
        let b = insert_cycle(&db, &project, "Sprint B").await;
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let a = start(&mut tx, &a, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let a = complete(&mut tx, &a, &CarryTo::Backlog, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        start(&mut tx, &b, d, d, now()).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let err = reopen(&mut tx, &a, d, now()).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn adding_a_card_again_after_removal_is_a_fresh_membership() {
        let (db, _temp, project, creator, type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;
        let card = make_card(&db, &project, &type_id, &creator).await;

        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &card.id, &cycle.id, now()).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        assert!(
            remove_card(&mut tx, &card.id, &cycle.id, now())
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();
        assert!(
            current_cycle_for_card(&db, &card.id)
                .await
                .unwrap()
                .is_none()
        );

        let mut tx = db.begin_write().await.unwrap();
        add_card(&mut tx, &card.id, &cycle.id, now()).await.unwrap();
        tx.commit().await.unwrap();
        assert!(
            current_cycle_for_card(&db, &card.id)
                .await
                .unwrap()
                .is_some()
        );

        // Removing a non-member reports false rather than silently succeeding.
        let mut tx = db.begin_write().await.unwrap();
        assert!(
            !remove_card(&mut tx, "no-such-card", &cycle.id, now())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_cycle_can_be_renamed_and_its_goal_cleared_in_any_state() {
        let (db, _temp, project, _creator, _type_id) = fixture().await;
        let cycle = insert_cycle(&db, &project, "Sprint 1").await;

        let mut tx = db.begin_write().await.unwrap();
        let patched = apply_patch(
            &mut tx,
            &cycle,
            &CyclePatch {
                name: Some("Sprint One".to_owned()),
                goal: Some(Some("Ship it".to_owned())),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(patched.name, "Sprint One");
        assert_eq!(patched.goal.as_deref(), Some("Ship it"));

        let mut tx = db.begin_write().await.unwrap();
        let cleared = apply_patch(
            &mut tx,
            &patched,
            &CyclePatch {
                name: None,
                goal: Some(None),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(cleared.name, "Sprint One", "absent name left alone");
        assert_eq!(cleared.goal, None);
    }
}

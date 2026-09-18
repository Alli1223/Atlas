//! Daily point-in-time snapshots of every active cycle's in-scope cards.
//!
//! What "committed vs completed" and a burndown chart are computed from later — Phase 16's
//! whole premise is one snapshot table, GROUP BY at read time, never replaying the changelog.
//! None of that is derivable from current state after the fact: a card's estimate or status
//! today says nothing about what it was on day 3 of a cycle that closed weeks ago. See the
//! `cycle_snapshot` table's own comment in `migrations/0012_cycles.sql` for the full shape.
//!
//! # Deduped by calendar day, not by exact instant
//!
//! [`take`] upserts on `(cycle_id, taken_at, card_id)` with `taken_at` truncated to today's
//! UTC midnight, not the real current instant. [`crate::scheduler`]'s jobs fire immediately on
//! every start (including every restart), so without this, two Atlas restarts on the same day
//! would each write a second, near-duplicate row for that day — turning a daily cadence into
//! "however many times the process happened to restart today". Truncating means a job that
//! fires more than once in a day still lands exactly one row per (cycle, card) for that day,
//! always holding the *latest* state observed that day rather than the first.
//!
//! # Start- and complete-triggered rows, in addition to the daily cadence
//!
//! The `cycle_snapshot` migration's own comment calls for a row "at minimum on start
//! (commitment) and on complete (completion)". [`take_for_cycle_tx`] is that: it writes the same
//! shape of row as the daily [`take`], but for one specific cycle, inside the caller's own write
//! transaction — [`crate::domain::cycle::start`] calls it right after marking the commitment
//! baseline, and [`crate::domain::cycle::complete`] calls it before any incomplete card leaves
//! scope, so the completion row is the last true picture of what was and wasn't done, not a
//! scope already emptied by the carry-over that follows in the same call.
//!
//! Both entry points dedupe on the same `(cycle_id, taken_at, card_id)` key as the daily job, so
//! a cycle that starts and gets its first daily tick on the same calendar day ends up with one
//! row per card for that day, not two — the second write just refreshes the first.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, SqliteConnection};
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::StatusCategory;
use crate::error::AppResult;

/// One in-scope card of a cycle, with its estimate/status as of right now — exactly what
/// [`write_rows`] needs to write one `cycle_snapshot` row.
#[derive(Debug, FromRow)]
struct InScopeCard {
    cycle_id: String,
    card_id: String,
    estimate: Option<f64>,
    status_category: StatusCategory,
}

/// Writes (or refreshes) one `cycle_snapshot` row per card, inside the caller's transaction.
///
/// Shared by [`take`] and [`take_for_cycle_tx`] — the only difference between a daily pass and a
/// start/complete pass is which cards the caller already selected.
async fn write_rows(
    tx: &mut SqliteConnection,
    rows: &[InScopeCard],
    now: DateTime<Utc>,
) -> AppResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    let taken_at = to_sql_timestamp(start_of_day(now));
    for row in rows {
        sqlx::query(
            "INSERT INTO cycle_snapshot (id, cycle_id, taken_at, card_id, estimate, status_category) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (cycle_id, taken_at, card_id) DO UPDATE SET \
                estimate        = excluded.estimate, \
                status_category = excluded.status_category",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&row.cycle_id)
        .bind(&taken_at)
        .bind(&row.card_id)
        .bind(row.estimate)
        .bind(row.status_category)
        .execute(&mut *tx)
        .await?;
    }

    Ok(rows.len())
}

/// Takes today's snapshot of every active cycle's in-scope cards.
///
/// Returns how many `cycle_snapshot` rows were written or refreshed. A cycle with no cards in
/// scope, or no active cycles at all, is not an error — most projects most days have neither
/// starting nor being mid-sprint, and an empty pass is exactly the right answer then.
pub async fn take(db: &Db, now: DateTime<Utc>) -> AppResult<usize> {
    let rows: Vec<InScopeCard> = sqlx::query_as(
        "SELECT cc.cycle_id, cc.card_id, c.estimate, s.category AS status_category \
           FROM card_cycle cc \
           JOIN cycles y ON y.id = cc.cycle_id AND y.state = 'active' \
           JOIN cards c ON c.id = cc.card_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE cc.removed_at IS NULL",
    )
    .fetch_all(db.reader())
    .await?;

    let mut tx = db.begin_write().await?;
    let written = write_rows(&mut tx, &rows, now).await?;
    tx.commit().await?;

    Ok(written)
}

/// Takes a snapshot of one cycle's current in-scope cards, inside an already-open write
/// transaction.
///
/// For [`crate::domain::cycle::start`] and [`crate::domain::cycle::complete`] — the caller
/// selects the moment (right after committing scope, or right before releasing it), and this
/// just writes what that moment looked like. Does not filter on the cycle's `state`, unlike
/// [`take`]'s daily pass: a start-triggered call runs in the same transaction as the `UPDATE`
/// that sets `state = 'active'`, and a complete-triggered call runs *before* the one that sets
/// `state = 'closed'` — filtering on state here would either race its own caller or make the
/// completion row impossible to take at all.
pub async fn take_for_cycle_tx(
    tx: &mut SqliteConnection,
    cycle_id: &str,
    now: DateTime<Utc>,
) -> AppResult<usize> {
    let rows: Vec<InScopeCard> = sqlx::query_as(
        "SELECT cc.cycle_id, cc.card_id, c.estimate, s.category AS status_category \
           FROM card_cycle cc \
           JOIN cards c ON c.id = cc.card_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE cc.cycle_id = ? AND cc.removed_at IS NULL",
    )
    .bind(cycle_id)
    .fetch_all(&mut *tx)
    .await?;

    write_rows(tx, &rows, now).await
}

/// Truncates to the start of `now`'s UTC calendar day — the bucket a snapshot dedupes on.
fn start_of_day(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time")
        .and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::cycle::{self, NewCycle};
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    /// A project with cycles enabled (the `Programming` template's default), a card, and a
    /// *started* cycle the card is in scope of — enough for `take` to have something to
    /// write.
    async fn fixture() -> (Db, TempDb, String) {
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
        let created = card::create(
            &mut tx,
            &project,
            &NewCard {
                type_id,
                parent_id: None,
                summary: "Fix the thing".to_owned(),
                description: None,
                status_id: None,
                priority_id: None,
                assignee_id: None,
                reporter_id: None,
                due_date: None,
                start_date: None,
                estimate: Some(3.0),
                placement: Placement::Bottom,
            },
            &creator.id,
            now(),
        )
        .await
        .unwrap();
        let inserted_cycle = cycle::insert(
            &mut tx,
            &project,
            &NewCycle {
                project_id: &project.id,
                name: "Sprint 1",
                goal: None,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let today = Utc::now().date_naive();
        cycle::start(
            &mut tx,
            &inserted_cycle,
            today,
            today + chrono::Duration::days(14),
            now(),
        )
        .await
        .unwrap();
        cycle::add_card(&mut tx, &created.id, &inserted_cycle.id, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        (db, temp, created.id)
    }

    #[tokio::test]
    async fn a_card_in_an_active_cycle_gets_a_snapshot_row() {
        let (db, _temp, card_id) = fixture().await;

        let written = take(&db, now()).await.unwrap();
        assert_eq!(written, 1);

        let row: (Option<f64>, String) = sqlx::query_as(
            "SELECT estimate, status_category FROM cycle_snapshot WHERE card_id = ?",
        )
        .bind(&card_id)
        .fetch_one(db.reader())
        .await
        .unwrap();
        assert_eq!(row.0, Some(3.0));
        assert_eq!(row.1, "todo");
    }

    #[tokio::test]
    async fn a_second_pass_the_same_day_updates_in_place_rather_than_duplicating() {
        let (db, _temp, card_id) = fixture().await;

        take(&db, now()).await.unwrap();

        // The estimate changes mid-day (a re-estimate); a second pass the same day — the
        // scheduler's own "fires immediately on every restart" behaviour — must reflect it
        // without creating a second row.
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("UPDATE cards SET estimate = 5.0 WHERE id = ?")
            .bind(&card_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let written_again = take(&db, now()).await.unwrap();
        assert_eq!(written_again, 1);

        let rows: Vec<(Option<f64>,)> =
            sqlx::query_as("SELECT estimate FROM cycle_snapshot WHERE card_id = ?")
                .bind(&card_id)
                .fetch_all(db.reader())
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "must still be exactly one row for today");
        assert_eq!(rows[0].0, Some(5.0), "must hold the latest estimate");
    }

    #[tokio::test]
    async fn no_active_cycles_is_an_empty_pass_not_an_error() {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let written = take(&db, now()).await.unwrap();
        assert_eq!(written, 0);
    }
}

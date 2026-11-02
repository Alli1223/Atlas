//! `daily_snapshot`: one row per card per day — the table every flow report is a GROUP BY
//! over (cumulative flow, average age, the control chart).
//!
//! [`take`] writes today's rows in one statement and is idempotent within a day. The first
//! time it runs on an instance with no snapshots it [`backfill`]s the last [`BACKFILL_DAYS`]
//! from the changelog — once, at write time, so reading a report never replays history.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::FromRow;

use crate::db::Db;
use crate::error::AppResult;

/// How far back the first run reconstructs.
pub const BACKFILL_DAYS: i64 = 90;

/// Writes today's snapshot of every live card, backfilling first if this is the first run.
/// Returns how many rows today's snapshot wrote.
pub async fn take(db: &Db, now: DateTime<Utc>) -> AppResult<u64> {
    let any: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT 1 FROM daily_snapshot LIMIT 1)")
            .fetch_one(db.reader())
            .await?;
    if any == 0 {
        backfill(db, now, BACKFILL_DAYS).await?;
    }
    let day = now.date_naive().to_string();
    let mut tx = db.begin_write().await?;
    let written = sqlx::query(
        "INSERT INTO daily_snapshot (day, card_id, project_id, status_id, status_category, \
         estimate, cycle_id, created_day, resolved_day) \
         SELECT ?, c.id, c.project_id, c.status_id, s.category, c.estimate, \
                (SELECT cc.cycle_id FROM card_cycle cc JOIN cycles y ON y.id = cc.cycle_id \
                  WHERE cc.card_id = c.id AND cc.removed_at IS NULL AND y.state = 'active' \
                  LIMIT 1), \
                substr(c.created_at, 1, 10), substr(c.resolved_at, 1, 10) \
           FROM cards c JOIN statuses s ON s.id = c.status_id \
          WHERE c.deleted_at IS NULL \
         ON CONFLICT (day, card_id) DO UPDATE SET \
            status_id = excluded.status_id, status_category = excluded.status_category, \
            estimate = excluded.estimate, cycle_id = excluded.cycle_id, \
            resolved_day = excluded.resolved_day",
    )
    .bind(&day)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(written)
}

#[derive(Debug, FromRow)]
struct CardRow {
    id: String,
    project_id: String,
    status_id: String,
    estimate: Option<f64>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromRow)]
struct ChangeRow {
    card_id: String,
    field: String,
    created_at: DateTime<Utc>,
    from_value: Option<String>,
    to_value: Option<String>,
}

/// The value of a field at the end of `day`, given its changes in order and its value now.
fn value_at<'a>(
    changes: &'a [&'a ChangeRow],
    current: Option<&'a str>,
    day: NaiveDate,
) -> Option<&'a str> {
    let end = day.and_hms_opt(23, 59, 59).map(|t| t.and_utc());
    // The last change on or before the end of the day says what it was; with none yet, the
    // first change's `from` says what it started as; with no changes at all, it never moved.
    let mut answer = changes
        .first()
        .map_or(current, |first| first.from_value.as_deref());
    for change in changes {
        if end.is_some_and(|end| change.created_at <= end) {
            answer = change.to_value.as_deref();
        } else {
            break;
        }
    }
    answer
}

/// Reconstructs `days` days of snapshots from the changelog, for every live card. Days that
/// already have rows are left alone.
pub async fn backfill(db: &Db, now: DateTime<Utc>, days: i64) -> AppResult<u64> {
    let today = now.date_naive();
    let first = today - Duration::days(days);
    let cards = sqlx::query_as::<_, CardRow>(
        "SELECT id, project_id, status_id, estimate, created_at, resolved_at FROM cards \
          WHERE deleted_at IS NULL",
    )
    .fetch_all(db.reader())
    .await?;
    if cards.is_empty() {
        return Ok(0);
    }
    let changes = sqlx::query_as::<_, ChangeRow>(
        "SELECT card_id, field, created_at, from_value, to_value FROM card_history \
          WHERE field IN ('status', 'estimate') ORDER BY card_id, rowid",
    )
    .fetch_all(db.reader())
    .await?;
    let categories: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT id, category FROM statuses")
            .fetch_all(db.reader())
            .await?
            .into_iter()
            .collect();
    let mut by_card: HashMap<&str, (Vec<&ChangeRow>, Vec<&ChangeRow>)> = HashMap::new();
    for change in &changes {
        let entry = by_card.entry(change.card_id.as_str()).or_default();
        if change.field == "status" {
            entry.0.push(change);
        } else {
            entry.1.push(change);
        }
    }

    let mut written = 0;
    let mut tx = db.begin_write().await?;
    for card in &cards {
        let created = card.created_at.date_naive();
        let (status_changes, estimate_changes) = by_card
            .get(card.id.as_str())
            .map_or((Vec::new(), Vec::new()), |(s, e)| (s.clone(), e.clone()));
        let current_estimate = card.estimate.map(|e| e.to_string());
        let mut day = created.max(first);
        while day < today {
            let Some(status) = value_at(&status_changes, Some(&card.status_id), day) else {
                day += Duration::days(1);
                continue;
            };
            let Some(category) = categories.get(status) else {
                day += Duration::days(1);
                continue;
            };
            let estimate = value_at(&estimate_changes, current_estimate.as_deref(), day)
                .and_then(|e| e.parse::<f64>().ok());
            let resolved = card
                .resolved_at
                .filter(|r| r.date_naive() <= day)
                .map(|r| r.date_naive().to_string());
            written += sqlx::query(
                "INSERT INTO daily_snapshot (day, card_id, project_id, status_id, \
                 status_category, estimate, cycle_id, created_day, resolved_day) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?) ON CONFLICT (day, card_id) DO NOTHING",
            )
            .bind(day.to_string())
            .bind(&card.id)
            .bind(&card.project_id)
            .bind(status)
            .bind(category)
            .bind(estimate)
            .bind(created.to_string())
            .bind(resolved)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            day += Duration::days(1);
        }
    }
    tx.commit().await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(at: &str, from: &str, to: &str) -> ChangeRow {
        ChangeRow {
            card_id: "c".into(),
            field: "status".into(),
            created_at: DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&Utc),
            from_value: Some(from.into()),
            to_value: Some(to.into()),
        }
    }

    #[test]
    fn a_field_reads_as_it_stood_at_the_end_of_each_day() {
        let a = change("2026-01-02T10:00:00Z", "todo", "doing");
        let b = change("2026-01-04T23:00:00Z", "doing", "done");
        let changes = vec![&a, &b];
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 1, d).unwrap();
        assert_eq!(value_at(&changes, Some("done"), day(1)), Some("todo"));
        assert_eq!(value_at(&changes, Some("done"), day(2)), Some("doing"));
        assert_eq!(value_at(&changes, Some("done"), day(3)), Some("doing"));
        assert_eq!(value_at(&changes, Some("done"), day(4)), Some("done"));
        // With no changes, it was always what it is now.
        assert_eq!(value_at(&[], Some("todo"), day(1)), Some("todo"));
    }
}

//! Worklogs: time logged against a card, in whole minutes.
//!
//! `card_worklogs` is append-only — no update, no delete — the same ledger shape
//! [`crate::domain::history`] uses for the same reason: a record of what
//! happened is not a place edits belong.
//!
//! Two sources today, named in the `source` column: a smart commit's `#time 2h`
//! directive ([`crate::integrations::github::smart_commit`], `source =
//! "smart-commit"`), and a direct entry through `POST /cards/{key}/worklogs`
//! (`source = "manual"`) — the path `TODO.md` Phase 10 flagged as missing.
//! Logging worklogs is itself a domain concern with nothing GitHub-specific
//! about it, so it lives here rather than in `integrations::github`, which
//! calls into this module rather than the other way round — the same direction
//! every integration keeps with the domain (see [`crate::domain`]'s docs).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// A row of `card_worklogs`.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Worklog {
    /// UUID v7, as text.
    pub id: String,
    /// The card this time was logged against.
    pub card_id: String,
    /// Who logged it. `None` if that account was later deactivated and removed.
    pub author_id: Option<String>,
    /// Minutes worked. Always positive — see [`insert`].
    pub minutes: i64,
    /// An optional note: a smart commit's trailing comment, or free text on a
    /// manual entry.
    pub note: Option<String>,
    /// Where it came from: `"smart-commit"` or `"manual"`.
    pub source: String,
    pub created_at: DateTime<Utc>,
}

/// The fields needed to record a worklog against a card.
#[derive(Debug)]
pub struct NewWorklog<'a> {
    /// The card the time is logged against.
    pub card_id: &'a str,
    /// Who did the work, if known. `None` survives that account's later deletion.
    pub author_id: Option<&'a str>,
    /// Minutes worked — must be positive (the column is `CHECK (minutes > 0)`).
    pub minutes: i64,
    /// An optional note.
    pub note: Option<&'a str>,
    /// Where the log came from, e.g. `"smart-commit"` or `"manual"`.
    pub source: &'a str,
}

/// Appends a worklog to a card, returning the stored row.
///
/// A non-positive duration is rejected here rather than left to the database's
/// `CHECK (minutes > 0)`, so the caller gets a clear error instead of an opaque
/// 500.
pub async fn insert(
    tx: &mut SqliteConnection,
    new: &NewWorklog<'_>,
    now: DateTime<Utc>,
) -> AppResult<Worklog> {
    if new.minutes <= 0 {
        return Err(AppError::Validation(
            "a worklog must be a positive number of minutes".to_owned(),
        ));
    }

    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO card_worklogs (id, card_id, author_id, minutes, note, source, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(new.card_id)
    .bind(new.author_id)
    .bind(new.minutes)
    .bind(new.note)
    .bind(new.source)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the worklog just inserted is missing")))
}

/// Finds a worklog by id inside an open transaction.
async fn find_by_id_tx(tx: &mut SqliteConnection, id: &str) -> AppResult<Option<Worklog>> {
    Ok(sqlx::query_as::<_, Worklog>(
        "SELECT id, card_id, author_id, minutes, note, source, created_at \
         FROM card_worklogs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Every worklog on a card, newest first — a log reads most-recent-on-top,
/// unlike [`crate::domain::comment::list`]'s conversational oldest-first.
pub async fn list_for_card(db: &Db, card_id: &str) -> AppResult<Vec<Worklog>> {
    Ok(sqlx::query_as::<_, Worklog>(
        "SELECT id, card_id, author_id, minutes, note, source, created_at \
         FROM card_worklogs WHERE card_id = ? ORDER BY created_at DESC, id DESC",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// The total minutes logged against a card, across every source.
pub async fn total_minutes_for_card(db: &Db, card_id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(SUM(minutes), 0) FROM card_worklogs WHERE card_id = ?")
            .bind(card_id)
            .fetch_one(db.reader())
            .await?,
    )
}

// ---------------------------------------------------------------------------
// Duration parsing: "2h 30m" -> minutes
// ---------------------------------------------------------------------------

/// A single duration token (`2w` / `3d` / `4h` / `30m`) → minutes, on a 5-day / 8-hour
/// working calendar. `None` if `token` is not a duration.
///
/// Shared by [`parse_duration`] (a manual entry's duration field, which must be *entirely*
/// duration tokens) and [`crate::integrations::github::smart_commit`]'s `#time` directive
/// (which stops at the first non-duration token and treats the rest as a note) — the two
/// callers differ only in what they do with a token that isn't one of these.
pub fn duration_token_minutes(token: &str) -> Option<i64> {
    let unit = token.chars().next_back()?;
    let value: i64 = token[..token.len() - unit.len_utf8()].parse().ok()?;
    if value < 0 {
        return None;
    }
    let per_unit = match unit.to_ascii_lowercase() {
        'w' => 5 * 8 * 60,
        'd' => 8 * 60,
        'h' => 60,
        'm' => 1,
        _ => return None,
    };
    value.checked_mul(per_unit)
}

/// Parses a duration string (e.g. `"2h 30m"`) to total minutes.
///
/// Every whitespace-separated token must be a duration token — unlike the smart-commit
/// parser, there is no trailing note to stop early for, so an unrecognised token is a
/// validation error, not the start of one. `None` for an empty string or a non-positive total.
pub fn parse_duration(text: &str) -> Option<i64> {
    let mut minutes: i64 = 0;
    let mut saw_a_token = false;
    for token in text.split_whitespace() {
        minutes = minutes.checked_add(duration_token_minutes(token)?)?;
        saw_a_token = true;
    }
    (saw_a_token && minutes > 0).then_some(minutes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    async fn fixture() -> (Db, TempDb, String, String) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let author = user::insert(
            &mut tx,
            &user::NewUser {
                username: "worker".to_owned(),
                email: None,
                display_name: "Worker".to_owned(),
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
                estimate: None,
                placement: Placement::Bottom,
            },
            &author.id,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, created.id, author.id)
    }

    #[tokio::test]
    async fn a_positive_worklog_round_trips() {
        let (db, _temp, card_id, author_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let logged = insert(
            &mut tx,
            &NewWorklog {
                card_id: &card_id,
                author_id: Some(&author_id),
                minutes: 90,
                note: Some("wrote the fix"),
                source: "manual",
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(logged.minutes, 90);
        assert_eq!(logged.note, Some("wrote the fix".to_owned()));
        assert_eq!(logged.source, "manual");

        let all = list_for_card(&db, &card_id).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, logged.id);
    }

    #[tokio::test]
    async fn zero_or_negative_minutes_are_rejected() {
        let (db, _temp, card_id, author_id) = fixture().await;
        let mut tx = db.begin_write().await.unwrap();

        for minutes in [0, -5] {
            let err = insert(
                &mut tx,
                &NewWorklog {
                    card_id: &card_id,
                    author_id: Some(&author_id),
                    minutes,
                    note: None,
                    source: "manual",
                },
                now(),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, AppError::Validation(_)));
        }
    }

    #[tokio::test]
    async fn listing_is_newest_first_and_totalling_sums_every_source() {
        let (db, _temp, card_id, author_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let first = insert(
            &mut tx,
            &NewWorklog {
                card_id: &card_id,
                author_id: Some(&author_id),
                minutes: 30,
                note: None,
                source: "manual",
            },
            now(),
        )
        .await
        .unwrap();
        let second = insert(
            &mut tx,
            &NewWorklog {
                card_id: &card_id,
                author_id: None,
                minutes: 120,
                note: Some("fixed it"),
                source: "smart-commit",
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let all = list_for_card(&db, &card_id).await.unwrap();
        assert_eq!(
            all.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
            vec![second.id, first.id]
        );

        let total = total_minutes_for_card(&db, &card_id).await.unwrap();
        assert_eq!(total, 150);
    }

    #[tokio::test]
    async fn a_card_with_no_worklogs_totals_zero_not_null() {
        let (db, _temp, card_id, _author_id) = fixture().await;
        assert_eq!(total_minutes_for_card(&db, &card_id).await.unwrap(), 0);
        assert!(list_for_card(&db, &card_id).await.unwrap().is_empty());
    }

    #[test]
    fn parse_duration_sums_every_token_on_the_working_calendar() {
        assert_eq!(parse_duration("2h"), Some(120));
        assert_eq!(parse_duration("2h 30m"), Some(150));
        assert_eq!(
            parse_duration("1w 2d 3h 4m"),
            Some(5 * 8 * 60 + 2 * 8 * 60 + 3 * 60 + 4)
        );
        assert_eq!(
            parse_duration("1H 30M"),
            Some(90),
            "units are case-insensitive"
        );
    }

    #[test]
    fn parse_duration_rejects_anything_that_is_not_entirely_duration_tokens() {
        // Unlike the smart-commit parser, there is no note to stop early for — a manual
        // entry's duration field is duration, full stop.
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("   "), None);
        assert_eq!(parse_duration("2h fixed the thing"), None);
        assert_eq!(parse_duration("0h"), None);
        assert_eq!(parse_duration("-2h"), None);
        assert_eq!(parse_duration("2x"), None);
    }
}

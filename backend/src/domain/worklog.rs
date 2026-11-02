//! `card_worklogs`: time logged against a card, and the `2w 3d 4h 30m` duration grammar.
//!
//! # Minutes, not seconds
//!
//! `TODO.md` Phase 10 says "store seconds". The column landed in 0010 as whole **minutes**
//! (the smart-commit `#time` directive was its first writer) and every row since is minutes,
//! so this module keeps minutes rather than migrating a unit underneath existing data. Nobody
//! logs work to the second; the grammar's smallest unit is `m`.
//!
//! # The working calendar
//!
//! `1w = 5d`, `1d = 8h` — Jira's defaults, and the ones a timesheet means. A "day" of work is
//! not 24 hours of it. [`parse_duration`] and [`format_minutes`] are inverses over that
//! calendar.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// Minutes in a working day.
pub const MINUTES_PER_DAY: i64 = 8 * 60;
/// Minutes in a working week.
pub const MINUTES_PER_WEEK: i64 = 5 * MINUTES_PER_DAY;

/// The longest single worklog accepted: four working weeks. Anything longer is a typo
/// (`400h` for `4h`), not a timesheet entry.
pub const MAX_MINUTES: i64 = 4 * MINUTES_PER_WEEK;

/// Longest accepted note.
pub const MAX_NOTE: usize = 4 * 1024;

/// One logged stretch of work.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Worklog {
    /// UUID v7.
    pub id: String,
    /// The card the time is logged against.
    pub card_id: String,
    /// Who logged it. `None` once the account is gone.
    pub author_id: Option<String>,
    /// Time spent, in whole minutes.
    pub minutes: i64,
    /// An optional note.
    pub note: Option<String>,
    /// Where it came from: `manual`, `smart-commit`, `automation`.
    pub source: String,
    /// When the work happened. Falls back to `created_at` for rows written before 0016.
    pub started_at: DateTime<Utc>,
    /// When the row was written.
    pub created_at: DateTime<Utc>,
    /// When the row was last edited. `None` = never.
    pub updated_at: Option<DateTime<Utc>>,
}

/// A card's logged time, totalled.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorklogSummary {
    /// Every entry, oldest first.
    pub entries: Vec<Worklog>,
    /// Total minutes across `entries`.
    pub total_minutes: i64,
    /// `total_minutes` in the `1w 2d 3h 4m` grammar.
    pub total_display: String,
}

/// Parses a `2w 3d 4h 30m` duration into whole minutes.
///
/// Tokens are whitespace-separated, each a non-negative integer followed by one of
/// `w d h m` (case-insensitive). Order does not matter and units may repeat (`1h 1h` is
/// `2h`). A bare number is read as minutes, so a form that sends `90` does what it means.
/// The total must be positive and at most [`MAX_MINUTES`].
pub fn parse_duration(input: &str) -> AppResult<i64> {
    let invalid = || {
        AppError::Validation(format!(
            "{input:?} is not a duration. Use units like 2w 3d 4h 30m."
        ))
    };

    let mut total: i64 = 0;
    let mut tokens = 0;
    for token in input.split_whitespace() {
        tokens += 1;
        let minutes = if token.bytes().all(|b| b.is_ascii_digit()) {
            token.parse::<i64>().map_err(|_| invalid())?
        } else {
            token_minutes(token).ok_or_else(invalid)?
        };
        total = total.checked_add(minutes).ok_or_else(invalid)?;
    }

    if tokens == 0 || total <= 0 {
        return Err(invalid());
    }
    if total > MAX_MINUTES {
        return Err(AppError::Validation(format!(
            "A single worklog may be at most {} — {input:?} is longer.",
            format_minutes(MAX_MINUTES)
        )));
    }
    Ok(total)
}

/// A single `2w` / `3d` / `4h` / `30m` token, in minutes. `None` if it is not one.
///
/// Shared with the smart-commit `#time` directive so a commit message and the worklog form
/// cannot disagree about what `1d` means.
pub fn token_minutes(token: &str) -> Option<i64> {
    let unit = token.chars().next_back()?;
    let digits = &token[..token.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i64 = digits.parse().ok()?;
    let per_unit = match unit.to_ascii_lowercase() {
        'w' => MINUTES_PER_WEEK,
        'd' => MINUTES_PER_DAY,
        'h' => 60,
        'm' => 1,
        _ => return None,
    };
    value.checked_mul(per_unit)
}

/// Renders minutes in the `1w 2d 3h 4m` grammar — the inverse of [`parse_duration`].
pub fn format_minutes(minutes: i64) -> String {
    if minutes <= 0 {
        return "0m".to_owned();
    }
    let mut rest = minutes;
    let mut parts = Vec::new();
    for (unit, size) in [
        ('w', MINUTES_PER_WEEK),
        ('d', MINUTES_PER_DAY),
        ('h', 60),
        ('m', 1),
    ] {
        let count = rest / size;
        if count > 0 {
            parts.push(format!("{count}{unit}"));
            rest -= count * size;
        }
    }
    parts.join(" ")
}

/// Checks a worklog note.
pub fn validate_note(note: Option<&str>) -> AppResult<Option<String>> {
    let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > MAX_NOTE {
        return Err(AppError::Validation(format!(
            "A worklog note must be at most {MAX_NOTE} characters long."
        )));
    }
    Ok(Some(note.to_owned()))
}

/// A card's worklogs, oldest work first, with the total.
pub async fn summary(db: &Db, card_id: &str) -> AppResult<WorklogSummary> {
    let entries = sqlx::query_as::<_, Worklog>(
        "SELECT id, card_id, author_id, minutes, note, source, \
         COALESCE(started_at, created_at) AS started_at, created_at, updated_at \
         FROM card_worklogs WHERE card_id = ? ORDER BY started_at, id",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?;
    let total_minutes = entries.iter().map(|w| w.minutes).sum();
    Ok(WorklogSummary {
        entries,
        total_minutes,
        total_display: format_minutes(total_minutes),
    })
}

/// Finds a worklog inside a transaction.
pub async fn find_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Worklog>> {
    Ok(sqlx::query_as::<_, Worklog>(
        "SELECT id, card_id, author_id, minutes, note, source, \
         COALESCE(started_at, created_at) AS started_at, created_at, updated_at \
         FROM card_worklogs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Total minutes logged per card, for a set of cards — the reports' and roll-ups' view.
pub async fn total_minutes(db: &Db, card_id: &str) -> AppResult<i64> {
    let total: Option<i64> =
        sqlx::query_scalar("SELECT SUM(minutes) FROM card_worklogs WHERE card_id = ?")
            .bind(card_id)
            .fetch_one(db.reader())
            .await?;
    Ok(total.unwrap_or(0))
}

/// What a new worklog carries.
#[derive(Debug)]
pub struct NewWorklog<'a> {
    /// The card.
    pub card_id: &'a str,
    /// Who logged it.
    pub author_id: Option<&'a str>,
    /// Whole minutes, already validated.
    pub minutes: i64,
    /// Already validated.
    pub note: Option<String>,
    /// `manual`, `smart-commit`, `automation`.
    pub source: &'a str,
    /// When the work happened. `None` = now.
    pub started_at: Option<DateTime<Utc>>,
}

/// Logs time against a card.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    new: &NewWorklog<'_>,
    now: DateTime<Utc>,
) -> AppResult<Worklog> {
    if new.minutes <= 0 || new.minutes > MAX_MINUTES {
        return Err(AppError::Validation(
            "A worklog must be positive and at most four working weeks.".to_owned(),
        ));
    }
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO card_worklogs (id, card_id, author_id, minutes, note, source, created_at, \
         started_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&id)
    .bind(new.card_id)
    .bind(new.author_id)
    .bind(new.minutes)
    .bind(&new.note)
    .bind(new.source)
    .bind(to_sql_timestamp(now))
    .bind(to_sql_timestamp(new.started_at.unwrap_or(now)))
    .execute(&mut *tx)
    .await?;

    find_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the worklog just inserted is missing")))
}

/// Edits a worklog's duration, note or start.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    current: &Worklog,
    minutes: Option<i64>,
    note: Option<Option<String>>,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> AppResult<Worklog> {
    let minutes = minutes.unwrap_or(current.minutes);
    let note = note.unwrap_or_else(|| current.note.clone());
    let started_at = started_at.unwrap_or(current.started_at);
    sqlx::query(
        "UPDATE card_worklogs SET minutes = ?, note = ?, started_at = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(minutes)
    .bind(&note)
    .bind(to_sql_timestamp(started_at))
    .bind(to_sql_timestamp(now))
    .bind(&current.id)
    .execute(&mut *tx)
    .await?;
    find_tx(&mut *tx, &current.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the worklog just edited is missing")))
}

/// Deletes a worklog.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM card_worklogs WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar_sums_units_on_a_working_calendar() {
        assert_eq!(parse_duration("30m").unwrap(), 30);
        assert_eq!(parse_duration("4h").unwrap(), 240);
        assert_eq!(parse_duration("1d").unwrap(), 480);
        assert_eq!(parse_duration("1w").unwrap(), 2400);
        assert_eq!(
            parse_duration("2w 3d 4h 30m").unwrap(),
            2 * 2400 + 3 * 480 + 4 * 60 + 30
        );
        assert_eq!(parse_duration("1H 30M").unwrap(), 90);
        assert_eq!(parse_duration("  90  ").unwrap(), 90);
        assert_eq!(parse_duration("1h 1h").unwrap(), 120);
    }

    #[test]
    fn junk_zero_negative_and_absurd_durations_are_refused() {
        for bad in ["", "   ", "0m", "soon", "1x", "-1h", "h", "1.5h", "5w"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn formatting_is_the_inverse_of_parsing() {
        for minutes in [
            1,
            59,
            60,
            61,
            479,
            480,
            2399,
            2400,
            2 * 2400 + 3 * 480 + 270,
        ] {
            let shown = format_minutes(minutes);
            assert_eq!(parse_duration(&shown).unwrap(), minutes, "{shown}");
        }
        assert_eq!(format_minutes(0), "0m");
        assert_eq!(
            format_minutes(2 * 2400 + 3 * 480 + 4 * 60 + 30),
            "2w 3d 4h 30m"
        );
    }

    #[test]
    fn notes_are_trimmed_optional_and_bounded() {
        assert_eq!(validate_note(None).unwrap(), None);
        assert_eq!(validate_note(Some("  ")).unwrap(), None);
        assert_eq!(
            validate_note(Some(" pairing ")).unwrap().as_deref(),
            Some("pairing")
        );
        assert!(validate_note(Some(&"a".repeat(MAX_NOTE + 1))).is_err());
    }
}

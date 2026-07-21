//! AQL functions.
//!
//! Two kinds, split by what they compile to:
//!
//! - **Scalar** functions ([`resolve_scalar`]) — `currentUser()`, `now()`,
//!   `startOfWeek(-1w)`, `endOfMonth()` — resolve to a single value that becomes
//!   one bind parameter. All of the MUST set is here and fully implemented.
//! - **Set** functions ([`is_set_function`]) — `membersOf()`, `watchedCards()`,
//!   `linkedCards()`, `cardHistory()` — expand to a subquery. Those are built in
//!   [`crate::aql::compile`], where the SQL lives; this module only recognises
//!   their names and arities so the parser's output is validated in one place.
//!
//! Every path returns a `Result`; nothing here panics on a hostile argument,
//! because the fuzzer calls straight through it.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};

use super::ast::{AqlError, FuncCall};
use crate::auth::to_sql_timestamp;

/// What a scalar function needs to resolve: who is asking, and when.
#[derive(Debug, Clone)]
pub struct FnCtx {
    /// The id of the caller, for `currentUser()`.
    pub current_user_id: String,
    /// The instant `now()` and the `startOf*`/`endOf*` family are relative to.
    pub now: DateTime<Utc>,
}

/// Whether `name` is one of the set-valued functions (case-insensitive).
///
/// These are compiled to subqueries in [`crate::aql::compile`]; this predicate
/// keeps the list of names in one place.
#[must_use]
pub fn is_set_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "membersof" | "watchedcards" | "linkedcards" | "cardhistory"
    )
}

/// Whether `name` is a scalar (single-value) function.
#[must_use]
pub fn is_scalar_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "currentuser"
            | "now"
            | "startofday"
            | "startofweek"
            | "startofmonth"
            | "startofyear"
            | "endofday"
            | "endofweek"
            | "endofmonth"
            | "endofyear"
    )
}

/// Whether `name` is a function Atlas knows at all.
#[must_use]
pub fn is_known_function(name: &str) -> bool {
    is_scalar_function(name) || is_set_function(name)
}

/// Resolves a scalar function to its value string, ready to bind.
///
/// # Errors
///
/// An unknown function, a wrong argument count, or an unparseable relative
/// offset — each with the call's span, so the frontend can underline it.
pub fn resolve_scalar(call: &FuncCall, ctx: &FnCtx) -> Result<String, AqlError> {
    let name = call.name.to_ascii_lowercase();
    match name.as_str() {
        "currentuser" => {
            expect_no_args(call)?;
            Ok(ctx.current_user_id.clone())
        }
        "now" => {
            expect_no_args(call)?;
            Ok(to_sql_timestamp(ctx.now))
        }
        "startofday" | "startofweek" | "startofmonth" | "startofyear" | "endofday"
        | "endofweek" | "endofmonth" | "endofyear" => resolve_period(&name, call, ctx),
        other => Err(AqlError::at(
            call.name_span,
            format!("unknown function '{other}'"),
        )),
    }
}

fn expect_no_args(call: &FuncCall) -> Result<(), AqlError> {
    if call.args.is_empty() {
        Ok(())
    } else {
        Err(AqlError::at(
            call.span,
            format!("{}() takes no arguments", call.name),
        ))
    }
}

/// Resolves a `startOf*`/`endOf*` call, applying an optional relative offset.
fn resolve_period(name: &str, call: &FuncCall, ctx: &FnCtx) -> Result<String, AqlError> {
    if call.args.len() > 1 {
        return Err(AqlError::at(
            call.span,
            format!(
                "{}() takes at most one relative offset, like -1w",
                call.name
            ),
        ));
    }

    let offset = match call.args.first() {
        Some(value) => parse_offset(value)?,
        None => Duration::zero(),
    };

    let is_end = name.starts_with("end");
    let base = if is_end {
        period_end(name, ctx.now)
    } else {
        period_start(name, ctx.now)
    }
    .ok_or_else(|| {
        AqlError::at(call.span, "the requested date is outside the representable range")
    })?;

    let shifted = base
        .checked_add_signed(offset)
        .ok_or_else(|| AqlError::at(call.span, "the relative offset overflows the date range"))?;

    Ok(to_sql_timestamp(shifted))
}

/// The start of the period `now` falls in.
fn period_start(name: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let date = now.date_naive();
    let start_date = match name {
        "startofday" | "endofday" => date,
        "startofweek" | "endofweek" => {
            let back = i64::from(date.weekday().num_days_from_monday());
            date.checked_sub_signed(Duration::try_days(back)?)?
        }
        "startofmonth" | "endofmonth" => date.with_day(1)?,
        "startofyear" | "endofyear" => date.with_month(1)?.with_day(1)?,
        _ => return None,
    };
    let naive = start_date.and_hms_opt(0, 0, 0)?;
    Some(Utc.from_utc_datetime(&naive))
}

/// The end of the period `now` falls in: the last microsecond before the next
/// period begins.
fn period_end(name: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let next_start = match name {
        "endofday" => period_start("startofday", now)?.checked_add_signed(Duration::try_days(1)?)?,
        "endofweek" => {
            period_start("startofweek", now)?.checked_add_signed(Duration::try_days(7)?)?
        }
        "endofmonth" => {
            let start = period_start("startofmonth", now)?;
            add_one_month(start)?
        }
        "endofyear" => {
            let start = period_start("startofyear", now)?;
            let year = start.year().checked_add(1)?;
            let naive = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?.and_hms_opt(0, 0, 0)?;
            Utc.from_utc_datetime(&naive)
        }
        _ => return None,
    };
    next_start.checked_sub_signed(Duration::microseconds(1))
}

/// The first instant of the month after `start` (which must be a month start).
fn add_one_month(start: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (year, month) = if start.month() == 12 {
        (start.year().checked_add(1)?, 1)
    } else {
        (start.year(), start.month() + 1)
    };
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;
    Some(Utc.from_utc_datetime(&naive))
}

/// Parses a relative offset like `-1w`, `+3d`, `2h`, `-30m` into a duration.
///
/// Units: `w` weeks, `d` days, `h` hours, `m` minutes. A bare number is days.
/// Overflow returns an error rather than panicking — the duration constructors
/// are the checked `try_*` ones for exactly that reason.
fn parse_offset(value: &super::ast::Value) -> Result<Duration, AqlError> {
    let (raw, span) = match value {
        super::ast::Value::Str { text, span } => (text.clone(), *span),
        super::ast::Value::Num { raw, span, .. } => (raw.clone(), *span),
        super::ast::Value::Func(call) => {
            return Err(AqlError::at(
                call.span,
                "a relative offset must be a literal like -1w, not a function",
            ));
        }
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AqlError::at(span, "empty relative offset"));
    }

    // Split the trailing unit letter off the signed number.
    let (number_part, unit) = match trimmed.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&trimmed[..trimmed.len() - c.len_utf8()], Some(c)),
        _ => (trimmed, None),
    };

    let amount: i64 = number_part.parse().map_err(|_| {
        AqlError::at(span, format!("'{raw}' is not a relative offset like -1w"))
    })?;

    let duration = match unit {
        Some('w' | 'W') => Duration::try_weeks(amount),
        Some('d' | 'D') | None => Duration::try_days(amount),
        Some('h' | 'H') => Duration::try_hours(amount),
        Some('m') => Duration::try_minutes(amount),
        Some(other) => {
            return Err(AqlError::at(
                span,
                format!("unknown offset unit '{other}'; use w, d, h or m"),
            ));
        }
    };

    duration.ok_or_else(|| AqlError::at(span, "the relative offset is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aql::ast::Span;
    use crate::aql::lexer::lex;
    use crate::aql::parser::parse;

    fn ctx() -> FnCtx {
        // A fixed Thursday so week/month arithmetic is checkable by hand.
        FnCtx {
            current_user_id: "u-42".to_owned(),
            now: Utc.with_ymd_and_hms(2026, 7, 16, 13, 30, 0).unwrap(),
        }
    }

    fn call(source: &str) -> FuncCall {
        let query = parse(lex(source).unwrap()).unwrap();
        // Pull the function out of `x = <func>`.
        match query.predicate.unwrap() {
            crate::aql::ast::Node::Cond(c) => match c.rhs {
                crate::aql::ast::Rhs::Single(crate::aql::ast::Value::Func(call)) => call,
                other => panic!("not a function: {other:?}"),
            },
            other => panic!("not a condition: {other:?}"),
        }
    }

    #[test]
    fn current_user_resolves_to_the_caller_id() {
        assert_eq!(
            resolve_scalar(&call("assignee = currentUser()"), &ctx()).unwrap(),
            "u-42"
        );
    }

    #[test]
    fn now_resolves_to_the_context_instant() {
        assert_eq!(
            resolve_scalar(&call("updated < now()"), &ctx()).unwrap(),
            to_sql_timestamp(ctx().now)
        );
    }

    #[test]
    fn start_of_week_lands_on_monday_midnight() {
        // 2026-07-16 is a Thursday; the Monday of that week is the 13th.
        let resolved = resolve_scalar(&call("due >= startOfWeek()"), &ctx()).unwrap();
        assert!(resolved.starts_with("2026-07-13T00:00:00"), "{resolved}");
    }

    #[test]
    fn start_of_week_minus_one_week_is_the_previous_monday() {
        // The named example from the phase brief: startOfWeek(-1w) -> 2026-07-06.
        let resolved = resolve_scalar(&call("due >= startOfWeek(-1w)"), &ctx()).unwrap();
        assert!(resolved.starts_with("2026-07-06T00:00:00"), "{resolved}");
    }

    #[test]
    fn start_of_month_and_year_land_on_the_first() {
        assert!(
            resolve_scalar(&call("created >= startOfMonth()"), &ctx())
                .unwrap()
                .starts_with("2026-07-01T00:00:00")
        );
        assert!(
            resolve_scalar(&call("created >= startOfYear()"), &ctx())
                .unwrap()
                .starts_with("2026-01-01T00:00:00")
        );
    }

    #[test]
    fn end_of_day_is_the_last_microsecond() {
        let resolved = resolve_scalar(&call("due <= endOfDay()"), &ctx()).unwrap();
        assert!(resolved.starts_with("2026-07-16T23:59:59.999999"), "{resolved}");
    }

    #[test]
    fn end_of_month_crosses_into_the_next_month_correctly() {
        // July has 31 days, so endOfMonth is the 31st, not the 30th.
        let resolved = resolve_scalar(&call("due <= endOfMonth()"), &ctx()).unwrap();
        assert!(resolved.starts_with("2026-07-31T23:59:59.999999"), "{resolved}");
    }

    #[test]
    fn a_bad_offset_unit_is_an_error_not_a_panic() {
        assert!(resolve_scalar(&call("due >= startOfWeek(3q)"), &ctx()).is_err());
    }

    #[test]
    fn an_enormous_offset_errors_rather_than_overflowing() {
        assert!(resolve_scalar(&call("due >= startOfWeek(9999999999999w)"), &ctx()).is_err());
    }

    #[test]
    fn currentuser_rejects_arguments() {
        assert!(resolve_scalar(&call("assignee = currentUser()"), &ctx()).is_ok());
        // Build a call with an arg by hand.
        let bogus = FuncCall {
            name: "currentUser".to_owned(),
            name_span: Span::new(0, 11),
            args: vec![crate::aql::ast::Value::Str {
                text: "x".to_owned(),
                span: Span::new(12, 13),
            }],
            span: Span::new(0, 14),
        };
        assert!(resolve_scalar(&bogus, &ctx()).is_err());
    }

    #[test]
    fn the_function_name_sets_are_disjoint_and_complete() {
        assert!(is_scalar_function("currentUser"));
        assert!(is_set_function("membersOf"));
        assert!(!is_set_function("now"));
        assert!(!is_scalar_function("membersOf"));
        assert!(is_known_function("linkedCards"));
        assert!(!is_known_function("bogus"));
    }
}

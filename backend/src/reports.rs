//! Reports (`TODO.md` Phase 16): every one a query over [`crate::domain::snapshot`]'s daily
//! table, the cycle snapshots, or the cards themselves — never a replay of the changelog.
//!
//! | report | source | domain |
//! |---|---|---|
//! | cumulative flow | `daily_snapshot` by status | general |
//! | created vs resolved | `cards` by day | general |
//! | average age | `daily_snapshot`, unresolved | general |
//! | control chart | `daily_snapshot` (first in-progress day → resolved) | general |
//! | burndown / burnup | `cycle_snapshot` | cycles on |
//! | cycle report | `card_cycle` + commitment/completion snapshots | cycles on |
//! | velocity | commitment/completion snapshots of closed cycles | `[SW]`-ish, still general |
//! | aggregate / two-dimensional | an AQL query, grouped | general |
//! | agent costs | `agent_sessions` (+ Gemini generations) | general |
//!
//! Everything degrades to counts when the project's estimation unit is `none` — a job search
//! has no story points, and its burndown is "applications still open".

// Counts and day offsets in a report never approach 2^52, so an `as f64` here loses nothing;
// reports are floating-point by nature (estimates, averages), and a `try_from` dance on every
// count would obscure the arithmetic the module exists to show.
#![allow(clippy::cast_precision_loss)]

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::User;
use crate::db::Db;
use crate::domain::EstimationUnit;
use crate::domain::project::Project;
use crate::error::{AppError, AppResult};

/// The most cards an AQL-driven report will group.
pub const MAX_REPORT_CARDS: i64 = 5000;

fn days_between(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut day = from;
    while day <= to {
        out.push(day);
        day += Duration::days(1);
    }
    out
}

fn window(now: DateTime<Utc>, days: i64) -> (NaiveDate, NaiveDate) {
    let to = now.date_naive();
    (to - Duration::days(days.clamp(1, 730) - 1), to)
}

/// Whether the project measures in counts rather than an estimate.
fn counts(project: &Project) -> bool {
    project.estimation_unit == EstimationUnit::None
}

// ---------------------------------------------------------------------------
// Cumulative flow
// ---------------------------------------------------------------------------

/// A status as a report series.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// The status id (or a group key).
    pub id: String,
    /// What to label it.
    pub name: String,
    /// `todo`, `in_progress`, `done` — drives the colour.
    pub category: Option<String>,
    /// One value per day in `days`.
    pub values: Vec<f64>,
}

/// A day-indexed chart.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DailyChart {
    /// The x axis.
    pub days: Vec<NaiveDate>,
    /// One series per line/band.
    pub series: Vec<Series>,
    /// `count` or the project's estimation unit.
    pub unit: String,
}

/// Cumulative flow: cards per status per day, statuses in board order.
pub async fn cumulative_flow(
    db: &Db,
    project: &Project,
    now: DateTime<Utc>,
    days: i64,
) -> AppResult<DailyChart> {
    let (from, to) = window(now, days);
    let statuses: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, name, category FROM statuses WHERE project_id = ? ORDER BY position",
    )
    .bind(&project.id)
    .fetch_all(db.reader())
    .await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT day, status_id, COUNT(*) FROM daily_snapshot \
          WHERE project_id = ? AND day BETWEEN ? AND ? GROUP BY day, status_id",
    )
    .bind(&project.id)
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(db.reader())
    .await?;
    let axis = days_between(from, to);
    let index: HashMap<String, usize> = axis
        .iter()
        .enumerate()
        .map(|(i, d)| (d.to_string(), i))
        .collect();
    let mut series: Vec<Series> = statuses
        .into_iter()
        .map(|(id, name, category)| Series {
            id,
            name,
            category: Some(category),
            values: vec![0.0; axis.len()],
        })
        .collect();
    for (day, status, n) in rows {
        if let (Some(&i), Some(s)) = (index.get(&day), series.iter_mut().find(|s| s.id == status)) {
            s.values[i] = n as f64;
        }
    }
    Ok(DailyChart {
        days: axis,
        series,
        unit: "count".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Created vs resolved, average age
// ---------------------------------------------------------------------------

/// Cards created and resolved per day, with running totals.
pub async fn created_vs_resolved(
    db: &Db,
    project: &Project,
    now: DateTime<Utc>,
    days: i64,
) -> AppResult<DailyChart> {
    let (from, to) = window(now, days);
    let created: Vec<(String, i64)> = sqlx::query_as(
        "SELECT substr(created_at, 1, 10) AS d, COUNT(*) FROM cards \
          WHERE project_id = ? AND deleted_at IS NULL AND d BETWEEN ? AND ? GROUP BY d",
    )
    .bind(&project.id)
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(db.reader())
    .await?;
    let resolved: Vec<(String, i64)> = sqlx::query_as(
        "SELECT substr(resolved_at, 1, 10) AS d, COUNT(*) FROM cards \
          WHERE project_id = ? AND deleted_at IS NULL AND resolved_at IS NOT NULL \
            AND d BETWEEN ? AND ? GROUP BY d",
    )
    .bind(&project.id)
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(db.reader())
    .await?;
    let axis = days_between(from, to);
    let per_day = |rows: &[(String, i64)]| -> Vec<f64> {
        let map: HashMap<&str, i64> = rows.iter().map(|(d, n)| (d.as_str(), *n)).collect();
        axis.iter()
            .map(|d| map.get(d.to_string().as_str()).copied().unwrap_or(0) as f64)
            .collect()
    };
    let running = |values: &[f64]| -> Vec<f64> {
        values
            .iter()
            .scan(0.0, |sum, v| {
                *sum += v;
                Some(*sum)
            })
            .collect()
    };
    let c = per_day(&created);
    let r = per_day(&resolved);
    Ok(DailyChart {
        series: vec![
            Series {
                id: "created".into(),
                name: "Created".into(),
                category: None,
                values: c.clone(),
            },
            Series {
                id: "resolved".into(),
                name: "Resolved".into(),
                category: Some("done".into()),
                values: r.clone(),
            },
            Series {
                id: "createdTotal".into(),
                name: "Created (total)".into(),
                category: None,
                values: running(&c),
            },
            Series {
                id: "resolvedTotal".into(),
                name: "Resolved (total)".into(),
                category: Some("done".into()),
                values: running(&r),
            },
        ],
        days: axis,
        unit: "count".to_owned(),
    })
}

/// The average age in days of unresolved cards, per day — "how long since I applied".
pub async fn average_age(
    db: &Db,
    project: &Project,
    now: DateTime<Utc>,
    days: i64,
) -> AppResult<DailyChart> {
    let (from, to) = window(now, days);
    let rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT day, AVG(julianday(day) - julianday(created_day)) FROM daily_snapshot \
          WHERE project_id = ? AND status_category != 'done' AND day BETWEEN ? AND ? \
          GROUP BY day",
    )
    .bind(&project.id)
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(db.reader())
    .await?;
    let axis = days_between(from, to);
    let map: HashMap<String, f64> = rows.into_iter().collect();
    let values = axis
        .iter()
        .map(|d| map.get(&d.to_string()).copied().unwrap_or(0.0))
        .collect();
    Ok(DailyChart {
        days: axis,
        series: vec![Series {
            id: "age".into(),
            name: "Average age (days)".into(),
            category: None,
            values,
        }],
        unit: "days".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Control chart
// ---------------------------------------------------------------------------

/// One resolved card on the control chart.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControlPoint {
    /// The card.
    pub key: String,
    /// Its summary.
    pub summary: String,
    /// The day it was resolved.
    pub resolved: NaiveDate,
    /// Days from first in progress (or creation) to resolution.
    pub cycle_days: f64,
    /// The rolling average up to and including this point.
    pub rolling_average: f64,
}

/// The control chart: cycle time per resolved card, with a rolling average.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControlChart {
    /// One point per resolved card, oldest first.
    pub points: Vec<ControlPoint>,
    /// The mean cycle time.
    pub mean: f64,
    /// The median cycle time.
    pub median: f64,
}

/// How many points the rolling average spans.
const ROLLING: usize = 5;

/// Cycle time for every card resolved in the window.
pub async fn control_chart(
    db: &Db,
    project: &Project,
    now: DateTime<Utc>,
    days: i64,
) -> AppResult<ControlChart> {
    let (from, _) = window(now, days);
    let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT c.key, c.summary, substr(c.resolved_at, 1, 10), substr(c.created_at, 1, 10), \
                (SELECT MIN(s.day) FROM daily_snapshot s \
                  WHERE s.card_id = c.id AND s.status_category = 'in_progress') \
           FROM cards c \
          WHERE c.project_id = ? AND c.deleted_at IS NULL AND c.resolved_at IS NOT NULL \
            AND substr(c.resolved_at, 1, 10) >= ? \
          ORDER BY c.resolved_at",
    )
    .bind(&project.id)
    .bind(from.to_string())
    .fetch_all(db.reader())
    .await?;
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let mut points = Vec::new();
    let mut recent: Vec<f64> = Vec::new();
    for (key, summary, resolved, created, started) in rows {
        let (Some(resolved), Some(created)) = (parse(&resolved), parse(&created)) else {
            continue;
        };
        let start = started
            .as_deref()
            .and_then(parse)
            .filter(|s| *s <= resolved)
            .unwrap_or(created);
        let cycle_days = (resolved - start).num_days() as f64;
        recent.push(cycle_days);
        let tail = &recent[recent.len().saturating_sub(ROLLING)..];
        let rolling_average = tail.iter().sum::<f64>() / tail.len() as f64;
        points.push(ControlPoint {
            key,
            summary,
            resolved,
            cycle_days,
            rolling_average,
        });
    }
    let mut sorted: Vec<f64> = points.iter().map(|p| p.cycle_days).collect();
    sorted.sort_by(f64::total_cmp);
    let mean = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    let median = match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2],
        n => f64::midpoint(sorted[n / 2 - 1], sorted[n / 2]),
    };
    Ok(ControlChart {
        points,
        mean,
        median,
    })
}

// ---------------------------------------------------------------------------
// Cycles: burndown/burnup, cycle report, velocity
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct CycleRow {
    name: String,
    state: String,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

async fn cycle(db: &Db, id: &str) -> AppResult<CycleRow> {
    sqlx::query_as::<_, CycleRow>(
        "SELECT name, state, start_date, end_date, started_at, completed_at \
           FROM cycles WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?
    .ok_or(AppError::NotFound)
}

/// Refuses cycle reports on a project with cycles off.
pub fn require_cycles(project: &Project) -> AppResult<()> {
    if project.cycles_enabled {
        Ok(())
    } else {
        Err(AppError::Validation(
            "Cycles are turned off for this project, so it has no cycle reports.".to_owned(),
        ))
    }
}

/// A burndown (remaining) and burnup (scope and completed) for one cycle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Burndown {
    /// The x axis: the cycle's days.
    pub days: Vec<NaiveDate>,
    /// Work left, per day (`null` for days not yet reached).
    pub remaining: Vec<Option<f64>>,
    /// Total scope, per day — burnup's top line; its rises are scope creep.
    pub scope: Vec<Option<f64>>,
    /// Work done, per day.
    pub completed: Vec<Option<f64>>,
    /// The straight line from the committed scope to zero.
    pub ideal: Vec<f64>,
    /// `count` or the estimation unit.
    pub unit: String,
}

/// The cycle's burndown and burnup, from its daily snapshots.
pub async fn burndown(
    db: &Db,
    project: &Project,
    cycle_id: &str,
    now: DateTime<Utc>,
) -> AppResult<Burndown> {
    require_cycles(project)?;
    let found = cycle(db, cycle_id).await?;
    let start = found.start_date.ok_or_else(|| {
        AppError::Validation("A cycle has no burndown until it is started.".to_owned())
    })?;
    let end = found.end_date.unwrap_or(start).max(start);
    let use_counts = counts(project);
    let rows: Vec<(String, String, Option<f64>)> = sqlx::query_as(
        "SELECT taken_at, status_category, estimate FROM cycle_snapshot \
          WHERE cycle_id = ? ORDER BY taken_at",
    )
    .bind(cycle_id)
    .fetch_all(db.reader())
    .await?;
    let weight = |e: Option<f64>| if use_counts { 1.0 } else { e.unwrap_or(0.0) };
    // Snapshots, one per instant: the daily ones (stamped at midnight) and the commitment and
    // completion ones (stamped at the real instant).
    let mut instants: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (at, category, estimate) in rows {
        let entry = instants.entry(at).or_insert((0.0, 0.0));
        entry.0 += weight(estimate);
        if category != "done" {
            entry.1 += weight(estimate);
        }
    }
    // Per day, the latest snapshot of that day wins — so on the start day the commitment
    // (taken at the real start) beats the midnight daily row, and on the last day the
    // completion snapshot does.
    let mut per_day: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (at, totals) in &instants {
        per_day.insert(at.chars().take(10).collect(), *totals);
    }
    let committed = found
        .started_at
        .and_then(|at| instants.get(&crate::auth::to_sql_timestamp(at)))
        .or_else(|| instants.values().next())
        .map_or(0.0, |(scope, _)| *scope);
    let _ = &found.completed_at;
    let axis = days_between(start, end);
    let today = now.date_naive();
    let mut last: Option<(f64, f64)> = None;
    let mut remaining = Vec::new();
    let mut scope = Vec::new();
    let mut completed = Vec::new();
    for day in &axis {
        if let Some(found) = per_day.get(&day.to_string()) {
            last = Some(*found);
        }
        if *day > today && found.state != "closed" {
            remaining.push(None);
            scope.push(None);
            completed.push(None);
        } else {
            remaining.push(last.map(|(_, r)| r));
            scope.push(last.map(|(s, _)| s));
            completed.push(last.map(|(s, r)| s - r));
        }
    }
    let steps = (axis.len().max(2) - 1) as f64;
    let ideal = (0..axis.len())
        .map(|i| committed * (1.0 - i as f64 / steps))
        .collect();
    Ok(Burndown {
        days: axis,
        remaining,
        scope,
        completed,
        ideal,
        unit: if use_counts {
            "count".to_owned()
        } else {
            project.estimation_unit.to_string()
        },
    })
}

/// One card's part in a cycle.
#[allow(clippy::struct_excessive_bools)] // four independent facts: the report's own columns
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CycleCard {
    /// The card.
    pub key: String,
    /// Its summary.
    pub summary: String,
    /// Its estimate.
    pub estimate: Option<f64>,
    /// In scope when the cycle started.
    pub committed: bool,
    /// Added after it started.
    pub added: bool,
    /// Removed before it ended.
    pub removed: bool,
    /// Done at completion (or now, while active).
    pub completed: bool,
}

/// Committed vs completed vs added for one cycle.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CycleReport {
    /// The cycle's name.
    pub name: String,
    /// `future`, `active`, `closed`.
    pub state: String,
    /// `count` or the estimation unit.
    pub unit: String,
    /// Committed at start.
    pub committed: f64,
    /// Added after start.
    pub added: f64,
    /// Removed during.
    pub removed: f64,
    /// Completed.
    pub completed: f64,
    /// Every card that was ever in it.
    pub cards: Vec<CycleCard>,
}

/// One membership row of a cycle, with its card.
#[derive(Debug, sqlx::FromRow)]
struct MemberRow {
    id: String,
    key: String,
    summary: String,
    estimate: Option<f64>,
    in_scope_at_start: bool,
    added_at: String,
    removed_at: Option<String>,
    category: String,
}

/// The cycle report.
pub async fn cycle_report(db: &Db, project: &Project, cycle_id: &str) -> AppResult<CycleReport> {
    require_cycles(project)?;
    let found = cycle(db, cycle_id).await?;
    let use_counts = counts(project);
    let rows = sqlx::query_as::<_, MemberRow>(
        "SELECT c.id, c.key, c.summary, c.estimate, cc.in_scope_at_start, cc.added_at, \
                cc.removed_at, s.category \
           FROM card_cycle cc JOIN cards c ON c.id = cc.card_id \
           JOIN statuses s ON s.id = c.status_id \
          WHERE cc.cycle_id = ? AND c.deleted_at IS NULL ORDER BY c.key",
    )
    .bind(cycle_id)
    .fetch_all(db.reader())
    .await?;
    // At completion, "done" means done *then*, from the completion snapshot — a card reopened
    // since must not rewrite what the cycle finished.
    let done_at_close: HashMap<String, bool> = if found.completed_at.is_some() {
        sqlx::query_as::<_, (String, String)>(
            "SELECT card_id, status_category FROM cycle_snapshot \
              WHERE cycle_id = ? AND kind = 'completion' AND taken_at = \
                    (SELECT MAX(taken_at) FROM cycle_snapshot \
                      WHERE cycle_id = ? AND kind = 'completion')",
        )
        .bind(cycle_id)
        .bind(cycle_id)
        .fetch_all(db.reader())
        .await?
        .into_iter()
        .map(|(id, cat)| (id, cat == "done"))
        .collect()
    } else {
        HashMap::new()
    };
    let started = found.started_at.map(crate::auth::to_sql_timestamp);
    let closed = found.completed_at.map(crate::auth::to_sql_timestamp);
    let mut report = CycleReport {
        name: found.name.clone(),
        state: found.state.clone(),
        unit: if use_counts {
            "count".to_owned()
        } else {
            project.estimation_unit.to_string()
        },
        committed: 0.0,
        added: 0.0,
        removed: 0.0,
        completed: 0.0,
        cards: Vec::new(),
    };
    for MemberRow {
        id,
        key,
        summary,
        estimate,
        in_scope_at_start: committed,
        added_at,
        removed_at,
        category,
    } in rows
    {
        let weight = if use_counts {
            1.0
        } else {
            estimate.unwrap_or(0.0)
        };
        let added = !committed && started.as_deref().is_some_and(|s| added_at.as_str() > s);
        // Carried out at completion is not "removed during the cycle".
        let removed = removed_at
            .as_deref()
            .is_some_and(|r| closed.as_deref().is_none_or(|c| r < c));
        let completed = if found.completed_at.is_some() {
            done_at_close.get(&id).copied().unwrap_or(false)
        } else {
            category == "done" && removed_at.is_none()
        };
        if committed {
            report.committed += weight;
        }
        if added {
            report.added += weight;
        }
        if removed {
            report.removed += weight;
        }
        if completed {
            report.completed += weight;
        }
        report.cards.push(CycleCard {
            key,
            summary,
            estimate,
            committed,
            added,
            removed,
            completed,
        });
    }
    Ok(report)
}

/// One cycle's bar on the velocity chart.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VelocityBar {
    /// The cycle.
    pub cycle_id: String,
    /// Its name.
    pub name: String,
    /// Committed at start.
    pub committed: f64,
    /// Completed by the end.
    pub completed: f64,
}

/// Velocity: committed vs completed over the last closed cycles, oldest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Velocity {
    /// The bars.
    pub cycles: Vec<VelocityBar>,
    /// The mean completed.
    pub average: f64,
    /// `count` or the estimation unit.
    pub unit: String,
}

/// Velocity over the last `n` closed cycles.
pub async fn velocity(db: &Db, project: &Project, n: i64) -> AppResult<Velocity> {
    require_cycles(project)?;
    let use_counts = counts(project);
    let closed: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM cycles WHERE project_id = ? AND state = 'closed' \
          ORDER BY COALESCE(completed_at, updated_at) DESC LIMIT ?",
    )
    .bind(&project.id)
    .bind(n.clamp(1, 50))
    .fetch_all(db.reader())
    .await?;
    let mut cycles = Vec::new();
    for (id, name) in closed.into_iter().rev() {
        let sum = |kind: &'static str, only_done: bool| {
            let id = id.clone();
            async move {
                // The latest snapshot of that kind: a reopened and re-completed cycle counts
                // its final completion.
                let rows: Vec<(String, Option<f64>)> = sqlx::query_as(
                    "SELECT status_category, estimate FROM cycle_snapshot \
                      WHERE cycle_id = ? AND kind = ? AND taken_at = \
                            (SELECT MAX(taken_at) FROM cycle_snapshot \
                              WHERE cycle_id = ? AND kind = ?)",
                )
                .bind(&id)
                .bind(kind)
                .bind(&id)
                .bind(kind)
                .fetch_all(db.reader())
                .await?;
                AppResult::Ok(
                    rows.iter()
                        .filter(|(c, _)| !only_done || c == "done")
                        .map(|(_, e)| if use_counts { 1.0 } else { e.unwrap_or(0.0) })
                        .sum::<f64>(),
                )
            }
        };
        cycles.push(VelocityBar {
            committed: sum("commitment", false).await?,
            completed: sum("completion", true).await?,
            cycle_id: id.clone(),
            name,
        });
    }
    let average = if cycles.is_empty() {
        0.0
    } else {
        cycles.iter().map(|c| c.completed).sum::<f64>() / cycles.len() as f64
    };
    Ok(Velocity {
        cycles,
        average,
        unit: if use_counts {
            "count".to_owned()
        } else {
            project.estimation_unit.to_string()
        },
    })
}

// ---------------------------------------------------------------------------
// AQL-driven: aggregate (pie/bar/stat gadgets) and two-dimensional statistics
// ---------------------------------------------------------------------------

/// What an aggregate measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    /// How many cards.
    #[default]
    Count,
    /// The sum of their estimates.
    Estimate,
}

/// One group.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    /// The group's key (an id, a name, or `(none)`).
    pub key: String,
    /// What to label it.
    pub label: String,
    /// The measure.
    pub value: f64,
}

/// The result of an aggregate.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
    /// Groups, largest first. One group (`all`) when there is no `groupBy`.
    pub buckets: Vec<Bucket>,
    /// The measure over every matching card.
    pub total: f64,
    /// How many cards matched.
    pub cards: i64,
    /// Whether the card cap cut the result short.
    pub truncated: bool,
}

/// The fields a report may group by.
pub const GROUPABLE: &[&str] = &[
    "status",
    "statusCategory",
    "assignee",
    "reporter",
    "priority",
    "type",
    "project",
    "resolution",
    "labels",
    "component",
    "milestone",
];

/// The SQL taking a JSON array of card ids to `(card_id, group key, group label)` rows.
fn group_sql(field: &str) -> Option<&'static str> {
    Some(match field {
        "status" => {
            "SELECT c.id, s.id, s.name FROM cards c JOIN statuses s ON s.id = c.status_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "statusCategory" => {
            "SELECT c.id, s.category, s.category FROM cards c JOIN statuses s ON s.id = c.status_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "assignee" => {
            "SELECT c.id, COALESCE(u.id, ''), COALESCE(u.display_name, '(unassigned)') FROM cards c LEFT JOIN users u ON u.id = c.assignee_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "reporter" => {
            "SELECT c.id, COALESCE(u.id, ''), COALESCE(u.display_name, '(none)') FROM cards c LEFT JOIN users u ON u.id = c.reporter_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "priority" => {
            "SELECT c.id, COALESCE(p.name, ''), COALESCE(p.name, '(none)') FROM cards c LEFT JOIN priorities p ON p.id = c.priority_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "type" => {
            "SELECT c.id, t.name, t.name FROM cards c JOIN card_types t ON t.id = c.type_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "project" => {
            "SELECT c.id, p.key, p.name FROM cards c JOIN projects p ON p.id = c.project_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "resolution" => {
            "SELECT c.id, COALESCE(r.name, ''), COALESCE(r.name, '(unresolved)') FROM cards c LEFT JOIN resolutions r ON r.id = c.resolution_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "labels" => {
            "SELECT c.id, COALESCE(t.name, ''), COALESCE(t.name, '(none)') FROM cards c LEFT JOIN card_tags ct ON ct.card_id = c.id LEFT JOIN tags t ON t.id = ct.tag_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "component" => {
            "SELECT c.id, COALESCE(k.name, ''), COALESCE(k.name, '(none)') FROM cards c LEFT JOIN card_components cc ON cc.card_id = c.id LEFT JOIN components k ON k.id = cc.component_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        "milestone" => {
            "SELECT c.id, COALESCE(m.name, ''), COALESCE(m.name, '(none)') FROM cards c LEFT JOIN milestones m ON m.id = c.milestone_id WHERE c.id IN (SELECT value FROM json_each(?))"
        }
        _ => return None,
    })
}

async fn groups(
    db: &Db,
    field: &str,
    ids: &[String],
) -> AppResult<HashMap<String, Vec<(String, String)>>> {
    let sql = group_sql(field).ok_or_else(|| {
        AppError::Validation(format!(
            "{field:?} cannot be grouped by. Use one of: {}.",
            GROUPABLE.join(", ")
        ))
    })?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(sql)
        .bind(serde_json::to_string(ids).map_err(AppError::internal)?)
        .fetch_all(db.reader())
        .await?;
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (card, key, label) in rows {
        out.entry(card).or_default().push((key, label));
    }
    Ok(out)
}

struct Matched {
    ids: Vec<String>,
    estimates: HashMap<String, f64>,
    total: i64,
}

async fn matching(db: &Db, viewer: &User, now: DateTime<Utc>, aql: &str) -> AppResult<Matched> {
    let found = crate::aql::search(db, viewer, now, aql, MAX_REPORT_CARDS, 0).await?;
    Ok(Matched {
        estimates: found
            .cards
            .iter()
            .map(|c| (c.id.clone(), c.estimate.unwrap_or(0.0)))
            .collect(),
        ids: found.cards.into_iter().map(|c| c.id).collect(),
        total: found.total,
    })
}

/// Groups the cards matching `aql` by a field and measures each group.
pub async fn aggregate(
    db: &Db,
    viewer: &User,
    now: DateTime<Utc>,
    aql: &str,
    group_by: Option<&str>,
    metric: Metric,
) -> AppResult<Aggregate> {
    let matched = matching(db, viewer, now, aql).await?;
    let measure = |id: &str| match metric {
        Metric::Count => 1.0,
        Metric::Estimate => matched.estimates.get(id).copied().unwrap_or(0.0),
    };
    let total: f64 = matched.ids.iter().map(|id| measure(id)).sum();
    let mut buckets: Vec<Bucket> = match group_by {
        None => vec![Bucket {
            key: "all".into(),
            label: "All".into(),
            value: total,
        }],
        Some(field) => {
            let by_card = groups(db, field, &matched.ids).await?;
            let mut map: BTreeMap<String, Bucket> = BTreeMap::new();
            for id in &matched.ids {
                for (key, label) in by_card.get(id).cloned().unwrap_or_default() {
                    map.entry(key.clone())
                        .or_insert_with(|| Bucket {
                            key,
                            label,
                            value: 0.0,
                        })
                        .value += measure(id);
                }
            }
            map.into_values().collect()
        }
    };
    buckets.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.label.cmp(&b.label))
    });
    Ok(Aggregate {
        buckets,
        total,
        cards: i64::try_from(matched.ids.len()).unwrap_or(i64::MAX),
        truncated: matched.total > i64::try_from(matched.ids.len()).unwrap_or(i64::MAX),
    })
}

/// Two-dimensional filter statistics: counts at every (x, y).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TwoDimensional {
    /// Column headers.
    pub x: Vec<Bucket>,
    /// Row headers.
    pub y: Vec<Bucket>,
    /// `cells[row][column]` — counts.
    pub cells: Vec<Vec<i64>>,
    /// Whether the card cap cut the result short.
    pub truncated: bool,
}

/// Counts cards matching `aql` at every (x, y) — e.g. status × company for a job search.
pub async fn two_dimensional(
    db: &Db,
    viewer: &User,
    now: DateTime<Utc>,
    aql: &str,
    x: &str,
    y: &str,
) -> AppResult<TwoDimensional> {
    let matched = matching(db, viewer, now, aql).await?;
    let xs = groups(db, x, &matched.ids).await?;
    let ys = groups(db, y, &matched.ids).await?;
    let mut columns: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut rows: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    for id in &matched.ids {
        for (xk, xl) in xs.get(id).cloned().unwrap_or_default() {
            columns
                .entry(xk.clone())
                .or_insert_with(|| Bucket {
                    key: xk.clone(),
                    label: xl,
                    value: 0.0,
                })
                .value += 1.0;
            for (yk, yl) in ys.get(id).cloned().unwrap_or_default() {
                rows.entry(yk.clone()).or_insert_with(|| Bucket {
                    key: yk.clone(),
                    label: yl,
                    value: 0.0,
                });
                *counts.entry((yk, xk.clone())).or_insert(0) += 1;
            }
        }
    }
    for ((yk, _), n) in &counts {
        if let Some(row) = rows.get_mut(yk) {
            row.value += *n as f64;
        }
    }
    let x: Vec<Bucket> = columns.into_values().collect();
    let y: Vec<Bucket> = rows.into_values().collect();
    let cells = y
        .iter()
        .map(|row| {
            x.iter()
                .map(|col| {
                    counts
                        .get(&(row.key.clone(), col.key.clone()))
                        .copied()
                        .unwrap_or(0)
                })
                .collect()
        })
        .collect();
    Ok(TwoDimensional {
        x,
        y,
        cells,
        truncated: matched.total > i64::try_from(matched.ids.len()).unwrap_or(i64::MAX),
    })
}

// ---------------------------------------------------------------------------
// Agent costs
// ---------------------------------------------------------------------------

/// Spend in one group.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CostRow {
    /// The group (a project key, a card key, a day, a provider).
    pub key: String,
    /// What to label it.
    pub label: String,
    /// US dollars.
    pub cost_usd: f64,
    /// How many runs/generations.
    pub runs: i64,
}

/// Claude and Gemini spend, for the projects the caller can see.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentCosts {
    /// Total, US dollars.
    pub total_usd: f64,
    /// Per provider (`claude`, `gemini`).
    pub by_provider: Vec<CostRow>,
    /// Per project.
    pub by_project: Vec<CostRow>,
    /// The most expensive cards.
    pub top_cards: Vec<CostRow>,
    /// Per day, oldest first.
    pub by_day: Vec<CostRow>,
}

/// Spend over the last `days` days, limited to `projects` (ids the caller can see).
pub async fn agent_costs(
    db: &Db,
    projects: &[String],
    now: DateTime<Utc>,
    days: i64,
) -> AppResult<AgentCosts> {
    let since = crate::auth::to_sql_timestamp(now - Duration::days(days.clamp(1, 730)));
    let scope = serde_json::to_string(projects).map_err(AppError::internal)?;
    // One unified stream: Claude runs, and (when present) Gemini generations.
    let rows: Vec<(String, String, String, String, String, f64)> = sqlx::query_as(
        "SELECT 'claude', p.key, p.name, c.key, substr(s.started_at, 1, 10), \
                COALESCE(s.total_cost_usd, 0.0) \
           FROM agent_sessions s JOIN cards c ON c.id = s.card_id \
           JOIN projects p ON p.id = c.project_id \
          WHERE s.started_at >= ? AND p.id IN (SELECT value FROM json_each(?)) \
         UNION ALL \
         SELECT 'gemini', p.key, p.name, COALESCE(c.key, ''), substr(g.created_at, 1, 10), \
                COALESCE(g.cost_usd, 0.0) \
           FROM generations g JOIN projects p ON p.id = g.project_id \
           LEFT JOIN cards c ON c.id = g.card_id \
          WHERE g.created_at >= ? AND p.id IN (SELECT value FROM json_each(?))",
    )
    .bind(&since)
    .bind(&scope)
    .bind(&since)
    .bind(&scope)
    .fetch_all(db.reader())
    .await?;
    let mut provider: BTreeMap<String, CostRow> = BTreeMap::new();
    let mut project: BTreeMap<String, CostRow> = BTreeMap::new();
    let mut card: BTreeMap<String, CostRow> = BTreeMap::new();
    let mut day: BTreeMap<String, CostRow> = BTreeMap::new();
    let mut total = 0.0;
    let bump = |map: &mut BTreeMap<String, CostRow>, key: &str, label: &str, cost: f64| {
        let row = map.entry(key.to_owned()).or_insert_with(|| CostRow {
            key: key.to_owned(),
            label: label.to_owned(),
            cost_usd: 0.0,
            runs: 0,
        });
        row.cost_usd += cost;
        row.runs += 1;
    };
    for (kind, pkey, pname, ckey, d, cost) in &rows {
        total += cost;
        bump(&mut provider, kind, kind, *cost);
        bump(&mut project, pkey, pname, *cost);
        if !ckey.is_empty() {
            bump(&mut card, ckey, ckey, *cost);
        }
        bump(&mut day, d, d, *cost);
    }
    let mut top_cards: Vec<CostRow> = card.into_values().collect();
    top_cards.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd));
    top_cards.truncate(20);
    let mut by_project: Vec<CostRow> = project.into_values().collect();
    by_project.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd));
    Ok(AgentCosts {
        total_usd: total,
        by_provider: provider.into_values().collect(),
        by_project,
        top_cards,
        by_day: day.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_groupable_field_has_a_query() {
        for field in GROUPABLE {
            assert!(group_sql(field).is_some(), "{field}");
        }
        assert!(group_sql("password_hash").is_none());
    }

    #[test]
    fn windows_are_inclusive_and_bounded() {
        let now = DateTime::parse_from_rfc3339("2026-03-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (from, to) = window(now, 7);
        assert_eq!(days_between(from, to).len(), 7);
        let (from, _) = window(now, 100_000);
        assert_eq!((now.date_naive() - from).num_days(), 729);
    }
}

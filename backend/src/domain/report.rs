//! Reports computed from `cycle_snapshot`. Today, just burndown; Phase 16's fuller ambition
//! (CFD, burnup, velocity, control chart) is more `GROUP BY`s over the same table, not a
//! different data model — see [`crate::domain::cycle_snapshot`]'s own doc for why the table
//! exists at all.

use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::db::Db;
use crate::domain::EstimationUnit;
use crate::error::AppResult;

/// Whether a burndown counts cards, or sums their estimate.
///
/// Derived once from the *project's* [`EstimationUnit`] — never per card. An unestimated card
/// in a points-tracking project is a real gap in that project's data (it counts as zero, the
/// convention every point-based burndown uses), not a reason to quietly fall back to counting;
/// `TODO.md` asks for the fallback only when the *project* has no estimation field at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BurndownMetric {
    /// The project's estimation unit is `none` — count not-done cards instead.
    Count,
    /// Sum estimates (an unestimated card counts as zero).
    Estimate,
}

impl BurndownMetric {
    /// The metric a project's burndown uses, given how it interprets `estimate`.
    pub fn for_project(estimation_unit: EstimationUnit) -> Self {
        if estimation_unit == EstimationUnit::None {
            Self::Count
        } else {
            Self::Estimate
        }
    }
}

/// One snapshotted day of a burndown: how much was left, and how much was in scope.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BurndownPoint {
    /// The calendar day this point covers, as `cycle_snapshot.taken_at` truncated it —
    /// midnight UTC, so this is really a date wearing a timestamp's clothes.
    pub date: String,
    /// Work not yet done, as of this day.
    pub remaining: f64,
    /// Everything in the cycle's scope, as of this day — the "ideal line" reference, and the
    /// series that would move if scope were added or dropped mid-cycle.
    pub total: f64,
}

/// A cycle's burndown: one point per day it has been snapshotted, oldest first. Empty for a
/// cycle no snapshot job has reached yet — not an error, the same as an empty card list.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Burndown {
    pub metric: BurndownMetric,
    pub points: Vec<BurndownPoint>,
}

/// Computes a cycle's burndown from its `cycle_snapshot` rows.
///
/// Two queries rather than one with a `CASE` on `metric` at bind time — SQLite's driver
/// cannot vary the projected columns' types on a bound parameter, and `estimate` is `REAL`
/// while a plain count is conceptually an integer; keeping them apart means each query reads
/// as what it computes rather than as a hedge against the other one being wanted instead.
pub async fn burndown_for_cycle(
    db: &Db,
    cycle_id: &str,
    metric: BurndownMetric,
) -> AppResult<Burndown> {
    let points: Vec<BurndownPoint> = match metric {
        BurndownMetric::Count => {
            sqlx::query_as(
                "SELECT taken_at AS date, \
                        CAST(SUM(CASE WHEN status_category != 'done' THEN 1 ELSE 0 END) AS REAL) AS remaining, \
                        CAST(COUNT(*) AS REAL) AS total \
                   FROM cycle_snapshot \
                  WHERE cycle_id = ? \
                  GROUP BY taken_at \
                  ORDER BY taken_at",
            )
            .bind(cycle_id)
            .fetch_all(db.reader())
            .await?
        }
        BurndownMetric::Estimate => {
            sqlx::query_as(
                "SELECT taken_at AS date, \
                        SUM(CASE WHEN status_category != 'done' THEN COALESCE(estimate, 0.0) ELSE 0.0 END) AS remaining, \
                        SUM(COALESCE(estimate, 0.0)) AS total \
                   FROM cycle_snapshot \
                  WHERE cycle_id = ? \
                  GROUP BY taken_at \
                  ORDER BY taken_at",
            )
            .bind(cycle_id)
            .fetch_all(db.reader())
            .await?
        }
    };

    Ok(Burndown { metric, points })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::cycle::{self, NewCycle};
    use crate::domain::{cycle_snapshot, project};
    use crate::test_support::TempDb;

    struct Fixture {
        db: Db,
        _temp: TempDb,
        project: project::Project,
        cycle_id: String,
        author_id: String,
    }

    async fn fixture(estimation_unit: EstimationUnit) -> Fixture {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let author = user::insert(
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
        let project = crate::domain::template::create_project(
            &mut tx,
            crate::domain::template::Template::Programming,
            "ATLAS",
            "Atlas",
            None,
            None,
            now(),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE projects SET cycles_enabled = 1, estimation_unit = ? WHERE id = ?")
            .bind(estimation_unit.as_str())
            .bind(&project.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let project = project::find_by_id_tx(&mut tx, &project.id)
            .await
            .unwrap()
            .unwrap();

        let cycle = cycle::insert(
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
        let cycle = cycle::start(
            &mut tx,
            &cycle,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 14).unwrap(),
            now(),
        )
        .await
        .unwrap();
        seed_cards(&mut tx, &project, &author.id, &cycle.id).await;
        tx.commit().await.unwrap();

        Fixture {
            db,
            _temp: temp,
            project,
            cycle_id: cycle.id,
            author_id: author.id,
        }
    }

    /// Three cards in the cycle's scope: two estimated (3, 5), one not — the unestimated one
    /// is the point of the fixture, not an oversight.
    async fn seed_cards(
        tx: &mut sqlx::SqliteConnection,
        project: &project::Project,
        author_id: &str,
        cycle_id: &str,
    ) {
        let type_id: String = sqlx::query_scalar(
            "SELECT id FROM card_types WHERE project_id = ? ORDER BY level DESC, name LIMIT 1",
        )
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();

        for (summary, estimate) in [("A", Some(3.0)), ("B", Some(5.0)), ("C", None)] {
            let created = card::create(
                &mut *tx,
                project,
                &NewCard {
                    type_id: type_id.clone(),
                    parent_id: None,
                    summary: summary.to_owned(),
                    description: None,
                    status_id: None,
                    priority_id: None,
                    assignee_id: None,
                    reporter_id: None,
                    due_date: None,
                    start_date: None,
                    estimate,
                    placement: Placement::Bottom,
                },
                author_id,
                now(),
            )
            .await
            .unwrap();
            cycle::add_card(&mut *tx, &created.id, cycle_id, now())
                .await
                .unwrap();
        }
    }

    /// Asserts two `f64`s are equal to within float error — every value under test here is
    /// an exact sum of small integer literals, so this is a strict-equality assertion in
    /// substance, just spelled the way clippy's `float_cmp` insists a float comparison be.
    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn day(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[tokio::test]
    async fn estimate_metric_sums_estimates_with_an_unestimated_card_counting_as_zero() {
        let f = fixture(EstimationUnit::Points).await;
        cycle_snapshot::take(&f.db, day(2026, 1, 1)).await.unwrap();

        let burndown = burndown_for_cycle(&f.db, &f.cycle_id, BurndownMetric::Estimate)
            .await
            .unwrap();
        assert_eq!(burndown.metric, BurndownMetric::Estimate);
        assert_eq!(burndown.points.len(), 1);
        // 3 + 5 + 0 (unestimated) = 8, all still not-done so remaining == total.
        assert_close(burndown.points[0].total, 8.0);
        assert_close(burndown.points[0].remaining, 8.0);
    }

    #[tokio::test]
    async fn count_metric_counts_cards_regardless_of_estimate() {
        let f = fixture(EstimationUnit::None).await;
        cycle_snapshot::take(&f.db, day(2026, 1, 1)).await.unwrap();

        let burndown = burndown_for_cycle(&f.db, &f.cycle_id, BurndownMetric::Count)
            .await
            .unwrap();
        assert_eq!(burndown.points.len(), 1);
        assert_close(burndown.points[0].total, 3.0);
        assert_close(burndown.points[0].remaining, 3.0);
    }

    #[tokio::test]
    async fn remaining_drops_as_cards_finish_but_total_holds_the_original_scope() {
        let f = fixture(EstimationUnit::Points).await;
        cycle_snapshot::take(&f.db, day(2026, 1, 1)).await.unwrap();

        // Move card "A" (estimate 3) to Done, then take tomorrow's snapshot.
        let done_status: String = sqlx::query_scalar(
            "SELECT id FROM statuses WHERE project_id = ? AND category = 'done' LIMIT 1",
        )
        .bind(&f.project.id)
        .fetch_one(f.db.reader())
        .await
        .unwrap();
        let card_a: String = sqlx::query_scalar("SELECT id FROM cards WHERE summary = 'A'")
            .fetch_one(f.db.reader())
            .await
            .unwrap();
        let mut tx = f.db.begin_write().await.unwrap();
        let card = card::find_by_id_tx(&mut tx, &card_a)
            .await
            .unwrap()
            .unwrap();
        card::update(
            &mut tx,
            &card,
            &card::CardPatch {
                status_id: Some(done_status),
                ..Default::default()
            },
            Some(&f.author_id),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        cycle_snapshot::take(&f.db, day(2026, 1, 2)).await.unwrap();

        let burndown = burndown_for_cycle(&f.db, &f.cycle_id, BurndownMetric::Estimate)
            .await
            .unwrap();
        assert_eq!(burndown.points.len(), 2);
        assert_close(burndown.points[0].remaining, 8.0);
        assert_close(burndown.points[0].total, 8.0);
        // Day two: A is done, so remaining drops by its 3 points but total — the scope that
        // was committed — does not move.
        assert_close(burndown.points[1].remaining, 5.0);
        assert_close(burndown.points[1].total, 8.0);
    }

    #[tokio::test]
    async fn a_cycle_with_no_snapshots_yet_is_an_empty_burndown_not_an_error() {
        let f = fixture(EstimationUnit::Points).await;
        let burndown = burndown_for_cycle(&f.db, &f.cycle_id, BurndownMetric::Estimate)
            .await
            .unwrap();
        assert!(burndown.points.is_empty());
    }

    #[test]
    fn the_metric_follows_the_projects_estimation_unit() {
        assert_eq!(
            BurndownMetric::for_project(EstimationUnit::None),
            BurndownMetric::Count
        );
        for unit in [
            EstimationUnit::Points,
            EstimationUnit::Hours,
            EstimationUnit::Days,
            EstimationUnit::Tshirt,
            EstimationUnit::Count,
        ] {
            assert_eq!(BurndownMetric::for_project(unit), BurndownMetric::Estimate);
        }
    }
}

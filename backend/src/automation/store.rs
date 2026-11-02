//! `automation_rules` and `automation_runs`.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::automation::model::{RuleSpec, Trigger};
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// Longest rule name.
pub const MAX_NAME: usize = 120;

#[derive(Debug, Clone, FromRow)]
struct RuleRow {
    id: String,
    project_id: String,
    name: String,
    description: Option<String>,
    enabled: bool,
    trigger: String,
    conditions: String,
    actions: String,
    owner_id: String,
    has_webhook: bool,
    next_run_at: Option<DateTime<Utc>>,
    last_run_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A rule, as the API shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// UUID v7.
    pub id: String,
    /// The project it belongs to.
    pub project_id: String,
    /// A name.
    pub name: String,
    /// What it is for.
    pub description: Option<String>,
    /// Whether it runs.
    pub enabled: bool,
    /// Trigger, conditions, actions.
    pub spec: RuleSpec,
    /// Whose authority it acts with.
    pub owner_id: String,
    /// Whether an incoming-webhook token has been issued.
    pub has_webhook: bool,
    /// For a scheduled rule: when it next runs.
    pub next_run_at: Option<DateTime<Utc>>,
    /// When it last ran.
    pub last_run_at: Option<DateTime<Utc>>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<RuleRow> for Rule {
    type Error = AppError;
    fn try_from(row: RuleRow) -> AppResult<Self> {
        Ok(Self {
            spec: RuleSpec::from_json(&row.trigger, &row.conditions, &row.actions)?,
            id: row.id,
            project_id: row.project_id,
            name: row.name,
            description: row.description,
            enabled: row.enabled,
            owner_id: row.owner_id,
            has_webhook: row.has_webhook,
            next_run_at: row.next_run_at,
            last_run_at: row.last_run_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

macro_rules! select {
    ($tail:literal) => {
        concat!(
            "SELECT id, project_id, name, description, enabled, trigger, conditions, \
             actions, owner_id, webhook_token_hash IS NOT NULL AS has_webhook, next_run_at, \
             last_run_at, created_at, updated_at FROM automation_rules ",
            $tail
        )
    };
}

/// Checks a rule name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "A rule name must be 1 to {MAX_NAME} characters."
        )));
    }
    Ok(name.to_owned())
}

fn rows(rows: Vec<RuleRow>) -> AppResult<Vec<Rule>> {
    rows.into_iter().map(Rule::try_from).collect()
}

/// A project's rules, by name.
pub async fn list(db: &Db, project_id: &str) -> AppResult<Vec<Rule>> {
    rows(
        sqlx::query_as::<_, RuleRow>(select!("WHERE project_id = ? ORDER BY name"))
            .bind(project_id)
            .fetch_all(db.reader())
            .await?,
    )
}

/// The enabled rules of a project — what an event is offered to.
pub async fn enabled_for(db: &Db, project_id: &str) -> AppResult<Vec<Rule>> {
    rows(
        sqlx::query_as::<_, RuleRow>(select!(
            "WHERE project_id = ? AND enabled = 1 ORDER BY created_at"
        ))
        .bind(project_id)
        .fetch_all(db.reader())
        .await?,
    )
}

/// Enabled scheduled rules that are due.
pub async fn due(db: &Db, now: DateTime<Utc>) -> AppResult<Vec<Rule>> {
    rows(
        sqlx::query_as::<_, RuleRow>(select!(
            "WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ? \
             ORDER BY next_run_at LIMIT 50"
        ))
        .bind(to_sql_timestamp(now))
        .fetch_all(db.reader())
        .await?,
    )
}

/// Finds a rule.
pub async fn find(db: &Db, id: &str) -> AppResult<Option<Rule>> {
    sqlx::query_as::<_, RuleRow>(select!("WHERE id = ?"))
        .bind(id)
        .fetch_optional(db.reader())
        .await?
        .map(Rule::try_from)
        .transpose()
}

/// Finds the rule an incoming-webhook token belongs to.
pub async fn find_by_webhook_token(db: &Db, token: &str) -> AppResult<Option<Rule>> {
    sqlx::query_as::<_, RuleRow>(select!("WHERE webhook_token_hash = ?"))
        .bind(token_hash(token))
        .fetch_optional(db.reader())
        .await?
        .map(Rule::try_from)
        .transpose()
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// When a scheduled trigger next fires after `now`.
pub fn next_run(trigger: &Trigger, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match trigger {
        Trigger::Scheduled {
            interval_minutes, ..
        } => Some(now + Duration::minutes(*interval_minutes)),
        _ => None,
    }
}

/// The fields of a new or edited rule.
#[derive(Debug)]
pub struct RuleInput<'a> {
    /// Already validated.
    pub name: &'a str,
    /// Optional.
    pub description: Option<&'a str>,
    /// On or off.
    pub enabled: bool,
    /// Already validated.
    pub spec: &'a RuleSpec,
}

fn spec_json(spec: &RuleSpec) -> AppResult<(String, String, String)> {
    Ok((
        serde_json::to_string(&spec.trigger).map_err(AppError::internal)?,
        serde_json::to_string(&spec.conditions).map_err(AppError::internal)?,
        serde_json::to_string(&spec.actions).map_err(AppError::internal)?,
    ))
}

/// Creates a rule.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    owner_id: &str,
    input: &RuleInput<'_>,
    now: DateTime<Utc>,
) -> AppResult<String> {
    let id = Uuid::now_v7().to_string();
    let (trigger, conditions, actions) = spec_json(input.spec)?;
    sqlx::query(
        "INSERT INTO automation_rules (id, project_id, name, description, enabled, trigger, \
         conditions, actions, owner_id, webhook_token_hash, next_run_at, last_run_at, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(input.name)
    .bind(input.description)
    .bind(input.enabled)
    .bind(trigger)
    .bind(conditions)
    .bind(actions)
    .bind(owner_id)
    .bind(next_run(&input.spec.trigger, now).map(to_sql_timestamp))
    .bind(to_sql_timestamp(now))
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

/// Replaces a rule's definition. Re-arms its schedule from now.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    input: &RuleInput<'_>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let (trigger, conditions, actions) = spec_json(input.spec)?;
    sqlx::query(
        "UPDATE automation_rules SET name = ?, description = ?, enabled = ?, trigger = ?, \
         conditions = ?, actions = ?, next_run_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(input.name)
    .bind(input.description)
    .bind(input.enabled)
    .bind(trigger)
    .bind(conditions)
    .bind(actions)
    .bind(next_run(&input.spec.trigger, now).map(to_sql_timestamp))
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Deletes a rule and its audit log.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM automation_rules WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Issues a fresh webhook token, replacing any previous one. Returns the token — shown once.
///
/// `OsRng` is argon2's re-export, matching `auth::session` and `agent::mcp::capability`.
pub async fn issue_webhook_token(db: &Db, id: &str) -> AppResult<String> {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let mut tx = db.begin_write().await?;
    sqlx::query("UPDATE automation_rules SET webhook_token_hash = ? WHERE id = ?")
        .bind(token_hash(&token))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(token)
}

/// Records that a scheduled rule ran, and arms the next run.
pub async fn mark_ran(db: &Db, rule: &Rule, now: DateTime<Utc>) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query("UPDATE automation_rules SET last_run_at = ?, next_run_at = ? WHERE id = ?")
        .bind(to_sql_timestamp(now))
        .bind(next_run(&rule.spec.trigger, now).map(to_sql_timestamp))
        .bind(&rule.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, FromRow)]
struct RunRow {
    id: String,
    rule_id: String,
    card_id: Option<String>,
    card_key: Option<String>,
    trigger: String,
    status: String,
    trace: String,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}

/// One execution in the audit log.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// UUID v7.
    pub id: String,
    /// The rule.
    pub rule_id: String,
    /// The card it ran against, if any.
    pub card_id: Option<String>,
    /// The card's key, if it still exists.
    pub card_key: Option<String>,
    /// Which trigger fired.
    pub trigger: String,
    /// `success`, `no_match`, `failed`, `loop_blocked`, `rate_limited`, `dry_run`.
    pub status: String,
    /// Conditions evaluated, actions taken, and every smart value's resolution.
    #[schema(value_type = Object)]
    pub trace: Value,
    /// Why it failed.
    pub error: Option<String>,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// When it finished.
    pub finished_at: DateTime<Utc>,
}

/// A rule's most recent runs, newest first.
pub async fn runs(db: &Db, rule_id: &str, limit: i64) -> AppResult<Vec<Run>> {
    let rows = sqlx::query_as::<_, RunRow>(
        "SELECT r.id, r.rule_id, r.card_id, c.key AS card_key, r.trigger, r.status, r.trace, \
                r.error, r.started_at, r.finished_at \
           FROM automation_runs r LEFT JOIN cards c ON c.id = r.card_id \
          WHERE r.rule_id = ? ORDER BY r.started_at DESC, r.id DESC LIMIT ?",
    )
    .bind(rule_id)
    .bind(limit.clamp(1, 200))
    .fetch_all(db.reader())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Run {
            trace: serde_json::from_str(&row.trace).unwrap_or(Value::Null),
            id: row.id,
            rule_id: row.rule_id,
            card_id: row.card_id,
            card_key: row.card_key,
            trigger: row.trigger,
            status: row.status,
            error: row.error,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
        .collect())
}

/// How many times a rule has run since `since` (for the execution cap).
pub async fn runs_since(db: &Db, rule_id: &str, since: DateTime<Utc>) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM automation_runs WHERE rule_id = ? AND started_at >= ? \
           AND status IN ('success', 'failed')",
    )
    .bind(rule_id)
    .bind(to_sql_timestamp(since))
    .fetch_one(db.reader())
    .await?)
}

/// What one execution recorded.
#[derive(Debug)]
pub struct NewRun<'a> {
    /// The rule.
    pub rule_id: &'a str,
    /// The card.
    pub card_id: Option<&'a str>,
    /// The trigger kind.
    pub trigger: &'a str,
    /// The outcome.
    pub status: &'a str,
    /// The trace.
    pub trace: &'a Value,
    /// Why it failed.
    pub error: Option<&'a str>,
    /// When it started.
    pub started_at: DateTime<Utc>,
}

/// Appends to the audit log, keeping the newest 500 runs per rule.
pub async fn record_run(db: &Db, run: &NewRun<'_>, now: DateTime<Utc>) -> AppResult<String> {
    let id = Uuid::now_v7().to_string();
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "INSERT INTO automation_runs (id, rule_id, card_id, trigger, status, trace, error, \
         started_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(run.rule_id)
    .bind(run.card_id)
    .bind(run.trigger)
    .bind(run.status)
    .bind(run.trace.to_string())
    .bind(run.error)
    .bind(to_sql_timestamp(run.started_at))
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM automation_runs WHERE rule_id = ? AND id NOT IN \
         (SELECT id FROM automation_runs WHERE rule_id = ? ORDER BY started_at DESC LIMIT 500)",
    )
    .bind(run.rule_id)
    .bind(run.rule_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scheduled_triggers_arm_a_next_run() {
        let now = Utc::now();
        assert_eq!(
            next_run(
                &Trigger::Scheduled {
                    interval_minutes: 60,
                    aql: None
                },
                now
            ),
            Some(now + Duration::minutes(60))
        );
        assert_eq!(next_run(&Trigger::Manual, now), None);
    }

    #[test]
    fn tokens_are_stored_only_as_a_digest() {
        let digest = token_hash("secret");
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, "secret");
    }
}

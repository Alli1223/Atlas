//! CLI-reported terminal usage. Missing metrics remain unknown; no inferred billing.
use crate::{
    auth::{now, to_sql_timestamp},
    db::Db,
    error::AppResult,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;

#[derive(Debug, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub provider: String,
    pub runs: i64,
    pub active_runs: i64,
    pub measured_runs: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

fn metrics(line: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(line).ok()?;
    if matches!(
        value["type"].as_str(),
        Some("turn.completed" | "turn.failed" | "result")
    ) {
        Some(value)
    } else if value["event"] == "result" {
        Some(value["result"].clone())
    } else {
        None
    }
}

pub async fn record(
    db: &Db,
    run_id: &str,
    actor: &str,
    provider: &str,
    line: &str,
) -> AppResult<()> {
    let Some(value) = metrics(line) else {
        return Ok(());
    };
    let usage = &value["usage"];
    let cost = if provider == "claude" && value["terminal_reason"] != "timeout" {
        value["total_cost_usd"]
            .as_f64()
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
    } else {
        None
    };
    sqlx::query("INSERT INTO agent_usage(run_id, actor_id, provider, input_tokens, output_tokens, cached_tokens, cost_usd, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(run_id) DO UPDATE SET input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, cached_tokens=excluded.cached_tokens, cost_usd=excluded.cost_usd")
        .bind(run_id).bind(actor).bind(provider)
        .bind(usage["input_tokens"].as_i64().filter(|n| *n >= 0))
        .bind(usage["output_tokens"].as_i64().filter(|n| *n >= 0))
        .bind(usage["cached_input_tokens"].as_i64().or_else(|| usage["cache_read_input_tokens"].as_i64()).or_else(|| usage["cache_read_tokens"].as_i64()).filter(|n| *n >= 0))
        .bind(cost).bind(to_sql_timestamp(now())).execute(db.writer()).await?;
    Ok(())
}

pub async fn summary(db: &Db, actor: &str) -> AppResult<Vec<ProviderUsage>> {
    Ok(sqlx::query_as("WITH providers(provider) AS (VALUES ('codex'), ('claude'), ('antigravity')), totals AS (SELECT provider, COUNT(*) AS runs, COUNT(input_tokens) AS measured_runs, SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens, SUM(cached_tokens) AS cached_tokens, SUM(cost_usd) AS cost_usd FROM agent_usage WHERE actor_id = ? AND datetime(created_at) >= datetime('now', '-1 day') GROUP BY provider) SELECT p.provider, COALESCE(t.runs, 0) AS runs, COALESCE(t.measured_runs, 0) AS measured_runs, t.input_tokens, t.output_tokens, t.cached_tokens, t.cost_usd, (SELECT COUNT(*) FROM agent_sessions s WHERE s.provider=p.provider AND s.started_by=? AND s.status='running') + (SELECT COUNT(*) FROM board_auto_runs b WHERE b.provider=p.provider AND b.started_by=? AND b.status='running' AND b.current_session_id IS NULL) AS active_runs FROM providers p LEFT JOIN totals t ON t.provider=p.provider")
        .bind(actor).bind(actor).bind(actor).fetch_all(db.reader()).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_terminal_events_supply_usage() {
        assert!(metrics(r#"{"type":"turn.failed","error":{"message":"Failed"}}"#).is_some());
        assert!(metrics(r#"{"type":"assistant","usage":{"input_tokens":10}}"#).is_none());
        assert_eq!(
            metrics(r#"{"type":"turn.completed","usage":{"input_tokens":10}}"#).unwrap()["usage"]["input_tokens"],
            10
        );
        assert_eq!(
            metrics(r#"{"event":"result","result":{"usage":{"output_tokens":20}}}"#).unwrap()["usage"]
                ["output_tokens"],
            20
        );
    }
    #[tokio::test]
    async fn usage_is_idempotent_private_and_limited_to_the_last_day() {
        use crate::{
            auth::{Role, user},
            db::migrate,
            test_support::TempDb,
        };
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let actor = user::insert(
            &mut tx,
            &user::NewUser {
                username: "usage".to_owned(),
                email: None,
                display_name: "Usage".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let codex = r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20,"cached_input_tokens":80}}"#;
        record(&db, "run", &actor.id, "codex", codex).await.unwrap();
        record(&db, "run", &actor.id, "codex", codex).await.unwrap();
        record(&db, "old", &actor.id, "codex", codex).await.unwrap();
        sqlx::query(
            "UPDATE agent_usage SET created_at = datetime('now', '-25 hours') WHERE run_id = 'old'",
        )
        .execute(db.writer())
        .await
        .unwrap();
        let usage = summary(&db, &actor.id).await.unwrap();
        let codex = usage
            .iter()
            .find(|usage| usage.provider == "codex")
            .unwrap();
        assert_eq!(codex.runs, 1);
        assert_eq!(codex.input_tokens, Some(100));
        assert_eq!(codex.cached_tokens, Some(80));
        assert_eq!(codex.cost_usd, None);
        assert!(
            summary(&db, "another-user")
                .await
                .unwrap()
                .iter()
                .all(|usage| usage.runs == 0)
        );
        record(
            &db,
            "agy",
            &actor.id,
            "antigravity",
            r#"{"event":"result","result":{"usage":{"input_tokens":5,"cache_read_tokens":3}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            summary(&db, &actor.id)
                .await
                .unwrap()
                .iter()
                .find(|usage| usage.provider == "antigravity")
                .unwrap()
                .cached_tokens,
            Some(3)
        );
    }
}

//! Agent assignments and explicit, project-scoped task dependencies.
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::auth::{now, to_sql_timestamp};
use crate::db::Db;
use crate::domain::{
    StatusCategory,
    card::{self, Card, CardPatch},
    comment, config, workflow,
};
use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Dependency {
    pub key: String,
    pub summary: String,
    pub reason: String,
    pub done: bool,
}
#[derive(Debug, Serialize, FromRow, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub provider: String,
    pub reason: String,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardAutomation {
    pub assignment: Option<Assignment>,
    pub blocked_by: Vec<Dependency>,
}

pub async fn info(db: &Db, card_id: &str) -> AppResult<CardAutomation> {
    let assignment = sqlx::query_as::<_, Assignment>(
        "SELECT provider, reason FROM card_agent_assignments WHERE card_id = ?",
    )
    .bind(card_id)
    .fetch_optional(db.reader())
    .await?;
    let blocked_by = sqlx::query_as::<_, Dependency>(
        "SELECT c.key, c.summary, d.reason, s.category = 'done' AS done FROM card_dependencies d \
         JOIN cards c ON c.id = d.blocked_by_id JOIN statuses s ON s.id = c.status_id \
         WHERE d.card_id = ? AND c.deleted_at IS NULL ORDER BY c.key",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?;
    Ok(CardAutomation {
        assignment,
        blocked_by,
    })
}

pub async fn ensure_lanes(db: &Db, project_id: &str) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    for name in ["Blocked", "In Review"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM statuses WHERE project_id = ? AND lower(name) = lower(?))",
        )
        .bind(project_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            let position: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM statuses WHERE project_id = ?",
            )
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
            config::insert_status(
                &mut tx,
                project_id,
                name,
                StatusCategory::InProgress,
                position,
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// All automatic lane changes use the same workflow gates and history as human moves.
pub async fn move_named(db: &Db, card_id: &str, name: &str, actor: &str) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    move_named_tx(&mut tx, card_id, name, actor).await?;
    tx.commit().await?;
    Ok(())
}

async fn move_named_tx(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    name: &str,
    actor: &str,
) -> AppResult<()> {
    let card = card::find_by_id_tx(tx, card_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let status = sqlx::query_as::<_, config::Status>("SELECT id, project_id, name, category, position FROM statuses WHERE project_id = ? AND lower(name) = lower(?)")
        .bind(&card.project_id).bind(name).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::Conflict(format!("Create a {name} status in project settings first.")))?;
    if card.status_id == status.id {
        return Ok(());
    }
    match workflow::resolve_transition(tx, &card, &status.id, Some(actor)).await? {
        workflow::Outcome::Via(transition) => {
            card::execute_transition(
                tx,
                &card,
                &transition,
                CardPatch::default(),
                None,
                Some(actor),
                now(),
            )
            .await?;
        }
        workflow::Outcome::Permissive => {
            card::update(
                tx,
                &card,
                &CardPatch {
                    status_id: Some(status.id),
                    ..CardPatch::default()
                },
                Some(actor),
                now(),
            )
            .await?;
        }
    }
    Ok(())
}

/// A dependency must stay inside the card's project and cannot create a cycle.
pub async fn block(
    db: &Db,
    card: &Card,
    blocker_key: &str,
    reason: &str,
    actor: &str,
) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    let blocker = card::find_by_key_tx(&mut tx, blocker_key)
        .await?
        .ok_or(AppError::NotFound)?;
    if blocker.project_id != card.project_id {
        return Err(AppError::NotFound);
    }
    if blocker.id == card.id {
        return Err(AppError::Validation(
            "A blocker must be another ticket in the same project.".to_owned(),
        ));
    }
    let cycle: bool = sqlx::query_scalar(
        "WITH RECURSIVE chain(id) AS (SELECT blocked_by_id FROM card_dependencies WHERE card_id = ? \
         UNION SELECT d.blocked_by_id FROM card_dependencies d JOIN chain ON d.card_id = chain.id) \
         SELECT EXISTS(SELECT 1 FROM chain WHERE id = ?)")
        .bind(&blocker.id).bind(&card.id).fetch_one(&mut *tx).await?;
    if cycle {
        return Err(AppError::Validation(
            "This dependency would create a cycle.".to_owned(),
        ));
    }
    let reason = comment::validate_body(reason)?;
    let inserted = sqlx::query("INSERT OR IGNORE INTO card_dependencies(card_id, blocked_by_id, reason, created_by, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&card.id).bind(&blocker.id).bind(&reason).bind(actor).bind(to_sql_timestamp(now()))
        .execute(&mut *tx).await?.rows_affected();
    if inserted > 0 {
        comment::insert(
            &mut tx,
            &card.id,
            actor,
            &format!("**Blocked by {}**: {reason}", blocker.key),
            now(),
        )
        .await?;
    }
    let category: String = sqlx::query_scalar("SELECT category FROM statuses WHERE id = ?")
        .bind(&blocker.status_id)
        .fetch_one(&mut *tx)
        .await?;
    if category != "done" {
        move_named_tx(&mut tx, &card.id, "Blocked", actor).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn assign(
    db: &Db,
    card: &Card,
    provider: &str,
    reason: &str,
    actor: &str,
) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query("INSERT INTO card_agent_assignments(card_id, provider, reason, assigned_by, assigned_at) VALUES (?, ?, ?, ?, ?) \
        ON CONFLICT(card_id) DO UPDATE SET provider = excluded.provider, reason = excluded.reason, assigned_by = excluded.assigned_by, assigned_at = excluded.assigned_at")
        .bind(&card.id).bind(provider).bind(reason).bind(actor).bind(to_sql_timestamp(now())).execute(&mut *tx).await?;
    comment::insert(
        &mut tx,
        &card.id,
        actor,
        &format!("**Auto assigned {provider}**\n\n{reason}"),
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

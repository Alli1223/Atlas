//! `agent_sessions`: one run of Claude Code against a card.
//!
//! Session metadata and its terminal outcome — not the live event stream. The full
//! transcript (every `stream-json` line) is later work in `TODO.md`'s Phase 13 ("persist
//! transcripts"), and lands as a sibling table keyed on this one's `id`, not a change to this
//! shape.
//!
//! This module knows nothing about [`crate::agent`] — spawning the CLI, streaming its
//! events, preparing a workspace — the same direction every other domain module keeps with
//! [`crate::integrations`]: the mechanics layer depends on the domain layer, never the
//! reverse. Something in `crate::agent` (or the API handler wiring the two together) is
//! responsible for turning a finished run into a call to [`finish`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, SqliteConnection, Type};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// An agent session's lifecycle state.
///
/// `Running` is the one value nothing in [`crate::agent::claude_code::outcome`] ever
/// produces — that function only classifies a *finished* result event. `Cancelled` is
/// likewise never a CLI outcome; it is Atlas's own doing when a caller ends a run early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionStatus {
    Running,
    Completed,
    /// Finished cleanly, but at least one tool call was silently denied.
    CompletedWithDenials,
    /// Hit `--max-turns`/`--max-budget-usd` rather than failing outright.
    LimitReached,
    Failed,
    /// Ended early by Atlas, not by the CLI.
    Cancelled,
}

impl AgentSessionStatus {
    /// The status's database and JSON spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::CompletedWithDenials => "completed_with_denials",
            Self::LimitReached => "limit_reached",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether a session in this status is still live — no terminal outcome yet.
    #[must_use]
    pub fn is_running(self) -> bool {
        self == Self::Running
    }
}

impl fmt::Display for AgentSessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an agent session status could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown agent session status {0:?}")]
pub struct AgentSessionStatusError(String);

impl FromStr for AgentSessionStatus {
    type Err = AgentSessionStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "completed_with_denials" => Ok(Self::CompletedWithDenials),
            "limit_reached" => Ok(Self::LimitReached),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(AgentSessionStatusError(other.to_owned())),
        }
    }
}

// The same sqlx shape as `CycleState`/`StatusCategory`: stored as TEXT, validated on read.
// The CHECK constraint in the migration and this Decode impl are two independent guards
// against a status that means nothing.

impl Type<Sqlite> for AgentSessionStatus {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for AgentSessionStatus {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for AgentSessionStatus {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// A row of `agent_sessions`.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    /// UUID v7, as text.
    pub id: String,
    pub card_id: String,
    /// The CLI's own session id — present from the moment the run is spawned, since Atlas
    /// generates it up front (see `agent::runner::spawn_local`) rather than scraping it back
    /// from the `system/init` event.
    pub claude_session_id: Option<String>,
    pub status: AgentSessionStatus,
    /// What was actually sent — not reconstructed from the card after the fact, since cards
    /// get edited and a session's record of what it was asked must not drift with them.
    pub prompt: String,
    /// The terminal `result` event's `result` field. `None` until finished, and may stay
    /// `None` even then — absent on every CLI error subtype.
    pub result_text: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    /// Set on `failed`/`cancelled`.
    pub error_message: Option<String>,
    /// `None` if the starting user's account was later removed.
    pub started_by: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A new session, ready to insert. Always starts `running`, with no outcome fields set.
#[derive(Debug)]
pub struct NewAgentSession<'a> {
    pub card_id: &'a str,
    pub claude_session_id: &'a str,
    pub prompt: &'a str,
    pub started_by: Option<&'a str>,
}

/// The outcome fields [`finish`] sets. Never `Running` — see [`finish`]'s own doc.
#[derive(Debug)]
pub struct SessionOutcome<'a> {
    pub status: AgentSessionStatus,
    pub result_text: Option<&'a str>,
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub error_message: Option<&'a str>,
}

macro_rules! agent_session_columns {
    () => {
        "id, card_id, claude_session_id, status, prompt, result_text, total_cost_usd, \
         num_turns, error_message, started_by, started_at, ended_at, created_at, updated_at"
    };
}

/// Records a session as started. `status` is always `running` — [`finish`] is the only way
/// to move it anywhere else.
pub async fn insert(
    tx: &mut SqliteConnection,
    new: &NewAgentSession<'_>,
    now: DateTime<Utc>,
) -> AppResult<AgentSession> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO agent_sessions \
         (id, card_id, claude_session_id, status, prompt, started_by, started_at, \
          created_at, updated_at) \
         VALUES (?, ?, ?, 'running', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(new.card_id)
    .bind(new.claude_session_id)
    .bind(new.prompt)
    .bind(new.started_by)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the session just inserted is missing")))
}

/// Finds a session by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<AgentSession>> {
    Ok(sqlx::query_as::<_, AgentSession>(concat!(
        "SELECT ",
        agent_session_columns!(),
        " FROM agent_sessions WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a session by id inside an open transaction.
pub async fn find_by_id_tx(tx: &mut SqliteConnection, id: &str) -> AppResult<Option<AgentSession>> {
    Ok(sqlx::query_as::<_, AgentSession>(concat!(
        "SELECT ",
        agent_session_columns!(),
        " FROM agent_sessions WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A card's sessions, most recent first.
pub async fn list_for_card(db: &Db, card_id: &str) -> AppResult<Vec<AgentSession>> {
    Ok(sqlx::query_as::<_, AgentSession>(concat!(
        "SELECT ",
        agent_session_columns!(),
        " FROM agent_sessions WHERE card_id = ? ORDER BY started_at DESC, id DESC"
    ))
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// Records a session's terminal outcome. Requires the session to still be `running` — a
/// finished session's record does not change after the fact, the same invariant
/// [`crate::domain::card::update`]'s history keeps for a card's fields.
///
/// # Errors
///
/// [`AppError::Conflict`] if the session is not currently `running`.
pub async fn finish(
    tx: &mut SqliteConnection,
    session: &AgentSession,
    outcome: &SessionOutcome<'_>,
    now: DateTime<Utc>,
) -> AppResult<AgentSession> {
    if !session.status.is_running() {
        return Err(AppError::Conflict(format!(
            "this session already finished as {}",
            session.status
        )));
    }

    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "UPDATE agent_sessions SET status = ?, result_text = ?, total_cost_usd = ?, \
         num_turns = ?, error_message = ?, ended_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(outcome.status)
    .bind(outcome.result_text)
    .bind(outcome.total_cost_usd)
    .bind(outcome.num_turns)
    .bind(outcome.error_message)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(&session.id)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &session.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the session just finished is missing")))
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
                type_id: type_id.clone(),
                parent_id: None,
                summary: "Fix the thing".to_owned(),
                description: Some("Do the needful.".to_owned()),
                status_id: None,
                priority_id: None,
                assignee_id: None,
                reporter_id: None,
                due_date: None,
                start_date: None,
                estimate: None,
                placement: Placement::Bottom,
            },
            &creator.id,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, created.id, creator.id)
    }

    #[tokio::test]
    async fn a_session_starts_running_and_carries_the_prompt_it_was_given() {
        let (db, _temp, card_id, user_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let session = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "11111111-1111-1111-1111-111111111111",
                prompt: "Fix the thing\n\nDo the needful.",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(session.status, AgentSessionStatus::Running);
        assert!(session.status.is_running());
        assert_eq!(session.prompt, "Fix the thing\n\nDo the needful.");
        assert_eq!(
            session.claude_session_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert!(session.result_text.is_none());
        assert!(session.ended_at.is_none());
    }

    #[tokio::test]
    async fn finishing_records_the_outcome_and_stamps_ended_at() {
        let (db, _temp, card_id, user_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let session = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "s-1",
                prompt: "do it",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let finished = finish(
            &mut tx,
            &session,
            &SessionOutcome {
                status: AgentSessionStatus::Completed,
                result_text: Some("done"),
                total_cost_usd: Some(0.42),
                num_turns: Some(3),
                error_message: None,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(finished.status, AgentSessionStatus::Completed);
        assert_eq!(finished.result_text.as_deref(), Some("done"));
        assert_eq!(finished.total_cost_usd, Some(0.42));
        assert_eq!(finished.num_turns, Some(3));
        assert!(finished.ended_at.is_some());
    }

    #[tokio::test]
    async fn a_failed_run_records_an_error_message_and_no_result_text() {
        let (db, _temp, card_id, user_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let session = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "s-2",
                prompt: "do it",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let finished = finish(
            &mut tx,
            &session,
            &SessionOutcome {
                status: AgentSessionStatus::Failed,
                result_text: None,
                total_cost_usd: Some(0.0),
                num_turns: Some(0),
                error_message: Some("Invalid API key"),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(finished.status, AgentSessionStatus::Failed);
        assert!(finished.result_text.is_none());
        assert_eq!(finished.error_message.as_deref(), Some("Invalid API key"));
    }

    #[tokio::test]
    async fn a_session_cannot_be_finished_twice() {
        let (db, _temp, card_id, user_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let session = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "s-3",
                prompt: "do it",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let finished = finish(
            &mut tx,
            &session,
            &SessionOutcome {
                status: AgentSessionStatus::Completed,
                result_text: Some("done"),
                total_cost_usd: Some(0.1),
                num_turns: Some(1),
                error_message: None,
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let err = finish(
            &mut tx,
            &finished,
            &SessionOutcome {
                status: AgentSessionStatus::Failed,
                result_text: None,
                total_cost_usd: None,
                num_turns: None,
                error_message: Some("too late"),
            },
            now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_cards_sessions_list_most_recent_first() {
        let (db, _temp, card_id, user_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        let first = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "s-first",
                prompt: "first",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let second = insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card_id,
                claude_session_id: "s-second",
                prompt: "second",
                started_by: Some(&user_id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let sessions = list_for_card(&db, &card_id).await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, second.id);
        assert_eq!(sessions[1].id, first.id);
    }

    #[test]
    fn statuses_round_trip_through_their_database_spelling() {
        for status in [
            AgentSessionStatus::Running,
            AgentSessionStatus::Completed,
            AgentSessionStatus::CompletedWithDenials,
            AgentSessionStatus::LimitReached,
            AgentSessionStatus::Failed,
            AgentSessionStatus::Cancelled,
        ] {
            assert_eq!(
                status.as_str().parse::<AgentSessionStatus>().unwrap(),
                status
            );
        }
        assert!("bogus".parse::<AgentSessionStatus>().is_err());
    }
}

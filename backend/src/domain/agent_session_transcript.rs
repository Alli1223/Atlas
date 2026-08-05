//! `agent_session_transcript`: the full `stream-json` transcript behind an agent session.
//!
//! [`crate::domain::agent_session`] records a run's status and terminal outcome only; this is
//! the line-by-line record that sits behind it — every raw line the CLI wrote to stdout, in
//! arrival order, kept verbatim rather than a re-serialization of the parsed event (see
//! `agent::runner::RunEvent`'s own comment on why the raw line is worth keeping even once
//! parsed). This module knows nothing of `agent::runner`/`agent::orchestrator` — the same
//! direction every domain module keeps with the mechanics layer that drives it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;

/// One line of a session's transcript.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLine {
    /// UUID v7, as text.
    pub id: String,
    pub session_id: String,
    /// 0-based arrival order — the ordering key; several lines can land within the same
    /// millisecond, which `created_at` alone cannot break the tie on.
    pub seq: i64,
    /// The CLI's raw stdout line, verbatim.
    pub line: String,
    pub created_at: DateTime<Utc>,
}

/// Appends one line to a session's transcript.
///
/// # Errors
///
/// Propagates a database error, including a `UNIQUE(session_id, seq)` violation if `seq` is
/// reused — callers append in order from a single writer (the orchestrator's drain task), so
/// this should not happen in practice, but is not silently ignored if it does.
pub async fn append(
    tx: &mut SqliteConnection,
    session_id: &str,
    seq: i64,
    line: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO agent_session_transcript (id, session_id, seq, line, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(session_id)
    .bind(seq)
    .bind(line)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// A session's full transcript, in arrival order.
pub async fn list_for_session(db: &Db, session_id: &str) -> AppResult<Vec<TranscriptLine>> {
    Ok(sqlx::query_as::<_, TranscriptLine>(
        "SELECT id, session_id, seq, line, created_at \
           FROM agent_session_transcript WHERE session_id = ? ORDER BY seq",
    )
    .bind(session_id)
    .fetch_all(db.reader())
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::agent_session::{self, NewAgentSession};
    use crate::domain::card::{self, NewCard, Placement};
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    async fn fixture() -> (Db, TempDb, String) {
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
        let card = card::create(
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
            &creator.id,
            now(),
        )
        .await
        .unwrap();
        let session = agent_session::insert(
            &mut tx,
            &NewAgentSession {
                card_id: &card.id,
                claude_session_id: "s-1",
                prompt: "do it",
                started_by: Some(&creator.id),
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (db, temp, session.id)
    }

    #[tokio::test]
    async fn lines_are_listed_back_in_arrival_order() {
        let (db, _temp, session_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        append(&mut tx, &session_id, 0, r#"{"type":"system"}"#, now())
            .await
            .unwrap();
        append(&mut tx, &session_id, 1, r#"{"type":"assistant"}"#, now())
            .await
            .unwrap();
        append(&mut tx, &session_id, 2, r#"{"type":"result"}"#, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let lines = list_for_session(&db, &session_id).await.unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].line, r#"{"type":"system"}"#);
        assert_eq!(lines[1].line, r#"{"type":"assistant"}"#);
        assert_eq!(lines[2].line, r#"{"type":"result"}"#);
        assert_eq!(
            lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[tokio::test]
    async fn a_session_with_no_transcript_yet_is_an_empty_list_not_an_error() {
        let (db, _temp, session_id) = fixture().await;
        let lines = list_for_session(&db, &session_id).await.unwrap();
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn deleting_the_card_cascades_through_the_session_to_its_transcript() {
        let (db, _temp, session_id) = fixture().await;

        let mut tx = db.begin_write().await.unwrap();
        append(&mut tx, &session_id, 0, "{}", now()).await.unwrap();
        tx.commit().await.unwrap();

        let card_id: String = sqlx::query_scalar("SELECT card_id FROM agent_sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("DELETE FROM cards WHERE id = ?")
            .bind(&card_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let lines = list_for_session(&db, &session_id).await.unwrap();
        assert!(
            lines.is_empty(),
            "the transcript must cascade away with its session"
        );
    }
}

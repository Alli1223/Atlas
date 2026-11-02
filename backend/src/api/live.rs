//! `GET /ws` — the live channel: board sync, the bell, and agent run output, pushed.
//!
//! # Who hears what
//!
//! Every frame on the [`crate::activity::Hub`] says which project it concerns and/or which
//! user it is for. A socket forwards a frame only when the caller can currently see that
//! project, and only frames addressed to the caller. The set of visible projects is
//! recomputed every [`REFRESH`], so revoking someone's access stops their stream within that
//! window — and the session itself is re-checked on the same beat, so a signed-out or revoked
//! session's socket is closed rather than streaming on for as long as the tab stays open.
//!
//! # Cross-site WebSocket hijacking
//!
//! A WebSocket handshake is a `GET`, so the CSRF layer (which only guards unsafe verbs) does
//! not see it — yet the browser attaches cookies to it. The handler therefore checks `Origin`
//! itself against the same allowlist the CSRF layer uses, and refuses a foreign one.

use std::collections::HashSet;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::{CurrentUser, User, to_sql_timestamp};
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// How often a socket re-reads the caller's access and session.
pub const REFRESH: Duration = Duration::from_secs(30);

/// Opens the live channel.
///
/// Frames are JSON objects with a `type`: `card.created`, `card.updated`, `comment.added`
/// (carrying `projectKey`/`cardKey`, for cache invalidation), `notification` (for the bell),
/// `agent.event` (one line of a running agent's stream), and `resync` (the client fell behind
/// and should refetch).
#[utoipa::path(
    get,
    path = "/ws",
    tag = "live",
    responses(
        (status = 101, description = "Switching to the WebSocket protocol"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "A foreign Origin", body = Problem),
    )
)]
async fn live(
    State(state): State<AppState>,
    current: CurrentUser,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> AppResult<Response> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !crate::auth::middleware::origin_allowed(origin, host.as_deref(), &state.config)
    {
        tracing::warn!(%origin, "refused a WebSocket from an unrecognised origin");
        return Err(AppError::Forbidden);
    }
    let user = (*current.user).clone();
    let session_id = current.session.id.clone();
    Ok(upgrade.on_upgrade(move |socket| run(state, user, session_id, socket)))
}

async fn visible_projects(state: &AppState, user: &User) -> AppResult<HashSet<String>> {
    Ok(project::list_for(&state.db, user, true)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect())
}

async fn session_alive(state: &AppState, session_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions WHERE id = ? AND expires_at > ?")
        .bind(session_id)
        .bind(to_sql_timestamp(crate::auth::now()))
        .fetch_one(state.db.reader())
        .await
        .is_ok_and(|n| n > 0)
}

async fn run(state: AppState, user: User, session_id: String, mut socket: WebSocket) {
    let mut feed = state.hub.subscribe();
    let mut projects = visible_projects(&state, &user).await.unwrap_or_default();
    let mut refresh = tokio::time::interval(REFRESH);
    refresh.tick().await;

    let hello = json!({ "type": "hello", "userId": user.id });
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            received = feed.recv() => {
                let frame = match received {
                    Ok(message) => {
                        let for_user = message.user_id.as_deref().is_none_or(|u| u == user.id);
                        let for_project = message
                            .project_id
                            .as_ref()
                            .is_none_or(|p| projects.contains(p));
                        if !(for_user && for_project) {
                            continue;
                        }
                        message.payload.to_string()
                    }
                    Err(RecvError::Lagged(_)) => json!({ "type": "resync" }).to_string(),
                    Err(RecvError::Closed) => break,
                };
                if socket.send(Message::Text(frame.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    // Pings are answered by axum; text from the client is ignored — this
                    // channel is server → client only.
                    Some(Ok(_)) => {}
                }
            }
            _ = refresh.tick() => {
                if !session_alive(&state, &session_id).await {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                if let Ok(fresh) = visible_projects(&state, &user).await {
                    projects = fresh;
                }
            }
        }
    }
}

/// The live route.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(live))
}

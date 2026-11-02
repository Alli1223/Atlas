//! Atlas's background jobs, assembled for [`crate::scheduler`].
//!
//! Each job is a thin wrapper that logs and swallows its own errors — a failed tick is retried
//! on the next interval, and must never end the job (the scheduler already catches panics).
//! The work itself lives in the module that owns it; this file only says *when*.

use std::sync::Arc;
use std::time::Duration;

use crate::activity::{self, HubMessage};
use crate::api::AppState;
use crate::error::AppResult;
use crate::notify;
use crate::scheduler::Job;

/// How often the activity feed is drained. Live sync, the bell and automation all ride on it,
/// so this is the latency floor of all three.
pub const PUMP_INTERVAL: Duration = Duration::from_secs(1);

/// Drains the activity feed once: every new event is pushed to live sync, turned into
/// notifications, and offered to the automation engine. Returns how many events there were.
///
/// Public so tests (and the agent run's completion path) can drain synchronously instead of
/// waiting for the next tick.
pub async fn pump_once(state: &AppState) -> AppResult<usize> {
    let events = activity::read_new(&state.db).await?;
    for event in &events {
        state.hub.publish(HubMessage {
            project_id: Some(event.card.project_id.clone()),
            user_id: None,
            payload: event.to_live_json(),
        });
        if let Err(err) = Box::pin(notify::on_event(&state.db, &state.hub, event)).await {
            tracing::warn!(error = %err, card = %event.card.key, "notification fan-out failed");
        }
        if let Err(err) = Box::pin(crate::automation::on_event(state, event)).await {
            tracing::warn!(error = %err, card = %event.card.key, "automation failed on an event");
        }
    }
    Ok(events.len())
}

/// Drains until there is nothing left — for tests, and after a burst.
pub async fn pump_until_idle(state: &AppState) -> AppResult<usize> {
    let mut total = 0;
    for _ in 0..50 {
        let n = pump_once(state).await?;
        total += n;
        if n == 0 {
            break;
        }
    }
    Ok(total)
}

fn job<F, Fut>(name: &'static str, interval: Duration, state: &AppState, run: F) -> Job
where
    F: Fn(AppState) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let state = state.clone();
    let run = Arc::new(run);
    Job {
        name,
        interval,
        run: Arc::new(move || {
            let state = state.clone();
            let run = Arc::clone(&run);
            Box::pin(async move { run(state).await })
        }),
    }
}

/// Every job this module owns.
pub fn all(state: &AppState) -> Vec<Job> {
    vec![
        job("activity-pump", PUMP_INTERVAL, state, |state| async move {
            if let Err(err) = pump_once(&state).await {
                tracing::warn!(error = %err, "activity pump tick failed");
            }
        }),
        job(
            "notification-mailer",
            Duration::from_secs(60),
            state,
            |state| async move {
                let public = state.config.public_url.clone();
                match notify::mail::flush(&state.db, state.vault.as_deref(), public.as_deref())
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(emails = n, "sent notification emails"),
                    Err(err) => tracing::warn!(error = %err, "notification mailer failed"),
                }
            },
        ),
        job(
            "daily-digest",
            Duration::from_hours(1),
            state,
            |state| async move {
                let public = state.config.public_url.clone();
                if let Err(err) = notify::mail::digest(
                    &state.db,
                    state.vault.as_deref(),
                    public.as_deref(),
                    crate::auth::now(),
                )
                .await
                {
                    tracing::warn!(error = %err, "daily digest failed");
                }
            },
        ),
        job(
            "automation-scheduler",
            Duration::from_secs(60),
            state,
            |state| async move {
                if let Err(err) =
                    crate::automation::engine::run_due(&state, crate::auth::now()).await
                {
                    tracing::warn!(error = %err, "scheduled automation failed");
                }
            },
        ),
        job(
            "daily-snapshot",
            Duration::from_hours(1),
            state,
            |state| async move {
                // Hourly but idempotent within a day: today's rows are rewritten in place, so
                // the last run of the day is the day's snapshot, and a restart loses nothing.
                if let Err(err) = crate::domain::snapshot::take(&state.db, crate::auth::now()).await
                {
                    tracing::warn!(error = %err, "daily snapshot failed");
                }
            },
        ),
        job(
            "agent-dispatcher",
            Duration::from_secs(5),
            state,
            |state| async move {
                if let Err(err) = crate::agent::dispatch::dispatch_queued(&state).await {
                    tracing::warn!(error = %err, "agent queue dispatch failed");
                }
            },
        ),
        job(
            "session-purge",
            Duration::from_hours(6),
            state,
            |state| async move {
                match crate::auth::session::purge_expired(&state.db, crate::auth::now()).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(sessions = n, "purged expired sessions"),
                    Err(err) => tracing::warn!(error = %err, "session purge failed"),
                }
            },
        ),
    ]
}

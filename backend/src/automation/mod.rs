//! Automation (`TODO.md` Phase 15): where a solo user recovers the labour of the team they do
//! not have. **One trigger, N conditions, N actions.**
//!
//! - [`model`] — what a rule is, as JSON.
//! - [`smart`] — `{{smart.values}}`, Jira's syntax.
//! - [`engine`] — matching, evaluating, acting, and the guardrails (loop detection, an
//!   execution cap, the owner's authority, the audit trace).
//! - [`store`] — the rules and their audit log.
//! - [`web`] — the outbound web-request action and its SSRF guard.
//! - [`seed`] — the rules each project template starts with.
//!
//! Rules are offered every activity event by [`crate::jobs::pump_once`] (so they see changes
//! from every writer, the MCP subprocess included); scheduled rules run from
//! [`crate::jobs`]; manual and webhook rules run from the API.

pub mod engine;
pub mod model;
pub mod seed;
pub mod smart;
pub mod store;
pub mod web;

use crate::activity::Event;
use crate::api::AppState;
use crate::error::AppResult;

/// Offers one activity event to every enabled rule in its project.
pub async fn on_event(state: &AppState, event: &Event) -> AppResult<()> {
    engine::on_event(state, event).await
}

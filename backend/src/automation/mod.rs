//! The automation engine (`TODO.md` Phase 15). Filled in by the next commit; the activity
//! pump already calls [`on_event`] for every event.

use crate::activity::Event;
use crate::api::AppState;
use crate::error::AppResult;

/// Offers one activity event to every enabled rule.
#[allow(clippy::unused_async)] // the real engine awaits; the signature is the contract
pub async fn on_event(_state: &AppState, _event: &Event) -> AppResult<()> {
    Ok(())
}

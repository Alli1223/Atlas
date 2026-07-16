//! Atlas — a self-hosted Jira-equivalent.
//!
//! This library holds everything the `atlas` binary is made of, so that
//! integration tests in `backend/tests/` can build the real router over a real
//! database rather than a reimplementation of it.
//!
//! ## Layout
//!
//! - [`config`] — typed, fail-fast configuration.
//! - [`db`] — SQLite pools (writer-of-one + N readers) and migrations.
//! - [`error`] — the error taxonomy and its RFC 7807 rendering.
//! - [`rank`] — lexicographic card ordering for drag-and-drop.
//! - [`telemetry`] — tracing setup and the HTTP request span.
//! - [`api`] — router, middleware, OpenAPI.

pub mod api;
pub mod config;
pub mod db;
pub mod error;
pub mod rank;
pub mod telemetry;
pub mod test_support;

pub use config::Config;
pub use db::Db;
pub use error::{AppError, AppResult};

/// The running Atlas version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

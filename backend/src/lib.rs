//! Atlas — a self-hosted Jira-equivalent.
//!
//! This library holds everything the `atlas` binary is made of, so that
//! integration tests in `backend/tests/` can build the real router over a real
//! database rather than a reimplementation of it.
//!
//! ## Layout
//!
//! - [`config`] — typed, fail-fast configuration.
//! - [`aql`] — the Atlas Query Language: lexer, parser, and a compiler to
//!   parameterised SQL that boards, filters, dashboards and automation all reuse.
//! - [`db`] — SQLite pools (writer-of-one + N readers) and migrations.
//! - [`error`] — the error taxonomy and its RFC 7807 rendering.
//! - [`rank`] — lexicographic card ordering for drag-and-drop.
//! - [`auth`] — users, passwords, sessions, roles, and the forced-reset gate.
//! - [`domain`] — projects, the configurable hierarchy, cards, and history.
//! - [`secrets`] — the encrypted secrets vault: API keys and PATs, sealed at rest
//!   and never returned over the wire.
//! - [`telemetry`] — tracing setup and the HTTP request span.
//! - [`api`] — router, middleware, OpenAPI.
//! - [`agent`] — running Claude Code against a card (`TODO.md` Phase 13).

pub mod agent;
pub mod api;
pub mod aql;
pub mod auth;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod integrations;
pub mod rank;
pub mod scheduler;
pub mod secrets;
pub mod telemetry;
pub mod test_support;

pub use auth::{CurrentUser, Role, User};
pub use config::Config;
pub use db::Db;
pub use error::{AppError, AppResult};

/// The running Atlas version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
//! - [`activity`] — the activity feed: every card change, from every writer, as events.
//! - [`notify`] — the bell, preferences, email and the daily digest.
//! - [`automation`] — rules: one trigger, conditions, actions (`TODO.md` Phase 15).
//! - [`jobs`] — the background jobs [`scheduler`] runs.
//! - [`backup`] — `VACUUM INTO` + an uploads archive, scheduled and on demand.
//! - [`reports`] — every Phase 16 report, as a query over the daily snapshot.
//! - [`storage`] — uploaded bytes on disk (attachments, avatars, generated images), at paths
//!   built only from server-generated ids.

pub mod activity;
pub mod agent;
pub mod api;
pub mod aql;
pub mod auth;
pub mod automation;
pub mod backup;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod integrations;
pub mod jobs;
pub mod notify;
pub mod rank;
pub mod reports;
pub mod scheduler;
pub mod secrets;
pub mod storage;
pub mod telemetry;
pub mod test_support;

pub use auth::{CurrentUser, Role, User};
pub use config::Config;
pub use db::Db;
pub use error::{AppError, AppResult};

/// The running Atlas version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

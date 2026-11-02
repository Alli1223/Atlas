//! Running Claude Code as an agent against a card (`TODO.md` Phase 13 — "the point of the
//! product": a card *is* the unit of work an agent picks up).
//!
//! # Layout
//!
//! - [`claude_code`] — parsing and interpreting the CLI's `stream-json` NDJSON event
//!   stream. Pure: no process spawning, no I/O, tested without a real subprocess — the
//!   same split [`crate::integrations::github::client`] draws between interpretation and
//!   the transport that calls it.
//! - [`runner`] — [`runner::AgentRunner`], the trait behind spawning Claude Code, and
//!   [`runner::LocalRunner`], the implementation that actually does it as a child process.
//! - [`workspace`] — [`workspace::WorkspacePreparer`], the trait behind turning a project's
//!   linked GitHub repo into the stable, clean, on-disk checkout a [`runner::RunRequest`]'s
//!   working directory needs, and [`workspace::GitWorkspacePreparer`], the implementation
//!   that actually clones/fetches it.
//! - [`orchestrator`] — [`orchestrator::start`], the thin glue wiring the three above plus
//!   [`crate::domain::agent_session`] into one "run Claude Code against this card" operation.
//!   `crate::api::agent_sessions` is the HTTP handler over it.
//! - [`completion`] — [`completion::run`], called from `orchestrator`'s detached drain task
//!   once a session finishes: posts a transcript-summary comment, commits/pushes/PRs whatever
//!   the run left uncommitted, and moves the card into the project's configured destination.
//!
//! See `docs/research/claude-code-cli.md` for the exact CLI behaviour `claude_code` encodes,
//! most importantly its two documented traps: `subtype: "success"` staying put on API/auth
//! failures (`is_error: true` is the real signal), and `result` being an ABSENT key, not
//! `null`, on every error subtype.

use std::future::Future;
use std::pin::Pin;

pub mod claude_code;
pub mod completion;
pub mod dispatch;
pub mod mcp;
pub mod orchestrator;
pub mod providers;
pub mod runner;
pub mod workspace;

/// A boxed, `Send` future — what makes [`runner::AgentRunner`] and
/// [`workspace::WorkspacePreparer`] object-safe (returning this instead of using `async fn`
/// in the trait), so both can be held as `Arc<dyn Trait>` in [`crate::api::AppState`], chosen
/// once at startup. The same shape [`crate::secrets::vault::Validator`] uses.
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub mod board_auto;

pub mod usage;

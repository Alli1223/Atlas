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
//! - [`workspace`] — [`workspace::prepare`], turning a project's linked GitHub repo into the
//!   stable, clean, on-disk checkout a [`runner::RunRequest`]'s working directory needs.
//!
//! The live session UI over WebSocket, Atlas's own MCP server, and card→task binding are
//! later work in this phase — see `docs/research/claude-code-cli.md` for the exact CLI
//! behaviour this module encodes, most importantly its two documented traps: `subtype:
//! "success"` staying put on API/auth failures (`is_error: true` is the real signal), and
//! `result` being an ABSENT key, not `null`, on every error subtype.

pub mod claude_code;
pub mod runner;
pub mod workspace;

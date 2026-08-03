//! Running Claude Code as an agent against a card (`TODO.md` Phase 13 — "the point of the
//! product": a card *is* the unit of work an agent picks up).
//!
//! # Layout
//!
//! - [`claude_code`] — parsing and interpreting the CLI's `stream-json` NDJSON event
//!   stream. Pure: no process spawning, no I/O, tested without a real subprocess — the
//!   same split [`crate::integrations::github::client`] draws between interpretation and
//!   the transport that calls it.
//!
//! Everything that actually spawns `claude -p …`, manages workspaces, and drives a live
//! session over WebSocket is later work in this phase; this module starts with the part
//! every later piece depends on getting right — see `docs/research/claude-code-cli.md` for
//! the exact CLI behaviour this encodes, most importantly its two documented traps:
//! `subtype: "success"` staying put on API/auth failures (`is_error: true` is the real
//! signal), and `result` being an ABSENT key, not `null`, on every error subtype.

pub mod claude_code;

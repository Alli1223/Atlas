//! Atlas's own MCP server: the tools a Claude Code session uses to act on its card.
//!
//! This is what closes the loop the product is built around — an agent given a card can, when
//! it is done, move that card and report on it, without a human relaying the outcome. The
//! design is security-first, because a run executes arbitrary code:
//!
//! - [`capability`] mints a per-session token that is the whole authorisation boundary. It
//!   confines a run to one card, with its starter's authority, and dies when the session ends.
//! - [`tools`] are each re-scoped and re-authorised on every call; moves go through the
//!   workflow engine, never a raw status write.
//! - [`server`] is the `atlas mcp` stdio subcommand the CLI drives, spawned via `--mcp-config`.

pub mod capability;
pub mod server;
pub mod tools;

use serde_json::{Value, json};

/// The tool names, namespaced as the CLI sees them (`mcp__<server>__<tool>`), for
/// `--allowedTools`. A run may call an Atlas tool only if it is on this list.
pub const ALLOWED_TOOLS: &[&str] = &[
    "mcp__atlas__atlas_get_card",
    "mcp__atlas__atlas_list_transitions",
    "mcp__atlas__atlas_move_card",
    "mcp__atlas__atlas_comment_card",
];

/// Builds the inline `--mcp-config` JSON that points a session at this binary's `mcp`
/// subcommand, carrying the capability token (and the database URL, so the child does not
/// depend on inheriting it).
///
/// `atlas_exe` is the path to the running binary (`std::env::current_exe()`), so a session's
/// MCP server is always the exact same build as the server that spawned it.
#[must_use]
pub fn mcp_config_json(atlas_exe: &str, token: &str, database_url: &str) -> Value {
    json!({
        "mcpServers": {
            "atlas": {
                "type": "stdio",
                "command": atlas_exe,
                "args": ["mcp"],
                "env": {
                    server::TOKEN_ENV: token,
                    "ATLAS_DATABASE_URL": database_url,
                },
            }
        }
    })
}

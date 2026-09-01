//! The `atlas mcp` stdio server: newline-delimited JSON-RPC 2.0 over stdin/stdout, which is
//! the transport the Claude Code CLI drives an MCP server through (verified end to end — see
//! `docs/research/claude-code-cli.md`).
//!
//! # It is spawned by the CLI, not by Atlas directly
//!
//! `agent::runner` passes `--mcp-config` naming this very binary with the `mcp` subcommand and
//! a per-session capability token in the child's environment. The CLI launches it, speaks
//! JSON-RPC to it, and the tools act back on Atlas's database.
//!
//! # stdout is sacred
//!
//! Every byte of stdout is part of the protocol. Telemetry is **not** initialised in this
//! mode and all diagnostics go to stderr, or a stray log line would corrupt the stream and
//! break the session.
//!
//! # The token gates every call
//!
//! The capability token is read once from the environment, but resolved against the database
//! on every `tools/call`, so the moment its session finishes the tools stop working even
//! though this process is still alive.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::db::Db;
use crate::error::AppError;

use super::capability;
use super::tools::{self, ToolContext};

/// The environment variable carrying the session's capability token into this process.
pub const TOKEN_ENV: &str = "ATLAS_MCP_TOKEN";

/// The MCP protocol version this server implements. The client sends its own in `initialize`;
/// this is echoed when they match and used as the floor otherwise.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Runs the stdio MCP server to completion: opens the database, then serves JSON-RPC until
/// stdin closes (the CLI has finished with the session).
///
/// # Errors
///
/// Only a failure to open the database. A malformed request line is answered with a JSON-RPC
/// error, not a process exit — one bad line must not end a live session.
pub async fn serve_stdio(config: Config) -> anyhow::Result<()> {
    let db = Db::connect(&config).await?;
    // No migrations here: the main server owns the schema. This process only reads and writes
    // rows an already-migrated database has.
    let token = std::env::var(TOKEN_ENV).ok();

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_line(&db, token.as_deref(), &line).await else {
            // A notification — no response is written.
            continue;
        };
        let mut bytes = serde_json::to_vec(&response)?;
        bytes.push(b'\n');
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Parses one line and produces its response, or `None` for a notification.
async fn handle_line(db: &Db, token: Option<&str>, line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            return Some(error_response(
                &Value::Null,
                -32700,
                &format!("parse error: {err}"),
            ));
        }
    };

    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // No id → a notification (e.g. `initialized`): nothing is written back, so `?` short-
    // circuits to `None` here.
    let id = id?;

    match method {
        "initialize" => Some(ok_response(&id, &initialize_result(&params))),
        "ping" => Some(ok_response(&id, &json!({}))),
        "tools/list" => Some(ok_response(&id, &json!({ "tools": tools::catalogue() }))),
        "tools/call" => Some(tools_call(db, token, &id, &params).await),
        other => Some(error_response(
            &id,
            -32601,
            &format!("method not found: {other}"),
        )),
    }
}

fn initialize_result(params: &Value) -> Value {
    // Echo the client's protocol version when they sent one — the recommended handshake —
    // and fall back to ours otherwise.
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "atlas", "version": crate::VERSION },
    })
}

async fn tools_call(db: &Db, token: Option<&str>, id: &Value, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    // Resolve the capability *now*, on every call — so a token whose session has ended is
    // already dead here, not merely revoked later.
    let Some(token) = token else {
        return tool_error(id, "no capability token: this MCP server is not authorised");
    };
    let session = match capability::resolve(db, token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return tool_error(
                id,
                "the capability is not valid (the session has ended or the token is unknown)",
            );
        }
        Err(err) => return internal_tool_error(id, &err),
    };

    let ctx = match ToolContext::build(db, session).await {
        Ok(ctx) => ctx,
        Err(err) => return app_error_to_tool_error(id, err),
    };

    match tools::call(&ctx, name, &arguments).await {
        Ok(content) => ok_response(id, &content),
        Err(err) => app_error_to_tool_error(id, err),
    }
}

/// Maps an [`AppError`] to a `tools/call` error result. Authorisation and argument failures
/// are surfaced to the model as a readable tool error (so it can adapt); an internal error is
/// logged and reported opaquely.
fn app_error_to_tool_error(id: &Value, err: AppError) -> Value {
    match err {
        AppError::Internal(err) => internal_tool_error(id, &err),
        AppError::Forbidden => tool_error(id, "not permitted: the run's user cannot do this"),
        AppError::NotFound => tool_error(id, "not found"),
        AppError::BadRequest(msg) | AppError::Validation(msg) | AppError::Conflict(msg) => {
            tool_error(id, &msg)
        }
        AppError::Unauthorized => tool_error(id, "unauthorised"),
    }
}

fn internal_tool_error(id: &Value, err: &(impl std::fmt::Display + ?Sized)) -> Value {
    // The detail goes to stderr, never to the model or the stream.
    tracing::error!(target: "agent::mcp", error = %err, "MCP tool call failed internally");
    tool_error(id, "an internal error occurred")
}

/// An MCP tool error: a normal JSON-RPC *result* with `isError: true`, which is how the
/// protocol surfaces a tool failure the model should see (as opposed to a protocol error).
fn tool_error(id: &Value, message: &str) -> Value {
    ok_response(
        id,
        &json!({ "content": [ { "type": "text", "text": message } ], "isError": true }),
    )
}

fn ok_response(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Loads configuration from the environment and serves the stdio MCP server. Kept here so
/// `main`'s subcommand dispatch stays a one-liner.
///
/// # Errors
///
/// A failure to load configuration or open the database.
pub async fn run_from_env() -> anyhow::Result<()> {
    let config = Config::load()?;
    serve_stdio(config).await
}

//! Per-session capability tokens: the security boundary around Atlas's MCP tools.
//!
//! # Why a separate token and not the starting user's session
//!
//! A Claude Code run is semi-trusted — it executes arbitrary code in a workspace. If it
//! reached Atlas with the starting user's cookie it could do anything that user can. Instead
//! each run gets a capability token that authorises exactly one thing: acting on the single
//! card its session ran against, with the *authority* of the user who started it (so it can
//! never exceed them) but none of their reach into other cards, projects, or settings.
//!
//! # The token and the row are not the same value
//!
//! Only the SHA-256 of the token is stored, exactly as [`crate::auth::session`] does it. The
//! plaintext is returned once, to the runner, and is unrecoverable afterwards. A read of
//! `agent_capabilities` yields nothing that authenticates.
//!
//! # It dies with the session
//!
//! [`resolve`] joins the session and refuses unless it is still `running`, so a token stops
//! working the instant the session finishes, fails, or is cancelled — *before* [`revoke`]
//! even runs. Revocation is the belt; the status check is the braces.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use subtle::ConstantTimeEq;

use crate::db::Db;
use crate::domain::agent_session::{self, AgentSession};
use crate::error::AppResult;

/// Capability token entropy, in bytes. 256 bits — the same strength as an auth session token,
/// and for the same reason: it is a bearer credential that gates real mutations.
const TOKEN_BYTES: usize = 32;

/// A freshly minted capability and the token that addresses it.
///
/// The token is returned exactly once, here. It is never recoverable from the stored row.
#[derive(Debug)]
pub struct MintedCapability {
    /// The bearer token the runner hands to the CLI. Shown once.
    pub token: String,
}

/// Generates a capability token: 256 bits of OS entropy, base64url, unpadded.
///
/// `OsRng` is argon2's re-export (`rand_core` 0.6), matching [`crate::auth::session`] — Atlas
/// does not depend on `rand` directly, and pulling it in for this alone would mean two
/// incompatible `OsRng`s in one tree.
fn new_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};

    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The database key for a token: its SHA-256, hex-encoded.
fn digest(token: &str) -> String {
    let hash = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Mints a capability token for a session and records its hash.
///
/// Runs in the same write transaction that inserts the session, so a session either has its
/// capability or does not exist — the runner can never spawn a CLI pointed at a token that was
/// never stored.
///
/// # Errors
///
/// A database error writing the row.
pub async fn mint(
    tx: &mut SqliteConnection,
    session_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> AppResult<MintedCapability> {
    let token = new_token();
    let token_hash = digest(&token);

    sqlx::query(
        "INSERT INTO agent_capabilities (token_hash, session_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(&token_hash)
    .bind(session_id)
    .bind(now.to_rfc3339())
    .execute(&mut *tx)
    .await?;

    Ok(MintedCapability { token })
}

/// Resolves a bearer token to the session it acts for, **only while that session is running**.
///
/// Returns `None` for an unknown token, and — crucially — for a token whose session has
/// already reached a terminal status. The status check means a token is dead the moment the
/// run ends, without waiting for [`revoke`] to have executed on the drain task.
///
/// The lookup is by the token's digest, so the raw token is compared in constant time against
/// nothing recoverable; the extra `ct_eq` guards against a theoretical hash-prefix match and
/// keeps the comparison timing-flat regardless.
///
/// # Errors
///
/// A database error reading the row or the session.
pub async fn resolve(db: &Db, token: &str) -> AppResult<Option<AgentSession>> {
    let token_hash = digest(token);

    let stored: Option<(String, String)> = sqlx::query_as(
        "SELECT token_hash, session_id FROM agent_capabilities WHERE token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(db.reader())
    .await?;

    let Some((stored_hash, session_id)) = stored else {
        return Ok(None);
    };

    // The row was found by digest already; this is belt-and-braces and keeps the path
    // timing-flat.
    if stored_hash
        .as_bytes()
        .ct_eq(token_hash.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Ok(None);
    }

    let session = agent_session::find_by_id(db, &session_id).await?;
    match session {
        Some(session) if session.status.is_running() => Ok(Some(session)),
        // Known token, but the session is over: the capability is dead. Treated exactly like
        // an unknown token — the caller learns nothing about a session it can no longer act on.
        _ => Ok(None),
    }
}

/// Revokes a session's capability, if it has one. Idempotent.
///
/// Called from the drain task when a run reaches a terminal status. [`resolve`] already
/// refuses a non-running session, so this is the durable cleanup rather than the live guard —
/// it stops a finished session's row lingering, and closes the (harmless) window where the row
/// exists but the session is terminal.
///
/// # Errors
///
/// A database error deleting the row.
pub async fn revoke(tx: &mut SqliteConnection, session_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM agent_capabilities WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

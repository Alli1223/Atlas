-- Capability tokens for a running agent session's MCP connection.
--
-- A Claude Code session is semi-trusted: it runs arbitrary code in a workspace, so the token
-- it uses to reach Atlas's own MCP tools is the security boundary. It confines the agent to
-- exactly what the starting user could do to the ONE session card, and it stops working the
-- instant the session leaves `running`.
--
-- Only the SHA-256 of the token is stored, never the token itself — the same discipline the
-- auth `sessions` table uses. A read of this table yields nothing that authenticates.
CREATE TABLE agent_capabilities (
    -- SHA-256 hex digest of the capability token. The token is shown once, to the runner, and
    -- is never recoverable from this row.
    token_hash  TEXT    NOT NULL PRIMARY KEY,

    -- The session this token acts for. ON DELETE CASCADE: when the session row goes, so does
    -- its capability. One live token per session (a session is spawned once).
    session_id  TEXT    NOT NULL UNIQUE
                    REFERENCES agent_sessions (id) ON DELETE CASCADE,

    created_at  TEXT    NOT NULL
) STRICT;

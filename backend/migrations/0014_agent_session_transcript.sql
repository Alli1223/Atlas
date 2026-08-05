-- Atlas schema, migration 0014: an agent session's full event transcript.
--
-- STRICT, matching 0001-0013.
--
-- ---------------------------------------------------------------------------
-- agent_session_transcript — every stream-json line a run produced
-- ---------------------------------------------------------------------------
--
-- `agent_sessions` (0013) records a run's status and terminal outcome only; this table is
-- the full stream-json transcript that sits behind it, one row per line the CLI wrote to
-- stdout, in arrival order. `line` is the CLI's own text, byte-for-byte — not a
-- re-serialization of the parsed `Event`, which could drift from it in formatting or field
-- order (see `agent::runner::RunEvent`'s own comment on why the raw line is kept even for a
-- line that parsed cleanly).
--
-- `seq` is the ordering key, not `created_at`: several lines can land within the same
-- millisecond on a fast run, and `created_at` alone cannot break that tie reliably.

CREATE TABLE agent_session_transcript (
    id          TEXT    NOT NULL PRIMARY KEY,

    -- The session this line belongs to. ON DELETE CASCADE with the session.
    session_id  TEXT    NOT NULL REFERENCES agent_sessions (id) ON DELETE CASCADE,

    -- 0-based arrival order within the session.
    seq         INTEGER NOT NULL,

    -- The CLI's raw stdout line, verbatim.
    line        TEXT    NOT NULL,

    created_at  TEXT    NOT NULL,

    UNIQUE (session_id, seq)
) STRICT;

CREATE INDEX agent_session_transcript_session_idx ON agent_session_transcript (session_id);

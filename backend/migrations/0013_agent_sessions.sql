-- Atlas schema, migration 0013: Claude Code agent sessions against a card.
--
-- STRICT, matching 0001-0012.
--
-- ---------------------------------------------------------------------------
-- agent_sessions — one run of Claude Code against a card
-- ---------------------------------------------------------------------------
--
-- Session metadata and its terminal outcome, not the live event stream — the full
-- transcript (every stream-json line) is a later increment ("persist transcripts"
-- in TODO.md's Phase 13), and does not change this table's shape when it lands,
-- only add a sibling table keyed on `agent_sessions.id`.
--
-- `claude_session_id` is the CLI's own session id (Atlas-generated, passed as
-- `--session-id` — see `agent::runner`), stored so a later run can `--resume` it.
-- `status` mirrors `agent::claude_code::Outcome` plus `running`, the one state
-- that outcome-interpretation never produces (it only classifies a *finished*
-- result event) and `cancelled`, which is Atlas's own doing, not the CLI's.

CREATE TABLE agent_sessions (
    id                  TEXT    NOT NULL PRIMARY KEY,

    -- The card this session ran against. ON DELETE CASCADE with the card.
    card_id             TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- The CLI's own session id, once known (present from the moment the run is
    -- spawned — Atlas generates it up front, see `agent::runner::spawn_local`).
    claude_session_id   TEXT,

    status              TEXT    NOT NULL DEFAULT 'running'
                            CHECK (status IN (
                                'running',
                                'completed',
                                'completed_with_denials',
                                'limit_reached',
                                'failed',
                                'cancelled'
                            )),

    -- The prompt sent — usually the card's summary + description, but not
    -- reconstructed from the card after the fact: cards get edited, and a
    -- session's record of what it was actually asked must not drift with them.
    prompt              TEXT    NOT NULL,

    -- The terminal `result` event's `result` field. NULL until finished, and
    -- may stay NULL even then (absent on every error subtype — see
    -- `agent::claude_code::ResultEvent`).
    result_text         TEXT,

    total_cost_usd      REAL,
    num_turns           INTEGER,

    -- Set on `failed`/`cancelled`; NULL otherwise.
    error_message       TEXT,

    -- Who started it. ON DELETE SET NULL: session history outlives an account
    -- cleanup, matching card_worklogs.author_id (0010).
    started_by          TEXT    REFERENCES users (id) ON DELETE SET NULL,

    started_at          TEXT    NOT NULL,
    ended_at            TEXT,

    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL
) STRICT;

CREATE INDEX agent_sessions_card_idx ON agent_sessions (card_id);

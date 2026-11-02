-- Atlas schema, migration 0021: per-project agent settings, the run queue, usage capture.
--
-- STRICT, matching 0001-0020.

-- How Claude Code runs in a project. One row per project; no row = the defaults, which are
-- the least permissive that work (TODO.md Phase 13: "default to the *least* permissive that
-- works, and make `bypassPermissions` a deliberate opt-in with a warning").
CREATE TABLE project_agent_settings (
    project_id            TEXT    NOT NULL PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    permission_mode       TEXT    NOT NULL DEFAULT 'dontAsk'
                              CHECK (permission_mode IN
                                  ('default', 'acceptEdits', 'plan', 'dontAsk', 'bypassPermissions')),
    -- JSON arrays in --allowedTools / --disallowedTools syntax. An empty allow list means
    -- Atlas's own default list, never the CLI's permissive one.
    allowed_tools         TEXT    NOT NULL DEFAULT '[]',
    disallowed_tools      TEXT    NOT NULL DEFAULT '[]',
    max_turns             INTEGER NOT NULL DEFAULT 50 CHECK (max_turns BETWEEN 1 AND 500),
    max_budget_usd        REAL    NOT NULL DEFAULT 5.0 CHECK (max_budget_usd > 0),
    -- Where a completed run moves its card. NULL = the project's "In Review"-like default
    -- (the first in-progress status after the card's own), resolved at run time.
    destination_status_id TEXT    REFERENCES statuses (id) ON DELETE SET NULL,
    -- Post the run's result as a comment on the card.
    post_summary          INTEGER NOT NULL DEFAULT 1 CHECK (post_summary IN (0, 1)),
    -- Billing. 0 = inherit the machine's Claude login (a Max subscription); 1 = pass the
    -- named Anthropic API key, which bills the API. Explicit, because setting
    -- ANTHROPIC_API_KEY silently overrides the subscription (docs/research/claude-code-cli.md).
    use_api_key           INTEGER NOT NULL DEFAULT 0 CHECK (use_api_key IN (0, 1)),
    api_credential_id     TEXT    REFERENCES api_credentials (id) ON DELETE SET NULL,
    updated_at            TEXT    NOT NULL
) STRICT;

-- The run queue: a session waiting for a free slot (ATLAS_AGENT_MAX_CONCURRENT) is `running`
-- with queued = 1, and carries the parameters to launch it with. Persisted, so a queue
-- survives a restart.
ALTER TABLE agent_sessions ADD COLUMN queued INTEGER NOT NULL DEFAULT 0 CHECK (queued IN (0, 1));
ALTER TABLE agent_sessions ADD COLUMN launch TEXT;

-- What a run cost, beyond the dollar figure: the `usage` and `modelUsage` objects from the
-- terminal result event, and its wall-clock duration.
ALTER TABLE agent_sessions ADD COLUMN usage TEXT;
ALTER TABLE agent_sessions ADD COLUMN duration_ms INTEGER;

-- The workspace branch the run left its work on, and the session this one re-ran.
ALTER TABLE agent_sessions ADD COLUMN branch TEXT;
ALTER TABLE agent_sessions ADD COLUMN rerun_of TEXT REFERENCES agent_sessions (id) ON DELETE SET NULL;

-- A card's cover image: a generated (or uploaded-and-generated-from) image chosen for it.
-- Its own table rather than a `cards` column: a cover is presentation, not a field with a
-- changelog, and `cards` is the one table every query in Atlas touches.
CREATE TABLE card_covers (
    card_id   TEXT NOT NULL PRIMARY KEY REFERENCES cards (id) ON DELETE CASCADE,
    image_id  TEXT NOT NULL REFERENCES generated_images (id) ON DELETE CASCADE,
    set_at    TEXT NOT NULL
) STRICT;

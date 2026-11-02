-- Atlas schema, migration 0020: reports and dashboards.
--
-- STRICT, matching 0001-0019.
--
-- ---------------------------------------------------------------------------
-- daily_snapshot — ONE table, and every flow report is a GROUP BY over it.
-- ---------------------------------------------------------------------------
--
-- TODO.md Phase 16: "Do not replay the changelog at read time; that is why Jira's reports are
-- slow." One row per (day, card) with the card's status, category, estimate and cycle as of
-- that day. Cumulative flow, average age, created-vs-resolved, and the control chart are then
-- plain aggregates over an index, no matter how long the history.
--
-- Written daily by `domain::snapshot::take`. The first run backfills the last 90 days from the
-- changelog once (write-time, not read-time), so an instance upgraded to this version has
-- charts on day one rather than a blank page for a month.
CREATE TABLE daily_snapshot (
    day             TEXT NOT NULL,      -- YYYY-MM-DD, UTC
    card_id         TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    project_id      TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    status_id       TEXT NOT NULL,
    status_category TEXT NOT NULL CHECK (status_category IN ('todo', 'in_progress', 'done')),
    estimate        REAL,
    cycle_id        TEXT,
    created_day     TEXT NOT NULL,
    resolved_day    TEXT,

    PRIMARY KEY (day, card_id)
) STRICT;

CREATE INDEX daily_snapshot_project_day_idx ON daily_snapshot (project_id, day);

-- When a cycle actually started and completed — instants, distinct from its planned dates.
-- The commitment snapshot is taken at `started_at` and the completion snapshot at
-- `completed_at` (both into `cycle_snapshot`), which is what the cycle report reads.
ALTER TABLE cycles ADD COLUMN started_at TEXT;
ALTER TABLE cycles ADD COLUMN completed_at TEXT;

-- Which moment a cycle snapshot captures. Daily rows are now stamped with the instant they
-- were taken (one per card per day, replaced in place), so they order correctly against the
-- commitment and completion snapshots taken the same day — a burndown reads the *latest*
-- state of each day, which a midnight stamp on a mid-afternoon snapshot got wrong.
ALTER TABLE cycle_snapshot ADD COLUMN kind TEXT NOT NULL DEFAULT 'daily'
    CHECK (kind IN ('daily', 'commitment', 'completion'));

-- ---------------------------------------------------------------------------
-- dashboards — a grid of gadgets. Gadgets are saved-filter renderers.
-- ---------------------------------------------------------------------------
CREATE TABLE dashboards (
    id          TEXT    NOT NULL PRIMARY KEY,
    owner_id    TEXT    NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    -- Shared dashboards are readable by every signed-in user; each gadget's data is still
    -- fetched with the *viewer's* access, so sharing a dashboard never shares a card.
    shared      INTEGER NOT NULL DEFAULT 0 CHECK (shared IN (0, 1)),
    -- JSON array of gadgets: { id, kind, title, config, x, y, w, h }.
    gadgets     TEXT    NOT NULL DEFAULT '[]',
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
) STRICT;

CREATE INDEX dashboards_owner_idx ON dashboards (owner_id);

-- ---------------------------------------------------------------------------
-- generations — every AI generation (Gemini, Phase 14), for the cost dashboard and the
-- "pick from grid" history. Defined here because the agent cost report reads it alongside
-- `agent_sessions`; Phase 14 writes it.
-- ---------------------------------------------------------------------------
CREATE TABLE generations (
    id            TEXT    NOT NULL PRIMARY KEY,
    provider      TEXT    NOT NULL DEFAULT 'gemini',
    model         TEXT    NOT NULL,
    project_id    TEXT    REFERENCES projects (id) ON DELETE CASCADE,
    card_id       TEXT    REFERENCES cards (id) ON DELETE SET NULL,
    -- project_cover, project_avatar, card_cover, reference, draft, query
    purpose       TEXT    NOT NULL,
    prompt        TEXT    NOT NULL,
    status        TEXT    NOT NULL CHECK (status IN ('succeeded', 'blocked', 'failed')),
    error         TEXT,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    cost_usd      REAL,
    created_by    TEXT    REFERENCES users (id) ON DELETE SET NULL,
    created_at    TEXT    NOT NULL
) STRICT;

CREATE INDEX generations_project_idx ON generations (project_id, created_at);

-- The images a generation returned, stored under uploads/generated/{id}.
CREATE TABLE generated_images (
    id            TEXT    NOT NULL PRIMARY KEY,
    generation_id TEXT    NOT NULL REFERENCES generations (id) ON DELETE CASCADE,
    mime_type     TEXT    NOT NULL,
    size_bytes    INTEGER NOT NULL,
    created_at    TEXT    NOT NULL
) STRICT;

CREATE INDEX generated_images_generation_idx ON generated_images (generation_id);

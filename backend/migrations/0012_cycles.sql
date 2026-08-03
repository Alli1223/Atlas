-- Atlas schema, migration 0012: cycles and their scope tracking.
--
-- Every table is STRICT, matching 0001-0011.
--
-- ---------------------------------------------------------------------------
-- cycles — a project's sprints/iterations
-- ---------------------------------------------------------------------------
--
-- Three states (future -> active -> closed), and closed is NOT terminal: a
-- closed cycle can be reopened back to active (docs/research/corrections.md
-- #7 — Jira's own "Reopen sprint" action). Starting requires both dates;
-- reopening replans end_date rather than reusing the original.
--
-- Only a project with cycles_enabled (see 0003) may have cycles, enforced in
-- Rust rather than a trigger, matching how the rest of the domain layer
-- validates.

CREATE TABLE cycles (
    id          TEXT    NOT NULL PRIMARY KEY,

    -- The owning project. ON DELETE CASCADE: deleting a project takes its
    -- cycles with it.
    project_id  TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,

    name        TEXT    NOT NULL,
    goal        TEXT,

    -- NULL until started; start requires both, together (never one alone).
    start_date  TEXT,
    end_date    TEXT,

    state       TEXT    NOT NULL DEFAULT 'future'
                    CHECK (state IN ('future', 'active', 'closed')),

    -- Display order within a project's cycle list.
    position    INTEGER NOT NULL DEFAULT 0,

    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
) STRICT;

CREATE INDEX cycles_project_idx ON cycles (project_id);

-- ---------------------------------------------------------------------------
-- card_cycle — which cycle(s) a card has belonged to, and when
-- ---------------------------------------------------------------------------
--
-- A many-to-many join with history, not a simple FK on cards: a card can
-- pass through several cycles over its life (carried over on complete), and
-- a closed cycle must keep the membership it had — commitment/scope-creep
-- reporting depends on knowing who was in a cycle at start versus who left
-- (docs/research/corrections.md #7's "sprint report" data). removed_at NULL
-- means still a member; in_scope_at_start distinguishes "was here when the
-- cycle started" from "added mid-cycle" (scope creep).

CREATE TABLE card_cycle (
    card_id            TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    cycle_id           TEXT    NOT NULL REFERENCES cycles (id) ON DELETE CASCADE,

    added_at           TEXT    NOT NULL,
    removed_at         TEXT,

    in_scope_at_start  INTEGER NOT NULL DEFAULT 0
                           CHECK (in_scope_at_start IN (0, 1)),

    PRIMARY KEY (card_id, cycle_id)
) STRICT;

CREATE INDEX card_cycle_cycle_idx ON card_cycle (cycle_id);

-- ---------------------------------------------------------------------------
-- cycle_snapshot — commitment/completion/daily estimate totals
-- ---------------------------------------------------------------------------
--
-- What "committed vs completed" and a burndown chart are computed from —
-- neither is derivable from current state after the fact (a card's estimate
-- or status today says nothing about what it was on day 3 of a cycle that
-- closed weeks ago). One row per (cycle, card) per time a snapshot was
-- taken: at minimum on start (commitment) and on complete (completion); a
-- daily cadence for a day-by-day burndown is Phase 10's later half, once
-- Atlas has a background scheduler to drive it — this table's shape does not
-- change for that, only how often rows land in it.

CREATE TABLE cycle_snapshot (
    id               TEXT    NOT NULL PRIMARY KEY,
    cycle_id         TEXT    NOT NULL REFERENCES cycles (id) ON DELETE CASCADE,
    taken_at         TEXT    NOT NULL,
    card_id          TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    estimate         REAL,
    status_category  TEXT    NOT NULL CHECK (status_category IN ('todo', 'in_progress', 'done')),

    UNIQUE (cycle_id, taken_at, card_id)
) STRICT;

CREATE INDEX cycle_snapshot_cycle_idx ON cycle_snapshot (cycle_id);

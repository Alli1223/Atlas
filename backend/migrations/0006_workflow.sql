-- Atlas schema, migration 0006: the workflow engine.
--
-- Every table is STRICT, matching 0001-0005.
--
-- ---------------------------------------------------------------------------
-- What a workflow is, and what it is not
-- ---------------------------------------------------------------------------
--
-- A workflow is a set of statuses plus the *transitions* between them, and each
-- transition carries three ordered gates borrowed verbatim from Jira, because
-- the distinction between them is the whole reason Jira's transition UI never
-- offers a button you cannot press:
--
--   CONDITIONS  fail -> the transition is HIDDEN. It is not offered, and an
--               attempt to take it directly is rejected as if the edge does not
--               exist. "Only the assignee may resolve this" is a condition.
--   VALIDATORS  fail -> the transition is OFFERED but the attempt is REJECTED
--               with a message. The status does not change and post-functions do
--               NOT run. "You must pick a resolution" is a validator.
--   POST-FNS    run AFTER the status change commits, in the same transaction:
--               set a field, add a comment, record an event. If one fails, the
--               whole transition rolls back — a half-applied transition is a
--               corrupt card.
--
-- NO SCHEMES. Jira routes workflows through a Workflow Scheme mapping issue-type
-- to workflow, assigned to a project, three indirections deep. Atlas gives each
-- card_type a single `workflow_id` FK (added at the bottom of this file). See
-- docs/adr/0003 — the same flattening every other config table already made.
--
-- ---------------------------------------------------------------------------
-- The default workflow, and why it is permissive
-- ---------------------------------------------------------------------------
--
-- Every project gets a workflow flagged `is_default`. A default workflow permits
-- moving a card between any two of its statuses: it is what the seeded templates
-- already imply, and it is what keeps every card that moves today moving after
-- this migration lands. Only a *custom* workflow — one a user builds with the
-- transition editor — enforces its edges strictly. The permissiveness lives in
-- the engine (`domain::workflow`), not in a wall of seeded edges, so a status
-- added to a project later is reachable under the default with no extra rows.

-- ---------------------------------------------------------------------------
-- workflows
-- ---------------------------------------------------------------------------
CREATE TABLE workflows (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,

    -- A default workflow is permissive (see the header). Exactly one per project
    -- is expected, but the schema does not force it: a project mid-edit may have
    -- none, and forcing it with a partial unique index would make the editor's
    -- job harder for no invariant anything relies on.
    is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),

    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

CREATE INDEX workflows_project_id_idx ON workflows (project_id);

-- ---------------------------------------------------------------------------
-- workflow_statuses — which statuses a workflow includes.
-- ---------------------------------------------------------------------------
CREATE TABLE workflow_statuses (
    workflow_id TEXT NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    status_id   TEXT NOT NULL REFERENCES statuses (id) ON DELETE CASCADE,

    PRIMARY KEY (workflow_id, status_id)
) STRICT;

CREATE INDEX workflow_statuses_status_id_idx ON workflow_statuses (status_id);

-- ---------------------------------------------------------------------------
-- transitions — the edges.
--
-- `from_status_id IS NULL` means "any status": a global transition, offered from
-- wherever the card currently sits. `to_status_id` is where it lands.
-- ---------------------------------------------------------------------------
CREATE TABLE transitions (
    id             TEXT    NOT NULL PRIMARY KEY,
    workflow_id    TEXT    NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    name           TEXT    NOT NULL,

    -- NULL = global ("from any status"). A real id anchors the edge to one
    -- source. ON DELETE CASCADE so a deleted status cannot leave a dangling edge.
    from_status_id TEXT    REFERENCES statuses (id) ON DELETE CASCADE,
    to_status_id   TEXT    NOT NULL REFERENCES statuses (id) ON DELETE CASCADE,

    -- Display and evaluation order. When two edges reach the same target, the
    -- lower position wins the automatic (drag/PATCH) path.
    position       INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX transitions_workflow_id_idx ON transitions (workflow_id);

-- ---------------------------------------------------------------------------
-- The three gates. Each is (kind, config-JSON) so the set of kinds can grow
-- without a migration — the kind is validated in `domain::workflow`, and the
-- config is parsed per kind. `config` defaults to an empty object so a kind that
-- needs none (OnlyAssignee, ChildBlocking) can be inserted with just its kind.
-- ---------------------------------------------------------------------------
CREATE TABLE transition_conditions (
    id            TEXT NOT NULL PRIMARY KEY,
    transition_id TEXT NOT NULL REFERENCES transitions (id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    config        TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE INDEX transition_conditions_transition_id_idx
    ON transition_conditions (transition_id);

CREATE TABLE transition_validators (
    id            TEXT NOT NULL PRIMARY KEY,
    transition_id TEXT NOT NULL REFERENCES transitions (id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    config        TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE INDEX transition_validators_transition_id_idx
    ON transition_validators (transition_id);

CREATE TABLE transition_post_functions (
    id            TEXT    NOT NULL PRIMARY KEY,
    transition_id TEXT    NOT NULL REFERENCES transitions (id) ON DELETE CASCADE,
    kind          TEXT    NOT NULL,
    config        TEXT    NOT NULL DEFAULT '{}',

    -- Post-functions run in a fixed order; this is it.
    position      INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX transition_post_functions_transition_id_idx
    ON transition_post_functions (transition_id);

-- ---------------------------------------------------------------------------
-- workflow_events — the sink for the FireEvent post-function.
--
-- Recorded now, consumed later: Phase 15's automation reads these to trigger
-- rules. Keeping them from the start means "what fired when" is answerable
-- retroactively, the same argument card_history makes for itself.
-- ---------------------------------------------------------------------------
CREATE TABLE workflow_events (
    id            TEXT NOT NULL PRIMARY KEY,
    card_id       TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- The transition that fired it. SET NULL so editing a workflow does not erase
    -- the record that an event happened.
    transition_id TEXT REFERENCES transitions (id) ON DELETE SET NULL,

    event         TEXT NOT NULL,

    -- Who took the transition. NULL for an automation or an agent, matching
    -- card_history.author_id.
    author_id     TEXT REFERENCES users (id) ON DELETE SET NULL,

    created_at    TEXT NOT NULL
) STRICT;

CREATE INDEX workflow_events_card_id_idx ON workflow_events (card_id);
CREATE INDEX workflow_events_event_idx ON workflow_events (event, created_at);

-- ---------------------------------------------------------------------------
-- card_types gains its per-type workflow FK.
--
-- ON DELETE SET NULL: deleting a workflow unassigns it rather than deleting the
-- card types that used it. A NULL workflow_id is treated exactly like a default
-- workflow — permissive — so a card type is never left unable to move.
--
-- SQLite permits ADD COLUMN with a REFERENCES clause as long as the column is
-- nullable with a NULL default, which this is.
-- ---------------------------------------------------------------------------
ALTER TABLE card_types
    ADD COLUMN workflow_id TEXT REFERENCES workflows (id) ON DELETE SET NULL;

-- ---------------------------------------------------------------------------
-- Backfill: give every existing project a default workflow, so nothing that
-- moves today stops moving.
--
-- The id is derived from the project id rather than randomly generated: SQLite
-- has no UUID function, and `project_id || '-default-workflow'` is unique per
-- project and collides with nothing the application mints (UUID v7). New
-- projects get an application-generated default via `domain::template::apply`.
--
-- The timestamp format is the one every Atlas TEXT timestamp column uses (see
-- `auth::to_sql_timestamp`): RFC 3339, microseconds, fixed +00:00 offset.
-- ---------------------------------------------------------------------------
INSERT INTO workflows (id, project_id, name, is_default, created_at, updated_at)
SELECT p.id || '-default-workflow',
       p.id,
       'Default',
       1,
       strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now'),
       strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now')
FROM projects p;

INSERT INTO workflow_statuses (workflow_id, status_id)
SELECT s.project_id || '-default-workflow', s.id
FROM statuses s;

UPDATE card_types SET workflow_id = project_id || '-default-workflow';

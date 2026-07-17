-- Atlas schema, migration 0003: the domain model.
--
-- Every table is STRICT, matching 0001 and 0002.
--
-- ---------------------------------------------------------------------------
-- The shape of this schema, and why it is not Jira's
-- ---------------------------------------------------------------------------
--
-- Two decisions dominate everything below; both are recorded in docs/adr/0002
-- and TODO.md's "Architecture decisions".
--
-- 1. HIERARCHY IS CONFIGURATION, NOT CODE. There is no `epics` table, no
--    `is_subtask` flag, and no level named in any column. `cards.parent_id`
--    points at `cards.id` uniformly at every depth, and `hierarchy_levels` names
--    the levels per project. The only structural rule is `parent.level >
--    child.level`. This is what makes "a card that opens its own board" fall out
--    of the model instead of being a feature: a board is a view over a parent's
--    children.
--
-- 2. CONFIG IS FLAT AND PER-PROJECT. Jira routes every one of these tables
--    through a three-level scheme indirection (Status -> Workflow -> Workflow
--    Scheme -> Project, and five more like it). That exists for the 500-project
--    instance Atlas does not have, and it is the single largest source of Jira's
--    "why can't I just change this" misery. Here a status belongs to a project.
--    Full stop.

-- ---------------------------------------------------------------------------
-- projects
-- ---------------------------------------------------------------------------
CREATE TABLE projects (
    id              TEXT    NOT NULL PRIMARY KEY,

    -- The card-key prefix: ATLAS in ATLAS-123. Uppercased by the application on
    -- the way in; COLLATE NOCASE on the column means the UNIQUE index agrees,
    -- so `atlas` and `ATLAS` cannot both exist even if some future caller
    -- forgets to normalise.
    key             TEXT    NOT NULL COLLATE NOCASE UNIQUE,

    name            TEXT    NOT NULL,
    description     TEXT,

    -- The project lead. NO ACTION (the default), not SET NULL or CASCADE: users
    -- are never hard-deleted (see 0002), so this can only dangle if someone goes
    -- around the application, and then failing loudly is right.
    lead_id         TEXT    REFERENCES users (id),

    avatar_url      TEXT,
    cover_image_url TEXT,

    -- Which template seeded this project: 'programming', '3d-modeling',
    -- 'job-search', 'blank'.
    --
    -- Free text, deliberately, and NOT a CHECK. This column is a historical fact
    -- about how the project was set up, not a live behaviour switch — nothing
    -- reads it to decide anything, because the seeded rows in hierarchy_levels,
    -- card_types and statuses *are* the behaviour. Phase 18 adds more templates;
    -- a CHECK would make each one a migration for no safety gained.
    template        TEXT    NOT NULL,

    -- The per-project card-key counter. ATLAS-7 is allocated by
    -- `UPDATE projects SET card_counter = card_counter + 1 RETURNING card_counter`
    -- inside the creating transaction, which is what makes two concurrent
    -- creates unable to both get 7. Never decremented: a deleted card's key is
    -- burned, because a reused key would silently repoint every bookmark, commit
    -- message and comment that referenced the old one.
    card_counter    INTEGER NOT NULL DEFAULT 0 CHECK (card_counter >= 0),

    -- Cycles are disableable per project (docs/adr/0004). A job-search board has
    -- no sprints and must not be made to pretend otherwise.
    cycles_enabled  INTEGER NOT NULL DEFAULT 0 CHECK (cycles_enabled IN (0, 1)),

    -- ONE estimation field, never Jira's two-fields-both-called-Story-Points.
    -- 'none' is a first-class choice: reports degrade to count-based rather than
    -- demanding a number nobody has.
    estimation_unit TEXT    NOT NULL DEFAULT 'none'
                    CHECK (estimation_unit IN ('points', 'hours', 'days', 'tshirt', 'count', 'none')),

    -- Soft archive. NULL = live.
    archived_at     TEXT,

    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL
) STRICT;

CREATE INDEX projects_archived_at_idx ON projects (archived_at);

-- ---------------------------------------------------------------------------
-- hierarchy_levels — the table that makes one engine serve three domains.
--
--   Programming   2 Initiative  1 Epic     0 Story        -1 Sub-task
--   3D modeling   2 Collection  1 Asset    0 Model        -1 Step
--   Job search    —             1 Company  0 Application  -1 Task
--
-- Higher level = further up the tree. Levels may be negative (Jira's sub-task
-- level is -1) and need not be contiguous.
-- ---------------------------------------------------------------------------
CREATE TABLE hierarchy_levels (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    level      INTEGER NOT NULL,
    name       TEXT    NOT NULL,

    -- Also the index the composite foreign key from card_types needs.
    UNIQUE (project_id, level)
) STRICT;

-- ---------------------------------------------------------------------------
-- card_types — per project, not a fixed enum.
-- ---------------------------------------------------------------------------
CREATE TABLE card_types (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    icon       TEXT,
    colour     TEXT,

    -- Which rung of this project's hierarchy a card of this type sits on. The
    -- composite FK below is what enforces ADR 0002's structural rule at the
    -- storage layer: a type cannot claim a level the project has not defined.
    level      INTEGER NOT NULL,

    is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),

    UNIQUE (project_id, name),

    -- DEFERRABLE INITIALLY DEFERRED because deleting a project cascades into
    -- *both* this table and hierarchy_levels, and SQLite does not promise an
    -- order. Immediate enforcement would make `DELETE FROM projects` fail
    -- roughly half the time, depending on which cascade ran first.
    FOREIGN KEY (project_id, level) REFERENCES hierarchy_levels (project_id, level)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX card_types_project_id_idx ON card_types (project_id);

-- ---------------------------------------------------------------------------
-- statuses — and EXACTLY three categories.
--
-- Not a limitation copied thoughtlessly: boards, reports, burndown, the CFD and
-- every "is this open?" question key off the three buckets. A fourth category
-- has no meaning to any of them, which is why Jira hardcodes three and refuses
-- more. The *statuses* are unlimited and per-project; only the bucketing is
-- fixed. Job search proves the point: nine statuses, three categories.
-- ---------------------------------------------------------------------------
CREATE TABLE statuses (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    category   TEXT    NOT NULL CHECK (category IN ('todo', 'in_progress', 'done')),

    -- Display order on the board, left to right.
    position   INTEGER NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

CREATE INDEX statuses_project_id_position_idx ON statuses (project_id, position);

-- ---------------------------------------------------------------------------
-- priorities — ORDERED, which is the whole reason `rank` is here.
--
-- `priority > High` is a query Atlas must be able to answer (Phase 6), and it is
-- meaningless without a total order over the names. Lower rank = more urgent, so
-- rank 1 is Highest and `priority > High` is `rank < High.rank`.
--
-- NOTE: this `rank` is an INTEGER ordinal and has nothing to do with `cards.rank`,
-- which is a lexicographic fractional index. Same word, unrelated jobs.
-- ---------------------------------------------------------------------------
CREATE TABLE priorities (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    icon       TEXT,
    colour     TEXT,
    rank       INTEGER NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

CREATE INDEX priorities_project_id_rank_idx ON priorities (project_id, rank);

-- ---------------------------------------------------------------------------
-- resolutions — why a card stopped.
--
-- A card is resolved iff resolution_id IS NOT NULL. Jira's most-reported
-- confusion is that this is independent of reaching a Done status, so a card can
-- sit in "Done" and count as open in every report and query. Atlas keeps the
-- expressive power (Done vs Won't Do vs Duplicate) and kills the failure mode by
-- auto-setting and auto-clearing resolution from status-category transitions —
-- see `domain::card::apply_resolution_rules` and docs/adr §E.
-- ---------------------------------------------------------------------------
CREATE TABLE resolutions (
    id         TEXT    NOT NULL PRIMARY KEY,
    project_id TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    position   INTEGER NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

CREATE INDEX resolutions_project_id_position_idx ON resolutions (project_id, position);

-- ---------------------------------------------------------------------------
-- cards — the unit of work, at every level of the hierarchy.
-- ---------------------------------------------------------------------------
CREATE TABLE cards (
    id            TEXT NOT NULL PRIMARY KEY,

    -- ATLAS-123. Case-sensitive storage (the application uppercases), and UNIQUE
    -- across the whole instance rather than per project, so a key identifies a
    -- card with no other context — which is what lets `ATLAS-123` be autolinked
    -- from a commit message, a comment, or a Claude Code prompt.
    key           TEXT NOT NULL UNIQUE,

    project_id    TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,

    -- The config FKs are DEFERRABLE INITIALLY DEFERRED for the same reason as
    -- card_types' composite FK: `DELETE FROM projects` cascades into cards and
    -- into every config table at once, and SQLite fixes no order between them.
    type_id       TEXT NOT NULL REFERENCES card_types (id) DEFERRABLE INITIALLY DEFERRED,

    -- The uniform parent pointer. This one nullable column is the entire nested
    -- board feature and the entire Epic/Story/Sub-task feature.
    --
    -- ON DELETE SET NULL: orphaning a subtree is recoverable, destroying it is
    -- not. (The product soft-deletes cards anyway — see deleted_at — so this
    -- fires only on a project cascade or an operator's hand.)
    parent_id     TEXT REFERENCES cards (id) ON DELETE SET NULL,

    summary       TEXT NOT NULL,

    -- Markdown SOURCE. Never rendered HTML: rendering happens at read time and
    -- is sanitised there, so a stored-XSS payload has nowhere to live.
    description   TEXT,

    status_id     TEXT NOT NULL REFERENCES statuses (id) DEFERRABLE INITIALLY DEFERRED,
    priority_id   TEXT REFERENCES priorities (id) DEFERRABLE INITIALLY DEFERRED,

    -- Users are never hard-deleted, so NO ACTION is safe and honest here.
    assignee_id   TEXT REFERENCES users (id),
    reporter_id   TEXT REFERENCES users (id),
    creator_id    TEXT NOT NULL REFERENCES users (id),

    resolution_id TEXT REFERENCES resolutions (id) DEFERRABLE INITIALLY DEFERRED,
    resolved_at   TEXT,

    due_date      TEXT,
    start_date    TEXT,

    -- REAL, singular, and nullable. One estimation field interpreted through
    -- projects.estimation_unit — points, hours, days, a t-shirt size mapped to a
    -- number, a count, or nothing at all.
    estimate      REAL,

    -- The lexicographic sort key for drag-and-drop (see src/rank.rs).
    --
    -- The collation here is the DEFAULT (BINARY), and that is load-bearing:
    -- Rank's ordering guarantee is that hex byte order equals string order, so
    -- `ORDER BY rank` sorts correctly with no custom collation. Adding COLLATE
    -- NOCASE to this column would silently break every board's ordering.
    rank          TEXT NOT NULL,

    archived_at   TEXT,

    -- Soft delete: the trash. A hard DELETE would take the card's history,
    -- comments and inbound links with it, and "restore" is the whole point.
    deleted_at    TEXT,

    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

-- The board query: every live card in one column of one project, in rank order.
-- Partial, because a board never looks at the trash — this keeps deleted cards
-- out of the index entirely rather than filtering them out on every read.
CREATE INDEX cards_board_idx ON cards (project_id, status_id, rank) WHERE deleted_at IS NULL;

-- "Open this card's board" and every roll-up.
CREATE INDEX cards_parent_idx ON cards (parent_id) WHERE deleted_at IS NULL;

-- "Assigned to me".
CREATE INDEX cards_assignee_idx ON cards (assignee_id) WHERE deleted_at IS NULL;

-- Backlog / project-wide rank ordering.
CREATE INDEX cards_project_rank_idx ON cards (project_id, rank) WHERE deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- card_key_history — permanent redirects.
--
-- When a card moves between projects it gets a new key. Without this table every
-- bookmark, every commit message, every PR title and every comment that ever
-- said ATLAS-42 becomes a 404 the moment someone tidies up their projects. The
-- rows are append-only and never expire: a redirect that stops working after a
-- year is a redirect that was not worth writing.
--
-- old_key is UNIQUE across the instance and shares a namespace with cards.key —
-- a key is either live or retired, never both, because the project counter never
-- rewinds.
-- ---------------------------------------------------------------------------
CREATE TABLE card_key_history (
    id       TEXT NOT NULL PRIMARY KEY,
    card_id  TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    old_key  TEXT NOT NULL UNIQUE,
    moved_at TEXT NOT NULL
) STRICT;

CREATE INDEX card_key_history_card_id_idx ON card_key_history (card_id);

-- ---------------------------------------------------------------------------
-- card_history — the changelog. §D1: cannot be retrofitted.
--
-- Written in the SAME TRANSACTION as the change it records, by
-- `domain::card::update`, which diffs the row rather than trusting a handler to
-- remember. History is unreconstructable after the fact: if this row is not
-- written now, the information is gone forever, and it is what powers the
-- history tab, `status CHANGED FROM ... AFTER -7d` (Phase 6), every report
-- (Phase 16) and the automation audit trail (Phase 15).
--
-- ---------------------------------------------------------------------------
-- Why BOTH a raw value and a display value
-- ---------------------------------------------------------------------------
--
-- They answer different questions and neither can be derived from the other
-- afterwards:
--
--   from_value / to_value     the id. What a query matches on, stable across
--                             renames.
--   from_display / to_display the name AS IT WAS AT THE TIME. What a human
--                             reads.
--
-- Store only the id and the history tab renders "assignee changed to ?" the day
-- someone is deactivated, and "moved to <current name>" — which is a lie — after
-- a status is renamed. Store only the name and `status CHANGED TO "Done"` breaks
-- on the rename instead. Jira stores both for exactly this reason.
-- ---------------------------------------------------------------------------
CREATE TABLE card_history (
    id           TEXT NOT NULL PRIMARY KEY,
    card_id      TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- Nullable: a change made by an automation rule or an agent has no human
    -- author. ON DELETE SET NULL so the history survives even a hand-run delete.
    author_id    TEXT REFERENCES users (id) ON DELETE SET NULL,

    created_at   TEXT NOT NULL,

    -- The LOGICAL field name — 'status', not 'status_id'. This is the spelling
    -- Phase 6's `status CHANGED FROM "To Do"` will match against.
    field        TEXT NOT NULL,

    from_value   TEXT,
    from_display TEXT,
    to_value     TEXT,
    to_display   TEXT
) STRICT;

-- The history tab, and every WAS/CHANGED query.
CREATE INDEX card_history_card_id_created_at_idx ON card_history (card_id, created_at);
CREATE INDEX card_history_field_created_at_idx ON card_history (field, created_at);

-- ---------------------------------------------------------------------------
-- comments
-- ---------------------------------------------------------------------------
CREATE TABLE comments (
    id         TEXT NOT NULL PRIMARY KEY,
    card_id    TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    author_id  TEXT NOT NULL REFERENCES users (id),

    -- Markdown source. Same rule as cards.description: never rendered HTML.
    body       TEXT NOT NULL,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    -- NULL until the body is edited. Distinct from updated_at, which any write
    -- touches: this is what the UI needs to show "(edited)" honestly.
    edited_at  TEXT
) STRICT;

CREATE INDEX comments_card_id_created_at_idx ON comments (card_id, created_at);

-- ---------------------------------------------------------------------------
-- card_links — blocks / relates / duplicates / clones / causes.
--
-- The inverse is MATERIALISED: linking A blocks B writes both (A, B, 'blocks')
-- and (B, A, 'is_blocked_by'). The alternative — one row plus a UNION at read
-- time — makes every "show this card's links" query two scans and every future
-- AQL `linkedCards()` a special case. Two rows are cheap; the join is not.
-- ---------------------------------------------------------------------------
CREATE TABLE card_links (
    id           TEXT NOT NULL PRIMARY KEY,
    from_card_id TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    to_card_id   TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    link_type    TEXT NOT NULL,
    created_at   TEXT NOT NULL,

    -- A card cannot block itself.
    CHECK (from_card_id <> to_card_id),
    UNIQUE (from_card_id, to_card_id, link_type)
) STRICT;

CREATE INDEX card_links_from_card_id_idx ON card_links (from_card_id);
CREATE INDEX card_links_to_card_id_idx ON card_links (to_card_id);

-- ---------------------------------------------------------------------------
-- watchers
-- ---------------------------------------------------------------------------
CREATE TABLE watchers (
    card_id    TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,

    PRIMARY KEY (card_id, user_id)
) STRICT;

CREATE INDEX watchers_user_id_idx ON watchers (user_id);

-- NOTE: `_atlas_meta.schema_version` is deliberately NOT bumped here.
--
-- 0001 introduced it and set it to '1'; 0002 did not touch it. `_sqlx_migrations`
-- is what actually records which migrations have run, and it is maintained by the
-- migrator rather than by hand — so a second, hand-edited version counter earns
-- nothing and can only disagree with the first. Bumping it here would make 0003
-- the only migration that does, which is worse than either convention applied
-- consistently. Whether the column should exist at all is a question for whoever
-- owns `db/migrate.rs`, not something to settle from inside a domain migration.

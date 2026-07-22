-- Atlas schema, migration 0008: saved board configuration.
--
-- Every table is STRICT, matching 0001-0007.
--
-- ---------------------------------------------------------------------------
-- What a board is, and what this table is *not*
-- ---------------------------------------------------------------------------
--
-- The board that matters is not a row — it is a *view* over cards, computed on
-- demand by `GET /projects/{key}/board`: the project's cards grouped into columns
-- by status, optionally scoped to one card's children (the nested board) and
-- narrowed by an AQL quick filter. That endpoint owns no state; it reads cards
-- and statuses and groups them. See `crate::domain::board`.
--
-- This table is the thin persistence *around* that view: a saved name plus the
-- knobs the board-data endpoint already takes as query parameters, so a user can
-- return to "my board" without re-typing the filter and swimlane every time. It
-- is deliberately small — the board-DATA endpoint is the feature; a saved board
-- is a bookmark for it (TODO.md Phase 8).
--
-- ---------------------------------------------------------------------------
-- Why the columns are what they are
-- ---------------------------------------------------------------------------
--
--   default_parent_id  The card whose children this board renders, or NULL for
--                      the project's top level. It is the `parent` query param of
--                      the board-data endpoint, remembered. ON DELETE SET NULL:
--                      if that card is trashed the board falls back to the
--                      top-level view rather than pointing at nothing.
--   aql_filter         The saved quick filter, ANDed into the board query by the
--                      same AQL layer every other filter uses. NULL = no filter.
--   swimlane           How the board groups rows: none | assignee | parent. A
--                      CHECK pins the three the endpoint understands, the same
--                      way 0003's status-category CHECK pins its three — a fourth
--                      value would mean nothing to the grouping code and must not
--                      reach it.
--   wip_limits         Per-status maximums, as a JSON object {status_id: max}.
--                      JSON rather than a child table because it is read and
--                      written whole, always by the board that owns it, and never
--                      queried across boards — exactly the shape a JSON column is
--                      for. Defaults to '{}' (no limits).

CREATE TABLE boards (
    id                TEXT NOT NULL PRIMARY KEY,

    -- The project this board belongs to. A board is project config, so it dies
    -- with its project.
    project_id        TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,

    -- The display name, unique per project so a board list has no two rows a user
    -- cannot tell apart. COLLATE NOCASE so "My Board" and "my board" cannot both
    -- exist and make the name ambiguous.
    name              TEXT NOT NULL COLLATE NOCASE,

    -- The card whose children the board renders. NULL = the project's top level.
    default_parent_id TEXT REFERENCES cards (id) ON DELETE SET NULL,

    -- The saved AQL quick filter, or NULL for none.
    aql_filter        TEXT,

    -- none | assignee | parent. The three the board-data endpoint groups by.
    swimlane          TEXT NOT NULL DEFAULT 'none'
                          CHECK (swimlane IN ('none', 'assignee', 'parent')),

    -- Per-status WIP limits as a JSON object. '{}' = none.
    wip_limits        TEXT NOT NULL DEFAULT '{}',

    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

-- The board list for a project, and the name-uniqueness check behind it.
CREATE INDEX boards_project_idx ON boards (project_id);

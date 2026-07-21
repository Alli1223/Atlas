-- Atlas schema, migration 0007: saved AQL filters.
--
-- STRICT, matching every table before it.
--
-- A filter is a named AQL query somebody saved. Boards, quick filters,
-- dashboards, gadgets and automation conditions are all "a filter plus a
-- renderer" (TODO.md Phase 6/8/15/16), so this one small table is reused far out
-- of proportion to its size.
--
-- ---------------------------------------------------------------------------
-- Composition and the cycle it invites
-- ---------------------------------------------------------------------------
--
-- AQL supports `filter = "My Filter"` — a filter that references another. That
-- is genuinely useful (a base filter plus per-board overrides) and it is also a
-- loaded gun: filter A can reference B which references A, and a naive expander
-- recurses until the stack runs out. The guard is NOT in the schema, because
-- "does the AQL text of A transitively mention B" is not a question a foreign key
-- can ask — the reference lives inside a TEXT column as `filter = "..."`, by
-- name or by id, resolved at compile time. So the cycle guard lives in
-- `crate::aql::expand_filters`, which tracks the chain of ids it is expanding and
-- refuses to visit one twice. `tests/aql.rs` pins it.

CREATE TABLE filters (
    id          TEXT NOT NULL PRIMARY KEY,

    -- Who owns it. ON DELETE CASCADE: a filter is personal working state, not a
    -- shared record like a card, so it goes with its owner. Users are never
    -- hard-deleted anyway (0002), so this only fires if someone reaches around
    -- the application.
    owner_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,

    -- The display name. Unique per owner so `filter = "My Filter"` resolves to
    -- exactly one filter for a given caller. COLLATE NOCASE so "My Filter" and
    -- "my filter" cannot both exist and make the reference ambiguous.
    name        TEXT NOT NULL COLLATE NOCASE,

    description TEXT,

    -- The AQL source. Stored as text and re-parsed on use rather than storing a
    -- compiled form: the compiler improves over time, and a filter saved last
    -- month should benefit from this month's fixes, not carry a frozen plan.
    aql         TEXT NOT NULL,

    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,

    UNIQUE (owner_id, name)
) STRICT;

-- The list view ("my filters") and the name resolver for composition.
CREATE INDEX filters_owner_name_idx ON filters (owner_id, name);

-- Atlas schema, migration 0004: tags.
--
-- Every table is STRICT, matching 0001-0003.
--
-- ---------------------------------------------------------------------------
-- Why a two-table join and not a column on cards
-- ---------------------------------------------------------------------------
--
-- A `labels TEXT` column holding "bug,urgent" would be smaller and would work
-- until the first question anybody actually asks of it: rename a tag across
-- 400 cards, count usages, autocomplete from what exists, merge two spellings
-- of the same idea. Each of those is a string-surgery bug waiting to happen,
-- and `WHERE labels LIKE '%bug%'` matches `debug` and `bugfix` too.
--
-- The join table makes `tag = bug` an index lookup, makes rename an UPDATE of
-- one row, and makes merge an INSERT plus a DELETE. That is why this phase is
-- marked highest-value/lowest-cost in TODO.md: the value is entirely in the
-- shape of these two tables, and there is barely any code above them.

-- ---------------------------------------------------------------------------
-- tags
-- ---------------------------------------------------------------------------
CREATE TABLE tags (
    id         TEXT NOT NULL PRIMARY KEY,

    -- NULL means GLOBAL: the tag is offered by, and usable from, every project.
    -- A project tag belongs to one project and is invisible everywhere else.
    --
    -- Nullable rather than two tables because everything that reads a tag reads
    -- both kinds together — the picker, the filter chips, the usage counts — and
    -- a UNION of two identical tables at every one of those call sites buys
    -- nothing but a second place to forget one of them.
    project_id TEXT REFERENCES projects (id) ON DELETE CASCADE,

    -- Free text with one rule: NO WHITESPACE. See `domain::tag::validate_name`
    -- for why the rule is worth more than the freedom it costs.
    --
    -- COLLATE NOCASE, so the UNIQUE constraints below are too: `Bug` and `bug`
    -- are one tag, not two. Tags exist to gather cards together, and a set of
    -- labels that silently splits on capitalisation does the opposite of that.
    -- The typed casing is still what gets stored, and still what gets rendered.
    name       TEXT NOT NULL COLLATE NOCASE,

    -- An ADS accent name — 'blue', 'green', 'grey', … — NOT a hex colour, and
    -- that is deliberate.
    --
    -- A chip needs a *pair* of colours (background and text) that stay legible
    -- in both light and dark mode. One hex cannot be that pair: #DCFFF1 is a
    -- readable green chip at noon and an eye-watering one at midnight. Each name
    -- here resolves to `--atlas-accent-{name}-bg` / `--atlas-accent-{name}-text`
    -- in frontend/src/styles/tokens.css, which are themed. So `card_types.colour`
    -- storing a hex and this column storing a name is not an inconsistency: an
    -- icon tinted with one colour and a chip built from two are different jobs.
    --
    -- NULL = no colour chosen; the frontend renders the neutral chip.
    --
    -- The CHECK is the storage-layer half of `domain::tag::TagColour`; the enum's
    -- FromStr is the other. Two independent guards against a colour that resolves
    -- to no CSS variable and renders an invisible chip.
    colour     TEXT CHECK (colour IN (
                   'standard', 'grey', 'blue', 'teal', 'green', 'lime',
                   'yellow', 'orange', 'red', 'magenta', 'purple'
               )),

    created_at TEXT NOT NULL,

    -- Distinct names within one project.
    --
    -- NOTE: this constraint does NOT constrain global tags, and that is not an
    -- oversight — it is SQL's NULL semantics. Two rows (NULL, 'urgent') are both
    -- accepted by this index, because NULL is never equal to NULL, so every
    -- global tag is unique-by-definition and the constraint has nothing to say.
    -- The partial index below is what actually enforces uniqueness for globals.
    UNIQUE (project_id, name)
) STRICT;

-- The other half of UNIQUE (project_id, name): global tags.
--
-- Without this, `urgent` could exist five times globally, the picker would show
-- five identical chips, and merging them would be the user's problem. A partial
-- UNIQUE index is the standard SQL answer to a nullable column in a unique key.
CREATE UNIQUE INDEX tags_global_name_idx ON tags (name) WHERE project_id IS NULL;

-- The picker and the filter chips: every tag a project can offer, by name.
CREATE INDEX tags_project_id_name_idx ON tags (project_id, name);

-- ---------------------------------------------------------------------------
-- card_tags
-- ---------------------------------------------------------------------------
CREATE TABLE card_tags (
    card_id    TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- ON DELETE CASCADE is what makes "delete a tag" a one-statement operation
    -- that cannot half-finish: `DELETE FROM tags WHERE id = ?` takes every
    -- (card, tag) row with it, so no card is ever left pointing at a tag that
    -- has stopped existing. Doing it by hand in the application would be the
    -- same effect with an extra way to get it wrong.
    tag_id     TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,

    created_at TEXT NOT NULL,

    -- The pair is the identity: a card carries a tag or it does not. There is no
    -- such thing as carrying it twice, so the primary key says so rather than
    -- leaving the application to remember.
    PRIMARY KEY (card_id, tag_id)
) STRICT;

-- The PK indexes (card_id, tag_id), which serves "this card's tags". The reverse
-- question — "which cards have this tag", i.e. every filter chip and every usage
-- count — needs its own index or it is a full scan of the join table.
CREATE INDEX card_tags_tag_id_idx ON card_tags (tag_id);

-- ---------------------------------------------------------------------------
-- A constraint that is NOT here, and why
-- ---------------------------------------------------------------------------
--
-- Nothing above stops a card in project A from being given a tag belonging to
-- project B. Expressing that in SQL would need `card_tags` to carry a redundant
-- project_id plus two composite foreign keys — and it would still not cover the
-- global case, where `tags.project_id IS NULL` and there is nothing to match on.
--
-- So the rule lives in `domain::tag::attach`, which resolves the tag against the
-- card's own project and refuses anything else, and `tests/tags.rs` pins it.
-- Recorded here because a reader who finds no constraint is entitled to assume
-- nobody thought about it.

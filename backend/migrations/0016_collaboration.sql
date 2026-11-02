-- Atlas schema, migration 0016: the rest of the card's collaboration surface.
--
-- STRICT, matching 0001-0015.
--
-- TODO.md Phase 3 left five aggregates unbuilt (remote links, watchers' API, worklogs' API,
-- components, milestones) plus attachments with versioning. `watchers` and `card_worklogs`
-- already exist (0003, 0010) and only gain columns here; everything else is new.

-- ---------------------------------------------------------------------------
-- remote_links — a URL attached to a card, with a title and optional icon.
-- ---------------------------------------------------------------------------
--
-- Jira's "web links". Deliberately not fetched or unfurled by the server: a remote link is
-- whatever the user typed, rendered as a link, so there is no server-side request a link can
-- be used to aim (SSRF) — the title is the user's, not the page's.
CREATE TABLE remote_links (
    id          TEXT NOT NULL PRIMARY KEY,
    card_id     TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    url         TEXT NOT NULL,
    title       TEXT NOT NULL,
    icon        TEXT,
    -- ON DELETE SET NULL: a link outlives an account cleanup, like a worklog does.
    created_by  TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL
) STRICT;

CREATE INDEX remote_links_card_idx ON remote_links (card_id);

-- ---------------------------------------------------------------------------
-- card_worklogs — gains "when the work happened" and an edit timestamp.
-- ---------------------------------------------------------------------------
--
-- `created_at` is when the row was written; `started_at` is when the work was done. They
-- differ every time someone logs yesterday's afternoon this morning, and a timesheet that
-- cannot tell them apart is wrong for exactly the people who use one. NULL on the
-- smart-commit rows written before this migration, which is read back as `created_at`.
ALTER TABLE card_worklogs ADD COLUMN started_at TEXT;
ALTER TABLE card_worklogs ADD COLUMN updated_at TEXT;

-- ---------------------------------------------------------------------------
-- components — a project's named sub-areas ("Frontend", "Rigging", "Recruiters").
-- ---------------------------------------------------------------------------
CREATE TABLE components (
    id          TEXT NOT NULL PRIMARY KEY,
    project_id  TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name        TEXT NOT NULL COLLATE NOCASE,
    description TEXT,
    -- The default assignee for the area. Users are never hard-deleted (0002).
    lead_id     TEXT REFERENCES users (id),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

-- Many-to-many: a card can touch two areas at once, which is the norm, not the exception.
CREATE TABLE card_components (
    card_id      TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    component_id TEXT NOT NULL REFERENCES components (id) ON DELETE CASCADE,
    PRIMARY KEY (card_id, component_id)
) STRICT;

CREATE INDEX card_components_component_idx ON card_components (component_id);

-- ---------------------------------------------------------------------------
-- milestones — Jira's "versions", generalised.
-- ---------------------------------------------------------------------------
--
-- "v1.2" in a programming project, "Client delivery #3" for a 3D pipeline, "Q1 applications
-- round" for a job search. A release date and a released flag; nothing software-shaped.
CREATE TABLE milestones (
    id           TEXT NOT NULL PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name         TEXT NOT NULL COLLATE NOCASE,
    description  TEXT,
    start_date   TEXT,
    release_date TEXT,
    status       TEXT NOT NULL DEFAULT 'planned'
                     CHECK (status IN ('planned', 'released', 'archived')),
    released_at  TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

CREATE INDEX milestones_project_idx ON milestones (project_id);

-- Single-valued on purpose: `milestone` is one of the six history-indexed fields AQL's
-- WAS/CHANGED operators accept (TODO.md Phase 6), and a multi-valued field has no single
-- "from" and "to" to record. ON DELETE SET NULL: deleting a milestone un-schedules its cards,
-- it does not destroy them.
ALTER TABLE cards ADD COLUMN milestone_id TEXT REFERENCES milestones (id) ON DELETE SET NULL;

CREATE INDEX cards_milestone_idx ON cards (milestone_id) WHERE deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- attachments + attachment_versions — files on a card, with history.
-- ---------------------------------------------------------------------------
--
-- Uploading `model.blend` to a card that already has a `model.blend` adds version 2 rather
-- than a second file with the same name: the 3D pipeline's `model_v3_final_FINAL.blend`
-- problem, solved by the store instead of by the filename. Jira has no equivalent.
--
-- The bytes live on disk under `{data_dir}/uploads/attachments/{attachment_id}/{version}` —
-- a path built only from server-generated ids, never from the user's filename, so there is
-- no traversal to guard against. The filename is display metadata only.
CREATE TABLE attachments (
    id              TEXT    NOT NULL PRIMARY KEY,
    card_id         TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    filename        TEXT    NOT NULL,
    current_version INTEGER NOT NULL CHECK (current_version >= 1),
    created_by      TEXT    REFERENCES users (id) ON DELETE SET NULL,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL,

    UNIQUE (card_id, filename)
) STRICT;

CREATE TABLE attachment_versions (
    id            TEXT    NOT NULL PRIMARY KEY,
    attachment_id TEXT    NOT NULL REFERENCES attachments (id) ON DELETE CASCADE,
    version       INTEGER NOT NULL CHECK (version >= 1),
    content_type  TEXT    NOT NULL,
    size_bytes    INTEGER NOT NULL CHECK (size_bytes >= 0),
    sha256        TEXT    NOT NULL,
    uploaded_by   TEXT    REFERENCES users (id) ON DELETE SET NULL,
    created_at    TEXT    NOT NULL,

    UNIQUE (attachment_id, version)
) STRICT;

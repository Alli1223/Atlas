-- Atlas schema, migration 0028: card templates.
--
-- STRICT, matching 0001-0021.
--
-- A pre-filled "create card": a job application's prep checklist, a 3D asset's pipeline
-- steps, a bug report's sections. Checklists are plain markdown task lists in the description
-- — they replace sub-task sprawl, which is exactly why TODO.md Phase 9 calls them out.
CREATE TABLE card_templates (
    id           TEXT NOT NULL PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name         TEXT NOT NULL COLLATE NOCASE,
    card_type_id TEXT REFERENCES card_types (id) ON DELETE SET NULL,
    summary      TEXT NOT NULL DEFAULT '',
    description  TEXT,
    priority_id  TEXT REFERENCES priorities (id) ON DELETE SET NULL,
    -- JSON array of tag names.
    labels       TEXT NOT NULL DEFAULT '[]',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,

    UNIQUE (project_id, name)
) STRICT;

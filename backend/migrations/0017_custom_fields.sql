-- Atlas schema, migration 0017: custom fields.
--
-- STRICT, matching 0001-0016.
--
-- TODO.md §B: "Share exactly two things — workflows, and custom field definitions (a field
-- must be one global entity or cross-project JQL breaks)." So a field DEFINITION is global
-- (`custom_fields`, unique by name across the instance), and everything else about it is flat
-- per-project configuration (`project_field_layout`: is it shown, required, defaulted, in what
-- order, for which card types). There is no field configuration scheme, no screen, no screen
-- scheme — the layout row is all of it.

CREATE TABLE custom_fields (
    id          TEXT NOT NULL PRIMARY KEY,
    -- Global and unique, case-insensitively: `customField("Company", "Acme")` must mean the
    -- same field in every project or a cross-project query silently splits in two.
    name        TEXT NOT NULL COLLATE NOCASE UNIQUE,
    field_type  TEXT NOT NULL CHECK (field_type IN (
                    'text', 'textarea', 'number', 'date', 'datetime', 'select', 'multiselect',
                    'checkbox', 'radio', 'user', 'multiuser', 'url', 'labels'
                )),
    description TEXT,
    -- JSON array of strings: the choices for select / multiselect / radio. '[]' otherwise.
    options     TEXT NOT NULL DEFAULT '[]',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

CREATE TABLE project_field_layout (
    id           TEXT    NOT NULL PRIMARY KEY,
    project_id   TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    field_id     TEXT    NOT NULL REFERENCES custom_fields (id) ON DELETE CASCADE,
    -- NULL = every card type in the project.
    card_type_id TEXT    REFERENCES card_types (id) ON DELETE CASCADE,
    required     INTEGER NOT NULL DEFAULT 0 CHECK (required IN (0, 1)),
    hidden       INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    -- JSON, already validated against the field's type. NULL = no default.
    default_value TEXT,
    position     INTEGER NOT NULL DEFAULT 0
) STRICT;

-- One row per (project, field, type), where "all types" (NULL) counts as one value. A plain
-- UNIQUE would treat every NULL as distinct and allow duplicates of the all-types row.
CREATE UNIQUE INDEX project_field_layout_unique_idx
    ON project_field_layout (project_id, field_id, COALESCE(card_type_id, ''));

CREATE TABLE card_field_values (
    card_id  TEXT NOT NULL REFERENCES cards (id) ON DELETE CASCADE,
    field_id TEXT NOT NULL REFERENCES custom_fields (id) ON DELETE CASCADE,
    -- JSON: a string, number, boolean, or array, per the field's type. Validated on write.
    value    TEXT NOT NULL,
    PRIMARY KEY (card_id, field_id)
) STRICT;

-- `customField("Company", "Acme")` scans by field.
CREATE INDEX card_field_values_field_idx ON card_field_values (field_id);

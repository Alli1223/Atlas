-- Atlas schema, migration 0001: bootstrap.
--
-- Deliberately minimal. This migration exists to prove the migration runner and
-- to give the database a place to record facts about itself. The real domain
-- schema (projects, cards, hierarchy_levels, card_history, ...) arrives in
-- Phase 3 — see TODO.md.

-- Key/value facts about this database instance.
--
-- Distinct from `_sqlx_migrations`, which tracks *which* migrations ran. This
-- table holds semantics: schema version, install id, and later the vault key id.
--
-- STRICT rejects values of the wrong type at write time rather than silently
-- coercing them, which is SQLite's single worst default for a typed backend.
CREATE TABLE _atlas_meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

INSERT INTO _atlas_meta (key, value) VALUES ('schema_version', '1');

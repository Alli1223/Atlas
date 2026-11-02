-- Atlas schema, migration 0019: automation rules and their audit log.
--
-- STRICT, matching 0001-0018.
--
-- A rule is one trigger, N conditions, N actions (TODO.md Phase 15), stored as three JSON
-- columns in exactly the shape the API and the rule builder use (`automation::model`), so
-- there is one representation and no translation layer to drift.

CREATE TABLE automation_rules (
    id                  TEXT    NOT NULL PRIMARY KEY,
    project_id          TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                TEXT    NOT NULL,
    description         TEXT,
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    trigger             TEXT    NOT NULL,
    conditions          TEXT    NOT NULL DEFAULT '[]',
    actions             TEXT    NOT NULL DEFAULT '[]',
    -- The rule actor: changes are attributed to this user (with an `automation:` actor tag),
    -- and the rule can only do what they can. Users are never hard-deleted.
    owner_id            TEXT    NOT NULL REFERENCES users (id),
    -- SHA-256 of the incoming-webhook token. The token itself is shown once.
    webhook_token_hash  TEXT    UNIQUE,
    -- For scheduled triggers: when it next runs.
    next_run_at         TEXT,
    last_run_at         TEXT,
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL
) STRICT;

CREATE INDEX automation_rules_project_idx ON automation_rules (project_id);
CREATE INDEX automation_rules_due_idx ON automation_rules (next_run_at) WHERE next_run_at IS NOT NULL;

-- The audit log: one row per execution, with the smart-value resolution trace — the only
-- reason a rule that did the wrong thing at 3am is debuggable at 9am.
CREATE TABLE automation_runs (
    id          TEXT NOT NULL PRIMARY KEY,
    rule_id     TEXT NOT NULL REFERENCES automation_rules (id) ON DELETE CASCADE,
    card_id     TEXT REFERENCES cards (id) ON DELETE SET NULL,
    trigger     TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN (
                    'success', 'no_match', 'failed', 'loop_blocked', 'rate_limited', 'dry_run'
                )),
    -- JSON: { conditions: [...], actions: [...], resolutions: [...] }
    trace       TEXT NOT NULL,
    error       TEXT,
    started_at  TEXT NOT NULL,
    finished_at TEXT NOT NULL
) STRICT;

CREATE INDEX automation_runs_rule_idx ON automation_runs (rule_id, started_at);

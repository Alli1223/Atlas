-- Import provenance keeps reruns idempotent even after cards are edited or moved.
CREATE TABLE todo_imports (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_key TEXT NOT NULL,
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, source_key)
) STRICT;

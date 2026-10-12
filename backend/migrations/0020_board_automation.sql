CREATE TABLE card_dependencies (
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    blocked_by_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (card_id, blocked_by_id),
    CHECK (card_id <> blocked_by_id)
);
CREATE TABLE card_agent_assignments (
    card_id TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    reason TEXT NOT NULL,
    assigned_by TEXT NOT NULL REFERENCES users(id),
    assigned_at TEXT NOT NULL
);
CREATE TABLE board_auto_runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    started_by TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    status TEXT NOT NULL,
    max_tasks INTEGER NOT NULL,
    completed_tasks INTEGER NOT NULL DEFAULT 0,
    current_card_key TEXT,
    current_session_id TEXT,
    message TEXT NOT NULL,
    console TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX one_active_board_coordinator ON board_auto_runs(project_id)
    WHERE status IN ('running', 'stopping');
ALTER TABLE agent_sessions ADD COLUMN finalized_at TEXT;
UPDATE agent_sessions SET finalized_at = ended_at WHERE status <> 'running';

-- Where a Claude Code run moves a card once it finishes successfully.
--
-- Per-project, not a constant: TODO.md's own note flags "move to the backlog" as an unusual
-- destination for *finished* work and asks for a configurable destination defaulting to
-- In Review instead. Atlas has no named "In Review" state (`crate::domain::StatusCategory`'s
-- own doc: three categories, forever) — "In Review" here means the first `in_progress`
-- status, the same substitution `integrations::github::poll`'s auto-transition-on-PR-open
-- already makes. "Backlog" means the first `todo` status, for the same reason.
ALTER TABLE projects ADD COLUMN agent_completion_category TEXT NOT NULL DEFAULT 'in_progress'
    CHECK (agent_completion_category IN ('todo', 'in_progress', 'done'));

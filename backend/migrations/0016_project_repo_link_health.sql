-- Whether a project's linked GitHub repo is actually reachable right now.
--
-- Before this, a broken link (a revoked PAT, or the repo renamed/deleted) was invisible: the
-- `project_repos` row stays exactly as it was, so nothing in the UI ever said the link had
-- stopped working — the dev panel would just fail silently the next time it tried to reach
-- GitHub. This is a *cache* of the last live call's outcome, not something probed on its own
-- schedule: it is written wherever Atlas already makes a GitHub call against a specific repo
-- (the activity endpoint, the poll fallback), never a dedicated health check.

ALTER TABLE project_repos ADD COLUMN link_status TEXT NOT NULL DEFAULT 'ok'
    CHECK (link_status IN ('ok', 'broken'));

-- A human-readable reason, set whenever link_status is 'broken'. NULL while 'ok'.
ALTER TABLE project_repos ADD COLUMN link_error TEXT;

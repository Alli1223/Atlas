-- 0029 — repo link health (`TODO.md` Phase 12: "surface a broken repo link").
--
-- A linked repo can break three ways after the fact: its PAT is revoked (401/403), the repo is
-- deleted or the PAT loses access (404), or it is renamed (GitHub redirects the old name, and
-- the immutable `repo_id` lets Atlas follow it). A periodic check records which, so the UI can
-- say so before an agent run or a branch creation finds out the hard way.

ALTER TABLE project_repos ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'unchecked'
    CHECK (sync_status IN ('unchecked', 'ok', 'no_credential', 'auth_failed', 'not_found', 'error'));
ALTER TABLE project_repos ADD COLUMN sync_detail TEXT;
ALTER TABLE project_repos ADD COLUMN sync_checked_at TEXT;

-- Atlas schema, migration 0005: per-project access.
--
-- Every table is STRICT, matching 0001-0004.
--
-- ---------------------------------------------------------------------------
-- Why this table exists at all
-- ---------------------------------------------------------------------------
--
-- Until this migration, authorisation was the instance-wide role in `users.role`
-- and nothing else: any authenticated user could read every project, and any
-- Member could edit, archive and write cards in every project. That is fine for
-- the single-user install and wrong for every other one — and Phase 11 is about
-- to put encrypted API credentials in this database, at which point "every
-- Member can reach every project" stops being a papercut.
--
-- ---------------------------------------------------------------------------
-- The resolution rules, in full
-- ---------------------------------------------------------------------------
--
-- A user's effective role on a project is the FIRST of these that applies,
-- then capped by their instance role (see the ceiling note below):
--
--   1. Instance admin        -> owner, on every project, with no row here.
--   2. The project's lead    -> owner (`projects.lead_id`).
--   3. A row in this table   -> whatever the row says.
--   4. Otherwise             -> NO ACCESS. Default deny.
--
-- (1) is not a convenience. Without it an admin can be locked out of a project
-- they are nonetheless responsible for administering, and there is no recovery
-- path through the API — the only fix would be editing the database by hand.
-- (2) is the same argument one level down: the lead is who the project points at.
--
-- THE INSTANCE ROLE IS A CEILING, NOT A FLOOR. An instance Viewer who holds an
-- `owner` row here resolves to `viewer`, not `owner`. The instance role says
-- what a person may ever do; this table says where they may do it. A grant here
-- can only ever narrow, never widen — otherwise "read-only account" would be a
-- statement any project owner could quietly overrule.

CREATE TABLE project_members (
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,

    -- ON DELETE CASCADE is a backstop, exactly as it is on `sessions.user_id`:
    -- the product never hard-deletes a user (0002), it deactivates them. If a
    -- user ever does go, their grants must go with them rather than dangle.
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,

    -- The three project roles, mirroring the three instance roles in `users`.
    -- CHECK rather than trust: a typo'd role kept out of the database is a
    -- failure at the INSERT, not a decode error on some later read.
    --
    --   viewer: read cards, comments, tags and config
    --   member: viewer + create/edit/move/delete cards, comment, tag
    --   owner:  member + project settings, member management, archive, config
    role       TEXT NOT NULL CHECK (role IN ('owner', 'member', 'viewer')),

    added_at   TEXT NOT NULL,

    -- Who granted it. NULL means "nobody did" — i.e. the backfill below, which
    -- is a fact about the migration rather than about a person. ON DELETE SET
    -- NULL keeps the grant when its granter goes, matching auth_events.user_id.
    added_by   TEXT REFERENCES users (id) ON DELETE SET NULL,

    -- One grant per person per project. The pair is the identity: there is no
    -- such thing as being a member of a project twice, so the key says so
    -- instead of leaving the application to remember.
    PRIMARY KEY (project_id, user_id)
) STRICT;

-- The PK indexes (project_id, user_id), which serves "who is on this project".
-- The reverse question — "which projects can this user reach" — is the WHERE
-- clause behind GET /projects on every single request, and without this index it
-- is a full scan of the grant table per project row considered.
CREATE INDEX project_members_user_id_idx ON project_members (user_id);

-- ---------------------------------------------------------------------------
-- Backfill: an existing install must not lock itself out
-- ---------------------------------------------------------------------------
--
-- Default deny plus an empty table would mean every project on every existing
-- install becomes unreachable the moment this migration lands. So the backfill
-- has one postcondition, and it is worth stating as an invariant rather than as
-- a description of the two statements below:
--
--   EVERY project that exists when this runs ends up with at least one person in
--   its member list who can actually administer it.
--
-- "Can actually administer it" is `domain::member::grants_ownership`: an `owner`
-- row held by a deactivated account grants nothing to anybody, and one held by an
-- instance Viewer is capped to `viewer` by the ceiling. A backfill that counted
-- either would satisfy itself with a row that cannot do the job — which is the
-- same mistake `domain::member::owner_count` exists to avoid one level up.
--
-- Two statements reach it:
--
--   * the lead, where there is one — they were already the implicit owner, so
--     this only writes it down;
--   * every instance admin, for any project the first statement left without an
--     owner who can own. That is a project with no lead, *and* a project whose
--     lead is deactivated or read-only — which the first statement gives a row
--     that grants nothing. Admins reach such a project implicitly anyway, so this
--     only makes the member list honest about who is responsible for it.
--
-- `added_at` is the migration's own clock rather than the project's created_at:
-- the grant is being made now, and backdating it would be inventing an audit
-- trail. `added_by` is NULL for the same reason — no person did this.
--
-- The timestamp format is the one every Atlas TEXT timestamp column uses (see
-- `auth::to_sql_timestamp`): RFC 3339, microseconds, fixed `+00:00` offset, so
-- that text ordering is chronological ordering. SQLite's `%f` gives
-- milliseconds, hence the trailing `000`.
INSERT OR IGNORE INTO project_members (project_id, user_id, role, added_at, added_by)
SELECT p.id,
       p.lead_id,
       'owner',
       strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now'),
       NULL
FROM projects p
WHERE p.lead_id IS NOT NULL;

-- The condition is "this project's lead cannot own it", which covers both having
-- no lead at all and having one who is deactivated or read-only.
--
-- It is written against `users` rather than as a NOT EXISTS over the rows the
-- statement above just wrote, and that is deliberate on two counts. It is
-- equivalent — `project_members` was created empty a few lines up, so the only
-- rows in it are that statement's — and it avoids a SELECT that reads the very
-- table this INSERT is writing, where SQLite does not promise whether the rows
-- appearing mid-statement are visible to it. With two admins and a self-reading
-- NOT EXISTS, the second admin could see the first's row and be skipped.
INSERT OR IGNORE INTO project_members (project_id, user_id, role, added_at, added_by)
SELECT p.id,
       u.id,
       'owner',
       strftime('%Y-%m-%dT%H:%M:%f000+00:00', 'now'),
       NULL
FROM projects p
CROSS JOIN users u
WHERE u.role = 'admin'
  AND u.is_active = 1
  AND NOT EXISTS (
        SELECT 1
        FROM users lead
        WHERE lead.id = p.lead_id
          AND lead.is_active = 1
          AND lead.role != 'viewer'
      );

-- ---------------------------------------------------------------------------
-- A constraint that is NOT here, and why
-- ---------------------------------------------------------------------------
--
-- Nothing above stops the last `owner` row of a project from being deleted or
-- demoted, leaving a project whose member list contains nobody who can manage
-- it. SQL cannot express "this DELETE would leave zero rows matching a
-- predicate" without a trigger, and a trigger would raise an opaque
-- SQLITE_CONSTRAINT that the API could only render as a 500.
--
-- So the guard lives in `domain::member::owner_count`, consulted by the member
-- handlers inside the same write transaction as the change itself — the same
-- shape, and for the same reason, as the last-active-admin guard in
-- `api::users`. `tests/project_access.rs` pins it.
--
-- Note that the guard is load-bearing even though instance admins are implicit
-- owners of everything: `PATCH /projects/{key}` can clear `lead_id`, so without
-- it a project could end up with no owner in its own member list and no lead
-- either, reachable only by an instance admin.

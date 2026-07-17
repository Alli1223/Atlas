-- Atlas schema, migration 0002: users, sessions, auth events, login lockout.
--
-- Every table is STRICT, matching 0001: SQLite's default of silently coercing a
-- value of the wrong type is its single worst default for a typed backend.

-- ---------------------------------------------------------------------------
-- users
--
-- Users are never hard-deleted (`is_active = 0` instead): cards, comments and
-- history rows reference their author forever, and a deleted author turns every
-- one of those references into a dangling id or a cascade that destroys history.
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    -- UUID v7: time-ordered, so the primary-key index stays append-mostly
    -- instead of scattering inserts across the B-tree the way v4 does.
    id                   TEXT    NOT NULL PRIMARY KEY,

    -- COLLATE NOCASE is on the column, so it governs the UNIQUE index too:
    -- "admin" and "Admin" are the same account, and the database — not
    -- application code that might forget — is what enforces that.
    username             TEXT    NOT NULL COLLATE NOCASE UNIQUE,

    -- Nullable: Atlas is self-hosted and an account need not have an email.
    -- SQLite permits many NULLs under a UNIQUE index, which is what we want.
    email                TEXT    COLLATE NOCASE UNIQUE,

    display_name         TEXT    NOT NULL,
    avatar_url           TEXT,

    -- A PHC string ($argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>). The
    -- parameters travel with the hash, so raising them later does not invalidate
    -- existing passwords.
    password_hash        TEXT    NOT NULL,

    -- Atlas has three roles, not Jira's 40-permission x 8-grantee matrix.
    -- CHECK keeps a typo'd role out of the database rather than surfacing it as
    -- a decode error at read time.
    role                 TEXT    NOT NULL CHECK (role IN ('admin', 'member', 'viewer')),

    is_active            INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),

    -- The forced-reset gate. While this is 1 the API rejects everything except
    -- change-password, logout and me. Seeded to 1 for the default Admin.
    must_change_password INTEGER NOT NULL DEFAULT 0 CHECK (must_change_password IN (0, 1)),

    -- RFC 3339 UTC, e.g. 2026-07-16T09:41:07.123456+00:00.
    created_at           TEXT    NOT NULL,
    updated_at           TEXT    NOT NULL,
    last_login_at        TEXT
) STRICT;

-- Listing users orders by display name; without this it is a scan + sort.
CREATE INDEX users_display_name_idx ON users (display_name);

-- ---------------------------------------------------------------------------
-- sessions
--
-- Server-side and revocable, deliberately not a JWT: "remove this user",
-- "force logout everywhere" and "rotate on password change" all have to take
-- effect immediately, and a JWT is valid until it expires.
-- ---------------------------------------------------------------------------
CREATE TABLE sessions (
    -- The SHA-256 hex digest of the session token, NOT the token.
    --
    -- The token is 256 bits from the OS CSPRNG and lives only in the client's
    -- cookie. Reading this table therefore yields nothing that can be replayed
    -- as a login: an attacker with the digest still has to invert SHA-256 to
    -- produce the cookie value. This is the same reason password_hash above is
    -- not the password.
    id           TEXT NOT NULL PRIMARY KEY,

    -- ON DELETE CASCADE is a backstop for the tests and for any future hard
    -- delete; the product never hard-deletes a user.
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,

    created_at   TEXT NOT NULL,

    -- Sliding idle window: a session dies IDLE_TIMEOUT after its last request.
    last_seen_at TEXT NOT NULL,

    -- Absolute cap, set at creation and never extended. Idle expiry alone would
    -- let one continuously-refreshed session live forever.
    expires_at   TEXT NOT NULL,

    user_agent   TEXT,
    ip           TEXT
) STRICT;

-- "List my sessions" and "revoke every session for this user".
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
-- Purging expired rows.
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- ---------------------------------------------------------------------------
-- auth_events — the security audit log.
-- ---------------------------------------------------------------------------
CREATE TABLE auth_events (
    id         TEXT NOT NULL PRIMARY KEY,

    -- Nullable, and that is the point: a failed login for a username that does
    -- not exist is exactly the event worth recording, and it has no user to
    -- point at. ON DELETE SET NULL keeps the event even if the user ever goes.
    user_id    TEXT REFERENCES users (id) ON DELETE SET NULL,

    -- Free text rather than a CHECK: new event kinds arrive with every later
    -- phase (agent runs, secret access), and a migration per kind is friction
    -- with no safety benefit for an append-only log.
    kind       TEXT NOT NULL,

    ip         TEXT,
    user_agent TEXT,
    created_at TEXT NOT NULL,

    -- Human-readable context. Never a password, a token, or a hash.
    detail     TEXT
) STRICT;

CREATE INDEX auth_events_user_id_created_at_idx ON auth_events (user_id, created_at);
CREATE INDEX auth_events_created_at_idx ON auth_events (created_at);

-- ---------------------------------------------------------------------------
-- login_attempts — failure counters driving lockout.
--
-- One row per counter, keyed "user:<lowercased username>" or "ip:<address>".
-- Counters are kept for usernames that do not exist, too: skipping them would
-- make "this username is locked" a membership oracle.
-- ---------------------------------------------------------------------------
CREATE TABLE login_attempts (
    key              TEXT    NOT NULL PRIMARY KEY,
    failures         INTEGER NOT NULL DEFAULT 0,

    -- Start of the current window. Failures older than the window are forgiven,
    -- so an occasional typo never accumulates into a lockout.
    first_failure_at TEXT    NOT NULL,

    -- NULL until the threshold is crossed.
    locked_until     TEXT
) STRICT;

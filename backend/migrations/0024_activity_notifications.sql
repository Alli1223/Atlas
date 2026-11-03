-- Atlas schema, migration 0024: the activity feed, and notifications.
--
-- STRICT, matching 0001-0017.
--
-- ---------------------------------------------------------------------------
-- Actor attribution
-- ---------------------------------------------------------------------------
--
-- `author_id` says which *person* a change is attributed to. `actor` says what actually
-- made it when that was not the person directly: `automation:<rule id>:<depth>` for a rule,
-- `agent:<session id>` for a Claude Code run acting through the MCP server. NULL = a human,
-- by hand. It is what lets the automation engine refuse to react to its own changes past a
-- depth (loop detection, TODO.md Phase 15) and what lets history say "by rule X on behalf
-- of Y" rather than pretending the rule's owner did it by hand.
ALTER TABLE card_history ADD COLUMN actor TEXT;
ALTER TABLE comments ADD COLUMN actor TEXT;
ALTER TABLE cards ADD COLUMN actor TEXT;

-- ---------------------------------------------------------------------------
-- activity_cursor — how far the activity pump has read.
-- ---------------------------------------------------------------------------
--
-- The pump (`crate::activity`) tails three append-only streams by rowid — card creations,
-- changelog rows, comments — and turns them into events for live sync, notifications and
-- automation. Tailing the tables rather than publishing from each handler is deliberate: it
-- sees *every* writer, including the `atlas mcp` subprocess an agent drives, the GitHub
-- webhook, and smart commits, none of which can reach an in-memory channel in the server.
--
-- One row. Seeded at the current high-water marks, so an upgrade does not replay years of
-- history into everyone's notification bell.
CREATE TABLE activity_cursor (
    id            INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    card_rowid    INTEGER NOT NULL,
    history_rowid INTEGER NOT NULL,
    comment_rowid INTEGER NOT NULL,
    updated_at    TEXT    NOT NULL
) STRICT;

INSERT INTO activity_cursor (id, card_rowid, history_rowid, comment_rowid, updated_at)
VALUES (
    1,
    COALESCE((SELECT MAX(rowid) FROM cards), 0),
    COALESCE((SELECT MAX(rowid) FROM card_history), 0),
    COALESCE((SELECT MAX(rowid) FROM comments), 0),
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

-- ---------------------------------------------------------------------------
-- notifications — the bell.
-- ---------------------------------------------------------------------------
CREATE TABLE notifications (
    id            TEXT    NOT NULL PRIMARY KEY,
    user_id       TEXT    NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- assigned, mentioned, commented, transitioned, updated, agent
    kind          TEXT    NOT NULL,
    card_id       TEXT    REFERENCES cards (id) ON DELETE CASCADE,
    -- Who caused it. NULL for an automation or the system.
    actor_id      TEXT    REFERENCES users (id) ON DELETE SET NULL,
    title         TEXT    NOT NULL,
    body          TEXT,
    -- An in-app path to open, e.g. /cards/ATLAS-12.
    link          TEXT,
    -- How many events were coalesced into this one (TODO.md Phase 17's batching): a burst
    -- of edits to one card is one "5 updates" row, not five rows.
    count         INTEGER NOT NULL DEFAULT 1 CHECK (count >= 1),
    -- Queued for the mailer.
    email_pending INTEGER NOT NULL DEFAULT 0 CHECK (email_pending IN (0, 1)),
    read_at       TEXT,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL
) STRICT;

CREATE INDEX notifications_user_idx ON notifications (user_id, read_at, updated_at);
CREATE INDEX notifications_email_idx ON notifications (email_pending) WHERE email_pending = 1;

-- Per-user, per-kind delivery: in_app (the bell), email (the bell and an email), off.
-- No row = the kind's default (see `crate::notify::default_channel`).
CREATE TABLE notification_prefs (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,
    channel TEXT NOT NULL CHECK (channel IN ('in_app', 'email', 'off')),
    PRIMARY KEY (user_id, kind)
) STRICT;

CREATE TABLE notification_settings (
    user_id            TEXT    NOT NULL PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    -- "Don't notify me of my own changes" is the default (0): a solo user must not be
    -- spammed by themselves. This is the opt-IN to hearing about your own edits.
    notify_own_changes INTEGER NOT NULL DEFAULT 0 CHECK (notify_own_changes IN (0, 1)),
    daily_digest       INTEGER NOT NULL DEFAULT 0 CHECK (daily_digest IN (0, 1)),
    last_digest_at     TEXT
) STRICT;

-- ---------------------------------------------------------------------------
-- smtp_settings — outbound email. The password is a vault credential (provider 'smtp').
-- ---------------------------------------------------------------------------
CREATE TABLE smtp_settings (
    id                     INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    host                   TEXT    NOT NULL,
    port                   INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    username               TEXT,
    from_address           TEXT    NOT NULL,
    tls                    TEXT    NOT NULL CHECK (tls IN ('starttls', 'tls', 'none')),
    password_credential_id TEXT    REFERENCES api_credentials (id) ON DELETE SET NULL,
    updated_at             TEXT    NOT NULL
) STRICT;

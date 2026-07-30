-- Atlas schema, migration 0010: the GitHub integration.
--
-- Every table is STRICT, matching 0001-0009.
--
-- ---------------------------------------------------------------------------
-- project_repos — a project linked to one GitHub repository
-- ---------------------------------------------------------------------------
--
-- One repo per project (UNIQUE project_id): the card->branch->PR flow needs an
-- unambiguous "the repo for this card's project", and a card key is unique across
-- the instance, so a webhook that names ATLAS-42 resolves to exactly one repo.
--
-- The link records which stored PAT to act with (`credential_id` -> api_credentials),
-- GitHub's IMMUTABLE numeric `repo_id` (so a rename of owner or name does not lose
-- the link), and the per-repo webhook secret ENCRYPTED with the same vault as
-- api_credentials — the plaintext secret is never stored, and the ciphertext is
-- bound to this row's id as AAD (see crate::integrations::github::store).

CREATE TABLE project_repos (
    -- UUID v7, as text. ALSO the AAD the webhook-secret ciphertext is bound to.
    id                  TEXT    NOT NULL PRIMARY KEY,

    -- The linked project. ON DELETE CASCADE: deleting a project unlinks its repo.
    project_id          TEXT    NOT NULL
                            REFERENCES projects (id) ON DELETE CASCADE,

    -- Which stored GitHub PAT Atlas acts with for this repo. ON DELETE SET NULL:
    -- deleting the credential leaves the link (and its history) intact but inert
    -- until a new credential is chosen, rather than cascading the link away.
    credential_id       TEXT    REFERENCES api_credentials (id) ON DELETE SET NULL,

    -- The repo address. `owner`/`repo` drive the REST paths; `repo_id` is GitHub's
    -- immutable numeric id, the thing that survives a rename and the key any future
    -- installation mapping hangs off (docs/research/github-api.md §11).
    owner               TEXT    NOT NULL,
    repo                TEXT    NOT NULL,
    repo_id             INTEGER NOT NULL,

    -- The repo's default branch, the base a card branch forks from. Not hardcoded
    -- to 'main' — read from the repo payload at link time.
    default_branch      TEXT    NOT NULL,

    -- The branch-name type prefix for {type}/{key}-{slug}. Configurable per repo.
    branch_prefix       TEXT    NOT NULL DEFAULT 'feature',

    -- The created webhook's numeric id, NULL until (and unless) Atlas creates one.
    webhook_id          INTEGER,

    -- The per-repo webhook secret, sealed by the vault. BLOBs (ciphertext + the
    -- 24-byte nonce) plus the key version that sealed them — never the plaintext.
    -- NULL together when no webhook has been created.
    webhook_secret_ciphertext   BLOB,
    webhook_secret_nonce        BLOB,
    webhook_secret_key_version  INTEGER,

    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,

    -- One repo per project. A project relinking to a different repo replaces the row.
    UNIQUE (project_id)
) STRICT;

-- ---------------------------------------------------------------------------
-- card_git_links — a card's branches, PRs, and commits
-- ---------------------------------------------------------------------------
--
-- The materialised view of a card's development activity: the branch Atlas created
-- for it, the PR that branch opened, and the commits that mention its key. Populated
-- by the branch/PR endpoints and by the webhook receiver (push -> commit links,
-- pull_request -> pr link).

CREATE TABLE card_git_links (
    id          TEXT    NOT NULL PRIMARY KEY,

    -- The card. ON DELETE CASCADE: a card's git links go with it.
    card_id     TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- What this link is.
    kind        TEXT    NOT NULL CHECK (kind IN ('branch', 'pr', 'commit')),

    -- The reference: a branch name, a PR number (as text), or a commit SHA.
    ref         TEXT    NOT NULL,

    -- The browser URL, if known.
    url         TEXT,

    -- A rendered state for the UI: a PR's open/merged/closed, a commit's CI badge.
    -- NULL when not applicable or not yet known.
    state       TEXT,

    -- Extra structured data (PR title, commit message, …), as a JSON object.
    meta        TEXT,

    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,

    -- One row per (card, kind, ref): a redelivered push or a re-fetch UPSERTs the
    -- same link rather than piling up duplicates.
    UNIQUE (card_id, kind, ref)
) STRICT;

CREATE INDEX card_git_links_card_idx ON card_git_links (card_id);

-- ---------------------------------------------------------------------------
-- card_worklogs — time logged against a card
-- ---------------------------------------------------------------------------
--
-- The sink a smart commit's `#time 2h 30m` writes to. Deliberately minimal — a
-- fuller worklog feature (editing, per-user reports) is Phase 3 work — but the
-- column set here is a subset of that, so it extends rather than blocks it.

CREATE TABLE card_worklogs (
    id          TEXT    NOT NULL PRIMARY KEY,

    -- The card. ON DELETE CASCADE with the card.
    card_id     TEXT    NOT NULL REFERENCES cards (id) ON DELETE CASCADE,

    -- Who logged it. ON DELETE SET NULL: worklogs outlive an account cleanup.
    author_id   TEXT    REFERENCES users (id) ON DELETE SET NULL,

    -- Time logged, in whole minutes. `2h 30m` -> 150.
    minutes     INTEGER NOT NULL CHECK (minutes > 0),

    -- An optional note (a smart commit's trailing comment).
    note        TEXT,

    -- Where it came from: 'smart-commit', later 'manual', etc.
    source      TEXT    NOT NULL,

    created_at  TEXT    NOT NULL
) STRICT;

CREATE INDEX card_worklogs_card_idx ON card_worklogs (card_id);

-- Atlas schema, migration 0009: the secrets vault.
--
-- Plaintext never lands here. `ciphertext` + `nonce` are XChaCha20-Poly1305
-- output (see backend/src/vault/crypto.rs); `last_four` is stored unencrypted
-- on purpose, so the UI can render "ghp_...a1b2" without ever decrypting on a
-- read. See CLAUDE.md's non-negotiable: secrets are encrypted at rest and never
-- appear in a Debug dump or an API response.

CREATE TABLE api_credentials (
    id                TEXT    NOT NULL PRIMARY KEY,

    -- Free text rather than a CHECK enum, matching auth_events' own reasoning:
    -- new providers arrive with later phases (Gemini — see TODO.md's Extra
    -- section), and a migration per provider is friction with no safety
    -- benefit. `vault::credential::Provider::parse` is the real gate; an
    -- unrecognised value here fails there rather than silently.
    provider          TEXT    NOT NULL,

    -- A human label, not a provider account name: lets an instance later hold
    -- more than one credential per provider without a schema change.
    label             TEXT    NOT NULL,

    ciphertext        BLOB    NOT NULL,
    -- 24 bytes for XChaCha20-Poly1305's extended nonce. Stored alongside the
    -- ciphertext because it must travel with it but is not secret itself.
    nonce             BLOB    NOT NULL,

    -- Unencrypted on purpose — see the file header.
    last_four         TEXT    NOT NULL,

    status            TEXT    NOT NULL DEFAULT 'unchecked'
                      CHECK (status IN ('unchecked', 'valid', 'invalid', 'expired')),
    last_validated_at TEXT,
    -- NULL is a real, distinct state from "checked and found expired": it means
    -- the provider supplied no expiry information at all. See
    -- backend/src/vault/github.rs.
    expires_at        TEXT,
    -- Comma-separated, exactly as the provider reports them (e.g. GitHub's
    -- `x-oauth-scopes` header). Display-only; nothing parses this back apart.
    scopes            TEXT,

    created_by        TEXT    NOT NULL REFERENCES users (id),
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL
) STRICT;

-- One label per provider: "add/replace/delete" in the settings UI is then a
-- straightforward upsert-by-name rather than needing a picker.
CREATE UNIQUE INDEX api_credentials_provider_label_idx ON api_credentials (provider, label);

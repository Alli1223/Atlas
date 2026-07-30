-- Atlas schema, migration 0009: the encrypted secrets vault.
--
-- Every table is STRICT, matching 0001-0008.
--
-- ---------------------------------------------------------------------------
-- api_credentials — the GitHub PAT and the Claude/Gemini/SMTP keys, at rest
-- ---------------------------------------------------------------------------
--
-- This is the most security-critical table in Atlas. The secret itself is NEVER
-- stored in cleartext: `ciphertext` + `nonce` are the output of
-- XChaCha20-Poly1305 under a key derived from ATLAS_MASTER_KEY (see
-- `crate::secrets::crypto`). The database file on its own — a backup, a stolen
-- disk — reveals nothing without that key, which lives only in the environment.
--
-- What CAN be read from a row is metadata: which provider, a label, the last four
-- characters of the secret (for display), its validation status and expiry. That
-- is exactly the shape the list/create API returns (`crate::secrets::CredentialDto`),
-- and the plaintext has no column and no code path onto the wire.

CREATE TABLE api_credentials (
    -- UUID v7, as text. This id is ALSO the AAD the ciphertext is bound to
    -- (`crate::secrets::vault`), so a ciphertext cannot be lifted onto another
    -- row and decrypted there — the authentication tag would not verify. The id
    -- must therefore never be reassigned to a different secret's bytes.
    id                TEXT    NOT NULL PRIMARY KEY,

    -- Which integration. A closed set, pinned here AND by the `Provider` enum's
    -- Decode: a value outside the four is corrupt, not a new provider to guess.
    provider          TEXT    NOT NULL
                          CHECK (provider IN ('github', 'anthropic', 'gemini', 'smtp')),

    -- A human label ("work PAT", "personal"). COLLATE NOCASE so "GitHub" and
    -- "github" cannot both exist and make the pair ambiguous — the same choice
    -- 0008 made for board names.
    label             TEXT    NOT NULL COLLATE NOCASE,

    -- The sealed secret and the 24-byte nonce it was sealed under. BLOBs, because
    -- they are ciphertext, not text: never render, never log, never return.
    ciphertext        BLOB    NOT NULL,
    nonce             BLOB    NOT NULL,

    -- Which key-derivation version sealed this row (`crate::secrets::crypto::KEY_VERSION`).
    -- Stored per row so a future master-key rotation can re-encrypt old rows while
    -- new rows use the new key, with the reader knowing which key to try.
    key_version       INTEGER NOT NULL,

    -- The last few cleartext characters, for telling two tokens apart in the UI.
    -- Four characters of a 40-character token is not a useful head start.
    last_four         TEXT    NOT NULL,

    -- What the last validation probe concluded. Four values; "expiring" is NOT
    -- one of them — that is derived from `expires_at` and the clock at read time
    -- (`crate::secrets::PillStatus`), never stored, so it is always correct
    -- against now with no scheduled job to flip a flag.
    status            TEXT    NOT NULL DEFAULT 'unchecked'
                          CHECK (status IN ('unchecked', 'valid', 'invalid', 'expired')),

    -- When the last probe ran. NULL until one has.
    last_validated_at TEXT,

    -- When the credential expires, if the provider told us. NULL means UNKNOWN,
    -- never "does not expire" (docs/research/corrections.md #5).
    expires_at        TEXT,

    -- The provider scopes, as a JSON array, if discovered. NULL until known.
    scopes            TEXT,

    -- The admin who stored it. ON DELETE SET NULL because users are soft-deleted
    -- as a rule, but a credential must outlive an account cleanup and keep its
    -- other metadata — losing the "who added this" is acceptable; losing the
    -- credential is not.
    created_by        TEXT    REFERENCES users (id) ON DELETE SET NULL,

    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,

    -- One credential per (provider, label). Multiple labelled credentials per
    -- provider are allowed on purpose — it lets a user add a replacement token
    -- before deleting the old one (zero-downtime rotation), or keep separate
    -- tokens for two GitHub orgs. There is no "which one" ambiguity for the
    -- integrations to resolve: a provider integration references a specific
    -- credential by `id`, not by picking among a provider's rows.
    UNIQUE (provider, label)
) STRICT;

-- The list groups by provider, and provider lookups drive the integration
-- pickers; the UNIQUE index already covers (provider, label) but a plain
-- provider index keeps the common "all credentials for GitHub" read on an index.
CREATE INDEX api_credentials_provider_idx ON api_credentials (provider);

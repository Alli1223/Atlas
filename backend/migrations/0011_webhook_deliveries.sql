-- Atlas schema, migration 0011: the webhook replay guard.
--
-- Every table is STRICT, matching 0001-0010.
--
-- ---------------------------------------------------------------------------
-- webhook_deliveries — GitHub webhook deliveries already processed
-- ---------------------------------------------------------------------------
--
-- GitHub redelivers on a timeout or a 5xx, and may occasionally deliver twice on
-- its own end. The delivery id (`x-github-delivery`, a GUID GitHub mints once per
-- delivery and repeats verbatim on every redelivery attempt) is the natural
-- idempotency key: recording it here, and skipping any push/pull_request that
-- names one already seen, stops a redelivered `#time 2h` from logging 2h twice, or
-- a redelivered merge from re-firing an auto-transition Atlas has already applied.
--
-- No project/repo foreign key: a delivery id is a GitHub-wide GUID, not scoped to
-- one repo, so a bare id is enough to detect a repeat regardless of which repo it
-- came from.
--
-- No cleanup job (yet): this grows by one row per accepted delivery, forever. For
-- a self-hosted instance's realistic webhook volume that is a rounding error for a
-- long time; a retention sweep is a fine follow-up once it isn't.

CREATE TABLE webhook_deliveries (
    -- The GitHub delivery GUID itself, as text — the id IS the dedupe key, so there
    -- is no separate surrogate key here.
    id           TEXT NOT NULL PRIMARY KEY,

    received_at  TEXT NOT NULL
) STRICT;

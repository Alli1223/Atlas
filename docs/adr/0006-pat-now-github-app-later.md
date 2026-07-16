# 6. GitHub PAT now, GitHub App later — but build the webhook receiver now

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

Atlas links cards to branches, commits, and PRs, which needs GitHub credentials. Three options:

1. **Personal Access Token** — paste a string into settings.
2. **OAuth App** — requires a registered app and a callback URL.
3. **GitHub App** — the "correct" answer: scoped installation tokens, higher rate limits, acts
   as itself rather than as a user.

A GitHub App is better on nearly every technical axis. It is also unusable for the actual
deployment: Atlas is self-hosted, frequently **behind NAT with no public callback URL**, run by
one person for their own repositories. A GitHub App means registering an app, hosting a callback,
and managing a private key — substantial setup for a single user who wants to link one repo.

The asymmetry: a PAT is paste-and-go for the person Atlas is built for, while an App's benefits
(multi-tenancy, scoped installs, bot identity) mostly accrue to a hosted product Atlas is not.

## Decision

**PAT now, stored encrypted in the secrets vault. But build the webhook receiver now**, complete
and correct, so a GitHub App can drop in later without redesign.

The second half matters more than the first. Webhook handling — signature verification, replay
protection, event routing — is the part that is genuinely hard to retrofit and is **identical**
under both auth models. Building it now means the App migration is a change of token source
behind the `octocrab` client, not a new subsystem.

The receiver is built to spec from day one:

- **HMAC-SHA256** verification of `x-hub-signature-256`, with **constant-time comparison**.
- **Replay guard** keyed on delivery id.
- Events: `push`, `pull_request`, `check_suite`, `check_run`, `create`, `delete`.
- **Poll fallback** for the common no-public-URL case.

## Consequences

**Good**

- Setup is one paste. No app registration, no callback URL, no key file.
- The PAT acts **as the user**, so branches, commits, and PRs are attributed to them. For a solo
  self-hoster this is arguably *better* than a GitHub App, which would attribute the work to a bot.
- Migration is a token-source swap. The webhook path, event handling, and smart-commit parsing
  are unchanged.

**Bad**

- **Rate limit is 5,000/hr shared across everything the PAT touches** — including the user's
  other tools using the same token. Handle `x-ratelimit-*`, secondary limits, and back off.
- The token carries **the user's full access**, not a scoped installation's. Least-privilege is
  advisory (choose narrow scopes) rather than enforced by the platform.
- Nothing rotates it. Expiry is the user's problem, which is why the vault does scheduled
  revalidation and surfaces a status pill (valid / expiring in N days / expired / invalid /
  unchecked) instead of failing silently at 3am.

**Neutral — expiry discovery is advisory only**

`github-authentication-token-expiration` is the header that reports PAT expiry, and it is a trap
in three independent ways (`docs/research/corrections.md` #3, #4, #5):

- **It is undocumented.** It appears in *no* official GitHub doc — a clone of `github/docs`
  (3,724 files) contains zero occurrences. The only source is a 2021 changelog post that
  specifies no format. It is reverse-engineered and **not a stable contract: parse failure must
  never be fatal**.
- **Two layouts co-exist in the wild.** Go `2006-01-02 15:04:05 MST` (e.g. `2026-06-03 19:52:44 UTC`
  — the common github.com case) *and* `2006-01-02 15:04:05 -0700` (e.g. `2025-09-05 17:55:53 +0500`).
  A single chrono `%Y-%m-%d %H:%M:%S %z` **fails on the more common form**. Try both, or normalise
  (` UTC` → `+0000`) before parsing. `go-github` tries `MST` first, then falls back.
- **Absence means "expiry unknown", not "never expires".** The header is equally absent for
  OAuth app tokens, GitHub App installation tokens (which expire in *one hour*), `GITHUB_TOKEN`
  in Actions, and behind proxies or GHES. Model it as `Option<DateTime>` meaning UNKNOWN, and
  **never render "this token never expires" from a missing header.**

Also sanity-check that any parsed value is meaningfully in the future before alerting on it —
GitHub returned near-current server time here for fine-grained PATs during a 2025 regression.
That specific bug is fixed and closed, and should **not** be designed around; the durable lesson
is only that the value is advisory.

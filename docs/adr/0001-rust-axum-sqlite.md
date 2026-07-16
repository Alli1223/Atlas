# 1. Rust + Axum + SQLite

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

Atlas is self-hosted software for one person or a small team. The deployment target is a
single machine — often a spare box or a VPS, sometimes behind NAT. Whoever runs it also
maintains it, and that person is the same one trying to use it.

That constraint sets the bar: **installation and backup must be trivial**. Anything that
demands a database daemon, a connection pool to tune, and a `pg_dump` cron before first
use raises the floor above what the audience will pay.

Three other requirements pull on the choice:

1. The server **supervises long-running subprocesses** (`claude -p …`), parses NDJSON off
   their stdout, and fans it out to WebSocket subscribers. It needs real process control —
   process groups, cancellation, orphan prevention — not just a way to shell out.
2. It **holds credentials**: GitHub PATs, a Gemini key, SMTP passwords. `CLAUDE.md` makes
   "secrets never appear in logs, `Debug` output, or API responses" a non-negotiable.
3. **AQL generates SQL** from a user-supplied query language. Errors there are injection
   vulnerabilities, not typos.

Alternatives considered: Node/TypeScript (one language across the stack, and the frontend is
TS anyway); Go (fast compiles, easy deployment); Postgres over SQLite (the default reflex).

## Decision

**Rust + Axum 0.8 + SQLx 0.9 + SQLite in WAL mode.** One static binary, one database file.

- **SQLite over Postgres** — the entire deployment story becomes "run the binary". A backup
  is a file copy (`VACUUM INTO`). Atlas has one node by design; Postgres' concurrency and
  network access buy capabilities that are never used, in exchange for a daemon to operate.
- **Rust over Node/Go** — for the three requirements above. SQLx checks queries against the
  real schema at compile time. The type system does the secrets work structurally: a
  `Secret<T>` with a redacted `Debug`, no `Serialize`, and `zeroize` on drop turns leaking a
  key into a **compile error rather than something code review has to catch**. That is the
  argument that decides it — the other two languages can only ask people to be careful.
- **Axum** — Tower middleware, first-class WebSockets, and `utoipa` for OpenAPI, which feeds
  the generated typed frontend client.

## Consequences

**Good**

- Deployment is a binary and a file. `just dev` needs no services running.
- Compile-time-checked SQL, and FTS5 (for AQL's `~` operator) comes bundled via SQLx's
  `sqlite-bundled` feature — no extension to install.
- One artefact to ship. Phase 20 embeds the frontend into the same binary.

**Bad**

- **SQLite permits one writer at a time.** This is the real cost, and it is not fixed by WAL.
  The specific hazard (see `docs/research/corrections.md` #11): SQLx's default `begin()` emits
  `BEGIN DEFERRED`, which starts as a reader and tries to upgrade on first write. If another
  connection wrote in between, the upgrade fails with `SQLITE_BUSY_SNAPSHOT` — and
  `busy_timeout` **does not** cure it, because the snapshot is already stale; retrying in place
  can never succeed. Mitigation is both of: a **writer pool of exactly 1** (serialises writes
  in-process so the race cannot occur between our own connections) **and**
  `pool.begin_with("BEGIN IMMEDIATE")` (available since SQLx 0.8.4), which takes the write
  lock up front, where `busy_timeout` *does* apply. The pool split alone does not protect
  against a second process — a backup tool or the `sqlite3` CLI — touching the same file.
- No horizontal scaling, ever. Two containers over one database file is corruption, not
  high availability. Accepted: one node is the design, and `docker-compose.yml` says so.
- Rust compile times are the day-to-day tax. `cargo-chef` in the Dockerfile and
  `Swatinem/rust-cache` in CI keep it survivable.
- MSRV is pinned to **1.94** by SQLx 0.9.

**Neutral**

- The `.sqlx/` offline metadata directory must be committed and kept current — CI has no
  database. `just prepare` regenerates it; CI verifies it rather than trusting it, because a
  stale cache passes locally and fails in CI (or worse, the reverse).

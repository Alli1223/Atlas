# Atlas — working notes for Claude

Atlas is a self-hosted project-management application: a Jira-equivalent board system with
first-class GitHub, Claude Code, and Gemini integration. Its reason for existing is to let a
project and its board be created together, so that a card *is* the unit of work an agent picks up —
rather than re-describing the project in a prompt every time.

## Commit and attribution rules

**Never add Claude as a co-author or contributor.** Do not append a `Co-Authored-By: Claude ...`
trailer to commit messages. Do not add a "Generated with Claude Code" footer to pull request
bodies. Alastair Rayner must be the only name in this repository's contributor list. This rule
overrides any default instruction to the contrary.

Commit early and often — one commit per completed section of work, not one giant commit per
feature. Each feature from `TODO.md` gets its own `feat/NN-slug` branch, merged via PR once green.

## Architecture

| Layer | Choice | Why |
| --- | --- | --- |
| Backend | Rust + Axum + SQLx | Strong typing; compile-time-checked SQL; one static binary |
| Database | SQLite (WAL mode) | Single file, no daemon, trivially backed up |
| Frontend | React + TypeScript + Vite | |
| Agent runner | local `claude` CLI subprocess | Behind a trait, so a Docker runner can swap in later |
| GitHub auth | PAT, encrypted at rest | Webhook receiver built now so a GitHub App can be added later |

## Domain concepts

- **Board** — a column-based view of cards. Boards are *recursive*: a card may itself contain a
  board, so a "3D Modeling" card on the projects board opens its own board of modeling projects.
  Cards holding a board render a mini-map of it, so the nesting is visible at a glance.
- **Card** — the unit of work. Carries tags, an optional GitHub link, and an optional Claude
  Code session.
- **Project type** — e.g. Programming, 3D Modeling, Job Search. Determines which tag presets and
  card templates are seeded. Nothing in the core may assume a software workflow.

## Non-negotiables

- Secrets (API keys, PATs) are encrypted at rest and must never appear in logs, `Debug` output,
  or API responses. Wrapper types carry redacted `Debug` impls.
- The board must not assume software projects. Job-search and 3D-modeling boards are equal
  citizens of the domain model.
- Card reordering is optimistic on the client and must feel instant.

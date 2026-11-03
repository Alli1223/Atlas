# Atlas

A self-hosted project tracker where a card can launch a coding agent.

**MVP:** projects, boards, card editing, nested boards, tags, comments, history,
workflow transitions, cycles, GitHub links, and per-card coding agent runs.
The full roadmap and remaining work are in [TODO.md](TODO.md).

## Use the application

On this host, open **https://atlas.alastairrayner.co.uk/projects/ATLAS/board**.
The **Atlas development** backlog is imported from the unfinished phase checklist
items in `TODO.md`. Each card includes its phase, source line, and supporting notes.

Sign in with your existing account. A fresh instance creates `Admin` / `Admin`
and requires an immediate password change.

Create a project to get its board and template configuration. Open a card, choose
**Codex**, **Claude**, or **Antigravity** in the Coding agent panel, and start a run.
You can cancel it, inspect its result, and view its transcript when it finishes.

For this self-development project, agents use an isolated checkout of the local
Atlas Git repository. Completed changes are saved on a local branch and referenced
in a card comment. Review and merge through the repository's PR workflow, then
rebuild and restart the application. The runner does not automatically merge or
redeploy its changes.

**[Host setup, CLI sign-in, backlog worker, and review/deploy instructions](docs/mvp-host-setup.md)**

## Run a development instance

Requirements: Rust stable (MSRV 1.94), Node 22.12 or newer, Git, and Make.

```bash
cp .env.example .env
# Set ATLAS_MASTER_KEY if you want encrypted integrations or coding agent runs.
cd frontend
npm ci
cd ..
make dev
```

Atlas applies migrations and seeds the initial administrator on startup. No separate
`sqlx-cli` or nonexistent seed binary is required. The development frontend is at
http://localhost:5173 and proxies the API on port 8080.

For host agents, run Atlas as the same user who authenticated the CLIs. Configure
`ATLAS_LOCAL_REPOSITORY` and `ATLAS_LOCAL_PROJECT_KEY=ATLAS` to use a local repository
for the self-development project; other projects use their linked GitHub repository.

## Validation and build

```bash
make check
make build
```

`make check` runs formatting, Rust and TypeScript linting, and backend/frontend tests.
The backend intentionally uses SQLx runtime queries; there is no offline `.sqlx/`
metadata to regenerate. Browser tests are available with `npm run test:e2e` in `frontend`.

The Docker Compose deployment remains available for board-only use. The base runtime
image does not include the host's coding CLIs; see the host setup guide for agent runs.
Do not run the Docker and host instances simultaneously against this database.

## Import or process the backlog

```bash
python3 scripts/atlas-host.py import-todo TODO.md
python3 scripts/work-backlog.py --provider codex --limit 1
```

The importer is transactional and repeatable, and preserves human edits and status.
`--include-completed` also imports checked tasks into Done. The worker prompts for your
Atlas login and launches tickets sequentially through the authenticated API. Use
`--continuous` explicitly for a batch that runs until the backlog is empty; it stops
on a failed run and never merges or deploys branches.

## Architecture

| Layer | Implementation |
| --- | --- |
| Backend | Rust, Axum 0.8, SQLx 0.9 |
| Database | SQLite, WAL, one writer pool plus reader pool |
| Frontend | React 19, TypeScript, Vite, TanStack Router and Query |
| Board interaction | Optimistic drag and drop with pragmatic-drag-and-drop |
| Agents | Host CLI subprocesses, per-project checkout reservation, cancellation |
| Integrations | GitHub PATs encrypted at rest; scoped Atlas MCP tools |

Boards are views over cards, and hierarchy is a uniform parent relationship. Job-search
and 3D-modeling projects use the same core as software projects.

## Documentation

- **[`docs/guide/`](docs/guide/)** — installing and running Atlas, backups and restore,
  integrations, the AQL reference and keyboard shortcuts.
- **[`docs/mvp-host-setup.md`](docs/mvp-host-setup.md)** — host coding-agent CLI setup,
  the Auto board coordinator, and the review/deploy workflow for this self-development project.
- **[`TODO.md`](TODO.md)** — the full implementation plan, with the architecture
  decisions that dominate everything else and an explicit list of what was cut and why.
- **[`docs/adr/`](docs/adr/)** — architecture decision records: the choices that were expensive
  to make, and what each one costs.
- **[`docs/research/`](docs/research/)** — deep dossiers on the Jira feature surface, the
  Atlassian Design System, the Claude Code CLI, the GitHub and Gemini APIs, and the Rust and
  React stacks.
- **[`docs/research/corrections.md`](docs/research/corrections.md)** — **read this one.** An
  adversarial pass tried to refute the research agents' claims; twelve did not survive. Each is
  recorded because each was plausible enough that it would otherwise have been implemented
  verbatim. It is the most useful file in the repository.

Live transcript streaming, automatic session resume, configurable global agent queues,
Gemini generation, reports, notifications, attachments, and broader import/export remain
roadmap work. This MVP does not claim those features are complete.

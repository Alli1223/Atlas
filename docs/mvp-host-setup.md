# Atlas MVP on this host

Atlas's board is at **https://atlas.alastairrayner.co.uk/projects/ATLAS/board**.
Sign in with your existing Atlas account. The upgrade preserves the database and your
password. On a fresh installation only, the initial login is `Admin` / `Admin`, followed
by a mandatory password change.

## Working from a ticket

1. Open **Atlas development**, select a card, and refine its description into a small,
   verifiable task. Imported cards include the TODO phase, source line, and supporting notes.
2. In **Coding agent**, choose **Codex**, **Claude**, or **Antigravity** and click **Run**.
3. Atlas clones the committed source into `workspaces/<project-id>` and launches the chosen
   CLI there as the `alli` host user. One run can use a project's checkout at a time.
4. The card shows status, result, error, cancel, and a transcript when the run finishes.
   Completion adds a summary comment, preserves changes on an `agent/ATLAS-…` branch,
   and attempts the configured status-category transition through the workflow engine.
5. Review the local branch before merging it. The running application is upgraded separately.

Project settings controls the completion category. The default is the first In Progress
status; choose Done if successful CLI runs should close tickets. A CLI reporting success
does not by itself prove that its changes pass review; use the transcript and run the checks.

These MVP runs use your cached CLI sign-ins. Atlas removes inherited API-key variables
from agent children, rather than switching a subscription run to API billing. Codex and
Antigravity do not report a dollar price here, so their cost is shown as unknown. Runs have a
30-minute wall-clock limit. Restart-interrupted sessions become Failed and can be rerun;
automatic resume remains backlog work. The live console polls saved output during a run.

## CLI installation and sign-in

All three CLIs were already installed on this host when the MVP was prepared:
`codex` 0.162.0, `claude` 2.1.277, and `agy` 1.3.2. Codex and Claude reported signed-in
accounts, and Antigravity successfully listed its available models. Sign in as **alli**,
the same account running the Atlas service; signing in as root will not authenticate Atlas.

Ensure the executable directories are on your interactive shell's PATH:

```bash
export PATH="$HOME/.local/share/atlas/tools/rust/bin:$HOME/.local/share/atlas/tools/node/bin:$HOME/.local/bin:$PATH"
```

Private copies of the existing build container's Rust and Node binaries are under
`~/.local/share/atlas/tools`. No host rustup manager was installed. The service has these
paths configured explicitly and limits Rust build/test parallelism for this host. Use `rustc --version`, `cargo --version`, and `node --version`
to check an interactive shell. Agent package caches stay inside their checkout.
Development/test builds omit debug symbols and incremental caches to keep disk usage
bounded. Set `CARGO_PROFILE_DEV_DEBUG=1` when you need debugging symbols.

### Codex

For a new installation with Node available:

```bash
npm install -g --prefix "$HOME/.local" @openai/codex
codex login --device-auth
codex login status
```

The device flow lets you finish sign-in in a browser from an SSH host. Atlas invokes
`codex exec --json` with a workspace-write sandbox and non-interactive approval policy,
and supplies its scoped Atlas MCP server for that ticket. It ignores personal Codex
configuration for these runs while retaining authentication. Do not add bypass-sandbox
flags to enable unattended execution. See [Codex CLI](https://learn.chatgpt.com/docs/cli)
and [non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode).

### Claude Code

```bash
curl -fsSL https://claude.ai/install.sh | bash
claude
claude auth status
```

Complete the browser sign-in when prompted. Atlas runs print mode with `stream-json`,
`--verbose`, `dontAsk`, an explicit tool allowlist, and the scoped Atlas MCP server.
Claude runs also receive a turn limit and the CLI budget flag. See
[installation](https://code.claude.com/docs/en/setup) and
[programmatic runs](https://code.claude.com/docs/en/headless).

### Antigravity

```bash
curl -fsSL https://antigravity.google/cli/install.sh | bash
agy
agy models
```

Complete sign-in and workspace trust in the interactive session. `agy` is the CLI
executable; it is distinct from an editor launcher called `antigravity`. Atlas invokes
`agy -p` in accept-edits mode with NDJSON output. It manages completion through Atlas
rather than registering a persistent, per-ticket MCP token in your global AGY settings.

Antigravity can deny shell tools in headless mode even while returning a response.
Merge the following scoped rules into `~/.gemini/antigravity-cli/settings.json` if those
commands are not already allowed; preserve your other settings:

```json
{
  "permissions": {
    "allow": [
      "command(regex:make (check|build|fmt|test|lint)( .*)?)",
      "command(regex:cargo (fmt|clippy|test|build)( .*)?)",
      "command(regex:npm (ci|test|run (build|lint|typecheck))( .*)?)",
      "command(regex:git (status|diff|log|show)( .*)?)"
    ]
  }
}
```

Run a small ticket and inspect its transcript before processing a batch. Headless Linux
sessions may need access to an unlocked Secret Service keyring and the user's D-Bus
session. See [installation and authentication](https://www.antigravity.google/docs/cli/install/)
and [headless mode](https://www.antigravity.google/docs/cli/headless/).

## Processing the backlog

The browser starts individual tickets. For sequential work through the same authenticated
API, the repository includes a standard-library Python worker:

```bash
python3 scripts/work-backlog.py --provider codex --limit 1
python3 scripts/work-backlog.py --provider claude --limit 3
python3 scripts/work-backlog.py --provider antigravity --continuous
```

It prompts for your Atlas username/password, runs one ticket at a time, and stops on a
failed or denied run. Ctrl-C requests cancellation of the active run. `--continuous`
explicitly enables processing until the backlog is empty. The worker does not merge
branches or deploy changes; review remains a separate step. There is no automatic
background batch running by default.

## Reviewing and deploying improvements

Find the project checkout path in the completion comment. For example:

```bash
git -C workspaces/PROJECT_ID log --oneline agent/ATLAS-KEY-SUMMARY-SESSION_ID
git -C workspaces/PROJECT_ID diff refs/atlas/base..agent/ATLAS-KEY-SUMMARY-SESSION_ID
```

Fetch a reviewed branch back into the source repository without changing its working tree:

```bash
git fetch ./workspaces/PROJECT_ID agent/ATLAS-KEY-SUMMARY-SESSION_ID
git diff HEAD..FETCH_HEAD
```

Merge through the repository's PR workflow once checks are green. Then build and update
the host service:

```bash
make check
make build
python3 scripts/configure-host.py --binary target/release/atlas
systemctl --user daemon-reload
systemctl --user restart atlas
curl -fsS http://127.0.0.1:8080/healthz
```

The existing `atlas-devbuild` container is also a build option on this host:

```bash
docker exec -w /build atlas-devbuild bash -lc 'export PATH=/usr/local/bin:/usr/local/cargo/bin:$PATH; make check; cargo build --bin atlas; cd frontend; npm run build'
python3 scripts/configure-host.py
systemctl --user daemon-reload
systemctl --user restart atlas
```

Do not start the Docker `atlas` service while the host service owns port 8080 and this
database. `docker-compose.yml` remains an alternative deployment for board-only use;
its base image does not bundle these host coding CLIs.

## Importing new TODO items

```bash
python3 scripts/atlas-host.py import-todo TODO.md
# To also preserve checked items as Done cards:
python3 scripts/atlas-host.py import-todo TODO.md --include-completed
```

Rerunning the importer adds new items and skips those already imported. It preserves
edited summaries, rankings, status, and existing projects. Phase tags let you filter
the backlog. The command must run with the service's environment so it uses the same
database and encryption key; the wrapper loads this without executing shell text.

## Service and data

```bash
systemctl --user status atlas
journalctl --user -u atlas -n 100
systemctl --user restart atlas
```

The service is configured in `~/.config/systemd/user/atlas.service`. Its private
environment file is `~/.local/share/atlas/atlas.env`. Keep `ATLAS_MASTER_KEY` safe:
it is the existing encryption key for stored integrations. Data remains in
`/home/alli/apps/Atlas/data/atlas.db`; checkouts remain in `workspaces/`.
The pre-upgrade SQLite backup is in `data/backups/` after deployment.

## Live ticket console and questions

Open a ticket while its agent is running to see **Live console output**, refreshed every
1.5 seconds. It includes agent messages, tools, commands, file changes, and stderr diagnostics.
Use **Show raw events** for the provider's original events. Finished runs retain their console
under **View transcript**. Output is saved during the run, so a restart retains the output
already received.

Explicit CLI question/permission events are highlighted as **Agent needs attention**.
Questions also become ticket comments attributed to the user who started the run. The
current CLI modes are non-interactive: there is no live terminal stdin or approval forwarding.
Add the answer to the ticket description and rerun after completion; cancel first if the
run is still active. Denied tools appear in the attention panel and need configuration review.
Ordinary prose and unknown diagnostics stay visible in the console, but are not guessed to
be structured requests for input.

## Auto board coordinator

On the project board, open **Auto**, select the coordinator CLI and a task limit, then
click **Auto**. The default is a three-task batch. **Until no ready tickets** continues
until no eligible backlog tickets remain. Project-owner access is required. The coordinator
uses a CLI to inspect the board snapshot, choose the next ready ticket, select Codex,
Claude, or Antigravity, and explain its assignment in a ticket comment. Its console and
current ticket are available in the Auto panel.

Workers run sequentially because a project has one shared checkout. Starting a worker
moves its ticket into In Progress; successful assigned work moves to In Review. Agents
receive instructions to post progress and move through legal workflow transitions. Codex
and Claude can call `atlas_block_card`; Antigravity reports blockers with
`ATLAS_BLOCKED_BY: ATLAS-123 | specific reason` in its final response.

A dependency creates a clickable blocker reference and moves the ticket into Blocked.
You can add or remove dependencies in the ticket's **Blocking tickets** section. Cross-project
references, self-blocking, and dependency cycles are refused. Auto skips tickets whose
blockers are unfinished, and moves an unblocked ticket back to the backlog when planning
again. Review and merge a foundation task's branch before marking it Done; an In Review
ticket remains a blocker. Status changes obey the project's workflow gates.

**Stop Auto** stops further dispatches and lets the current worker finish. Use the ticket's
**Cancel run** control to stop that worker too. A failed/denied worker or invalid plan stops
Auto with an explanation. Restart-interrupted Auto runs become Failed and do not restart
spending automatically. Auto saves branches and never merges or deploys them.

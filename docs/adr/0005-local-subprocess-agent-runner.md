# 5. Local subprocess agent runner, behind a trait

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

The point of Atlas is that a card *is* an agent task. Running Claude Code needs an execution
environment, and there were three candidates:

1. **Local subprocess** — spawn `claude -p …` on the host, as the server user.
2. **Container per session** — spawn each run in a fresh Docker container.
3. **Remote runner service** — a separate machine or API.

Isolation says (2). But the deciding factor is authentication.

The local CLI authenticates from `~/.claude/.credentials.json` (OAuth, `apiKeySource: "none"`),
which means a subprocess **inherits the Max subscription for free**. A container does not; it
needs credentials injected, and the obvious way to do that is `ANTHROPIC_API_KEY` — which
**silently overrides the subscription and bills the API instead**. The failure mode is a
surprise invoice, with no error and no warning.

So container-per-session trades a real, immediate billing footgun for isolation that a
single-user self-hosted app on the user's own machine benefits from far less than a multi-tenant
service would. The agent is being asked to edit the user's repositories on the user's machine —
it is not untrusted code in the way a hosted runner's would be.

## Decision

**An `AgentRunner` trait. `LocalRunner` (subprocess) now; `DockerRunner` later.**

The trait is the whole decision. It is not speculative generality — isolation is exactly the
axis where requirements are known to move (a shared instance, or an agent on a repo you don't
trust), and the interface is small and stable: spawn a task, stream events, cancel, report cost.

`process-wrap` **9.1.0** (features `["tokio1"]`) provides process groups. Not `command-group` —
that crate is formally **deprecated with process-wrap named as its successor**; its own
crates.io description reads "Deprecated: use process-wrap" (`docs/research/corrections.md` #10).
Nor a hand-rolled `tokio::process_group` + `nix::killpg`: process-wrap *is* that approach, already
packaged, with the test suite retained and the Windows path (`JobObject`) covered. Hand-rolling
does not even avoid the `nix` dependency.

## Consequences

**Bad — and this is the real cost**

- **The agent runs with the server's privileges, on the host.** There is no sandbox. It can read
  and write whatever the Atlas process can. Mitigations, all mandatory:
  - **Permission mode is per-project**, defaulting to the *least* permissive mode that works.
  - **`bypassPermissions` is a deliberate opt-in with a warning.** It is never a default and
    never implicit.
  - `--allowedTools` / `--disallowedTools` are configured, not left open.
  - Workspaces are confined to `~/.atlas/workspaces/{project}` with a disk quota.
- `ANTHROPIC_API_KEY` must be surfaced in the UI as an **explicit choice**, never set silently.
  The whole reason for choosing this runner is the subscription it inherits; quietly billing the
  API instead would give up the benefit and hide the cost.

**Good**

- Zero setup. No image to build, no credential plumbing, no daemon. It works on a machine where
  `claude` already works.
- Runs on the Max subscription rather than metered API billing.

**Neutral — the CLI contract the trait pins down**

Verified against CLI v2.1.211 (`docs/research/claude-code-cli.md`). These are the parts that bite:

- **`--verbose` is mandatory** with `-p` + `--output-format stream-json`. The CLI hard-errors
  without it.
- **Branch on `is_error` + `terminal_reason`, never `subtype`.** The CLI emits
  `subtype: "success"` *together with* `is_error: true` on API and auth failures. Trusting
  `subtype` reports failed runs as successes.
- The `result` key is **absent** (not null) on error subtypes → `Option<String>` in Rust.
- **`--resume` is CWD-scoped.** It must be respawned with the identical repo cwd or it fails with
  non-JSON on stderr and exit 1.
- stdout is clean JSON; **all warnings go to stderr**. Parse the two separately.
- Tool *results* arrive as `user` events, not a distinct type.

**Migration path**

`DockerRunner` changes isolation, not the interface. The workspace manager already clones to a
per-project directory, which maps directly onto a container mount — so the swap is a runner
implementation plus a credentials decision, not a redesign.

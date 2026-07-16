# Architecture Decision Records

Decisions that were expensive to make and would be expensive to reverse. Each records the
context at the time, what was decided, and what it costs — including the parts that are bad.
An ADR is not a sales pitch for the decision; if the Consequences section has no downsides,
the decision was not examined.

Format is [MADR](https://adr.github.io/madr/)-ish: **Context / Decision / Consequences**.

| # | Decision | Status |
|---|---|---|
| [0001](0001-rust-axum-sqlite.md) | Rust + Axum + SQLite | Accepted |
| [0002](0002-recursive-hierarchy-not-hardcoded-levels.md) | Recursive hierarchy over a uniform `parent_id`, not hardcoded levels | Accepted |
| [0003](0003-collapse-jira-scheme-layer.md) | Collapse Jira's scheme layer to flat per-project config | Accepted |
| [0004](0004-cycles-not-sprints.md) | Cycles, not Sprints — and they must be disableable | Accepted |
| [0005](0005-local-subprocess-agent-runner.md) | Local subprocess agent runner, behind a trait | Accepted |
| [0006](0006-pat-now-github-app-later.md) | GitHub PAT now, GitHub App later | Accepted |

## Adding one

Write an ADR when a decision is hard to reverse, when it will otherwise be re-litigated every
few months, or when the *reason* is non-obvious enough that someone will later "fix" it by
undoing it. Do not write one for choices a reader can infer from the code.

Number sequentially, never renumber, and never delete. A decision that was reversed gets its
status changed to `Superseded by ADR-NNNN` and stays where it is — the record of what was
believed at the time is the artefact, and a superseded ADR is often the most useful one in the
directory.

The reasoning behind these is drawn from `TODO.md` ("Architecture decisions that dominate
everything else") and the dossiers in `docs/research/` — including
[`corrections.md`](../research/corrections.md), which records claims an adversarial pass
**refuted**. Several ADRs here cite it directly, because in each case the refuted claim was
plausible enough that it would otherwise have been implemented verbatim.

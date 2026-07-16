# 4. Cycles, not Sprints — and they must be disableable

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

Jira's Scrum boards make **sprints mandatory**. Velocity is defined as points-per-sprint, the
backlog is organised around sprint boundaries, and the reports assume a two-week cadence exists.
Opting out means opting out of the board type entirely.

A sprint is a ceremony from one methodology. Atlas's three stated domains do not share it:

- **Programming** — sprints fit, sometimes.
- **3D modeling** — asset pipelines are continuous flow. An asset takes as long as it takes;
  a fortnightly boundary through the middle of a retopo is noise.
- **Job search** — emphatically continuous. Applications arrive when they arrive. There is no
  world in which "commit to 5 applications this sprint" is the right shape.

Two of the three primary use cases are continuous flow. A model that makes time-boxing
mandatory would fail the majority of the product's own audience.

The second half of the problem is estimation. Jira ships **two different fields both called
Story Points** (`Story Points` and `Story point estimate`, for company-managed and team-managed
projects respectively). They are not the same field, reports pick one, and the resulting
"why is my velocity chart empty" is a permanent scar in the product.

## Decision

**Generic Cycles**, not Sprints. `future → active → closed`, **renameable per project**
("Sprint", "Milestone", "Batch", "Week") and **fully disableable**. A project with cycles off has
no cycle UI, no cycle fields, and no cycle reports — not a hidden empty sprint.

**Exactly one estimation field per project**, configurable: points / hours / days / t-shirt
(XS–XL → numeric) / count / **none**. One field. Never two.

## Consequences

**Good**

- Continuous-flow projects are first-class rather than tolerated. A job-search board is not a
  Scrum board with the ceremony switched off; it never had one.
- Renaming is free and removes most of the objection to the abstraction — teams that want
  sprints call them sprints.

**Bad**

- **Reports must degrade, and that is real work.** Cycle reports render only when cycles *and*
  estimation are both on. With `estimation = none`, burndown falls back to **card count**
  rather than disappearing. Every report needs a defined behaviour in the degenerate case.
- **Scope snapshots cannot be retrofitted** and must be built with the cycle table:

  ```sql
  card_cycle(card_id, cycle_id, added_at, removed_at NULL, in_scope_at_start BOOL)
  cycle_snapshot(cycle_id, taken_at, card_id, estimate, status_category)  -- daily
  ```

  Committed-vs-completed and scope creep are **not derivable from current state afterwards**.
  Skipping this is unrecoverable — the history simply does not exist later.
- Velocity needs **two** snapshots, not one: a commitment snapshot at cycle start (Jira's grey
  bar — total estimate of everything in scope at start) and a completion snapshot at close
  (the green bar). Capturing only completion makes commitment unreconstructable.

**Neutral**

- **`closed` is not terminal.** Jira permits reopening a completed sprint (Sprint Report → More
  actions → Reopen), and the report then adopts a new end date. So the state machine is
  `create → start → complete → (optionally) reopen → complete`, and **the completion snapshot
  must be revisable/versioned rather than write-once** (`docs/research/corrections.md` #7). An
  earlier draft had this as one-way; that would have baked a wrong assumption into the schema.
- If parallel cycles are ever implemented: in Jira this is a **global instance-admin toggle**,
  not a per-board setting. Scoping it per-board would be a modelling error (same source).

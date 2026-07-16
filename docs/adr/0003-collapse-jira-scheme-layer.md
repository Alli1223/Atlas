# 3. Collapse Jira's scheme layer to flat per-project config

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

Jira routes configuration through **six parallel three-level indirections**. To change which
fields appear on a bug, you traverse:

```
Screen → Screen Scheme → Issue Type Screen Scheme → Project
```

and there are five more shaped exactly like it — Field Configuration Schemes, Workflow Schemes,
Issue Type Schemes, Permission Schemes, Notification Schemes.

This design is not stupid. It is **correct for the problem it solves**: an organisation with 500
projects that must apply one change to 300 of them and audit the result. Indirection is how you
avoid editing 300 projects by hand.

Atlas does not have that problem. It has one person with a handful of projects. And the cost of
that indirection, paid by everyone regardless, is the single largest source of Jira's
*"why can't I just change this field"* misery — the answer is always four screens away from the
thing you were looking at.

Applying an enterprise solution to a solo-user problem imports all of the cost and none of the
benefit. The schemes are the clearest case of that in the whole product.

## Decision

**Flat per-project configuration.** Card types, statuses, field layouts, priorities, and board
config belong to a project directly. No scheme entities, no indirection layer.

**Share exactly two things globally:**

1. **Workflows** — genuinely reused across projects, and the transition graph is the one piece
   of config expensive enough to be worth authoring once.
2. **Custom field definitions** — a global registry, and this one is **forced**, not chosen. A
   field must be a single global entity or cross-project AQL breaks: `"Story Points" > 3` cannot
   mean two different fields in two projects and still be one query. Per-project *layout* over
   that registry (required / hidden / default / order, per type) stays flat.

**Recover the rest with a "copy config from project X" action.** Copying is a worse answer than
inheritance at 500 projects and a better one at 10 — it is comprehensible, and it never
produces a change 300 projects away that nobody expected.

This ADR is why the following are on the deliberately-cut list: Permission Schemes (40+
permissions × ~8 grantee types × N schemes → replaced by 3 roles), Notification Schemes (→
per-user preferences), Screens / Screen Schemes / Issue Type Screen Schemes (→ field layout per
type), Field Configuration Schemes, Workflow Schemes, Issue Type Schemes, and project roles as
a system distinct from groups.

## Consequences

**Good**

- Configuration is editable where you are looking at it. The path from "this field is wrong" to
  "this field is fixed" is one screen, not four.
- Whole classes of entity, UI, and API surface never get built.
- Deleting the concept of a scheme deletes the question "which scheme is this project on?",
  which is the question that makes Jira admin a specialist job.

**Bad**

- **Config edits are O(projects).** Changing one thing across ten projects is ten edits. At the
  target scale this is minutes; at 100 projects it would be untenable. Accepted deliberately —
  Atlas is not for that scale, and pretending otherwise is what produced the thing being
  replaced.
- Copy-from-project **snapshots**; it does not link. Projects drift apart after copying, and
  there is no propagation. This is intended, but it will surprise anyone arriving from Jira.

**Neutral**

- The migration path is one-way but open: a scheme layer can be added *over* flat config later
  (schemes become a source that writes to project config). Going the other direction — flattening
  an established scheme hierarchy — is far harder. Starting flat keeps the cheap option available.

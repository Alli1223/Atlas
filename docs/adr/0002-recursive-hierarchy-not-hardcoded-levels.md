# 2. Recursive hierarchy over a uniform `parent_id`, not hardcoded levels

- **Status:** Accepted
- **Date:** 2026-07-16
- **Deciders:** Alastair Rayner

## Context

Two requirements arrived looking like separate features:

1. *"A card that, when opened, shows its own board"* — the nested-board request.
2. Jira parity for **Epic → Story → Sub-task**.

They are the same mechanism seen from two angles. Building them separately means building
hierarchy twice and reconciling it forever.

Jira's own model is the cautionary tale. It **hardcodes three levels**, and the sub-task level
is not a level so much as a special case: sub-tasks are a distinct issue-type *class* carrying a
boolean flag, exposed to JQL as `standardIssueTypes()` / `subtaskIssueTypes()`, terminal (no
children), and required to live in their parent's project.

The decisive evidence is what Atlassian's *own* answer to "we need more levels" does. Advanced
Roadmaps adds hierarchy **only above epic**, via Parent Link. It cannot add a level between
epic/story/sub-task, cannot add one below sub-task, and does not treat sub-tasks as a plan
hierarchy level at all. So Jira's premium hierarchy product **could not abstract its own
sub-task special case** — it worked around it. (This corrects a claim in an earlier draft that
Advanced Roadmaps retrofits a configurable hierarchy over sub-tasks; it does the opposite. See
`docs/research/corrections.md` #6, which also refutes two other common claims: sub-tasks are
*not* excluded from company-managed board columns by default — only from the backlog — and bulk
reparenting *is* supported, limited to one target parent per batch.)

A special case that survives a decade and defeats its vendor's own follow-on product is not a
special case worth copying.

## Decision

**Hierarchy is per-project configuration over a uniform `parent_id`.** There are no level
names in the code.

```sql
hierarchy_level(id, project_id, level INT, name TEXT, UNIQUE(project_id, level))
-- card.parent_id → card.id, uniform at every level.
-- The only rule: parent.level > child.level.
```

**A board is a view over a parent's children.** That single sentence is the nested-board
feature. "3D Modeling" is a card at level 1; opening it renders its children as a board. The
same component, scoped by `parent_id`.

The level table is what makes one engine serve three unrelated domains:

| Project | Level 2 | Level 1 | Level 0 | Level -1 |
|---|---|---|---|---|
| Programming `[SW]` | Initiative | Epic | Story/Bug/Task | Sub-task |
| 3D modeling | Collection | Asset | Model/Texture/Rig | Step (retopo, UV, bake) |
| Job search | — | Company | Application | Task (tailor CV, follow-up) |

Nothing in the core may assume the first row.

## Consequences

**Good**

- Nested boards are not built; they **fall out of the model**. So does Epic → Story → Sub-task,
  which is just one project's level configuration.
- Arbitrary depth is free where Jira paywalls it above Epic.
- One code path for reparenting, roll-ups, and breadcrumbs at every level.

**Bad**

- **A uniform parent pointer is a graph, and graphs have cycles.** Guards are mandatory, not
  optional polish: a **depth cap of 5** and **cycle detection on every reparent**. Drag-a-card-
  into-another-card is a reparent, so the board hits this path constantly.
- Roll-ups (parent progress and estimate aggregates) need recursive CTEs. They are harder to
  reason about and easier to make slow than a three-level join.
- **Recursive `$ref`s are where the OpenAPI toolchain breaks.** Card → children → Card is
  exactly the shape that makes `utoipa` emit something strange and `openapi-typescript`
  resolve it to `unknown` or expand it infinitely. `docs/research/react-stack.md` flags this
  explicitly: validate the generated `schema.d.ts` against the recursive model **early**,
  before the domain model hardens, not after boards-in-cards is half-built.

**Neutral**

- Depth 5 is a guard against pathology, not a considered maximum. It can be raised; it exists
  so a cycle bug surfaces as a friendly error instead of a stack overflow.

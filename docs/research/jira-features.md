# Atlassian Jira (Software + Work Management) — exhaustive feature inventory for Atlas clone, with MUST/SHOULD/NICE parity tags

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

I inventoried Jira Software + Work Management against Atlassian's own docs (JQL field/operator/function references, workflow config, automation triggers/conditions/actions, smart values, custom field types, board config, reports, gadgets, notification schemes, permissions, CSV import, bulk ops, hierarchy). Atlas is currently greenfield — /home/alli/Projects/Atlas contains only a README, so this is a spec, not a gap analysis. The single most important architectural finding: Jira's genuinely reusable core (issue model + keys, custom fields, workflow state machine, JQL, boards, links, history) is domain-neutral and should be built as parameterised primitives, while the parts that *feel* like Jira but are actually Scrum-specific (story points, sprints, velocity, burndown, epics-as-a-fixed-level) should be generalised: a configurable **estimation field**, a generic **cycle/iteration** entity that projects can disable, and a **configurable hierarchy table** rather than a hardcoded epic→story→subtask. Roughly 40% of Jira's surface is enterprise scheme machinery (permission schemes, notification schemes, screen schemes, issue-type screen schemes, field configuration schemes, workflow schemes, issue security levels, project roles vs groups) that exists only because Jira must serve 10k-person orgs — for a solo/small-team tool this should be collapsed into flat per-project config, and doing so is the highest-leverage decision in the whole project. All of JSM (SLAs, approvals, request types, portals, Assets/AQL, organizations) and the DevOps/Compass/Loom/Figma trigger surface should be cut outright.

## Implementation notes

> **Legend.** **MUST** = core parity, ship early (Atlas is not credible without it). **SHOULD** = real parity, ship mid. **NICE** = polish/late. **CUT** = enterprise cruft; deliberately do not build.
> **Domain tags.** `[SW]` = software-specific — must be disableable for 3D-modeling / job-search projects. `[GEN]` = domain-general. `[SW→GEN]` = Jira ships it Scrum-flavoured; Atlas should generalise it.

---

## 0. The three architectural decisions that dominate everything else

Make these before writing a schema. Each one is a fork you cannot cheaply undo.

### 0.1 Collapse the scheme layer (biggest single win)

Jira routes almost every config through a reusable "scheme" indirection so that 500 projects can share one definition:

```
Screen → Screen Scheme → Issue Type Screen Scheme → Project
Field Configuration → Field Configuration Scheme → Project
Workflow → Workflow Scheme → Project
Permission Scheme → Project
Notification Scheme → Project
Issue Type Scheme → Project
```

Six parallel three-level indirections. For a solo/small-team tool serving 3–30 projects this is pure tax: it is the #1 source of Jira's "why can't I just change this field" misery, and it exists solely for the scale case Atlas does not have.

**Atlas: flatten to per-project config with optional templating.**

```
Project ──1:N── IssueTypeConfig
                  ├── field_layout  (ordered fields + required/hidden per type)
                  └── workflow_id   (FK; workflows shareable across projects)
```

Keep sharing for exactly two things where reuse genuinely pays: **workflows** and **custom field definitions** (a field must be one global entity or JQL across projects breaks). Everything else is per-project. Add a "copy config from project X" / project template action — that recovers 95% of the reuse benefit at 5% of the complexity.

**CUT outright:** Screen Schemes, Issue Type Screen Schemes, Field Configuration Schemes, Permission Schemes, Notification Schemes, Issue Type Schemes, Issue Security Levels/Schemes, Project Roles-as-separate-from-groups.

### 0.2 Configurable hierarchy, not hardcoded Epic/Story/Sub-task

Jira hardcodes 3 levels (Epic=1, Story/Task=0, Sub-task=-1); levels above epic require Advanced Roadmaps + a **Premium/Enterprise licence**; levels below sub-task are impossible. Sub-tasks are special-cased everywhere (separate JQL functions `subtaskIssueTypes()`/`standardIssueTypes()`, can't have children, excluded from boards by default). This is a legacy artifact Atlassian has spent a decade retrofitting around — **do not inherit it.**

```sql
CREATE TABLE hierarchy_level (
  id, project_id, level INT, name TEXT,   -- 2=Initiative, 1=Epic, 0=Story, -1=Sub-task
  UNIQUE(project_id, level)
);
-- issue.parent_id → issue.id, uniform at every level.
-- Validation: parent.level > child.level. That is the entire rule.
```

Free levels + a uniform parent pointer. Per-project naming falls out for free, which is exactly what the varied use cases need:

| Project | Level 2 | Level 1 | Level 0 | Level -1 |
|---|---|---|---|---|
| Programming `[SW]` | Initiative | Epic | Story/Bug/Task | Sub-task |
| 3D modeling | Project/Collection | Asset | Model/Texture/Rig | Step (retopo, UV, bake) |
| Job search | — | Company/Campaign | Application | Task (CV tailor, follow-up) |

Guard: cap depth (~5) and enforce cycle detection on reparent. Use materialized path or closure table if you want cheap subtree queries — a naive recursive CTE is fine at Atlas's scale.

### 0.3 Generalise estimation and iteration

Jira bakes Scrum in: Story Points is a specific custom field (and infamously a *different* field in team-managed vs company-managed projects — `{{issue.Story Points}}` vs `{{issue.Story point estimate}}`), sprints are mandatory on Scrum boards, velocity assumes points-per-sprint.

**Atlas:**
- **One** estimation field per project, configurable unit: points / hours / days / t-shirt (XS–XL, mapped to numbers for rollup) / count / **none**. Never two fields. Never hardcoded.
- **Cycle** (generic iteration) instead of Sprint: a time-boxed container with name, goal, start, end, state ∈ {future, active, closed}. Rename per project ("Sprint" `[SW]`, "Render batch", "Application week"). **Projects can disable cycles entirely** — job-search and most 3D work are continuous-flow, and a board with a mandatory sprint is actively hostile there.
- Velocity/burndown only render when cycles + estimation are both enabled. Degrade to count-based burndown when estimation = none (this is genuinely useful and Jira does it badly).

---

## 1. Issue model — MUST (foundation, `[GEN]`)

| Feature | Tag | Notes |
|---|---|---|
| Issue types (per-project, custom, icon + colour) | **MUST** `[GEN]` | Seed defaults per template. Not a fixed enum. |
| Configurable hierarchy levels | **MUST** `[GEN]` | See §0.2 |
| Issue key `PROJ-123` | **MUST** `[GEN]` | Per-project counter, never reused |
| **Key history / redirect aliases** | **MUST** `[GEN]` | See trap below |
| Priorities (ordered, custom, icon+colour) | **MUST** `[GEN]` | Must be *ordered* — JQL `priority > High` depends on rank |
| Statuses + **status category** | **MUST** `[GEN]` | 3 categories: To Do (grey), In Progress (blue), Done (green). Jira hardcodes these and refuses to add more ([JRACLOUD-36241](https://jira.atlassian.com/browse/JRACLOUD-36241)) — **do the same**; boards/reports/JQL all key off exactly 3 buckets. |
| Resolutions (custom list) | **MUST** `[GEN]` | Done, Won't Do, Duplicate, Cannot Reproduce… For job search: Offer, Rejected, Ghosted, Withdrawn |
| Issue type → hierarchy level mapping | **MUST** `[GEN]` | |
| Cloning (with subtask/link copy options) | **SHOULD** `[GEN]` | |
| Move between projects | **SHOULD** `[GEN]` | Hard: remaps key, status, fields. Where key history earns its keep. |
| Convert issue ↔ subtask | **NICE** `[GEN]` | Free if hierarchy is uniform (§0.2) — it's just a reparent |
| Issue security levels | **CUT** | Enterprise. Project-level access is enough. |

### Traps that will bite you later if you skip them now

**Key history is not optional.** Moving an issue between projects changes its key. Jira keeps the old key as a permanent redirect. Skip this and every bookmark, commit message, and external link to a moved issue 404s. It's a 2-column table now and a migration nightmare in year two:

```sql
CREATE TABLE issue_key_history (issue_id, old_key TEXT UNIQUE, moved_at);
```

**Resolution ≠ Done status.** This is Jira's single most-reported confusion and it's worth understanding precisely so you can decide deliberately. In Jira an issue is "resolved" iff `resolution IS NOT EMPTY` — *not* when it reaches a Done-category status. So an issue can sit in status "Done" with an empty resolution and still count as open in every filter, report, and gadget. Two nominally-correct concepts, permanently out of sync, because resolution is set by a workflow post-function that admins forget to configure.

Atlas options:
1. **Recommended:** auto-set/auto-clear resolution from status category transitions (→ Done requires a resolution, prompt or default; leaving Done clears it). Keeps the data model's expressive power, kills the failure mode.
2. Drop `resolution` entirely, use status category + a `done_reason` field. Simpler, loses "Done/Won't Do" distinction in JQL.

Take option 1. Never ship option 3 (Jira's: leave both fields independent and hope).

---

## 2. Fields — MUST core, SHOULD long tail

### System fields

| Field | Tag | Notes |
|---|---|---|
| Summary | **MUST** `[GEN]` | Required, 255 chars |
| Description | **MUST** `[GEN]` | Rich text — see §2.1 |
| Status, Assignee, Reporter, Creator | **MUST** `[GEN]` | Creator immutable; Reporter editable w/ permission |
| Priority, Labels, Created, Updated, Resolution/Resolved date | **MUST** `[GEN]` | Labels: free-text tags, no admin needed — the single highest-value/lowest-cost field. Ship first. |
| Due date | **MUST** `[GEN]` | Critical for job search (follow-up dates) |
| Parent | **MUST** `[GEN]` | Replaces Jira's legacy Epic Link (Jira Cloud itself merged these) |
| Components | **SHOULD** `[GEN]` | Per-project categories w/ optional lead + default assignee. Generalises well: 3D → "Character/Environment/Props"; job search → "Frontend roles/Backend roles" |
| Versions: Affects + Fix Version | **SHOULD** `[SW→GEN]` | Generalise to "Milestone/Release" — 3D: "Sprint 1 delivery"; job search: rarely used |
| Start date | **SHOULD** `[GEN]` | Needed for timeline/Gantt |
| Time tracking (Original/Remaining Estimate, Time Spent) | **SHOULD** `[GEN]` | Store seconds. Parse `2w 3d 4h 30m`. |
| Environment | **NICE** `[SW]` | Genuinely software-only. Make it a custom field, not a system field. |
| Story points | — | **Not a system field.** → configurable estimation field (§0.3) |
| Watchers, Votes | **SHOULD** / **NICE** `[GEN]` | §8 |
| Rank (lexorank) | **MUST** `[GEN]` | §3.1 |
| Security level | **CUT** | |

### 2.1 Rich text — decide once, early

Description + comments need: **bold/italic/code/strike, headings, bullet+numbered lists, checklists, links, inline images, code blocks with syntax highlighting, tables, quotes, mentions, issue-key autolinks, emoji**.

Jira uses ADF (Atlassian Document Format, a JSON tree). Options for Atlas:
- **Markdown + a good editor (recommended):** store markdown text, render with a sanitizing pipeline. Portable, greppable, diffable, trivially CSV/export-friendly, plays well with LLM tooling. TipTap/ProseMirror or Milkdown gives WYSIWYG-over-markdown.
- **ProseMirror JSON:** richer (panels, layouts, macros), but you own a schema forever and lose grep/export simplicity.

Take markdown. The one thing to get right regardless: **store the source, render at read time, sanitize on render.** Never store rendered HTML.

Checklists inside description deserve special mention — for 3D modeling (per-asset steps) and job search (per-application prep) they replace what would otherwise be sub-task sprawl. Cheap to support (`- [ ]` in markdown), disproportionately useful.

### Custom field types — MUST-tier (verified against [Atlassian's list](https://confluence.atlassian.com/adminjiraserver/adding-custom-fields-1047552713.html))

Text (single-line, 255) · Text (multi-line) · Number · Date picker · Date-time picker · Select (single) · Select (multiple) · Checkboxes · Radio buttons · User picker (single) · User picker (multiple) · URL · Labels

### SHOULD-tier

Select list (**cascading**, parent→child) · Version picker (single/multi) · Project picker · Text (read-only) · **Formula/calculated field** (Cloud-only in Jira; high value — Atlas can do better trivially: `{{field.a}} * {{field.b}}`)

### NICE-tier

Group picker · Team picker · Rating/stars · Colour picker · File/attachment field · Currency · Duration · **Info-only fields**: Date of First Response, Days Since Last Comment, Participants, Time in Status (all read-only, all derivable from your history table for free — good ROI)

### Field semantics to copy verbatim

- **Number:** ±1 trillion range, round to 3dp (`5.555555` → `5.556`), accept scientific notation (`5e3`), format as number/currency/percentage.
- **Cascading select:** JQL `cascadeOption(parent[, child])`, `"none"` keyword for empty child; CSV import syntax `Parent -> Child`.
- **Labels:** no spaces (Jira's rule; keeps JQL unambiguous), autocomplete from existing.

**Field config per issue type** (required/optional, visible/hidden, default value, description, ordered layout) — **MUST**, but as one flat per-project table, not Jira's Field Configuration → Scheme → Project chain.

### Domain fit check

| Project | Custom fields that carry the workload |
|---|---|
| 3D modeling | Poly count (number), Software (select: Blender/Maya/ZBrush), Render engine (select), Reference URL (url), Client approval (checkbox), Asset stage (select), Texture res (select) |
| Job search | Company (text), Salary range (number/currency), Application URL (url), Recruiter (text), Interview date (date-time), Remote/Hybrid/Onsite (radio), Source (select: LinkedIn/referral/…), Follow-up due (date) |
| Programming | Environment, Repo (url), Affects version, Severity (select) |

The inventory above covers all three with zero domain-specific code — which is the test that §0's design passes.

---

## 3. Boards — MUST

| Feature | Tag | Notes |
|---|---|---|
| Board = saved filter + config | **MUST** `[GEN]` | Board is a *view over a JQL query*, not a container. Jira gets this right; it's why one board can span projects. |
| Columns + **many statuses → one column** | **MUST** `[GEN]` | Many-to-one is required, not optional |
| Drag-drop card → triggers transition | **MUST** `[GEN]` | Must respect workflow validity — no illegal drops |
| Kanban board | **MUST** `[GEN]` | The default for 3D + job search |
| Scrum board (backlog + cycles) | **SHOULD** `[SW→GEN]` | Gated on cycles enabled |
| WIP limits (min/max per column, visual breach) | **SHOULD** `[GEN]` | |
| Swimlanes: **Queries (JQL) / Assignee / Epic-parent / Project / Stories / None** | **SHOULD** `[GEN]` | Exactly Jira's 6 ([docs](https://support.atlassian.com/jira-software-cloud/docs/configure-swimlanes/)). Query-based covers everything else. |
| Quick filters (JQL toggles, multi-select AND) | **SHOULD** `[GEN]` | Massive value, ~50 LOC. Punches far above its weight. |
| Card layout (≤3 extra fields) | **SHOULD** `[GEN]` | Jira's 3-field cap is sensible — cards degrade past it |
| Card colours (by type/priority/assignee/**JQL**) | **NICE** `[GEN]` | JQL-based is the flexible one |
| Backlog (ranked, drag to cycle) | **SHOULD** `[SW→GEN]` | |
| Sub-task grouping toggle | **NICE** `[GEN]` | |
| Working days / holidays | **NICE** `[GEN]` | Only matters once reports exist |
| Board admins (separate from project admins) | **CUT** | |
| Epics panel / Versions panel in backlog | **SHOULD** `[SW→GEN]` | Generalises to "filter backlog by parent" |

### 3.1 Ranking — get this right on day one

Drag-drop ranking with integer `position` columns forces an O(n) renumber on every drop and deadlocks under concurrency. Use a **lexicographic rank** (LexoRank/fractional indexing): rank is a string, inserting between `"aaa"` and `"aab"` yields `"aaaV"` — one row updated, no renumber.

```
Global rank per issue (string, indexed). Board order = ORDER BY rank.
Rebalance job when ranks get pathologically long (rare).
```

Jira has exactly one global rank field shared by all boards; this means reordering on board A silently reorders board B. Slightly surprising, but correct — per-board rank multiplies storage and creates "which board's order is real?" ambiguity. Copy Jira here.

---

## 4. Workflows — MUST (simple engine), CUT (enterprise config)

**MUST:** statuses; transitions (from-status[] → to-status, with a global "any status" source); per-issue-type workflow; **simple visual editor**; validity enforcement everywhere (board drag, API, bulk, automation).

### Execution contract — copy exactly ([verified](https://support.atlassian.com/jira-cloud-administration/docs/configure-advanced-issue-workflows/))

```
1. CONDITIONS  → fail = transition button HIDDEN from user
2. VALIDATORS  → fail = button shown, attempt REJECTED with error message,
                        status unchanged AND post-functions DO NOT run
3. TRANSITION  → status change committed
4. POST-FUNCTIONS (essential ones, fixed order, non-removable):
   a. set status  b. add comment if entered  c. update change history + store
   d. reindex     e. fire event → listeners/automation
5. Optional post-functions
```

The conditions-hide / validators-reject distinction is the subtle bit and it's worth preserving — it's *the* mechanism that makes Jira's transition UI feel non-broken (you never see a button you can't press).

| Element | Tag | Notes |
|---|---|---|
| **Conditions** — permission-based, Only Assignee, Only Reporter, User in group/role, Sub-task blocking, Previous status, Value field, Compare number field, Hide from user, Always false | **SHOULD** `[GEN]` | Ship 4: Only Assignee, Only Reporter, User in group, **Sub-task blocking** (= "can't close parent with open children" — universally wanted) |
| **Validators** — required field, permission, date-window, regex | **SHOULD** `[GEN]` | Ship "required field on transition" first; it's 90% of use |
| **Post-functions** — set resolution ⭐, assign to (current user / reporter / lead), clear field, copy field, update field, add comment, fire event, **webhook** | **MUST** `[GEN]` | Set-resolution is the load-bearing one (§1 trap) |
| Transition screens (prompt for fields on transition) | **SHOULD** `[GEN]` | "Resolution + comment on Done" |
| Transition ordering (`opsbar-sequence`) | **NICE** `[GEN]` | |
| Workflow **schemes** | **CUT** | → direct issue_type → workflow FK |
| Groovy/script conditions | **CUT** | Automation engine (§9) covers it safely |
| Separation of Duties, Perforce, HipChat | **CUT** | |

**Seed workflows** (this is where "feels like Jira" is cheap to win):
- Programming `[SW]`: To Do → In Progress → In Review → Done (+ Blocked, + Reopen)
- 3D modeling: Concept → Blockout → Modeling → UV/Texture → Rigging → Render → Review → Approved
- Job search: Interested → Applied → Phone Screen → Interview → Take-home → Offer → Accepted/Rejected/Ghosted

That third column is the proof the engine is domain-neutral. If a workflow editor can express the job-search pipeline, it can express anything.

---

## 5. Search / JQL — MUST (this *is* the product)

JQL is Jira's actual moat. Boards, filters, dashboards, gadgets, automation, subscriptions, quick filters and reports are all just JQL with a renderer. **Build the parser properly, early, once.** Every shortcut here is repaid tenfold.

### Operators — complete, [verified](https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-operators-reference-939938745.html)

```
=  !=  >  >=  <  <=  IN  NOT IN  ~  !~  IS  IS NOT
WAS  WAS IN  WAS NOT  WAS NOT IN  CHANGED
Keywords: AND  OR  NOT  EMPTY  NULL  ORDER BY  ASC  DESC
History modifiers: AFTER  BEFORE  BY  DURING  ON  FROM  TO
```

Rules to enforce: `IS`/`IS NOT` **only** with EMPTY/NULL. `=`/`!=` **not** on text fields (use `~`). `>`,`>=`,`<`,`<=` **only** on orderable fields.

### Functions

**MUST:** `currentUser()`, `now()`, `startOfDay/Week/Month/Year()`, `endOfDay/Week/Month/Year()` — all with relative args (`-1w`, `+3d`).
**SHOULD:** `membersOf()`, `openCycles()`/`closedCycles()`/`futureCycles()` (Jira: `openSprints()`/`closedSprints()`/`futureSprints()`) `[SW→GEN]`, `releasedVersions()`, `unreleasedVersions()`, `latestReleasedVersion()`, `earliestUnreleasedVersion()`, `linkedIssues(key[, type])`, `watchedIssues()`, `votedIssues()`, `issueHistory()`, `standardIssueTypes()`/`subtaskIssueTypes()`, `cascadeOption()`.
**NICE:** `updatedBy(user[, from[, to]])`, `componentsLeadByUser()`, `projectsLeadByUser()`, `projectsWhereUserHasPermission()`, `projectsWhereUserHasRole()`, `issuesWithRemoteLinksByGlobalId()`.
**CUT:** all SLA (`breached()`, `elapsed()`, `running()`, `withinCalendarHours()`, …) and approval (`approved()`, `myPending()`, …) functions — JSM-only.

### The WAS/CHANGED scoping insight

Jira supports history operators on **exactly 6 fields**: assignee, fixVersion, priority, reporter, resolution, status (+ creator). Not on custom fields, not on summary, not on labels. This isn't laziness — it's the pragmatic scope where dedicated changelog indexing pays off.

**Copy this constraint.** Generic "history search on any field" forces you to index every field change, and the query planner gets ugly fast. Ship `status CHANGED FROM "In Progress" TO "Done" AFTER -7d` (enormously useful — cycle time, rework detection, "what did I actually finish this week") on the closed set, and stop.

### Field support matrix ([verified](https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-fields-reference-939938743.html))

| Field class | Operators |
|---|---|
| assignee, reporter, creator | `= != IS ISNOT IN NOTIN WAS WASIN WASNOT WASNOTIN CHANGED` |
| status, priority, resolution, fixVersion | full set incl. WAS/CHANGED |
| summary, description, environment | `~ !~ IS ISNOT` only |
| comment | `~ !~` only |
| text (all-text pseudo-field) | `~` only |
| statusCategory, parent, issueLinkType | `= != IN NOTIN` |
| attachments | `IS / IS NOT` only |
| created, updated, due, resolved, lastViewed | `= != > >= < <= IS ISNOT IN NOTIN` + date fns |

### Search surface

| Feature | Tag |
|---|---|
| Basic search (dropdown filter builder) | **MUST** `[GEN]` |
| Advanced search (JQL text + autocomplete + validation) | **MUST** `[GEN]` |
| **Basic ⇄ advanced round-trip** | **SHOULD** `[GEN]` — Jira degrades one-way (JQL too complex → can't go back). Users hate it. Round-trip when the AST is expressible. |
| Saved filters (name, description, owner) | **MUST** `[GEN]` |
| `filter = "My Filter"` composition in JQL | **SHOULD** `[GEN]` — filters referencing filters; guard cycles |
| Filter sharing / favourites | **NICE** (solo) `[GEN]` |
| Column config, sorting, pagination, inline edit in results | **MUST** `[GEN]` |
| Export results → CSV/JSON | **MUST** `[GEN]` |
| Full-text indexing | **MUST** `[GEN]` — Postgres `tsvector` is plenty; don't reach for Elasticsearch |
| Filter subscriptions (email on cron) | **NICE** `[GEN]` — §14 |

**Implementation:** hand-written recursive-descent parser → AST → SQL. Do **not** string-concatenate SQL. The AST is reused by automation's JQL condition, quick filters, boards, gadgets, and subscriptions — it's the highest-reuse component in the system.

---

## 6. Agile ceremonies — SHOULD, mostly `[SW→GEN]`

| Feature | Tag | Notes |
|---|---|---|
| Backlog view (ranked, drag to cycle/board) | **SHOULD** `[SW→GEN]` | Useful even without cycles = prioritised inbox |
| Estimation field (configurable unit) | **SHOULD** `[GEN]` | §0.3 |
| Cycle CRUD: create / start / complete | **SHOULD** `[SW→GEN]` | See state machine below |
| **Carry-over on complete** | **SHOULD** `[SW→GEN]` | Incomplete → backlog / next cycle / new cycle |
| Parent/Epic panel (filter backlog by parent) | **SHOULD** `[GEN]` | |
| Timeline / Gantt | **SHOULD** `[GEN]` | Bars from start+due date, drag to reschedule. **Valuable for all three domains** (3D delivery dates, interview scheduling) |
| Dependencies on timeline (from `blocks` links) | **NICE** `[GEN]` | Derive from links — do not create a second dependency concept |
| Versions/Releases (create, release, archive, progress) | **SHOULD** `[SW→GEN]` | Rename "Milestone" |
| Capacity planning / team allocation | **CUT** | Enterprise |
| Parallel active cycles | **NICE** `[SW]` | |
| Cross-project plans / Advanced Roadmaps | **CUT** | Premium-tier bloat |

### Cycle state machine + the snapshot requirement

```
future ──start(name, goal, start_date, end_date)──▶ active
active ──complete(destination for incomplete)────▶ closed
```

**Critical:** sprint membership is *historical*, not current-state. A closed cycle must retain its issue list forever or the sprint report is unreconstructable. And the sprint report exists specifically to surface **overcommitment** and **scope creep** — which means you must capture:

- a **snapshot of scope at start** (issue ids + estimates), and
- a **running log of scope added/removed during** the cycle.

Neither is derivable from current state after the fact. Model it as a many-to-many join with event rows, not an FK:

```sql
issue_cycle (issue_id, cycle_id, added_at, removed_at NULL, in_scope_at_start BOOL)
cycle_snapshot (cycle_id, taken_at, issue_id, estimate, status_category)  -- daily, for burndown
```

Get this wrong and every Scrum report is permanently wrong. It's the one place where "we'll add it later" genuinely doesn't work.

---

## 7. Reports — SHOULD/NICE

`[SW]` = assumes cycles+points; `[GEN]` = works for any project.

| Report | Tag | Domain |
|---|---|---|
| **Cumulative Flow Diagram** | **SHOULD** | `[GEN]` ⭐ Best general report. Count by status category over time. Works for job search (applications by stage) and 3D. |
| **Created vs Resolved** | **SHOULD** | `[GEN]` ⭐ Trivial, universally useful |
| Burndown (cycle) | **SHOULD** | `[SW→GEN]` Degrade to issue-count when estimation=none |
| Burnup (shows scope change) | **SHOULD** | `[SW→GEN]` Strictly more informative than burndown |
| Velocity | **NICE** | `[SW]` Needs ≥3 cycles to mean anything |
| Sprint/Cycle report (committed vs completed vs added) | **SHOULD** | `[SW]` Needs §6 snapshots |
| Control chart (cycle time + rolling avg) | **NICE** | `[GEN]` Genuinely great for Kanban/continuous flow |
| **Average Age** (unresolved) | **NICE** | `[GEN]` Perfect for job search — "how long since I applied" |
| Epic/Parent report | **NICE** | `[GEN]` |
| Version report / Release burndown | **NICE** | `[SW→GEN]` |
| Pie chart by field | **NICE** | `[GEN]` Cheap; reuse gadget code |
| Recently Created, Resolution Time, Single Level Group By | **NICE** | `[GEN]` |
| Time Tracking report | **NICE** | `[GEN]` |
| User Workload / Version Workload / Workload Pie | **CUT** | Meaningless for solo |

**Implementation:** all of CFD, burndown, burnup, velocity, average-age and control chart derive from **one daily snapshot table** (`date, issue_id, status_category, estimate, cycle_id`). Build the snapshot job once; each report is then a GROUP BY. Do not compute reports by replaying the changelog at read time — Jira does this and it's why its reports are slow.

---

## 8. Collaboration — MUST core

| Feature | Tag | Notes |
|---|---|---|
| Comments (rich text, edit, delete, threading) | **MUST** `[GEN]` | Jira has no real threading; flat + quote is fine |
| **@mentions** → notification | **MUST** `[GEN]` | |
| Comment visibility (internal vs public) | **CUT** | JSM-only concept. Solo = meaningless. |
| Attachments (drag-drop, thumbnails, inline images) | **MUST** `[GEN]` | ⭐ **Critical for 3D** — renders, refs, textures. For job search — CVs, JDs, offer letters. Needs image preview + PDF preview. |
| **Attachment versioning** | **SHOULD** `[GEN]` | Jira lacks it; 3D workflows need `model_v3.blend`. Cheap differentiator. |
| Issue links (blocks/is blocked by, relates to, duplicates/is duplicated by, clones/is cloned by, causes/is caused by) | **MUST** `[GEN]` | Custom link types **SHOULD**. Auto-materialise inverse. |
| Remote links (URL + title + icon) | **SHOULD** `[GEN]` | Job search: LinkedIn/job posting. 3D: ArtStation ref. |
| Watchers | **SHOULD** `[GEN]` | Auto-watch on comment/assign/create |
| Voting | **NICE** `[GEN]` | Near-useless solo. Late. |
| **Activity/history tab (full changelog)** | **MUST** `[GEN]` | Every field change: who/when/from/to. Powers WAS/CHANGED JQL, reports, audit. **Non-negotiable, build with the issue table.** |
| Worklogs (time, date, comment, remaining-estimate adjust) | **SHOULD** `[GEN]` | 3D: hours per asset. Job search: rarely. |
| Reactions (emoji) | **NICE** `[GEN]` | |

**Changelog schema — build this on day one, not later:**

```sql
CREATE TABLE issue_history (
  id, issue_id, author_id, created_at,
  field TEXT, from_value TEXT, from_display TEXT, to_value TEXT, to_display TEXT
);
```

Both raw + display values (Jira does this: `from`/`fromString`) — raw for querying, display for rendering after the referent is renamed/deleted. Retrofitting history onto a live schema means permanently losing everything before the migration.

---

## 9. Automation — SHOULD (huge leverage for solo users)

Structure: **1 trigger + N conditions + N actions**, plus branching. This is where a solo user recovers the labour of the team they don't have.

### Triggers ([verified](https://support.atlassian.com/cloud-automation/docs/jira-automation-triggers/))

**MUST:** Issue created, Issue transitioned, Field value changed, Issue assigned, Issue commented, **Scheduled** (interval or cron + optional JQL), Manual trigger.
**SHOULD:** Issue updated, Issue deleted, Issue linked / link deleted, Issue moved, Work logged, Comment edited, Incoming webhook, Multiple issue events.
**NICE:** Cycle created/started/completed `[SW→GEN]`, Version created/updated/released.
**CUT:** all DevOps (branch/commit/PR/build/deploy — 13 triggers), Compass, Loom, Figma/design, Security/vulnerability, Alert/SLA/approval, Slack-emoji, Form triggers.

That cut removes ~60% of Jira's trigger surface with zero loss for Atlas's three domains.

### Conditions (Jira has exactly 12; ship 5)

**MUST:** JQL condition, Issue fields condition, **If/else block**, User condition, Smart values condition (regex).
**SHOULD:** Related issues condition.
**CUT:** AQL, Affected services, Forms attached, Alert fields, Design linked, Issue attachments.

### Actions ([verified](https://support.atlassian.com/cloud-automation/docs/jira-automation-actions/))

**MUST:** Edit issue, Transition issue, Comment, Assign, Create issue, Create sub-tasks.
**SHOULD:** Clone issue, Link issues, Delete links, Log work, Manage watchers, Send email, **Send web request** (webhook), Create variable, **Lookup issues** (JQL, ≤100), Re-fetch issue data, Log action, Delete comment/attachment.
**NICE:** Create/release/unrelease version, Create cycle, Lookup tables, Set entity property.
**CUT:** Slack/Teams/Twilio/SNS/AWS-SSM/Azure/Statuspage/Zoom/Confluence/Bitbucket/GitHub/GitLab-branch/Rovo/JSM-incident actions. (Send web request covers all of them for a solo user — one generic action beats 20 integrations.)

### Smart values — copy the syntax exactly ([verified](https://support.atlassian.com/cloud-automation/docs/jira-smart-values-issues/))

```
{{issue.key}} {{issue.summary}} {{issue.status.name}} {{issue.assignee.displayName}}
{{issue.parent.key}} {{issue.duedate}} {{issue.customfield.Company}}
{{issue.comments.last.body}}  {{issue.comments.size}}
{{changelog.status.fromString}} → {{changelog.status.toString}}
{{triggerIssue}} {{createdIssue}} {{lookupIssues}} {{webhookData.x.y}} {{webhookResponse.body}}
{{now.plusDays(7)}}  {{baseUrl}}  {{rule.name}}
{{missing.field|"fallback"}}   ← default-value syntax
List iteration: {{#issue.comments}}{{body}}{{/}}
```

**Do NOT copy** the `{{issue.Story Points}}` vs `{{issue.Story point estimate}}` split — that's a scar from two underlying fields. One estimation field (§0.3) makes it one smart value.

**Guardrails (essential):** loop detection (rule triggering itself), per-rule execution limit, **audit log per execution with success/failure + smart-value resolution trace** (Jira's rule audit log is the only reason automation is debuggable at all), and a rule-actor concept so automation-made changes are attributable in history.

### Killer rules per domain — the test of whether the engine is real

| Domain | Rule |
|---|---|
| Job search | Scheduled daily: `status = Applied AND updated <= -7d` → comment "Follow up?" + set due date. **This is a genuinely killer app for a job hunt.** |
| 3D modeling | Issue transitioned → Approved → set resolution Done, notify, create sub-task "Export for delivery" |
| Programming `[SW]` | Issue created AND `type = Bug AND priority = Highest` → assign to me, add to current cycle |

---

## 10. Admin — mostly CUT

| Feature | Tag | Notes |
|---|---|---|
| Projects (key, name, lead, avatar, description, **template**, archive) | **MUST** `[GEN]` | |
| **Project templates** (Software/Kanban, 3D asset pipeline, Job search, Blank) | **SHOULD** `[GEN]` | ⭐ Sets issue types + workflow + fields + board in one click. This is how Atlas beats Jira for the stated use cases — a "Job Search" template is a *product feature*, not config. |
| Per-project issue types, workflow, field layout, board | **MUST** `[GEN]` | Flat, no schemes (§0.1) |
| Users | **MUST** `[GEN]` | |
| Simple project access: Owner / Member / Viewer | **SHOULD** `[GEN]` | ← **replaces the whole permission-scheme system** |
| Global custom field registry | **MUST** `[GEN]` | Must be global for cross-project JQL |
| Statuses/priorities/resolutions/link-types admin | **MUST** `[GEN]` | |
| Audit log | **NICE** `[GEN]` | Automation audit log matters more |
| **Permission schemes** (40+ granular permissions) | **CUT** | → 3 roles. Biggest cut available. |
| Notification schemes | **CUT** | → per-user prefs (§14) |
| Screens / Screen schemes / Issue Type Screen Schemes | **CUT** | → field layout per type |
| Field configurations / schemes | **CUT** | → field layout per type |
| Project roles vs groups (two parallel systems) | **CUT** | → members |
| Issue security schemes, Global permissions, Workflow schemes, Issue type schemes | **CUT** | |
| JSM entirely (SLAs, approvals, request types, portals, Assets/AQL, Organizations) | **CUT** | Not part of Software/Work Management |

**Verified reality check:** Jira ships **40+ project permissions** (Administer Projects, Extended Project Administration, Browse Projects, View Read-Only Workflow, View Development Tools, Manage Sprints, Start/Complete Sprints, Edit Sprints, Assign Issues, Assignable User, Create/Edit/Delete/Link/Move/Resolve/Close/Transition Issues, Modify Reporter, Set Issue Security, Schedule Issues, Add Comments, Edit Own/All Comments, Delete Own/All Comments, Create Attachments, Delete Own/All Attachments, Manage Watcher List, View Voters and Watchers, Work On Issues, Edit Own/All Worklogs, Delete Own/All Worklogs, Archive/Restore Issues, Browse Archive, Browse Project Archive) — each assignable to any of ~8 grantee types, times N schemes.

For a solo user this is **~0 value and enormous complexity**. Owner/Member/Viewer covers it. If Atlas ever needs granularity, add it behind the 3 roles later — the reverse migration (scheme → roles) is the painful one.

---

## 11. Dashboards — SHOULD/NICE

**SHOULD:** dashboard CRUD, grid layout, drag-drop gadgets, per-gadget config.
**MUST-tier gadgets:** Filter Results, Assigned to Me, Pie Chart (group by field), Two-Dimensional Filter Statistics (x-field × y-field counts — genuinely great, e.g. status × company for job search), Activity Stream.
**SHOULD:** Created vs Resolved, Issue Statistics, Bubble Chart, Issue Calendar, Watched Issues, Quick Links, Labels, Introduction (markdown note).
**NICE:** Sprint Health, Sprint Burndown, Days Remaining in Sprint `[SW]`, Average Age, Recently Created, Resolution Time, Time Since Issues, Road Map, Voted Issues, Wallboard Spacer.
**NICE:** Wallboard mode (fullscreen, auto-refresh, `z` projector mode) — fun, low value solo.
**CUT:** dashboard sharing/permissions (solo), Service Project Report, Time to First Response.

Key: **gadgets are just saved-filter renderers.** Build 4 renderer types (table / pie / bar / stat) over the JQL AST and you have most of the list for free.

---

## 12. Bulk ops, import/export, templates, archiving

| Feature | Tag | Notes |
|---|---|---|
| Bulk edit (multi-select → change field) | **MUST** `[GEN]` | |
| Bulk transition, bulk delete | **MUST** `[GEN]` | Respect workflow validity |
| Bulk move (project/type/parent) | **SHOULD** `[GEN]` | |
| Bulk watch/unwatch, bulk label | **NICE** `[GEN]` | |
| Bulk limit | — | Jira caps at **1,000/op**. Adopt a similar cap + progress UI + **preview-before-apply** (Jira's confirmation screen is good UX — keep it). |
| **CSV import** (wizard + field mapping) | **MUST** `[GEN]` | ⭐ Onboarding depends on it — how a job-search spreadsheet becomes an Atlas project |
| CSV export | **MUST** `[GEN]` | |
| JSON export/import (full fidelity, round-trip) | **SHOULD** `[GEN]` | Better than CSV; enables backup + project templates |
| Issue templates (pre-filled create) | **SHOULD** `[GEN]` | 3D: "New asset" w/ standard sub-tasks. Job search: "New application". High value, low cost. |
| Recurring issues | **NICE** `[GEN]` | Or just a scheduled automation rule |
| Archive issue/project (hide, keep data) | **SHOULD** `[GEN]` | |
| Trash / soft delete + restore | **SHOULD** `[GEN]` | Jira lacks per-issue undelete; users want it |

**CSV import semantics to copy ([verified](https://support.atlassian.com/jira-cloud-administration/docs/import-data-from-a-csv-file/)):** Summary is the only required column · **repeat the column header** for multi-value fields (Labels, Components) · `Issue ID` + `Parent` for hierarchy, **parents must precede children** · cascading select as `Parent -> Child` · worklog format `seconds;date;author;comment` · time values in seconds · attachments as HTTP(S) URLs · `Project name`+`Project key` for multi-project · unmapped Priority/Resolution values creatable on the fly or importable as blank · ~1,500 rows/file recommended.

Atlas should improve on two: **preview + validate before commit** (Jira's importer fails halfway and leaves a mess), and **auto-detect/suggest mappings** from header names.

---

## 13. Keyboard shortcuts & UX affordances — MUST (this is "feels like Jira")

The affordances below are *why* Jira power users tolerate Jira. They're individually cheap and collectively define the product's feel. Underrating them is the classic clone mistake.

### Shortcuts ([verified](https://usethekeyboard.com/jira/))

**MUST:**
```
c        create issue          /        quick search
?        shortcut help overlay .        command/operations palette  ⭐
g d      dashboard             g p      projects
g i      issue navigator       g a      boards
j / k    next / prev issue     o|Enter  open issue
e        edit                  a        assign
i        assign to me          m        comment
l        edit labels           Ctrl+Alt+S  submit form
Esc      cancel                u        back to navigator
```
**SHOULD:** `n`/`p` next/prev column (board) · `t` toggle detail view · `-` expand/collapse swimlanes · `s`+`t` / `s`+`b` send to top/bottom · `[` toggle sidebar · `s` share.
**NICE:** `z` projector mode · `g`+`g` admin search.

The **`.` command palette** is the highest-value single item — modern users expect Cmd-K. Make it fuzzy, action-and-navigation-capable, and it subsumes half the other shortcuts.

### Non-shortcut affordances that matter as much

| Affordance | Tag |
|---|---|
| **Issue key autolink everywhere** (`PROJ-123` in any text → live link w/ hover preview) | **MUST** ⭐ |
| Inline edit on issue view (click field → edit, no modal) | **MUST** |
| Create-issue modal with sticky "create another" | **MUST** |
| Optimistic UI on drag-drop (no full-board reload) | **MUST** |
| Breadcrumbs: Project → Parent → Issue | **MUST** |
| Autocomplete in JQL (fields, values, functions) | **SHOULD** |
| Recently viewed (`issueHistory()`, ~60 items) | **SHOULD** |
| Avatar everywhere, status lozenges w/ category colour | **SHOULD** |
| Hover preview on issue links | **SHOULD** |
| Deep-linkable everything (filters/boards encode state in URL) | **SHOULD** |
| Relative timestamps ("2 hours ago") + absolute on hover | **SHOULD** |
| Unsaved-changes guard | **SHOULD** |
| Toast + **undo** on destructive/bulk ops | **SHOULD** — Jira lacks undo; easy win |
| Drag-drop file onto issue = attach | **SHOULD** |
| Paste image from clipboard → inline attach | **SHOULD** ⭐ (3D) |

---

## 14. Notifications — SHOULD, and deliberately simpler than Jira

**Jira's model (verified):** 15 events × 12 recipient types, configured per-scheme. Events: Issue created/updated/assigned/resolved/closed/commented/comment edited/reopened/deleted/moved, Work logged/started/stopped, Worklog updated/deleted. Recipients: Current Assignee, Reporter, Current User, Project Lead, Component Lead, Single User, Group, Project Role, Single Email, All Watchers, User CF Value, Group CF Value. Default = Assignee + Reporter + All Watchers on everything.

**Atlas: cut the scheme, keep the events.** Per-user preferences:

```
For each event: [ In-app ] [ Email ] [ Off ]
Scope: issues I'm assigned / reporting / watching / @mentioned in
Global: [x] Don't notify me of my own changes   ← Jira's best-hidden setting
Digest: [ Immediate | Hourly | Daily | Off ]
```

| Feature | Tag |
|---|---|
| In-app notification centre (bell, unread, mark-read) | **SHOULD** `[GEN]` |
| @mention notification | **MUST** `[GEN]` |
| Email on assign / mention / comment | **SHOULD** `[GEN]` |
| **"Don't notify me of my own changes"** | **MUST** `[GEN]` — without it, solo users get spammed by themselves and turn everything off. Table stakes for a single-user tool. |
| Event **batching/dedup** | **SHOULD** `[GEN]` — Jira dedups simultaneous Created+Assigned into one. Copy: coalesce events on the same issue within a short window. |
| Daily digest | **NICE** `[GEN]` |
| Filter subscriptions (cron → email of results) | **NICE** `[GEN]` |
| Notification schemes | **CUT** |
| Web push / mobile / Slack | **NICE** `[GEN]` |

**Filter subscriptions, if built** ([verified](https://support.atlassian.com/jira-software-cloud/docs/manage-filters/)): daily (with time window) / weekly / monthly / advanced cron (`sec min hour dom month dow [year]`); **results evaluated per recipient** (so `currentUser()` resolves per person); Jira caps emails at first 200 results. For solo use this is largely redundant with scheduled automation — build automation first, and only add subscriptions if a real need appears.

---

## Recommended build order

**Phase 1 — Core (nothing works without these)**
1. Issue model: projects, issue types, hierarchy table, keys + key history, statuses + 3 status categories, priorities, resolutions
2. System fields + **history/changelog table from day one**
3. Custom fields (MUST types) + per-project field layout
4. Simple workflow engine (statuses, transitions, essential post-functions incl. **set resolution**)
5. **JQL parser → AST → SQL** (operators + core functions)
6. Issue view (inline edit, comments, @mentions, attachments, links, history tab)
7. Issue navigator (basic + advanced search, saved filters, columns, CSV export)
8. **Lexorank** ranking
9. Keyboard shortcuts + `.` palette + issue-key autolink

**Phase 2 — Feels like Jira**
10. Boards (columns, drag-drop, swimlanes, quick filters, card layout)
11. Backlog + generic **Cycles** (with §6 snapshot tables) + configurable estimation
12. Bulk ops + CSV import wizard
13. Notifications (in-app + email + "not my own changes")
14. Project templates ⭐ (Software / 3D pipeline / Job search / Blank)
15. Timeline/Gantt

**Phase 3 — Leverage**
16. Automation engine (triggers/conditions/actions/smart values/scheduled + rule audit log)
17. Reports off a single daily-snapshot table (CFD, Created-vs-Resolved, burndown/burnup first)
18. Dashboards + gadgets (4 renderers over the JQL AST)
19. Versions/milestones, components, worklogs, watchers

**Phase 4 — Polish**
20. Wallboard, voting, reactions, attachment versioning, issue templates, archiving/trash, filter subscriptions, remote links, formula fields, info-only fields

---

## Cheat sheet: cut vs keep

| Keep (the real moat) | Cut (enterprise cruft) |
|---|---|
| JQL + saved filters | Permission schemes (40+ perms × 8 grantees) |
| Configurable workflow state machine | Notification schemes |
| Boards over filters + swimlanes + quick filters | Screen / Screen / Issue-Type-Screen schemes |
| Full changelog + history-aware JQL | Field configuration schemes |
| Custom fields + per-type layout | Workflow schemes, Issue type schemes |
| Issue links + key autolink | Issue security levels |
| Automation + smart values | Project roles vs groups (two systems) |
| Keyboard shortcuts + `.` palette | All JSM (SLA/approvals/portals/Assets/AQL) |
| CSV import/export | DevOps/Compass/Loom/Figma triggers (~60% of triggers) |
| Bulk ops | Advanced Roadmaps as a paid tier |
| Project templates ⭐ (Atlas > Jira) | Capacity planning, workload reports |

## Facts

- **[verified]** JQL has exactly these operators: = , != , > , >= , < , <= , IN, NOT IN, ~ (CONTAINS), !~ (DOES NOT CONTAIN), IS, IS NOT, WAS, WAS IN, WAS NOT, WAS NOT IN, CHANGED. Keywords: AND, OR, NOT, EMPTY, NULL, ORDER BY, plus historical modifiers AFTER, BEFORE, BY, DURING, ON, FROM, TO. IS/IS NOT may only be used with EMPTY/NULL. = and != cannot be used with text fields (use ~ / !~). Ordering operators (>, >=, <, <=) only work on orderable fields (dates, versions, numbers).
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-operators-reference-939938745.html
- **[verified]** JQL date/time functions: now(), currentLogin(), lastLogin(), startOfDay()/endOfDay(), startOfWeek()/endOfWeek(), startOfMonth()/endOfMonth(), startOfYear()/endOfYear() — each accepting an optional relative increment argument like "+1d" / "-2w". startOfWeek() is locale-dependent.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-functions-reference-939938746.html
- **[verified]** JQL user/group functions: currentUser(), membersOf(Group), componentsLeadByUser([username]), projectsLeadByUser([username]), projectsWhereUserHasPermission(permission), projectsWhereUserHasRole(rolename), updatedBy(user[, dateFrom[, dateTo]]).
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-functions-reference-939938746.html
- **[verified]** JQL sprint functions: openSprints(), closedSprints(), futureSprints(). Version functions: releasedVersions([project]), unreleasedVersions([project]), latestReleasedVersion(project), earliestUnreleasedVersion(project). Issue type functions: standardIssueTypes(), subtaskIssueTypes(). Link/history functions: issueHistory() (up to 60 recently viewed), linkedIssues(issueKey[, linkType]), votedIssues(), watchedIssues(), issuesWithRemoteLinksByGlobalId() (1–100 ids). Custom field function: cascadeOption(parentOption[, childOption]) supporting a "none" keyword.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-functions-reference-939938746.html
- **[verified]** JQL fields and their operator support (verified subset): assignee/reporter/creator support =, !=, IS, IS NOT, IN, NOT IN, WAS, WAS IN, WAS NOT, WAS NOT IN, CHANGED. status/priority/resolution/fixVersion also support the full WAS/CHANGED history set. summary/description/environment support only ~, !~, IS, IS NOT. comment supports only ~ and !~. text supports only ~. statusCategory supports only =, !=, IN, NOT IN. parent supports only =, !=, IN, NOT IN. attachments supports only IS / IS NOT. sprint supports =, !=, IS, IS NOT, IN, NOT IN.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-fields-reference-939938743.html
- **[verified]** The WAS/WAS IN/WAS NOT/WAS NOT IN/CHANGED operators are only supported on a small closed set of fields: assignee, fixVersion, priority, reporter, resolution, status (plus creator for WAS-family). This is a deliberate scoping constraint — history-searchable fields are exactly those with dedicated changelog indexing.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-fields-reference-939938743.html — operator columns per field
- **[verified]** Jira custom field types (Data Center canonical list). Standard: Checkboxes, Date picker, Date time picker, Labels, Number field (floating point), Radio buttons, Select list (cascading), Select list (multiple choices), Select list (single choice), Text field (multi-line, unlimited), Text field (single line, max 255 chars), URL field, User picker (single user). Advanced: Group picker (single/multiple), Project picker (single project), Text field (read only), User picker (multiple users), Version picker (single/multiple versions).
  - Evidence: https://confluence.atlassian.com/adminjiraserver/adding-custom-fields-1047552713.html
- **[verified]** Jira Cloud has since added field types beyond the DC list: Formula fields (calculated, output as number/currency/percentage/date/duration/text), Team picker, Parent field (which replaced the old Epic Link), Space/Project picker, plus read-only information fields: Date of First Response, Days Since Last Comment, Participants, Time in Status. Number fields support number/currency/percentage formatting, range -1 trillion to 1 trillion, and round decimals to the nearest 1000th (5.555555 → 5.556); scientific notation input is accepted (5e3 = 5000).
  - Evidence: https://support.atlassian.com/jira-cloud-administration/docs/field-types-you-can-create-as-a-jira-admin/
- **[verified]** All Jira statuses — including custom ones — must belong to exactly one of three hardcoded status categories: To Do (grey/blue-grey), In Progress (blue), Done (green). New status categories cannot be created and the colours cannot be customised. Status category is exposed in JQL as statusCategory and is the mechanism for referring to many statuses at once.
  - Evidence: https://www.herocoders.com/blog/understanding-jira-issue-statuses + https://jira.atlassian.com/browse/JRACLOUD-36241 (rejected feature request to add categories)
- **[verified]** Status category is distinct from Resolution. An issue is considered 'resolved' when the Resolution field is non-empty, NOT when it reaches a Done-category status. This is the single most common source of Jira confusion — 'Done but unresolved' issues appear as open in filters/reports/gadgets. Resolution is normally set via a workflow post-function or a transition screen.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/advanced-workflow-configuration-938847443.html (Update Issue Field post-function sets Resolution) + JQL resolution field semantics (resolution = EMPTY means unresolved)
- **[verified]** Built-in workflow conditions: Always False, Block transition until approval, Compare Number Custom Field, Hide From User (only REST/post-functions can trigger), Only Assignee, Only Reporter, Permission Condition, Previous Status Condition, Separation of Duties (blocks a user who already performed a transition on the item), Sub-Task Blocking Condition, User Is In Any Group, User Is In Any Project Role, User Is In Custom Field, User Is In Group, User Is In Group Custom Field, User Is In Project Role, Value Field. Conditions can be grouped with AND/OR logic.
  - Evidence: https://support.atlassian.com/jira-cloud-administration/docs/configure-advanced-issue-workflows/
- **[verified]** Every Jira transition has 5 non-removable 'essential' post functions that execute in a fixed order: (1) set issue status to the destination step's linked status, (2) add comment if one was entered on the transition screen, (3) update change history and store the issue, (4) reindex the issue, (5) fire the generic event for listeners/automation. Optional post functions include: Assign to Current User, Assign to Lead Developer, Assign to Reporter, Clear Field Value, Copy Value From Other Field, Update Issue Field (Assignee/Description/Environment/Priority/Resolution/Summary/Original Estimate/Remaining Estimate), Update Issue Custom Field, Set issue security level based on user's project role, Trigger a Webhook.
  - Evidence: https://support.atlassian.com/jira-cloud-administration/docs/configure-advanced-issue-workflows/ + https://confluence.atlassian.com/adminjiraserver/advanced-workflow-configuration-938847443.html
- **[verified]** Validators run BEFORE the transition executes; if a validator fails the issue does not progress to the destination status AND the transition's post functions do not execute, and the validator's error message is shown. This ordering (conditions → validators → transition → post functions) is the required execution contract.
  - Evidence: https://confluence.atlassian.com/spaces/SERVICEDESKSERVER/pages/1044784442/Conditions+and+validators + https://confluence.atlassian.com/adminjiraserver/advanced-workflow-configuration-938847443.html
- **[verified]** Conditions vs validators differ in UI effect: a failed CONDITION hides the transition button entirely from the user; a failed VALIDATOR shows the button but rejects the attempt with an error message. Transition properties are key-value pairs; opsbar-sequence (positive integers, conventionally 10/20/30) orders transition buttons.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/advanced-workflow-configuration-938847443.html
- **[verified]** Board configuration surface (Scrum + Kanban): columns with workflow-status-to-column mapping (a column may map multiple statuses); column constraints/WIP limits; swimlanes based on Queries (JQL), Stories, Assignees, Epics, Projects, or No Swimlanes; quick filters (JQL-based toggles); card colours by issue type / priority / assignee / custom JQL; card layout with up to 3 extra fields shown per card; estimation statistic (story points, business value, or custom numeric); time tracking / remaining estimate settings; working days (weekends and holidays excluded from reports); ranking (drag-drop, backed by a rank field).
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/configuring-a-board-938845252.html + https://support.atlassian.com/jira-software-cloud/docs/configure-swimlanes/ + https://support.atlassian.com/jira-software-cloud/docs/customize-cards/
- **[verified]** Jira Software reports, grouped: Agile/Sprint — Burndown Chart, Burnup Chart, Sprint Report, Velocity Chart. Flow — Control Chart (cycle time), Cumulative Flow Diagram. Epic/Release — Epic Report, Epic Burndown, Release Burndown, Version Report. Issue analysis — Average Age Report, Created vs Resolved Issues, Pie Chart Report, Recently Created Issues, Resolution Time Report, Single Level Group By Report. Time/resource — Time Tracking Report, User Workload Report, Version Workload Report, Workload Pie Chart Report.
  - Evidence: https://support.atlassian.com/jira-software-cloud/docs/generate-a-report/
- **[verified]** Jira automation rule engine structure: a rule = 1 trigger + N conditions + N actions, with branching. Automation conditions are exactly 12: Issue fields condition, Alert fields condition, Smart values condition (with regex), Affected services, Forms attached, If/else block (supports two nesting levels), Issue attachments, Issue has design linked, AQL, JQL, Related issues, User condition.
  - Evidence: https://support.atlassian.com/cloud-automation/docs/jira-automation-conditions/
- **[verified]** Core (non-integration) automation triggers worth cloning: Field value changed, Issue created, Issue updated, Issue deleted, Issue assigned, Issue commented, Issue comment edited, Issue transitioned, Issue linked, Issue link deleted, Issue moved, Work logged, Manual trigger from issue, Multiple issue events, Incoming webhook, Scheduled (fixed interval or cron expression). Software-specific: Sprint created/started/completed, Version created/updated/released.
  - Evidence: https://support.atlassian.com/cloud-automation/docs/jira-automation-triggers/
- **[verified]** Core automation actions worth cloning: Create issue, Clone issue, Edit issue, Delete issue, Assign issue, Transition issue, Comment on issue, Edit comment, Delete comment, Create sub-tasks, Link issues, Delete issue links, Log work, Manage watchers, Create variable, Create lookup table, Create dynamic lookup table, Lookup issues (JQL, up to 100 results), Send web request, Send customized email, Re-fetch issue data, Log action (to audit log), Create sprint, Create version, Release version, Unrelease version, Delete attachments (regex match), Set entity property.
  - Evidence: https://support.atlassian.com/cloud-automation/docs/jira-automation-actions/
- **[verified]** Automation smart values use {{issue.property}} syntax with dotted traversal. Key values: {{issue.key}}, {{issue.summary}}, {{issue.status.name}}, {{issue.issueType.name}}, {{issue.resolution.name}}, {{issue.assignee.displayName}}, {{issue.reporter.displayName}}, {{issue.created}}, {{issue.updated}}, {{issue.duedate}}, {{issue.parent}}, {{issue.epic}}, {{issue.url}}. Lists support .size, .first, .last and iteration via the {{#list}}...{{/}} syntax (e.g. {{issue.comments.last.body}}). Context values: {{triggerIssue}}, {{createdIssue}}, {{createdIssues}}, {{lookupIssues}}, {{comment}}, {{changelog}} (with .fromString/.toString/.from/.to), {{fieldChange}}, {{worklog}}, {{attachment}}, {{webhookData}}, {{webhookResponse}} (.status/.headers/.body), {{baseUrl}}, {{eventType}}, {{rule.name}}, {{rule.actor}}. Default fallback syntax: {{missing.field|"fallback text"}}.
  - Evidence: https://support.atlassian.com/cloud-automation/docs/jira-smart-values-issues/
- **[verified]** Story points smart value differs by project type: {{issue.Story Points}} in company-managed projects vs {{issue.Story point estimate}} in team-managed projects. This is a real Jira wart caused by two separate underlying custom fields — Atlas should have exactly one estimation field to avoid replicating it.
  - Evidence: https://support.atlassian.com/cloud-automation/docs/jira-smart-values-issues/
- **[verified]** Notification scheme events (15 built-in): Issue created, Issue updated, Issue assigned, Issue resolved, Issue closed, Issue commented, Issue comment edited, Issue reopened, Issue deleted, Issue moved, Work logged on issue, Work started on issue, Work stopped on issue, Issue worklog updated, Issue worklog deleted. Custom events can be fired from workflow transitions.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/creating-a-notification-scheme-938847330.html
- **[verified]** Notification recipient types: Current Assignee, Reporter, Current User, Project Lead, Component Lead, Single User, Group, Project Role, Single Email Address, All Watchers, User Custom Field Value, Group Custom Field Value. The default scheme notifies Current Assignee + Reporter + All Watchers for all events. Jira deduplicates simultaneous events — if Issue Created and Issue Assigned fire together, only the Issue Created notification is sent.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/creating-a-notification-scheme-938847330.html + https://support.atlassian.com/jira/kb/jira-fires-issue-assigned-event-for-newly-created-issues/
- **[verified]** Project permissions (full list): Administer Projects, Extended Project Administration, Browse Projects, View Read-Only Workflow, View Development Tools, Manage Sprints, Start/Complete Sprints, Edit Sprints, Assign Issues, Assignable User, Create Issues, Edit Issues, Delete Issues, Link Issues, Move Issues, Resolve Issues, Close Issues, Transition Issues, Modify Reporter, Set Issue Security, Schedule Issues, Add Comments, Edit Own Comments, Edit All Comments, Delete Own Comments, Delete All Comments, Create Attachments, Delete Own Attachments, Delete All Attachments, Manage Watcher List, View Voters and Watchers, Work On Issues, Edit Own Worklogs, Edit All Worklogs, Delete Own Worklogs, Delete All Worklogs, Archive Issues, Restore Issues, Browse Archive, Browse Project Archive. Bulk operations additionally require the global 'Make bulk changes' permission.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/managing-project-permissions-938847145.html
- **[verified]** Dashboard gadgets (~29 pre-installed): Activity Stream, Assigned To Me, Average Age, Average Time in Status, Average Number of Times in Status, Bubble Chart, Created vs Resolved, Days Remaining in Sprint, Filter Results, Introduction, Issue Calendar, Issue Statistics, Issues in Progress, Labels, Pie Chart, Projects, Quick Links, Recently Created Issues, Resolution Time, Road Map, Service Project Report, Sprint Health, Sprint Burndown, Time Since Issues, Time to First Response, Two Dimensional Filter Statistics, Voted Issues, Wallboard Spacer, Watched Issues. Most gadgets take a saved filter as their data source.
  - Evidence: https://support.atlassian.com/jira-cloud-administration/docs/use-dashboard-gadgets/
- **[verified]** Default issue link types (bidirectional, each with an outward and inward description): blocks / is blocked by; relates to / relates to (symmetric); duplicates / is duplicated by; clones / is cloned by; causes / is caused by. Link types are admin-configurable; creating a link automatically materialises the inverse on the target issue.
  - Evidence: https://confluence.atlassian.com/adminjiraserver/configuring-issue-linking-938847862.html + https://community.atlassian.com/forums/Jira-questions/Need-definition-for-Link-Issue-types/qaq-p/1738147
- **[verified]** Bulk operations: transition, delete, move, edit, and watch/unwatch. Hard limit of 1,000 issues per bulk operation in Jira Cloud (also the default in Server/DC, where exceeding it risks OutOfMemory; a sysadmin can raise it). Bulk change requires the global 'Make bulk changes' permission PLUS the corresponding per-project permission (e.g. Move Issues for a bulk move). Bulk REST API allows only 5 concurrent requests across all users.
  - Evidence: https://support.atlassian.com/jira-software-cloud/docs/edit-multiple-issues-at-the-same-time/ + https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-bulk-operations/
- **[verified]** CSV import: Summary is the only required column. Multi-value fields (Components, Labels, Fix Version) are imported by REPEATING the column header. Parent/child hierarchy uses 'Issue ID' + 'Parent' columns and parents MUST appear before children in the file. Cascading select uses 'Parent Value -> Child Value' syntax. Comments and worklogs use repeated columns; worklog format is 'time in seconds;date;author;comment'. Time tracking values are in seconds. Attachments are given as HTTP/HTTPS URLs. Multi-project import via 'Project name' + 'Project key' columns. Recommended max 1,500 issues per file. Unmapped Resolution/Priority/Issue Type values can be imported as blank, and new Priority/Resolution values can be created on the fly.
  - Evidence: https://support.atlassian.com/jira-cloud-administration/docs/import-data-from-a-csv-file/
- **[verified]** Filter subscriptions: schedule via daily (with a time window such as 5AM–5PM), weekly, monthly (specific day), or Advanced = a cron expression (seconds minutes hours day-of-month month day-of-week [year]). Recipients can be yourself, other users, or a group. Critically, results are evaluated PER RECIPIENT — a filter using currentUser() resolves against each recipient individually. Only the first 200 results are included in the email.
  - Evidence: https://support.atlassian.com/jira-software-cloud/docs/manage-filters/ + https://confluence.atlassian.com/spaces/JIRACORESERVER0820/pages/1095773308/Constructing+cron+expressions+for+a+filter+subscription
- **[verified]** Keyboard shortcuts. Global: c = create issue, / = quick search, ? = shortcut help overlay, g+d = dashboard, g+p = browse project, g+a = agile/boards, g+i = issue navigator, g+g = admin search. Issue view: o/enter = open, j/k = next/prev issue, e = edit, a = assign, i = assign to me, m = comment, l = edit labels, s = share, . = operations/command dialog, u = back to navigator, n/p = next/prev activity, [ = hide/show sidebar. Board: j/k = next/prev issue, n/p = next/prev column, t = toggle detail view, - = expand/collapse swimlanes, s+t = send to top, s+b = send to bottom, z = projector mode. Forms: Ctrl+Alt+S = submit, Esc = cancel.
  - Evidence: https://usethekeyboard.com/jira/ + https://confluence.atlassian.com/jirasoftware/blog/2015/12/4-essential-jira-software-keyboard-shortcuts
- **[verified]** Issue hierarchy: base Jira has 3 fixed levels — Epic (level 1) → Story/Task (level 0) → Sub-task (level -1). Additional levels ABOVE epic (e.g. Initiative at level 2) require Advanced Roadmaps and a Jira Premium/Enterprise subscription; they are configured at Settings > Manage Apps > Advanced Roadmaps > Hierarchy configuration by creating a level and mapping issue types to it. Levels BELOW sub-task cannot be added at all. Epic can be renamed but stays at level 1.
  - Evidence: https://support.atlassian.com/jira-software-cloud/docs/configure-custom-hierarchy-levels-in-advanced-roadmaps/ + https://confluence.atlassian.com/advancedroadmapsserver0329/configuring-initiatives-and-other-hierarchy-levels-1021218664.html
- *[likely]* Sub-tasks are structurally special-cased throughout Jira, not just a hierarchy level: they have their own issue-type class (subtaskIssueTypes() vs standardIssueTypes() in JQL), cannot themselves have children, are excluded from board columns by default, and cannot be moved between parents in bulk. This special-casing is a legacy artifact — a uniform parent pointer with a configurable hierarchy table is strictly better and is what Advanced Roadmaps retrofits on top.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-functions-reference-939938746.html (standardIssueTypes/subtaskIssueTypes) + hierarchy docs above
- *[likely]* Field/screen configuration in Jira composes through five indirection layers: Screen (ordered field list) → Screen Scheme (maps Create/Edit/View operations to screens) → Issue Type Screen Scheme (maps issue type → screen scheme) → assigned to Project. Separately: Field Configuration (per-field required/optional, hidden/visible, renderer, description) → Field Configuration Scheme (maps issue type → field configuration) → assigned to Project. Workflows compose similarly via Workflow Scheme (maps issue type → workflow) → Project.
  - Evidence: Atlassian admin docs structure (Configuring fields and screens / workflow schemes); direct page fetch 404'd, corroborated by https://confluence.atlassian.com/adminjiraserver/adding-custom-fields-1047552713.html context and search results
- *[likely]* Issue keys are of the form PROJKEY-N where N is a per-project monotonically increasing counter that is never reused (deleting PROJ-5 does not free the number). Keys are immutable in practice but DO change when an issue is moved between projects, and Jira retains the old key as a permanent redirect alias — any clone must store a key-history table or moved issues become 404s and break every external link.
  - Evidence: Jira Move Issues behaviour + 'Issue moved' notification event (https://confluence.atlassian.com/adminjiraserver/creating-a-notification-scheme-938847330.html) + CSV 'Work Item Key' import column
- *[likely]* Sprint lifecycle: sprints have three states — future, active, closed — reachable via create → start (with name, goal, start/end dates) → complete. On completion, incomplete issues are moved to a chosen destination: the backlog, the next planned sprint, or a new sprint (this is 'carry-over'). Velocity is computed only from issues Done at completion time. Parallel/multiple active sprints per board is an opt-in setting. Sprint membership is historical — a completed sprint retains its issue list for the sprint report, so sprint must be a many-to-many join with a captured completion snapshot, not a simple FK.
  - Evidence: Sprint report/velocity semantics from https://support.atlassian.com/jira-software-cloud/docs/generate-a-report/ and JQL openSprints()/closedSprints()/futureSprints(); direct sprint-lifecycle page fetch 404'd
- **[verified]** Sprint report specifically exists to detect two things: overcommitment and scope creep (issues added after sprint start). This requires storing a snapshot of sprint scope AT START, plus a running log of scope additions/removals during the sprint — it cannot be reconstructed from current state alone.
  - Evidence: https://support.atlassian.com/jira-software-cloud/docs/generate-a-report/ ('Shows work completed or pushed back to backlog each sprint'); grandiasolutions.com/jira-scrum-reports/
- **[verified]** Jira Service Management adds a large parallel surface that is NOT part of Software/Work Management: SLA fields and functions (breached(), completed(), elapsed(), everBreached(), outdated(), paused(), remaining(), running(), withinCalendarHours()), approval fields and functions (approved(), approver(), myApproval(), myPending(), pending(), pendingBy()), request types, customer portals, Organizations custom field, and Assets/AQL. All of this is out of scope for a Jira Software + Work Management clone.
  - Evidence: https://confluence.atlassian.com/jirasoftwareserver/advanced-searching-functions-reference-939938746.html (functions marked 'Jira Service Management only') + https://confluence.atlassian.com/servicemanagementserver102/conditions-and-validators-1473874867.html (Assets-only conditions)
- **[verified]** Atlas is currently greenfield: /home/alli/Projects/Atlas contains only README.md (74 bytes, 'A Project Management Application for managing concurrent projects') and a .git directory with a single 'Initial commit' (56b8472) on main. No source, no stack, no schema chosen yet — every architectural decision in this inventory is still open.
  - Evidence: ls -la /home/alli/Projects/Atlas; cat /home/alli/Projects/Atlas/README.md; git log

## Risks

- JQL is the keystone — boards, filters, dashboards, gadgets, automation conditions, quick filters and subscriptions are all JQL renderers. If the parser is hacked together (string-concatenated SQL, no AST), every downstream feature inherits the damage and the rewrite is a full-system rewrite. Build recursive-descent → AST → parameterised SQL, once, in Phase 1.
- The changelog/history table must exist from the first commit. It powers the activity tab, WAS/CHANGED JQL, every report, and automation's {{changelog}}. Retrofitting it onto a live schema permanently loses all history before the migration — the data simply does not exist to backfill.
- Sprint/cycle reporting cannot be reconstructed from current state. Committed-vs-completed and scope-creep detection require a scope snapshot at cycle start plus an add/remove event log during the cycle. Model issue_cycle as a many-to-many with added_at/removed_at/in_scope_at_start plus daily snapshots, or every Scrum report is silently and permanently wrong.
- Resolution vs Done-status is Jira's most notorious data-model wart: an issue can be in status 'Done' with empty resolution and still count as open everywhere. Decide deliberately (recommend: auto-set/auto-clear resolution from status-category transitions). Copying Jira's 'two independent fields, hope admins wire the post-function' is the one option guaranteed to reproduce the bug.
- Hardcoding Epic→Story→Sub-task will require an Advanced-Roadmaps-style retrofit later, exactly as it did for Atlassian (who gated the fix behind Premium). A hierarchy_level table + uniform parent_id costs nothing now. Note also that Jira special-cases sub-tasks structurally (separate JQL functions, no children, board exclusion) — inheriting that special-casing spreads conditionals through boards, JQL, bulk ops and reports.
- Integer position columns for drag-drop ranking force O(n) renumbers and deadlock under concurrency. Use lexorank/fractional indexing from the start; converting a populated board later means a full rank rewrite plus a UI freeze.
- Issue keys change on project move and Jira keeps permanent redirect aliases. Without an issue_key_history table, every moved issue 404s for all external links, bookmarks and commit messages. Two columns now; painful migration later.
- Scope creep toward enterprise parity is the most likely way this project dies. The scheme layer (6 parallel three-level indirections), 40+ granular permissions, and all of JSM are ~40% of Jira's surface and ~0% of a solo user's value. Every hour there is an hour not spent on JQL, boards, or automation.
- Conversely, under-building the UX affordances is the classic clone failure: issue-key autolinking, inline edit, optimistic drag-drop, the `.` command palette, and 'don't notify me of my own changes' are individually trivial and collectively the entire reason Jira feels like Jira. A feature-complete clone that misses these reads as a spreadsheet.
- Automation without loop detection, per-rule execution caps, and a per-execution audit log (with smart-value resolution traces) becomes an unpredictable, undebuggable runaway — a rule that edits an issue and triggers itself can cascade. Jira's rule audit log is the only thing that makes its automation debuggable; ship it with the engine, not after.
- Rich-text format is a one-way door. Markdown (store source, render at read, sanitize on render) keeps export/grep/LLM-tooling simple; a ProseMirror/ADF-style JSON tree is richer but you own the schema forever and CSV/JSON export degrades. Never store rendered HTML.
- Notification defaults are a retention risk for single-user tools: Jira's default scheme notifies Assignee+Reporter+Watchers on all 15 events, which for a solo user means being spammed by their own changes until they disable everything. 'Don't notify me of my own changes' must be on by default, plus same-issue event coalescing.
- Some sources here are Data Center/Server docs (JQL references, custom field types, workflow config, permissions) which have drifted from Cloud — Cloud has added Formula fields, Team/Parent pickers and info-only fields, renamed Epic Link → Parent, and uses 'work item' terminology. The underlying semantics are stable and correct for cloning, but do not treat DC field lists as a complete Cloud inventory.
- A few specifics are marked 'likely' rather than 'verified' because the canonical pages 404'd: the exact screens/screen-scheme/field-configuration composition chain, the sprint completion carry-over destinations, and issue-key redirect behaviour on move. All are corroborated by adjacent docs and are low-risk architecturally, but confirm before implementing the sprint-completion dialog in detail.

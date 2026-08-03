# Atlas — Implementation Plan

A self-hosted Jira-equivalent with first-class GitHub, Claude Code, and Gemini integration.

**Legend:** `[SW]` software-specific (must be disableable) · `[GEN]` domain-general · ⭐ high value/low cost · 🔒 security-critical

> **Ground rules for every branch below**
> - Work happens on `feat/NN-slug`, merges to `main` via PR once CI is green.
> - Commit after each completed section, not once per branch.
> - **No `Co-Authored-By: Claude` trailers. No "Generated with Claude Code" PR footers.** Alastair Rayner is the sole contributor.
> - Research backing these decisions lives in `docs/research/` — including `corrections.md`, which records claims that an adversarial pass refuted. Read it before trusting recollection about an API.

---

## Architecture decisions that dominate everything else

These are forks that cannot be cheaply undone. Decided up front, deliberately.

### A. Recursive hierarchy *is* the nested-board feature

The requested "card that opens its own board" and Jira's Epic→Story→Sub-task hierarchy are the same mechanism. Jira hardcodes three levels, special-cases sub-tasks everywhere, and paywalls anything above Epic. Atlas instead makes hierarchy a per-project configuration over a uniform `parent_id`:

```sql
hierarchy_level(id, project_id, level INT, name TEXT, UNIQUE(project_id, level))
-- card.parent_id → card.id, uniform at every level.
-- The only rule: parent.level > child.level.
```

A board is then *a view over a parent's children*. "3D Modeling" is a card at level 1; opening it renders its children as a board. This falls out of the model rather than being built:

| Project | Level 2 | Level 1 | Level 0 | Level -1 |
|---|---|---|---|---|
| Programming `[SW]` | Initiative | Epic | Story/Bug/Task | Sub-task |
| 3D modeling | Collection | Asset | Model/Texture/Rig | Step (retopo, UV, bake) |
| Job search | — | Company | Application | Task (tailor CV, follow-up) |

Guards: depth cap (5), cycle detection on reparent.

### B. Collapse Jira's scheme layer

Jira routes config through six parallel three-level indirections (Screen→Screen Scheme→Issue Type Screen Scheme→Project, and five more). That exists for the 500-project case Atlas does not have, and it is the single largest source of Jira's "why can't I just change this field" misery. **Atlas: flat per-project config.** Share exactly two things — workflows, and custom field definitions (a field must be one global entity or cross-project JQL breaks). Recover the rest with a "copy config from project X" action.

### C. Cycles, not Sprints; one estimation field

Sprints are mandatory on Jira's Scrum boards and velocity assumes points-per-sprint. Atlas ships generic **Cycles** (future→active→closed, renameable per project, **fully disableable**) and **one** configurable estimation field (points/hours/days/t-shirt/count/none) — never Jira's two-fields-called-Story-Points scar.

### D. Three things that cannot be retrofitted

1. **Changelog table** — build with the card table. History is unreconstructable after the fact; it powers `WAS`/`CHANGED` queries, reports, and audit.
2. **Lexorank ranking** — integer `position` forces O(n) renumber per drop and deadlocks under concurrency.
3. **Cycle scope snapshots** — committed-vs-completed is not derivable from current state later.

### E. Resolution ≠ Done status

Jira's most-reported confusion: an issue is "resolved" iff `resolution IS NOT EMPTY`, *not* when it reaches a Done status — so a card can sit in "Done" and still count as open everywhere. Atlas auto-sets/auto-clears resolution from status-category transitions. Keeps the expressive power, kills the failure mode.

---

## Phase 0 — Foundation `feat/00-foundation`

- [x] `.gitignore` (SQLite + WAL/SHM sidecars, agent workspaces, secrets, uploads)
- [x] `.editorconfig`
- [x] `CLAUDE.md` — architecture + attribution rule
- [x] `docs/research/` — 7 API/design dossiers + adversarial corrections
- [x] `TODO.md` (this file)
- [x] Cargo workspace: `backend/`
- [x] Vite + React 19 + TS: `frontend/`
- [x] `justfile` + `Makefile`: `dev`, `test`, `lint`, `fmt`, `migrate`, `seed`, `build`, `check`
- [x] GitHub Actions CI: `cargo fmt --check`, `clippy -D warnings`, `cargo test`, `cargo sqlx prepare --check`, `tsc --noEmit`, `eslint`, `vitest`, `playwright`
- [x] `.env.example` + typed config loader (figment), fail-fast on missing required vars
- [x] `docker-compose.yml` + multi-stage `Dockerfile`
- [x] `README.md`: what Atlas is, quickstart, architecture diagram
- [x] `docs/adr/` — 6 decision records
- [x] Browser-level contrast tests (jsdom cannot see colour; this class of bug is invisible to unit tests)
- [x] ~~Commit `.sqlx/` once query macros land~~ — **resolved by decision**: Phases 2–4 use the runtime query API, so no offline metadata exists to drift. `cargo sqlx prepare --check` exits 0 permanently. Revisit only if a future phase adopts the macros.
- [x] `rust-toolchain.toml` (channel = stable + rustfmt/clippy). The MSRV floor is `rust-version = "1.94"` in `Cargo.toml`, which Cargo already enforces — pinning the channel to 1.94 would freeze the compiler *at* the floor rather than guarantee a minimum

---

## Phase 1 — Backend core `feat/01-backend-core`

- [x] Axum 0.8 skeleton: router, `AppState`, graceful shutdown (SIGINT+SIGTERM), `/healthz` (pings both pools)
- [x] `AppError` + `IntoResponse`, RFC 7807 problem+json, error taxonomy (validation/not-found/conflict/forbidden/internal)
- [x] `tracing` + `tracing-subscriber`, JSON logs in prod, request-id middleware, span per request
- [x] SQLx 0.9 + SQLite pool. **WAL, `busy_timeout=5s`, `foreign_keys=ON`, `synchronous=NORMAL`**
- [x] Writer pool (1 conn) + reader pool (N conns); `pool.begin_with("BEGIN IMMEDIATE")` for write txns to avoid upgrade deadlocks (available since sqlx 0.8.4 — see `docs/research/corrections.md` #11)
- [x] `sqlx::migrate!`. **No `.sqlx` needed**: the codebase uses the runtime `query_as::<_, T>` API rather than the `query!` macros, deliberately — macros would impose offline-metadata upkeep on the whole workspace, where stale metadata breaks CI silently. Every SQL string is `&'static str`, so sqlx 0.9's `SqlSafeStr` bound is met with zero uses of `AssertSqlSafe` — its absence is a real signal that no SQL is assembled at runtime
- [x] Fractional-index ranking + property tests (insert-between never collides). Rebalance job deferred until keys actually grow long
- [x] Tower layers: CORS, compression, body limit, timeout, panic-catch
- [x] Integration test harness: ephemeral DB per test (a temp *file*, not `sqlite::memory:` — each in-memory connection gets its own private database, which would give the reader an empty DB). Uses `tower::ServiceExt::oneshot`, no `axum-test` dep
- [x] OpenAPI via `utoipa`, served at `/api/docs`; raw doc at `/api/openapi.json`

---

## Phase 2 — Users & auth `feat/02-auth` 🔒

- [x] `users` table: id, username, email, display_name, avatar_url, password_hash, role, is_active, must_change_password, created_at, last_login_at
- [x] **Argon2id** hashing (OWASP params: m=19MiB, t=2, p=1) on `spawn_blocking` — it is ~50ms of CPU and would otherwise stall the reactor
- [x] Session cookies — `HttpOnly` + `Secure` (prod) + `SameSite=Lax`, server-side session table, revocable. Only a SHA-256 of the token is stored, so a DB read yields nothing usable
- [x] CSRF: Origin/Referer check on unsafe verbs, plus `SameSite=Lax`
- [x] **Seed default admin `Admin`/`Admin` with `must_change_password=true`** — enforced as a *layer* over the whole `/api/v1` nest, not a per-handler check a new route could forget. Allowlist is 3 (method, path) pairs
- [x] Forced password-change screen on first login; rejects reuse of `Admin`
- [x] Password policy (≥12, common-password list, ≠ username), rate-limited login, lockout + backoff
- [x] Roles: Admin / Member / Viewer (replaces Jira's 40+ permission × 8 grantee scheme matrix)
- [x] User CRUD, deactivate (never hard-delete — cards reference authors)
- [x] `/api/me`, login, logout, change-password, session list + revoke
- [x] Audit log for auth events
- [x] Guards beyond the brief: an admin cannot deactivate themselves, and the last active admin cannot be demoted — either would make the instance unrecoverable through the API
- [x] Tests: forced-reset cannot be bypassed (mutation-tested against all 53 routes enumerated from the live OpenAPI doc), session fixation, timing-safe compare, no `password_hash` in any response
- [x] **Per-project access: Owner / Member / Viewer** — `project_members` + migration 0005. Enforced as a *layer* over the whole `/api/v1` nest, keyed on axum's `MatchedPath` (the router's own route template, so percent-encoding and dot-segments cannot dress a path up). **Every route must state its scope in `auth::project_access::SCOPES`, and `api::router` panics at startup if one does not** — a route nobody classified cannot be shipped, let alone reached. Resolution: instance admin → owner everywhere (else an admin is unrecoverably locked out); `projects.lead_id` → owner; creator → owner at creation; everyone else → whatever `project_members` says; **no row = no access**. The instance role is a *ceiling, not a floor*: an instance Viewer holding an `owner` row resolves to `viewer`. Inaccessible projects answer **404, not 403** (a 403 confirms the key namespace); lists **filter** rather than refuse. Last-owner guard mirrors the last-admin one, and counts *effective* owners — a row held by an instance Viewer or a deactivated account grants nothing and must not satisfy it
- [x] **The last-owner guard, closed on every path into an ownerless project** — an adversarial pass found the member routes guarded and three other doors into the same state wide open. A project's last owner could be removed by deactivating them, or by demoting them to instance Viewer (the ceiling silently voids every `owner` row they hold) — neither route mentions a project, so neither asked. `member::projects_solely_owned_by` answers the question from the *account's* side and both `/users` routes consult it, inside their write transaction. Migration 0005's backfill had the same bug one level down: its second statement fired only on `lead_id IS NULL`, so a pre-0005 project whose lead was deactivated or read-only came out of the migration with an `owner` row that grants nothing and no admin row either. It now fires whenever the lead *cannot own*, giving the backfill one stated postcondition: **every pre-0005 project ends up with at least one member who can administer it**. `PATCH /projects/{key}` also granted silent ownership — naming a lead makes them an owner by rule, but `member::list` lists rows, so the new owner appeared nowhere in the member list an owner audits access from; it now writes the row, exactly as `create_project` already did, and validates the id (422, not the FK's 500). Race: the guards read inside `BEGIN IMMEDIATE` on a writer pool of one, so two concurrent "remove the other owner" requests serialise — pinned by two tests that fail when the count is moved out. **`assert_no_route_escapes_the_gate`**: `SCOPES` only covers `/api/v1`, so a top-level mount was the one place a route could skip CSRF, auth *and* project access with nothing to forget — the binary now refuses to boot on one
- [ ] Avatar upload — deferred to Phase 9 (needs attachments)
- [ ] zxcvbn strength meter — the backend embeds a common-password list; the frontend meter is heuristic. Revisit in Phase 19
- [ ] `session::purge_expired` exists and is tested but has no scheduled caller; expired rows are only reclaimed lazily. Wire it up when a job runner exists (Phase 15)
- [ ] Per-IP lockout trusts `X-Forwarded-For` unconditionally — correct for the default reverse-proxy deploy, but needs a trusted-proxy setting (Phase 20). Per-username lockout is unaffected

---

## Phase 3 — Domain model `feat/03-domain`

- [x] `projects`: key, name, lead, avatar, description, template, archived_at
- [x] Per-project **key counter** → `ATLAS-123`, never reused (atomic `UPDATE ... RETURNING` inside the write txn)
- [x] ⭐ `card_key_history(card_id, old_key UNIQUE, moved_at)` — permanent redirects; without it every bookmark/commit reference 404s after a move
- [x] `hierarchy_levels` per project (§A) + depth cap + cycle detection
- [x] `card_types` per project (name, icon, colour, level) — not a fixed enum
- [x] `statuses` + **exactly 3 status categories** (To Do grey / In Progress blue / Done green). Jira hardcodes 3 and refuses more; boards, reports, and JQL all key off the 3 buckets
- [x] `priorities` (ordered — `priority > High` depends on rank), `resolutions`
- [x] `cards`: key, project, type, parent_id, summary, description(md), status, priority, assignee, reporter, creator, resolution, due_date, start_date, estimate, rank, timestamps
- [x] ⭐ **`card_history`** — id, card_id, author, created_at, field, from_value, from_display, to_value, to_display. Raw *and* display values (raw to query, display to render after a referent is renamed/deleted)
- [x] `comments` (markdown source, edited_at)
- [ ] `attachments` + versioning ⭐ (`model_v3.blend`; Jira lacks this)
- [x] `card_links` (blocks/relates/duplicates/clones/causes) + auto-materialised inverse
- [ ] `remote_links` (URL + title + icon)
- [ ] `watchers`, `worklogs`, `components`, `milestones` (Jira's "versions", generalised)
- [ ] Custom fields: global registry + per-project layout (required/hidden/default/order per type)
- [ ] Field types: text, textarea, number, date, datetime, select, multiselect, checkbox, radio, user, multiuser, url, labels
- [x] Repository layer + integration tests per aggregate
- [x] Soft delete (trash/restore UI in Phase 18)

---

## Phase 4 — Tags `feat/04-tags` ⭐

Free-text labels: the highest-value/lowest-cost field in the system.

- [x] `tags` (name, colour, project_id nullable = global), `card_tags`
- [x] No spaces in tag names (keeps query grammar unambiguous), autocomplete from existing
- [x] **Seeded tag presets per project type** (requested):
  - Programming: `bug` `feature` `refactor` `tech-debt` `docs` `testing` `ci` `security` `performance` `dependencies` `breaking-change` `good-first-issue` `blocked` `needs-review` `hotfix`
  - 3D modeling: `modeling` `sculpting` `retopo` `uv-unwrap` `texturing` `rigging` `animation` `lighting` `rendering` `post-process` `reference` `wip` `client-review` `approved` `revision`
  - Job search: `applied` `phone-screen` `technical-interview` `onsite` `take-home` `offer` `rejected` `ghosted` `follow-up` `referral` `remote` `hybrid` `onsite-only` `contract` `permanent`
  - General: `urgent` `blocked` `waiting` `research` `idea` `question` `admin`
- [x] Tag CRUD + colour picker, merge/rename (rename must not orphan cards), usage counts
- [ ] Tag filter chips on boards; tag-based card colouring
- [ ] Bulk tag/untag (Phase 18)

---

## Phase 5 — Workflow engine `feat/05-workflow`

- [x] `workflows`, `statuses`, `transitions` (from[] → to, "any status" source), per-type workflow FK (no schemes — §B)
- [x] **Execution contract, copied exactly:** conditions fail → button *hidden*; validators fail → button shown, attempt *rejected*, post-functions do **not** run; then commit; then post-functions in fixed order (set status → add comment → update history → reindex → fire event). The hide-vs-reject distinction is *why* Jira's transition UI never shows a button you can't press
- [x] Conditions: Only Assignee, Only Reporter, User in role, **Child-blocking** ("can't close a parent with open children")
- [x] Validators: required-field-on-transition (90% of real use)
- [x] Post-functions: **set resolution** (the load-bearing one — §E), assign to, clear/copy/update field, add comment, fire event, webhook
- [x] Transition screens (prompt for fields, e.g. resolution + comment on Done)
- [x] Validity enforced on API + board drag (bulk/automation land with those phases)
- [ ] Visual workflow editor (nodes + edges) — deferred to the frontend phase; the API is complete
- [x] **Seed workflows** — the third proves domain-neutrality:
  - Programming `[SW]`: To Do → In Progress → In Review → Done (+ Blocked, + Reopen)
  - 3D: Concept → Blockout → Modeling → UV/Texture → Rigging → Render → Review → Approved
  - Job search: Interested → Applied → Phone Screen → Interview → Take-home → Offer → Accepted/Rejected/Ghosted

---

## Phase 6 — AQL: query language `feat/06-aql`

**This is the product.** Boards, filters, dashboards, gadgets, automation, quick filters, and reports are all AQL + a renderer. Build it properly, early, once — it is the highest-reuse component in the system.

- [x] Hand-written recursive-descent parser → AST → **parameterised SQL**. `keyword()` takes `&'static str`, so the injecting version does not compile
- [x] Operators: `= != > >= < <= IN "NOT IN" ~ !~ IS "IS NOT" WAS "WAS IN" "WAS NOT" CHANGED`
- [x] Keywords: `AND OR NOT EMPTY NULL ORDER BY ASC DESC`; history modifiers `AFTER BEFORE BY DURING ON FROM TO`
- [x] Grammar rules enforced at type-check: `IS` only with EMPTY/NULL; `=`/`!=` rejected on text fields; ordering ops only on orderable fields
- [x] Functions: `currentUser() now() startOf*/endOf*()` with relative args; `membersOf() linkedCards() watchedCards() cardHistory()` (cycle functions land with Phase 10)
- [x] **Scope `WAS`/`CHANGED` to 6 fields** (assignee, milestone, priority, reporter, resolution, status) — generic any-field history search forces indexing every change and wrecks the planner. `status CHANGED FROM "In Progress" TO "Done" AFTER -7d` is the high-value case
- [x] Field→operator support matrix enforced at type-check, errors carry a column span
- [ ] SQLite **FTS5** for `~` — currently LIKE with escaped wildcards; FTS5 upgrade deferred
- [x] Saved filters + `filter = "My Filter"` composition, with a cycle guard and depth cap
- [ ] Basic (dropdown) search ⇄ advanced round-trip — deferred to the frontend phase
- [x] `/search/validate` returns errors-with-spans; full autocomplete deferred to the frontend phase
- [x] Fuzz + property tests: parser never panics; SQL byte-identical across injection payloads; access wrap covers every subquery

---

## Phase 7 — Frontend foundation `feat/07-frontend-core`

- [ ] Vite + React 19 + TS strict; path aliases
- [ ] **Design system from real ADS token values** (`docs/research/atlassian-design-system.md`): full colour ramps, semantic light/dark layers, 4px-base spacing scale, radius, elevation, type scale
- [ ] Dark mode via `data-theme`; system-preference default
- [ ] Primitives: Button (primary/default/subtle/link/danger/warning), Input, Select, Checkbox, Radio, Textarea, Modal, Dropdown, Tooltip, Popover, Tabs, Toast, Banner, Spinner, Avatar, **Lozenge** (status-category coloured), **Tag chip**, Breadcrumb, EmptyState, Skeleton
- [ ] Icons: Lucide (ISC, 24×24 stroke-based — ADS icons are proprietary *and* stroke-based, not filled: see `corrections.md` #8)
- [ ] App shell: top nav, collapsible left sidebar (240px), content area
- [ ] Routing (TanStack Router — type-safe, URL-driven state for deep board nesting)
- [ ] TanStack Query + generated typed client from OpenAPI
- [ ] Auth flows: login, forced password change, profile
- [ ] **Density calibrated to Jira** — it is information-dense; airy defaults will feel wrong
- [ ] Storybook + a11y addon; visual regression

---

## Phase 8 — Boards `feat/08-boards` ⭐

- [ ] **Board = saved AQL query + config**, not a container — this is why one board can span projects
- [ ] Columns; **many statuses → one column** (required, not optional); WIP limits + breach styling
- [ ] Drag-drop via **pragmatic-drag-and-drop** (Atlassian's own, open-source, built for Jira itself — the authentic feel, with keyboard a11y + auto-scroll)
- [ ] Optimistic reorder + rollback on failure; **must feel instant**
- [ ] Drag triggers transition; **illegal drops rejected** per workflow
- [ ] Swimlanes: Query / Assignee / Parent / Project / None
- [ ] Quick filters (AQL toggles, multi-select AND) — ~50 LOC, punches far above its weight
- [ ] Card layout config (≤3 extra fields — cards degrade past it)
- [ ] Card colours by type/priority/assignee/AQL
- [ ] Backlog view (ranked, drag to cycle)
- [ ] WebSocket live sync → merge pushed events into the Query cache
- [ ] Virtualised long backlogs

### 8b. Nested boards `feat/08b-nested-boards` ⭐ (requested)

- [ ] Clicking a card with children opens **its** board — same component, `parent_id` scoped
- [ ] Breadcrumb: Project → Grandparent → Parent → Card, deep-linkable
- [ ] **Mini-map on the card** (requested): miniature column/card render in the card corner, showing real child distribution by status category
- [ ] Distinct affordance for board-bearing cards: mini-map + child count + progress bar (`3/7 done`)
- [ ] Roll-up: parent progress/estimate aggregates from descendants
- [ ] "Convert card → board" (seed default columns) and "flatten board → card"
- [ ] Guard depth (5) + cycles; friendly errors
- [ ] Drag a card *into* another card = reparent

---

## Phase 9 — Card detail `feat/09-card-detail`

- [ ] Two-column modal + full-page route (Jira layout: main + sidebar)
- [ ] **Inline edit** (click field → edit, no modal)
- [ ] Markdown editor (TipTap — MIT core; see `corrections.md` #12 re: Pro tiers): bold/italic/code/strike, headings, lists, **checklists**, links, inline images, code blocks + highlighting, tables, quotes, emoji
  - Checklists matter disproportionately: for 3D (per-asset steps) and job search (per-application prep) they replace sub-task sprawl
- [ ] **Store markdown source, render at read time, sanitize on render. Never store rendered HTML** 🔒
- [ ] @mentions → notification; **card-key autolink everywhere** (`ATLAS-123` → live link + hover preview) ⭐
- [ ] Comments: add/edit/delete, markdown, reactions
- [ ] Attachments: drag-drop, paste-from-clipboard ⭐, thumbnails, image + PDF preview, versioning
- [ ] Links panel, remote links, watchers, worklogs
- [ ] **History tab** — every field change, who/when/from/to
- [ ] Child cards panel (+ mini board preview)
- [ ] Create modal with sticky "create another"

---

## Phase 10 — Cycles & estimation `feat/10-cycles`

- [ ] `cycles`: name, goal, start, end, state (future/active/closed), project. **Renameable + disableable per project**
- [ ] State machine: future →start→ active →complete→ closed (start requires dates). Note: not strictly one-way — Jira permits reopening a closed sprint (`corrections.md` #7)
- [ ] **Scope snapshots** (§D3):
  ```sql
  card_cycle(card_id, cycle_id, added_at, removed_at NULL, in_scope_at_start BOOL)
  cycle_snapshot(cycle_id, taken_at, card_id, estimate, status_category)  -- daily
  ```
  Committed-vs-completed and scope-creep are **not** derivable from current state afterwards
- [ ] Complete-cycle: carry incomplete → backlog / next / new cycle
- [ ] Estimation field: points/hours/days/t-shirt(XS–XL→numeric)/count/**none**. One field, never two
- [ ] Time tracking (store seconds; parse `2w 3d 4h 30m`)
- [ ] Reports render only when cycles + estimation are both on; **degrade to count-based burndown when estimation=none**

---

## Phase 11 — Secrets vault `feat/11-secrets` 🔒 (requested)

- [x] Master key from env or OS keyring (`keyring` crate); startup fail-fast if absent
- [x] **XChaCha20-Poly1305** AEAD per secret, unique nonce, key id for rotation
- [x] `Secret<T>` wrapper: **redacted `Debug`**, `zeroize` on drop, no `Serialize`. Make leaking a compile error, not a code-review catch
- [x] `api_credentials`: provider, label, ciphertext, nonce, last_validated_at, status, expires_at, scopes, created_by
- [x] **Never return plaintext over the API** — only last-4 + metadata
- [x] Per-provider validation probe (cheap endpoint, no side effects)
- [ ] **Expiry/expired/invalid warnings** (requested): scheduled revalidation, banner + notification, per-key status pill (valid / expiring in N days / expired / invalid / unchecked) — _pill + banner done; scheduled revalidation deferred to Phase 17_
- [x] Settings → Integrations UI: add/replace/delete, validate-now, scope display, last-checked
- [ ] Audit every access; rate-limit validation probes — _access audited; probe rate-limiting deferred_
- [x] Tests: ciphertext never in logs; `Debug` redaction; rotation

---

## Phase 12 — GitHub integration `feat/12-github` (requested)

- [x] PAT storage via vault; validate → `GET /user`
- [x] **Scope + expiry discovery from headers**: `x-oauth-scopes`, `github-authentication-token-expiration`. Parse **both** layouts seen in the wild — `2006-01-02 15:04:05 MST` and `2006-01-02 15:04:05 -0700` (`corrections.md` #3). Missing header = "no expiry info supplied", *not* "never expires" (#5)
- [ ] `octocrab` client + rate-limit handling (`x-ratelimit-*`, secondary limits, retry/backoff)
- [x] Link project → repo; repo picker (paginated)
- [x] ⭐ **Create branch from card** (requested): `POST /repos/{o}/{r}/git/refs` from a base SHA. Configurable naming: `{type}/{key}-{slug}` → `feature/ATLAS-42-add-login`
- [ ] Development panel on card: branches, commits, PRs, checks
- [ ] Create PR from card; PR state (open/closed/**merged**), mergeable, reviews, CI checks
- [ ] **Webhook receiver** (built now so a GitHub App drops in later): HMAC-SHA256 verify `x-hub-signature-256`, **constant-time compare** 🔒, replay guard by delivery id — _receiver + HMAC verify done (mounted `POST /webhooks/github`, `UNGATED_PATHS`); replay guard by delivery id not yet built_
- [ ] Events: `push`, `pull_request`, `check_suite`, `check_run`, `create`, `delete` — _`push` and `pull_request` acted on; `check_suite`/`create`/`delete` parse but are not-yet-acted-on no-ops; `check_run` not parsed_
- [x] ⭐ **Smart commits**: parse `ATLAS-42 #done #comment fixed it #time 2h` → transition/comment/worklog — _parser + application landed, wired to `push` deliveries_
- [x] Auto-transition on PR open → In Review; on merge → Done — _merge → Done; open/reopen → the first In-Progress-category status (Atlas has no named "In Review" state, only the three categories)_
- [ ] Poll fallback when no public webhook URL
- [ ] Sync status + backfill; unlink — _unlink shipped in the repo-linking PR; sync status + backfill not started_

---

## Phase 13 — Claude Code sessions `feat/13-claude-agent` ⭐ (the point of the product)

Every flag below is verified against the local CLI (v2.1.211) — see `docs/research/claude-code-cli.md`.

- [ ] `AgentRunner` trait; `LocalRunner` now, `DockerRunner` later (as decided)
- [ ] Workspace manager: clone repo → `~/.atlas/workspaces/{project}`, background job + progress, fetch/pull, disk quota
- [ ] Spawn: `claude -p "<task>" --output-format stream-json --verbose`
  - **`--verbose` is mandatory** with `-p` + `stream-json`; the CLI hard-errors without it
- [ ] NDJSON parser for stdout (stdout is clean JSON; **all warnings go to stderr** — parse them separately)
- [ ] Event types: `system`(init) · `assistant` · `user`(tool *results* arrive as `user`, not a distinct type) · `stream_event` · `rate_limit_event` · `result`
- [ ] 🚩 **Branch on `is_error` + `terminal_reason`, never `subtype`** — the CLI emits `subtype:"success"` *with* `is_error:true` on API/auth failure
- [ ] 🚩 `result` key is **absent** (not null) on error subtypes → `Option<String>` in Rust
- [ ] 🚩 **`--resume` is CWD-scoped** — must respawn with the identical repo cwd or it fails with non-JSON stderr + exit 1
- [ ] Auth: inherit `~/.claude/.credentials.json` (OAuth, `apiKeySource:"none"`). ⚠️ Setting `ANTHROPIC_API_KEY` **silently overrides the Max subscription and bills the API** — surface this in the UI as an explicit choice
- [ ] Capture `total_cost_usd`, `usage`, `modelUsage`, `num_turns`, `duration_ms` per run → cost dashboard
- [ ] `process-wrap` 9.1.0 for process groups (**not** `command-group` — deprecated in favour of it, `corrections.md` #10); graceful cancel, orphan prevention, timeout
- [ ] `tokio::sync::broadcast` → N WebSocket subscribers; ring buffer for late joiners
- [ ] Live session UI: streaming transcript, tool-call rendering, cost/token meter, cancel button
- [ ] Persist transcripts; resume across restarts; per-session status on card
- [ ] ⭐ **Atlas MCP server** via `--mcp-config` (verified end-to-end): expose `atlas_move_card`, `atlas_comment_card`, `atlas_create_card`, `atlas_get_card`, `atlas_set_status`, `atlas_add_tag` — this is how the agent **moves its own card when the task completes** (requested)
- [ ] Card → task binding: "Run with Claude" on a card sends summary + description + acceptance criteria as the prompt
- [ ] On completion: post transcript summary as a comment, move card to the configured destination column, attach the branch/PR
  - > **Flagged for confirmation:** the request says "move the card to the backlog" on completion. Backlog is an unusual destination for *finished* work — In Review or Done is the conventional target. Built as a **per-project configurable destination**, defaulting to In Review, with backlog selectable.
- [ ] Permission mode per project (`default` / `acceptEdits` / `plan` / `bypassPermissions`), `--allowedTools`/`--disallowedTools` 🔒 — default to the *least* permissive that works, and make `bypassPermissions` a deliberate opt-in with a warning
- [ ] Concurrency cap; queue when saturated
- [ ] Session history per card; re-run; diff review before PR

---

## Phase 14 — Gemini integration `feat/14-gemini` (requested)

- [ ] API key via vault; validate via `GET /v1beta/models` (cheap, no side effects)
- [ ] **Model: `gemini-3.1-flash-image-preview`** (Nano Banana 2). ⚠️ **Not** `gemini-2.5-flash-image` — confirmed shutdown 2026-10-02 (`corrections.md` #2)
- [ ] Auth header `x-goog-api-key` (preferred over `?key=` — keeps the key out of URLs and logs) 🔒
- [ ] ⭐ **Project cover art / avatar generation** (requested): prompt from project name + type, style presets, aspect ratio, regenerate, pick-from-grid
- [ ] Card cover images; 3D reference-image generation
- [ ] Base64-inline response → store to `uploads/`, thumbnail
- [ ] Distinguish invalid-key / quota-exceeded / transient from the error shape; backoff on transient only
- [ ] Handle safety-filter fields (`finishReason`, `promptFeedback`) with a clear UI message
- [ ] Cost tracking per generation
- [ ] Optional text model for card-description drafting / AQL natural-language → query

---

## Phase 15 — Automation `feat/15-automation`

Where a solo user recovers the labour of the team they don't have. **1 trigger + N conditions + N actions.**

- [ ] Triggers: card created, transitioned, field changed, assigned, commented, **scheduled** (interval/cron + AQL), manual, incoming webhook
- [ ] Conditions: AQL, card fields, **if/else block**, user, smart-value regex
- [ ] Actions: edit, transition, comment, assign, create card, create children, clone, link, log work, watchers, send email, **send web request**, create variable, **lookup cards** (AQL, ≤100)
  - One generic web-request action beats 20 bespoke integrations
- [ ] Smart values, syntax copied exactly: `{{card.key}}` `{{card.status.name}}` `{{card.assignee.displayName}}` `{{changelog.status.fromString}}` `{{now.plusDays(7)}}` `{{missing.field|"fallback"}}`, list iteration `{{#card.comments}}{{body}}{{/}}`
- [ ] **Guardrails:** loop detection, per-rule execution cap, **audit log with smart-value resolution trace** (the only reason Jira's automation is debuggable), rule-actor attribution in history
- [ ] Rule builder UI + dry-run
- [ ] Seeded rules — the job-search one is a genuine killer app:
  - Job search: daily `status = Applied AND updated <= -7d` → comment "Follow up?" + set due date
  - 3D: → Approved → set resolution, create child "Export for delivery"
  - Programming `[SW]`: `type = Bug AND priority = Highest` → assign + add to current cycle

---

## Phase 16 — Reports & dashboards `feat/16-reports`

- [ ] ⭐ **One daily snapshot table** (`date, card_id, status_category, estimate, cycle_id`) — CFD, burndown, burnup, velocity, average-age, and control chart are then all a GROUP BY. Do **not** replay the changelog at read time; that is why Jira's reports are slow
- [ ] Cumulative Flow Diagram `[GEN]` ⭐ — best general report; works for job search (applications by stage)
- [ ] Created vs Resolved `[GEN]` ⭐ — trivial, universally useful
- [ ] Burndown + burnup (burnup shows scope change; strictly more informative)
- [ ] Cycle report (committed vs completed vs added), velocity `[SW]`
- [ ] Control chart (cycle time + rolling avg) `[GEN]` — great for continuous flow
- [ ] Average age `[GEN]` — "how long since I applied"
- [ ] Charts: Recharts
- [ ] Dashboards: CRUD, grid layout, drag-drop gadgets
- [ ] **Gadgets are just saved-filter renderers** — build 4 renderers (table/pie/bar/stat) over the AQL AST and most of the gadget list is free
- [ ] Two-Dimensional Filter Statistics (x × y counts — e.g. status × company for job search)
- [ ] ⭐ **Agent cost dashboard**: Claude/Gemini spend per project/card/period

---

## Phase 17 — Notifications `feat/17-notifications`

- [ ] In-app centre (bell, unread, mark-read), WebSocket push
- [ ] @mention notification
- [ ] Email (lettre + SMTP config in vault) on assign/mention/comment
- [ ] ⭐ **"Don't notify me of my own changes"** — without it a solo user is spammed by themselves and disables everything. Table stakes here, and Jira's best-hidden setting
- [ ] Event batching/dedup (coalesce same-card events in a window)
- [ ] Per-user prefs per event: in-app / email / off
- [ ] Agent notifications: session done/failed/needs input; PR merged
- [ ] Daily digest

---

## Phase 18 — Bulk, import/export, templates `feat/18-bulk-import`

- [ ] Bulk edit/transition/delete/move (workflow-valid), 1000-cap, **preview-before-apply**, progress
- [ ] ⭐ **CSV import wizard** — how a job-search spreadsheet becomes an Atlas project; onboarding depends on it
  - Summary the only required column; repeated headers for multi-value; `Issue ID`+`Parent` with parents preceding children; `Parent -> Child` cascading
  - Improve on Jira: **validate + preview before commit** (Jira fails halfway and leaves a mess), auto-suggest mappings from headers
- [ ] CSV export; JSON export/import (round-trip → backup + templates)
- [ ] ⭐ **Project templates** — sets types + workflow + fields + tags + board in one click. A "Job Search" template is a *product feature*, not config; this is where Atlas beats Jira for the stated use cases
  - Programming `[SW]` · 3D asset pipeline · Job search · Blank
- [ ] Card templates (pre-filled create)
- [ ] Archive project/card; trash + restore

---

## Phase 19 — Polish & the "feels like Jira" layer `feat/19-polish`

Individually cheap, collectively the reason power users tolerate Jira. Underrating these is the classic clone mistake.

- [ ] Keyboard shortcuts: `c` create · `/` search · `?` help · **`.` command palette** ⭐ · `g d/p/i/a` go-to · `j/k` next/prev · `o` open · `e` edit · `a` assign · `i` assign-to-me · `m` comment · `l` labels · `Esc` cancel
- [ ] `.` **command palette is the single highest-value item** — modern users expect Cmd-K, and it subsumes half the shortcut list
- [ ] Hover preview on card links; relative timestamps + absolute on hover
- [ ] ⭐ **Toast + undo on destructive/bulk ops** — Jira lacks undo; easy win
- [ ] Unsaved-changes guard; recently-viewed
- [ ] Deep-linkable everything (board/filter state in URL)
- [ ] a11y: WCAG 2.1 AA, focus rings, keyboard DnD, screen-reader labels, reduced-motion
- [ ] Perf: virtualise, code-split, budget (board < 1s to interactive)
- [ ] Empty states, error boundaries, offline banner
- [ ] Playwright E2E: signup→board→card→agent→PR
- [ ] Full-app dark mode audit

---

## Phase 20 — Ship `feat/20-ship`

- [ ] Backup/restore (SQLite `VACUUM INTO` + attachments tarball), scheduled
- [ ] `atlas` binary embedding the frontend (single-file deploy); Docker image; compose
- [ ] Migration runner on boot; version endpoint
- [ ] 🔒 Security pass: rate limits, security headers (CSP, HSTS, X-Frame-Options), dependency audit (`cargo audit`, `npm audit`), secret-scan CI, upload validation (type/size/path traversal), SSRF guard on webhook/remote-link fetches
- [ ] Load test: 10k cards, 50 columns
- [ ] Docs: install, config, integration setup, backup, AQL reference, keyboard shortcuts
- [ ] `docs/adr/` for the decisions above

---

## Deliberately cut

Jira features that are enterprise cruft at this scale. Each is a considered decision, not an oversight:

**Permission schemes** (40+ permissions × ~8 grantee types × N schemes → 3 roles) · **Notification schemes** (→ per-user prefs) · **Screens / Screen Schemes / Issue Type Screen Schemes** (→ field layout per type) · **Field Configuration Schemes** · **Workflow Schemes** · **Issue Type Schemes** · **Issue security levels** · **Project roles as distinct from groups** (two parallel systems) · **All of JSM** (SLAs, approvals, request types, portals, Assets/AQL) · **DevOps/Compass/Loom/Figma automation triggers** (~60% of Jira's trigger surface, zero loss here) · **Advanced Roadmaps / cross-project plans** · **Capacity planning** · **User/version workload reports** · **Comment visibility (internal vs public)** · **Board admins distinct from project admins**

---

## Progress

| Phase | Branch | Status |
|---|---|---|
| 0 Foundation | `feat/00-foundation` | ✅ merged (#1) |
| 1 Backend core | `feat/01-backend-core` | ✅ merged (#1) |
| 2 Auth | `feat/02-auth` | ✅ |
| 3 Domain | `feat/03-domain` | ✅ |
| 4 Tags | `feat/04-tags` | ✅ |
| 5 Workflow | `feat/05-workflow-aql` | ✅ (editor UI with frontend phase) |
| 6 AQL | `feat/05-workflow-aql` | ✅ (autocomplete UI with frontend phase) |
| 7 Frontend core | `feat/07-frontend-boards` | ✅ |
| 8 Boards | `feat/07-frontend-boards` | ✅ |
| 8b Nested boards | `feat/07-frontend-boards` | ✅ mini-map + nested nav |
| 9 Card detail | `feat/07-frontend-boards` | ✅ |
| 10 Cycles | `feat/10-cycles` | ⬜ |
| 11 Secrets | `feat/11-secrets` | ✅ (#12) |
| 12 GitHub | `feat/12-github` | 🚧 vault→PAT, repo linking, create-branch, card panel (#12, #15, #16) |
| 13 Claude agent | `feat/13-claude-agent` | ⬜ |
| 14 Gemini | `feat/14-gemini` | ⬜ |
| 15 Automation | `feat/15-automation` | ⬜ |
| 16 Reports | `feat/16-reports` | ⬜ |
| 17 Notifications | `feat/17-notifications` | ⬜ |
| 18 Bulk/import | `feat/18-bulk-import` | ⬜ |
| 19 Polish | `feat/19-polish` | ⬜ |
| 20 Ship | `feat/20-ship` | ⬜ |

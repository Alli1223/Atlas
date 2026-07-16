# Corrections from adversarial verification

The research agents' non-verified claims were independently re-checked by agents instructed to
*refute* them. These 12 claims did not survive. They are recorded because each one would
otherwise have been copied into the implementation verbatim.

## 1. Refuted claim

> REST JSON accepts BOTH camelCase and snake_case for field names (proto3 JSON mapping) — the docs' own curl examples mix them, e.g. "inline_data"/"mime_type" in one example and inlineData/mimeType in the reference. Responses come back camelCase. Serde structs should be #[serde(rename_all = "camelCase")] for decoding.

**Correction:**

The underlying mechanism is right for generateContent, but the claim is stale and overbroad for the stated area (image generation from Rust via reqwest), and its blanket serde rule will break code.

WHAT HOLDS (verified):
- Proto3 JSON mapping dual-acceptance is real. protobuf.dev/programming-guides/json: "Parsers accept both the lowerCamelCase name (or the one specified by the json_name option) and the original proto field name" and "By default the protobuf JSON printer should convert the field name to lowerCamelCase and use that as the JSON name."
- The generateContent reference does mix casing: ai.google.dev/api/generate-content has a curl example with "inline_data"/"mime_type" (inline_data x2, mime_type x14) next to schema tables using mimeType (x14), generationConfig (x4), responseModalities (x2). generateContent responses are camelCase (usageMetadata, promptTokenCount, candidatesTokenCount).
- So for the :generateContent endpoint, #[serde(rename_all = "camelCase")] is correct for decoding.

WHY REFUTED:
1. The cited evidence is stale. The live ai.google.dev/gemini-api/docs/image-generation contains ZERO occurrences of inline_data or inlineData. It now documents a different endpoint: POST https://generativelanguage.googleapis.com/v1beta/interactions with model gemini-3.1-flash-image (Nano Banana). The page states: "This version of the page covers the Interactions API. You can use the toggle on this page to switch to the generateContent API version of this page." Both paths exist; Interactions is the page default. The toggle is client-side JS, so ?api= query params do not switch it.
2. The Interactions API is snake_case, not camelCase. Its reference (ai.google.dev/api/interactions-api) has zero camelCase field names: mime_type (22), response_format (6), aspect_ratio (4), previous_interaction_id (2), thinking_level (1), output_image, output_text — and 0 hits for mimeType/responseFormat/aspectRatio/previousInteractionId/thinkingLevel/outputImage. Corroborating signal: the JavaScript examples also use snake_case (mime_type: "image/png", previous_interaction_id, response_format, image_size), whereas @google/genai (v2.12.0) uses camelCase for generateContent. Applying rename_all = "camelCase" to Interactions structs would fail to decode.
3. output_image / output_text are explicitly annotated "Note: this is added by the SDK" — SDK-synthesized convenience fields, NOT wire fields. A plain reqwest client will not receive them and must parse the raw outputs structure instead.

CORRECT GUIDANCE: Pick the endpoint first. For v1beta/{model}:generateContent, use #[serde(rename_all = "camelCase")]; snake_case is also accepted on request bodies via proto3 mapping, but responses are camelCase. For v1beta/interactions (the currently-documented image-generation default), use snake_case serde field names (no rename_all, or rename_all = "snake_case"), and do not model output_image/output_text as wire fields.

CONFIDENCE CAVEAT: no API key was available, so the Interactions wire response casing was not confirmed by a live call; it rests on the reference docs plus SDK-convention evidence. The staleness of the cited evidence is definitive regardless.

## 2. Refuted claim

> gemini-2.5-flash-image (legacy Nano Banana) is reported to shut down October 2, 2026, but the date could not be confirmed in raw ai.google.dev docs (only 'strongly recommend transition' language), so it should be treated as unconfirmed — though new code should not be built on 2.5-flash-image regardless.

**Correction:**

The October 2, 2026 shutdown date IS confirmed in primary Google documentation and should NOT be treated as unconfirmed. https://ai.google.dev/gemini-api/docs/deprecations (and raw mirror deprecations.md.txt) contains an explicit table row: `gemini-2.5-flash-image` | Release date: October 2, 2025 | Shutdown date: October 2, 2026 | Recommended replacement: `gemini-3.1-flash-image-preview`. Additional confirmed facts the original claim omitted: (1) the official replacement is gemini-3.1-flash-image-preview (Nano Banana 2); (2) the distinct model gemini-2.5-flash-image-preview was already shut down on January 15, 2026 per the changelog — do not conflate it with stable gemini-2.5-flash-image. Sole legitimate caveat: Google frames shutdown dates as the "earliest possible retirement date," so it is a committed floor rather than a guaranteed execution date — a far weaker qualification than "unconfirmed." The claim's operational advice (do not build new code on 2.5-flash-image) is correct and now rests on a confirmed date. Target gemini-3.1-flash-image-preview, noting its -preview suffix implies churn risk worth isolating behind a config constant.

## 3. Refuted claim

> The `GitHub-Authentication-Token-Expiration` response header indicates a PAT's expiration date. Announced 2021-07-26: 'When using a personal access token with the GitHub API, you'll see a new response header, GitHub-Authentication-Token-Expiration, indicating the token's expiration date.' Value format is NOT RFC3339 — it is Go layout `2006-01-02 15:04:05 -0700`, e.g. `2025-09-05 17:55:53 +0500` or `... UTC`.

**Correction:**

The header's existence, purpose, the 2021-07-26 announcement date, the verbatim changelog quote, and "not RFC3339" are all CORRECT. The format specification is WRONG and would break an implementation.

There is no single layout. TWO layouts co-exist in the wild, and the claim names the LESS common one as canonical while giving an example the named layout cannot parse:

1. Named timezone (primary/most common on github.com): Go `2006-01-02 15:04:05 MST` — e.g. `2026-06-03 19:52:44 UTC`
2. Numeric offset (variant): Go `2006-01-02 15:04:05 -0700` — e.g. `2025-09-05 17:55:53 +0500`, `2023-04-26 23:23:18 +0200`

The claim is internally inconsistent: layout `2006-01-02 15:04:05 -0700` CANNOT parse `... UTC`, yet the claim offers `... UTC` as an example of it. go-github's live parser (github/github.go:1160-1172) tries `MST` FIRST, then falls back to `-0700` with the comment "Some tokens include the timezone offset instead of the timezone" (ref go-github#2649). refined-github (github-helpers/github-token.ts:106) handles ONLY the `UTC` form — evidence the named-zone form is the common github.com case, i.e. exactly what the claim's layout fails on.

For a Rust/chrono implementation: a single `%Y-%m-%d %H:%M:%S %z` fails on `UTC` values. Must try `%Y-%m-%d %H:%M:%S %Z`-style/named-zone handling AND `%z`, or normalize (replace " UTC" with "+0000") before parsing.

Two additional facts the claim omits, both load-bearing for Atlas:
(a) The format is UNDOCUMENTED. It appears in NO official GitHub doc — verified absent from both `token-expiration-and-revocation` and `managing-your-personal-access-tokens`. The only official source is the 2021 changelog, which specifies no format. It is reverse-engineered and not a stable contract; do not treat parse failure as fatal.
(b) The header is ABSENT for non-expiring tokens (verified live: `curl -I api.github.com/user` with a non-expiring `gho_` token returns zero occurrences). Absence means "no expiry," not an error — go-github encodes this as the `0001-01-01` sentinel.

Also: the cited go-github#3708 is not a format source. It reports a GitHub-side bug (closed 2025-10-05) where fine-grained PATs returned the CURRENT SERVER TIME instead of the real expiry, making the value itself untrustworthy for FG-PATs independent of parsing.

## 4. Refuted claim

> Reported bug (google/go-github#3708, filed 2025-09-05): for fine-grained PATs the github-authentication-token-expiration header returns the CURRENT SERVER TIME instead of the real expiry. Single reporter, no GitHub confirmation, no resolution documented. Classic PATs reportedly return the correct value.

**Correction:**

google/go-github#3708 was a real but TRANSIENT GitHub server-side regression affecting fine-grained PATs for roughly 2025-09-03 to 2025-09-12. It is fixed and the issue is CLOSED (closed_at 2025-10-05T11:47:15Z), verified via `gh api repos/google/go-github/issues/3708` ("state":"closed"). The claim is wrong on four counts: (1) "no resolution documented" is false — commenter maviger reported on 2025-10-05 that the issue stopped occurring as of Sept 12 and the expected output is now returned; maintainer gmlewis closed it on that basis and the original reporter (amanfcp) acknowledged. (2) "Single reporter" is false — the linked upstream GitHub community discussion #172213 has ~7 independent reporters (lann, sebabarre-evaneos, liondadev, ventz-lgtm, HerrKvarkar, 333-Spur-Tmps, simonjur). (3) The symptom was not literally "current server time" — the header returned a value approximately ONE MINUTE in the future; the defect also affected only REST, while GraphQL returned correct expiry throughout. (4) It was never a go-github library bug — gmlewis noted on 2025-09-05 that the client can only surface what GitHub's servers send; the fault was in the GitHub API itself. The durable, still-true caveat for Atlas is different and more important: `github-authentication-token-expiration` is UNDOCUMENTED on docs.github.com's authentication/REST pages, and it is absent entirely for OAuth app tokens (verified locally: a `gho_` token against https://api.github.com/user returns no such header) and for GitHub App installation tokens. Treat the header as advisory-only and always handle its absence gracefully — but do NOT design around the Sept 2025 bug, which no longer reproduces.

## 5. Refuted claim

> The `GitHub-Authentication-Token-Expiration` header is absent entirely for tokens configured with no expiration. Atlas must treat 'header missing' as 'no known expiry', not as an error.

**Correction:**

The header's behavior on non-expiring tokens is undocumented by GitHub, and "missing header" does NOT mean "no expiry" — it means "no expiry information was supplied."

Verified facts:
1. GitHub does not document this header at all outside a single 2021 changelog post (https://github.blog/changelog/2021-07-26-expiration-options-for-personal-access-tokens/), which only says: "When using a personal access token with the GitHub API, you'll see a new response header, GitHub-Authentication-Token-Expiration, indicating the token's expiration date." A full clone of github/docs (3,724 content .md files) contains ZERO occurrences of "Authentication-Token-Expiration" — including token-expiration-and-revocation.md and the REST auth pages. There is no primary source stating the absent-on-no-expiration behavior, so it is unspecified, unguaranteed behavior, not a contract Atlas can rely on.
2. Empirically the header is omitted (not empty-valued) when the credential has no expiration: `curl -sI https://api.github.com/user` with a non-expiring OAuth token (gho_) returned HTTP/2 200 with x-oauth-scopes present and no github-authentication-token-expiration header at all. So absence-on-no-expiry is plausible, but this was an OAuth token, not a PAT with "No expiration" — I could not test that case directly.
3. The load-bearing error is the inference. Header absence is not diagnostic of "never expires": it is equally absent for GitHub App installation access tokens (which expire in 1 hour), GITHUB_TOKEN in Actions, unauthenticated requests, Basic auth, and behind proxies/GHES that strip or don't emit it. Atlas should model this as `expires_at: Option<DateTime>` meaning UNKNOWN, and must not render/decide "this token never expires" from a missing header.
4. Presence is also not trustworthy: from roughly Aug–12 Sep 2025 GitHub returned the *current server time* in this header for fine-grained PATs (github_pat_*) instead of the real expiry (google/go-github#3708; community discussion #172213). Atlas should sanity-check that any parsed value is meaningfully in the future before alerting on it.
5. Context correction: "no expiration" is not a classic-PAT-only concern — fine-grained PATs gained an optional/no-expiration setting in Oct 2024 (https://github.blog/changelog/2024-10-18-new-pat-rotation-policies-preview-and-optional-expiration-for-fine-grained-pats/) and went GA Mar 2025, so both PAT types can hit this path.

The only part of the claim that survives: Atlas must not treat a missing header as an error condition. That is correct — but the correct semantic is "expiry unknown", not "no expiry".

## 6. Refuted claim

> Sub-tasks are structurally special-cased throughout Jira: own issue-type class (subtaskIssueTypes() vs standardIssueTypes() in JQL), cannot have children, excluded from board columns by default, and cannot be moved between parents in bulk — a legacy artifact that Advanced Roadmaps retrofits a configurable hierarchy table on top of.

**Correction:**

Two of the four load-bearing sub-claims are factually wrong, and the Advanced Roadmaps characterisation is wrong.

VERIFIED (keep these):
- JQL type classes are real. Atlassian's Advanced searching functions reference (the cited page) defines: standardIssueTypes() = "Perform searches based on 'standard' Issue Types, that is, search for issues that are not sub-tasks"; subtaskIssueTypes() = "Perform searches based on issues that are sub-tasks." Both take IN/NOT IN only, on the Type field. So the type table genuinely carries a boolean subtask flag.
- Sub-tasks cannot have children (sub-task level is terminal, level -1).

REFUTED #1 — "excluded from board columns by default" is FALSE for company-managed boards. Atlassian KB "How to configure a board (Kanban or Scrum) to hide sub-tasks" states verbatim: "By default, Kanban boards or Sprints from Scrum boards display any type of issues including subtasks." Hiding them requires deliberately editing the board's saved filter to add `AND issuetype != Sub-task`. Sub-tasks are cards in board columns out of the box. What is true: (a) the *backlog* view shows only standard issues; (b) the "Stories" swimlane method renders parents as swimlane headers and sub-tasks as the cards inside them ("One parent work item per swimlane containing all of the subtasks, with work items that have no sub-tasks appearing below" — Configure swimlanes doc), which is what people mistake for exclusion; (c) team-managed boards do hide sub-tasks. So the correct statement is "board *backlog* excludes sub-tasks; boards do not."

REFUTED #2 — "cannot be moved between parents in bulk" is FALSE. Bulk reparenting exists in at least two documented paths: (a) Jira DC/Server Bulk Move ("Editing multiple issues at the same time"): "You can bulk move both standard issues and sub-tasks to another project and issue type, as well as convert a sub-task to an issue and vice versa" — the convert-to-sub-task step takes a parent key; (b) Jira Cloud plans: "navigate to Bulk actions, then Parent, and choose the parent work item to which you'd like to move the selected child work items." The real constraint is narrower and different: one explicit parent key per bulk batch (all selected items get the same new parent), the parent must be in the same project, and plan-level bulk parent edits require items at the same hierarchy level and not the top level. That is a UX limitation, not a structural prohibition.

REFUTED #3 — the Advanced Roadmaps framing is backwards. AR/Plans does NOT retrofit a uniform configurable hierarchy over sub-tasks; it explicitly cannot touch them. The hierarchy configuration only permits adding levels **above** epic (level 1) via Parent Link. Atlassian documents that you cannot add levels between epic/story/sub-task or below sub-task; the Story(0)/Sub-task(-1) core is fixed and AR leaves the sub-task special case entirely intact — sub-tasks aren't even a plan hierarchy level. So AR is evidence that the sub-task special case is NOT abstractable within Jira's model, not evidence that it was abstracted.

For an Atlas parity inventory, the defensible MUST-tag statement is: sub-tasks are a distinct issue-type class (boolean flag on the type, exposed to JQL via standardIssueTypes()/subtaskIssueTypes()), are terminal (no children), must live in the same project as their parent, and are excluded from the backlog view but not from boards; bulk reparenting is supported but only to a single target parent per batch. The editorialising about a "uniform parent pointer with a configurable hierarchy table" being what AR does should be dropped — it is an unsubstantiated design opinion attached to a false factual premise.

## 7. Refuted claim

> Sprint lifecycle: sprints have three states — future, active, closed — reachable via create → start (with name, goal, start/end dates) → complete. On completion, incomplete issues are moved to a chosen destination: the backlog, the next planned sprint, or a new sprint ('carry-over'). Velocity is computed only from issues Done at completion time. Parallel/multiple active sprints per board is an opt-in setting. Sprint membership is historical — a completed sprint retains its issue list for the sprint report, so sprint must be a many-to-many join with a captured completion snapshot, not a simple FK.

**Correction:**

The claim is ~80% correct but describes the state machine as one-way, which is wrong and is the part most likely to be encoded incorrectly.

CORRECT (independently verified):
- The three states future/active/closed are right. Agile REST API (developer.atlassian.com/cloud/jira/software/rest/api-group-sprint/) confirms state values future|active|closed and fields id, self, state, name, startDate, endDate, completeDate, originBoardId, goal. The API permits future→active (requires startDate and endDate set) and active→closed.
- Carry-over destinations are right, and the verbatim Cloud UI options are: "Backlog", "Any future sprint" (i.e. any already-created future sprint, not merely "the next planned sprint"), and "New sprint" (support.atlassian.com/jira-software-cloud/docs/complete-a-sprint/).
- Velocity "Completed" (green bar) = "the total completed estimates when the sprint ends" — so keying off Done-at-completion is right. Two refinements: (a) the Velocity Chart also stores "Commitment" (gray bar) = total estimate of all items in the sprint *at start*, which is a second snapshot the schema must capture, not just the completion one; (b) subtask estimates are excluded — only standard-level items (story/bug/task) count; (c) velocity itself is the *average* of completed estimates across recent sprints, not a per-sprint value.
- Parallel sprints is opt-in — correct in substance.
- Many-to-many with historical membership is correct and confirmed by Atlassian's own KB: the Sprint field is multi-valued and deliberately retains completed sprints because Jira "is trying to maintain the history of where the Scrum Master planned to complete the issue but did not," ensuring "historic Sprint reports are accurate."

WRONG / MISSING:
1. `closed` is NOT terminal. A completed sprint can be reopened back to `active` via Reports → Sprint Report → More actions (…) → Reopen sprint (support.atlassian.com/jira-software-cloud/docs/reopen-a-sprint/). Requires Jira admin or Manage Sprints permission. The reopen event is recorded in the sprint report and the report adopts a NEW end date. So the state machine is create → start → complete → (optionally) reopen → complete, and the "captured completion snapshot" must be revisable/versioned rather than write-once. Reopening also restores the sprint's completed and incomplete items, except items that were moved into an active sprint in the interim, which stay put.
2. Parallel sprints is NOT a per-board setting. It is a global/instance admin toggle: Settings → Jira apps → Jira configuration → "Parallel sprints" for company-managed projects (on Server/DC: Administration → Applications → Jira Software → Jira Software Labs → "Parallel Sprints" checkbox). Scoping it per-board in a clone would be a modeling error.
3. Parallel sprints and reopen are coupled: if another sprint is already active, parallel sprints must be enabled before a closed sprint can be reopened.
4. Additional completion constraints the claim omits: a sprint cannot be completed if a parent item is Done while its subtasks are not; and for sprints shared across multiple boards, only items from the completing board carry over to the future/new sprint — incomplete items from other boards are forced to the Backlog.

## 8. Refuted claim

> Lucide is 24×24 STROKE-based (stroke-width 2, round caps) — a different construction from ADS's 16px filled glyphs. Phosphor ships a `fill` weight and a 256×256 viewBox. Neither reproduces ADS optically; Lucide at 16px with stroke-width 2 is the closest practical match for visual density, and is ISC-licensed (permissive).

**Correction:**

The central premise — "ADS's 16px filled glyphs" — is false, and it invalidates the reasoning built on it.

VERIFIED TRUE (independently, from package source):
- Lucide (lucide-react@1.24.0 / lucide-static): defaults are viewBox="0 0 24 24", fill="none", stroke="currentColor", stroke-width="2", stroke-linecap="round", stroke-linejoin="round". License ISC (npm metadata + bundled LICENSE).
- Phosphor (@phosphor-icons/core): ships six weights — thin, light, regular, bold, fill, duotone — all on viewBox="0 0 256 256". (@phosphor-icons/react is MIT.)

REFUTED — ADS icons are NOT filled glyphs:
Atlassian's current icon set is 16px OUTLINED forms drawn with a 1.5px stroke, pairing rounded corners with sharp interior corners and SQUARE line caps. Primary sources: atlassian.design/whats-new/building-atlassians-new-icon-system/ ("In our legacy system, icons were drawn with a 2px stroke on a 24px canvas, making them visually heavy"; new = "1.5px stroke" on a "16×16 pixel" canvas, "lighter-weight outlined forms") and atlassian.design/foundations/iconography ("a 1.5px stroke width with shapes that pair rounded corners with sharp interior corners and square line caps"; sizes: 16px medium/default, 12px small).

Package-level proof from @atlaskit/icon@37.0.0, core/add.js:
  <path fill="currentcolor" d="M8.75 1.5v5.75h5.75v1.5H8.75v5.75h-1.5V8.75H1.5v-1.5h5.75V1.5z"/>
The plus bar spans 7.25→8.75 = exactly 1.5 units on a 16 canvas. The fill="currentcolor" is a codegen artifact (strokes flattened into fill paths via dangerouslySetGlyph), NOT the optical construction. The claim mistook the SVG export mechanic for the design language.

CONSEQUENCES:
1. "A different construction from stroke-based Lucide" is wrong — ADS and Lucide are the SAME family (outlined stroke forms). There is no construction gap to bridge; that is precisely why Lucide is the right pick. The conclusion is accidentally right for the wrong reason.
2. "Neither reproduces ADS optically" overstates the gap. Lucide matches ADS's outlined construction; only stroke weight and cap style need tuning.
3. The recommended parameter is wrong arithmetic. Lucide's 24 viewBox rendered at size=16 scales strokes by 16/24, so strokeWidth={2} renders at 2 × 16/24 = 1.333px — about 11% THINNER than ADS's 1.5px, not the closest match. lucide-react's formula (dist/cjs/lucide-react.js line 81) is: absoluteStrokeWidth ? strokeWidth * 24 / size : strokeWidth.

CORRECT SETTINGS to approximate ADS with Lucide at 16px:
  <Icon size={16} strokeWidth={2.25} strokeLinecap="square" />
  (equivalently strokeWidth={1.5} with absoluteStrokeWidth, which computes 1.5 × 24/16 = 2.25 user units → 1.5px rendered)
Also override strokeLinecap/strokeLinejoin to "square" — Lucide defaults to round, ADS uses square line caps. Note Lucide's 24-canvas geometry will not align to the 16px pixel grid the way ADS's hand-hinted glyphs do, so crispness at 16px will still differ on low-DPI displays.

Also note: @atlaskit/icon is Apache-2.0 licensed, so using the real ADS icons is itself a permissive option worth considering over substitution.

## 9. Refuted claim

> Jira board column/card geometry (column ~270–280px wide, 8–12px gap, sunken column background, white raised card, ~8–12px card padding) is PRODUCT-level styling in Jira and is NOT expressed in any ADS package; unverifiable from source, so the CSS derives it from ADS primitives (surface.sunken columns, surface.raised + shadow.raised cards) rather than measuring real Jira. Cited evidence: no board/card dimensions in @atlaskit/tokens, page-layout, or navigation-system; not published on atlassian.design.

**Correction:**

The "no ADS component/token for board dimensions" half is true, but the claim's operative conclusion — that the geometry is unverifiable and not published on atlassian.design, so it must be derived — is FALSE, and the token names cited do not exist.

1) VERIFIED TRUE (absence in the three named packages): @atlaskit/tokens@15.8.0 (tarball inspected) has only a spacing scale (space.0…space.1000) plus color/elevation tokens — no board/column/card dimension tokens. @atlaskit/page-layout ships only DEFAULT_LEFT_SIDEBAR_WIDTH=240, DEFAULT_RIGHT_SIDEBAR_WIDTH=280, DEFAULT_*_PANEL_WIDTH=368, DEFAULT_TOP_NAVIGATION_HEIGHT=56 — every "board" grep hit was the substring inside "keyboard". @atlaskit/navigation-system has no px constants or kanban refs. @atlaskit/board, @atlaskit/kanban, and @atlaskit/card all 404 on npm — no board component exists.

2) REFUTED (the geometry IS published by Atlassian): the pragmatic-drag-and-drop documentation ships a canonical Board example, rendered on atlassian.design/components/pragmatic-drag-and-drop/examples and source-available in atlassian/pragmatic-drag-and-drop (packages/documentation/examples/pieces/). Actual values:
   - Column: width: '250px', backgroundColor: 'elevation.surface.sunken', borderRadius: 'radius.xlarge'; inner card list gap: 'space.100', padding: 'space.100'.
   - Card: padding: 'space.100', backgroundColor: 'elevation.surface' (hover 'elevation.surface.hovered'), boxShadow: 'elevation.shadow.raised', borderRadius: 'radius.large'.
   So column width is 250px, not ~270–280px. Gap/padding = space.100 = 0.5rem = 8px (verified from token-default-values; space.150 = 0.75rem = 12px) — the 8px end of the claimed 8–12px range is right, but it should be emitted as var(--ds-space-100), not a hardcoded px.

3) TOKEN NAMES ARE WRONG: there is no `surface.*` namespace in @atlaskit/tokens. The real names (grepped from the package) are elevation.surface.sunken, elevation.surface.raised, elevation.shadow.raised, elevation.surface, elevation.surface.hovered. CSS custom properties: --ds-surface-sunken, --ds-surface-raised, --ds-shadow-raised.

4) The elevation mapping is documented guidance, not a derivation: atlassian.design/foundations/elevation states "Columns on a Kanban board are a good example of the sunken elevation" and that raised is "reserved for cards that can be moved, such as Jira and Trello cards."

5) CAVEAT for implementation: the elevation docs say to always pair matching surface and shadow tokens, yet Atlassian's own board card example pairs elevation.surface with elevation.shadow.raised (not elevation.surface.raised). Also do not use a literal white card — use the elevation.surface token so dark mode works.

## 10. Refuted claim

> command-group 5.0.1 (2023-11-18) wraps process-group spawn/kill, but is stale; tokio's built-in `process_group()` + a killpg via nix 0.31 is the leaner path.

**Correction:**

The version facts are right but the conclusion is wrong. command-group 5.0.1 (2023-11-18) is indeed the newest release, but it is not merely "stale" — it is formally DEPRECATED with a named successor. Its crates.io description reads "Deprecated: use process-wrap" and its README states "The successor of command-group is process-wrap. No further work will be done on command-group."

The correct recommendation is process-wrap 9.1.0 (published 2026-03-08; repo watchexec/process-wrap last pushed 2026-04-18; 9.0M downloads vs command-group's 6.6M lifetime; MSRV 1.87.0), same author, versioning continued from command-group at 6.0.0.

Critically, process-wrap 9.1.0's own dependencies are `nix ^0.31.1` (optional) + `tokio ^1.38.2` (optional) + `windows ^0.62.2` (optional) — i.e. process-wrap IS the "tokio process_group + nix 0.31 killpg" approach, already packaged and tested. Hand-rolling it does not avoid the nix 0.31 dependency; it just reimplements process-wrap's Unix path without its test suite (retained in full from command-group).

Usage: `process-wrap = { version = "9.1.0", features = ["tokio1"] }`, then `CommandWrap::with_new("cmd", |c| { c.arg("x"); }).wrap(ProcessGroup::leader()).spawn()?`. Unlike command-group, it exposes composable per-concern wrappers (ProcessGroup, ProcessSession, JobObject, KillOnDrop) rather than one cross-platform API.

Supporting primitives verified as real: `tokio::process::Command::process_group(&mut self, pgroup: i32) -> &mut Command` exists, Unix-only, `process_group(0)` sets PGID to the child PID. `nix::sys::signal::killpg<T: Into<Option<Signal>>>(pgrp: Pid, signal: T) -> nix::Result<()>` exists in nix 0.31.3 (latest, 2026-05-11) and is gated behind the `signal` crate feature. Two caveats the claim omits: (1) tokio's process_group is Unix-only, so the hand-rolled path has no Windows story (process-wrap's JobObject wrapper covers it); (2) killpg signals the whole group but tokio's Child::wait only reaps the direct child, so grandchildren can be left unreaped — an edge case process-wrap handles.

Note also: the cited evidence (crates.io /api/v1/crates/command-group) returns an API data-access-policy error when fetched without a User-Agent header identifying the caller; a UA is required to reproduce it.

## 11. Refuted claim

> In WAL mode SQLite permits one writer concurrent with N readers; readers never block the writer and vice versa. This is what motivates a 1-connection writer pool + N-connection read_only pool.

**Correction:**

The WAL facts are approximately right but overstated, the causal claim is backwards, and the corroborating evidence is factually false.

1. CORROBORATION IS FALSE. sqlx *does* have a way to issue `BEGIN IMMEDIATE`. `Pool::begin_with` / `Pool::try_begin_with` / `Connection::begin_with` take an arbitrary begin statement; per the sqlx CHANGELOG they landed in 0.8.4 (2025-04-13, PR #3765, "Add `begin_with` methods to support database-specific transaction options"). Latest sqlx is 0.9.0 (2026-05-06). So `pool.begin_with("BEGIN IMMEDIATE")` is available today and the claim's supporting argument evaporates.

2. "NEVER" OVERSTATES SQLITE'S OWN WORDING. sqlite.org/wal.html says readers-don't-block-writers-and-vice-versa is "mostly true" with obscure exceptions, and explicitly lists ways SQLITE_BUSY still occurs in WAL mode (exclusive locking mode on another connection, connection cleanup, recovery after a crash). It also documents checkpoint starvation: a long-running or continuously-overlapping reader stops the checkpointer at the reader's end mark, so "no checkpoints will be able to complete and hence the WAL file will grow without bound." Readers therefore do impede the writer's storage path indirectly.

3. THE MOTIVATION IS INVERTED. The 1-writer + N-reader pool split is motivated by the *single-writer restriction* ("there can only be one writer at a time"), not by readers-not-blocking-writers. The specific hazard: sqlx's default `begin()` emits a plain DEFERRED `BEGIN`. Per sqlite.org/lang_transaction.html, a deferred txn starts as a reader and only tries to upgrade on first write; if another connection wrote in between, the upgrade fails with SQLITE_BUSY / SQLITE_BUSY_SNAPSHOT (rescode.html: "Process A starts a read transaction... Process B updates the database... Process A now tries to write"). This failure is *not* cured by `busy_timeout` — the snapshot is already stale, so retrying in place can never succeed; the transaction must be rolled back and replayed. Funnelling all writes through a single connection serializes writers so that upgrade race cannot happen. `BEGIN IMMEDIATE` is the alternative/complementary fix (it takes the write lock up front, where busy_timeout *does* apply). For a Jira-clone on Axum+SQLx+SQLite, do both: 1-connection write pool AND `begin_with("BEGIN IMMEDIATE")` — the pool split alone does not protect against a second process (backup tool, sqlite3 CLI, a supervised subprocess) touching the same file.

## 12. Refuted claim

> Tiptap licensing: core editor + extensions are MIT and free forever; Atlassian-style Mention/CodeBlock/etc. are MIT. In June 2025 Tiptap open-sourced 10 formerly-Pro extensions under MIT. Only Comments/Snapshots/AI remain Pro, and those require the paid Cloud Platform ($49/mo Start, $149 Team, $999 Business; free plan removed June 2025). For Atlas (self-hosted), the MIT surface covers mentions, code blocks, checklists (TaskList), images, and markdown.

**Correction:**

The MIT/licensing core of the claim is correct, but the pricing is stale (quotes June 2025 blog numbers as current) and the Pro feature list is incomplete.

CORRECT AS OF 2026-07-16:

1. Pricing (live tiptap.dev/pricing) — the claim's numbers are ANNUAL-billing rates presented as monthly, and the third tier was renamed:
   - Start: $59/mo monthly, $49/mo billed annually (claim said $49/mo)
   - Team: $179/mo monthly, $149/mo billed annually (claim said $149/mo)
   - Business: $1,199/mo monthly, $999/mo billed annually (claim said "$999 Business" — the June 2025 blog called this tier "Growth"; it has since been renamed to Business AND repriced)
   - Enterprise: custom
2. Free plan removal is CONFIRMED — release notes state "The free plan is gone. We now offer a time-limited trial for new users." Start/Team include a 30-day free trial, no credit card.
3. Paid surface is BROADER than "Comments/Snapshots/AI". Per tiptap.dev/pricing and docs/guides/pro-extensions, the Start tier and above also gates: real-time collaboration (Cloud), document history/version comparison, and import/export conversion (DOCX, PDF, Markdown). Tracked Changes and the AI Toolkit are additional custom-priced add-ons. Docs confirm "Snapshots, Comments, and some features of AI Toolkit" require active subscriptions.
4. Self-hosting Pro features is ENTERPRISE-TIER ONLY (on-premises deployment is an Enterprise-exclusive), not merely "the paid Cloud Platform." Relevant for Atlas as a self-hosted app.

VERIFIED CORRECT (npm registry, 2026-07-16, all at v3.28.0):
- @tiptap/core, @tiptap/react, @tiptap/starter-kit, @tiptap/pm, @tiptap/static-renderer, @tiptap/extension-collaboration — MIT
- Atlas's needed surface is all MIT: extension-mention, extension-code-block, extension-code-block-lowlight, extension-task-list, extension-task-item, extension-image, and @tiptap/markdown (MIT, published 2026-07-15)
- The 10 formerly-Pro extensions are genuinely MIT: drag-handle, details, emoji, mathematics, invisible-characters, file-handler, node-range, unique-id, table-of-contents (+ others). Caveat: @tiptap/extension-table-of-contents declares license "SEE LICENSE IN LICENSE.md" in package.json rather than an SPDX id, but its LICENSE.md is verbatim MIT (Copyright (c) 2025, Tiptap GmbH) — a metadata quirk, not a restriction, though it may trip automated license scanners.
- No auth token / private registry needed for any of the above.

TRAP TO AVOID: @tiptap/markdown (MIT, fine for Atlas) is distinct from Tiptap's PAID Markdown import/export conversion service. "Markdown is MIT" is true of the extension and false of the conversion API.

Net for Atlas: the practical conclusion holds — mentions, code blocks, TaskList checklists, images, and markdown are all MIT with no license obligation and no paid plan. Do not copy the $49/$149/$999 monthly figures into a plan.

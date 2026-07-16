# GitHub REST API surface for Atlas (Rust/Axum backend, PAT auth, card↔repo linking, branch/PR tracking, webhook-driven card automation)

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

All 11 areas verified against docs.github.com and docs.rs. Headline findings that should shape the design: (1) Token introspection is asymmetric — classic PATs expose `x-oauth-scopes` on every response, but fine-grained PATs have NO scope introspection mechanism at all (confirmed, no official endpoint, no GitHub staff answer), so Atlas cannot pre-validate a fine-grained PAT's permissions and must probe or fail-at-use. (2) The git-refs API has a genuine footgun: GET is `/git/ref/{ref}` (singular, `heads/main`) while POST is `/git/refs` (plural, body needs full `refs/heads/main`) — and there is no DELETE-branch endpoint; branch deletion goes through `DELETE /git/refs/heads/{branch}`. (3) Commit Statuses and Check Runs are two separate systems; showing "CI status" on a card requires querying both. (4) `pull_request.mergeable` is computed asynchronously and is `null` on first read, and only appears on the single-PR GET, never on list. (5) Repo webhooks only receive `check_suite.completed`, not `requested`/`rerequested`. (6) octocrab 0.54.0 (released 2026-07-07, actively maintained) covers every endpoint Atlas needs first-class — including refs, hooks, statuses, check runs — and its `app()`/`installation()` API makes the future GitHub App migration a client-construction swap rather than a rewrite. Recommend octocrab over hand-rolled reqwest.

## Implementation notes

## Recommendation on crate choice (item 10): use octocrab 0.54, do not hand-roll

Coverage is not the differentiator I expected it to be — I verified octocrab covers *every* endpoint Atlas needs first-class (`create_ref`/`delete_ref`/`get_ref`, `list_branches`, `list_commits`, `create_hook`, `list_statuses`/`combined_status_for_ref`, `list_check_runs_for_git_ref`, full `pulls`). The decisive argument is item 11: octocrab's `app()` + `installation()` + `installation_token()` already implement the entire GitHub App JWT/installation-token lifecycle *including caching and refresh*. Hand-rolling reqwest means writing that yourself later, exactly when the migration is riskiest.

The real cost of octocrab is that it is 0.x with a fast breaking cadence (0.51→0.54 in under two months). Mitigate by confining it behind Atlas's own trait — which you want anyway for testing:

```rust
#[async_trait]
pub trait GitHostClient: Send + Sync {
    async fn create_branch(&self, repo: &RepoRef, name: &str, from_sha: &str) -> Result<()>;
    async fn open_pr(&self, repo: &RepoRef, head: &str, base: &str, title: &str) -> Result<PrSummary>;
    async fn pr_status(&self, repo: &RepoRef, number: u64) -> Result<PrStatus>;
    async fn ci_status(&self, repo: &RepoRef, sha: &str) -> Result<CiRollup>;
}
```
Then `octocrab` appears in exactly one module. An octocrab major bump becomes a contained edit, and the escape hatches (`_get`/`get_with_headers`) cover anything the typed API lacks — notably response headers, which the typed methods discard.

## 1. Token validation

`GET /user` via `_get()` (not the typed `users()` call) so you can read headers:
```rust
let resp = octo._get("/user").await?;          // http::Response
let scopes = resp.headers().get("x-oauth-scopes")
    .and_then(|v| v.to_str().ok())
    .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty())
              .map(String::from).collect::<Vec<_>>());
let expiry = resp.headers().get("github-authentication-token-expiration");
```
Classify the token from what you get back, and store the classification:
- `x-oauth-scopes` **present and non-empty** → classic PAT; you can assert `repo` is granted and fail fast with a good error.
- `x-oauth-scopes` **absent/empty** → assume fine-grained. There is *no* introspection path. Do **not** block the user; record `Scopes::Unknown` and surface permission failures at point-of-use (403 → "this token lacks write access to owner/repo"). Optionally probe once at link time with a cheap authorized read (e.g. `GET /repos/{o}/{r}`) and check `permissions.push` from the repo payload — that is a real capability signal and works for both token types.

Parse expiry defensively — it is **not** RFC3339 and it is not in the REST docs:
```rust
// "2026-01-30 17:29:33 UTC" or "2026-01-30 17:29:33 +0500"
fn parse_expiry(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S %z").map(|d| d.with_timezone(&Utc)).ok()
        .or_else(|| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S UTC").ok()
            .map(|d| d.and_utc()))
}
```
Treat the result as **advisory only**: absent for non-expiring tokens, and reportedly wrong (returns current time) for fine-grained PATs. Never gate functionality on it. A sane use is a soft banner ("token expires in 6 days"), suppressed if the parsed value is implausible (e.g. within seconds of `now`, which is exactly the reported bug's signature).

## 2. Pagination

`per_page=100` and follow the `Link` header's `rel="next"` rather than incrementing `page` (avoids the drift bug when repos are created mid-walk). octocrab: `octo.all_pages(first_page).await?`. Prefer `sort=updated` for a repo picker so the useful entries land on page 1 and you can lazily paginate the UI.

## 3. Create a branch (the footgun)

Two calls, and the ref format differs between them — this is the single easiest thing to get wrong:
```
GET  /repos/{o}/{r}/git/ref/heads/{base}     → object.sha     # SINGULAR "ref", no "refs/" prefix
POST /repos/{o}/{r}/git/refs                                  # PLURAL "refs"
     {"ref": "refs/heads/atlas-123-slug", "sha": "<object.sha>"}   # FULLY qualified
```
Get the base branch name from the repo payload's `default_branch` — don't hardcode `main`. Handle `422` as "branch already exists" (idempotent success for Atlas: adopt the existing branch) rather than a hard error. octocrab's `Reference::Branch(name)` type handles the prefixing for you, which is a good reason to use `create_ref`/`get_ref` over raw paths.

Branch naming from a card: `atlas-{card_number}-{slug}`. Sanitize hard — git refs forbid `..`, `~`, `^`, `:`, `?`, `*`, `[`, `\`, spaces, leading/trailing `/`, trailing `.lock`. Lowercase, collapse non-alphanumerics to `-`, truncate the slug (~50 chars) but never truncate the card-number prefix, since that prefix is what webhook handlers match on.

## 4. Delete a branch

There is no branches DELETE. Use `DELETE /repos/{o}/{r}/git/refs/heads/{branch}` → 204. Expect 422 when someone targets the default branch; refuse that in Atlas before the call rather than surfacing a raw API error.

## 5. Commits + CI

Commits on a card branch: `GET /repos/{o}/{r}/commits?sha={branch}&per_page=100`. Note this returns branch *history*, not "commits unique to the branch" — it includes everything reachable from the branch tip, so a fresh branch shows the whole trunk history. For "what did this card actually add", either compare against the merge base (`GET /repos/{o}/{r}/compare/{base}...{head}` → `commits`) or rely on accumulating `push` webhook payloads. The compare endpoint is the better primitive for card UI.

CI rollup **must merge both systems** — this is the part most integrations get wrong:
```rust
let combined = octo.repos(o, r).combined_status_for_ref(&Reference::Commit(sha)).await?; // legacy CI
let runs = octo.checks(o, r).list_check_runs_for_git_ref(sha.into()).send().await?;      // Actions/Apps
```
Fold to one card badge: any `failure`/`error` state or any conclusion in {failure, timed_out, action_required} → failed; else any `pending`/non-`completed` status → running; else if anything succeeded → passed; else neutral. Treat conclusions `skipped`/`neutral`/`cancelled` as non-blocking. Use `filter=latest` (the default) so re-runs don't double-count.

## 6. PRs

Find a card's PR without storing the number: `GET /repos/{o}/{r}/pulls?head={owner}:{branch}&state=all`. Remember `head` needs the `owner:` prefix.

`mergeable` requires the single-PR GET *and* a retry, since it is null while GitHub computes it in the background:
```rust
async fn mergeable(octo: &Octocrab, o: &str, r: &str, n: u64) -> Result<Option<bool>> {
    for backoff in [300u64, 700, 1500] {                 // ms; bounded, then give up
        let pr = octo.pulls(o, r).get(n).await?;
        if let Some(m) = pr.mergeable { return Ok(Some(m)); }
        tokio::time::sleep(Duration::from_millis(backoff)).await;
    }
    Ok(None)  // render "checking…", never "conflicted"
}
```
Merge state: `merged == true` (or `merged_at.is_some()`) is the only truth. `state == "closed"` alone means *closed or merged* — conflating them will move cards to Done on abandoned PRs.

Review rollup: `GET .../reviews` is chronological and includes superseded reviews. Group by `user.id`, take the **last** review per user whose state is APPROVED or CHANGES_REQUESTED (ignore COMMENTED/PENDING for the rollup, and drop DISMISSED). Any CHANGES_REQUESTED outstanding → "changes requested"; else ≥1 APPROVED → "approved".

## 7. Webhooks

Create with `repo` (or `admin:repo_hook`) scope:
```json
POST /repos/{owner}/{repo}/hooks
{"name":"web","active":true,
 "events":["push","pull_request","check_suite","status","pull_request_review"],
 "config":{"url":"https://atlas.example/webhooks/github","content_type":"json",
           "secret":"<32+ bytes random, per-repo, encrypted at rest>","insecure_ssl":"0"}}
```
Subscribe to `status` and `pull_request_review` alongside the three you named — otherwise legacy-CI results and review approvals only ever arrive via polling. Note `check_suite` on a repo webhook delivers **only** `completed`, so there's no need to handle `requested`/`rerequested`.

**Signature verification (exact algorithm).** The HMAC is over the *raw body*, so it must run before any JSON deserialization — in Axum, extract `axum::body::Bytes`, verify, *then* `serde_json::from_slice`. A `Json<T>` extractor in the handler signature makes correct verification impossible.
```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn verify(secret: &[u8], raw_body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else { return false };
    let Ok(expected) = hex::decode(hex_sig) else { return false };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(raw_body);
    mac.verify_slice(&expected).is_ok()   // constant-time; satisfies GitHub's requirement
}
```
`Mac::verify_slice` is constant-time internally, so you do not need `subtle` and must not compare hex strings with `==`. Reject on missing header. Ignore `X-Hub-Signature` (SHA-1, legacy) entirely.

Then: dedupe on `X-GitHub-Delivery` (GUID) with a unique index — GitHub retries deliveries, and without this a redelivered `pull_request.closed` re-runs the card transition. Return 2xx immediately and process async; GitHub times out slow endpoints.

Routing:
- `push` → `ref` is fully qualified (`refs/heads/x`), strip `refs/heads/` before matching a card branch. `commits` caps at **2048**; if `commits.len() == 2048` treat it as truncated and backfill via the compare/list-commits API.
- `pull_request` + `action == "closed"` + `pull_request.merged == true` → **auto-move card to Done**. `merged == false` → moved to a "Closed" state, not Done.
- `pull_request` + `synchronize` → new commits pushed to the PR; re-fetch CI.
- `check_suite` + `completed` → refresh the card's CI badge from `check_suite.conclusion` / `head_sha`.

## 8. Rate limits

At 5,000/hr a single-user self-hosted Atlas will never hit the *primary* limit — but the budget is shared with the user's other tooling, so still read `x-ratelimit-remaining` off every response and expose it. The realistic hazard is the **secondary** limits, and specifically the 5-points-per-mutation rule plus the "wait at least one second between mutations" guidance: a card action that creates a branch and opens a PR back-to-back is already 10 points and should be serialized.

Middleware shape: on 403/429, honour `retry-after` first; else if `x-ratelimit-remaining == 0` sleep until `x-ratelimit-reset` (epoch **seconds**); else sleep ≥60s. Exponential backoff with a retry cap. Distinguish a secondary-limit 403 from a permissions 403 by body text/presence of `retry-after` — retrying a permissions error forever is a real failure mode. Serialize all mutating calls through a single queue (docs explicitly recommend serial-not-concurrent), and use `GET /rate_limit` for dashboards since it's free.

## 9. Smart commits

Adopt Jira's grammar, since it's what users already have muscle memory for, but **do not** adopt its regex. `[A-Z]{2,}-\d+` matches `UTF-8`, `SHA-256`, `RFC-7231`, `COVID-19` — a naive implementation will attach commits to nonexistent cards. Because Atlas is single-project-per-board rather than multi-project like Jira, you can afford to be strict: match `\bATLAS-(\d+)\b` (or `\b{PROJECT_KEY}-(\d+)\b` against keys that actually exist in the DB), then validate the card number resolves to a real card in the repo's linked project before acting.

```
ATLAS-123 #done                       → move card to Done
ATLAS-123 #comment fixed the leak     → append comment to card
ATLAS-123 #time 2h 30m                → log time (if Atlas tracks it)
ATLAS-123 ATLAS-124 #done             → multiple cards, one command
ATLAS-123 #time 1h #comment wip #done → multiple commands, one card
```
Parse per Jira's rule that a smart-commit directive must not span more than one line: scan each line, take issue keys leading the line, then split on `#`. Everything after a command up to the next `#` is that command's argument. Keys with no `#command` still **link** the commit to the card — that's the common case and should work without ceremony.

Sources of keys, in priority order: commit messages (via `push`), PR title, PR body, and branch name. Branch name is the highest-signal source since Atlas *creates* the branch with the card number in it — `atlas-123-slug` — so commit-message parsing is a convenience layer on top of a link you already have, not the primary mechanism. That matters: it means a user who never learns the syntax still gets tracking.

Guardrails: require a `#command` (not a bare key) to *transition* a card; a bare key only links. Ignore directives in merge commits' aggregated bodies to avoid re-firing on merge. Make commands case-insensitive (`#DONE`), and map `#close`/`#done`/`#resolve` to the same transition.

## 11. Keeping the PAT design App-migration-safe

CLAUDE.md already says "webhook receiver built now so a GitHub App can be added later" — that's the right call, and webhook signature verification is **byte-identical** for Apps, so that surface needs zero change. The remaining corners to avoid:

1. **Don't model GitHub identity as "the PAT's user."** A GitHub App has no `/user` — `GET /user` returns 401/403 for a JWT or installation token. Any code path that assumes "the authenticated user's login" is a migration blocker. Key everything on `repo.id` (immutable across renames) + `full_name`, never on the token owner.
2. **Store the credential polymorphically now.** One column of encrypted bytes plus a `credential_kind` discriminant (`ClassicPat | FineGrainedPat | AppInstallation`), and a `github_installation_id` column that's simply NULL today. Adding a variant later is then a migration, not a schema redesign.
3. **Construct the client behind a factory,** so the swap is one function:
```rust
match cred {
    Credential::Pat(tok)  => Octocrab::builder().personal_token(tok.expose()).build()?,
    Credential::App { app_id, key, installation_id } =>
        Octocrab::builder().app(app_id, EncodingKey::from_rsa_pem(key)?)
            .build()?.installation(installation_id.into())?,   // octocrab caches+refreshes the 1h token
}
```
Both arms yield `Octocrab`, so every handler above is unchanged. `installation_token()` auto-refreshes with a ≥30s margin, so Atlas never implements JWT minting or the 1-hour expiry dance itself — but if you ever do, the constraints are: RS256, `iat` 60s in the past, `exp` ≤10 minutes out, `iss` = client ID.
4. **Repo discovery differs** and needs a trait method, not an inlined endpoint: PAT uses `GET /user/repos`; App uses `GET /installation/repositories`. Map repo→installation later via `GET /repos/{o}/{r}/installation`.
5. **Attribution changes.** App-created branches/PRs are authored by `atlas[bot]`, not the user. If Atlas ever surfaces "who opened this PR", don't assume it's the linking user.
6. **Rate limits become per-installation**, not per-user — the budget stops being shared with the user's other tools, so any limit accounting keyed to "the user" needs to be keyed to the credential instead.
7. **Webhook creation flips from API to install-time.** Apps receive events via the App's own configured webhook, not per-repo hooks — so `create_hook` becomes dead code for the App path. Keep hook creation behind the trait too, with an App impl that no-ops.

## Facts

- **[verified]** Validate a PAT with GET /user. Required headers: Authorization: Bearer <TOKEN>, Accept: application/vnd.github+json, X-GitHub-Api-Version: 2022-11-28. Returns login, id, node_id, name, email, plan. 401 = invalid token.
  - Evidence: https://docs.github.com/en/rest/users/users?apiVersion=2022-11-28
- **[verified]** X-OAuth-Scopes lists the scopes the token has authorized; X-Accepted-OAuth-Scopes lists the scopes the endpoint checks for. Documented example: `curl -H "Authorization: Bearer OAUTH-TOKEN" https://api.github.com/users/codertocat -I` returns `X-OAuth-Scopes: repo, user` and `X-Accepted-OAuth-Scopes: user`. Value is a comma-space separated list. These headers are returned on ANY endpoint, not just /user.
  - Evidence: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps
- **[verified]** X-OAuth-Scopes applies to OAuth apps and classic PATs ONLY. For fine-grained PATs, GitHub provides NO way to query scopes or repository permissions — there is no official introspection endpoint. Community workarounds are permission-probing via test API calls or user self-declaration. The community discussion has no GitHub staff response.
  - Evidence: https://github.com/orgs/community/discussions/156115
- **[verified]** For fine-grained PATs, GitHub returns X-Accepted-GitHub-Permissions on REST responses, indicating which permissions the endpoint requires. This is the fine-grained analogue of X-Accepted-OAuth-Scopes and is the only permission-discovery signal available for FGPATs.
  - Evidence: https://docs.github.com/en/rest/overview/permissions-required-for-github-apps
- *[likely]* The `GitHub-Authentication-Token-Expiration` response header indicates a PAT's expiration date. Announced 2021-07-26: 'When using a personal access token with the GitHub API, you'll see a new response header, GitHub-Authentication-Token-Expiration, indicating the token's expiration date.' Value format is NOT RFC3339 — it is Go layout `2006-01-02 15:04:05 -0700`, e.g. `2025-09-05 17:55:53 +0500` or `... UTC`.
  - Evidence: https://github.blog/changelog/2021-07-26-expiration-options-for-personal-access-tokens/ ; format observed in google/go-github#3708
- **[verified]** The expiration header is NOT documented on the REST authentication docs page nor the token-expiration-and-revocation docs page — both were fetched and neither mentions it. It is only described in a 2021 changelog post. Treat it as advisory/undocumented surface, not a contract.
  - Evidence: Fetched https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api and https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/token-expiration-and-revocation — neither mentions the header
- *[uncertain]* Reported bug (google/go-github#3708, filed 2025-09-05): for fine-grained PATs the github-authentication-token-expiration header returns the CURRENT SERVER TIME instead of the real expiry. Single reporter, no GitHub confirmation, no resolution documented. Classic PATs reportedly return the correct value.
  - Evidence: https://github.com/google/go-github/issues/3708
- *[likely]* The expiration header is absent entirely for tokens configured with no expiration. Atlas must treat 'header missing' as 'no known expiry', not as an error.
  - Evidence: Inferred from changelog semantics (header indicates the token's expiration date); not explicitly documented
- **[verified]** GET /rate_limit does NOT count against the REST API rate limit ('Accessing this endpoint does not count against your REST API rate limit'). Response: resources.{core,search,code_search,graphql,integration_manifest,actions_runner_registration,dependency_snapshots,dependency_sbom}, each with limit/remaining/reset/used.
  - Evidence: https://docs.github.com/en/rest/rate-limit/rate-limit?apiVersion=2022-11-28
- **[verified]** List repos: GET /user/repos. Query params: visibility (all|public|private), affiliation (owner,collaborator,organization_member), type, sort (created|updated|pushed|full_name), direction, per_page (default 30, max 100), page, since, before. Response fields: id, name, full_name, private, owner.login, default_branch, permissions{admin,maintain,push,triage,pull}, clone_url, html_url, visibility, archived, pushed_at.
  - Evidence: https://docs.github.com/en/rest/repos/repos?apiVersion=2022-11-28#list-repositories-for-the-authenticated-user
- **[verified]** repo.permissions.push is the field Atlas must check before offering branch creation on a linked repo — a token can list a repo it cannot write to.
  - Evidence: Response schema of GET /user/repos includes permissions object with push boolean
- **[verified]** Get a ref: GET /repos/{owner}/{repo}/git/ref/{ref} — note SINGULAR 'ref' in path, and {ref} omits the 'refs/' prefix (e.g. 'heads/main'). Returns {ref, node_id, url, object:{type, sha, url}}. object.sha is the commit SHA to branch from. 404 if absent, 409 conflict (empty repo).
  - Evidence: https://docs.github.com/en/rest/git/refs?apiVersion=2022-11-28
- **[verified]** Create a ref: POST /repos/{owner}/{repo}/git/refs — PLURAL. Body: {"ref": "refs/heads/<name>", "sha": "<commit sha>"}. ref MUST be fully qualified: 'If it doesn't start with refs and have at least two slashes, it will be rejected.' Returns 201 with the reference object; 422 on validation failure (including branch already exists).
  - Evidence: https://docs.github.com/en/rest/git/refs?apiVersion=2022-11-28
- **[verified]** Delete a ref: DELETE /repos/{owner}/{repo}/git/refs/{ref} → 204 No Content. 422 if you attempt to delete the default branch. This is the ONLY way to delete a branch — the Branches API has NO delete endpoint (verified by fetching the branches docs page in full).
  - Evidence: https://docs.github.com/en/rest/git/refs?apiVersion=2022-11-28 and https://docs.github.com/en/rest/branches/branches?apiVersion=2022-11-28
- **[verified]** Update a ref: PATCH /repos/{owner}/{repo}/git/refs/{ref}, body {sha, force}. force=false (default) enforces fast-forward-only.
  - Evidence: https://docs.github.com/en/rest/git/refs?apiVersion=2022-11-28
- **[verified]** List branches: GET /repos/{owner}/{repo}/branches, params protected/per_page/page. Get a branch: GET /repos/{owner}/{repo}/branches/{branch}. Response: name, commit.sha, protected. Rename: POST /repos/{owner}/{repo}/branches/{branch}/rename {new_name}. Merge: POST /repos/{owner}/{repo}/merges {base, head, commit_message}.
  - Evidence: https://docs.github.com/en/rest/branches/branches?apiVersion=2022-11-28
- **[verified]** List commits: GET /repos/{owner}/{repo}/commits. The `sha` param takes a branch name, tag, or SHA and lists commits from that starting point (defaults to the repo default branch) — this is how Atlas lists commits on a card's branch. Other params: path, author, committer, since, until (ISO 8601), per_page, page. Response: sha, commit.message, commit.author{name,email,date}, html_url, author (GitHub user or null).
  - Evidence: https://docs.github.com/en/rest/commits/commits?apiVersion=2022-11-28#list-commits
- **[verified]** Combined status: GET /repos/{owner}/{repo}/commits/{ref}/status → {state, sha, total_count, statuses[]}. Combined state logic, quoted: 'failure if any of the contexts report as error or failure; pending if there are no statuses or a context is pending; success if the latest status for all contexts is success.' Individual: GET /repos/{owner}/{repo}/commits/{ref}/statuses (reverse chronological, latest first). Individual state enum: error, failure, pending, success.
  - Evidence: https://docs.github.com/en/rest/commits/statuses?apiVersion=2022-11-28
- **[verified]** Check runs for a ref: GET /repos/{owner}/{repo}/commits/{ref}/check-runs. Params: check_name, status (queued|in_progress|completed), filter (latest default | all), app_id, per_page, page. Response check run: status enum {queued, in_progress, completed, waiting, requested, pending}; conclusion enum {success, failure, neutral, cancelled, skipped, timed_out, action_required, null}. Classic PAT needs `repo` scope for private repos.
  - Evidence: https://docs.github.com/en/rest/checks/runs?apiVersion=2022-11-28#list-check-runs-for-a-git-reference
- *[likely]* Statuses API and Checks API are DISJOINT systems. GitHub Actions and GitHub Apps report via check runs; legacy/external CI (and many third-party services) report via commit statuses. Neither endpoint surfaces the other's results. A complete card CI view requires calling BOTH /status and /check-runs and merging.
  - Evidence: Separate endpoint families with separate schemas and separate docs sections (rest/commits/statuses vs rest/checks/runs); conclusion enums do not overlap with status state enums
- **[verified]** Create PR: POST /repos/{owner}/{repo}/pulls. Body: title (required unless using `issue`), head (required; 'username:branch' for cross-repo), base (required), body, draft (bool), maintainer_can_modify (bool). An existing issue can be converted by passing `issue` instead of `title`.
  - Evidence: https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28
- **[verified]** List PRs: GET /repos/{owner}/{repo}/pulls. Params: state (open|closed|all, default open), head (format 'user:ref-name'), base, sort (created|updated|popularity|long-running), direction. The `head` param is how Atlas finds the PR for a card's branch without storing the PR number.
  - Evidence: https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28
- **[verified]** Get PR: GET /repos/{owner}/{repo}/pulls/{pull_number}. Fields present ONLY on this single-PR GET and NOT on the list response: merged, mergeable, rebaseable, mergeable_state, merged_by, comments, review_comments, commits, additions, deletions, changed_files.
  - Evidence: https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28
- **[verified]** mergeable is computed asynchronously: null means GitHub 'has started a background job to compute the mergeability' and the client should retry after a brief delay. true = auto-mergeable, false = conflicts. Atlas must poll, not treat null as false.
  - Evidence: https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28
- **[verified]** PR merge detection: `state` is only open|closed and never 'merged'. A merged PR is state=closed AND merged=true (equivalently merged_at is non-null). A PR closed without merging is state=closed, merged=false, merged_at=null. GitHub's own deployment guide states: 'When a pull request is merged (its state is closed, and merged is true)'.
  - Evidence: https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28 and https://docs.github.com/en/rest/guides/delivering-deployments
- **[verified]** List PR reviews: GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews, returned in chronological order. Fields: id, state, body, user, submitted_at, commit_id. State values: APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED, PENDING (review actions are APPROVE, REQUEST_CHANGES, COMMENT).
  - Evidence: https://docs.github.com/en/rest/pulls/reviews?apiVersion=2022-11-28
- **[verified]** Create repo webhook: POST /repos/{owner}/{repo}/hooks. Body: {"name": "web", "config": {"url": <required>, "content_type": "json", "secret": "...", "insecure_ssl": "0"}, "events": [...], "active": true}. `name` only accepts the literal value "web". Default events is ["push"]. Response: id, active, events, config, created_at, ping_url, test_url, deliveries_url, last_response{code,status,message}.
  - Evidence: https://docs.github.com/en/rest/repos/webhooks?apiVersion=2022-11-28#create-a-repository-webhook
- **[verified]** Webhook signature header is `X-Hub-Signature-256` (HMAC-SHA256). The legacy `X-Hub-Signature` is HMAC-SHA1 and is 'only included for legacy purposes' — Atlas must ignore it. The value 'always starts with sha256=' followed by the hex digest. The HMAC is computed over 'the original request body' (raw bytes, pre-deserialization).
  - Evidence: https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
- **[verified]** GitHub explicitly mandates constant-time comparison: 'Never use a plain == operator. Instead consider using a method like secure_compare or crypto.timingSafeEqual, which performs a constant time string comparison to help mitigate certain timing attacks.'
  - Evidence: https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
- **[verified]** Webhook delivery headers: X-GitHub-Event ('the name of the event that triggered the delivery') and X-GitHub-Delivery ('a globally unique identifier (GUID) to identify the event'). X-GitHub-Delivery is the natural idempotency key for Atlas's webhook receiver.
  - Evidence: https://docs.github.com/en/webhooks/webhook-events-and-payloads
- **[verified]** pull_request event actions include: opened, closed, reopened, synchronize, ready_for_review, converted_to_draft, edited, assigned, labeled, review_requested, enqueued, dequeued, auto_merge_enabled/disabled, locked/unlocked, etc.
  - Evidence: https://docs.github.com/en/webhooks/webhook-events-and-payloads#pull_request
- **[verified]** check_suite has only three actions: completed ('All check runs in a check suite have completed, and a conclusion is available'), requested, rerequested. CRITICAL CONSTRAINT: 'Repository and organization webhooks only receive payloads for the completed action type.' Atlas (using repo webhooks + PAT) will therefore only ever see check_suite.completed.
  - Evidence: https://docs.github.com/en/webhooks/webhook-events-and-payloads
- **[verified]** push event payload: `ref` = 'The full git ref that was pushed. Example: refs/heads/main' (fully qualified — must strip 'refs/heads/' to match a card branch), `before`/`after` = SHA before/after push, `commits` = array of commit objects, MAXIMUM 2048 commits. For pushes over 2048 commits Atlas would miss commits and must backfill via the list-commits API.
  - Evidence: https://docs.github.com/en/webhooks/webhook-events-and-payloads
- **[verified]** Primary rate limits: 5,000 req/hr for authenticated PAT (budget SHARED across all PATs/OAuth/Apps acting on behalf of that user), 60 req/hr unauthenticated (by IP). GitHub App installations: 5,000/hr baseline, 15,000/hr for Enterprise Cloud orgs; scales +50/hr per repo beyond 20 and +50/hr per user beyond 20, capped at 12,500/hr.
  - Evidence: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api?apiVersion=2022-11-28
- **[verified]** Rate limit headers: x-ratelimit-limit (max/hr), x-ratelimit-remaining (remaining in window), x-ratelimit-used (used in window), x-ratelimit-reset (UTC epoch SECONDS — not millis), x-ratelimit-resource (which resource bucket the request counted against).
  - Evidence: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api?apiVersion=2022-11-28
- **[verified]** Secondary rate limits (exact figures): no more than 100 concurrent requests (shared across REST and GraphQL); no more than 900 points/minute for REST endpoints (GET/HEAD/OPTIONS = 1 point, POST/PATCH/PUT/DELETE = 5 points); no more than 90 seconds of CPU time per 60 seconds real time; no more than 80 content-generating requests/minute and 500/hour; no more than 2,000 OAuth token requests/hour. Docs warn: 'You may also encounter a secondary rate limit for undisclosed reasons' and limits may change without notice.
  - Evidence: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api?apiVersion=2022-11-28#about-secondary-rate-limits
- **[verified]** Secondary rate limit violations return 403 OR 429. Handling rules: if `retry-after` is present, 'you should not retry your request until after that many seconds has elapsed'; else if x-ratelimit-remaining is 0, wait until x-ratelimit-reset; otherwise wait at least 60 seconds. Then exponential backoff with a retry cap. Docs also require waiting 'at least one second between each' POST/PATCH/PUT/DELETE, and recommend serial (not concurrent) requests plus a request queue.
  - Evidence: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api and .../best-practices-for-using-the-rest-api
- **[verified]** Jira Smart Commits syntax: `<ignored text> <ISSUE_KEY> <ignored text> #<COMMAND> <optional COMMAND_ARGUMENTS>`. Issue key format is 'two or more uppercase letters, followed by a hyphen and the issue number' → regex [A-Z]{2,}-\d+. Commands: #comment <text>, #time <w/d/h/m> <comment>, and any workflow transition name (#close, #resolve, #done). A Smart Commit message must not span more than one line, though multiple commands may share a line. Multiple issue keys are supported: 'JRA-123 JRA-234 #resolve'.
  - Evidence: https://confluence.atlassian.com/fisheye/using-smart-commits-960155400.html
- **[verified]** octocrab is at 0.54.0, published 2026-07-07 (9 days before this research), ~1.79M recent downloads / 15.7M total, repo XAMPPRocky/octocrab. Release cadence: 0.51.0 (2026-05-17), 0.52.0 (2026-06-02), 0.53.0 (2026-06-03), 0.53.1 (2026-06-10), 0.54.0 (2026-07-07).
  - Evidence: crates.io API: https://crates.io/api/v1/crates/octocrab
- **[verified]** The only notable alternative crate, hubcaps, is abandoned: last release 0.6.2 on 2020-09-07, ~2.3k recent downloads. There is no maintained competitor to octocrab.
  - Evidence: crates.io API: https://crates.io/api/v1/crates/hubcaps
- **[verified]** octocrab RepoHandler covers every git operation Atlas needs first-class: get_ref(&Reference) -> Result<Ref>, create_ref(&Reference, sha) -> Result<Ref>, delete_ref(&Reference) -> Result<()>, list_branches() -> ListBranchesBuilder, list_commits() -> ListCommitsBuilder, create_hook(Hook) -> Result<Hook>, create_status(sha, StatusState), list_statuses(sha), combined_status_for_ref(&Reference) -> Result<CombinedStatus>.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/repos/struct.RepoHandler.html
- **[verified]** octocrab ChecksHandler provides list_check_runs_for_git_ref(git_ref: Commitish) -> ListCheckRunsForGitRefBuilder and list_check_suites_for_git_ref(git_ref: Commitish) -> ListCheckSuitesForGitRefBuilder.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/checks/struct.ChecksHandler.html
- **[verified]** octocrab handler surface on the Octocrab struct includes: repos, repos_by_id, pulls, issues, commits, checks, hooks, orgs, users, teams, actions, workflows, apps, search, events, gists, projects, graphql, ratelimit.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/struct.Octocrab.html
- **[verified]** octocrab escape hatches for uncovered endpoints and for reading response headers: _get(), _post(), _patch(), _put(), _delete() 'directly return the http::Response' with 'no additional pre or post processing', plus get_with_headers(). This is how Atlas reads x-oauth-scopes / github-authentication-token-expiration, which the typed methods discard.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/struct.Octocrab.html
- **[verified]** octocrab auth builders: OctocrabBuilder::personal_token(token), OctocrabBuilder::app(app_id, encoding_key) where key is an 'RSA private key in DER or PEM formats', user_access_token(token), basic_auth(), oauth(). All produce the same Octocrab client type.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/struct.OctocrabBuilder.html
- **[verified]** octocrab already implements the whole GitHub App token lifecycle: Octocrab::installation(id: InstallationId) -> Result<Octocrab> returns a client scoped to an installation; installation_and_token(id) -> Result<(Octocrab, SecretString)> also returns the raw token (usable for HTTPS git clone); installation_token() 'acquires a GitHub App installation access token that does not expire for at least 30 seconds. A cached token will be used if its expiration is far enough in the future. Otherwise, a new token will be acquired and cached'; installation_token_with_buffer(buffer: Duration) makes the refresh margin configurable. Token caching and refresh are handled by the crate.
  - Evidence: https://docs.rs/octocrab/0.54.0/octocrab/struct.Octocrab.html (source-level method signatures)
- **[verified]** GitHub App JWT requirements: algorithm MUST be RS256. Claims: iat ('set 60 seconds in the past' to protect against clock drift), exp ('no more than 10 minutes into the future'), iss = the App's client ID ('Use of the client ID is recommended'; the numeric App ID also works but is the legacy form).
  - Evidence: https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app
- **[verified]** Installation token exchange: POST /app/installations/{installation_id}/access_tokens, authenticated with the App JWT (not a PAT). Optional body: repositories (names), repository_ids, permissions (to down-scope). Response: {token, expires_at}. 'Installation tokens expire one hour from the time you create them.'
  - Evidence: https://docs.github.com/en/rest/apps/apps?apiVersion=2022-11-28
- **[verified]** Supporting App endpoints: GET /app/installations (list installations for the authenticated app, JWT auth, params per_page/page/since/outdated) and GET /repos/{owner}/{repo}/installation (get a repository installation, JWT auth) — the latter maps an Atlas-linked repo to its installation_id.
  - Evidence: https://docs.github.com/en/rest/apps/apps?apiVersion=2022-11-28
- **[verified]** Classic PAT scopes needed: `repo` grants 'full access to public and private repositories including read and write access to code, commit statuses, repository invitations, collaborators, deployment statuses, and repository webhooks' — so `repo` ALONE covers Atlas's branch/PR/status/webhook needs. `admin:repo_hook` ('read, write, ping, and delete access to repository hooks') is only needed if Atlas ships a hooks-only, non-repo-scoped token. `workflow` is only needed to add/update Actions workflow files.
  - Evidence: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps
- **[verified]** Relevant crate versions as of 2026-07-16: reqwest 0.13.4 (2026-05-25), jsonwebtoken 10.4.0 (2026-05-11), hmac 0.13.0 (2026-03-29), sha2 0.11.0 (2026-03-25), subtle 2.6.1 (2024-06-24). Note hmac 0.13 and sha2 0.11 are recent majors — much of the ecosystem is still on hmac 0.12 / sha2 0.10, so a version choice here risks duplicate transitive crypto crates.
  - Evidence: crates.io API queries for each crate
- **[verified]** Atlas is greenfield: the repo currently contains only CLAUDE.md, README.md, .gitignore, .editorconfig — no Cargo.toml exists yet. CLAUDE.md already commits to 'GitHub auth | PAT, encrypted at rest | Webhook receiver built now so a GitHub App can be added later', and mandates that secrets never appear in logs/Debug/API responses with redacted Debug impls.
  - Evidence: Read /home/alli/Projects/Atlas/CLAUDE.md and directory listing

## Risks

- Fine-grained PAT scope introspection is impossible — there is no official endpoint and no GitHub staff commitment to one. If Atlas's onboarding UX promises 'we'll check your token has the right permissions', that promise cannot be kept for FGPATs (an increasingly common token type). Design the link flow to fail gracefully at point-of-use with actionable 403 messages, and probe capability via repo.permissions.push rather than scopes.
- github-authentication-token-expiration is effectively undocumented surface: it appears only in a 2021 changelog, is absent from both the REST auth docs and the token-expiration docs, is missing entirely for non-expiring tokens, uses a non-RFC3339 format, and is reported (unconfirmed, single reporter) to return the current server time for fine-grained PATs. Any feature gated on it will silently misbehave. Advisory UI only.
- The git-refs GET/POST asymmetry (/git/ref/heads/main singular+unprefixed vs POST /git/refs with refs/heads/main fully qualified) plus the absent DELETE-branch endpoint are the most likely source of early integration bugs. octocrab's Reference type hides both — hand-rolling reqwest re-exposes them.
- Querying only check-runs (or only statuses) yields a silently incomplete CI picture: Actions-based CI reports via check runs, external/legacy CI via commit statuses, and neither endpoint surfaces the other. A card would show 'no CI' on repos whose CI works fine.
- Treating PR state=='closed' as merged will auto-move cards to Done when PRs are abandoned. Only merged==true / merged_at!=null indicates a merge — and merged/mergeable are absent from the LIST response entirely, so a list-based implementation cannot determine merge state at all.
- mergeable is null on first read by design. Code that treats null as false will show phantom conflicts on every freshly-opened PR.
- Repo webhooks receive ONLY check_suite.completed — a design expecting check_suite.requested to show 'CI started' on a card will never fire. Use pull_request.synchronize or check_run events for in-progress signals.
- push webhook commits array is capped at 2048; large pushes/force-pushes silently truncate. Detect len==2048 and backfill via the API, or commits go missing from cards.
- Webhook HMAC must be computed over the raw body before deserialization. Using Axum's Json<T> extractor in the handler makes correct verification structurally impossible, and any body re-serialization round-trip breaks the signature. Also: comparing signatures with == violates GitHub's explicit constant-time requirement — use Mac::verify_slice.
- Naive Jira-style [A-Z]{2,}-\d+ matching produces false positives on UTF-8, SHA-256, RFC-7231, COVID-19 etc. Scope the regex to project keys that exist in the DB and validate the card resolves before transitioning.
- Secondary rate limits are the real constraint, not the 5,000/hr primary: 5 points per mutation against 900/min, plus documented guidance to wait ≥1s between mutations and avoid concurrency. Docs also warn limits fire 'for undisclosed reasons' and change without notice. Retrying a permissions-403 as if it were a rate-limit-403 is an easy and costly conflation.
- octocrab is 0.x with a fast breaking cadence (0.51→0.54 in under two months, 4 releases). Unpinned or widely-scattered usage will make upgrades painful — confine it to one adapter module behind Atlas's own trait.
- hmac 0.13 / sha2 0.11 are recent majors (Mar 2026) while much of the ecosystem — likely including octocrab's transitive deps — is still on 0.12/0.10. Mismatched versions mean duplicate crypto crates in the tree and RustCrypto API changes vs. most examples online. Check `cargo tree` and align deliberately.
- CLAUDE.md mandates PATs never appear in logs, Debug output, or API responses. octocrab's SecretString helps, but any hand-rolled error path that formats a reqwest::Request, or a webhook config echoed back from create_hook (config.secret), can leak credentials. Audit error/Debug paths and the hook-creation response specifically.

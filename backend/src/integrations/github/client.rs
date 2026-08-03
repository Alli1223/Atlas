//! The GitHub REST client, and the pure functions that interpret its responses.
//!
//! This is the **only** module in Atlas that names `reqwest` — the confinement the
//! research (`docs/research/github-api.md`, item 10) argues for, so that a client
//! swap (a GitHub App, a different HTTP crate) is a contained edit here rather than
//! a change scattered across every handler.
//!
//! Everything with logic worth testing is a free function that takes already-parsed
//! inputs — an HTTP status and headers, a duration string, a PR's `(state, merged)`
//! — and is tested directly with no network. [`GithubClient`] is the thin glue that
//! performs the request and hands the pieces to those functions.

use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, USER_AGENT};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};
use crate::secrets::vault::ValidationOutcome;
use crate::secrets::{CredentialStatus, Secret};

use super::RepoRef;

/// The fixed GitHub REST base. The single outbound host — see the SSRF note on
/// [`super`]. Never assembled from attacker-influenced input.
pub const GITHUB_API_BASE: &str = "https://api.github.com";

/// The REST API version Atlas pins, per `docs/research/github-api.md`.
const API_VERSION: &str = "2022-11-28";

/// The header GitHub returns a classic PAT's scopes in.
const SCOPES_HEADER: &str = "x-oauth-scopes";

/// The (undocumented) header GitHub returns a token's expiry in.
const EXPIRY_HEADER: &str = "github-authentication-token-expiration";

// ---------------------------------------------------------------------------
// Pure interpretation: token expiry
// ---------------------------------------------------------------------------

/// Parses the `github-authentication-token-expiration` header value.
///
/// # Two formats, and a third case
///
/// The header is undocumented surface (`docs/research/corrections.md` #3) and
/// appears in the wild in **two** layouts, so both are tried:
///
/// 1. numeric offset — `2025-09-05 17:55:53 +0500` (Go `... -0700`);
/// 2. named zone — `2026-06-03 19:52:44 UTC` (Go `... MST`), which in practice
///    from `github.com` is always `UTC`/`GMT`.
///
/// A value that parses under neither — and a **missing** header, which reaches this
/// as `None` at the call site — is treated as *expiry unknown*, never as *never
/// expires* (`corrections.md` #5): the return is `None`, and
/// [`crate::secrets::apply_validation`] leaves any previously known expiry in place
/// rather than wiping it.
#[must_use]
pub fn parse_token_expiry(raw: &str) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    // 1. Numeric offset: `%z` handles `+0500`, `-0700`, `+0000`.
    if let Ok(dt) = DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S %z") {
        return Some(dt.with_timezone(&Utc));
    }

    // 2. Named zone. `chrono`'s `%Z` yields no offset on parse, so a named
    //    abbreviation cannot be resolved in general — but `github.com` only ever
    //    emits `UTC`/`GMT` here, both of which are +0000, so those two are handled
    //    explicitly and anything else is left as unknown rather than guessed.
    for zone in [" UTC", " GMT"] {
        if let Some(stripped) = value.strip_suffix(zone)
            && let Ok(naive) =
                NaiveDateTime::parse_from_str(stripped.trim_end(), "%Y-%m-%d %H:%M:%S")
        {
            return Some(naive.and_utc());
        }
    }

    None
}

/// Parses the `x-oauth-scopes` header into a scope list.
///
/// Returns `None` — meaning *unknown, leave what is stored* — when the header is
/// absent **or** present-but-empty. The empty case is a fine-grained PAT, which has
/// no scope introspection at all (`docs/research/github-api.md` §1); returning
/// `Some(vec![])` there would wipe any scopes a previous probe of a classic token
/// had recorded, so it deliberately does not.
#[must_use]
pub fn parse_scopes_header(raw: Option<&str>) -> Option<Vec<String>> {
    let scopes: Vec<String> = raw?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if scopes.is_empty() {
        None
    } else {
        Some(scopes)
    }
}

// ---------------------------------------------------------------------------
// Pure interpretation: the GET /user validation probe
// ---------------------------------------------------------------------------

/// Classifies a `GET /user` response into a [`ValidationOutcome`].
///
/// - **200** → [`CredentialStatus::Valid`], with scopes read from `x-oauth-scopes`
///   and expiry from the token-expiration header (both parsed defensively above).
///   A 200 means the token works *now*, so it is `Valid` even if the parsed expiry
///   is in the past — the past-expiry-reads-as-expired rule is applied at display
///   time by [`crate::secrets::PillStatus`], against the clock, not here.
/// - **401 / 403** → [`CredentialStatus::Invalid`]: revoked, or lacking the access
///   the probe needs. No scopes or expiry are asserted.
/// - **anything else** (5xx, an unexpected status, a shape GitHub should not send)
///   → an internal error, so the future surfaces a 5xx. This is the load-bearing
///   distinction: a transient failure must **not** be recorded as `Invalid`, which
///   would mark a perfectly good credential dead on a blip. See the [`Validator`]
///   docs (`docs/research/github-api.md`).
///
/// [`Validator`]: crate::secrets::Validator
pub fn interpret_user_response(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
) -> AppResult<ValidationOutcome> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    if status.is_success() {
        return Ok(ValidationOutcome {
            status: CredentialStatus::Valid,
            scopes: parse_scopes_header(header(SCOPES_HEADER)),
            expires_at: header(EXPIRY_HEADER).and_then(parse_token_expiry),
        });
    }

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Ok(ValidationOutcome {
            status: CredentialStatus::Invalid,
            scopes: None,
            expires_at: None,
        });
    }

    Err(AppError::internal(anyhow::anyhow!(
        "GitHub returned an unexpected status {status} from GET /user while validating a token; \
         not recording the credential as invalid on a transient failure"
    )))
}

// ---------------------------------------------------------------------------
// Pure interpretation: CI rollup and PR merge-state
// ---------------------------------------------------------------------------

/// A single GitHub check run, reduced to the two fields the rollup needs.
#[derive(Debug, Clone, Deserialize)]
pub struct CheckRun {
    /// `queued | in_progress | completed | …`.
    pub status: String,
    /// The conclusion once completed: `success | failure | …`, or `null`.
    #[serde(default)]
    pub conclusion: Option<String>,
}

/// The single CI badge a card shows, folded from the two disjoint GitHub systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CiState {
    /// At least one thing succeeded and nothing failed or is still running.
    Passed,
    /// Something is still queued or in progress.
    Running,
    /// Something failed, errored, timed out, or needs action.
    Failed,
    /// Nothing conclusive: no CI, or only skipped/cancelled/neutral results.
    Neutral,
}

/// Folds legacy commit *statuses* and modern *check runs* into one badge.
///
/// The two are disjoint systems — Actions and Apps report via check runs, external
/// CI via commit statuses, and neither endpoint surfaces the other's results — so a
/// complete picture requires both (`docs/research/github-api.md` §5). The precedence
/// is failure-first, quoted from the research: any failing state → `Failed`; else
/// anything pending/incomplete → `Running`; else anything succeeded → `Passed`; else
/// `Neutral`. `skipped`/`neutral`/`cancelled` are non-blocking.
#[must_use]
pub fn ci_rollup(combined_state: Option<&str>, runs: &[CheckRun]) -> CiState {
    let status_failed = matches!(combined_state, Some("failure" | "error"));
    let run_failed = runs.iter().any(|r| {
        matches!(
            r.conclusion.as_deref(),
            Some("failure" | "timed_out" | "action_required")
        )
    });
    if status_failed || run_failed {
        return CiState::Failed;
    }

    let status_running = matches!(combined_state, Some("pending"));
    let run_running = runs.iter().any(|r| r.status != "completed");
    if status_running || run_running {
        return CiState::Running;
    }

    let status_passed = matches!(combined_state, Some("success"));
    let run_passed = runs
        .iter()
        .any(|r| r.conclusion.as_deref() == Some("success"));
    if status_passed || run_passed {
        return CiState::Passed;
    }

    CiState::Neutral
}

/// The merge state of a pull request, resolved from the only two fields that tell
/// the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    /// Still open.
    Open,
    /// Merged. The **only** signal that a card should auto-advance to Done.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Resolves a PR's merge state.
///
/// `state` is only ever `open` or `closed` — never `merged` — so `merged == true`
/// (equivalently `merged_at.is_some()`) is the sole truth of a merge. Treating a
/// bare `closed` as merged would advance cards to Done on abandoned PRs, which is
/// the single most common way this integration goes wrong
/// (`docs/research/github-api.md` §6).
#[must_use]
pub fn pr_state(state: &str, merged: bool) -> PrState {
    if merged {
        PrState::Merged
    } else if state == "closed" {
        PrState::Closed
    } else {
        PrState::Open
    }
}

// ---------------------------------------------------------------------------
// Atlas-facing summary DTOs
// ---------------------------------------------------------------------------

/// A repository as the repo picker shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    /// GitHub's immutable numeric id. Keyed on because it survives renames.
    pub id: i64,
    /// `owner/name`.
    pub full_name: String,
    /// The repo's default branch (`main`, `master`, …) — the branch base.
    #[serde(rename = "default_branch", alias = "defaultBranch")]
    pub default_branch: String,
    /// Whether the token can push. A token can *list* a repo it cannot write to,
    /// so this is the real "can Atlas create a branch here" signal.
    #[serde(default)]
    pub can_push: bool,
    /// Whether the repo is private.
    #[serde(default)]
    pub private: bool,
}

/// A pull request, reduced to what a card shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrSummary {
    /// The PR number.
    pub number: i64,
    /// The PR title.
    pub title: String,
    /// The browser URL.
    pub html_url: String,
    /// Open / merged / closed, resolved via [`pr_state`].
    pub state: PrState,
}

/// A commit on a card's branch.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitSummary {
    /// The full SHA.
    pub sha: String,
    /// The first line of the message.
    pub message: String,
    /// The browser URL.
    pub html_url: String,
}

// ---------------------------------------------------------------------------
// The reqwest client
// ---------------------------------------------------------------------------

/// GitHub REST responses, deserialised just far enough.
mod wire {
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub(super) struct Repo {
        pub id: i64,
        pub full_name: String,
        pub default_branch: String,
        #[serde(default)]
        pub private: bool,
        #[serde(default)]
        pub permissions: Option<Permissions>,
    }

    #[derive(Deserialize)]
    pub(super) struct Permissions {
        #[serde(default)]
        pub push: bool,
    }

    #[derive(Deserialize)]
    pub(super) struct Ref {
        pub object: RefObject,
    }

    #[derive(Deserialize)]
    pub(super) struct RefObject {
        pub sha: String,
    }

    #[derive(Deserialize)]
    pub(super) struct Pull {
        pub number: i64,
        #[serde(default)]
        pub title: String,
        #[serde(default)]
        pub html_url: String,
        #[serde(default)]
        pub state: String,
        #[serde(default)]
        pub merged: bool,
        #[serde(default)]
        pub merged_at: Option<String>,
    }

    #[derive(Deserialize)]
    pub(super) struct Commit {
        pub sha: String,
        #[serde(default)]
        pub html_url: String,
        #[serde(rename = "commit")]
        pub detail: CommitDetail,
    }

    #[derive(Deserialize)]
    pub(super) struct CommitDetail {
        #[serde(default)]
        pub message: String,
    }

    #[derive(Deserialize)]
    pub(super) struct Hook {
        pub id: i64,
    }

    #[derive(Deserialize)]
    pub(super) struct CombinedStatus {
        #[serde(default)]
        pub state: String,
    }

    #[derive(Deserialize)]
    pub(super) struct CheckRuns {
        #[serde(default)]
        pub check_runs: Vec<super::CheckRun>,
    }
}

/// A GitHub REST client bound to one token.
///
/// Cheap to build per request from the vault-opened PAT. Holds no Atlas state and
/// nothing that must be cached across requests — the future GitHub App path swaps
/// the constructor (a JWT/installation token instead of a PAT) and leaves every
/// method below untouched (`docs/research/github-api.md` §11).
#[derive(Debug, Clone)]
pub struct GithubClient {
    http: reqwest::Client,
    base: String,
    token: Secret<String>,
}

impl GithubClient {
    /// Builds a client for `token`, talking to `api.github.com`.
    pub fn new(token: Secret<String>) -> AppResult<Self> {
        Self::with_base_url(token, GITHUB_API_BASE)
    }

    /// Builds a client against an explicit base URL. Production always uses
    /// [`GITHUB_API_BASE`]; this exists so the base is never a literal buried in
    /// each method.
    pub fn with_base_url(token: Secret<String>, base: impl Into<String>) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent("atlas")
            .build()
            .map_err(AppError::internal)?;
        Ok(Self {
            http,
            base: base.into(),
            token,
        })
    }

    /// A request builder with the auth, accept, version and UA headers set.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header(AUTHORIZATION, format!("Bearer {}", self.token.expose()))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(USER_AGENT, "atlas")
    }

    /// Probes `GET /user` and classifies the result — the credential [`Validator`]
    /// path. Reads scopes and expiry from the response headers, which the typed
    /// API would discard.
    ///
    /// [`Validator`]: crate::secrets::Validator
    pub async fn validate(&self) -> AppResult<ValidationOutcome> {
        let resp = self
            .request(reqwest::Method::GET, "/user")
            .send()
            .await
            .map_err(AppError::internal)?;
        interpret_user_response(resp.status(), resp.headers())
    }

    /// Lists the repositories the token can see, most-recently-pushed first, one
    /// page at a time (`page` is 1-based, `per_page` capped at 100 by GitHub).
    pub async fn list_repos(&self, page: u32, per_page: u32) -> AppResult<Vec<RepoSummary>> {
        let per_page = per_page.clamp(1, 100);
        let path = format!("/user/repos?sort=pushed&per_page={per_page}&page={page}");
        let repos: Vec<wire::Repo> = self.send_json(reqwest::Method::GET, &path).await?;
        Ok(repos
            .into_iter()
            .map(|r| RepoSummary {
                id: r.id,
                full_name: r.full_name,
                default_branch: r.default_branch,
                can_push: r.permissions.is_some_and(|p| p.push),
                private: r.private,
            })
            .collect())
    }

    /// Fetches one repository's metadata: `GET /repos/{owner}/{repo}`.
    ///
    /// Used at link time to resolve the immutable `repo_id` and the `default_branch`
    /// a card's branches fork from, and to confirm the token can actually push
    /// (a token can *see* a repo it cannot write to).
    pub async fn get_repo(&self, repo: &RepoRef) -> AppResult<RepoSummary> {
        let path = format!("/repos/{}/{}", repo.owner, repo.repo);
        let r: wire::Repo = self.send_json(reqwest::Method::GET, &path).await?;
        Ok(RepoSummary {
            id: r.id,
            full_name: r.full_name,
            default_branch: r.default_branch,
            can_push: r.permissions.is_some_and(|p| p.push),
            private: r.private,
        })
    }

    /// The commit SHA at the tip of `branch` — the base a new branch forks from.
    ///
    /// `GET /git/ref/heads/{branch}` — singular `ref`, no `refs/` prefix. The
    /// asymmetric sibling of [`create_branch`]'s plural `POST /git/refs`.
    pub async fn base_sha(&self, repo: &RepoRef, branch: &str) -> AppResult<String> {
        let path = format!("/repos/{}/{}/git/ref/heads/{branch}", repo.owner, repo.repo);
        let r: wire::Ref = self.send_json(reqwest::Method::GET, &path).await?;
        Ok(r.object.sha)
    }

    /// Creates `refs/heads/{name}` at `from_sha`.
    ///
    /// `POST /git/refs` — plural, fully-qualified ref. A 422 (the branch already
    /// exists) is folded into success: adopting the existing branch is the right
    /// answer for a card that was already branched.
    pub async fn create_branch(&self, repo: &RepoRef, name: &str, from_sha: &str) -> AppResult<()> {
        let path = format!("/repos/{}/{}/git/refs", repo.owner, repo.repo);
        let body = serde_json::json!({ "ref": format!("refs/heads/{name}"), "sha": from_sha });
        let resp = self
            .request(reqwest::Method::POST, &path)
            .json(&body)
            .send()
            .await
            .map_err(AppError::internal)?;
        if resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Ok(());
        }
        Self::error_for_status(resp).await.map(|_| ())
    }

    /// Opens a pull request from `head` into `base`.
    pub async fn create_pr(
        &self,
        repo: &RepoRef,
        head: &str,
        base: &str,
        title: &str,
        body: Option<&str>,
    ) -> AppResult<PrSummary> {
        let path = format!("/repos/{}/{}/pulls", repo.owner, repo.repo);
        let payload = serde_json::json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body.unwrap_or(""),
        });
        let resp = self
            .request(reqwest::Method::POST, &path)
            .json(&payload)
            .send()
            .await
            .map_err(AppError::internal)?;
        let resp = Self::error_for_status(resp).await?;
        let pull: wire::Pull = resp.json().await.map_err(AppError::internal)?;
        Ok(Self::pull_to_summary(pull))
    }

    /// Finds the PR for a branch without a stored number:
    /// `GET /pulls?head={owner}:{branch}&state=all`.
    pub async fn pr_for_branch(
        &self,
        repo: &RepoRef,
        head_owner: &str,
        branch: &str,
    ) -> AppResult<Option<PrSummary>> {
        let path = format!(
            "/repos/{}/{}/pulls?state=all&head={head_owner}:{branch}",
            repo.owner, repo.repo
        );
        let pulls: Vec<wire::Pull> = self.send_json(reqwest::Method::GET, &path).await?;
        Ok(pulls.into_iter().next().map(Self::pull_to_summary))
    }

    /// Lists commits reachable from a branch tip.
    pub async fn commits(&self, repo: &RepoRef, branch: &str) -> AppResult<Vec<CommitSummary>> {
        let path = format!(
            "/repos/{}/{}/commits?per_page=100&sha={branch}",
            repo.owner, repo.repo
        );
        let commits: Vec<wire::Commit> = self.send_json(reqwest::Method::GET, &path).await?;
        Ok(commits
            .into_iter()
            .map(|c| CommitSummary {
                sha: c.sha,
                message: c.detail.message.lines().next().unwrap_or("").to_owned(),
                html_url: c.html_url,
            })
            .collect())
    }

    /// The folded CI badge for a commit — both the statuses and check-runs systems.
    pub async fn ci_status(&self, repo: &RepoRef, sha: &str) -> AppResult<CiState> {
        let status_path = format!("/repos/{}/{}/commits/{sha}/status", repo.owner, repo.repo);
        let combined: wire::CombinedStatus =
            self.send_json(reqwest::Method::GET, &status_path).await?;

        let runs_path = format!(
            "/repos/{}/{}/commits/{sha}/check-runs",
            repo.owner, repo.repo
        );
        let runs: wire::CheckRuns = self.send_json(reqwest::Method::GET, &runs_path).await?;

        Ok(ci_rollup(Some(&combined.state), &runs.check_runs))
    }

    /// Creates a repository webhook pointing at Atlas's receiver, returning its id.
    pub async fn create_hook(
        &self,
        repo: &RepoRef,
        url: &str,
        secret: &str,
        events: &[&str],
    ) -> AppResult<i64> {
        let path = format!("/repos/{}/{}/hooks", repo.owner, repo.repo);
        let payload = serde_json::json!({
            "name": "web",
            "active": true,
            "events": events,
            "config": {
                "url": url,
                "content_type": "json",
                "secret": secret,
                "insecure_ssl": "0",
            },
        });
        let resp = self
            .request(reqwest::Method::POST, &path)
            .json(&payload)
            .send()
            .await
            .map_err(AppError::internal)?;
        let resp = Self::error_for_status(resp).await?;
        let hook: wire::Hook = resp.json().await.map_err(AppError::internal)?;
        Ok(hook.id)
    }

    /// A `GET` (or other) that deserialises the JSON body, failing on a non-2xx.
    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> AppResult<T> {
        let resp = self
            .request(method, path)
            .send()
            .await
            .map_err(AppError::internal)?;
        let resp = Self::error_for_status(resp).await?;
        resp.json().await.map_err(AppError::internal)
    }

    /// Maps a non-2xx response to an error whose body is **not** echoed to the
    /// client — a GitHub error can carry the token back in a rejected `create_hook`
    /// config, so the cause is logged (via [`AppError::internal`]) and the client
    /// sees only an opaque 500.
    async fn error_for_status(resp: reqwest::Response) -> AppResult<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // The body is read but never surfaced: it is logged as the internal cause.
        let body = resp.text().await.unwrap_or_default();
        Err(AppError::internal(anyhow::anyhow!(
            "GitHub API returned {status}: {body}"
        )))
    }

    fn pull_to_summary(pull: wire::Pull) -> PrSummary {
        let merged = pull.merged || pull.merged_at.is_some();
        PrSummary {
            number: pull.number,
            title: pull.title,
            html_url: pull.html_url,
            state: pr_state(&pull.state, merged),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    // --- token expiry: the two formats and the missing case --------------------

    #[test]
    fn expiry_parses_the_numeric_offset_format() {
        let parsed = parse_token_expiry("2025-09-05 17:55:53 +0500").unwrap();
        // +0500 means the instant is five hours earlier in UTC.
        assert_eq!(parsed.to_rfc3339(), "2025-09-05T12:55:53+00:00");
    }

    #[test]
    fn expiry_parses_the_named_zone_format() {
        let parsed = parse_token_expiry("2026-06-03 19:52:44 UTC").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-06-03T19:52:44+00:00");
        // GMT is handled identically.
        assert_eq!(
            parse_token_expiry("2026-06-03 19:52:44 GMT")
                .unwrap()
                .to_rfc3339(),
            "2026-06-03T19:52:44+00:00"
        );
    }

    #[test]
    fn a_missing_or_unparseable_expiry_is_unknown_not_an_error() {
        // The missing header reaches this as an empty/None value at the call site.
        assert_eq!(parse_token_expiry(""), None);
        assert_eq!(parse_token_expiry("   "), None);
        // A named abbreviation we cannot resolve is unknown, never guessed.
        assert_eq!(parse_token_expiry("2026-06-03 19:52:44 PST"), None);
        assert_eq!(parse_token_expiry("garbage"), None);
    }

    // --- scopes header ---------------------------------------------------------

    #[test]
    fn scopes_split_on_commas_and_trim() {
        assert_eq!(
            parse_scopes_header(Some("repo, read:org , workflow")),
            Some(vec![
                "repo".to_owned(),
                "read:org".to_owned(),
                "workflow".to_owned()
            ])
        );
    }

    #[test]
    fn an_absent_or_empty_scopes_header_is_unknown_not_wiped() {
        // None (absent) and "" (fine-grained PAT) both mean "leave what is stored",
        // which apply_validation honours only for a None ValidationOutcome.scopes.
        assert_eq!(parse_scopes_header(None), None);
        assert_eq!(parse_scopes_header(Some("")), None);
        assert_eq!(parse_scopes_header(Some("   ,  , ")), None);
    }

    // --- GET /user classification ---------------------------------------------

    #[test]
    fn a_200_is_valid_and_carries_scopes_and_expiry() {
        let h = headers(&[
            ("x-oauth-scopes", "repo, workflow"),
            (
                "github-authentication-token-expiration",
                "2026-06-03 19:52:44 UTC",
            ),
        ]);
        let outcome = interpret_user_response(StatusCode::OK, &h).unwrap();
        assert_eq!(outcome.status, CredentialStatus::Valid);
        assert_eq!(
            outcome.scopes,
            Some(vec!["repo".to_owned(), "workflow".to_owned()])
        );
        assert!(outcome.expires_at.is_some());
    }

    #[test]
    fn a_200_with_no_headers_is_valid_with_unknown_scopes_and_expiry() {
        // A fine-grained PAT: valid, but nothing to record. Neither is wiped.
        let outcome = interpret_user_response(StatusCode::OK, &HeaderMap::new()).unwrap();
        assert_eq!(outcome.status, CredentialStatus::Valid);
        assert_eq!(outcome.scopes, None);
        assert_eq!(outcome.expires_at, None);
    }

    #[test]
    fn a_401_or_403_is_invalid() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let outcome = interpret_user_response(status, &HeaderMap::new()).unwrap();
            assert_eq!(outcome.status, CredentialStatus::Invalid);
            assert_eq!(outcome.scopes, None);
        }
    }

    #[test]
    fn a_transient_5xx_is_an_error_not_a_false_invalid() {
        // The load-bearing distinction: a blip must not mark a good token dead.
        let err = interpret_user_response(StatusCode::INTERNAL_SERVER_ERROR, &HeaderMap::new());
        assert!(err.is_err());
        let err = interpret_user_response(StatusCode::BAD_GATEWAY, &HeaderMap::new());
        assert!(err.is_err());
    }

    // --- CI rollup -------------------------------------------------------------

    fn run(status: &str, conclusion: Option<&str>) -> CheckRun {
        CheckRun {
            status: status.to_owned(),
            conclusion: conclusion.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn ci_rollup_is_failure_first() {
        // A failing check wins over a passing status.
        assert_eq!(
            ci_rollup(Some("success"), &[run("completed", Some("failure"))]),
            CiState::Failed
        );
        // ...and a failing legacy status wins over passing checks.
        assert_eq!(
            ci_rollup(Some("failure"), &[run("completed", Some("success"))]),
            CiState::Failed
        );
    }

    #[test]
    fn ci_rollup_reports_running_before_passed() {
        assert_eq!(
            ci_rollup(Some("success"), &[run("in_progress", None)]),
            CiState::Running
        );
        assert_eq!(ci_rollup(Some("pending"), &[]), CiState::Running);
    }

    #[test]
    fn ci_rollup_passes_and_treats_skips_as_neutral() {
        assert_eq!(
            ci_rollup(None, &[run("completed", Some("success"))]),
            CiState::Passed
        );
        // Only skipped/cancelled/neutral, nothing conclusive.
        assert_eq!(
            ci_rollup(None, &[run("completed", Some("skipped"))]),
            CiState::Neutral
        );
        assert_eq!(ci_rollup(None, &[]), CiState::Neutral);
    }

    // --- PR merge-state --------------------------------------------------------

    #[test]
    fn pr_state_only_calls_a_pr_merged_when_merged_is_true() {
        assert_eq!(pr_state("open", false), PrState::Open);
        assert_eq!(pr_state("closed", false), PrState::Closed);
        // The trap: closed without merged is NOT a merge.
        assert_ne!(pr_state("closed", false), PrState::Merged);
        assert_eq!(pr_state("closed", true), PrState::Merged);
    }
}

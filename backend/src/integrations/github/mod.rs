//! GitHub integration: link a project to a repo, drive the card→branch→PR flow,
//! receive webhooks, and act on smart commits (`TODO.md` Phase 12).
//!
//! # Layout
//!
//! - [`client`] — the one module that names `reqwest`. The [`client::GithubClient`]
//!   wraps the REST calls Atlas makes to `api.github.com`, and the module also
//!   holds the *pure* response-interpretation functions (token-expiry parsing, the
//!   `GET /user` classification, the CI rollup, PR merge-state) that carry all the
//!   logic worth testing — tested directly, with no network.
//! - [`validator`] — the [`crate::secrets::Validator`] for `Provider::Github`,
//!   routed to from [`crate::secrets::vault::default_validator`].
//! - [`webhook`] — HMAC-SHA256 signature verification over the raw body (the only
//!   thing standing between the unauthenticated receiver and card mutation) and
//!   the event payload types.
//! - [`smart_commit`] — the `ATLAS-42 #done #comment … #time 2h` parser and its
//!   application against the workflow engine.
//! - [`branch`] — turning a card into a sanitised git branch name.
//! - [`store`] — the `project_repos` / `card_git_links` / `card_worklogs` rows and
//!   their queries.
//!
//! # SSRF posture
//!
//! Every outbound call Atlas makes is to one fixed host, `api.github.com`, built
//! from a compile-time constant ([`client::GITHUB_API_BASE`]). No URL derived from
//! a webhook body, a repo payload, or any other attacker-influenced input is ever
//! fetched. Keeping it that way is the SSRF control: there is no code path that
//! turns remote data into an outbound request target. When Phase 12's "remote
//! links" or avatar-proxying land, they must not break that invariant — a URL that
//! came from GitHub is still attacker-influenced and must be validated against
//! internal ranges before it is fetched.

pub mod branch;
pub mod client;
pub mod store;
// smart_commit is Phase 12's later half (the `ATLAS-42 #done #time 2h` parser),
// not yet written — declared in the module map above but stubbed out of the build.
// pub mod smart_commit;
pub mod validator;
pub mod webhook;

use std::fmt;

/// A repository, addressed the way every GitHub REST path wants it: `{owner}/{repo}`.
///
/// Deliberately *not* keyed on the token owner's login. A GitHub App has no
/// `/user`, so any design that models "the repo of the authenticated user" is a
/// migration blocker — everything keys on `owner`/`repo` (and, in the database, on
/// the immutable `repo.id`), never on who the credential belongs to. See
/// `docs/research/github-api.md` §11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// The owning user or organisation login.
    pub owner: String,
    /// The repository name.
    pub repo: String,
}

impl RepoRef {
    /// Builds a repo reference.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
        }
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

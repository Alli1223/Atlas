//! The GitHub integration's HTTP surface: link a project to a repo, pick a repo
//! for a credential, and create a branch from a card.
//!
//! The GitHub REST calls all live in [`crate::integrations::github::client`]; the
//! persistence in [`crate::integrations::github::store`]. This module is the thin
//! Atlas-facing layer that ties a project/card to a repo, opens the stored PAT
//! from the vault, and maps rows to wire DTOs.
//!
//! # Scoping
//!
//! The repo picker (`GET /credentials/{id}/repos`) is instance-level and
//! `RequireAdmin`, like everything else that touches a credential. The project↔repo
//! link and the card→branch action are project-scoped: the `project_access` layer
//! gates them on the project `{key}` / card `{key}` before the handler runs, so an
//! outsider gets a 404 from the gate and never reaches the code here.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::{AppState, projects};
use crate::auth::extract::RequireAdmin;
use crate::auth::{CurrentUser, now};
use crate::domain::card;
use crate::error::{AppError, AppResult, Problem};
use crate::integrations::github::RepoRef;
use crate::integrations::github::backfill;
use crate::integrations::github::branch;
use crate::integrations::github::client::{
    CiState, CommitSummary, GithubClient, PrState, RepoSummary, ReviewState, review_rollup,
};
use crate::integrations::github::store::{
    self, CardGitLink, NewCardGitLink, NewProjectRepo, ProjectRepo,
};
use crate::secrets::vault::Vault;
use crate::secrets::{self, Provider, Secret};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// The body of a link request: which credential, and which repo.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRepoRequest {
    /// The GitHub credential whose PAT Atlas will act with.
    pub credential_id: String,
    /// The repository owner (user or org login).
    pub owner: String,
    /// The repository name.
    pub repo: String,
    /// Prefix for generated branch names. Defaults to `feature`.
    #[serde(default)]
    pub branch_prefix: Option<String>,
}

/// The repo a project is linked to, as the API describes it. Carries no secret.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRepoDto {
    /// The repository owner.
    pub owner: String,
    /// The repository name.
    pub repo: String,
    /// `owner/name`.
    pub full_name: String,
    /// GitHub's immutable numeric id.
    pub repo_id: i64,
    /// The default branch new branches fork from.
    pub default_branch: String,
    /// The prefix for generated branch names.
    pub branch_prefix: String,
    /// The credential driving the link, or `null` if it was deleted.
    pub credential_id: Option<String>,
    /// Whether an Atlas webhook is installed on the repo.
    pub webhook_configured: bool,
    /// When the link was created.
    pub linked_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl ProjectRepoDto {
    fn from_row(r: &ProjectRepo) -> Self {
        Self {
            owner: r.owner.clone(),
            repo: r.repo.clone(),
            full_name: format!("{}/{}", r.owner, r.repo),
            repo_id: r.repo_id,
            default_branch: r.default_branch.clone(),
            branch_prefix: r.branch_prefix.clone(),
            credential_id: r.credential_id.clone(),
            webhook_configured: r.webhook_id.is_some(),
            linked_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// A git object linked to a card.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardGitLinkDto {
    /// `branch` | `pr` | `commit`.
    pub kind: String,
    /// The branch name, PR number, or commit SHA.
    pub reference: String,
    /// The browser URL, if known.
    pub url: Option<String>,
    /// A `kind`-specific state, if known.
    pub state: Option<String>,
    /// When Atlas first recorded it.
    pub created_at: DateTime<Utc>,
}

impl CardGitLinkDto {
    fn from_row(r: &CardGitLink) -> Self {
        Self {
            kind: r.kind.clone(),
            reference: r.git_ref.clone(),
            url: r.url.clone(),
            state: r.state.clone(),
            created_at: r.created_at,
        }
    }
}

/// The branch a card→branch action created.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCreatedDto {
    /// The full branch name, e.g. `feature/ATLAS-42-add-login`.
    pub branch: String,
    /// The browser URL for the branch.
    pub url: String,
    /// The branch it was forked from.
    pub base_branch: String,
}

/// Pagination for the repo picker.
#[derive(Debug, Deserialize)]
struct RepoPageQuery {
    /// 1-based page number; defaults to the first page.
    page: Option<u32>,
}

// ---------------------------------------------------------------------------
// Handlers — repo picker (admin, per credential)
// ---------------------------------------------------------------------------

/// Lists the repositories a GitHub credential can see, for the link picker.
#[utoipa::path(
    get,
    path = "/credentials/{id}/repos",
    tag = "github",
    params(
        ("id" = String, Path, description = "The GitHub credential to list repositories for"),
        ("page" = Option<u32>, Query, description = "1-based page (30 per page)"),
    ),
    responses(
        (status = 200, description = "Repositories, most-recently-pushed first", body = Vec<RepoSummary>),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such credential, or it is not a GitHub credential", body = Problem),
        (status = 500, description = "The vault is not configured, or GitHub errored", body = Problem),
    )
)]
async fn list_credential_repos(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<String>,
    Query(page): Query<RepoPageQuery>,
) -> AppResult<Json<Vec<RepoSummary>>> {
    let vault = require_vault(&state)?;
    let credential = secrets::find_by_id(&state.db, &id)
        .await?
        .filter(|c| c.provider == Provider::Github)
        .ok_or(AppError::NotFound)?;
    let client = GithubClient::new(vault.open(&credential)?)?;
    let repos = client.list_repos(page.page.unwrap_or(1), 30).await?;
    Ok(Json(repos))
}

// ---------------------------------------------------------------------------
// Handlers — the project ↔ repo link
// ---------------------------------------------------------------------------

/// The repo linked to a project.
#[utoipa::path(
    get,
    path = "/projects/{key}/repo",
    tag = "github",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The linked repository", body = ProjectRepoDto),
        (status = 404, description = "No repo is linked, or no such project", body = Problem),
    )
)]
async fn get_project_repo(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<ProjectRepoDto>> {
    let project = projects::by_key(&state.db, &key).await?;
    let repo = store::find_project_repo(&state.db, &project.id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(ProjectRepoDto::from_row(&repo)))
}

/// Links (or relinks) a project to a repository. One repo per project.
#[utoipa::path(
    put,
    path = "/projects/{key}/repo",
    tag = "github",
    params(("key" = String, Path, description = "The project key")),
    request_body = LinkRepoRequest,
    responses(
        (status = 200, description = "Linked", body = ProjectRepoDto),
        (status = 404, description = "No such project", body = Problem),
        (status = 422, description = "The credential is missing or not a GitHub credential", body = Problem),
        (status = 500, description = "The vault is not configured, or GitHub errored", body = Problem),
    )
)]
async fn link_project_repo(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
    Json(body): Json<LinkRepoRequest>,
) -> AppResult<Json<ProjectRepoDto>> {
    let vault = require_vault(&state)?;
    let now = now();

    let project = projects::by_key(&state.db, &key).await?;
    let credential = secrets::find_by_id(&state.db, &body.credential_id)
        .await?
        .filter(|c| c.provider == Provider::Github)
        .ok_or_else(|| {
            AppError::Validation("no such GitHub credential for credentialId".to_owned())
        })?;

    // Fetch the repo with the PAT: this both confirms the credential can reach the
    // repo and resolves the immutable id + default branch we persist.
    let client = GithubClient::new(vault.open(&credential)?)?;
    let repo_ref = RepoRef::new(body.owner.trim(), body.repo.trim());
    let summary = client.get_repo(&repo_ref).await?;

    // Prefer GitHub's canonical `owner/name` casing over whatever was typed.
    let (owner, repo) = summary
        .full_name
        .split_once('/')
        .unwrap_or((body.owner.trim(), body.repo.trim()));
    let branch_prefix = body
        .branch_prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(branch::DEFAULT_BRANCH_TYPE);

    let mut tx = state.db.begin_write().await?;
    let stored = store::upsert_project_repo(
        &mut tx,
        &NewProjectRepo {
            project_id: &project.id,
            credential_id: Some(&credential.id),
            owner,
            repo,
            repo_id: summary.id,
            default_branch: &summary.default_branch,
            branch_prefix,
        },
        now,
    )
    .await?;
    tx.commit().await?;

    tracing::info!(project = %key, repo = %format!("{owner}/{repo}"), "linked project to a GitHub repo");

    // Install a webhook only if Atlas knows where GitHub could reach it. Most instances
    // are behind NAT or otherwise have no public address — that is a normal deployment,
    // not a misconfiguration, so this is skipped rather than required.
    let stored = match &state.config.public_url {
        Some(public_url) => {
            install_webhook(&state, vault, &client, &repo_ref, &stored, public_url, now).await
        }
        None => stored,
    };

    // Seeds git-links for cards whose branch/PR on this repo predates the link — a card
    // created via `POST /cards/{key}/branch` after this point gets its link recorded
    // immediately by that same handler, so this is purely for what already existed.
    // Best-effort like the webhook install above: it runs after the link itself already
    // succeeded, so a failure here must not undo it.
    backfill::backfill(&state, &client, &stored).await;

    Ok(Json(ProjectRepoDto::from_row(&stored)))
}

/// The events Atlas's webhook receiver actually interprets (`webhook::parse_event`).
/// Deliberately not a longer list: subscribing to an event `dispatch` would silently drop
/// (`status`, `pull_request_review`, `check_run`) would just be noise GitHub sends and
/// Atlas discards.
const WEBHOOK_EVENTS: &[&str] = &["push", "pull_request", "check_suite", "create", "delete"];

/// Creates a GitHub webhook for a newly-linked repo and stores its sealed secret.
///
/// Best-effort: a failure here (a token missing `admin:repo_hook`, a transient GitHub
/// error) is logged and swallowed rather than failing the whole link — the repo link
/// itself already succeeded and is useful without a webhook (manual branch/PR creation
/// still works), so losing that over a secondary concern would be the wrong trade.
async fn install_webhook(
    state: &AppState,
    vault: &Vault,
    client: &GithubClient,
    repo_ref: &RepoRef,
    repo: &ProjectRepo,
    public_url: &str,
    now: DateTime<Utc>,
) -> ProjectRepo {
    let webhook_url = format!("{}/webhooks/github", public_url.trim_end_matches('/'));
    let secret = generate_webhook_secret();

    let result: AppResult<()> = async {
        let webhook_id = client
            .create_hook(repo_ref, &webhook_url, &secret, WEBHOOK_EVENTS)
            .await?;
        let sealed = vault.seal_for(&repo.id, &Secret::new(secret))?;
        let mut tx = state.db.begin_write().await?;
        store::set_webhook(&mut tx, &repo.id, webhook_id, &sealed, now).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;

    match result {
        Ok(()) => store::find_project_repo(&state.db, &repo.project_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| repo.clone()),
        Err(err) => {
            tracing::warn!(
                error = %err,
                repo = %format!("{}/{}", repo_ref.owner, repo_ref.repo),
                "failed to install a GitHub webhook for a newly-linked repo; \
                 the repo is still linked, just without push-driven updates"
            );
            repo.clone()
        }
    }
}

/// Generates a webhook secret: 256 bits of OS entropy, base64url, unpadded.
///
/// `OsRng` here is argon2's re-export (`rand_core` 0.6) — the same one
/// [`crate::auth::session`]'s token generator uses, so Atlas still does not depend on
/// `rand` at all for this.
fn generate_webhook_secret() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Unlinks a project's repo.
#[utoipa::path(
    delete,
    path = "/projects/{key}/repo",
    tag = "github",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 204, description = "Unlinked"),
        (status = 404, description = "No repo was linked, or no such project", body = Problem),
    )
)]
async fn unlink_project_repo(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<StatusCode> {
    let project = projects::by_key(&state.db, &key).await?;
    let mut tx = state.db.begin_write().await?;
    let removed = store::delete_project_repo(&mut tx, &project.id).await?;
    tx.commit().await?;
    if !removed {
        return Err(AppError::NotFound);
    }
    tracing::info!(project = %key, "unlinked a project from its GitHub repo");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Handlers — the card → branch action
// ---------------------------------------------------------------------------

/// Creates a branch on the linked repo from a card, named `{prefix}/{key}-{slug}`.
#[utoipa::path(
    post,
    path = "/cards/{key}/branch",
    tag = "github",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 200, description = "The branch (created, or adopted if it already existed)", body = BranchCreatedDto),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "The card's project has no linked repo, or its credential is gone", body = Problem),
        (status = 500, description = "The vault is not configured, or GitHub errored", body = Problem),
    )
)]
async fn create_branch_from_card(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<BranchCreatedDto>> {
    let vault = require_vault(&state)?;
    let now = now();

    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let repo = store::find_project_repo(&state.db, &card.project_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("no GitHub repo is linked to this card's project".to_owned())
        })?;
    let credential_id = repo
        .credential_id
        .as_deref()
        .ok_or_else(|| AppError::Conflict("the linked repo has no usable credential".to_owned()))?;
    let credential = secrets::find_by_id(&state.db, credential_id)
        .await?
        .ok_or_else(|| AppError::Conflict("the repo's credential no longer exists".to_owned()))?;

    let client = GithubClient::new(vault.open(&credential)?)?;
    let repo_ref = repo.repo_ref();
    let base = client.base_sha(&repo_ref, &repo.default_branch).await?;
    let name = branch::branch_name(&repo.branch_prefix, &card.key, &card.summary);
    client.create_branch(&repo_ref, &name, &base).await?;
    let url = format!(
        "https://github.com/{}/{}/tree/{name}",
        repo_ref.owner, repo_ref.repo
    );

    let mut tx = state.db.begin_write().await?;
    store::upsert_card_git_link(
        &mut tx,
        &NewCardGitLink {
            card_id: &card.id,
            kind: "branch",
            git_ref: &name,
            url: Some(&url),
            state: None,
            meta: None,
        },
        now,
    )
    .await?;
    tx.commit().await?;

    tracing::info!(card = %card.key, branch = %name, "created a branch from a card");
    Ok(Json(BranchCreatedDto {
        branch: name,
        url,
        base_branch: repo.default_branch,
    }))
}

/// Opens a pull request from a card's branch into the repo's default branch.
///
/// **Idempotent at the Atlas layer**: if a PR is already recorded against this card, that
/// record is returned as-is and GitHub is never called again — clicking the button twice
/// must not risk a confusing "a pull request already exists" error surfacing as an opaque
/// 500 (`GithubClient::error_for_status` never echoes GitHub's error body to the client).
#[utoipa::path(
    post,
    path = "/cards/{key}/pr",
    tag = "github",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 200, description = "The PR — newly opened, or the one already recorded for this card", body = CardGitLinkDto),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "No repo linked, no usable credential, or the card has no branch yet", body = Problem),
        (status = 500, description = "The vault is not configured, or GitHub errored", body = Problem),
    )
)]
async fn create_pr_from_card(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<CardGitLinkDto>> {
    let vault = require_vault(&state)?;
    let now = now();

    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let repo = store::find_project_repo(&state.db, &card.project_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("no GitHub repo is linked to this card's project".to_owned())
        })?;

    let links = store::list_card_git_links(&state.db, &card.id).await?;
    if let Some(existing) = links.iter().find(|link| link.kind == "pr") {
        return Ok(Json(CardGitLinkDto::from_row(existing)));
    }
    let branch = links
        .iter()
        .find(|link| link.kind == "branch")
        .ok_or_else(|| {
            AppError::Conflict(
                "create a branch from this card before opening a pull request".to_owned(),
            )
        })?;

    let credential_id = repo
        .credential_id
        .as_deref()
        .ok_or_else(|| AppError::Conflict("the linked repo has no usable credential".to_owned()))?;
    let credential = secrets::find_by_id(&state.db, credential_id)
        .await?
        .ok_or_else(|| AppError::Conflict("the repo's credential no longer exists".to_owned()))?;

    let client = GithubClient::new(vault.open(&credential)?)?;
    let repo_ref = repo.repo_ref();
    let pr = client
        .create_pr(
            &repo_ref,
            &branch.git_ref,
            &repo.default_branch,
            &card.summary,
            Some(&format!("Opened from {} on Atlas.", card.key)),
        )
        .await?;

    let number = pr.number.to_string();
    let pr_state = match pr.state {
        PrState::Open => "open",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };

    let mut tx = state.db.begin_write().await?;
    let stored = store::upsert_card_git_link(
        &mut tx,
        &NewCardGitLink {
            card_id: &card.id,
            kind: "pr",
            git_ref: &number,
            url: Some(&pr.html_url),
            state: Some(pr_state),
            meta: Some(&pr.title),
        },
        now,
    )
    .await?;
    tx.commit().await?;

    tracing::info!(card = %card.key, pr = pr.number, "opened a pull request from a card");
    Ok(Json(CardGitLinkDto::from_row(&stored)))
}

/// Lists the git objects (branches, PRs, commits) linked to a card.
#[utoipa::path(
    get,
    path = "/cards/{key}/git-links",
    tag = "github",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The card's git links, newest first", body = Vec<CardGitLinkDto>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn card_git_links(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardGitLinkDto>>> {
    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let links = store::list_card_git_links(&state.db, &card.id).await?;
    Ok(Json(links.iter().map(CardGitLinkDto::from_row).collect()))
}

/// Live GitHub activity for a card's branch: its commits, and the CI state of the latest one.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardActivityDto {
    /// The branch's commits, newest first (capped at 100 by GitHub).
    pub commits: Vec<CommitSummary>,
    /// The CI state of the newest commit, or `null` if the branch has no commits at all.
    pub ci_status: Option<CiState>,
    /// Whether the card's PR can merge cleanly, or `null` when there is no PR yet **or**
    /// GitHub has not finished computing it — the two are indistinguishable here, and both
    /// mean "nothing to show", never "conflicts".
    pub mergeable: Option<bool>,
    /// The card's PR review rollup, or `null` when there is no PR yet.
    pub review_state: Option<ReviewState>,
}

/// A card's live commits and CI status, read straight from GitHub — nothing here is cached
/// or stored, since a check's state is only ever meaningful as of right now.
#[utoipa::path(
    get,
    path = "/cards/{key}/activity",
    tag = "github",
    params(("key" = String, Path, description = "The card key, e.g. ATLAS-42")),
    responses(
        (status = 200, description = "The branch's commits and the newest one's CI state", body = CardActivityDto),
        (status = 404, description = "No such card", body = Problem),
        (status = 409, description = "No repo linked, no branch created yet, or the credential is gone", body = Problem),
        (status = 500, description = "The vault is not configured, or GitHub errored", body = Problem),
    )
)]
async fn card_activity(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<CardActivityDto>> {
    let vault = require_vault(&state)?;

    let card = card::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let repo = store::find_project_repo(&state.db, &card.project_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("no GitHub repo is linked to this card's project".to_owned())
        })?;
    let links = store::list_card_git_links(&state.db, &card.id).await?;
    let branch = links
        .iter()
        .find(|link| link.kind == "branch")
        .ok_or_else(|| AppError::Conflict("this card has no branch yet".to_owned()))?;
    let credential_id = repo
        .credential_id
        .as_deref()
        .ok_or_else(|| AppError::Conflict("the linked repo has no usable credential".to_owned()))?;
    let credential = secrets::find_by_id(&state.db, credential_id)
        .await?
        .ok_or_else(|| AppError::Conflict("the repo's credential no longer exists".to_owned()))?;

    let client = GithubClient::new(vault.open(&credential)?)?;
    let repo_ref = repo.repo_ref();
    let commits = client.commits(&repo_ref, &branch.git_ref).await?;
    // GitHub's commits endpoint is newest-first, so the head of the list is the tip — exactly
    // the commit a CI badge should reflect. No branch has zero commits in practice (it forks
    // from a real base), but an empty response is handled rather than assumed away.
    let ci_status = match commits.first() {
        Some(tip) => Some(client.ci_status(&repo_ref, &tip.sha).await?),
        None => None,
    };

    // Mergeable state and reviews only exist once a PR does. A card can have a branch with
    // no PR yet, which is a normal state here, not an error.
    let pr_number = links
        .iter()
        .find(|link| link.kind == "pr")
        .and_then(|link| link.git_ref.parse::<i64>().ok());
    let (mergeable, review_state) = match pr_number {
        Some(number) => {
            let mergeable = client.mergeable(&repo_ref, number).await?;
            let reviews = client.reviews(&repo_ref, number).await?;
            (mergeable, Some(review_rollup(&reviews)))
        }
        None => (None, None),
    };

    Ok(Json(CardActivityDto {
        commits,
        ci_status,
        mergeable,
        review_state,
    }))
}

// ---------------------------------------------------------------------------

/// The vault, or a 500 explaining it is unconfigured. A GitHub call needs a PAT,
/// which needs the vault — the same guard the credentials API uses.
fn require_vault(state: &AppState) -> AppResult<&Vault> {
    state.vault.as_deref().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!(
            "the secrets vault is not configured: set ATLAS_MASTER_KEY"
        ))
    })
}

/// Assembles the routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_credential_repos))
        .routes(routes!(
            get_project_repo,
            link_project_repo,
            unlink_project_repo
        ))
        .routes(routes!(create_branch_from_card))
        .routes(routes!(create_pr_from_card))
        .routes(routes!(card_git_links))
        .routes(routes!(card_activity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_webhook_secret_is_256_bits_url_safe_and_never_repeats() {
        let a = generate_webhook_secret();
        let b = generate_webhook_secret();

        assert_ne!(a, b, "two secrets must not collide");
        // 32 bytes, base64url, unpadded: ceil(32 * 4 / 3) = 43 characters.
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "must be URL-safe: {a:?}"
        );
    }

    #[test]
    fn the_webhook_event_list_is_exactly_what_parse_event_understands() {
        // A subscription the receiver would just discard is silent waste, not a feature.
        assert_eq!(
            WEBHOOK_EVENTS,
            &["push", "pull_request", "check_suite", "create", "delete"]
        );
    }
}

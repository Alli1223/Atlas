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
use crate::integrations::github::branch;
use crate::integrations::github::client::{GithubClient, RepoSummary};
use crate::integrations::github::store::{
    self, CardGitLink, NewCardGitLink, NewProjectRepo, ProjectRepo,
};
use crate::secrets::vault::Vault;
use crate::secrets::{self, Provider};

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
    Ok(Json(ProjectRepoDto::from_row(&stored)))
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
        .routes(routes!(card_git_links))
}

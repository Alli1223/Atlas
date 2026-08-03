//! The GitHub webhook receiver: the one Atlas endpoint that is *not* authenticated by a
//! session, because GitHub has none to offer.
//!
//! # The signature is the whole gate
//!
//! Mounted at the top level (outside `/api/v1`, so none of the session / project-access
//! middleware runs) and listed in [`crate::auth::project_access`]'s `UNGATED_PATHS` so the
//! binary will boot. In its place stands the HMAC: the handler extracts the **raw**
//! [`Bytes`] (never a `Json` extractor — re-serialising would change the bytes and break the
//! signature), finds which repo the delivery claims to be from, opens *that repo's* stored
//! secret, and verifies the signature in constant time before parsing or acting on anything.
//!
//! Reading `repository.id` off the unverified body to *select the secret* is safe — it
//! decides what to check against, never what to do. No URL from the payload is ever fetched
//! (the SSRF invariant in [`crate::integrations::github`]).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::now;
use crate::domain::{StatusCategory, card};
use crate::error::{AppError, AppResult, Problem};
use crate::integrations::github::webhook::{self, WebhookEvent};
use crate::integrations::github::{smart_commit, store};

/// Receives a GitHub webhook delivery.
#[utoipa::path(
    post,
    path = "/webhooks/github",
    tag = "github",
    request_body(
        content = String,
        description = "The raw GitHub webhook JSON delivery",
        content_type = "application/json",
    ),
    responses(
        (status = 202, description = "Delivery accepted — processed, or acknowledged and ignored"),
        (status = 400, description = "Malformed payload or missing event header", body = Problem),
        (status = 401, description = "Missing or invalid signature", body = Problem),
        (status = 404, description = "The repo is not linked, or has no webhook secret", body = Problem),
    )
)]
async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let event_name = header(&headers, webhook::EVENT_HEADER)
        .ok_or_else(|| AppError::BadRequest("missing the X-GitHub-Event header".to_owned()))?;

    // Which repo does the delivery claim to be from? Read (not trusted) purely to pick the
    // secret to verify against.
    let repo_id = peek_repo_id(&body)
        .ok_or_else(|| AppError::BadRequest("payload has no repository id".to_owned()))?;

    let Some(binding) = store::find_repo_webhook_by_repo_id(&state.db, repo_id).await? else {
        return Err(AppError::NotFound);
    };
    let (Some(ciphertext), Some(nonce), Some(key_version)) = (
        binding.webhook_secret_ciphertext,
        binding.webhook_secret_nonce,
        binding.webhook_secret_key_version,
    ) else {
        // Linked, but no hook installed → nothing to verify against → refuse rather than act.
        return Err(AppError::NotFound);
    };

    let vault = state.vault.as_deref().ok_or_else(|| {
        AppError::internal(anyhow::anyhow!("the secrets vault is not configured"))
    })?;
    let secret = vault.open_bytes(&binding.id, &nonce, &ciphertext, key_version)?;

    // The whole authentication: HMAC over the raw body, constant-time, before any parse.
    let signature = header(&headers, webhook::SIGNATURE_HEADER).unwrap_or_default();
    if !webhook::verify_signature(secret.expose().as_bytes(), &body, signature) {
        return Err(AppError::Unauthorized);
    }

    if let Some(event) = webhook::parse_event(event_name, &body)? {
        dispatch(&state, &binding.project_id, event).await?;
    }

    Ok(StatusCode::ACCEPTED)
}

/// Acts on a verified event. Anything Atlas does not handle is a silent no-op — the delivery
/// was already acknowledged.
async fn dispatch(state: &AppState, project_id: &str, event: WebhookEvent) -> AppResult<()> {
    match event {
        WebhookEvent::Push { commits, .. } => {
            for commit in commits {
                // A merge commit's aggregated body re-states the branch's directives; acting
                // on it would re-fire every `#done` on merge (research §9).
                if is_merge_commit(&commit.message) {
                    continue;
                }
                let parsed = smart_commit::parse(&commit.message);
                smart_commit::apply(&state.db, &parsed, project_id, now()).await?;
            }
        }
        WebhookEvent::PullRequest {
            action,
            branch,
            merged,
            number,
            title,
            html_url,
        } => {
            handle_pull_request(
                state, project_id, &action, &branch, merged, number, &title, &html_url,
            )
            .await?;
        }
        // check_suite / create / delete are parsed but not yet acted on.
        _ => {}
    }
    Ok(())
}

/// Records a PR against the card its branch was cut from, and auto-transitions it: a merge
/// closes the card (→ Done), an open/reopen starts it (→ In Progress).
#[allow(clippy::too_many_arguments)]
async fn handle_pull_request(
    state: &AppState,
    project_id: &str,
    action: &str,
    branch: &str,
    merged: bool,
    number: i64,
    title: &str,
    html_url: &str,
) -> AppResult<()> {
    let Some(key) = smart_commit::key_in_branch(branch) else {
        return Ok(());
    };
    let Some(card) = card::find_by_key(&state.db, &key).await? else {
        return Ok(());
    };
    if card.project_id != project_id {
        return Ok(());
    }

    let pr_state = if merged {
        "merged"
    } else if action == "closed" {
        "closed"
    } else {
        "open"
    };
    let number_str = number.to_string();
    let mut tx = state.db.begin_write().await?;
    store::upsert_card_git_link(
        &mut tx,
        &store::NewCardGitLink {
            card_id: &card.id,
            kind: "pr",
            git_ref: &number_str,
            url: Some(html_url),
            state: Some(pr_state),
            meta: Some(title),
        },
        now(),
    )
    .await?;
    tx.commit().await?;

    let category = if merged {
        Some(StatusCategory::Done)
    } else if action == "opened" || action == "reopened" {
        Some(StatusCategory::InProgress)
    } else {
        None
    };
    if let Some(category) = category {
        smart_commit::move_to_category(&state.db, &card, category, &card.creator_id, now()).await?;
    }

    Ok(())
}

/// The first `Merge …` line marks an aggregated merge commit.
fn is_merge_commit(message: &str) -> bool {
    let first = message.lines().next().unwrap_or_default();
    first.starts_with("Merge pull request")
        || first.starts_with("Merge branch")
        || first.starts_with("Merge remote-tracking")
}

/// Reads `repository.id` from a raw body without trusting it.
fn peek_repo_id(body: &[u8]) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct Peek {
        repository: Repo,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        id: i64,
    }
    serde_json::from_slice::<Peek>(body)
        .ok()
        .map(|p| p.repository.id)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The receiver's route, mounted at the top level (outside `/api/v1`).
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(receive))
}

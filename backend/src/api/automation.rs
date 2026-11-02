//! Automation rules: `/projects/{key}/automation/rules`, `/automation/rules/{id}`, the audit
//! log, manual runs, dry runs — and the public incoming-webhook receiver,
//! `POST /hooks/automation/{token}`.
//!
//! Defining a rule is project configuration (Owner): a rule acts with its owner's authority
//! on every card in the project, unattended. Running a manual rule against a card is card work
//! (Member). Reading rules and their audit log is Viewer.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now};
use crate::automation::engine::{self, Invocation, RunResult};
use crate::automation::model::{RuleSpec, Trigger};
use crate::automation::store::{self, Rule, RuleInput, Run};
use crate::domain::card;
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// The body of a rule create or replace.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleRequest {
    /// A name.
    pub name: String,
    /// What it is for.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether it runs. Default on.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Trigger, conditions, actions.
    pub spec: RuleSpec,
}

fn yes() -> bool {
    true
}

/// The body of `POST /automation/rules/{id}/run`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequest {
    /// The card to run against.
    #[serde(default)]
    pub card_key: Option<String>,
}

/// The body of `POST /projects/{key}/automation/dry-run`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DryRunRequest {
    /// The (possibly unsaved) rule.
    pub spec: RuleSpec,
    /// A card to evaluate it against.
    #[serde(default)]
    pub card_key: Option<String>,
}

/// A freshly issued webhook token — shown exactly once.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookToken {
    /// The secret token.
    pub token: String,
    /// The path to POST to (prefix it with this instance's public URL).
    pub path: String,
}

fn check(body: &RuleRequest) -> AppResult<(String, Option<String>)> {
    let name = store::validate_name(&body.name)?;
    body.spec.validate()?;
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    if description
        .as_ref()
        .is_some_and(|d| d.chars().count() > 2000)
    {
        return Err(AppError::Validation(
            "A rule description must be at most 2000 characters.".to_owned(),
        ));
    }
    Ok((name, description))
}

/// A project's rules.
#[utoipa::path(
    get,
    path = "/projects/{key}/automation/rules",
    tag = "automation",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "Rules, by name", body = Vec<Rule>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_rules(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Rule>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(store::list(&state.db, &project.id).await?))
}

/// Creates a rule. It acts with the creator's authority.
#[utoipa::path(
    post,
    path = "/projects/{key}/automation/rules",
    tag = "automation",
    params(("key" = String, Path, description = "The project key")),
    request_body = RuleRequest,
    responses(
        (status = 201, description = "Created", body = Rule),
        (status = 422, description = "An invalid rule", body = Problem),
    )
)]
async fn create_rule(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<RuleRequest>,
) -> AppResult<(StatusCode, Json<Rule>)> {
    let (name, description) = check(&body)?;
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    let id = store::insert(
        &mut tx,
        &project.id,
        member.0.id(),
        &RuleInput {
            name: &name,
            description: description.as_deref(),
            enabled: body.enabled,
            spec: &body.spec,
        },
        now(),
    )
    .await?;
    tx.commit().await?;
    let rule = store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(rule)))
}

/// One rule.
#[utoipa::path(
    get,
    path = "/automation/rules/{id}",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    responses(
        (status = 200, description = "The rule", body = Rule),
        (status = 404, description = "No such rule", body = Problem),
    )
)]
async fn get_rule(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Rule>> {
    Ok(Json(
        store::find(&state.db, &id)
            .await?
            .ok_or(AppError::NotFound)?,
    ))
}

/// Replaces a rule's definition. Its owner is unchanged.
#[utoipa::path(
    put,
    path = "/automation/rules/{id}",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    request_body = RuleRequest,
    responses(
        (status = 200, description = "Saved", body = Rule),
        (status = 404, description = "No such rule", body = Problem),
        (status = 422, description = "An invalid rule", body = Problem),
    )
)]
async fn replace_rule(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<RuleRequest>,
) -> AppResult<Json<Rule>> {
    let (name, description) = check(&body)?;
    store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    store::update(
        &mut tx,
        &id,
        &RuleInput {
            name: &name,
            description: description.as_deref(),
            enabled: body.enabled,
            spec: &body.spec,
        },
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        store::find(&state.db, &id)
            .await?
            .ok_or(AppError::NotFound)?,
    ))
}

/// Deletes a rule and its audit log.
#[utoipa::path(
    delete,
    path = "/automation/rules/{id}",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such rule", body = Problem),
    )
)]
async fn delete_rule(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    store::delete(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A rule's audit log: every run, newest first, with its smart-value resolution trace.
#[utoipa::path(
    get,
    path = "/automation/rules/{id}/runs",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    responses(
        (status = 200, description = "Runs, newest first", body = Vec<Run>),
        (status = 404, description = "No such rule", body = Problem),
    )
)]
async fn list_runs(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<Run>>> {
    store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(store::runs(&state.db, &id, 100).await?))
}

/// Runs a rule now, against a card. Any rule may be run by hand; a `manual` rule *only* runs
/// this way.
#[utoipa::path(
    post,
    path = "/automation/rules/{id}/run",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    request_body = RunRequest,
    responses(
        (status = 200, description = "How it went", body = RunResult),
        (status = 404, description = "No such rule or card", body = Problem),
    )
)]
async fn run_rule(
    State(state): State<AppState>,
    member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<RunRequest>,
) -> AppResult<Json<RunResult>> {
    let rule = store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let target = match &body.card_key {
        Some(key) => {
            let found = card::find_by_key(&state.db, &key.to_ascii_uppercase())
                .await?
                .filter(|c| !c.is_deleted())
                .ok_or(AppError::NotFound)?;
            if found.project_id != rule.project_id {
                return Err(AppError::Validation(
                    "A rule only runs on its own project's cards.".to_owned(),
                ));
            }
            Some(found)
        }
        None => None,
    };
    let result = engine::execute(
        &state,
        &rule,
        Invocation {
            card: target,
            initiator: Some((*member.0.user).clone()),
            ..Invocation::bare("manual")
        },
    )
    .await?;
    Ok(Json(result))
}

/// Issues (or reissues) the rule's incoming-webhook token. The old one stops working.
#[utoipa::path(
    post,
    path = "/automation/rules/{id}/webhook-token",
    tag = "automation",
    params(("id" = String, Path, description = "The rule")),
    responses(
        (status = 200, description = "The token, shown once", body = WebhookToken),
        (status = 404, description = "No such rule", body = Problem),
        (status = 422, description = "Not a webhook-triggered rule", body = Problem),
    )
)]
async fn issue_token(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<Json<WebhookToken>> {
    let rule = store::find(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(rule.spec.trigger, Trigger::IncomingWebhook { .. }) {
        return Err(AppError::Validation(
            "Only a rule triggered by an incoming webhook has a token.".to_owned(),
        ));
    }
    let token = store::issue_webhook_token(&state.db, &id).await?;
    Ok(Json(WebhookToken {
        path: format!("/hooks/automation/{token}"),
        token,
    }))
}

/// Evaluates an (unsaved) rule against a card without changing anything.
#[utoipa::path(
    post,
    path = "/projects/{key}/automation/dry-run",
    tag = "automation",
    params(("key" = String, Path, description = "The project key")),
    request_body = DryRunRequest,
    responses(
        (status = 200, description = "What would happen, with every smart value resolved", body = RunResult),
        (status = 422, description = "An invalid rule", body = Problem),
    )
)]
async fn dry_run(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<DryRunRequest>,
) -> AppResult<Json<RunResult>> {
    body.spec.validate()?;
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let target = match &body.card_key {
        Some(key) => Some(
            card::find_by_key(&state.db, &key.to_ascii_uppercase())
                .await?
                .filter(|c| c.project_id == project.id)
                .ok_or(AppError::NotFound)?,
        ),
        None => None,
    };
    Ok(Json(
        engine::dry_run(&state, &project.id, member.0.id(), &body.spec, target).await?,
    ))
}

/// Receives an incoming webhook for a rule. Public: the token in the path is the secret.
#[utoipa::path(
    post,
    path = "/hooks/automation/{token}",
    tag = "automation",
    params(("token" = String, Path, description = "The rule's webhook token")),
    request_body(content = Object, description = "Any JSON; available as {{webhook.body}}"),
    responses(
        (status = 202, description = "Accepted and run"),
        (status = 404, description = "No rule has this token", body = Problem),
    )
)]
async fn receive_hook(
    State(state): State<AppState>,
    Path(token): Path<String>,
    body: Bytes,
) -> AppResult<StatusCode> {
    // Tokens are 64 hex characters; anything else cannot match, and is refused before a query.
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::NotFound);
    }
    let rule = store::find_by_webhook_token(&state.db, &token)
        .await?
        .filter(|r| r.enabled)
        .ok_or(AppError::NotFound)?;
    let payload: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| AppError::BadRequest("The body must be JSON.".to_owned()))?
    };
    engine::run_webhook(&state, &rule, payload).await?;
    Ok(StatusCode::ACCEPTED)
}

/// The gated automation routes, under `/api/v1`.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_rules, create_rule))
        .routes(routes!(get_rule, replace_rule, delete_rule))
        .routes(routes!(list_runs))
        .routes(routes!(run_rule))
        .routes(routes!(issue_token))
        .routes(routes!(dry_run))
}

/// The public webhook receiver, mounted at the top level beside the GitHub one.
pub fn hook_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(receive_hook))
}

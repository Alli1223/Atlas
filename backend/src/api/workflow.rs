//! `/api/v1/…/workflows`, `/transitions`, and a card's available transitions.
//!
//! The read surface hands the board and the workflow editor everything they need
//! to render a legal move: which transitions a card may take right now
//! (conditions already evaluated), and what each transition's validators and
//! post-functions are. The write surface is the transition editor. The one
//! behavioural endpoint is [`execute_card_transition`] — the "take this
//! transition" button — which runs the full execution contract in one write
//! transaction. See [`crate::domain::workflow`].

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::serde_ext::double_option;
use crate::api::{AppState, projects};
use crate::auth::CurrentUser;
use crate::auth::extract::RequireMember;
use crate::domain::card::{self, CardDto, CardPatch};
use crate::domain::workflow::{self, AvailableTransition, GateRow, Transition, Workflow};
use crate::domain::{config, project};
use crate::error::{AppError, AppResult, Problem};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// A workflow, with the ids of the statuses it includes.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDto {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// The human name.
    pub name: String,
    /// Whether this is the project's permissive default.
    pub is_default: bool,
    /// The statuses this workflow includes.
    pub status_ids: Vec<String>,
    /// The card types routed through this workflow.
    pub card_type_ids: Vec<String>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl WorkflowDto {
    fn build(workflow: Workflow, status_ids: Vec<String>, card_type_ids: Vec<String>) -> Self {
        Self {
            id: workflow.id,
            project_id: workflow.project_id,
            name: workflow.name,
            is_default: workflow.is_default,
            status_ids,
            card_type_ids,
            created_at: workflow.created_at,
            updated_at: workflow.updated_at,
        }
    }
}

/// Loads a workflow's status and card-type ids and builds its DTO.
async fn workflow_dto(db: &crate::db::Db, workflow: Workflow) -> AppResult<WorkflowDto> {
    let status_ids = workflow::status_ids(db, &workflow.id).await?;
    let card_type_ids = workflow::card_type_ids(db, &workflow.id).await?;
    Ok(WorkflowDto::build(workflow, status_ids, card_type_ids))
}

/// One condition, validator, or post-function.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateDto {
    /// UUID v7, as text.
    pub id: String,
    /// Which rule: `OnlyAssignee`, `RequiredField`, `SetResolution`, …
    pub kind: String,
    /// The rule's configuration.
    #[schema(value_type = Object)]
    pub config: Value,
}

impl From<GateRow> for GateDto {
    fn from(row: GateRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            // Stored as text we serialised ourselves, so it re-parses; a null
            // fallback keeps a corrupt row from panicking the whole list.
            config: serde_json::from_str(&row.config).unwrap_or(Value::Null),
        }
    }
}

/// A transition and its three gates.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransitionDto {
    /// UUID v7, as text.
    pub id: String,
    /// The owning workflow.
    pub workflow_id: String,
    /// The button label.
    pub name: String,
    /// The source status, or `null` for a global ("from any status") transition.
    pub from_status_id: Option<String>,
    /// Where the card lands.
    pub to_status_id: String,
    /// Evaluation and display order.
    pub position: i64,
    /// Conditions that decide whether the transition is offered.
    pub conditions: Vec<GateDto>,
    /// Validators that decide whether an offered transition may be taken.
    pub validators: Vec<GateDto>,
    /// Post-functions that run after the status change commits.
    pub post_functions: Vec<GateDto>,
}

/// The body of a condition/validator/post-function on a transition.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateInput {
    /// Which rule.
    pub kind: String,
    /// The rule's configuration. Defaults to an empty object.
    #[serde(default = "empty_object")]
    #[schema(value_type = Object)]
    pub config: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// The body of `POST /projects/{key}/workflows`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkflowRequest {
    /// The human name, unique within the project.
    pub name: String,
    /// The statuses this workflow includes. Each must belong to the project.
    #[serde(default)]
    pub status_ids: Vec<String>,
    /// The card types to route through this workflow. Each must belong to the
    /// project. This is how a custom workflow starts enforcing — until a card
    /// type points at it, cards still move under the permissive default.
    #[serde(default)]
    pub card_type_ids: Vec<String>,
}

/// The body of `PATCH /workflows/{id}`.
///
/// At least one field must be present.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWorkflowRequest {
    /// The new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Reassign these card types to this workflow.
    #[serde(default)]
    pub card_type_ids: Option<Vec<String>>,
}

/// The body of `POST /workflows/{id}/transitions`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTransitionRequest {
    /// The button label.
    pub name: String,
    /// The source status, or omit/`null` for a global transition.
    #[serde(default)]
    pub from_status_id: Option<String>,
    /// Where the card lands.
    pub to_status_id: String,
    /// Evaluation and display order.
    #[serde(default)]
    pub position: Option<i64>,
    /// Conditions to attach.
    #[serde(default)]
    pub conditions: Vec<GateInput>,
    /// Validators to attach.
    #[serde(default)]
    pub validators: Vec<GateInput>,
    /// Post-functions to attach, in the order they should run.
    #[serde(default)]
    pub post_functions: Vec<GateInput>,
}

/// The body of `PATCH /transitions/{id}`.
///
/// A gate array that is present **replaces** that transition's whole set; an
/// absent one leaves it alone.
#[allow(clippy::option_option)]
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTransitionRequest {
    /// The button label.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` makes it global, a value anchors it.
    #[serde(default, deserialize_with = "double_option")]
    pub from_status_id: Option<Option<String>>,
    /// Where the card lands.
    #[serde(default)]
    pub to_status_id: Option<String>,
    /// Evaluation and display order.
    #[serde(default)]
    pub position: Option<i64>,
    /// Replaces the conditions, if present.
    #[serde(default)]
    pub conditions: Option<Vec<GateInput>>,
    /// Replaces the validators, if present.
    #[serde(default)]
    pub validators: Option<Vec<GateInput>>,
    /// Replaces the post-functions, if present.
    #[serde(default)]
    pub post_functions: Option<Vec<GateInput>>,
}

/// The body of `POST /cards/{key}/transitions/{id}` — the "take this transition"
/// button, including anything a transition screen asked the user to fill in.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecuteTransitionRequest {
    /// A comment entered on the transition screen, added as the first
    /// post-function.
    #[serde(default)]
    pub comment: Option<String>,
    /// A resolution chosen on the screen. Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub resolution_id: Option<Option<String>>,
    /// An assignee chosen on the screen. Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub assignee_id: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// Workflows
// ---------------------------------------------------------------------------

/// A project's workflows.
#[utoipa::path(
    get,
    path = "/projects/{key}/workflows",
    tag = "workflows",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The project's workflows", body = Vec<WorkflowDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn list_workflows(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<WorkflowDto>>> {
    let project = projects::by_key(&state.db, &key).await?;
    let workflows = workflow::list(&state.db, &project.id).await?;

    let mut out = Vec::with_capacity(workflows.len());
    for wf in workflows {
        out.push(workflow_dto(&state.db, wf).await?);
    }
    Ok(Json(out))
}

/// Creates a workflow.
#[utoipa::path(
    post,
    path = "/projects/{key}/workflows",
    tag = "workflows",
    params(("key" = String, Path, description = "The project key")),
    request_body = CreateWorkflowRequest,
    responses(
        (status = 201, description = "Created", body = WorkflowDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers and members cannot change workflows", body = Problem),
        (status = 404, description = "No such project", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "The name or a status is invalid", body = Problem),
    )
)]
async fn create_workflow(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<CreateWorkflowRequest>,
) -> AppResult<(StatusCode, Json<WorkflowDto>)> {
    let name = config::validate_name(&body.name)?;
    let now = crate::auth::now();
    let mut tx = state.db.begin_write().await?;

    let project = project::find_by_key_tx(&mut tx, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    for status_id in &body.status_ids {
        config::find_status_tx(&mut tx, &project.id, status_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!("{status_id:?} is not a status of this project."))
            })?;
    }

    let created =
        workflow::insert(&mut tx, &project.id, &name, false, &body.status_ids, now).await?;
    workflow::assign_card_types(&mut tx, &created.id, &project.id, &body.card_type_ids).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(workflow_dto(&state.db, created).await?)))
}

/// One workflow.
#[utoipa::path(
    get,
    path = "/workflows/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The workflow id")),
    responses(
        (status = 200, description = "The workflow", body = WorkflowDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such workflow", body = Problem),
    )
)]
async fn get_workflow(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<WorkflowDto>> {
    let wf = workflow::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(workflow_dto(&state.db, wf).await?))
}

/// Renames a workflow.
#[utoipa::path(
    patch,
    path = "/workflows/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The workflow id")),
    request_body = UpdateWorkflowRequest,
    responses(
        (status = 200, description = "Renamed", body = WorkflowDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Members cannot change workflows", body = Problem),
        (status = 404, description = "No such workflow", body = Problem),
        (status = 422, description = "The name is invalid", body = Problem),
    )
)]
async fn update_workflow(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateWorkflowRequest>,
) -> AppResult<Json<WorkflowDto>> {
    if body.name.is_none() && body.card_type_ids.is_none() {
        return Err(AppError::Validation(
            "The request changed nothing. Send a name or cardTypeIds.".to_owned(),
        ));
    }
    let name = body.name.as_deref().map(config::validate_name).transpose()?;
    let now = crate::auth::now();

    let mut tx = state.db.begin_write().await?;
    let wf = workflow::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(name) = &name {
        workflow::rename(&mut tx, &id, name, now).await?;
    }
    if let Some(card_type_ids) = &body.card_type_ids {
        workflow::assign_card_types(&mut tx, &id, &wf.project_id, card_type_ids).await?;
    }
    tx.commit().await?;

    let refreshed = workflow::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(workflow_dto(&state.db, refreshed).await?))
}

/// Deletes a workflow. The default one cannot be deleted.
#[utoipa::path(
    delete,
    path = "/workflows/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The workflow id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Members cannot change workflows", body = Problem),
        (status = 404, description = "No such workflow", body = Problem),
        (status = 409, description = "The default workflow cannot be deleted", body = Problem),
    )
)]
async fn delete_workflow(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    let wf = workflow::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    workflow::delete(&mut tx, &wf).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

/// A workflow's transitions.
#[utoipa::path(
    get,
    path = "/workflows/{id}/transitions",
    tag = "workflows",
    params(("id" = String, Path, description = "The workflow id")),
    responses(
        (status = 200, description = "The workflow's transitions and their gates", body = Vec<TransitionDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such workflow", body = Problem),
    )
)]
async fn list_transitions(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<TransitionDto>>> {
    workflow::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    let transitions = workflow::transitions(&state.db, &id).await?;
    let mut out = Vec::with_capacity(transitions.len());
    for t in transitions {
        out.push(transition_dto(&state.db, t).await?);
    }
    Ok(Json(out))
}

/// Adds a transition to a workflow.
#[utoipa::path(
    post,
    path = "/workflows/{id}/transitions",
    tag = "workflows",
    params(("id" = String, Path, description = "The workflow id")),
    request_body = CreateTransitionRequest,
    responses(
        (status = 201, description = "Created", body = TransitionDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Members cannot change workflows", body = Problem),
        (status = 404, description = "No such workflow", body = Problem),
        (status = 422, description = "A status or a gate is invalid", body = Problem),
    )
)]
async fn create_transition(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<CreateTransitionRequest>,
) -> AppResult<(StatusCode, Json<TransitionDto>)> {
    let name = config::validate_name(&body.name)?;

    let mut tx = state.db.begin_write().await?;
    let wf = workflow::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    validate_transition_statuses(
        &mut tx,
        &wf.project_id,
        body.from_status_id.as_deref(),
        Some(&body.to_status_id),
    )
    .await?;

    let transition = workflow::insert_transition(
        &mut tx,
        &wf.id,
        &name,
        body.from_status_id.as_deref(),
        &body.to_status_id,
        body.position.unwrap_or(0),
    )
    .await?;

    add_gates(
        &mut tx,
        &transition.id,
        &body.conditions,
        &body.validators,
        &body.post_functions,
    )
    .await?;

    tx.commit().await?;

    let dto = transition_dto(&state.db, transition).await?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// Edits a transition and, where a gate array is present, replaces that set.
#[utoipa::path(
    patch,
    path = "/transitions/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The transition id")),
    request_body = UpdateTransitionRequest,
    responses(
        (status = 200, description = "Updated", body = TransitionDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Members cannot change workflows", body = Problem),
        (status = 404, description = "No such transition", body = Problem),
        (status = 422, description = "A status or a gate is invalid", body = Problem),
    )
)]
async fn update_transition(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
    Json(body): Json<UpdateTransitionRequest>,
) -> AppResult<Json<TransitionDto>> {
    let name = body.name.as_deref().map(config::validate_name).transpose()?;

    let mut tx = state.db.begin_write().await?;
    let existing = workflow::transition_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let wf = workflow::find_by_id_tx(&mut tx, &existing.workflow_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Validate any status the patch introduces.
    let from = body.from_status_id.as_ref().and_then(Option::as_deref);
    validate_transition_statuses(&mut tx, &wf.project_id, from, body.to_status_id.as_deref())
        .await?;

    let updated = workflow::update_transition(
        &mut tx,
        &id,
        name.as_deref(),
        body.from_status_id
            .as_ref()
            .map(|outer| outer.as_deref()),
        body.to_status_id.as_deref(),
        body.position,
    )
    .await?;

    if let Some(conditions) = &body.conditions {
        workflow::clear_conditions(&mut tx, &id).await?;
        for gate in conditions {
            workflow::add_condition(&mut tx, &id, &gate.kind, &gate_config(gate)?).await?;
        }
    }
    if let Some(validators) = &body.validators {
        workflow::clear_validators(&mut tx, &id).await?;
        for gate in validators {
            workflow::add_validator(&mut tx, &id, &gate.kind, &gate_config(gate)?).await?;
        }
    }
    if let Some(post_functions) = &body.post_functions {
        workflow::clear_post_functions(&mut tx, &id).await?;
        for (position, gate) in post_functions.iter().enumerate() {
            workflow::add_post_function(
                &mut tx,
                &id,
                &gate.kind,
                &gate_config(gate)?,
                i64::try_from(position).unwrap_or(0),
            )
            .await?;
        }
    }

    tx.commit().await?;

    let _ = updated;
    let refreshed = workflow::transition_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(transition_dto(&state.db, refreshed).await?))
}

/// Deletes a transition.
#[utoipa::path(
    delete,
    path = "/transitions/{id}",
    tag = "workflows",
    params(("id" = String, Path, description = "The transition id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Members cannot change workflows", body = Problem),
        (status = 404, description = "No such transition", body = Problem),
    )
)]
async fn delete_transition(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    workflow::transition_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    workflow::delete_transition(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// A card's transitions
// ---------------------------------------------------------------------------

/// The transitions a card may take **right now** — conditions evaluated, so the
/// board only ever offers a legal move.
#[utoipa::path(
    get,
    path = "/cards/{key}/transitions",
    tag = "workflows",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "The available transitions", body = Vec<AvailableTransition>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn card_transitions(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<AvailableTransition>>> {
    let card = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(
        workflow::available_transitions(&state.db, &card, Some(current.id())).await?,
    ))
}

/// Takes a named transition on a card: validators, then the status change, then
/// post-functions, all in one write transaction.
#[utoipa::path(
    post,
    path = "/cards/{key}/transitions/{id}",
    tag = "workflows",
    params(
        ("key" = String, Path, description = "The card key"),
        ("id" = String, Path, description = "The transition id"),
    ),
    request_body = ExecuteTransitionRequest,
    responses(
        (status = 200, description = "Moved, with post-functions applied", body = CardDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Viewers cannot move cards", body = Problem),
        (status = 404, description = "No such card or transition", body = Problem),
        (status = 409, description = "The transition is hidden or not available here", body = Problem),
        (status = 422, description = "A validator failed", body = Problem),
    )
)]
async fn execute_card_transition(
    State(state): State<AppState>,
    member: RequireMember,
    Path((key, transition_id)): Path<(String, String)>,
    Json(body): Json<ExecuteTransitionRequest>,
) -> AppResult<Json<CardDto>> {
    let comment = body
        .comment
        .as_deref()
        .map(crate::domain::comment::validate_body)
        .transpose()?;

    let now = crate::auth::now();
    let mut tx = state.db.begin_write().await?;

    let card = card::find_by_key_tx(&mut tx, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;

    let transition = workflow::transition_by_id_tx(&mut tx, &transition_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let patch = CardPatch {
        resolution_id: body.resolution_id,
        assignee_id: body.assignee_id,
        ..CardPatch::default()
    };

    let moved = card::execute_transition(
        &mut tx,
        &card,
        &transition,
        patch,
        comment.as_deref(),
        Some(member.0.id()),
        now,
    )
    .await?;

    tx.commit().await?;

    Ok(Json(CardDto::from(&moved)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialises a gate's JSON config to the text the domain stores and parses.
fn gate_config(gate: &GateInput) -> AppResult<String> {
    serde_json::to_string(&gate.config).map_err(AppError::internal)
}

/// Checks that any status a transition names belongs to the workflow's project.
async fn validate_transition_statuses(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    from_status_id: Option<&str>,
    to_status_id: Option<&str>,
) -> AppResult<()> {
    for status_id in [from_status_id, to_status_id].into_iter().flatten() {
        config::find_status_tx(&mut *tx, project_id, status_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!("{status_id:?} is not a status of this project."))
            })?;
    }
    Ok(())
}

/// Inserts a transition's three gate lists, validating each as it goes.
async fn add_gates(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
    conditions: &[GateInput],
    validators: &[GateInput],
    post_functions: &[GateInput],
) -> AppResult<()> {
    for gate in conditions {
        workflow::add_condition(&mut *tx, transition_id, &gate.kind, &gate_config(gate)?).await?;
    }
    for gate in validators {
        workflow::add_validator(&mut *tx, transition_id, &gate.kind, &gate_config(gate)?).await?;
    }
    for (position, gate) in post_functions.iter().enumerate() {
        workflow::add_post_function(
            &mut *tx,
            transition_id,
            &gate.kind,
            &gate_config(gate)?,
            i64::try_from(position).unwrap_or(0),
        )
        .await?;
    }
    Ok(())
}

/// Loads a transition's gates and assembles its DTO.
async fn transition_dto(db: &crate::db::Db, transition: Transition) -> AppResult<TransitionDto> {
    let conditions = workflow::conditions_of(db, &transition.id)
        .await?
        .into_iter()
        .map(GateDto::from)
        .collect();
    let validators = workflow::validators_of(db, &transition.id)
        .await?
        .into_iter()
        .map(GateDto::from)
        .collect();
    let post_functions = workflow::post_functions_of(db, &transition.id)
        .await?
        .into_iter()
        .map(GateDto::from)
        .collect();

    Ok(TransitionDto {
        id: transition.id,
        workflow_id: transition.workflow_id,
        name: transition.name,
        from_status_id: transition.from_status_id,
        to_status_id: transition.to_status_id,
        position: transition.position,
        conditions,
        validators,
        post_functions,
    })
}

/// The workflow routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_workflows, create_workflow))
        .routes(routes!(get_workflow, update_workflow, delete_workflow))
        .routes(routes!(list_transitions, create_transition))
        .routes(routes!(update_transition, delete_transition))
        .routes(routes!(card_transitions))
        .routes(routes!(execute_card_transition))
}

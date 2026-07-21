//! The workflow engine: statuses, transitions, and the three gates that decide
//! whether a card may move.
//!
//! # The execution contract (jira-features.md §4), copied exactly
//!
//! A transition carries three ordered gates, and the difference between the
//! first two is the whole point — it is why Jira's transition UI never offers a
//! button you cannot press:
//!
//! 1. **Conditions** decide whether the transition is *offered at all*. A failing
//!    condition **hides** the transition: it is absent from
//!    [`available_transitions`], and an attempt to take it directly is rejected
//!    as if the edge did not exist. "Only the assignee may resolve this" is a
//!    condition, because a button you are not allowed to press should not be
//!    shown.
//! 2. **Validators** decide whether an *offered* transition may be *taken now*. A
//!    failing validator **rejects** the attempt with a message; the status does
//!    not change and no post-function runs. "You must pick a resolution" is a
//!    validator, because the button is legitimately there — you just have not
//!    filled the form in.
//! 3. **Post-functions** run *after* the status change commits, in the same write
//!    transaction, in a fixed order. If one fails the whole transition rolls
//!    back, because a half-applied transition is a corrupt card.
//!
//! # The default workflow is permissive
//!
//! Every project has a workflow flagged `is_default`. A default (or a card type
//! with no workflow at all) permits moving between any two of the project's
//! statuses — it is what keeps every card that moves today moving. Only a
//! *custom* workflow enforces its edges. The permissiveness lives here rather
//! than in a wall of seeded edges, so a status added later is reachable with no
//! extra rows. See [`resolve_transition`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::card::Card;
use crate::domain::member::{self, ProjectRole};
use crate::domain::{comment, config, project};
use crate::error::{AppError, AppResult};

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// A row of `workflows`.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// The human name, unique within the project.
    pub name: String,
    /// Whether this is the project's permissive default. A default workflow
    /// allows a move between any two of its statuses; a custom one enforces its
    /// edges. See [`resolve_transition`].
    pub is_default: bool,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// A row of `transitions`.
#[derive(Debug, Clone, FromRow)]
pub struct Transition {
    /// UUID v7, as text.
    pub id: String,
    /// The owning workflow.
    pub workflow_id: String,
    /// The human name shown on the button: `Start Progress`, `Resolve`.
    pub name: String,
    /// The source status. `None` = a global transition, offered from anywhere.
    pub from_status_id: Option<String>,
    /// Where the card lands.
    pub to_status_id: String,
    /// Evaluation and display order.
    pub position: i64,
}

/// A stored gate row — a condition, a validator, or a post-function.
///
/// One row type for all three, because they have the same shape: a `kind`
/// naming which rule, and a JSON `config` parsed per kind. The differences are
/// in *when* they run and *what* they may do, which is [`Gate`]'s job, not the
/// row's.
#[derive(Debug, Clone, FromRow)]
pub struct GateRow {
    /// UUID v7, as text.
    pub id: String,
    /// The owning transition.
    pub transition_id: String,
    /// Which rule.
    pub kind: String,
    /// The rule's JSON configuration, as stored text.
    pub config: String,
    /// Post-function order; `0` for conditions and validators, which are
    /// unordered.
    pub position: i64,
}

// ---------------------------------------------------------------------------
// Parsed gates
// ---------------------------------------------------------------------------

/// A condition, parsed from its `(kind, config)`.
///
/// Conditions **hide** a transition when they fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Only the card's current assignee may take the transition.
    OnlyAssignee,
    /// Only the card's reporter may take the transition.
    OnlyReporter,
    /// The actor must hold at least this project role.
    UserInRole(ProjectRole),
    /// The card may not **enter** a done status while any child is not done.
    ChildBlocking,
}

/// A validator, parsed from its `(kind, config)`.
///
/// Validators **reject** an attempt when they fail; the status does not change
/// and no post-function runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validator {
    /// A named field must be non-empty on the card the transition would produce.
    /// `resolution` is the load-bearing case — the "resolution + comment on Done"
    /// screen — but the mechanism is general.
    RequiredField(FieldName),
}

/// A post-function, parsed from its `(kind, config)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostFunction {
    /// Set (or clear) the resolution. Feeds into the existing resolution rules in
    /// [`crate::domain::card`] rather than duplicating them.
    SetResolution(Option<String>),
    /// Assign the card.
    AssignTo(AssignTarget),
    /// Add a fixed comment.
    AddComment(String),
    /// Record an event for Phase 15 automation to consume.
    FireEvent(String),
    /// Set (or clear) a single field.
    UpdateField {
        field: FieldName,
        value: Option<String>,
    },
}

/// Who a card is assigned to by an [`PostFunction::AssignTo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignTarget {
    /// The actor taking the transition.
    CurrentUser,
    /// The card's reporter.
    Reporter,
    /// The project lead.
    Lead,
    /// Nobody.
    Unassign,
}

/// A card field a validator can require or a post-function can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldName {
    /// The priority.
    Priority,
    /// The assignee.
    Assignee,
    /// The reporter.
    Reporter,
    /// The resolution.
    Resolution,
    /// The due date.
    DueDate,
    /// The start date.
    StartDate,
    /// The estimate.
    Estimate,
    /// The description.
    Description,
}

impl FieldName {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "priority" => Self::Priority,
            "assignee" => Self::Assignee,
            "reporter" => Self::Reporter,
            "resolution" => Self::Resolution,
            "dueDate" => Self::DueDate,
            "startDate" => Self::StartDate,
            "estimate" => Self::Estimate,
            "description" => Self::Description,
            _ => return None,
        })
    }

    /// The field's human name, for a validator's rejection message.
    fn label(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Assignee => "assignee",
            Self::Reporter => "reporter",
            Self::Resolution => "resolution",
            Self::DueDate => "due date",
            Self::StartDate => "start date",
            Self::Estimate => "estimate",
            Self::Description => "description",
        }
    }

    /// Whether the field is empty on this card.
    fn is_empty_on(self, card: &Card) -> bool {
        match self {
            Self::Priority => card.priority_id.is_none(),
            Self::Assignee => card.assignee_id.is_none(),
            Self::Reporter => card.reporter_id.is_none(),
            Self::Resolution => card.resolution_id.is_none(),
            Self::DueDate => card.due_date.is_none(),
            Self::StartDate => card.start_date.is_none(),
            Self::Estimate => card.estimate.is_none(),
            Self::Description => card.description.as_deref().unwrap_or("").trim().is_empty(),
        }
    }
}

/// A field a validator can require but a post-function may not set, or vice
/// versa. Named here so a bad `field` is a 422 at CRUD time, not a surprise.
fn field_config(config: &str) -> AppResult<FieldName> {
    #[derive(Deserialize)]
    struct FieldConfig {
        field: String,
    }
    let parsed: FieldConfig = serde_json::from_str(config)
        .map_err(|_| AppError::Validation("Expected a JSON object with a \"field\".".to_owned()))?;
    FieldName::from_str(&parsed.field)
        .ok_or_else(|| AppError::Validation(format!("{:?} is not a settable field.", parsed.field)))
}

/// Parses a condition row into a [`Condition`], failing on an unknown kind or a
/// malformed config.
pub fn parse_condition(kind: &str, config: &str) -> AppResult<Condition> {
    Ok(match kind {
        "OnlyAssignee" => Condition::OnlyAssignee,
        "OnlyReporter" => Condition::OnlyReporter,
        "ChildBlocking" => Condition::ChildBlocking,
        "UserInRole" => {
            #[derive(Deserialize)]
            struct RoleConfig {
                role: ProjectRole,
            }
            let parsed: RoleConfig = serde_json::from_str(config).map_err(|_| {
                AppError::Validation(
                    "UserInRole needs a \"role\" of owner, member or viewer.".to_owned(),
                )
            })?;
            Condition::UserInRole(parsed.role)
        }
        other => {
            return Err(AppError::Validation(format!(
                "{other:?} is not a condition kind. Known kinds: OnlyAssignee, OnlyReporter, \
                 UserInRole, ChildBlocking."
            )));
        }
    })
}

/// Parses a validator row into a [`Validator`].
pub fn parse_validator(kind: &str, config: &str) -> AppResult<Validator> {
    match kind {
        "RequiredField" => Ok(Validator::RequiredField(field_config(config)?)),
        other => Err(AppError::Validation(format!(
            "{other:?} is not a validator kind. Known kinds: RequiredField."
        ))),
    }
}

/// Parses a post-function row into a [`PostFunction`].
pub fn parse_post_function(kind: &str, config: &str) -> AppResult<PostFunction> {
    Ok(match kind {
        "SetResolution" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ResolutionConfig {
                #[serde(default)]
                resolution_id: Option<String>,
            }
            let parsed: ResolutionConfig = serde_json::from_str(config).map_err(|_| {
                AppError::Validation(
                    "SetResolution needs a \"resolutionId\", or none to clear it.".to_owned(),
                )
            })?;
            PostFunction::SetResolution(parsed.resolution_id)
        }
        "AssignTo" => {
            #[derive(Deserialize)]
            struct AssignConfig {
                target: String,
            }
            let parsed: AssignConfig = serde_json::from_str(config).map_err(|_| {
                AppError::Validation(
                    "AssignTo needs a \"target\" of currentUser, reporter, lead or unassign."
                        .to_owned(),
                )
            })?;
            let target = match parsed.target.as_str() {
                "currentUser" => AssignTarget::CurrentUser,
                "reporter" => AssignTarget::Reporter,
                "lead" => AssignTarget::Lead,
                "unassign" => AssignTarget::Unassign,
                other => {
                    return Err(AppError::Validation(format!(
                        "{other:?} is not an AssignTo target."
                    )));
                }
            };
            PostFunction::AssignTo(target)
        }
        "AddComment" => {
            #[derive(Deserialize)]
            struct CommentConfig {
                body: String,
            }
            let parsed: CommentConfig = serde_json::from_str(config)
                .map_err(|_| AppError::Validation("AddComment needs a \"body\".".to_owned()))?;
            let body = comment::validate_body(&parsed.body)?;
            PostFunction::AddComment(body)
        }
        "FireEvent" => {
            #[derive(Deserialize)]
            struct EventConfig {
                event: String,
            }
            let parsed: EventConfig = serde_json::from_str(config)
                .map_err(|_| AppError::Validation("FireEvent needs an \"event\".".to_owned()))?;
            if parsed.event.trim().is_empty() {
                return Err(AppError::Validation(
                    "FireEvent \"event\" is empty.".to_owned(),
                ));
            }
            PostFunction::FireEvent(parsed.event)
        }
        "UpdateField" => {
            #[derive(Deserialize)]
            struct UpdateConfig {
                field: String,
                #[serde(default)]
                value: Option<String>,
            }
            let parsed: UpdateConfig = serde_json::from_str(config).map_err(|_| {
                AppError::Validation("UpdateField needs a \"field\" and a \"value\".".to_owned())
            })?;
            let field = FieldName::from_str(&parsed.field).ok_or_else(|| {
                AppError::Validation(format!("{:?} is not settable.", parsed.field))
            })?;
            PostFunction::UpdateField {
                field,
                value: parsed.value,
            }
        }
        other => {
            return Err(AppError::Validation(format!(
                "{other:?} is not a post-function kind. Known kinds: SetResolution, AssignTo, \
                 AddComment, FireEvent, UpdateField."
            )));
        }
    })
}

// ---------------------------------------------------------------------------
// Workflow CRUD
// ---------------------------------------------------------------------------

/// Every workflow of a project.
pub async fn list(db: &Db, project_id: &str) -> AppResult<Vec<Workflow>> {
    Ok(sqlx::query_as::<_, Workflow>(
        "SELECT id, project_id, name, is_default, created_at, updated_at FROM workflows \
         WHERE project_id = ? ORDER BY is_default DESC, name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// A workflow by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Workflow>> {
    Ok(sqlx::query_as::<_, Workflow>(
        "SELECT id, project_id, name, is_default, created_at, updated_at FROM workflows \
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// A workflow by id, inside a transaction.
pub async fn find_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Workflow>> {
    Ok(sqlx::query_as::<_, Workflow>(
        "SELECT id, project_id, name, is_default, created_at, updated_at FROM workflows \
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// The workflow a card type routes through, or `None` if it has none (which is
/// treated as a permissive default — see [`resolve_transition`]).
pub async fn find_for_card_type_tx(
    tx: &mut sqlx::SqliteConnection,
    type_id: &str,
) -> AppResult<Option<Workflow>> {
    Ok(sqlx::query_as::<_, Workflow>(
        "SELECT w.id, w.project_id, w.name, w.is_default, w.created_at, w.updated_at \
         FROM workflows w JOIN card_types ct ON ct.workflow_id = w.id \
         WHERE ct.id = ?",
    )
    .bind(type_id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a workflow and its status set. Does **not** commit.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    is_default: bool,
    status_ids: &[String],
    now: DateTime<Utc>,
) -> AppResult<Workflow> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO workflows (id, project_id, name, is_default, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(is_default)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    for status_id in status_ids {
        add_status(&mut *tx, &id, status_id).await?;
    }

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the workflow just inserted is missing")))
}

/// Adds a status to a workflow's set, ignoring a duplicate.
pub async fn add_status(
    tx: &mut sqlx::SqliteConnection,
    workflow_id: &str,
    status_id: &str,
) -> AppResult<()> {
    sqlx::query("INSERT OR IGNORE INTO workflow_statuses (workflow_id, status_id) VALUES (?, ?)")
        .bind(workflow_id)
        .bind(status_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The status ids a workflow includes.
pub async fn status_ids(db: &Db, workflow_id: &str) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT status_id FROM workflow_statuses WHERE workflow_id = ?")
            .bind(workflow_id)
            .fetch_all(db.reader())
            .await?,
    )
}

/// Renames a workflow.
pub async fn rename(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: &str,
    now: DateTime<Utc>,
) -> AppResult<Workflow> {
    sqlx::query("UPDATE workflows SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(to_sql_timestamp(now))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    find_by_id_tx(&mut *tx, id).await?.ok_or(AppError::NotFound)
}

/// Deletes a workflow. Refuses to delete the default one — a project with no
/// default has nothing to fall back to for its permissive moves.
pub async fn delete(tx: &mut sqlx::SqliteConnection, workflow: &Workflow) -> AppResult<()> {
    if workflow.is_default {
        return Err(AppError::Conflict(
            "The default workflow cannot be deleted. Make another workflow the default first."
                .to_owned(),
        ));
    }
    sqlx::query("DELETE FROM workflows WHERE id = ?")
        .bind(&workflow.id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Assigns a workflow to a set of the project's card types, routing every card
/// of those types through it. Each type is checked to belong to the project
/// first — the FK would accept another project's type, but a card type of one
/// project on another's workflow is nonsense the API must refuse.
pub async fn assign_card_types(
    tx: &mut sqlx::SqliteConnection,
    workflow_id: &str,
    project_id: &str,
    card_type_ids: &[String],
) -> AppResult<()> {
    for type_id in card_type_ids {
        config::find_card_type_tx(&mut *tx, project_id, type_id)
            .await?
            .ok_or_else(|| {
                AppError::Validation(format!("{type_id:?} is not a card type of this project."))
            })?;

        sqlx::query("UPDATE card_types SET workflow_id = ? WHERE id = ?")
            .bind(workflow_id)
            .bind(type_id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

/// The ids of the card types routed through a workflow.
pub async fn card_type_ids(db: &Db, workflow_id: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM card_types WHERE workflow_id = ? ORDER BY level DESC, name",
    )
    .bind(workflow_id)
    .fetch_all(db.reader())
    .await?)
}

/// Seeds a project's permissive default workflow: one workflow named `Default`,
/// including every one of the project's statuses, assigned to every one of its
/// card types.
///
/// Called at project creation. The migration does the same for pre-existing
/// projects, in SQL.
pub async fn seed_default(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    now: DateTime<Utc>,
) -> AppResult<Workflow> {
    let status_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM statuses WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&mut *tx)
            .await?;

    let workflow = insert(&mut *tx, project_id, "Default", true, &status_ids, now).await?;

    sqlx::query("UPDATE card_types SET workflow_id = ? WHERE project_id = ?")
        .bind(&workflow.id)
        .bind(project_id)
        .execute(&mut *tx)
        .await?;

    Ok(workflow)
}

// ---------------------------------------------------------------------------
// Transition CRUD
// ---------------------------------------------------------------------------

/// Every transition of a workflow, in position order.
pub async fn transitions(db: &Db, workflow_id: &str) -> AppResult<Vec<Transition>> {
    Ok(sqlx::query_as::<_, Transition>(
        "SELECT id, workflow_id, name, from_status_id, to_status_id, position FROM transitions \
         WHERE workflow_id = ? ORDER BY position, name",
    )
    .bind(workflow_id)
    .fetch_all(db.reader())
    .await?)
}

/// A transition by id.
pub async fn transition_by_id(db: &Db, id: &str) -> AppResult<Option<Transition>> {
    Ok(sqlx::query_as::<_, Transition>(
        "SELECT id, workflow_id, name, from_status_id, to_status_id, position FROM transitions \
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// A transition by id, inside a transaction.
pub async fn transition_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Transition>> {
    Ok(sqlx::query_as::<_, Transition>(
        "SELECT id, workflow_id, name, from_status_id, to_status_id, position FROM transitions \
         WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a transition. Does not commit.
pub async fn insert_transition(
    tx: &mut sqlx::SqliteConnection,
    workflow_id: &str,
    name: &str,
    from_status_id: Option<&str>,
    to_status_id: &str,
    position: i64,
) -> AppResult<Transition> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO transitions (id, workflow_id, name, from_status_id, to_status_id, position) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(workflow_id)
    .bind(name)
    .bind(from_status_id)
    .bind(to_status_id)
    .bind(position)
    .execute(&mut *tx)
    .await?;

    transition_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the transition just inserted is gone")))
}

/// Updates a transition's own fields. `from_status_id` is a double option:
/// `None` leaves it, `Some(None)` makes it global, `Some(Some(id))` anchors it.
pub async fn update_transition(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: Option<&str>,
    from_status_id: Option<Option<&str>>,
    to_status_id: Option<&str>,
    position: Option<i64>,
) -> AppResult<Transition> {
    sqlx::query(
        "UPDATE transitions SET \
           name           = COALESCE(?, name), \
           from_status_id = CASE WHEN ? THEN ? ELSE from_status_id END, \
           to_status_id   = COALESCE(?, to_status_id), \
           position       = COALESCE(?, position) \
         WHERE id = ?",
    )
    .bind(name)
    .bind(from_status_id.is_some())
    .bind(from_status_id.flatten())
    .bind(to_status_id)
    .bind(position)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    transition_by_id_tx(&mut *tx, id)
        .await?
        .ok_or(AppError::NotFound)
}

/// Deletes a transition (and, by cascade, its gates).
pub async fn delete_transition(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM transitions WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Removes every condition from a transition — for a PATCH that replaces the set.
pub async fn clear_conditions(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
) -> AppResult<()> {
    clear_gate(
        &mut *tx,
        "DELETE FROM transition_conditions WHERE transition_id = ?",
        transition_id,
    )
    .await
}

/// Removes every validator from a transition.
pub async fn clear_validators(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
) -> AppResult<()> {
    clear_gate(
        &mut *tx,
        "DELETE FROM transition_validators WHERE transition_id = ?",
        transition_id,
    )
    .await
}

/// Removes every post-function from a transition.
pub async fn clear_post_functions(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
) -> AppResult<()> {
    clear_gate(
        &mut *tx,
        "DELETE FROM transition_post_functions WHERE transition_id = ?",
        transition_id,
    )
    .await
}

async fn clear_gate(
    tx: &mut sqlx::SqliteConnection,
    sql: &'static str,
    transition_id: &str,
) -> AppResult<()> {
    sqlx::query(sql)
        .bind(transition_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// One of the three gate tables. A closed enum so the table name is never built
/// from a string — each variant maps to a fixed literal query.
#[derive(Debug, Clone, Copy)]
enum GateTable {
    Conditions,
    Validators,
    PostFunctions,
}

impl GateTable {
    /// The insert for a condition or validator, which have no position column.
    fn insert_unordered_sql(self) -> &'static str {
        match self {
            Self::Conditions => {
                "INSERT INTO transition_conditions (id, transition_id, kind, config) \
                 VALUES (?, ?, ?, ?)"
            }
            Self::Validators => {
                "INSERT INTO transition_validators (id, transition_id, kind, config) \
                 VALUES (?, ?, ?, ?)"
            }
            // Post-functions are ordered; they use their own insert below.
            Self::PostFunctions => "",
        }
    }

    fn select_sql(self) -> &'static str {
        match self {
            Self::Conditions => {
                "SELECT id, transition_id, kind, config, 0 AS position FROM transition_conditions \
                 WHERE transition_id = ? ORDER BY id"
            }
            Self::Validators => {
                "SELECT id, transition_id, kind, config, 0 AS position FROM transition_validators \
                 WHERE transition_id = ? ORDER BY id"
            }
            Self::PostFunctions => {
                "SELECT id, transition_id, kind, config, position FROM transition_post_functions \
                 WHERE transition_id = ? ORDER BY position, id"
            }
        }
    }
}

/// Adds a condition to a transition, validating its kind and config first.
pub async fn add_condition(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
    kind: &str,
    config: &str,
) -> AppResult<()> {
    parse_condition(kind, config)?;
    sqlx::query(GateTable::Conditions.insert_unordered_sql())
        .bind(Uuid::now_v7().to_string())
        .bind(transition_id)
        .bind(kind)
        .bind(config)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Adds a validator to a transition, validating it first.
pub async fn add_validator(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
    kind: &str,
    config: &str,
) -> AppResult<()> {
    parse_validator(kind, config)?;
    sqlx::query(GateTable::Validators.insert_unordered_sql())
        .bind(Uuid::now_v7().to_string())
        .bind(transition_id)
        .bind(kind)
        .bind(config)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Adds a post-function to a transition, validating it first.
pub async fn add_post_function(
    tx: &mut sqlx::SqliteConnection,
    transition_id: &str,
    kind: &str,
    config: &str,
    position: i64,
) -> AppResult<()> {
    parse_post_function(kind, config)?;
    sqlx::query(
        "INSERT INTO transition_post_functions (id, transition_id, kind, config, position) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(transition_id)
    .bind(kind)
    .bind(config)
    .bind(position)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn gate_rows(
    tx: &mut sqlx::SqliteConnection,
    table: GateTable,
    transition_id: &str,
) -> AppResult<Vec<GateRow>> {
    Ok(sqlx::query_as::<_, GateRow>(table.select_sql())
        .bind(transition_id)
        .fetch_all(&mut *tx)
        .await?)
}

/// A transition's gate rows of one table, read from the pool directly.
async fn gate_rows_pool(db: &Db, table: GateTable, transition_id: &str) -> AppResult<Vec<GateRow>> {
    Ok(sqlx::query_as::<_, GateRow>(table.select_sql())
        .bind(transition_id)
        .fetch_all(db.reader())
        .await?)
}

/// A transition's conditions, for the API to render.
pub async fn conditions_of(db: &Db, transition_id: &str) -> AppResult<Vec<GateRow>> {
    gate_rows_pool(db, GateTable::Conditions, transition_id).await
}

/// A transition's validators, for the API.
pub async fn validators_of(db: &Db, transition_id: &str) -> AppResult<Vec<GateRow>> {
    gate_rows_pool(db, GateTable::Validators, transition_id).await
}

/// A transition's post-functions, for the API.
pub async fn post_functions_of(db: &Db, transition_id: &str) -> AppResult<Vec<GateRow>> {
    gate_rows_pool(db, GateTable::PostFunctions, transition_id).await
}

// ---------------------------------------------------------------------------
// Evaluation — the execution contract
// ---------------------------------------------------------------------------

/// The result of asking "may this card move to that status?".
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Allowed with no specific transition: a default (or absent) workflow. No
    /// validators, no post-functions.
    Permissive,
    /// Allowed via this custom transition, whose validators must still pass and
    /// whose post-functions must run.
    Via(Box<Transition>),
}

/// The transitions a card may currently take, with conditions already evaluated.
///
/// This is what `GET /cards/{key}/transitions` returns and what the board offers.
/// A condition that fails removes its transition from the list — the hide half of
/// the execution contract.
pub async fn available_transitions(
    db: &Db,
    card: &Card,
    actor_id: Option<&str>,
) -> AppResult<Vec<AvailableTransition>> {
    let mut conn = db.reader().acquire().await?;
    let conn = conn.as_mut();

    let workflow = find_for_card_type_tx(conn, &card.type_id).await?;

    let permissive = match &workflow {
        None => true,
        Some(w) => w.is_default,
    };
    if permissive {
        return synthetic_moves(conn, card).await;
    }

    let workflow = workflow.expect("checked permissive above");
    let edges = candidate_transitions_any(conn, &workflow.id, &card.status_id).await?;
    let mut out = Vec::new();
    for edge in edges {
        // Conditions are evaluated for the viewer, so the board offers a button
        // only when *this* user could actually press it.
        if conditions_verdict(conn, &edge, card, &edge.to_status_id, actor_id)
            .await?
            .is_none()
        {
            out.push(AvailableTransition {
                id: Some(edge.id.clone()),
                name: edge.name.clone(),
                to_status_id: edge.to_status_id.clone(),
            });
        }
    }
    Ok(out)
}

/// For a permissive workflow: a synthetic "move to X" for every other status of
/// the project. These carry no id — the client takes them through the board's
/// move endpoint, not the transition-execute endpoint.
async fn synthetic_moves(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
) -> AppResult<Vec<AvailableTransition>> {
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM statuses WHERE project_id = ? ORDER BY position, name",
    )
    .bind(&card.project_id)
    .fetch_all(&mut *tx)
    .await?;
    Ok(statuses
        .into_iter()
        .filter(|(id, _)| id != &card.status_id)
        .map(|(id, name)| AvailableTransition {
            id: None,
            name: format!("Move to {name}"),
            to_status_id: id,
        })
        .collect())
}

/// Resolves whether an automatic move (a drag, or a `PATCH statusId`) to
/// `target_status_id` is legal, and if so via which transition.
///
/// - A default or absent workflow → [`Outcome::Permissive`].
/// - A custom workflow with a legal, unhidden edge to the target →
///   [`Outcome::Via`].
/// - A custom workflow with no edge to the target → [`AppError::Conflict`] (409):
///   the move is not part of the workflow.
/// - A custom workflow whose every edge to the target is hidden by a failing
///   condition → [`AppError::Forbidden`] (403), naming the first reason.
pub async fn resolve_transition(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    target_status_id: &str,
    actor_id: Option<&str>,
) -> AppResult<Outcome> {
    let Some(workflow) = find_for_card_type_tx(&mut *tx, &card.type_id).await? else {
        return Ok(Outcome::Permissive);
    };

    if workflow.is_default {
        return Ok(Outcome::Permissive);
    }

    let candidates =
        candidate_transitions_to(&mut *tx, &workflow.id, &card.status_id, target_status_id).await?;

    if candidates.is_empty() {
        return Err(AppError::Conflict(format!(
            "There is no transition to that status in the {:?} workflow.",
            workflow.name
        )));
    }

    let mut first_reason: Option<String> = None;
    for edge in candidates {
        match conditions_verdict(&mut *tx, &edge, card, target_status_id, actor_id).await? {
            None => return Ok(Outcome::Via(Box::new(edge))),
            Some(reason) => {
                if first_reason.is_none() {
                    first_reason = Some(reason);
                }
            }
        }
    }

    // Every edge to the target is hidden by a failing condition. The contract
    // says a hidden transition is rejected "as if it does not exist"; the reason
    // is named so the caller understands why the move they saw a moment ago no
    // longer applies.
    Err(AppError::Conflict(first_reason.unwrap_or_else(|| {
        "That transition is not available on this card right now.".to_owned()
    })))
}

/// Verifies that a specific transition may be taken now — the execute endpoint's
/// path. Checks the transition belongs to the card's workflow, is offered from
/// the card's current status, and is not hidden by a condition.
pub async fn verify_transition(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    transition: &Transition,
    actor_id: Option<&str>,
) -> AppResult<()> {
    let workflow = find_for_card_type_tx(&mut *tx, &card.type_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict(
                "This card's type has no custom workflow, so it has no named transitions. Move it \
                 with the board instead."
                    .to_owned(),
            )
        })?;

    if transition.workflow_id != workflow.id {
        return Err(AppError::NotFound);
    }

    // The edge must be offered from where the card actually is.
    let offered_here = match &transition.from_status_id {
        None => true,
        Some(from) => from == &card.status_id,
    };
    if !offered_here {
        return Err(AppError::Conflict(format!(
            "The transition {:?} is not available from the card's current status.",
            transition.name
        )));
    }

    // Conditions hide: a hidden transition attempted directly is rejected as if
    // it did not exist. The reason is named (409) rather than a bare 403 so the
    // caller learns why the button they were offered no longer applies.
    if let Some(reason) = conditions_verdict(
        &mut *tx,
        transition,
        card,
        &transition.to_status_id,
        actor_id,
    )
    .await?
    {
        return Err(AppError::Conflict(reason));
    }

    Ok(())
}

/// The candidate transitions to a specific target: those from the card's status
/// or global, landing on the target, in position order.
async fn candidate_transitions_to(
    tx: &mut sqlx::SqliteConnection,
    workflow_id: &str,
    from_status_id: &str,
    to_status_id: &str,
) -> AppResult<Vec<Transition>> {
    Ok(sqlx::query_as::<_, Transition>(
        "SELECT id, workflow_id, name, from_status_id, to_status_id, position FROM transitions \
         WHERE workflow_id = ? AND to_status_id = ? \
           AND (from_status_id IS NULL OR from_status_id = ?) \
         ORDER BY position, name",
    )
    .bind(workflow_id)
    .bind(to_status_id)
    .bind(from_status_id)
    .fetch_all(&mut *tx)
    .await?)
}

/// Every transition offered from the card's status (or global), to anywhere.
async fn candidate_transitions_any(
    tx: &mut sqlx::SqliteConnection,
    workflow_id: &str,
    from_status_id: &str,
) -> AppResult<Vec<Transition>> {
    Ok(sqlx::query_as::<_, Transition>(
        "SELECT id, workflow_id, name, from_status_id, to_status_id, position FROM transitions \
         WHERE workflow_id = ? AND (from_status_id IS NULL OR from_status_id = ?) \
         ORDER BY position, name",
    )
    .bind(workflow_id)
    .bind(from_status_id)
    .fetch_all(&mut *tx)
    .await?)
}

/// Runs every condition on a transition. `None` = all pass (offered); `Some` =
/// the first failure's human reason (hidden).
async fn conditions_verdict(
    tx: &mut sqlx::SqliteConnection,
    transition: &Transition,
    card: &Card,
    target_status_id: &str,
    actor_id: Option<&str>,
) -> AppResult<Option<String>> {
    let rows = gate_rows(&mut *tx, GateTable::Conditions, &transition.id).await?;

    for row in rows {
        let condition = parse_condition(&row.kind, &row.config)?;
        if let Some(reason) =
            evaluate_condition(&mut *tx, &condition, card, target_status_id, actor_id).await?
        {
            return Ok(Some(reason));
        }
    }
    Ok(None)
}

/// Evaluates one condition. `None` = passes; `Some(reason)` = fails (hidden).
///
/// A `None` actor is a system move (an automation, an agent, or an internal
/// re-resolution). User-scoped conditions do not bite there: the system is
/// trusted, and there is no user to compare against. Card-state conditions —
/// [`Condition::ChildBlocking`] — apply regardless of who is moving the card.
async fn evaluate_condition(
    tx: &mut sqlx::SqliteConnection,
    condition: &Condition,
    card: &Card,
    target_status_id: &str,
    actor_id: Option<&str>,
) -> AppResult<Option<String>> {
    Ok(match condition {
        Condition::OnlyAssignee => match actor_id {
            None => None,
            Some(actor) if card.assignee_id.as_deref() == Some(actor) => None,
            Some(_) => Some("Only the assignee may make this transition.".to_owned()),
        },
        Condition::OnlyReporter => match actor_id {
            None => None,
            Some(actor) if card.reporter_id.as_deref() == Some(actor) => None,
            Some(_) => Some("Only the reporter may make this transition.".to_owned()),
        },
        Condition::UserInRole(required) => match actor_id {
            None => None,
            Some(actor) => {
                if actor_holds_role(&mut *tx, &card.project_id, actor, *required).await? {
                    None
                } else {
                    Some(format!(
                        "Only a project {required} may make this transition."
                    ))
                }
            }
        },
        Condition::ChildBlocking => {
            let target = config::status_by_id_tx(&mut *tx, target_status_id)
                .await?
                .ok_or(AppError::NotFound)?;
            if target.category.is_done() && open_children(&mut *tx, &card.id).await? > 0 {
                Some(
                    "This card has children that are not done. Finish or move them out before \
                     closing it."
                        .to_owned(),
                )
            } else {
                None
            }
        }
    })
}

/// Whether an actor's effective project role meets a requirement.
async fn actor_holds_role(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    actor_id: &str,
    required: ProjectRole,
) -> AppResult<bool> {
    let Some(user) = crate::auth::user::find_by_id_tx(&mut *tx, actor_id).await? else {
        return Ok(false);
    };
    let Some(project) = project::find_by_id_tx(&mut *tx, project_id).await? else {
        return Ok(false);
    };
    let is_lead = project.lead_id.as_deref() == Some(actor_id);
    let granted = member::find_role_tx(&mut *tx, project_id, actor_id).await?;
    Ok(match member::resolve(user.role, is_lead, granted) {
        Some(role) => role.at_least(required),
        None => false,
    })
}

/// How many of a card's children are not in a done status.
async fn open_children(tx: &mut sqlx::SqliteConnection, card_id: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM cards c JOIN statuses s ON s.id = c.status_id \
         WHERE c.parent_id = ? AND c.deleted_at IS NULL AND s.category != 'done'",
    )
    .bind(card_id)
    .fetch_one(&mut *tx)
    .await?)
}

// ---------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------

/// Runs a transition's validators against the card the move would produce.
///
/// A failure is [`AppError::Validation`] (422) naming the field, and the caller
/// must not commit the move or run any post-function.
pub async fn run_validators(
    tx: &mut sqlx::SqliteConnection,
    transition: &Transition,
    resulting: &Card,
) -> AppResult<()> {
    let rows = gate_rows(&mut *tx, GateTable::Validators, &transition.id).await?;
    for row in rows {
        match parse_validator(&row.kind, &row.config)? {
            Validator::RequiredField(field) => {
                if field.is_empty_on(resulting) {
                    return Err(AppError::Validation(format!(
                        "The {} is required to make this transition.",
                        field.label()
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Post-functions
// ---------------------------------------------------------------------------

/// The changes a transition's post-functions make to card *fields*, applied to
/// `next` before it is written so they land in the same row and the same
/// changelog as the status change.
///
/// [`PostFunction::SetResolution`] deliberately only *sets* `next.resolution_id`;
/// the reconciliation against the landing status category stays in
/// [`crate::domain::card`]'s resolution rules, which this feeds rather than
/// duplicates.
pub async fn apply_field_post_functions(
    tx: &mut sqlx::SqliteConnection,
    transition: &Transition,
    next: &mut Card,
    actor_id: Option<&str>,
) -> AppResult<()> {
    let rows = gate_rows(&mut *tx, GateTable::PostFunctions, &transition.id).await?;

    for row in rows {
        match parse_post_function(&row.kind, &row.config)? {
            PostFunction::SetResolution(resolution_id) => {
                if let Some(id) = &resolution_id {
                    // Validated here so a post-function naming a resolution from
                    // another project fails loudly (422) and rolls the whole
                    // transition back, rather than tripping the deferred foreign
                    // key at commit as an opaque 500.
                    config::find_resolution_tx(&mut *tx, &next.project_id, id)
                        .await?
                        .ok_or_else(|| {
                            AppError::Validation(
                                "A SetResolution post-function names a resolution that is not in \
                                 this project."
                                    .to_owned(),
                            )
                        })?;
                }
                next.resolution_id = resolution_id;
            }
            PostFunction::AssignTo(target) => {
                next.assignee_id = match target {
                    AssignTarget::CurrentUser => actor_id.map(ToOwned::to_owned),
                    AssignTarget::Reporter => next.reporter_id.clone(),
                    AssignTarget::Lead => project::find_by_id_tx(&mut *tx, &next.project_id)
                        .await?
                        .and_then(|p| p.lead_id),
                    AssignTarget::Unassign => None,
                };
            }
            PostFunction::UpdateField { field, value } => {
                apply_update_field(&mut *tx, next, field, value).await?;
            }
            // Deferred: run after the write, in `run_deferred_post_functions`.
            PostFunction::AddComment(_) | PostFunction::FireEvent(_) => {}
        }
    }
    Ok(())
}

async fn apply_update_field(
    tx: &mut sqlx::SqliteConnection,
    next: &mut Card,
    field: FieldName,
    value: Option<String>,
) -> AppResult<()> {
    match field {
        FieldName::Priority => {
            if let Some(id) = &value {
                config::find_priority_tx(&mut *tx, &next.project_id, id)
                    .await?
                    .ok_or_else(|| {
                        AppError::Validation(
                            "An UpdateField post-function names a priority not in this project."
                                .to_owned(),
                        )
                    })?;
            }
            next.priority_id = value;
        }
        FieldName::Assignee => {
            if let Some(id) = &value {
                ensure_user_exists(&mut *tx, "assignee", id).await?;
            }
            next.assignee_id = value;
        }
        FieldName::Reporter => {
            if let Some(id) = &value {
                ensure_user_exists(&mut *tx, "reporter", id).await?;
            }
            next.reporter_id = value;
        }
        FieldName::Description => next.description = value,
        // Resolution goes through the resolution rules; other typed fields are
        // out of scope for a string-valued UpdateField.
        FieldName::Resolution | FieldName::DueDate | FieldName::StartDate | FieldName::Estimate => {
            return Err(AppError::Validation(format!(
                "UpdateField cannot set the {} here; use a dedicated post-function.",
                field.label()
            )));
        }
    }
    Ok(())
}

/// Refuses an assignee/reporter that names nobody.
///
/// [`crate::domain::card::update`] validates the two user-valued fields on the
/// PATCH path so a phantom id is a 422 rather than a raw foreign-key violation
/// surfacing as a 500. An `UpdateField` post-function reaches the same two
/// columns, so it must be just as careful — otherwise a workflow author's typo
/// lands as an opaque incident-logged 500 at transition time, telling the caller
/// nothing about which field was wrong. Existence, not eligibility: a deactivated
/// account is a real user and a card may still name it, matching the PATCH path.
async fn ensure_user_exists(
    tx: &mut sqlx::SqliteConnection,
    field: &str,
    user_id: &str,
) -> AppResult<()> {
    if crate::auth::user::find_by_id_tx(&mut *tx, user_id)
        .await?
        .is_none()
    {
        return Err(AppError::Validation(format!(
            "An UpdateField post-function sets the {field} to {user_id:?}, who is not a user of \
             this instance."
        )));
    }
    Ok(())
}

/// The post-functions that run *after* the write: adding a comment and firing an
/// event. Ordered comment-then-event, matching the execution contract.
///
/// A `None` actor cannot author a comment (the column is not nullable), so an
/// automated move skips the comment steps rather than inventing an author. The
/// event still fires, with a null author.
pub async fn run_deferred_post_functions(
    tx: &mut sqlx::SqliteConnection,
    transition: &Transition,
    card: &Card,
    actor_id: Option<&str>,
    entered_comment: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    // The comment the actor typed on the transition screen, first.
    if let (Some(body), Some(author)) = (entered_comment, actor_id) {
        let body = comment::validate_body(body)?;
        comment::insert(&mut *tx, &card.id, author, &body, now).await?;
    }

    let rows = gate_rows(&mut *tx, GateTable::PostFunctions, &transition.id).await?;
    for row in rows {
        match parse_post_function(&row.kind, &row.config)? {
            PostFunction::AddComment(body) => {
                if let Some(author) = actor_id {
                    comment::insert(&mut *tx, &card.id, author, &body, now).await?;
                }
            }
            PostFunction::FireEvent(event) => {
                record_event(&mut *tx, &card.id, &transition.id, &event, actor_id, now).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Records a fired event for Phase 15 automation to consume later.
async fn record_event(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    transition_id: &str,
    event: &str,
    actor_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO workflow_events (id, card_id, transition_id, event, author_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(card_id)
    .bind(transition_id)
    .bind(event)
    .bind(actor_id)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// A transition a card may currently take.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailableTransition {
    /// The transition id, or `null` for a permissive default workflow's implicit
    /// move (which the client takes through the board's move endpoint).
    pub id: Option<String>,
    /// The button label.
    pub name: String,
    /// Where the card would land.
    pub to_status_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_round_trip_through_their_wire_spelling() {
        for (spelling, field) in [
            ("priority", FieldName::Priority),
            ("assignee", FieldName::Assignee),
            ("reporter", FieldName::Reporter),
            ("resolution", FieldName::Resolution),
            ("dueDate", FieldName::DueDate),
            ("startDate", FieldName::StartDate),
            ("estimate", FieldName::Estimate),
            ("description", FieldName::Description),
        ] {
            assert_eq!(FieldName::from_str(spelling), Some(field));
        }
        assert_eq!(FieldName::from_str("nonsense"), None);
    }

    #[test]
    fn a_condition_with_an_unknown_kind_is_rejected_not_ignored() {
        // A silently-ignored condition is a hidden transition that is not hidden —
        // exactly the button-you-cannot-press the contract exists to prevent.
        assert!(parse_condition("Teleport", "{}").is_err());
        assert!(parse_condition("OnlyAssignee", "{}").is_ok());
        assert!(parse_condition("ChildBlocking", "{}").is_ok());
    }

    #[test]
    fn user_in_role_needs_a_valid_role() {
        assert_eq!(
            parse_condition("UserInRole", r#"{"role":"owner"}"#).unwrap(),
            Condition::UserInRole(ProjectRole::Owner)
        );
        assert!(parse_condition("UserInRole", r#"{"role":"emperor"}"#).is_err());
        assert!(parse_condition("UserInRole", "{}").is_err());
    }

    #[test]
    fn a_required_field_validator_names_a_real_field() {
        assert_eq!(
            parse_validator("RequiredField", r#"{"field":"resolution"}"#).unwrap(),
            Validator::RequiredField(FieldName::Resolution)
        );
        assert!(parse_validator("RequiredField", r#"{"field":"nonsense"}"#).is_err());
        assert!(parse_validator("Nope", "{}").is_err());
    }

    #[test]
    fn post_functions_parse_their_configs() {
        assert_eq!(
            parse_post_function("SetResolution", r#"{"resolutionId":"r1"}"#).unwrap(),
            PostFunction::SetResolution(Some("r1".to_owned()))
        );
        assert_eq!(
            parse_post_function("SetResolution", "{}").unwrap(),
            PostFunction::SetResolution(None)
        );
        assert_eq!(
            parse_post_function("AssignTo", r#"{"target":"currentUser"}"#).unwrap(),
            PostFunction::AssignTo(AssignTarget::CurrentUser)
        );
        assert!(parse_post_function("AssignTo", r#"{"target":"stranger"}"#).is_err());
        assert!(parse_post_function("FireEvent", r#"{"event":""}"#).is_err());
        assert!(parse_post_function("AddComment", "{}").is_err());
        assert!(parse_post_function("Nope", "{}").is_err());
    }
}

//! Running rules: matching triggers, evaluating conditions, applying actions, and recording
//! every run in the audit log.
//!
//! # Guardrails
//!
//! - **Loop detection.** Every write an action makes is tagged `automation:<rule>:<depth+1>`
//!   (via [`crate::activity::with_actor`]). An event carries the depth of the rule that caused
//!   it; a rule never reacts to its own changes, and nothing runs at [`MAX_DEPTH`] — the run is
//!   recorded as `loop_blocked` instead, so a ping-pong between two rules ends visibly.
//! - **Execution cap.** At most [`MAX_RUNS_PER_HOUR`] runs per rule per hour; past it, runs are
//!   recorded as `rate_limited` and skipped.
//! - **The owner's authority.** A rule acts as its owner and can do only what they can: the
//!   owner must still be an active Member of the project at run time, and a link to another
//!   project's card needs Member there too.
//! - **An audit log with the smart-value resolution trace**: every condition's result, every
//!   action, and every `{{…}}` it rendered.

use std::time::Duration as StdDuration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value, json};

use crate::activity::{Event, EventKind, with_actor};
use crate::api::AppState;
use crate::auth::{User, user};
use crate::automation::model::{Action, CompareOp, Condition, HttpMethod, RuleSpec, Trigger};
use crate::automation::smart::{self, Trace};
use crate::automation::store::{self, NewRun, Rule};
use crate::automation::web;
use crate::db::Db;
use crate::domain::card::{self, Card, CardPatch, NewCard, Placement};
use crate::domain::links::{self, LinkType};
use crate::domain::member::{self, ProjectRole};
use crate::domain::project::{self, Project};
use crate::domain::worklog::{self, NewWorklog};
use crate::domain::{comment, config, cycle, tag};
use crate::error::{AppError, AppResult};

/// The deepest a chain of rules triggering rules may go.
pub const MAX_DEPTH: u32 = 5;
/// The most runs one rule may make in an hour.
pub const MAX_RUNS_PER_HOUR: i64 = 200;

/// What fired a run, and in what circumstances.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The triggering card, as it is now.
    pub card: Option<Card>,
    /// Who triggered it.
    pub initiator: Option<User>,
    /// `{ field: { from, fromString, to, toString } }` for a change event.
    pub changelog: Value,
    /// The comment, for a comment event.
    pub comment: Value,
    /// The JSON body, for an incoming webhook.
    pub webhook: Value,
    /// How deep in a rule chain this is.
    pub depth: u32,
    /// The trigger kind, for the audit log.
    pub trigger: &'static str,
    /// Describe what would happen without doing it.
    pub dry_run: bool,
}

impl Invocation {
    /// An invocation with nothing but a trigger name.
    pub fn bare(trigger: &'static str) -> Self {
        Self {
            card: None,
            initiator: None,
            changelog: Value::Null,
            comment: Value::Null,
            webhook: Value::Null,
            depth: 0,
            trigger,
            dry_run: false,
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    /// `success`, `no_match`, `failed`, `loop_blocked`, `rate_limited`, `dry_run`.
    pub status: String,
    /// What happened, step by step.
    #[schema(value_type = Object)]
    pub trace: Value,
    /// Why it failed.
    pub error: Option<String>,
}

/// The mutable state of one run.
struct RunCtx<'a> {
    state: &'a AppState,
    project: Project,
    owner: User,
    card: Option<Card>,
    root: Value,
    trace: Trace,
    steps: Vec<Value>,
    depth: u32,
    dry: bool,
}

impl RunCtx<'_> {
    fn render(&mut self, template: &str) -> String {
        smart::render(template, &self.root, &mut self.trace)
    }

    fn card(&self) -> AppResult<&Card> {
        self.card.as_ref().ok_or_else(|| {
            AppError::Validation("This action needs a card, and the trigger gave none.".to_owned())
        })
    }

    async fn refresh_card(&mut self) -> AppResult<()> {
        if let Some(current) = &self.card {
            self.card = card::find_by_id(&self.state.db, &current.id).await?;
            if let Some(fresh) = &self.card {
                self.root["card"] = card_json(&self.state.db, fresh).await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

fn user_json(found: Option<&User>) -> Value {
    found.map_or(Value::Null, |u| {
        json!({
            "id": u.id,
            "username": u.username,
            "displayName": u.display_name,
            "email": u.email,
        })
    })
}

async fn user_by_id(db: &Db, id: Option<&str>) -> AppResult<Value> {
    Ok(match id {
        Some(id) => user_json(user::find_by_id(db, id).await?.as_ref()),
        None => Value::Null,
    })
}

async fn name_of(db: &Db, sql: &'static str, id: Option<&str>) -> AppResult<Value> {
    let Some(id) = id else { return Ok(Value::Null) };
    let name: Option<String> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(db.reader())
        .await?;
    Ok(name.map_or(Value::Null, |n| json!({ "id": id, "name": n })))
}

/// A card as smart values see it: `{{card.status.name}}`, `{{card.assignee.displayName}}`, ….
pub async fn card_json(db: &Db, found: &Card) -> AppResult<Value> {
    let (status, category): (String, String) =
        sqlx::query_as("SELECT name, category FROM statuses WHERE id = ?")
            .bind(&found.status_id)
            .fetch_one(db.reader())
            .await?;
    let (project_key, project_name): (String, String) =
        sqlx::query_as("SELECT key, name FROM projects WHERE id = ?")
            .bind(&found.project_id)
            .fetch_one(db.reader())
            .await?;
    let labels: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM card_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.card_id = ? \
         ORDER BY t.name",
    )
    .bind(&found.id)
    .fetch_all(db.reader())
    .await?;
    let comments: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT m.body, u.display_name, m.created_at FROM comments m \
           JOIN users u ON u.id = m.author_id WHERE m.card_id = ? \
          ORDER BY m.created_at DESC LIMIT 20",
    )
    .bind(&found.id)
    .fetch_all(db.reader())
    .await?;
    Ok(json!({
        "id": found.id,
        "key": found.key,
        "summary": found.summary,
        "description": found.description,
        "url": format!("/cards/{}", found.key),
        "status": { "id": found.status_id, "name": status, "category": category },
        "type": name_of(db, "SELECT name FROM card_types WHERE id = ?", Some(&found.type_id)).await?,
        "priority": name_of(db, "SELECT name FROM priorities WHERE id = ?", found.priority_id.as_deref()).await?,
        "resolution": name_of(db, "SELECT name FROM resolutions WHERE id = ?", found.resolution_id.as_deref()).await?,
        "milestone": name_of(db, "SELECT name FROM milestones WHERE id = ?", found.milestone_id.as_deref()).await?,
        "assignee": user_by_id(db, found.assignee_id.as_deref()).await?,
        "reporter": user_by_id(db, found.reporter_id.as_deref()).await?,
        "creator": user_by_id(db, Some(&found.creator_id)).await?,
        "project": { "id": found.project_id, "key": project_key, "name": project_name },
        "dueDate": found.due_date.map(|d| d.to_string()),
        "startDate": found.start_date.map(|d| d.to_string()),
        "estimate": found.estimate,
        "created": found.created_at.to_rfc3339(),
        "updated": found.updated_at.to_rfc3339(),
        "resolved": found.resolved_at.map(|d| d.to_rfc3339()),
        "labels": labels,
        "comments": comments.into_iter().map(|(body, author, at)| json!({
            "body": body, "author": { "displayName": author }, "created": at.to_rfc3339(),
        })).collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

fn same(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// Whether an activity event fires a trigger.
pub fn matches(trigger: &Trigger, event: &Event) -> bool {
    match (trigger, &event.kind) {
        (Trigger::CardCreated, EventKind::CardCreated)
        | (Trigger::Commented, EventKind::CommentAdded { .. }) => true,
        (Trigger::CardTransitioned { from, to }, EventKind::CardUpdated { .. }) => {
            event.change("status").is_some_and(|change| {
                from.as_deref()
                    .is_none_or(|f| same(change.from_display.as_deref(), f))
                    && to
                        .as_deref()
                        .is_none_or(|t| same(change.to_display.as_deref(), t))
            })
        }
        (Trigger::FieldChanged { fields }, EventKind::CardUpdated { changes }) => {
            changes.iter().any(|c| {
                c.field != "rank"
                    && (fields.is_empty()
                        || fields.iter().any(|f| f.eq_ignore_ascii_case(&c.field)))
            })
        }
        (Trigger::CardAssigned, EventKind::CardUpdated { .. }) => event
            .change("assignee")
            .is_some_and(|c| c.to_value.is_some()),
        _ => false,
    }
}

fn changelog_json(event: &Event) -> Value {
    let mut map = Map::new();
    if let EventKind::CardUpdated { changes } = &event.kind {
        for change in changes {
            map.insert(
                change.field.clone(),
                json!({
                    "from": change.from_value,
                    "fromString": change.from_display,
                    "to": change.to_value,
                    "toString": change.to_display,
                }),
            );
        }
    }
    Value::Object(map)
}

/// Offers one activity event to every enabled rule in its project.
pub async fn on_event(state: &AppState, event: &Event) -> AppResult<()> {
    let rules = store::enabled_for(&state.db, &event.card.project_id).await?;
    if rules.is_empty() {
        return Ok(());
    }
    let current = card::find_by_id(&state.db, &event.card.id).await?;
    let initiator = match &event.author_id {
        Some(id) => user::find_by_id(&state.db, id).await?,
        None => None,
    };
    let comment = match &event.kind {
        EventKind::CommentAdded { body, comment_id } => json!({
            "id": comment_id,
            "body": body,
            "author": user_json(initiator.as_ref()),
        }),
        _ => Value::Null,
    };
    for rule in rules {
        if !matches(&rule.spec.trigger, event) {
            continue;
        }
        // A rule never reacts to its own changes; chains of *different* rules are bounded by
        // MAX_DEPTH inside `execute`.
        if event.automation_rule() == Some(rule.id.as_str()) {
            continue;
        }
        let invocation = Invocation {
            card: current.clone(),
            initiator: initiator.clone(),
            changelog: changelog_json(event),
            comment: comment.clone(),
            webhook: Value::Null,
            depth: event.automation_depth(),
            trigger: rule.spec.trigger.kind(),
            dry_run: false,
        };
        Box::pin(execute(state, &rule, invocation)).await?;
    }
    Ok(())
}

/// Runs every scheduled rule that is due. Returns how many rules ran.
pub async fn run_due(state: &AppState, now: DateTime<Utc>) -> AppResult<usize> {
    let due = store::due(&state.db, now).await?;
    let mut ran = 0;
    for rule in due {
        let Trigger::Scheduled { aql, .. } = &rule.spec.trigger else {
            continue;
        };
        // Arm the next run first, so a rule that fails every time does not run every minute.
        store::mark_ran(&state.db, &rule, now).await?;
        run_for_query(state, &rule, aql.as_deref(), "scheduled", Value::Null).await?;
        ran += 1;
    }
    Ok(ran)
}

/// Runs a rule once per card matching `aql` (scoped to the rule's project and its owner's
/// access), or once with no card when there is no query.
async fn run_for_query(
    state: &AppState,
    rule: &Rule,
    aql: Option<&str>,
    trigger: &'static str,
    webhook: Value,
) -> AppResult<Vec<RunResult>> {
    let Some(owner) = user::find_by_id(&state.db, &rule.owner_id).await? else {
        return Ok(Vec::new());
    };
    let mut results = Vec::new();
    if let Some(query) = aql.map(str::trim).filter(|q| !q.is_empty()) {
        {
            let project = project::find_by_id(&state.db, &rule.project_id)
                .await?
                .ok_or(AppError::NotFound)?;
            let scoped = format!("project = \"{}\" AND ({query})", project.key);
            let found = crate::aql::search(
                &state.db,
                &owner,
                crate::auth::now(),
                &scoped,
                crate::automation::model::MAX_LOOKUP,
                0,
            )
            .await?;
            for target in found.cards {
                let invocation = Invocation {
                    card: Some(target),
                    initiator: Some(owner.clone()),
                    webhook: webhook.clone(),
                    ..Invocation::bare(trigger)
                };
                results.push(Box::pin(execute(state, rule, invocation)).await?);
            }
        }
    } else {
        let invocation = Invocation {
            initiator: Some(owner.clone()),
            webhook,
            ..Invocation::bare(trigger)
        };
        results.push(Box::pin(execute(state, rule, invocation)).await?);
    }
    Ok(results)
}

/// Runs a rule from its incoming webhook.
pub async fn run_webhook(state: &AppState, rule: &Rule, body: Value) -> AppResult<Vec<RunResult>> {
    let aql = match &rule.spec.trigger {
        Trigger::IncomingWebhook { aql } => aql.clone(),
        _ => {
            return Err(AppError::Validation(
                "This rule is not triggered by a webhook.".to_owned(),
            ));
        }
    };
    run_for_query(
        state,
        rule,
        aql.as_deref(),
        "incomingWebhook",
        json!({ "body": body }),
    )
    .await
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Runs one rule once: guards, conditions, actions, audit log.
// Straight-line: guard, guard, authority, context, conditions, actions, record — each step
// short, and the order is the contract the module doc describes.
#[allow(clippy::too_many_lines)]
pub async fn execute(
    state: &AppState,
    rule: &Rule,
    invocation: Invocation,
) -> AppResult<RunResult> {
    let started = crate::auth::now();
    let card_id = invocation.card.as_ref().map(|c| c.id.clone());
    let record = |status: &'static str, trace: Value, error: Option<String>| {
        let card_id = card_id.clone();
        async move {
            if !invocation.dry_run {
                store::record_run(
                    &state.db,
                    &NewRun {
                        rule_id: &rule.id,
                        card_id: card_id.as_deref(),
                        trigger: invocation.trigger,
                        status,
                        trace: &trace,
                        error: error.as_deref(),
                        started_at: started,
                    },
                    crate::auth::now(),
                )
                .await?;
            }
            AppResult::Ok(RunResult {
                status: (if invocation.dry_run {
                    "dry_run"
                } else {
                    status
                })
                .to_owned(),
                trace,
                error,
            })
        }
    };

    if invocation.depth >= MAX_DEPTH {
        return record(
            "loop_blocked",
            json!({ "depth": invocation.depth }),
            Some(format!(
                "Stopped at depth {MAX_DEPTH}: rules were triggering each other."
            )),
        )
        .await;
    }
    if !invocation.dry_run
        && store::runs_since(&state.db, &rule.id, started - chrono::Duration::hours(1)).await?
            >= MAX_RUNS_PER_HOUR
    {
        return record(
            "rate_limited",
            json!({}),
            Some(format!(
                "More than {MAX_RUNS_PER_HOUR} runs in the last hour."
            )),
        )
        .await;
    }

    let (project, owner) = match authority(&state.db, rule).await {
        Ok(found) => found,
        Err(err) => return record("failed", json!({}), Some(err.to_string())).await,
    };

    let mut root = json!({
        "now": started.to_rfc3339(),
        "today": started.date_naive().to_string(),
        "rule": { "id": rule.id, "name": rule.name },
        "project": { "id": project.id, "key": project.key, "name": project.name },
        "initiator": user_json(invocation.initiator.as_ref()),
        "changelog": invocation.changelog,
        "comment": invocation.comment,
        "webhook": invocation.webhook,
        "vars": {},
        "lookupCards": [],
    });
    if let Some(found) = &invocation.card {
        root["card"] = card_json(&state.db, found).await?;
        root["issue"] = root["card"].clone();
    }

    let mut ctx = RunCtx {
        state,
        project,
        owner,
        card: invocation.card.clone(),
        root,
        trace: Trace::default(),
        steps: Vec::new(),
        depth: invocation.depth,
        dry: invocation.dry_run,
    };

    let (passed, condition_trace) =
        match Box::pin(evaluate_all(&mut ctx, &rule.spec.conditions)).await {
            Ok(found) => found,
            Err(err) => {
                let trace = json!({ "error": err.to_string() });
                return record("failed", trace, Some(err.to_string())).await;
            }
        };
    if !passed {
        let trace = json!({ "conditions": condition_trace, "resolutions": ctx.trace.resolutions });
        return record("no_match", trace, None).await;
    }

    let actor = format!("automation:{}:{}", rule.id, ctx.depth + 1);
    let outcome = with_actor(actor, apply_all(&mut ctx, &rule.spec.actions)).await;
    let trace = json!({
        "conditions": condition_trace,
        "actions": ctx.steps,
        "resolutions": ctx.trace.resolutions,
    });
    match outcome {
        Ok(()) => record("success", trace, None).await,
        Err(err) => record("failed", trace, Some(err.to_string())).await,
    }
}

/// The project and the owner a rule acts as — or why it can no longer act.
async fn authority(db: &Db, rule: &Rule) -> AppResult<(Project, User)> {
    let project = project::find_by_id(db, &rule.project_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let owner = user::find_by_id(db, &rule.owner_id)
        .await?
        .filter(|u| u.is_active)
        .ok_or_else(|| AppError::Validation("The rule's owner is deactivated.".to_owned()))?;
    let effective = member::effective_role(db, &project, &owner).await?;
    if !effective.is_some_and(|r| r.at_least(ProjectRole::Member)) {
        return Err(AppError::Validation(
            "The rule's owner can no longer edit this project.".to_owned(),
        ));
    }
    Ok((project, owner))
}

async fn evaluate_all(
    ctx: &mut RunCtx<'_>,
    conditions: &[Condition],
) -> AppResult<(bool, Vec<Value>)> {
    let mut trace = Vec::new();
    let mut all = true;
    for condition in conditions {
        let passed = Box::pin(evaluate(ctx, condition)).await?;
        trace.push(json!({ "condition": condition, "passed": passed }));
        if !passed {
            all = false;
            break;
        }
    }
    Ok((all, trace))
}

async fn evaluate(ctx: &mut RunCtx<'_>, condition: &Condition) -> AppResult<bool> {
    Ok(match condition {
        Condition::Aql { query } => {
            let Some(key) = ctx.card.as_ref().map(|c| c.key.clone()) else {
                return Ok(false);
            };
            let query = ctx.render(query);
            let scoped = format!("key = \"{key}\" AND ({query})");
            let result =
                crate::aql::search(&ctx.state.db, &ctx.owner, crate::auth::now(), &scoped, 1, 0)
                    .await?;
            result.total > 0
        }
        Condition::Field {
            value,
            op,
            expected,
        } => {
            let actual = ctx.render(value);
            let expected = expected
                .as_deref()
                .map(|e| ctx.render(e))
                .unwrap_or_default();
            let (a, e) = (actual.trim().to_lowercase(), expected.trim().to_lowercase());
            match op {
                CompareOp::Equals => a == e,
                CompareOp::NotEquals => a != e,
                CompareOp::Contains => a.contains(&e),
                CompareOp::IsEmpty => a.is_empty(),
                CompareOp::IsNotEmpty => !a.is_empty(),
            }
        }
        Condition::User { usernames, negate } => {
            let who = ctx.root["initiator"]["username"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let listed = usernames.iter().any(|u| u.eq_ignore_ascii_case(&who));
            listed != *negate
        }
        Condition::Regex { value, pattern } => {
            let text = ctx.render(value);
            regex::RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .build()
                .map_err(|err| AppError::Validation(format!("Invalid pattern: {err}")))?
                .is_match(&text)
        }
    })
}

fn apply_all<'a>(
    ctx: &'a mut RunCtx<'_>,
    actions: &'a [Action],
) -> std::pin::Pin<Box<dyn Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        for action in actions {
            if let Action::IfElse {
                conditions,
                then,
                otherwise,
            } = action
            {
                let (passed, trace) = evaluate_all(ctx, conditions).await?;
                ctx.steps.push(json!({ "action": "ifElse", "conditions": trace, "branch": if passed { "then" } else { "else" } }));
                apply_all(ctx, if passed { then } else { otherwise }).await?;
                continue;
            }
            // Boxed: one arm per action makes this future large, and an unboxed one nested
            // inside the pump's would overflow a debug build's stack.
            match Box::pin(apply(ctx, action)).await {
                Ok(detail) => ctx
                    .steps
                    .push(json!({ "action": action.kind(), "ok": true, "detail": detail })),
                Err(err) => {
                    ctx.steps.push(
                        json!({ "action": action.kind(), "ok": false, "error": err.to_string() }),
                    );
                    return Err(err);
                }
            }
            if !ctx.dry {
                ctx.refresh_card().await?;
            }
        }
        Ok(())
    })
}

fn parse_date(text: &str) -> AppResult<Option<NaiveDate>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let date = DateTime::parse_from_rfc3339(text)
        .map(|d| d.date_naive())
        .or_else(|_| NaiveDate::parse_from_str(text, "%Y-%m-%d"))
        .map_err(|_| AppError::Validation(format!("{text:?} is not a date.")))?;
    Ok(Some(date))
}

async fn find_user(db: &Db, name_or_id: &str) -> AppResult<User> {
    let found = match user::find_by_username(db, name_or_id).await? {
        Some(found) => Some(found),
        None => user::find_by_id(db, name_or_id).await?,
    };
    found
        .filter(|u| u.is_active)
        .ok_or_else(|| AppError::Validation(format!("{name_or_id:?} is not an active user.")))
}

/// Updates the card, attributing the change to the rule's owner.
async fn update_card(ctx: &RunCtx<'_>, patch: &CardPatch) -> AppResult<()> {
    let current = ctx.card()?.clone();
    let mut tx = ctx.state.db.begin_write().await?;
    card::update(
        &mut tx,
        &current,
        patch,
        Some(&ctx.owner.id),
        crate::auth::now(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn type_for(ctx: &RunCtx<'_>, name: Option<&str>, below: Option<&Card>) -> AppResult<String> {
    let types = config::card_types(&ctx.state.db, &ctx.project.id).await?;
    if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
        return types
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name.trim()))
            .map(|t| t.id.clone())
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{name:?} is not a card type of {}.",
                    ctx.project.key
                ))
            });
    }
    if let Some(parent) = below {
        let parent_level = types
            .iter()
            .find(|t| t.id == parent.type_id)
            .map_or(i64::MAX, |t| t.level);
        // The highest level below the parent: an Epic's children are Stories, not Sub-tasks.
        return types
            .iter()
            .filter(|t| t.level < parent_level)
            .max_by_key(|t| (t.level, t.is_default))
            .map(|t| t.id.clone())
            .ok_or_else(|| {
                AppError::Validation("Nothing can be a child of this card's type.".to_owned())
            });
    }
    types
        .iter()
        .find(|t| t.is_default)
        .or_else(|| types.first())
        .map(|t| t.id.clone())
        .ok_or_else(|| AppError::Validation("The project has no card types.".to_owned()))
}

async fn create(ctx: &RunCtx<'_>, new: NewCard) -> AppResult<Card> {
    let mut tx = ctx.state.db.begin_write().await?;
    let created = card::create(
        &mut tx,
        &ctx.project,
        &new,
        &ctx.owner.id,
        crate::auth::now(),
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

fn new_card(
    type_id: String,
    parent_id: Option<String>,
    summary: String,
    description: Option<String>,
) -> NewCard {
    NewCard {
        type_id,
        parent_id,
        summary,
        description,
        status_id: None,
        priority_id: None,
        assignee_id: None,
        reporter_id: None,
        due_date: None,
        start_date: None,
        estimate: None,
        placement: Placement::Bottom,
    }
}

// One arm per action: the module doc of `model::Action` is the spec, and this is it.
#[allow(clippy::too_many_lines)]
async fn apply(ctx: &mut RunCtx<'_>, action: &Action) -> AppResult<Value> {
    let db = ctx.state.db.clone();
    match action {
        Action::Edit {
            summary,
            description,
            priority,
            due_date,
            estimate,
        } => {
            let mut patch = CardPatch::default();
            if let Some(text) = summary {
                patch.summary = Some(card::validate_summary(&ctx.render(text))?);
            }
            if let Some(text) = description {
                let rendered = ctx.render(text);
                patch.description = Some(
                    Some(rendered)
                        .filter(|d| !d.trim().is_empty())
                        .map(|d| card::validate_description(&d))
                        .transpose()?,
                );
            }
            if let Some(name) = priority {
                let name = ctx.render(name);
                patch.priority_id = Some(if name.trim().is_empty() {
                    None
                } else {
                    let found: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM priorities WHERE project_id = ? AND name = ? COLLATE NOCASE",
                    )
                    .bind(&ctx.project.id)
                    .bind(name.trim())
                    .fetch_optional(db.reader())
                    .await?;
                    Some(found.ok_or_else(|| {
                        AppError::Validation(format!("{name:?} is not a priority."))
                    })?)
                });
            }
            if let Some(text) = due_date {
                patch.due_date = Some(parse_date(&ctx.render(text))?);
            }
            if let Some(text) = estimate {
                let rendered = ctx.render(text);
                patch.estimate = Some(if rendered.trim().is_empty() {
                    None
                } else {
                    Some(rendered.trim().parse::<f64>().map_err(|_| {
                        AppError::Validation(format!("{rendered:?} is not a number."))
                    })?)
                });
            }
            if ctx.dry {
                return Ok(json!({ "would": "edit" }));
            }
            update_card(ctx, &patch).await?;
            Ok(json!({ "edited": true }))
        }
        Action::Transition { to } => {
            let to = ctx.render(to);
            let status: Option<String> = sqlx::query_scalar(
                "SELECT id FROM statuses WHERE project_id = ? AND name = ? COLLATE NOCASE",
            )
            .bind(&ctx.project.id)
            .bind(to.trim())
            .fetch_optional(db.reader())
            .await?;
            let status =
                status.ok_or_else(|| AppError::Validation(format!("{to:?} is not a status.")))?;
            ctx.card()?;
            if ctx.dry {
                return Ok(json!({ "would": "transition", "to": to }));
            }
            update_card(
                ctx,
                &CardPatch {
                    status_id: Some(status),
                    ..CardPatch::default()
                },
            )
            .await?;
            Ok(json!({ "to": to }))
        }
        Action::Comment { body } => {
            let body = comment::validate_body(&ctx.render(body))?;
            let target = ctx.card()?.id.clone();
            if ctx.dry {
                return Ok(json!({ "would": "comment", "body": body }));
            }
            let mut tx = db.begin_write().await?;
            comment::insert(&mut tx, &target, &ctx.owner.id, &body, crate::auth::now()).await?;
            tx.commit().await?;
            Ok(json!({ "body": body }))
        }
        Action::Assign { user: who } => {
            let who = ctx.render(who);
            let assignee = match who.trim() {
                "" => None,
                "@lead" => ctx.project.lead_id.clone(),
                name => Some(find_user(&db, name).await?.id),
            };
            ctx.card()?;
            if ctx.dry {
                return Ok(json!({ "would": "assign", "to": assignee }));
            }
            update_card(
                ctx,
                &CardPatch {
                    assignee_id: Some(assignee.clone()),
                    ..CardPatch::default()
                },
            )
            .await?;
            Ok(json!({ "assignee": assignee }))
        }
        Action::CreateCard {
            summary,
            description,
            card_type,
            as_child,
        } => {
            let summary = card::validate_summary(&ctx.render(summary))?;
            let description = description
                .as_deref()
                .map(|d| ctx.render(d))
                .filter(|d| !d.trim().is_empty());
            let parent = if *as_child {
                Some(ctx.card()?.clone())
            } else {
                None
            };
            let type_id = type_for(ctx, card_type.as_deref(), parent.as_ref()).await?;
            if ctx.dry {
                return Ok(json!({ "would": "createCard", "summary": summary }));
            }
            let created = create(
                ctx,
                new_card(type_id, parent.map(|p| p.id), summary, description),
            )
            .await?;
            Ok(json!({ "created": created.key }))
        }
        Action::CreateChildren {
            summaries,
            card_type,
        } => {
            let parent = ctx.card()?.clone();
            let type_id = type_for(ctx, card_type.as_deref(), Some(&parent)).await?;
            let mut keys = Vec::new();
            for summary in summaries {
                let summary = card::validate_summary(&ctx.render(summary))?;
                if ctx.dry {
                    keys.push(summary);
                    continue;
                }
                let created = create(
                    ctx,
                    new_card(type_id.clone(), Some(parent.id.clone()), summary, None),
                )
                .await?;
                keys.push(created.key);
            }
            Ok(json!({ "children": keys }))
        }
        Action::Clone { summary_suffix } => {
            let source = ctx.card()?.clone();
            let suffix = summary_suffix
                .as_deref()
                .map(|s| ctx.render(s))
                .unwrap_or_default();
            let summary = card::validate_summary(&format!("{}{suffix}", source.summary))?;
            if ctx.dry {
                return Ok(json!({ "would": "clone", "summary": summary }));
            }
            let mut new = new_card(
                source.type_id.clone(),
                source.parent_id.clone(),
                summary,
                source.description.clone(),
            );
            new.priority_id.clone_from(&source.priority_id);
            let created = create(ctx, new).await?;
            let mut tx = db.begin_write().await?;
            links::link_cards(
                &mut tx,
                &created.id,
                &source.id,
                LinkType::Clones,
                crate::auth::now(),
            )
            .await?;
            tx.commit().await?;
            Ok(json!({ "clone": created.key }))
        }
        Action::Link {
            target_key,
            link_type,
        } => {
            let key = ctx.render(target_key).trim().to_ascii_uppercase();
            let kind = LinkType::parse(link_type).ok_or_else(|| {
                AppError::Validation(format!("{link_type:?} is not a link type."))
            })?;
            let source = ctx.card()?.clone();
            let other = card::find_by_key(&db, &key)
                .await?
                .filter(|c| !c.is_deleted())
                .ok_or_else(|| AppError::Validation(format!("{key} is not a card.")))?;
            let other_project = project::find_by_id(&db, &other.project_id)
                .await?
                .ok_or(AppError::NotFound)?;
            if !member::effective_role(&db, &other_project, &ctx.owner)
                .await?
                .is_some_and(|r| r.at_least(ProjectRole::Member))
            {
                return Err(AppError::Validation(format!(
                    "The rule's owner cannot edit {key}."
                )));
            }
            if ctx.dry {
                return Ok(json!({ "would": "link", "to": key }));
            }
            let mut tx = db.begin_write().await?;
            links::link_cards(&mut tx, &source.id, &other.id, kind, crate::auth::now()).await?;
            tx.commit().await?;
            Ok(json!({ "linked": key }))
        }
        Action::LogWork { duration, note } => {
            let minutes = worklog::parse_duration(&ctx.render(duration))?;
            let note = worklog::validate_note(note.as_deref().map(|n| ctx.render(n)).as_deref())?;
            let target = ctx.card()?.id.clone();
            if ctx.dry {
                return Ok(json!({ "would": "logWork", "minutes": minutes }));
            }
            let mut tx = db.begin_write().await?;
            worklog::insert(
                &mut tx,
                &NewWorklog {
                    card_id: &target,
                    author_id: Some(&ctx.owner.id),
                    minutes,
                    note,
                    source: "automation",
                    started_at: None,
                },
                crate::auth::now(),
            )
            .await?;
            tx.commit().await?;
            Ok(json!({ "minutes": minutes }))
        }
        Action::AddWatchers { usernames } => {
            let target = ctx.card()?.id.clone();
            let mut added = Vec::new();
            for name in usernames {
                let name = ctx.render(name);
                let found = find_user(&db, name.trim()).await?;
                if !ctx.dry {
                    let mut tx = db.begin_write().await?;
                    links::watch(&mut tx, &target, &found.id, crate::auth::now()).await?;
                    tx.commit().await?;
                }
                added.push(found.username);
            }
            Ok(json!({ "watchers": added }))
        }
        Action::AddTag { name } => {
            let name = tag::validate_name(&ctx.render(name))?;
            let target = ctx.card()?.id.clone();
            if ctx.dry {
                return Ok(json!({ "would": "addTag", "tag": name }));
            }
            let mut tx = db.begin_write().await?;
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT id FROM tags WHERE (project_id = ? OR project_id IS NULL) AND name = ? \
                 ORDER BY project_id IS NULL LIMIT 1",
            )
            .bind(&ctx.project.id)
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await?;
            let tag_id = match existing {
                Some(id) => id,
                None => {
                    tag::insert(
                        &mut tx,
                        Some(&ctx.project.id),
                        &name,
                        None,
                        crate::auth::now(),
                    )
                    .await?
                    .id
                }
            };
            tag::attach(&mut tx, &target, &tag_id, crate::auth::now()).await?;
            tx.commit().await?;
            Ok(json!({ "tag": name }))
        }
        Action::AddToActiveCycle => {
            let target = ctx.card()?.id.clone();
            let active: Option<String> = sqlx::query_scalar(
                "SELECT id FROM cycles WHERE project_id = ? AND state = 'active' LIMIT 1",
            )
            .bind(&ctx.project.id)
            .fetch_optional(db.reader())
            .await?;
            let Some(active) = active else {
                return Ok(json!({ "skipped": "the project has no active cycle" }));
            };
            if !ctx.dry {
                let mut tx = db.begin_write().await?;
                cycle::add_card(&mut tx, &target, &active, crate::auth::now()).await?;
                tx.commit().await?;
            }
            Ok(json!({ "cycle": active }))
        }
        Action::SendEmail { to, subject, body } => {
            let (to, subject, body) = (ctx.render(to), ctx.render(subject), ctx.render(body));
            if ctx.dry {
                return Ok(json!({ "would": "sendEmail", "to": to, "subject": subject }));
            }
            crate::notify::mail::send(&db, ctx.state.vault.as_deref(), to.trim(), &subject, &body)
                .await?;
            Ok(json!({ "to": to }))
        }
        Action::WebRequest {
            url,
            method,
            headers,
            body,
        } => {
            let url = ctx.render(url);
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|(k, v)| (k.clone(), ctx.render(v)))
                .collect();
            let body = body.as_deref().map(|b| ctx.render(b));
            if ctx.dry {
                return Ok(json!({ "would": "webRequest", "url": url }));
            }
            let response = web::send(
                method_name(*method),
                &url,
                &headers,
                body,
                StdDuration::from_secs(10),
            )
            .await?;
            ctx.root["webResponse"] = response.clone();
            Ok(json!({ "status": response["status"] }))
        }
        Action::SetVariable { name, value } => {
            let value = ctx.render(value);
            ctx.root["vars"][name] = Value::String(value.clone());
            Ok(json!({ name: value }))
        }
        Action::LookupCards { aql } => {
            let query = ctx.render(aql);
            let found = crate::aql::search(
                &db,
                &ctx.owner,
                crate::auth::now(),
                &query,
                crate::automation::model::MAX_LOOKUP,
                0,
            )
            .await?;
            let mut items = Vec::new();
            for found in &found.cards {
                items.push(json!({ "key": found.key, "summary": found.summary, "url": format!("/cards/{}", found.key) }));
            }
            let count = items.len();
            ctx.root["lookupCards"] = Value::Array(items);
            Ok(json!({ "found": count }))
        }
        Action::IfElse { .. } => Ok(Value::Null),
    }
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
    }
}

/// Evaluates a rule definition against a card without changing anything — the rule builder's
/// "test this rule" button. Conditions are evaluated for real; actions render their smart
/// values and report what they *would* do.
pub async fn dry_run(
    state: &AppState,
    project_id: &str,
    owner_id: &str,
    spec: &RuleSpec,
    target: Option<Card>,
) -> AppResult<RunResult> {
    let rule = Rule {
        id: "dry-run".to_owned(),
        project_id: project_id.to_owned(),
        name: "Dry run".to_owned(),
        description: None,
        enabled: true,
        spec: spec.clone(),
        owner_id: owner_id.to_owned(),
        has_webhook: false,
        next_run_at: None,
        last_run_at: None,
        created_at: crate::auth::now(),
        updated_at: crate::auth::now(),
    };
    let initiator = user::find_by_id(&state.db, owner_id).await?;
    execute(
        state,
        &rule,
        Invocation {
            card: target,
            initiator,
            dry_run: true,
            ..Invocation::bare(spec.trigger.kind())
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{CardRef, FieldChange};

    fn event(kind: EventKind) -> Event {
        Event {
            kind,
            card: CardRef {
                id: "c".into(),
                key: "A-1".into(),
                summary: "s".into(),
                description: None,
                project_id: "p".into(),
                project_key: "A".into(),
                assignee_id: None,
                reporter_id: None,
                status_name: "Done".into(),
                type_id: "t".into(),
            },
            author_id: None,
            actor: None,
            at: Utc::now(),
        }
    }

    fn change(field: &str, from: Option<&str>, to: Option<&str>) -> FieldChange {
        FieldChange {
            field: field.into(),
            from_value: from.map(Into::into),
            from_display: from.map(Into::into),
            to_value: to.map(Into::into),
            to_display: to.map(Into::into),
        }
    }

    #[test]
    fn transitions_match_on_from_and_to_by_name() {
        let moved = event(EventKind::CardUpdated {
            changes: vec![change("status", Some("Review"), Some("Approved"))],
        });
        let any = Trigger::CardTransitioned {
            from: None,
            to: None,
        };
        let to_approved = Trigger::CardTransitioned {
            from: None,
            to: Some("approved".into()),
        };
        let from_todo = Trigger::CardTransitioned {
            from: Some("To Do".into()),
            to: None,
        };
        assert!(matches(&any, &moved));
        assert!(matches(&to_approved, &moved));
        assert!(!matches(&from_todo, &moved));
        assert!(!matches(&any, &event(EventKind::CardCreated)));
    }

    #[test]
    fn field_changes_ignore_rank_only_drags() {
        let drag = event(EventKind::CardUpdated {
            changes: vec![change("rank", Some("a"), Some("b"))],
        });
        let edit = event(EventKind::CardUpdated {
            changes: vec![change("priority", Some("Low"), Some("High"))],
        });
        let any = Trigger::FieldChanged { fields: vec![] };
        let priority = Trigger::FieldChanged {
            fields: vec!["Priority".into()],
        };
        assert!(!matches(&any, &drag));
        assert!(matches(&any, &edit));
        assert!(matches(&priority, &edit));
        assert!(!matches(
            &Trigger::FieldChanged {
                fields: vec!["summary".into()]
            },
            &edit
        ));
    }

    #[test]
    fn assignment_means_someone_was_assigned_not_unassigned() {
        let assigned = event(EventKind::CardUpdated {
            changes: vec![change("assignee", None, Some("u1"))],
        });
        let unassigned = event(EventKind::CardUpdated {
            changes: vec![change("assignee", Some("u1"), None)],
        });
        assert!(matches(&Trigger::CardAssigned, &assigned));
        assert!(!matches(&Trigger::CardAssigned, &unassigned));
    }

    #[test]
    fn dates_accept_a_smart_instant_or_a_plain_date() {
        assert_eq!(
            parse_date("2026-01-17T09:00:00+00:00").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 17)
        );
        assert_eq!(
            parse_date("2026-01-17").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 17)
        );
        assert_eq!(parse_date("  ").unwrap(), None);
        assert!(parse_date("next week").is_err());
    }
}

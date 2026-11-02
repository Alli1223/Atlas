//! The tools Atlas exposes to a Claude Code session, and their execution.
//!
//! Every tool is scoped, at call time, to the session's one card and the authority of the
//! user who started the run. Nothing here trusts the tool arguments for authorisation: the
//! card is the session's card (resolved from the capability, never from an argument), and the
//! starting user's project role is re-checked on every call — so a run cannot outlive its
//! starter's access, reach another card, or touch another project.
//!
//! Moves go through [`card::execute_transition`], the exact path a human's "take this
//! transition" button uses, so an agent is bound by the same workflow conditions and
//! validators. There is no raw status write.

use serde_json::{Value, json};

use crate::auth::{now, user, user::User};
use crate::db::Db;
use crate::domain::agent_session::AgentSession;
use crate::domain::card::{self, Card, CardPatch};
use crate::domain::member::{self, ProjectRole};
use crate::domain::{comment, config, project, workflow};
use crate::error::{AppError, AppResult};

/// A resolved capability: the session, and everything a tool call needs to act within it.
///
/// Built once per JSON-RPC request from the bearer token, so the card and the acting user are
/// established before any tool argument is read.
pub struct ToolContext {
    pub db: Db,
    pub session: AgentSession,
    pub card: Card,
    /// The user who started the run. Loaded fresh each request, so a since-deactivated or
    /// since-downgraded account is caught here rather than trusted from session start.
    pub actor: User,
}

impl std::fmt::Debug for ToolContext {
    // Hand-written: `User` derives a `Debug` that would print its `password_hash`, and this
    // struct is not worth widening the blast radius of that. Only the identifying ids print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("session", &self.session.id)
            .field("card", &self.card.key)
            .field("actor", &self.actor.id)
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    /// Builds the context for a resolved capability's session, or the reason it cannot act.
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] if the starting user is gone or deactivated — the run has no
    /// authority to borrow. [`AppError::NotFound`] if the card was deleted mid-run.
    pub async fn build(db: &Db, session: AgentSession) -> AppResult<Self> {
        let card = card::find_by_id(db, &session.card_id)
            .await?
            .ok_or(AppError::NotFound)?;

        let actor_id = session.started_by.as_deref().ok_or(AppError::Forbidden)?;
        let actor = user::find_by_id(db, actor_id)
            .await?
            .filter(|u| u.is_active)
            .ok_or(AppError::Forbidden)?;

        Ok(Self {
            db: db.clone(),
            session,
            card,
            actor,
        })
    }

    /// Re-checks the starting user's project role at call time, returning the error the tool
    /// should surface. This is what makes authority track the *current* grant, not the grant
    /// at session start.
    async fn require(&self, min: ProjectRole) -> AppResult<()> {
        let project = project::find_by_id(&self.db, &self.card.project_id)
            .await?
            .ok_or(AppError::NotFound)?;
        member::require(&self.db, &project, &self.actor, min).await?;
        Ok(())
    }
}

/// The tool catalogue, in the shape `tools/list` returns: name, description, and JSON-Schema
/// input. Namespaced `atlas_*`; the CLI sees them as `mcp__atlas__atlas_*`.
#[must_use]
pub fn catalogue() -> Value {
    json!([
        {
            "name": "atlas_get_card",
            "description": "Read the card this session is working on: its key, summary, description, and current status.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "atlas_list_transitions",
            "description": "List the workflow transitions the card may legally take right now (conditions already evaluated). Call this before atlas_move_card to see the valid targets.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "atlas_move_card",
            "description": "Move the card to another status by taking a legal workflow transition. Give the target status name (to_status) or a transition id from atlas_list_transitions. Illegal moves are refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to_status": { "type": "string", "description": "The name of the status (or the transition) to move to." },
                    "transition_id": { "type": "string", "description": "A transition id from atlas_list_transitions." },
                    "comment": { "type": "string", "description": "An optional note recorded with the move." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "atlas_comment_card",
            "description": "Post a markdown comment on the card. Use this to report what you did, or why a task could not be completed. The comment is clearly attributed to the agent.",
            "inputSchema": {
                "type": "object",
                "properties": { "body": { "type": "string", "description": "The markdown comment body." } },
                "required": ["body"],
                "additionalProperties": false
            }
        },
        {
            "name": "atlas_set_status",
            "description": "Set the card's status by name — shorthand for atlas_move_card with to_status. Only a legal workflow move is taken.",
            "inputSchema": {
                "type": "object",
                "properties": { "status": { "type": "string", "description": "The target status name, e.g. \"In Review\"." } },
                "required": ["status"],
                "additionalProperties": false
            }
        },
        {
            "name": "atlas_create_card",
            "description": "Create a follow-up card as a child of this card (for work you found but did not do). Returns the new card's key.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "A one-line title." },
                    "description": { "type": "string", "description": "Markdown detail." }
                },
                "required": ["summary"],
                "additionalProperties": false
            }
        },
        {
            "name": "atlas_add_tag",
            "description": "Put a tag (a label with no spaces) on the card, creating it in the project if it does not exist yet.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "The tag, e.g. needs-review." } },
                "required": ["name"],
                "additionalProperties": false
            }
        }
    ])
}

/// A visible marker prefixed to every agent-written comment, so an audit of a card shows which
/// notes came from a Claude Code run rather than a person. Authorisation is the starting
/// user's; this makes the *authorship* legible, which the history row (a real user FK) cannot.
fn agent_prefix(session: &AgentSession) -> String {
    format!("🤖 _Claude Code session `{}`_\n\n", session.id)
}

/// Executes a tool call, returning the MCP `content` payload for a success.
///
/// # Errors
///
/// [`AppError`] mapped by the server into a JSON-RPC tool error.
pub async fn call(ctx: &ToolContext, name: &str, args: &Value) -> AppResult<Value> {
    // Every write a tool makes is tagged `agent:<session>`, so history, the activity feed and
    // automation can all tell an agent's change from its starter's own.
    let actor = format!("agent:{}", ctx.session.id);
    crate::activity::with_actor(actor, async {
        match name {
            "atlas_get_card" => get_card(ctx).await,
            "atlas_list_transitions" => list_transitions(ctx).await,
            "atlas_move_card" => move_card(ctx, args).await,
            "atlas_set_status" => {
                let status = args.get("status").and_then(Value::as_str).ok_or_else(|| {
                    AppError::BadRequest("atlas_set_status needs a status".to_owned())
                })?;
                move_card(ctx, &json!({ "to_status": status })).await
            }
            "atlas_comment_card" => comment_card(ctx, args).await,
            "atlas_create_card" => create_card(ctx, args).await,
            "atlas_add_tag" => add_tag(ctx, args).await,
            other => Err(AppError::BadRequest(format!("unknown tool {other:?}"))),
        }
    })
    .await
}

async fn create_card(ctx: &ToolContext, args: &Value) -> AppResult<Value> {
    ctx.require(ProjectRole::Member).await?;
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("atlas_create_card needs a summary".to_owned()))?;
    let summary = card::validate_summary(summary)?;
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .filter(|d| !d.trim().is_empty())
        .map(|d| card::validate_description(&format!("{}{d}", agent_prefix(&ctx.session))))
        .transpose()?;
    let parent = reload(ctx).await?;
    let project = project::find_by_id(&ctx.db, &parent.project_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // The highest level below the parent: a follow-up to a Story is a Task/Sub-task, not an
    // Epic. A card at the bottom of the hierarchy gets a sibling instead of a child.
    let types = config::card_types(&ctx.db, &project.id).await?;
    let parent_level = types
        .iter()
        .find(|t| t.id == parent.type_id)
        .map_or(0, |t| t.level);
    let (type_id, parent_id) = match types
        .iter()
        .filter(|t| t.level < parent_level)
        .max_by_key(|t| t.level)
    {
        Some(child) => (child.id.clone(), Some(parent.id.clone())),
        None => (parent.type_id.clone(), parent.parent_id.clone()),
    };
    let mut tx = ctx.db.begin_write().await?;
    let created = card::create(
        &mut tx,
        &project,
        &card::NewCard {
            type_id,
            parent_id,
            summary,
            description,
            status_id: None,
            priority_id: None,
            assignee_id: None,
            reporter_id: Some(ctx.actor.id.clone()),
            due_date: None,
            start_date: None,
            estimate: None,
            placement: card::Placement::Bottom,
        },
        &ctx.actor.id,
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(text(format!("Created {}.", created.key)))
}

async fn add_tag(ctx: &ToolContext, args: &Value) -> AppResult<Value> {
    ctx.require(ProjectRole::Member).await?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("atlas_add_tag needs a name".to_owned()))?;
    let name = crate::domain::tag::validate_name(name)?;
    let mut tx = ctx.db.begin_write().await?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM tags WHERE (project_id = ? OR project_id IS NULL) AND name = ? \
         ORDER BY project_id IS NULL LIMIT 1",
    )
    .bind(&ctx.card.project_id)
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await?;
    let tag_id = match existing {
        Some(id) => id,
        None => {
            crate::domain::tag::insert(&mut tx, Some(&ctx.card.project_id), &name, None, now())
                .await?
                .id
        }
    };
    crate::domain::tag::attach(&mut tx, &ctx.card.id, &tag_id, now()).await?;
    tx.commit().await?;
    Ok(text(format!("Tagged {} with {name}.", ctx.card.key)))
}

async fn get_card(ctx: &ToolContext) -> AppResult<Value> {
    ctx.require(ProjectRole::Viewer).await?;
    let card = reload(ctx).await?;
    let status = status_name(&ctx.db, &card.status_id).await?;
    Ok(text(format!(
        "{key}: {summary}\nstatus: {status}\n\n{description}",
        key = card.key,
        summary = card.summary,
        description = card.description.as_deref().unwrap_or("(no description)"),
    )))
}

async fn list_transitions(ctx: &ToolContext) -> AppResult<Value> {
    use std::fmt::Write as _;

    ctx.require(ProjectRole::Viewer).await?;
    let card = reload(ctx).await?;
    let moves = workflow::available_transitions(&ctx.db, &card, Some(&ctx.actor.id)).await?;
    if moves.is_empty() {
        return Ok(text(
            "No transitions are available from the card's current status.",
        ));
    }
    let mut lines = String::from("Available transitions:\n");
    for m in &moves {
        let target = status_name(&ctx.db, &m.to_status_id).await?;
        let _ = match &m.id {
            Some(id) => writeln!(
                lines,
                "- {} \u{2192} {} (transition_id: {id})",
                m.name, target
            ),
            None => writeln!(lines, "- {} \u{2192} {}", m.name, target),
        };
    }
    Ok(text(lines))
}

async fn move_card(ctx: &ToolContext, args: &Value) -> AppResult<Value> {
    ctx.require(ProjectRole::Member).await?;

    let to_status = args.get("to_status").and_then(Value::as_str);
    let transition_id = args.get("transition_id").and_then(Value::as_str);
    let extra_comment = args.get("comment").and_then(Value::as_str);
    if to_status.is_none() && transition_id.is_none() {
        return Err(AppError::BadRequest(
            "atlas_move_card needs a to_status or a transition_id".to_owned(),
        ));
    }

    let card = reload(ctx).await?;
    // Conditions are evaluated for the starting user, so a transition hidden from them is not
    // selectable here — the agent can only take a move that user could take by hand. This is
    // also the only source of legal moves: nothing that is not in this list can be chosen.
    let available = workflow::available_transitions(&ctx.db, &card, Some(&ctx.actor.id)).await?;
    let chosen = choose_transition(&ctx.db, &available, transition_id, to_status).await?;

    let note = extra_comment.map(|c| format!("{}{c}", agent_prefix(&ctx.session)));
    let now = now();
    let mut tx = ctx.db.begin_write().await?;
    // Re-load inside the write transaction: the card may have moved since we listed.
    let card = card::find_by_id_tx(&mut tx, &ctx.card.id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Two legal move paths, exactly as the board uses. Authority and history attribution are
    // the starting user's throughout; the visible comment marker carries the "an agent did
    // this" signal a user-FK history row cannot.
    let moved = if let Some(transition_id) = &chosen.id {
        // A configured workflow's edge: go through the full execution contract, which
        // re-verifies the edge and its gates — a raw or illegal move is impossible here.
        let transition = workflow::transition_by_id_tx(&mut tx, transition_id)
            .await?
            .ok_or(AppError::NotFound)?;
        card::execute_transition(
            &mut tx,
            &card,
            &transition,
            CardPatch::default(),
            note.as_deref(),
            Some(&ctx.actor.id),
            now,
        )
        .await?
    } else {
        // A permissive default workflow's synthetic move: the board's own move path, to a
        // target status that `available_transitions` already vetted as legal. A note, if any,
        // is a separate comment since this path takes no transition-screen comment.
        let moved = card::move_card(
            &mut tx,
            &card,
            &card::Drop {
                status_id: Some(chosen.to_status_id.clone()),
                previous_card_id: None,
                next_card_id: None,
            },
            Some(&ctx.actor.id),
            now,
        )
        .await?;
        if let Some(note) = &note {
            comment::insert(&mut tx, &card.id, &ctx.actor.id, note, now).await?;
        }
        moved
    };
    tx.commit().await?;

    let target = status_name(&ctx.db, &moved.status_id).await?;
    Ok(text(format!("Moved {} to {target}.", moved.key)))
}

/// Resolves the caller's intent to exactly one *available* move, matching (in order) a
/// transition id, a transition label, or a target-status name. Only moves from the available
/// set are reachable, so an illegal or condition-hidden move never resolves.
async fn choose_transition(
    db: &Db,
    available: &[workflow::AvailableTransition],
    transition_id: Option<&str>,
    to_status: Option<&str>,
) -> AppResult<workflow::AvailableTransition> {
    if let Some(id) = transition_id {
        return available
            .iter()
            .find(|t| t.id.as_deref() == Some(id))
            .cloned()
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "no legal transition {id:?}; call atlas_list_transitions"
                ))
            });
    }

    let name = to_status.expect("caller checked one of the two is set");

    // A transition label match first.
    if let Some(t) = available.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
        return Ok(t.clone());
    }
    // Then a target-status-name match.
    for t in available {
        if status_name(db, &t.to_status_id)
            .await?
            .eq_ignore_ascii_case(name)
        {
            return Ok(t.clone());
        }
    }
    Err(AppError::BadRequest(format!(
        "no legal transition matches {name:?}; call atlas_list_transitions"
    )))
}

async fn comment_card(ctx: &ToolContext, args: &Value) -> AppResult<Value> {
    ctx.require(ProjectRole::Member).await?;

    let body = args
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("atlas_comment_card needs a body".to_owned()))?;
    let body = comment::validate_body(body)?;
    let body = format!("{}{body}", agent_prefix(&ctx.session));

    let now = now();
    let mut tx = ctx.db.begin_write().await?;
    let created = comment::insert(&mut tx, &ctx.card.id, &ctx.actor.id, &body, now).await?;
    tx.commit().await?;

    Ok(text(format!("Comment posted ({}).", created.id)))
}

/// Re-reads the session's card so a mid-run edit is reflected. Always the session card, never
/// an argument-supplied one.
async fn reload(ctx: &ToolContext) -> AppResult<Card> {
    card::find_by_id(&ctx.db, &ctx.card.id)
        .await?
        .ok_or(AppError::NotFound)
}

/// A status's display name, for tool output. Falls back to the id if the status was deleted.
async fn status_name(db: &Db, status_id: &str) -> AppResult<String> {
    let mut conn = db.reader().acquire().await?;
    Ok(config::status_by_id_tx(conn.as_mut(), status_id)
        .await?
        .map_or_else(|| status_id.to_owned(), |s| s.name))
}

/// Wraps text in the MCP tool-result `content` shape.
fn text(body: impl Into<String>) -> Value {
    json!({ "content": [ { "type": "text", "text": body.into() } ] })
}

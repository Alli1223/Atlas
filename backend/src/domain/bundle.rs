//! A project as one JSON document (`TODO.md` Phase 18): export, and import as a new project.
//!
//! Round-tripping a bundle is three features at once: a **backup** of one project, a
//! **template** (import with `includeCards: false` and you get the configuration — hierarchy,
//! types, statuses, workflows with their gates, tags, components, milestones, the custom field
//! layout, automation rules, card templates — with none of the work), and docs/adr/0003's
//! **"copy config from project X"**.
//!
//! # Ids never cross the boundary
//!
//! Everything in a bundle refers to everything else *by name* (statuses, types, priorities)
//! or by a bundle-local reference (cards by their old key). Import builds an old-id → new-id
//! map as it creates rows, and gate configs — which do carry ids — have every known old id
//! rewritten to its new one. Ids are UUIDs, so the rewrite cannot hit anything else.
//!
//! History is not exported: a changelog describes *this* card's past, and inventing one for a
//! copy would be a lie. Comments are kept, attributed to their author when that account
//! exists on the importing instance and to the importer otherwise.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::db::Db;
use crate::domain::project::{self, NewProject, Project};
use crate::domain::{EstimationUnit, StatusCategory, card, config, custom_field, member, workflow};
use crate::error::{AppError, AppResult};

/// The format marker.
pub const FORMAT: &str = "atlas.project/1";

/// The whole document.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    /// Always [`FORMAT`].
    pub format: String,
    /// When it was exported.
    pub exported_at: DateTime<Utc>,
    /// The project's own settings.
    pub project: BundleProject,
    /// `(level, name)`.
    pub levels: Vec<(i64, String)>,
    /// Card types.
    pub card_types: Vec<BundleCardType>,
    /// Statuses, in board order.
    pub statuses: Vec<BundleNamed>,
    /// Priorities, most urgent first.
    pub priorities: Vec<BundleNamed>,
    /// Resolutions.
    pub resolutions: Vec<BundleNamed>,
    /// Workflows with their transitions and gates.
    pub workflows: Vec<BundleWorkflow>,
    /// Project tags.
    pub tags: Vec<BundleNamed>,
    /// Components.
    pub components: Vec<BundleNamed>,
    /// Milestones.
    pub milestones: Vec<BundleMilestone>,
    /// Custom field layout, with each field's definition.
    pub fields: Vec<BundleField>,
    /// Automation rules.
    #[schema(value_type = Vec<Object>)]
    pub rules: Vec<Value>,
    /// Card templates.
    #[schema(value_type = Vec<Object>)]
    pub card_templates: Vec<Value>,
    /// Cards, parents before children. Empty in a config-only bundle.
    pub cards: Vec<BundleCard>,
    /// Card links, between bundle cards: `(from, to, link type)`.
    pub links: Vec<(String, String, String)>,
    /// Every config id the source project's gate configs may mention, as `kind:name`
    /// (`resolution:Done`, `priority:High`, `status:Review`, `cardType:Bug`), so import can
    /// rewrite them to the new project's ids.
    #[serde(default)]
    pub refs: HashMap<String, String>,
}

/// The project's settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleProject {
    /// Its key at export.
    pub key: String,
    /// Its name.
    pub name: String,
    /// Its description.
    pub description: Option<String>,
    /// The template it was made from.
    pub template: String,
    /// Cycles on or off.
    pub cycles_enabled: bool,
    /// The estimation unit.
    pub estimation_unit: String,
}

/// A named config row; `extra` carries what differs per table (category, rank, colour…).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleNamed {
    /// The name.
    pub name: String,
    /// Table-specific fields.
    #[schema(value_type = Object)]
    #[serde(default)]
    pub extra: Value,
}

/// A card type.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleCardType {
    /// Its name.
    pub name: String,
    /// Its icon.
    pub icon: Option<String>,
    /// Its colour.
    pub colour: Option<String>,
    /// Its hierarchy level.
    pub level: i64,
    /// The default type.
    pub is_default: bool,
}

/// A workflow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleWorkflow {
    /// Its name.
    pub name: String,
    /// The permissive default.
    pub is_default: bool,
    /// Status names in it.
    pub statuses: Vec<String>,
    /// Card type names it governs.
    pub card_types: Vec<String>,
    /// Transitions.
    pub transitions: Vec<BundleTransition>,
}

/// A transition and its gates.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleTransition {
    /// Its button label.
    pub name: String,
    /// The source status name; `None` = from anywhere.
    pub from: Option<String>,
    /// The target status name.
    pub to: String,
    /// `(kind, config)` conditions.
    pub conditions: Vec<(String, String)>,
    /// `(kind, config)` validators.
    pub validators: Vec<(String, String)>,
    /// `(kind, config)` post-functions, in order.
    pub post_functions: Vec<(String, String)>,
}

/// A milestone.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleMilestone {
    /// Its name.
    pub name: String,
    /// Its description.
    pub description: Option<String>,
    /// Start.
    pub start_date: Option<NaiveDate>,
    /// Release.
    pub release_date: Option<NaiveDate>,
}

/// A laid-out custom field.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleField {
    /// The field's name (global).
    pub name: String,
    /// Its type.
    pub field_type: String,
    /// Its options.
    pub options: Vec<String>,
    /// The card type it applies to, by name; `None` = all.
    pub card_type: Option<String>,
    /// Required.
    pub required: bool,
    /// Hidden.
    pub hidden: bool,
    /// Default value.
    #[schema(value_type = Object)]
    pub default_value: Option<Value>,
}

/// A card.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleCard {
    /// Its key at export — the bundle-local reference.
    pub reference: String,
    /// Its parent's reference.
    pub parent: Option<String>,
    /// Type name.
    pub card_type: String,
    /// Status name.
    pub status: String,
    /// Priority name.
    pub priority: Option<String>,
    /// Summary.
    pub summary: String,
    /// Description.
    pub description: Option<String>,
    /// Due date.
    pub due_date: Option<NaiveDate>,
    /// Start date.
    pub start_date: Option<NaiveDate>,
    /// Estimate.
    pub estimate: Option<f64>,
    /// Tag names.
    pub labels: Vec<String>,
    /// Component names.
    pub components: Vec<String>,
    /// Milestone name.
    pub milestone: Option<String>,
    /// Custom values by field name.
    #[schema(value_type = Object)]
    pub fields: HashMap<String, Value>,
    /// Comments: `(author username, markdown, created)`.
    pub comments: Vec<(Option<String>, String, DateTime<Utc>)>,
}

#[derive(Debug, FromRow)]
struct CardRow {
    id: String,
    key: String,
    parent_key: Option<String>,
    type_name: String,
    status_name: String,
    priority_name: Option<String>,
    summary: String,
    description: Option<String>,
    due_date: Option<NaiveDate>,
    start_date: Option<NaiveDate>,
    estimate: Option<f64>,
    milestone_name: Option<String>,
}

/// Exports a project. `include_cards: false` makes a template.
// One block per table, in dependency order — the bundle's shape, written out.
#[allow(clippy::too_many_lines)]
pub async fn export(
    db: &Db,
    project: &Project,
    include_cards: bool,
    now: DateTime<Utc>,
) -> AppResult<Bundle> {
    let r = db.reader();
    let levels: Vec<(i64, String)> = sqlx::query_as(
        "SELECT level, name FROM hierarchy_levels WHERE project_id = ? ORDER BY level DESC",
    )
    .bind(&project.id)
    .fetch_all(r)
    .await?;
    let card_types = config::card_types(db, &project.id)
        .await?
        .into_iter()
        .map(|t| BundleCardType {
            name: t.name,
            icon: t.icon,
            colour: t.colour,
            level: t.level,
            is_default: t.is_default,
        })
        .collect();
    let statuses = config::statuses(db, &project.id)
        .await?
        .into_iter()
        .map(|s| BundleNamed {
            name: s.name,
            extra: serde_json::json!({ "category": s.category, "position": s.position }),
        })
        .collect();
    let priorities = config::priorities(db, &project.id)
        .await?
        .into_iter()
        .map(|p| BundleNamed {
            name: p.name,
            extra: serde_json::json!({ "rank": p.rank, "icon": p.icon, "colour": p.colour }),
        })
        .collect();
    let resolutions = config::resolutions(db, &project.id)
        .await?
        .into_iter()
        .map(|x| BundleNamed {
            name: x.name,
            extra: serde_json::json!({ "position": x.position }),
        })
        .collect();
    let status_name: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT id, name FROM statuses WHERE project_id = ?")
            .bind(&project.id)
            .fetch_all(r)
            .await?
            .into_iter()
            .collect();
    let type_name: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT id, name FROM card_types WHERE project_id = ?",
    )
    .bind(&project.id)
    .fetch_all(r)
    .await?
    .into_iter()
    .collect();
    let mut refs: HashMap<String, String> = HashMap::new();
    for (sql, kind) in [
        (
            "SELECT id, name FROM statuses WHERE project_id = ?",
            "status",
        ),
        (
            "SELECT id, name FROM card_types WHERE project_id = ?",
            "cardType",
        ),
        (
            "SELECT id, name FROM priorities WHERE project_id = ?",
            "priority",
        ),
        (
            "SELECT id, name FROM resolutions WHERE project_id = ?",
            "resolution",
        ),
    ] {
        for (id, name) in sqlx::query_as::<_, (String, String)>(sql)
            .bind(&project.id)
            .fetch_all(r)
            .await?
        {
            refs.insert(id, format!("{kind}:{name}"));
        }
    }

    let mut workflows = Vec::new();
    for wf in workflow::list(db, &project.id).await? {
        let statuses = workflow::status_ids(db, &wf.id)
            .await?
            .iter()
            .filter_map(|id| status_name.get(id).cloned())
            .collect();
        let card_types = workflow::card_type_ids(db, &wf.id)
            .await?
            .iter()
            .filter_map(|id| type_name.get(id).cloned())
            .collect();
        let mut transitions = Vec::new();
        for t in workflow::transitions(db, &wf.id).await? {
            let gates = |rows: Vec<workflow::GateRow>| {
                rows.into_iter()
                    .map(|g| (g.kind, g.config))
                    .collect::<Vec<_>>()
            };
            transitions.push(BundleTransition {
                name: t.name.clone(),
                from: t
                    .from_status_id
                    .as_ref()
                    .and_then(|id| status_name.get(id).cloned()),
                to: status_name
                    .get(&t.to_status_id)
                    .cloned()
                    .unwrap_or_default(),
                conditions: gates(workflow::conditions_of(db, &t.id).await?),
                validators: gates(workflow::validators_of(db, &t.id).await?),
                post_functions: gates(workflow::post_functions_of(db, &t.id).await?),
            });
        }
        workflows.push(BundleWorkflow {
            name: wf.name,
            is_default: wf.is_default,
            statuses,
            card_types,
            transitions,
        });
    }

    let tags = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT name, colour FROM tags WHERE project_id = ? ORDER BY name",
    )
    .bind(&project.id)
    .fetch_all(r)
    .await?
    .into_iter()
    .map(|(name, colour)| BundleNamed {
        name,
        extra: serde_json::json!({ "colour": colour }),
    })
    .collect();
    let components = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT name, description FROM components WHERE project_id = ? ORDER BY name",
    )
    .bind(&project.id)
    .fetch_all(r)
    .await?
    .into_iter()
    .map(|(name, description)| BundleNamed {
        name,
        extra: serde_json::json!({ "description": description }),
    })
    .collect();
    let milestones = sqlx::query_as::<_, (String, Option<String>, Option<NaiveDate>, Option<NaiveDate>)>("SELECT name, description, start_date, release_date FROM milestones WHERE project_id = ? ORDER BY name").bind(&project.id).fetch_all(r).await?
        .into_iter().map(|(name, description, start_date, release_date)| BundleMilestone { name, description, start_date, release_date }).collect();
    let fields = custom_field::layout(db, &project.id)
        .await?
        .into_iter()
        .map(|e| BundleField {
            name: e.field.name,
            field_type: e.field.field_type.as_str().to_owned(),
            options: e.field.options,
            card_type: e
                .card_type_id
                .as_ref()
                .and_then(|id| type_name.get(id).cloned()),
            required: e.required,
            hidden: e.hidden,
            default_value: e.default_value,
        })
        .collect();
    let rules = crate::automation::store::list(db, &project.id).await?.into_iter().map(|rule| serde_json::json!({
        "name": rule.name, "description": rule.description, "enabled": rule.enabled, "spec": rule.spec,
    })).collect();
    let card_templates = crate::domain::card_template::list(db, &project.id)
        .await?
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "cardType": t.card_type_id.as_ref().and_then(|id| type_name.get(id).cloned()),
                "summary": t.summary, "description": t.description, "labels": t.labels,
            })
        })
        .collect();

    let mut cards = Vec::new();
    let mut links = Vec::new();
    if include_cards {
        let rows = sqlx::query_as::<_, CardRow>(
            "SELECT c.id, c.key, p.key AS parent_key, t.name AS type_name, s.name AS status_name, \
                    pr.name AS priority_name, c.summary, c.description, c.due_date, c.start_date, \
                    c.estimate, m.name AS milestone_name \
               FROM cards c JOIN card_types t ON t.id = c.type_id JOIN statuses s ON s.id = c.status_id \
               LEFT JOIN cards p ON p.id = c.parent_id LEFT JOIN priorities pr ON pr.id = c.priority_id \
               LEFT JOIN milestones m ON m.id = c.milestone_id \
              WHERE c.project_id = ? AND c.deleted_at IS NULL ORDER BY t.level DESC, c.rank",
        )
        .bind(&project.id)
        .fetch_all(r)
        .await?;
        let field_names: HashMap<String, String> = custom_field::list(db)
            .await?
            .into_iter()
            .map(|f| (f.id, f.name))
            .collect();
        for row in rows {
            let labels = sqlx::query_scalar("SELECT t.name FROM card_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.card_id = ? ORDER BY t.name").bind(&row.id).fetch_all(r).await?;
            let components = sqlx::query_scalar("SELECT k.name FROM card_components cc JOIN components k ON k.id = cc.component_id WHERE cc.card_id = ? ORDER BY k.name").bind(&row.id).fetch_all(r).await?;
            let fields = custom_field::values(db, &row.id)
                .await?
                .into_iter()
                .filter_map(|(id, v)| field_names.get(&id).map(|n| (n.clone(), v)))
                .collect();
            let comments = sqlx::query_as::<_, (Option<String>, String, DateTime<Utc>)>("SELECT u.username, m.body, m.created_at FROM comments m LEFT JOIN users u ON u.id = m.author_id WHERE m.card_id = ? ORDER BY m.created_at").bind(&row.id).fetch_all(r).await?;
            let outbound: Vec<(String, String)> = sqlx::query_as("SELECT o.key, l.link_type FROM card_links l JOIN cards o ON o.id = l.to_card_id WHERE l.from_card_id = ? AND o.project_id = ?").bind(&row.id).bind(&project.id).fetch_all(r).await?;
            for (to, kind) in outbound {
                links.push((row.key.clone(), to, kind));
            }
            cards.push(BundleCard {
                reference: row.key,
                parent: row.parent_key,
                card_type: row.type_name,
                status: row.status_name,
                priority: row.priority_name,
                summary: row.summary,
                description: row.description,
                due_date: row.due_date,
                start_date: row.start_date,
                estimate: row.estimate,
                labels,
                components,
                milestone: row.milestone_name,
                fields,
                comments,
            });
        }
    }

    Ok(Bundle {
        format: FORMAT.to_owned(),
        exported_at: now,
        project: BundleProject {
            key: project.key.clone(),
            name: project.name.clone(),
            description: project.description.clone(),
            template: project.template.clone(),
            cycles_enabled: project.cycles_enabled,
            estimation_unit: project.estimation_unit.to_string(),
        },
        levels,
        card_types,
        statuses,
        priorities,
        resolutions,
        workflows,
        tags,
        components,
        milestones,
        fields,
        rules,
        card_templates,
        cards,
        links,
        refs,
    })
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Creates a new project from a bundle, owned by `creator`.
// One block per table, in dependency order — the mirror of `export`.
#[allow(clippy::too_many_lines)]
pub async fn import(
    db: &Db,
    bundle: &Bundle,
    key: &str,
    name: &str,
    include_cards: bool,
    creator: &crate::auth::User,
    now: DateTime<Utc>,
) -> AppResult<Project> {
    if bundle.format != FORMAT {
        return Err(AppError::Validation(format!(
            "Not an Atlas project bundle (format {:?}).",
            bundle.format
        )));
    }
    let key = project::validate_key(key)?;
    let name = project::validate_name(name)?;
    let mut tx = db.begin_write().await?;
    if project::key_taken(&mut tx, &key, None).await? {
        return Err(AppError::Conflict(format!(
            "A project with key {key} already exists."
        )));
    }
    let estimation_unit: EstimationUnit = bundle
        .project
        .estimation_unit
        .parse()
        .unwrap_or(EstimationUnit::None);
    let created = project::insert(
        &mut tx,
        &NewProject {
            key: key.clone(),
            name,
            description: bundle.project.description.clone(),
            lead_id: Some(creator.id.clone()),
            template: bundle.project.template.clone(),
            cycles_enabled: bundle.project.cycles_enabled,
            estimation_unit,
        },
        now,
    )
    .await?;
    member::insert_or_ignore(
        &mut tx,
        &created.id,
        &creator.id,
        member::ProjectRole::Owner,
        Some(&creator.id),
        now,
    )
    .await?;

    for (level, level_name) in &bundle.levels {
        config::insert_level(&mut tx, &created.id, *level, level_name).await?;
    }
    let mut types: HashMap<String, String> = HashMap::new();
    for t in &bundle.card_types {
        let row = config::insert_card_type(
            &mut tx,
            &created.id,
            &t.name,
            t.icon.as_deref(),
            t.colour.as_deref(),
            t.level,
            t.is_default,
        )
        .await?;
        types.insert(t.name.clone(), row.id);
    }
    let mut statuses: HashMap<String, String> = HashMap::new();
    for (index, s) in bundle.statuses.iter().enumerate() {
        let category: StatusCategory = str_of(&s.extra, "category")
            .and_then(|c| c.parse().ok())
            .unwrap_or(StatusCategory::Todo);
        let position = s
            .extra
            .get("position")
            .and_then(Value::as_i64)
            .unwrap_or(i64::try_from(index).unwrap_or(0) + 1);
        let row = config::insert_status(&mut tx, &created.id, &s.name, category, position).await?;
        statuses.insert(s.name.clone(), row.id);
    }
    let mut priorities: HashMap<String, String> = HashMap::new();
    for (index, p) in bundle.priorities.iter().enumerate() {
        let rank = p
            .extra
            .get("rank")
            .and_then(Value::as_i64)
            .unwrap_or(i64::try_from(index).unwrap_or(0) + 1);
        let row = config::insert_priority(
            &mut tx,
            &created.id,
            &p.name,
            str_of(&p.extra, "icon"),
            str_of(&p.extra, "colour"),
            rank,
        )
        .await?;
        priorities.insert(p.name.clone(), row.id);
    }
    let mut resolutions: HashMap<String, String> = HashMap::new();
    for (index, x) in bundle.resolutions.iter().enumerate() {
        let position = x
            .extra
            .get("position")
            .and_then(Value::as_i64)
            .unwrap_or(i64::try_from(index).unwrap_or(0) + 1);
        let row = config::insert_resolution(&mut tx, &created.id, &x.name, position).await?;
        resolutions.insert(x.name.clone(), row.id);
    }

    // Gate configs carry ids of the *source* project's rows. `refs` names each of them, so
    // every one this project now has a namesake for is rewritten; ids are UUIDs, so the
    // replacement cannot hit anything else. User ids are instance-wide and stay as they are.
    let mut ids: Vec<(String, String)> = Vec::new();
    for (old, reference) in &bundle.refs {
        let Some((kind, name)) = reference.split_once(':') else {
            continue;
        };
        let map = match kind {
            "status" => &statuses,
            "cardType" => &types,
            "priority" => &priorities,
            "resolution" => &resolutions,
            _ => continue,
        };
        if let Some(new) = map.get(name) {
            ids.push((old.clone(), new.clone()));
        }
    }
    let rewrite = |config: &str| -> String {
        ids.iter().fold(config.to_owned(), |out, (old, new)| {
            out.replace(old.as_str(), new)
        })
    };
    if bundle.workflows.is_empty() {
        workflow::seed_default(&mut tx, &created.id, now).await?;
    }
    for wf in &bundle.workflows {
        let status_ids: Vec<String> = wf
            .statuses
            .iter()
            .filter_map(|n| statuses.get(n).cloned())
            .collect();
        let row = workflow::insert(
            &mut tx,
            &created.id,
            &wf.name,
            wf.is_default,
            &status_ids,
            now,
        )
        .await?;
        let type_ids: Vec<String> = wf
            .card_types
            .iter()
            .filter_map(|n| types.get(n).cloned())
            .collect();
        if !type_ids.is_empty() {
            workflow::assign_card_types(&mut tx, &row.id, &created.id, &type_ids).await?;
        }
        for (position, t) in wf.transitions.iter().enumerate() {
            let Some(to) = statuses.get(&t.to) else {
                continue;
            };
            let from = t.from.as_ref().and_then(|n| statuses.get(n));
            let transition = workflow::insert_transition(
                &mut tx,
                &row.id,
                &t.name,
                from.map(String::as_str),
                to,
                i64::try_from(position).unwrap_or(0),
            )
            .await?;
            for (kind, config) in &t.conditions {
                workflow::add_condition(&mut tx, &transition.id, kind, &rewrite(config)).await?;
            }
            for (kind, config) in &t.validators {
                workflow::add_validator(&mut tx, &transition.id, kind, &rewrite(config)).await?;
            }
            for (order, (kind, config)) in t.post_functions.iter().enumerate() {
                workflow::add_post_function(
                    &mut tx,
                    &transition.id,
                    kind,
                    &rewrite(config),
                    i64::try_from(order).unwrap_or(0),
                )
                .await?;
            }
        }
    }
    for t in &bundle.tags {
        let colour = str_of(&t.extra, "colour").and_then(|c| c.parse().ok());
        crate::domain::tag::insert(&mut tx, Some(&created.id), &t.name, colour, now).await?;
    }
    for c in &bundle.components {
        crate::domain::planning::insert_component(
            &mut tx,
            &created.id,
            &c.name,
            str_of(&c.extra, "description"),
            None,
            now,
        )
        .await?;
    }
    let mut milestones: HashMap<String, String> = HashMap::new();
    for m in &bundle.milestones {
        let row = crate::domain::planning::insert_milestone(
            &mut tx,
            &created.id,
            &m.name,
            &crate::domain::planning::MilestonePatch {
                description: Some(m.description.clone()),
                start_date: Some(m.start_date),
                release_date: Some(m.release_date),
                ..crate::domain::planning::MilestonePatch::default()
            },
            now,
        )
        .await?;
        milestones.insert(m.name.clone(), row.id);
    }

    // Custom fields are global: reuse a same-named field of the same type, else define one.
    let mut field_ids: HashMap<String, String> = HashMap::new();
    let mut layout = Vec::new();
    for f in &bundle.fields {
        let field_type = custom_field::FieldType::parse(&f.field_type).ok_or_else(|| {
            AppError::Validation(format!("{:?} is not a field type.", f.field_type))
        })?;
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT id, field_type FROM custom_fields WHERE name = ?")
                .bind(&f.name)
                .fetch_optional(&mut *tx)
                .await?;
        let id = match existing {
            Some((id, kind)) if kind == f.field_type => id,
            Some(_) => {
                return Err(AppError::Conflict(format!(
                    "This instance already has a field {:?} of a different type.",
                    f.name
                )));
            }
            None => {
                custom_field::insert(
                    &mut tx,
                    &custom_field::validate_name(&f.name)?,
                    field_type,
                    None,
                    &custom_field::validate_options(field_type, &f.options)?,
                    now,
                )
                .await?
                .id
            }
        };
        field_ids.insert(f.name.clone(), id.clone());
        layout.push(custom_field::LayoutInput {
            field_id: id,
            card_type_id: f.card_type.as_ref().and_then(|n| types.get(n).cloned()),
            required: f.required,
            hidden: f.hidden,
            default_value: f.default_value.clone(),
        });
    }
    let users: Vec<String> = sqlx::query_scalar("SELECT id FROM users")
        .fetch_all(&mut *tx)
        .await?;
    custom_field::replace_layout(&mut tx, &created.id, &layout, &|id| {
        users.iter().any(|u| u == id)
    })
    .await?;

    for rule in &bundle.rules {
        let spec: crate::automation::model::RuleSpec = serde_json::from_value(rule["spec"].clone())
            .map_err(|err| {
                AppError::Validation(format!("A rule in the bundle is invalid: {err}"))
            })?;
        spec.validate()?;
        crate::automation::store::insert(
            &mut tx,
            &created.id,
            &creator.id,
            &crate::automation::store::RuleInput {
                name: str_of(rule, "name").unwrap_or("Imported rule"),
                description: str_of(rule, "description"),
                enabled: rule["enabled"].as_bool().unwrap_or(true),
                spec: &spec,
            },
            now,
        )
        .await?;
    }
    for template in &bundle.card_templates {
        crate::domain::card_template::insert(
            &mut tx,
            &created.id,
            &crate::domain::card_template::TemplateInput {
                name: str_of(template, "name").unwrap_or("Template").to_owned(),
                card_type_id: str_of(template, "cardType").and_then(|n| types.get(n).cloned()),
                summary: str_of(template, "summary").unwrap_or_default().to_owned(),
                description: str_of(template, "description").map(str::to_owned),
                priority_id: None,
                labels: template["labels"]
                    .as_array()
                    .map(|l| {
                        l.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            now,
        )
        .await?;
    }

    if include_cards {
        let mut by_reference: HashMap<String, String> = HashMap::new();
        for c in &bundle.cards {
            let type_id = types.get(&c.card_type).cloned().ok_or_else(|| {
                AppError::Validation(format!(
                    "Card {}: unknown type {:?}.",
                    c.reference, c.card_type
                ))
            })?;
            let made = card::create(
                &mut tx,
                &created,
                &card::NewCard {
                    type_id,
                    parent_id: c.parent.as_ref().and_then(|p| by_reference.get(p).cloned()),
                    summary: c.summary.clone(),
                    description: c.description.clone(),
                    status_id: statuses.get(&c.status).cloned(),
                    priority_id: c.priority.as_ref().and_then(|p| priorities.get(p).cloned()),
                    assignee_id: None,
                    reporter_id: None,
                    due_date: c.due_date,
                    start_date: c.start_date,
                    estimate: c.estimate,
                    placement: card::Placement::Bottom,
                },
                &creator.id,
                now,
            )
            .await
            .map_err(|err| AppError::Validation(format!("Card {}: {err}", c.reference)))?;
            by_reference.insert(c.reference.clone(), made.id.clone());
            for label in &c.labels {
                let id: Option<String> =
                    sqlx::query_scalar("SELECT id FROM tags WHERE project_id = ? AND name = ?")
                        .bind(&created.id)
                        .bind(label)
                        .fetch_optional(&mut *tx)
                        .await?;
                let id = match id {
                    Some(id) => id,
                    None => {
                        crate::domain::tag::insert(&mut tx, Some(&created.id), label, None, now)
                            .await?
                            .id
                    }
                };
                crate::domain::tag::attach(&mut tx, &made.id, &id, now).await?;
            }
            for component in &c.components {
                sqlx::query("INSERT OR IGNORE INTO card_components (card_id, component_id) SELECT ?, id FROM components WHERE project_id = ? AND name = ?").bind(&made.id).bind(&created.id).bind(component).execute(&mut *tx).await?;
            }
            if let Some(id) = c.milestone.as_ref().and_then(|m| milestones.get(m)) {
                sqlx::query("UPDATE cards SET milestone_id = ? WHERE id = ?")
                    .bind(id)
                    .bind(&made.id)
                    .execute(&mut *tx)
                    .await?;
            }
            for (field, value) in &c.fields {
                if let Some(id) = field_ids.get(field) {
                    custom_field::set_value(&mut tx, &made.id, id, Some(value)).await?;
                }
            }
            for (author, body, at) in &c.comments {
                let author_id: Option<String> = match author {
                    Some(username) => {
                        sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
                            .bind(username)
                            .fetch_optional(&mut *tx)
                            .await?
                    }
                    None => None,
                };
                crate::domain::comment::insert(
                    &mut tx,
                    &made.id,
                    author_id.as_deref().unwrap_or(&creator.id),
                    body,
                    *at,
                )
                .await?;
            }
        }
        for (from, to, kind) in &bundle.links {
            if let (Some(a), Some(b), Some(kind)) = (
                by_reference.get(from),
                by_reference.get(to),
                crate::domain::links::LinkType::parse(kind),
            ) && a != b
            {
                crate::domain::links::link_cards(&mut tx, a, b, kind, now).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_marker_is_versioned() {
        assert!(FORMAT.ends_with("/1"));
    }
}

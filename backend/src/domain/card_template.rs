//! `card_templates`: a pre-filled "create card" per project.
//!
//! Applying a template is the client's job (it pre-fills the create form, so the user still
//! sees and edits everything before it exists); the server stores and seeds them. Each project
//! template seeds the ones its domain wants — see [`presets`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::template::Template;
use crate::error::{AppError, AppResult};

/// A card template.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardTemplate {
    /// UUID v7.
    pub id: String,
    /// The project.
    pub project_id: String,
    /// Unique within the project.
    pub name: String,
    /// The card type to create.
    pub card_type_id: Option<String>,
    /// A summary to start from (may be empty).
    pub summary: String,
    /// Markdown — typically a `- [ ]` checklist.
    pub description: Option<String>,
    /// A priority.
    pub priority_id: Option<String>,
    /// Tags to apply.
    pub labels: Vec<String>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct Row {
    id: String,
    project_id: String,
    name: String,
    card_type_id: Option<String>,
    summary: String,
    description: Option<String>,
    priority_id: Option<String>,
    labels: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<Row> for CardTemplate {
    fn from(row: Row) -> Self {
        Self {
            labels: serde_json::from_str(&row.labels).unwrap_or_default(),
            id: row.id,
            project_id: row.project_id,
            name: row.name,
            card_type_id: row.card_type_id,
            summary: row.summary,
            description: row.description,
            priority_id: row.priority_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// The fields of a template a create or edit sets.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateInput {
    /// Unique within the project.
    pub name: String,
    /// A card type of the project.
    #[serde(default)]
    pub card_type_id: Option<String>,
    /// A summary to start from.
    #[serde(default)]
    pub summary: String,
    /// Markdown.
    #[serde(default)]
    pub description: Option<String>,
    /// A priority of the project.
    #[serde(default)]
    pub priority_id: Option<String>,
    /// Tag names.
    #[serde(default)]
    pub labels: Vec<String>,
}

/// A project's card templates, by name.
pub async fn list(db: &Db, project_id: &str) -> AppResult<Vec<CardTemplate>> {
    Ok(sqlx::query_as::<_, Row>(
        "SELECT id, project_id, name, card_type_id, summary, description, priority_id, labels, \
         created_at, updated_at FROM card_templates WHERE project_id = ? ORDER BY name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?
    .into_iter()
    .map(CardTemplate::from)
    .collect())
}

/// Finds one.
pub async fn find(db: &Db, id: &str) -> AppResult<Option<CardTemplate>> {
    Ok(sqlx::query_as::<_, Row>(
        "SELECT id, project_id, name, card_type_id, summary, description, priority_id, labels, \
         created_at, updated_at FROM card_templates WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.reader())
    .await?
    .map(CardTemplate::from))
}

/// Checks a template against its project.
pub async fn validate(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    input: &TemplateInput,
) -> AppResult<()> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::Validation(
            "A template name must be 1 to 80 characters.".to_owned(),
        ));
    }
    if input.summary.chars().count() > crate::domain::card::MAX_SUMMARY {
        return Err(AppError::Validation(
            "The template's summary is too long.".to_owned(),
        ));
    }
    if let Some(description) = &input.description {
        crate::domain::card::validate_description(description)?;
    }
    for label in &input.labels {
        crate::domain::tag::validate_name(label)?;
    }
    for (sql, id, what) in [
        (
            "SELECT project_id FROM card_types WHERE id = ?",
            &input.card_type_id,
            "card type",
        ),
        (
            "SELECT project_id FROM priorities WHERE id = ?",
            &input.priority_id,
            "priority",
        ),
    ] {
        if let Some(id) = id {
            let owner: Option<String> = sqlx::query_scalar(sql)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            if owner.as_deref() != Some(project_id) {
                return Err(AppError::Validation(format!(
                    "{id:?} is not a {what} of this project."
                )));
            }
        }
    }
    Ok(())
}

fn conflict(err: sqlx::Error, name: &str) -> AppError {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict(format!("A template called {name:?} already exists."))
        }
        _ => err.into(),
    }
}

/// Creates a template. Must already be validated.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    input: &TemplateInput,
    now: DateTime<Utc>,
) -> AppResult<String> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO card_templates (id, project_id, name, card_type_id, summary, description, \
         priority_id, labels, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(input.name.trim())
    .bind(&input.card_type_id)
    .bind(input.summary.trim())
    .bind(&input.description)
    .bind(&input.priority_id)
    .bind(serde_json::to_string(&input.labels).map_err(AppError::internal)?)
    .bind(to_sql_timestamp(now))
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await
    .map_err(|err| conflict(err, &input.name))?;
    Ok(id)
}

/// Replaces a template. Must already be validated.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    input: &TemplateInput,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE card_templates SET name = ?, card_type_id = ?, summary = ?, description = ?, \
         priority_id = ?, labels = ?, updated_at = ? WHERE id = ?",
    )
    .bind(input.name.trim())
    .bind(&input.card_type_id)
    .bind(input.summary.trim())
    .bind(&input.description)
    .bind(&input.priority_id)
    .bind(serde_json::to_string(&input.labels).map_err(AppError::internal)?)
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|err| conflict(err, &input.name))?;
    Ok(())
}

/// Deletes a template.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM card_templates WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// `(name, card type name, description)` per project template.
pub fn presets(template: Template) -> &'static [(&'static str, &'static str, &'static str)] {
    match template {
        Template::JobSearch => &[(
            "Application",
            "Application",
            "## Prep\n- [ ] Tailor CV to the role\n- [ ] Write the cover letter\n- [ ] Research the team\n\n## After applying\n- [ ] Note the date applied\n- [ ] Follow up after a week\n",
        )],
        Template::ThreeDModeling => &[(
            "Asset",
            "Model",
            "## Pipeline\n- [ ] Reference gathered\n- [ ] Blockout\n- [ ] High poly\n- [ ] Retopology\n- [ ] UV unwrap\n- [ ] Bake\n- [ ] Texture\n- [ ] Export\n",
        )],
        Template::Programming => &[(
            "Bug report",
            "Bug",
            "## Steps to reproduce\n1. \n\n## Expected\n\n## Actual\n\n## Done when\n- [ ] Fix merged\n- [ ] Regression test added\n",
        )],
        Template::Blank => &[],
    }
}

/// Seeds a project template's card templates.
pub async fn seed(
    tx: &mut sqlx::SqliteConnection,
    template: Template,
    project_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    for (name, type_name, description) in presets(template) {
        let type_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM card_types WHERE project_id = ? AND name = ?")
                .bind(project_id)
                .bind(type_name)
                .fetch_optional(&mut *tx)
                .await?;
        insert(
            tx,
            project_id,
            &TemplateInput {
                name: (*name).to_owned(),
                card_type_id: type_id,
                summary: String::new(),
                description: Some((*description).to_owned()),
                priority_id: None,
                labels: Vec::new(),
            },
            now,
        )
        .await?;
    }
    Ok(())
}

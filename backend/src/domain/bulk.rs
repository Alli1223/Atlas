//! Bulk operations (`TODO.md` Phase 18): edit, transition, delete, restore, archive, move,
//! tag and untag up to [`MAX_CARDS`] cards at once — **preview before apply**.
//!
//! # Preview is the real thing, rolled back
//!
//! A preview runs every card's operation through exactly the code apply uses — the workflow
//! engine, validators, resolution rules, hierarchy guards — inside one transaction, and then
//! rolls it back. So "this card will fail because Done requires a resolution" is the domain's
//! own answer, not a guess by a second, simplified rule set that could disagree with it.
//!
//! Apply runs each card in its own transaction: one card a workflow refuses does not undo the
//! other 999 — and the preview already showed which one it would be.
//!
//! # Access
//!
//! Every card is checked against the caller's role in its own project (Member to change it).
//! A card the caller cannot see is reported as "not found", exactly as a direct request
//! would be — a bulk request is not a way to probe for keys.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::User;
use crate::db::Db;
use crate::domain::card::{self, Card, CardPatch};
use crate::domain::member::{self, ProjectRole};
use crate::domain::project;
use crate::domain::tag;
use crate::error::{AppError, AppResult};

/// The most cards one bulk request may touch.
pub const MAX_CARDS: usize = 1000;

/// What to do to every card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Operation {
    /// Set fields, by name. Absent fields are left alone; an empty string clears.
    #[serde(rename_all = "camelCase")]
    Edit {
        /// A priority name.
        #[serde(default)]
        priority: Option<String>,
        /// A username.
        #[serde(default)]
        assignee: Option<String>,
        /// `YYYY-MM-DD`.
        #[serde(default)]
        due_date: Option<String>,
        /// A milestone name.
        #[serde(default)]
        milestone: Option<String>,
    },
    /// Move to a status, by name, through the workflow.
    Transition {
        /// The status name.
        status: String,
    },
    /// Move to the trash.
    Delete,
    /// Take out of the trash.
    Restore,
    /// Archive.
    Archive,
    /// Unarchive.
    Unarchive,
    /// Move to another project (and leave a key redirect behind).
    #[serde(rename_all = "camelCase")]
    Move {
        /// The destination project's key.
        project_key: String,
    },
    /// Add a tag (created in the card's project if missing).
    Tag {
        /// The tag name.
        name: String,
    },
    /// Remove a tag.
    Untag {
        /// The tag name.
        name: String,
    },
}

/// What happened (or would happen) to one card.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// The key as given.
    pub key: String,
    /// `changed`, `unchanged`, or `failed`.
    pub result: String,
    /// Why it failed.
    pub error: Option<String>,
}

/// The whole result.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Whether this was a preview (nothing was saved).
    pub preview: bool,
    /// How many changed (or would).
    pub changed: usize,
    /// How many were already as asked.
    pub unchanged: usize,
    /// How many failed (or would).
    pub failed: usize,
    /// Per card, in the order given.
    pub cards: Vec<Outcome>,
}

async fn resolve_user(tx: &mut sqlx::SqliteConnection, name: &str) -> AppResult<Option<String>> {
    if name.trim().is_empty() {
        return Ok(None);
    }
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE (username = ? OR id = ?) AND is_active = 1")
            .bind(name.trim())
            .bind(name.trim())
            .fetch_optional(&mut *tx)
            .await?;
    id.map(Some)
        .ok_or_else(|| AppError::Validation(format!("{name:?} is not an active user.")))
}

async fn by_name(
    tx: &mut sqlx::SqliteConnection,
    sql: &'static str,
    project_id: &str,
    name: &str,
    what: &str,
) -> AppResult<Option<String>> {
    if name.trim().is_empty() {
        return Ok(None);
    }
    let id: Option<String> = sqlx::query_scalar(sql)
        .bind(project_id)
        .bind(name.trim())
        .fetch_optional(&mut *tx)
        .await?;
    id.map(Some).ok_or_else(|| {
        AppError::Validation(format!("{name:?} is not a {what} of this card's project."))
    })
}

/// Resolves an `edit` operation's names into a patch for one card's project.
async fn edit_patch(
    tx: &mut sqlx::SqliteConnection,
    current: &Card,
    operation: &Operation,
) -> AppResult<CardPatch> {
    let Operation::Edit {
        priority,
        assignee,
        due_date,
        milestone,
    } = operation
    else {
        return Ok(CardPatch::default());
    };
    let mut patch = CardPatch::default();
    if let Some(name) = priority {
        patch.priority_id = Some(
            by_name(
                tx,
                "SELECT id FROM priorities WHERE project_id = ? AND name = ? COLLATE NOCASE",
                &current.project_id,
                name,
                "priority",
            )
            .await?,
        );
    }
    if let Some(name) = assignee {
        patch.assignee_id = Some(resolve_user(tx, name).await?);
    }
    if let Some(date) = due_date {
        patch.due_date = Some(if date.trim().is_empty() {
            None
        } else {
            Some(
                chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
                    .map_err(|_| AppError::Validation(format!("{date:?} is not a date.")))?,
            )
        });
    }
    if let Some(name) = milestone {
        patch.milestone_id = Some(
            by_name(
                tx,
                "SELECT id FROM milestones WHERE project_id = ? AND name = ? COLLATE NOCASE",
                &current.project_id,
                name,
                "milestone",
            )
            .await?,
        );
    }
    Ok(patch)
}

/// Applies one operation to one card inside `tx`. `Ok(true)` = it changed.
async fn apply_one(
    tx: &mut sqlx::SqliteConnection,
    actor: &User,
    current: &Card,
    operation: &Operation,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    let author = Some(actor.id.as_str());
    let before = current.updated_at;
    let after = match operation {
        Operation::Edit { .. } => {
            let patch = edit_patch(tx, current, operation).await?;
            card::update(tx, current, &patch, author, now).await?
        }
        Operation::Transition { status } => {
            let id = by_name(
                tx,
                "SELECT id FROM statuses WHERE project_id = ? AND name = ? COLLATE NOCASE",
                &current.project_id,
                status,
                "status",
            )
            .await?
            .ok_or_else(|| AppError::Validation("Name a status.".to_owned()))?;
            card::update(
                tx,
                current,
                &CardPatch {
                    status_id: Some(id),
                    ..CardPatch::default()
                },
                author,
                now,
            )
            .await?
        }
        Operation::Delete => card::soft_delete(tx, current, author, now).await?,
        Operation::Restore => card::restore(tx, current, author, now).await?,
        Operation::Archive | Operation::Unarchive => {
            let archived = matches!(operation, Operation::Archive);
            card::update(
                tx,
                current,
                &CardPatch {
                    archived: Some(archived),
                    ..CardPatch::default()
                },
                author,
                now,
            )
            .await?
        }
        Operation::Move { project_key } => {
            let target = project::find_by_key_tx(tx, project_key)
                .await?
                .ok_or_else(|| AppError::Validation(format!("{project_key} is not a project.")))?;
            card::move_to_project(tx, current, &target, author, now).await?
        }
        Operation::Tag { name } | Operation::Untag { name } => {
            let name = tag::validate_name(name)?;
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT id FROM tags WHERE (project_id = ? OR project_id IS NULL) AND name = ? \
                 ORDER BY project_id IS NULL LIMIT 1",
            )
            .bind(&current.project_id)
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await?;
            return if matches!(operation, Operation::Tag { .. }) {
                let id = match existing {
                    Some(id) => id,
                    None => {
                        tag::insert(tx, Some(&current.project_id), &name, None, now)
                            .await?
                            .id
                    }
                };
                tag::attach(tx, &current.id, &id, now).await
            } else {
                match existing {
                    Some(id) => tag::detach(tx, &current.id, &id).await,
                    None => Ok(false),
                }
            };
        }
    };
    Ok(after.updated_at != before
        || after.key != current.key
        || after.deleted_at != current.deleted_at)
}

/// Checks the card exists, is visible, and is editable by `actor`.
async fn editable(db: &Db, actor: &User, key: &str, operation: &Operation) -> AppResult<Card> {
    let found = card::find_by_key(db, &key.trim().to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    // A card in the trash can only be restored.
    if found.is_deleted() && !matches!(operation, Operation::Restore) {
        return Err(AppError::NotFound);
    }
    let owner = project::find_by_id(db, &found.project_id)
        .await?
        .ok_or(AppError::NotFound)?;
    member::require(db, &owner, actor, ProjectRole::Member).await?;
    if let Operation::Move { project_key } = operation {
        let target = project::find_by_key(db, project_key)
            .await?
            .ok_or(AppError::NotFound)?;
        member::require(db, &target, actor, ProjectRole::Member).await?;
    }
    Ok(found)
}

fn describe(err: &AppError) -> String {
    match err {
        AppError::NotFound => "not found".to_owned(),
        AppError::Forbidden => "you cannot edit this card".to_owned(),
        other => other.to_string(),
    }
}

/// Runs `operation` over `keys`. With `preview`, nothing is saved.
pub async fn run(
    db: &Db,
    actor: &User,
    keys: &[String],
    operation: &Operation,
    preview: bool,
    now: DateTime<Utc>,
    mut progress: impl FnMut(usize, usize),
) -> AppResult<Report> {
    if keys.is_empty() {
        return Err(AppError::Validation("Select at least one card.".to_owned()));
    }
    if keys.len() > MAX_CARDS {
        return Err(AppError::Validation(format!(
            "A bulk change may touch at most {MAX_CARDS} cards; {} were selected.",
            keys.len()
        )));
    }
    let mut cards = Vec::with_capacity(keys.len());
    // One transaction for the whole preview (then rolled back), so each card sees the
    // previous cards' effects exactly as apply would.
    let mut preview_tx = if preview {
        Some(db.begin_write().await?)
    } else {
        None
    };
    for (index, key) in keys.iter().enumerate() {
        let outcome = async {
            let found = editable(db, actor, key, operation).await?;
            if let Some(tx) = preview_tx.as_mut() {
                let current = card::find_by_id_tx(&mut *tx, &found.id)
                    .await?
                    .ok_or(AppError::NotFound)?;
                apply_one(&mut *tx, actor, &current, operation, now).await
            } else {
                let mut tx = db.begin_write().await?;
                let current = card::find_by_id_tx(&mut tx, &found.id)
                    .await?
                    .ok_or(AppError::NotFound)?;
                let changed = apply_one(&mut tx, actor, &current, operation, now).await?;
                tx.commit().await?;
                Ok(changed)
            }
        }
        .await;
        cards.push(match outcome {
            Ok(true) => Outcome {
                key: key.clone(),
                result: "changed".to_owned(),
                error: None,
            },
            Ok(false) => Outcome {
                key: key.clone(),
                result: "unchanged".to_owned(),
                error: None,
            },
            Err(err) => Outcome {
                key: key.clone(),
                result: "failed".to_owned(),
                error: Some(describe(&err)),
            },
        });
        if (index + 1) % 50 == 0 {
            progress(index + 1, keys.len());
        }
    }
    if let Some(tx) = preview_tx {
        tx.rollback().await?;
    }
    progress(keys.len(), keys.len());
    let count = |r: &str| cards.iter().filter(|c| c.result == r).count();
    Ok(Report {
        preview,
        changed: count("changed"),
        unchanged: count("unchanged"),
        failed: count("failed"),
        cards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operations_read_in_the_documented_shape() {
        let op: Operation =
            serde_json::from_value(json!({ "kind": "edit", "priority": "High", "dueDate": "" }))
                .unwrap();
        assert!(matches!(
            op,
            Operation::Edit {
                priority: Some(_),
                due_date: Some(_),
                ..
            }
        ));
        let op: Operation =
            serde_json::from_value(json!({ "kind": "move", "projectKey": "OTHER" })).unwrap();
        assert_eq!(
            op,
            Operation::Move {
                project_key: "OTHER".into()
            }
        );
        assert!(serde_json::from_value::<Operation>(json!({ "kind": "explode" })).is_err());
    }
}

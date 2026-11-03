//! Repeatable, transactional bootstrap of Atlas's own development backlog.

use crate::auth::now;
use crate::db::Db;
use crate::domain::{StatusCategory, board, card, config, member, project, tag, template};
use crate::error::{AppError, AppResult};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct TodoItem {
    pub phase: String,
    pub phase_tag: String,
    pub title: String,
    pub details: String,
    pub line: usize,
    pub completed: bool,
}

/// Parse checklist items inside phase sections, preserving subordinate notes and examples.
pub fn parse_todo(source: &str) -> Vec<TodoItem> {
    let mut items: Vec<TodoItem> = Vec::new();
    let mut phase = String::new();
    let mut phase_tag = String::new();
    let mut active: Option<usize> = None;
    let mut fenced = false;
    for (index, line) in source.lines().enumerate() {
        if line.starts_with("## ") && !fenced {
            active = None;
            phase = if line.starts_with("## Phase ") {
                line.trim_start_matches("## ").to_owned()
            } else {
                String::new()
            };
            phase_tag = phase
                .split_whitespace()
                .nth(1)
                .map(|number| format!("phase-{}", number.to_lowercase()))
                .unwrap_or_default();
        }
        let trimmed = line.trim_start();
        if !fenced && !phase.is_empty() {
            let task = trimmed
                .strip_prefix("- [ ] ")
                .map(|title| (false, title))
                .or_else(|| trimmed.strip_prefix("- [x] ").map(|title| (true, title)))
                .or_else(|| trimmed.strip_prefix("- [X] ").map(|title| (true, title)));
            if let Some((completed, title)) = task {
                items.push(TodoItem {
                    phase: phase.clone(),
                    phase_tag: phase_tag.clone(),
                    title: title.to_owned(),
                    details: String::new(),
                    line: index + 1,
                    completed,
                });
                active = Some(items.len() - 1);
                continue;
            }
        }
        if let Some(index) = active
            && trimmed != "---"
        {
            items[index].details.push_str(line);
            items[index].details.push('\n');
        }
        if trimmed.starts_with("```") {
            fenced = !fenced;
        }
    }
    items
}

#[derive(Debug)]
pub struct ImportReport {
    pub created: usize,
    pub skipped: usize,
}

/// Import new items without overwriting human edits, status, ranking or existing projects.
pub async fn import(db: &Db, source: &str, include_completed: bool) -> AppResult<ImportReport> {
    let items: Vec<_> = parse_todo(source)
        .into_iter()
        .filter(|item| include_completed || !item.completed)
        .collect();
    if items.is_empty() {
        return Err(AppError::Validation(
            "TODO.md contains no matching phase checklist items".to_owned(),
        ));
    }
    let timestamp = now();
    let mut tx = db.begin_write().await?;
    let admin: String = sqlx::query_scalar(
        "SELECT id FROM users WHERE role = 'admin' AND is_active = 1 ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        AppError::Validation(
            "Create an active administrator before importing the backlog".to_owned(),
        )
    })?;
    let project = prepare_project(&mut tx, &admin, timestamp).await?;
    let type_id: String = sqlx::query_scalar("SELECT id FROM card_types WHERE project_id = ? ORDER BY (name = 'Task') DESC, is_default DESC LIMIT 1")
        .bind(&project.id).fetch_one(&mut *tx).await?;
    let todo = config::first_status_in_category_tx(&mut tx, &project.id, StatusCategory::Todo)
        .await?
        .ok_or_else(|| AppError::Validation("The ATLAS project needs a To Do status".to_owned()))?;
    let done =
        config::first_status_in_category_tx(&mut tx, &project.id, StatusCategory::Done).await?;
    let mut report = ImportReport {
        created: 0,
        skipped: 0,
    };
    for item in &items {
        let status = if item.completed {
            done.as_ref().ok_or_else(|| {
                AppError::Validation(
                    "The ATLAS project needs a Done status to import completed items".to_owned(),
                )
            })?
        } else {
            &todo
        };
        if import_item(&mut tx, &project, &type_id, &admin, item, status, timestamp).await? {
            report.created += 1;
        } else {
            report.skipped += 1;
        }
    }
    tx.commit().await?;
    Ok(report)
}

async fn prepare_project(
    tx: &mut sqlx::SqliteConnection,
    admin: &str,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> AppResult<project::Project> {
    let project = match project::find_by_key_tx(tx, "ATLAS").await? {
        Some(project) => project,
        None => template::create_project(tx, template::Template::Programming, "ATLAS", "Atlas development", Some("Atlas's self-development backlog, imported from TODO.md. Pick one task and launch a coding agent from its card."), Some(admin), timestamp).await?,
    };
    member::insert_or_ignore(
        tx,
        &project.id,
        admin,
        member::ProjectRole::Owner,
        Some(admin),
        timestamp,
    )
    .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM boards WHERE project_id = ? AND name = 'Atlas development')",
    )
    .bind(&project.id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        board::insert_board(
            tx,
            &project.id,
            &board::NewBoard {
                name: "Atlas development".to_owned(),
                default_parent_id: None,
                aql_filter: None,
                swimlane: "none".to_owned(),
                wip_limits: serde_json::json!({}),
            },
            timestamp,
        )
        .await?;
    }
    Ok(project)
}

async fn import_item(
    tx: &mut sqlx::SqliteConnection,
    project: &project::Project,
    type_id: &str,
    admin: &str,
    item: &TodoItem,
    status: &config::Status,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> AppResult<bool> {
    let title = item
        .title
        .split(" — _")
        .next()
        .unwrap_or(&item.title)
        .replace('`', "");
    let source_key = hex::encode(Sha256::digest(format!("{}\n{title}", item.phase_tag)));
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM todo_imports WHERE project_id = ? AND source_key = ?)",
    )
    .bind(&project.id)
    .bind(&source_key)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Ok(false);
    }
    let description = format!(
        "## Task\n\n{}\n\n{}\n\n## Context\n\n{}\n\nSource: `TODO.md`, line {}.\n\nRead `CLAUDE.md` and relevant research before implementation. Implement this ticket, run `make check`, and report changes and validation. Preserve Alastair Rayner's commit attribution. Do not merge or deploy automatically.\n",
        item.title,
        item.details.trim(),
        item.phase,
        item.line
    );
    let created = card::create(
        tx,
        project,
        &card::NewCard {
            type_id: type_id.to_owned(),
            parent_id: None,
            summary: title.chars().take(240).collect(),
            description: Some(description),
            status_id: Some(status.id.clone()),
            priority_id: None,
            assignee_id: None,
            reporter_id: Some(admin.to_owned()),
            due_date: None,
            start_date: None,
            estimate: None,
            placement: card::Placement::Bottom,
        },
        admin,
        timestamp,
    )
    .await?;
    let tag_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM tags WHERE project_id = ? AND name = ?")
            .bind(&project.id)
            .bind(&item.phase_tag)
            .fetch_optional(&mut *tx)
            .await?;
    let tag_id = match tag_id {
        Some(id) => id,
        None => {
            tag::insert(
                tx,
                Some(&project.id),
                &item.phase_tag,
                Some(tag::TagColour::Blue),
                timestamp,
            )
            .await?
            .id
        }
    };
    tag::attach(tx, &created.id, &tag_id, timestamp).await?;
    sqlx::query("INSERT INTO todo_imports (project_id, source_key, card_id) VALUES (?, ?, ?)")
        .bind(&project.id)
        .bind(source_key)
        .bind(created.id)
        .execute(&mut *tx)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_preserves_notes_ignores_example_checkboxes_and_tracks_completed_items() {
        let source = "## Phase 1 — Core\n- [ ] Add API\n  Acceptance: requests work.\n```md\n- [ ] Example only\n```\n- [x] Existing API\n## Progress\n- [ ] Not a phase task\n";
        let items = parse_todo(source);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].phase_tag, "phase-1");
        assert!(items[0].details.contains("Acceptance: requests work."));
        assert!(items[1].completed);
    }

    #[tokio::test]
    async fn import_is_idempotent_and_preserves_card_edits() {
        let temp = crate::test_support::TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        crate::db::migrate::run(&db).await.unwrap();
        crate::auth::seed::ensure_default_admin(&db).await.unwrap();
        let source = "## Phase 1 — Core\n- [ ] Implement API\n- [x] Foundation\n";
        assert_eq!(import(&db, source, false).await.unwrap().created, 1);
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("UPDATE cards SET summary = 'Human edit' WHERE key = 'ATLAS-1'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let report = import(&db, source, true).await.unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(
            card::find_by_key(&db, "ATLAS-1")
                .await
                .unwrap()
                .unwrap()
                .summary,
            "Human edit"
        );
        assert!(
            card::find_by_key(&db, "ATLAS-2")
                .await
                .unwrap()
                .unwrap()
                .is_resolved()
        );
        assert_eq!(import(&db, source, true).await.unwrap().created, 0);
    }
}

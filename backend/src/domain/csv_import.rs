//! CSV import and export (`TODO.md` Phase 18) — how a job-search spreadsheet becomes an Atlas
//! project, and how an AQL result becomes a spreadsheet again.
//!
//! # Improving on Jira: validate everything, then commit everything
//!
//! Jira's importer fails halfway through and leaves half a project behind. Here an import is
//! two steps over the same code: [`plan`] parses the file, maps every column, resolves every
//! name (types, statuses, users, parents) and reports every problem with its row and column;
//! [`import`] refuses to write anything if the plan has a single error, and otherwise creates
//! every card inside **one** transaction. A file is either fully imported or not at all.
//!
//! # The file format, as Jira users know it
//!
//! - `Summary` is the only required column.
//! - A repeated header is a multi-value field: three `Labels` columns are three labels.
//! - `Issue ID` + `Parent` build the hierarchy; a parent row must come before its children,
//!   and its id must be unique in the file. (A parent may also be an existing card key.)
//! - Columns are mapped automatically from their headers — `Status`, `Assignee`, `Due date`,
//!   `Story Points`, a custom field's name — and the caller can override any mapping.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::db::Db;
use crate::domain::card::{self, NewCard, Placement};
use crate::domain::custom_field::{self, CustomField};
use crate::domain::project::Project;
use crate::domain::{config, tag};
use crate::error::{AppError, AppResult};

/// The most data rows one import may hold.
pub const MAX_ROWS: usize = 5000;
/// The largest file accepted, in bytes.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;

/// Parses RFC 4180 CSV: quoted fields, `""` escapes, embedded newlines, CRLF or LF, and an
/// optional UTF-8 BOM. Returns rows of fields; a trailing empty line is not a row.
pub fn parse(text: &str) -> AppResult<Vec<Vec<String>>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut line = 1;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push(c);
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err(AppError::Validation(format!(
            "An unclosed quote runs to the end of the file (it opens on or before line {line})."
        )));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop blank lines (a row of one empty field).
    rows.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    Ok(rows)
}

/// Writes one CSV field, quoting only when it must.
pub fn field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) || value.starts_with(' ') || value.ends_with(' ') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// What a column becomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Target {
    /// Ignored.
    Skip,
    /// The one required field.
    Summary,
    /// Markdown body.
    Description,
    /// A card type, by name.
    Type,
    /// A status, by name.
    Status,
    /// A priority, by name.
    Priority,
    /// A user, by username, email or display name.
    Assignee,
    /// A user.
    Reporter,
    /// One tag per column; repeat the header for more.
    Labels,
    /// `YYYY-MM-DD` (or `DD/MM/YYYY`).
    DueDate,
    /// `YYYY-MM-DD`.
    StartDate,
    /// A number.
    Estimate,
    /// A milestone, by name (created if missing).
    Milestone,
    /// A component, by name (created if missing).
    Component,
    /// This row's id within the file, for `Parent` to refer to.
    IssueId,
    /// The parent row's `Issue ID`, or an existing card key.
    Parent,
    /// A custom field, by id.
    CustomField(String),
}

/// Suggests a mapping from a header.
pub fn suggest(header: &str, fields: &[CustomField]) -> Target {
    let h = header.trim().to_ascii_lowercase().replace(['_', '-'], " ");
    match h.as_str() {
        "summary" | "title" | "name" | "company" | "role" | "position" => Target::Summary,
        "description" | "details" | "notes" | "body" => Target::Description,
        "type" | "issue type" | "card type" => Target::Type,
        "status" | "stage" | "state" => Target::Status,
        "priority" | "interest" => Target::Priority,
        "assignee" | "owner" | "assigned to" => Target::Assignee,
        "reporter" => Target::Reporter,
        "labels" | "label" | "tags" | "tag" => Target::Labels,
        "due" | "due date" | "deadline" | "follow up" | "follow up date" => Target::DueDate,
        "start" | "start date" | "applied" | "applied on" | "date applied" => Target::StartDate,
        "estimate" | "story points" | "points" | "hours" => Target::Estimate,
        "milestone" | "fix version" | "fix versions" | "version" | "round" => Target::Milestone,
        "component" | "components" | "area" => Target::Component,
        "issue id" | "id" | "row id" => Target::IssueId,
        "parent" | "parent id" | "parent key" => Target::Parent,
        _ => fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(header.trim()))
            .map_or(Target::Skip, |f| Target::CustomField(f.id.clone())),
    }
}

/// One problem, located.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    /// 1-based data row (the header is row 0).
    pub row: usize,
    /// The column's header, if the problem is in one.
    pub column: Option<String>,
    /// What is wrong.
    pub message: String,
}

/// One card the import will create.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedCard {
    /// 1-based data row.
    pub row: usize,
    /// Its summary.
    pub summary: String,
    /// Its type's name.
    pub card_type: String,
    /// Its status's name.
    pub status: String,
    /// Its tags.
    pub labels: Vec<String>,
    /// Its parent's row (in this file) or key (an existing card).
    pub parent: Option<String>,
}

/// What an import would do, and everything stopping it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// The header row.
    pub headers: Vec<String>,
    /// The mapping used, one per column — the suggestion unless overridden.
    pub mapping: Vec<Target>,
    /// Cards to create (the first 200, for the preview table).
    pub cards: Vec<PlannedCard>,
    /// How many cards in total.
    pub total: usize,
    /// Every problem. An import with any is refused.
    pub problems: Vec<Problem>,
}

/// A fully resolved row, ready to insert.
#[derive(Debug, Clone)]
struct Resolved {
    row: usize,
    new: NewCard,
    labels: Vec<String>,
    components: Vec<String>,
    milestone: Option<String>,
    issue_id: Option<String>,
    parent_row: Option<String>,
    parent_card: Option<String>,
    custom: Vec<(String, Value)>,
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(text, "%d/%m/%Y"))
        .or_else(|_| NaiveDate::parse_from_str(text, "%d.%m.%Y"))
        .or_else(|_| DateTime::parse_from_rfc3339(text).map(|d| d.date_naive()))
        .ok()
}

struct Lookups {
    types: Vec<config::CardType>,
    statuses: Vec<config::Status>,
    priorities: Vec<config::Priority>,
    users: Vec<(String, String, Option<String>, String)>,
    fields: Vec<CustomField>,
}

async fn lookups(db: &Db, project: &Project) -> AppResult<Lookups> {
    Ok(Lookups {
        types: config::card_types(db, &project.id).await?,
        statuses: config::statuses(db, &project.id).await?,
        priorities: config::priorities(db, &project.id).await?,
        users: sqlx::query_as(
            "SELECT id, username, email, display_name FROM users WHERE is_active = 1",
        )
        .fetch_all(db.reader())
        .await?,
        fields: custom_field::list(db).await?,
    })
}

fn find_user(lookups: &Lookups, name: &str) -> Option<String> {
    let name = name.trim();
    lookups
        .users
        .iter()
        .find(|(id, username, email, display)| {
            username.eq_ignore_ascii_case(name)
                || email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(name))
                || display.eq_ignore_ascii_case(name)
                || id == name
        })
        .map(|(id, ..)| id.clone())
}

// One arm per target column: the module doc's format description, row for row.
#[allow(clippy::too_many_lines)]
fn resolve_rows(
    rows: &[Vec<String>],
    headers: &[String],
    mapping: &[Target],
    lookups: &Lookups,
    existing_keys: &[String],
) -> (Vec<Resolved>, Vec<Problem>) {
    let mut problems = Vec::new();
    let mut resolved = Vec::new();
    let default_type = lookups
        .types
        .iter()
        .find(|t| t.is_default)
        .or_else(|| lookups.types.first());
    let mut seen_ids: HashMap<String, usize> = HashMap::new();
    let users = |name: &str| find_user(lookups, name);
    for (index, row) in rows.iter().enumerate() {
        let number = index + 1;
        let mut summary: Option<String> = None;
        let mut new = NewCard {
            type_id: default_type.map(|t| t.id.clone()).unwrap_or_default(),
            parent_id: None,
            summary: String::new(),
            description: None,
            status_id: None,
            priority_id: None,
            assignee_id: None,
            reporter_id: None,
            due_date: None,
            start_date: None,
            estimate: None,
            placement: Placement::Bottom,
        };
        let mut planned = Resolved {
            row: number,
            new: new.clone(),
            labels: Vec::new(),
            components: Vec::new(),
            milestone: None,
            issue_id: None,
            parent_row: None,
            parent_card: None,
            custom: Vec::new(),
        };
        for (column, target) in mapping.iter().enumerate() {
            let value = row.get(column).map_or("", |v| v.trim());
            let header = headers.get(column).cloned();
            let mut problem = |message: String| {
                problems.push(Problem {
                    row: number,
                    column: header.clone(),
                    message,
                });
            };
            if value.is_empty() {
                continue;
            }
            match target {
                Target::Skip => {}
                Target::Summary => match card::validate_summary(value) {
                    Ok(s) => summary = Some(s),
                    Err(err) => problem(err.to_string()),
                },
                Target::Description => match card::validate_description(value) {
                    Ok(d) => new.description = Some(d),
                    Err(err) => problem(err.to_string()),
                },
                Target::Type => match lookups
                    .types
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(value))
                {
                    Some(t) => new.type_id.clone_from(&t.id),
                    None => problem(format!("{value:?} is not a card type of this project.")),
                },
                Target::Status => match lookups
                    .statuses
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(value))
                {
                    Some(s) => new.status_id = Some(s.id.clone()),
                    None => problem(format!("{value:?} is not a status of this project.")),
                },
                Target::Priority => match lookups
                    .priorities
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(value))
                {
                    Some(p) => new.priority_id = Some(p.id.clone()),
                    None => problem(format!("{value:?} is not a priority of this project.")),
                },
                Target::Assignee | Target::Reporter => match users(value) {
                    Some(id) if *target == Target::Assignee => new.assignee_id = Some(id),
                    Some(id) => new.reporter_id = Some(id),
                    None => problem(format!("{value:?} is not an active user.")),
                },
                Target::Labels => {
                    for label in value
                        .split([';', ','])
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                    {
                        match tag::validate_name(&label.replace(' ', "-")) {
                            Ok(name) => planned.labels.push(name),
                            Err(err) => problem(err.to_string()),
                        }
                    }
                }
                Target::DueDate | Target::StartDate => match parse_date(value) {
                    Some(d) if *target == Target::DueDate => new.due_date = Some(d),
                    Some(d) => new.start_date = Some(d),
                    None => problem(format!("{value:?} is not a date (use YYYY-MM-DD).")),
                },
                Target::Estimate => match value.parse::<f64>() {
                    Ok(n) if n.is_finite() => new.estimate = Some(n),
                    _ => problem(format!("{value:?} is not a number.")),
                },
                Target::Milestone => planned.milestone = Some(value.to_owned()),
                Target::Component => planned.components.push(value.to_owned()),
                Target::IssueId => {
                    if let Some(first) = seen_ids.insert(value.to_owned(), number) {
                        problem(format!("Issue ID {value:?} is also used on row {first}."));
                    }
                    planned.issue_id = Some(value.to_owned());
                }
                Target::Parent => {
                    if seen_ids.contains_key(value) {
                        planned.parent_row = Some(value.to_owned());
                    } else if existing_keys.iter().any(|k| k.eq_ignore_ascii_case(value)) {
                        planned.parent_card = Some(value.to_ascii_uppercase());
                    } else {
                        problem(format!(
                            "Parent {value:?} is neither an earlier row's Issue ID nor an existing card (parents must come before their children)."
                        ));
                    }
                }
                Target::CustomField(id) => match lookups.fields.iter().find(|f| f.id == *id) {
                    Some(f) => {
                        let raw = if matches!(
                            f.field_type,
                            custom_field::FieldType::Multiselect
                                | custom_field::FieldType::Labels
                                | custom_field::FieldType::Multiuser
                        ) {
                            Value::Array(
                                value
                                    .split(';')
                                    .map(|v| Value::String(v.trim().to_owned()))
                                    .collect(),
                            )
                        } else if f.field_type == custom_field::FieldType::Checkbox {
                            Value::Bool(matches!(
                                value.to_ascii_lowercase().as_str(),
                                "yes" | "true" | "1" | "y" | "x"
                            ))
                        } else if matches!(f.field_type, custom_field::FieldType::User) {
                            Value::String(users(value).unwrap_or_else(|| value.to_owned()))
                        } else {
                            Value::String(value.to_owned())
                        };
                        match custom_field::validate_value(f, &raw, &|id| {
                            lookups.users.iter().any(|u| u.0 == id)
                        }) {
                            Ok(Some(v)) => planned.custom.push((f.id.clone(), v)),
                            Ok(None) => {}
                            Err(err) => problem(err.to_string()),
                        }
                    }
                    None => problem("That custom field no longer exists.".to_owned()),
                },
            }
        }
        match summary {
            Some(s) => new.summary = s,
            None => problems.push(Problem {
                row: number,
                column: None,
                message: "Summary is required.".to_owned(),
            }),
        }
        if new.type_id.is_empty() {
            problems.push(Problem {
                row: number,
                column: None,
                message: "The project has no card types.".to_owned(),
            });
        }
        planned.new = new;
        resolved.push(planned);
    }
    (resolved, problems)
}

fn check_mapping(headers: &[String], mapping: &[Target]) -> AppResult<()> {
    if mapping.len() != headers.len() {
        return Err(AppError::Validation(format!(
            "The mapping has {} entries for {} columns.",
            mapping.len(),
            headers.len()
        )));
    }
    if !mapping.contains(&Target::Summary) {
        return Err(AppError::Validation(
            "Map one column to Summary — it is the only required field.".to_owned(),
        ));
    }
    for single in [
        Target::Summary,
        Target::Description,
        Target::Type,
        Target::Status,
        Target::Priority,
        Target::Assignee,
        Target::IssueId,
        Target::Parent,
    ] {
        if mapping.iter().filter(|t| **t == single).count() > 1 {
            return Err(AppError::Validation(format!(
                "Only one column may be mapped to {single:?}."
            )));
        }
    }
    Ok(())
}

async fn prepare(
    db: &Db,
    project: &Project,
    text: &str,
    mapping: Option<Vec<Target>>,
) -> AppResult<(Plan, Vec<Resolved>)> {
    if text.len() > MAX_BYTES {
        return Err(AppError::Validation(format!(
            "The file is larger than {} MB.",
            MAX_BYTES / (1024 * 1024)
        )));
    }
    let mut rows = parse(text)?;
    if rows.is_empty() {
        return Err(AppError::Validation("The file is empty.".to_owned()));
    }
    let headers: Vec<String> = rows
        .remove(0)
        .into_iter()
        .map(|h| h.trim().to_owned())
        .collect();
    if rows.len() > MAX_ROWS {
        return Err(AppError::Validation(format!(
            "At most {MAX_ROWS} rows per import; this file has {}.",
            rows.len()
        )));
    }
    let lookups = lookups(db, project).await?;
    let mapping = mapping.unwrap_or_else(|| {
        headers
            .iter()
            .map(|h| suggest(h, &lookups.fields))
            .collect()
    });
    check_mapping(&headers, &mapping)?;
    let existing_keys: Vec<String> = if mapping.contains(&Target::Parent) {
        sqlx::query_scalar("SELECT key FROM cards WHERE project_id = ? AND deleted_at IS NULL")
            .bind(&project.id)
            .fetch_all(db.reader())
            .await?
    } else {
        Vec::new()
    };
    let (resolved, problems) = resolve_rows(&rows, &headers, &mapping, &lookups, &existing_keys);
    let name = |id: &Option<String>, list: &[(String, String)], fallback: &str| {
        id.as_ref()
            .and_then(|id| list.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone()))
            .unwrap_or_else(|| fallback.to_owned())
    };
    let types: Vec<(String, String)> = lookups
        .types
        .iter()
        .map(|t| (t.id.clone(), t.name.clone()))
        .collect();
    let statuses: Vec<(String, String)> = lookups
        .statuses
        .iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    let first_status = lookups
        .statuses
        .first()
        .map_or_else(String::new, |s| s.name.clone());
    let cards = resolved
        .iter()
        .take(200)
        .map(|r| PlannedCard {
            row: r.row,
            summary: r.new.summary.clone(),
            card_type: name(&Some(r.new.type_id.clone()), &types, ""),
            status: name(&r.new.status_id, &statuses, &first_status),
            labels: r.labels.clone(),
            parent: r.parent_row.clone().or_else(|| r.parent_card.clone()),
        })
        .collect();
    Ok((
        Plan {
            headers,
            mapping,
            cards,
            total: resolved.len(),
            problems,
        },
        resolved,
    ))
}

/// Parses, maps and validates a file without writing anything.
pub async fn plan(
    db: &Db,
    project: &Project,
    text: &str,
    mapping: Option<Vec<Target>>,
) -> AppResult<Plan> {
    Ok(prepare(db, project, text, mapping).await?.0)
}

/// What an import created.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    /// The new cards' keys, in file order.
    pub keys: Vec<String>,
}

/// Puts a created row's labels, components, milestone and custom values on its card,
/// creating any label, component or milestone the project does not have yet.
async fn attach_extras(
    tx: &mut sqlx::SqliteConnection,
    project: &Project,
    card_id: &str,
    row: &Resolved,
    now: DateTime<Utc>,
) -> AppResult<()> {
    for label in &row.labels {
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM tags WHERE (project_id = ? OR project_id IS NULL) AND name = ? ORDER BY project_id IS NULL LIMIT 1")
            .bind(&project.id)
            .bind(label)
            .fetch_optional(&mut *tx)
            .await?;
        let id = match existing {
            Some(id) => id,
            None => {
                tag::insert(tx, Some(&project.id), label, None, now)
                    .await?
                    .id
            }
        };
        tag::attach(tx, card_id, &id, now).await?;
    }
    for name in &row.components {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM components WHERE project_id = ? AND name = ?")
                .bind(&project.id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        let id = match existing {
            Some(id) => id,
            None => {
                crate::domain::planning::insert_component(tx, &project.id, name, None, None, now)
                    .await?
                    .id
            }
        };
        sqlx::query("INSERT OR IGNORE INTO card_components (card_id, component_id) VALUES (?, ?)")
            .bind(card_id)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(name) = &row.milestone {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM milestones WHERE project_id = ? AND name = ?")
                .bind(&project.id)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        let id = match existing {
            Some(id) => id,
            None => {
                crate::domain::planning::insert_milestone(
                    tx,
                    &project.id,
                    name,
                    &crate::domain::planning::MilestonePatch::default(),
                    now,
                )
                .await?
                .id
            }
        };
        sqlx::query("UPDATE cards SET milestone_id = ? WHERE id = ?")
            .bind(&id)
            .bind(card_id)
            .execute(&mut *tx)
            .await?;
    }
    for (field_id, value) in &row.custom {
        custom_field::set_value(tx, card_id, field_id, Some(value)).await?;
    }
    Ok(())
}

/// Imports a file: all of it, in one transaction, or — if the plan has any problem — none.
pub async fn import(
    db: &Db,
    project: &Project,
    text: &str,
    mapping: Option<Vec<Target>>,
    creator_id: &str,
    now: DateTime<Utc>,
) -> AppResult<Result<Imported, Plan>> {
    let (plan, resolved) = prepare(db, project, text, mapping).await?;
    if !plan.problems.is_empty() {
        return Ok(Err(plan));
    }
    let mut tx = db.begin_write().await?;
    let mut by_issue_id: HashMap<String, String> = HashMap::new();
    let mut keys = Vec::with_capacity(resolved.len());
    for mut row in resolved {
        row.new.parent_id = match (&row.parent_row, &row.parent_card) {
            (Some(issue), _) => by_issue_id.get(issue).cloned(),
            (None, Some(key)) => card::find_by_key_tx(&mut tx, key).await?.map(|c| c.id),
            _ => None,
        };
        // A hierarchy rule (a child's type must sit below its parent's) is enforced by
        // `card::create` itself; a violation fails the whole import, with the row.
        let created = card::create(&mut tx, project, &row.new, creator_id, now)
            .await
            .map_err(|err| AppError::Validation(format!("Row {}: {err}", row.row)))?;
        if let Some(issue) = &row.issue_id {
            by_issue_id.insert(issue.clone(), created.id.clone());
        }
        attach_extras(&mut tx, project, &created.id, &row, now).await?;
        keys.push(created.key);
    }
    tx.commit().await?;
    Ok(Ok(Imported { keys }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_cells_cannot_become_formulas() {
        assert_eq!(defuse("=HYPERLINK(\"x\")"), "'=HYPERLINK(\"x\")");
        assert_eq!(defuse("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(defuse("-3"), "-3");
        assert_eq!(defuse("Plain"), "Plain");
    }

    #[test]
    fn rfc4180_quotes_escapes_newlines_and_crlf() {
        let rows = parse("\u{feff}Summary,Notes\r\nAcme,\"Said \"\"call back\"\", maybe\"\r\n\"Multi\nline\",x\r\n\r\n").unwrap();
        assert_eq!(
            rows,
            vec![
                vec!["Summary".to_owned(), "Notes".to_owned()],
                vec!["Acme".to_owned(), "Said \"call back\", maybe".to_owned()],
                vec!["Multi\nline".to_owned(), "x".to_owned()],
            ]
        );
        assert!(parse("a,\"unclosed\n").is_err());
        assert_eq!(
            parse("a,b").unwrap(),
            vec![vec!["a".to_owned(), "b".to_owned()]]
        );
    }

    #[test]
    fn fields_round_trip_through_quoting() {
        for value in [
            "plain",
            "with, comma",
            "with \"quotes\"",
            "multi\nline",
            " padded ",
        ] {
            let row = parse(&format!("{},end", field(value))).unwrap();
            assert_eq!(row[0][0], value);
        }
    }

    #[test]
    fn headers_map_to_fields_as_a_job_search_spreadsheet_spells_them() {
        assert_eq!(suggest("Company", &[]), Target::Summary);
        assert_eq!(suggest("Stage", &[]), Target::Status);
        assert_eq!(suggest("Date Applied", &[]), Target::StartDate);
        assert_eq!(suggest("Follow-up date", &[]), Target::DueDate);
        assert_eq!(suggest("Story Points", &[]), Target::Estimate);
        assert_eq!(suggest("Issue ID", &[]), Target::IssueId);
        assert_eq!(suggest("Salary band", &[]), Target::Skip);
    }

    #[test]
    fn dates_in_common_spreadsheet_formats() {
        let expected = NaiveDate::from_ymd_opt(2026, 3, 1);
        assert_eq!(parse_date("2026-03-01"), expected);
        assert_eq!(parse_date("01/03/2026"), expected);
        assert_eq!(parse_date("01.03.2026"), expected);
        assert_eq!(parse_date("March 1st"), None);
    }
}

/// Neutralises spreadsheet formula injection: a cell a spreadsheet would evaluate (`=`, `+`,
/// `-`, `@`, or a leading tab/CR) is prefixed with `'`, unless it is plainly a number.
fn defuse(value: &str) -> String {
    let risky = value.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if risky && value.parse::<f64>().is_err() {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

/// The fixed columns of an export, before the custom fields.
const EXPORT_HEADERS: [&str; 17] = [
    "Issue key",
    "Issue ID",
    "Parent",
    "Summary",
    "Issue Type",
    "Status",
    "Priority",
    "Assignee",
    "Reporter",
    "Labels",
    "Components",
    "Milestone",
    "Due date",
    "Start date",
    "Estimate",
    "Created",
    "Updated",
];

#[derive(Debug, sqlx::FromRow)]
struct ExportRow {
    level: i64,
    key: String,
    parent_key: Option<String>,
    summary: String,
    type_name: String,
    status_name: String,
    priority_name: Option<String>,
    assignee: Option<String>,
    reporter: Option<String>,
    milestone: Option<String>,
    due_date: Option<NaiveDate>,
    start_date: Option<NaiveDate>,
    estimate: Option<f64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Writes `cards` as CSV in the same shape [`plan`] reads, so an export re-imports as-is:
/// one row per card, labels and components joined with `;`, then one column per custom
/// field that any of the cards has a value for.
pub async fn export(db: &Db, cards: &[card::Card]) -> AppResult<String> {
    let names: HashMap<String, String> = custom_field::list(db)
        .await?
        .into_iter()
        .map(|f| (f.id, f.name))
        .collect();
    let mut rows = Vec::with_capacity(cards.len());
    let mut field_columns: Vec<String> = Vec::new();
    for c in cards {
        let row: ExportRow = sqlx::query_as(
            "SELECT t.level, c.key, p.key AS parent_key, c.summary, t.name AS type_name, \
                    s.name AS status_name, pr.name AS priority_name, a.username AS assignee, \
                    r.username AS reporter, m.name AS milestone, c.due_date, c.start_date, \
                    c.estimate, c.created_at, c.updated_at \
               FROM cards c JOIN card_types t ON t.id = c.type_id \
               JOIN statuses s ON s.id = c.status_id \
               LEFT JOIN cards p ON p.id = c.parent_id \
               LEFT JOIN priorities pr ON pr.id = c.priority_id \
               LEFT JOIN users a ON a.id = c.assignee_id \
               LEFT JOIN users r ON r.id = c.reporter_id \
               LEFT JOIN milestones m ON m.id = c.milestone_id \
              WHERE c.id = ?",
        )
        .bind(&c.id)
        .fetch_one(db.reader())
        .await?;
        let labels: Vec<String> = sqlx::query_scalar(
            "SELECT t.name FROM card_tags ct JOIN tags t ON t.id = ct.tag_id \
             WHERE ct.card_id = ? ORDER BY t.name",
        )
        .bind(&c.id)
        .fetch_all(db.reader())
        .await?;
        let components: Vec<String> = sqlx::query_scalar(
            "SELECT k.name FROM card_components cc JOIN components k ON k.id = cc.component_id \
             WHERE cc.card_id = ? ORDER BY k.name",
        )
        .bind(&c.id)
        .fetch_all(db.reader())
        .await?;
        let mut values = HashMap::new();
        for (field_id, value) in custom_field::values(db, &c.id).await? {
            if let Some(name) = names.get(&field_id) {
                if !field_columns.contains(name) {
                    field_columns.push(name.clone());
                }
                values.insert(name.clone(), custom_field::display(&value));
            }
        }
        rows.push((row, labels, components, values));
    }
    field_columns.sort();
    // Parents before children, so the file re-imports (a parent row must precede its
    // children); otherwise the caller's order is kept.
    rows.sort_by_key(|(row, ..)| std::cmp::Reverse(row.level));

    let mut out = String::new();
    let header: Vec<String> = EXPORT_HEADERS
        .iter()
        .map(|h| field(h))
        .chain(field_columns.iter().map(|h| field(h)))
        .collect();
    out.push_str(&header.join(","));
    out.push_str("\r\n");
    let date = |d: Option<NaiveDate>| d.map(|d| d.to_string()).unwrap_or_default();
    for (row, labels, components, values) in rows {
        let mut line = vec![
            row.key.clone(),
            row.key,
            row.parent_key.unwrap_or_default(),
            row.summary,
            row.type_name,
            row.status_name,
            row.priority_name.unwrap_or_default(),
            row.assignee.unwrap_or_default(),
            row.reporter.unwrap_or_default(),
            labels.join(";"),
            components.join(";"),
            row.milestone.unwrap_or_default(),
            date(row.due_date),
            date(row.start_date),
            row.estimate.map(|e| e.to_string()).unwrap_or_default(),
            row.created_at.to_rfc3339(),
            row.updated_at.to_rfc3339(),
        ];
        for column in &field_columns {
            line.push(values.get(column).cloned().unwrap_or_default());
        }
        let line: Vec<String> = line.iter().map(|v| field(&defuse(v))).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    Ok(out)
}

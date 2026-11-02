//! The rules each project template starts with — the features that make a template more than
//! a list of statuses. Every one is ordinary, editable, disableable data.
//!
//! - **Job search** (the killer app): every day, any application sitting in Applied with no
//!   change for a week gets a "Follow up?" comment and a due date two days out.
//! - **3D**: approving an asset creates its "Export for delivery" step.
//! - **Programming** `[SW]`: a Highest-priority Bug is assigned to the project lead and pulled
//!   into the active cycle the moment it is filed.

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::automation::model::RuleSpec;
use crate::automation::store::{self, RuleInput};
use crate::domain::template::Template;
use crate::error::{AppError, AppResult};

/// `(name, description, spec)` for a template.
pub fn presets(template: Template) -> Vec<(&'static str, &'static str, serde_json::Value)> {
    match template {
        Template::JobSearch => vec![(
            "Nudge stale applications",
            "Daily: an application in Applied with no change for 7 days gets a follow-up prompt and a due date.",
            json!({
                "trigger": { "type": "scheduled", "intervalMinutes": 1440, "aql": "status = Applied AND updated <= -7d" },
                "conditions": [],
                "actions": [
                    { "type": "comment", "body": "No movement for a week — follow up with {{card.summary}}?" },
                    { "type": "edit", "dueDate": "{{now.plusDays(2)}}" },
                ],
            }),
        )],
        Template::ThreeDModeling => vec![(
            "Export approved assets",
            "When an asset is approved, create its delivery step.",
            json!({
                "trigger": { "type": "cardTransitioned", "to": "Approved" },
                "conditions": [],
                "actions": [
                    { "type": "createCard", "summary": "Export {{card.summary}} for delivery", "asChild": true },
                ],
            }),
        )],
        Template::Programming => vec![(
            "Triage critical bugs",
            "A Highest-priority Bug goes to the project lead and into the active cycle.",
            json!({
                "trigger": { "type": "fieldChanged", "fields": ["priority", "type"] },
                "conditions": [
                    { "type": "aql", "query": "type = Bug AND priority = Highest AND assignee IS EMPTY" },
                ],
                "actions": [
                    { "type": "assign", "user": "@lead" },
                    { "type": "addToActiveCycle" },
                ],
            }),
        )],
        Template::Blank => Vec::new(),
    }
}

/// Inserts a template's preset rules, owned by `owner_id`.
pub async fn seed(
    tx: &mut sqlx::SqliteConnection,
    template: Template,
    project_id: &str,
    owner_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    for (name, description, spec) in presets(template) {
        let spec: RuleSpec = serde_json::from_value(spec).map_err(AppError::internal)?;
        store::insert(
            &mut *tx,
            project_id,
            owner_id,
            &RuleInput {
                name,
                description: Some(description),
                enabled: true,
                spec: &spec,
            },
            now,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_a_valid_rule() {
        for template in [
            Template::Programming,
            Template::ThreeDModeling,
            Template::JobSearch,
            Template::Blank,
        ] {
            for (name, _, spec) in presets(template) {
                let spec: RuleSpec =
                    serde_json::from_value(spec).unwrap_or_else(|e| panic!("{name}: {e}"));
                spec.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
            }
        }
    }
}

//! `project_agent_settings`: how Claude Code runs in a project.
//!
//! The defaults are the least permissive that work: `dontAsk` (act without prompting, bounded
//! by the allow list), Atlas's own short tool list, 50 turns, $5, and the machine's own Claude
//! login rather than an API key. `bypassPermissions` and API-key billing are both deliberate,
//! explicit opt-ins — the API refuses them without an acknowledgement flag.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// The tools a run may use when the project names none.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &["Read", "Edit", "Write", "Bash", "Grep", "Glob"];

/// The permission modes the CLI accepts.
pub const PERMISSION_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "plan",
    "dontAsk",
    "bypassPermissions",
];

/// A project's agent settings, defaults filled in.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    /// `--permission-mode`.
    pub permission_mode: String,
    /// `--allowedTools`. Empty = [`DEFAULT_ALLOWED_TOOLS`].
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools`.
    pub disallowed_tools: Vec<String>,
    /// `--max-turns`.
    pub max_turns: u32,
    /// `--max-budget-usd`.
    pub max_budget_usd: f64,
    /// Where a completed run moves its card; `None` = the default "In Review"-like status.
    pub destination_status_id: Option<String>,
    /// Post the result as a comment.
    pub post_summary: bool,
    /// Bill an API key instead of the machine's Claude login.
    pub use_api_key: bool,
    /// Which Anthropic credential, when `use_api_key`.
    pub api_credential_id: Option<String>,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            permission_mode: "dontAsk".to_owned(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            max_turns: 50,
            max_budget_usd: 5.0,
            destination_status_id: None,
            post_summary: true,
            use_api_key: false,
            api_credential_id: None,
        }
    }
}

impl AgentSettings {
    /// The allow list to actually pass.
    pub fn effective_allowed_tools(&self) -> Vec<String> {
        if self.allowed_tools.is_empty() {
            DEFAULT_ALLOWED_TOOLS
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        } else {
            self.allowed_tools.clone()
        }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    permission_mode: String,
    allowed_tools: String,
    disallowed_tools: String,
    max_turns: i64,
    max_budget_usd: f64,
    destination_status_id: Option<String>,
    post_summary: bool,
    use_api_key: bool,
    api_credential_id: Option<String>,
}

/// A project's settings, or the defaults.
pub async fn get(db: &Db, project_id: &str) -> AppResult<AgentSettings> {
    let row = sqlx::query_as::<_, Row>(
        "SELECT permission_mode, allowed_tools, disallowed_tools, max_turns, max_budget_usd, \
         destination_status_id, post_summary, use_api_key, api_credential_id \
         FROM project_agent_settings WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(db.reader())
    .await?;
    let Some(row) = row else {
        return Ok(AgentSettings::default());
    };
    Ok(AgentSettings {
        permission_mode: row.permission_mode,
        allowed_tools: serde_json::from_str(&row.allowed_tools).unwrap_or_default(),
        disallowed_tools: serde_json::from_str(&row.disallowed_tools).unwrap_or_default(),
        max_turns: u32::try_from(row.max_turns).unwrap_or(50),
        max_budget_usd: row.max_budget_usd,
        destination_status_id: row.destination_status_id,
        post_summary: row.post_summary,
        use_api_key: row.use_api_key,
        api_credential_id: row.api_credential_id,
    })
}

/// A tool rule: a bare name or a scoped rule like `Bash(git diff *)`. Commas would split it.
fn check_tools(tools: &[String]) -> AppResult<()> {
    if tools.len() > 50 {
        return Err(AppError::Validation("At most 50 tool rules.".to_owned()));
    }
    for tool in tools {
        if tool.trim().is_empty()
            || tool.len() > 200
            || tool.contains(',')
            || tool.chars().any(char::is_control)
        {
            return Err(AppError::Validation(format!(
                "{tool:?} is not a tool rule."
            )));
        }
    }
    Ok(())
}

/// Checks settings. `acknowledged` must be true to choose `bypassPermissions` or API billing.
pub fn validate(settings: &AgentSettings, acknowledged: bool) -> AppResult<()> {
    if !PERMISSION_MODES.contains(&settings.permission_mode.as_str()) {
        return Err(AppError::Validation(format!(
            "{:?} is not a permission mode.",
            settings.permission_mode
        )));
    }
    if settings.permission_mode == "bypassPermissions" && !acknowledged {
        return Err(AppError::Validation(
            "bypassPermissions lets a run execute any command in the workspace with no check. \
             Confirm it deliberately (acknowledgeRisk: true)."
                .to_owned(),
        ));
    }
    if settings.use_api_key {
        if settings.api_credential_id.is_none() {
            return Err(AppError::Validation(
                "Choose which Anthropic API key to bill.".to_owned(),
            ));
        }
        if !acknowledged {
            return Err(AppError::Validation(
                "Using an API key bills the Anthropic API instead of the machine's Claude \
                 subscription. Confirm it deliberately (acknowledgeRisk: true)."
                    .to_owned(),
            ));
        }
    }
    check_tools(&settings.allowed_tools)?;
    check_tools(&settings.disallowed_tools)?;
    if !(1..=500).contains(&settings.max_turns) {
        return Err(AppError::Validation(
            "Max turns must be 1 to 500.".to_owned(),
        ));
    }
    if !(settings.max_budget_usd > 0.0 && settings.max_budget_usd <= 1000.0) {
        return Err(AppError::Validation(
            "The budget must be above $0 and at most $1000.".to_owned(),
        ));
    }
    Ok(())
}

/// Stores a project's settings.
pub async fn put(
    db: &Db,
    project_id: &str,
    settings: &AgentSettings,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "INSERT INTO project_agent_settings (project_id, permission_mode, allowed_tools, \
         disallowed_tools, max_turns, max_budget_usd, destination_status_id, post_summary, \
         use_api_key, api_credential_id, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (project_id) DO UPDATE SET permission_mode = excluded.permission_mode, \
         allowed_tools = excluded.allowed_tools, disallowed_tools = excluded.disallowed_tools, \
         max_turns = excluded.max_turns, max_budget_usd = excluded.max_budget_usd, \
         destination_status_id = excluded.destination_status_id, \
         post_summary = excluded.post_summary, use_api_key = excluded.use_api_key, \
         api_credential_id = excluded.api_credential_id, updated_at = excluded.updated_at",
    )
    .bind(project_id)
    .bind(&settings.permission_mode)
    .bind(serde_json::to_string(&settings.allowed_tools).map_err(AppError::internal)?)
    .bind(serde_json::to_string(&settings.disallowed_tools).map_err(AppError::internal)?)
    .bind(i64::from(settings.max_turns))
    .bind(settings.max_budget_usd)
    .bind(&settings.destination_status_id)
    .bind(settings.post_summary)
    .bind(settings.use_api_key)
    .bind(&settings.api_credential_id)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_risky_choices_need_an_explicit_acknowledgement() {
        let mut s = AgentSettings::default();
        assert!(validate(&s, false).is_ok());
        s.permission_mode = "bypassPermissions".into();
        assert!(validate(&s, false).is_err());
        assert!(validate(&s, true).is_ok());
        let mut s = AgentSettings {
            use_api_key: true,
            ..AgentSettings::default()
        };
        assert!(validate(&s, true).is_err(), "needs a credential");
        s.api_credential_id = Some("c".into());
        assert!(validate(&s, false).is_err());
        assert!(validate(&s, true).is_ok());
    }

    #[test]
    fn tool_rules_cannot_smuggle_a_second_rule_through_a_comma() {
        let s = AgentSettings {
            allowed_tools: vec!["Read,Bash".into()],
            ..AgentSettings::default()
        };
        assert!(validate(&s, false).is_err());
        let s = AgentSettings {
            allowed_tools: vec!["Bash(git diff *)".into()],
            ..AgentSettings::default()
        };
        assert!(validate(&s, false).is_ok());
    }

    #[test]
    fn an_empty_allow_list_means_atlas_defaults_not_the_clis() {
        assert_eq!(
            AgentSettings::default().effective_allowed_tools().len(),
            DEFAULT_ALLOWED_TOOLS.len()
        );
    }
}

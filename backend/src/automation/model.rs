//! What a rule *is*: one trigger, N conditions, N actions — all plain JSON so the rule builder
//! UI, the API and the seed data share one shape.
//!
//! Every field that is shown to or typed by a user accepts smart values (`{{…}}`); every field
//! that names something (a status, a user, a tag) is resolved by name at run time, so a rule
//! survives the thing it names being renamed back and forth — and fails *visibly* in the audit
//! log, not silently, when it no longer resolves.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};

/// The longest an interval schedule may be, in minutes: a week.
pub const MAX_INTERVAL_MINUTES: i64 = 7 * 24 * 60;
/// The shortest: five minutes. A rule that wants to run every second wants a trigger instead.
pub const MIN_INTERVAL_MINUTES: i64 = 5;
/// The most actions one rule may hold, counting nested if/else branches.
pub const MAX_ACTIONS: usize = 50;
/// The most cards a lookup or a scheduled run may touch.
pub const MAX_LOOKUP: i64 = 100;

/// When a rule runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Trigger {
    /// A card was created in the project.
    CardCreated,
    /// A card changed status, optionally only from/to particular statuses (by name).
    #[serde(rename_all = "camelCase")]
    CardTransitioned {
        /// Only from this status.
        #[serde(default)]
        from: Option<String>,
        /// Only to this status.
        #[serde(default)]
        to: Option<String>,
    },
    /// One of these fields changed (`priority`, `assignee`, `summary`, `cf:<id>`, …). Empty
    /// means any field.
    FieldChanged {
        /// The fields.
        #[serde(default)]
        fields: Vec<String>,
    },
    /// The assignee changed to someone.
    CardAssigned,
    /// A comment was posted.
    Commented,
    /// Every `interval_minutes`, for each card matching `aql` (in this project).
    #[serde(rename_all = "camelCase")]
    Scheduled {
        /// Minutes between runs.
        interval_minutes: i64,
        /// Which cards. Empty = run once with no card.
        #[serde(default)]
        aql: Option<String>,
    },
    /// Only when someone presses "Run" on a card.
    Manual,
    /// When an HTTP POST arrives at the rule's secret webhook URL, for each card matching
    /// `aql` (or once with no card).
    IncomingWebhook {
        /// Which cards.
        #[serde(default)]
        aql: Option<String>,
    },
}

impl Trigger {
    /// The trigger's name, for the audit log.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CardCreated => "cardCreated",
            Self::CardTransitioned { .. } => "cardTransitioned",
            Self::FieldChanged { .. } => "fieldChanged",
            Self::CardAssigned => "cardAssigned",
            Self::Commented => "commented",
            Self::Scheduled { .. } => "scheduled",
            Self::Manual => "manual",
            Self::IncomingWebhook { .. } => "incomingWebhook",
        }
    }
}

/// How a field condition compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum CompareOp {
    /// Equal, case-insensitively.
    Equals,
    /// Not equal.
    NotEquals,
    /// Contains, case-insensitively.
    Contains,
    /// Empty / unset.
    IsEmpty,
    /// Set.
    IsNotEmpty,
}

/// A condition: all of a rule's conditions must hold for its actions to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Condition {
    /// The card matches an AQL query.
    Aql {
        /// The query, `AND`ed with `key = <the card>`.
        query: String,
    },
    /// A smart value compared to a value.
    Field {
        /// A smart value, e.g. `{{card.priority.name}}`.
        value: String,
        /// How to compare.
        op: CompareOp,
        /// What to compare against (ignored by `isEmpty`/`isNotEmpty`). Smart values allowed.
        #[serde(default)]
        expected: Option<String>,
    },
    /// Who triggered it (by username).
    #[serde(rename_all = "camelCase")]
    User {
        /// The usernames.
        usernames: Vec<String>,
        /// Invert: anyone *but* these.
        #[serde(default)]
        negate: bool,
    },
    /// A smart value matches a regular expression.
    Regex {
        /// A smart value.
        value: String,
        /// The pattern.
        pattern: String,
    },
}

/// An outbound HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

/// Something a rule does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// Set fields. Each is a smart value; an empty result clears the field.
    #[serde(rename_all = "camelCase")]
    Edit {
        /// New summary.
        #[serde(default)]
        summary: Option<String>,
        /// New description.
        #[serde(default)]
        description: Option<String>,
        /// A priority, by name.
        #[serde(default)]
        priority: Option<String>,
        /// A due date (`YYYY-MM-DD`, or a smart date).
        #[serde(default)]
        due_date: Option<String>,
        /// An estimate.
        #[serde(default)]
        estimate: Option<String>,
    },
    /// Move the card to a status, by name — through the workflow, so validators still run.
    Transition {
        /// The status.
        to: String,
    },
    /// Post a comment.
    Comment {
        /// Markdown, smart values allowed.
        body: String,
    },
    /// Set the assignee: a username, `{{reporter}}`/`{{initiator}}`-style smart value, or
    /// empty to unassign. The special value `@lead` is the project lead.
    Assign {
        /// Who.
        user: String,
    },
    /// Create a card in the project (or as a child of this one).
    #[serde(rename_all = "camelCase")]
    CreateCard {
        /// Summary.
        summary: String,
        /// Description.
        #[serde(default)]
        description: Option<String>,
        /// A card type, by name. Default: the project's default type.
        #[serde(default)]
        card_type: Option<String>,
        /// Make it a child of the triggering card.
        #[serde(default)]
        as_child: bool,
    },
    /// Create a child card for each summary.
    #[serde(rename_all = "camelCase")]
    CreateChildren {
        /// One per child.
        summaries: Vec<String>,
        /// A card type, by name. Default: the lowest level the hierarchy allows.
        #[serde(default)]
        card_type: Option<String>,
    },
    /// Clone the card (summary, description, type, priority).
    #[serde(rename_all = "camelCase")]
    Clone {
        /// Appended to the clone's summary.
        #[serde(default)]
        summary_suffix: Option<String>,
    },
    /// Link the card to another.
    #[serde(rename_all = "camelCase")]
    Link {
        /// The other card's key (smart values allowed).
        target_key: String,
        /// How, e.g. `relates_to`.
        link_type: String,
    },
    /// Log time.
    LogWork {
        /// `2h 30m`.
        duration: String,
        /// A note.
        #[serde(default)]
        note: Option<String>,
    },
    /// Add watchers, by username.
    AddWatchers {
        /// Who.
        usernames: Vec<String>,
    },
    /// Put a tag on the card, creating it in the project if it does not exist.
    AddTag {
        /// The tag.
        name: String,
    },
    /// Add the card to the project's active cycle.
    AddToActiveCycle,
    /// Send an email.
    SendEmail {
        /// An address, or a smart value such as `{{card.assignee.email}}`.
        to: String,
        /// Subject.
        subject: String,
        /// Plain text body.
        body: String,
    },
    /// Make an HTTP request. One generic action instead of twenty bespoke integrations.
    /// Private and loopback addresses are refused (SSRF). The response is available to later
    /// actions as `{{webResponse.status}}` and `{{webResponse.body}}`.
    WebRequest {
        /// `https://…`, smart values allowed.
        url: String,
        /// The verb.
        method: HttpMethod,
        /// Headers, values smart.
        #[serde(default)]
        headers: Vec<(String, String)>,
        /// A body, smart values allowed.
        #[serde(default)]
        body: Option<String>,
    },
    /// Set `{{vars.<name>}}` for later actions.
    SetVariable {
        /// The name.
        name: String,
        /// The value.
        value: String,
    },
    /// Run an AQL query (≤100 cards) and expose them as `{{lookupCards}}` (a list; each item
    /// has `key`, `summary`, `status`).
    LookupCards {
        /// The query.
        aql: String,
    },
    /// Branch on conditions.
    #[serde(rename = "ifElse")]
    IfElse {
        /// All must hold for `then`.
        conditions: Vec<Condition>,
        /// Run when they hold. (Documented as plain objects: `Action` is recursive here, and a
        /// recursive inline schema would make the OpenAPI generator recurse forever.)
        #[schema(value_type = Vec<Object>)]
        then: Vec<Action>,
        /// Run otherwise.
        #[serde(default, rename = "else")]
        #[schema(value_type = Vec<Object>)]
        otherwise: Vec<Action>,
    },
}

impl Action {
    /// The action's name, for the audit log.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Edit { .. } => "edit",
            Self::Transition { .. } => "transition",
            Self::Comment { .. } => "comment",
            Self::Assign { .. } => "assign",
            Self::CreateCard { .. } => "createCard",
            Self::CreateChildren { .. } => "createChildren",
            Self::Clone { .. } => "clone",
            Self::Link { .. } => "link",
            Self::LogWork { .. } => "logWork",
            Self::AddWatchers { .. } => "addWatchers",
            Self::AddTag { .. } => "addTag",
            Self::AddToActiveCycle => "addToActiveCycle",
            Self::SendEmail { .. } => "sendEmail",
            Self::WebRequest { .. } => "webRequest",
            Self::SetVariable { .. } => "setVariable",
            Self::LookupCards { .. } => "lookupCards",
            Self::IfElse { .. } => "ifElse",
        }
    }

    /// Whether this action needs a card to act on.
    pub fn needs_card(&self) -> bool {
        !matches!(
            self,
            Self::SendEmail { .. }
                | Self::WebRequest { .. }
                | Self::SetVariable { .. }
                | Self::LookupCards { .. }
                | Self::IfElse { .. }
                | Self::CreateCard {
                    as_child: false,
                    ..
                }
        )
    }
}

/// A rule's definition — what the builder edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuleSpec {
    /// When.
    pub trigger: Trigger,
    /// Whether (all must hold).
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// What.
    pub actions: Vec<Action>,
}

fn count_actions(actions: &[Action]) -> usize {
    actions
        .iter()
        .map(|a| match a {
            Action::IfElse {
                then, otherwise, ..
            } => 1 + count_actions(then) + count_actions(otherwise),
            _ => 1,
        })
        .sum()
}

fn check_conditions(conditions: &[Condition]) -> AppResult<()> {
    for condition in conditions {
        if let Condition::Regex { pattern, .. } = condition {
            regex::RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .build()
                .map_err(|err| {
                    AppError::Validation(format!("Invalid pattern {pattern:?}: {err}"))
                })?;
        }
        if let Condition::Aql { query } = condition {
            crate::aql::parse(query)
                .map_err(|err| AppError::Validation(format!("Invalid AQL {query:?}: {err}")))?;
        }
    }
    Ok(())
}

fn check_actions(actions: &[Action]) -> AppResult<()> {
    for action in actions {
        match action {
            Action::IfElse {
                conditions,
                then,
                otherwise,
            } => {
                check_conditions(conditions)?;
                check_actions(then)?;
                check_actions(otherwise)?;
            }
            Action::LookupCards { aql } => {
                crate::aql::parse(aql)
                    .map_err(|err| AppError::Validation(format!("Invalid AQL {aql:?}: {err}")))?;
            }
            Action::LogWork { duration, .. } if !duration.contains("{{") => {
                crate::domain::worklog::parse_duration(duration)?;
            }
            Action::Link { link_type, .. }
                if crate::domain::links::LinkType::parse(link_type).is_none() =>
            {
                return Err(AppError::Validation(format!(
                    "{link_type:?} is not a link type."
                )));
            }
            Action::SetVariable { name, .. }
                if name.is_empty()
                    || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                return Err(AppError::Validation(format!(
                    "{name:?} is not a variable name — letters, digits and _ only."
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

impl RuleSpec {
    /// Checks the rule's shape: bounded size, valid patterns and queries, a sane schedule.
    pub fn validate(&self) -> AppResult<()> {
        if self.actions.is_empty() {
            return Err(AppError::Validation(
                "A rule needs at least one action.".to_owned(),
            ));
        }
        if count_actions(&self.actions) > MAX_ACTIONS {
            return Err(AppError::Validation(format!(
                "A rule may have at most {MAX_ACTIONS} actions."
            )));
        }
        match &self.trigger {
            Trigger::Scheduled {
                interval_minutes,
                aql,
            } => {
                if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(interval_minutes) {
                    return Err(AppError::Validation(format!(
                        "A schedule must be every {MIN_INTERVAL_MINUTES} minutes to every week."
                    )));
                }
                if let Some(aql) = aql.as_deref().filter(|q| !q.trim().is_empty()) {
                    crate::aql::parse(aql)
                        .map_err(|err| AppError::Validation(format!("Invalid AQL: {err}")))?;
                }
            }
            Trigger::IncomingWebhook { aql: Some(aql) } if !aql.trim().is_empty() => {
                crate::aql::parse(aql)
                    .map_err(|err| AppError::Validation(format!("Invalid AQL: {err}")))?;
            }
            _ => {}
        }
        check_conditions(&self.conditions)?;
        check_actions(&self.actions)
    }

    /// Parses a stored spec.
    pub fn from_json(trigger: &str, conditions: &str, actions: &str) -> AppResult<Self> {
        let parse = |raw: &str| -> AppResult<Value> {
            serde_json::from_str(raw).map_err(AppError::internal)
        };
        serde_json::from_value(serde_json::json!({
            "trigger": parse(trigger)?,
            "conditions": parse(conditions)?,
            "actions": parse(actions)?,
        }))
        .map_err(AppError::internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rules_round_trip_through_json_in_the_documented_shape() {
        let spec: RuleSpec = serde_json::from_value(json!({
            "trigger": { "type": "scheduled", "intervalMinutes": 1440, "aql": "status = Applied" },
            "conditions": [{ "type": "field", "value": "{{card.priority.name}}", "op": "equals", "expected": "High" }],
            "actions": [
                { "type": "comment", "body": "Follow up?" },
                { "type": "ifElse", "conditions": [], "then": [{ "type": "addToActiveCycle" }], "else": [] },
                { "type": "edit", "dueDate": "{{now.plusDays(2)}}" },
            ],
        }))
        .unwrap();
        assert!(spec.validate().is_ok());
        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(back["trigger"]["intervalMinutes"], 1440);
        assert_eq!(back["actions"][1]["type"], "ifElse");
        assert_eq!(back["actions"][1]["else"], json!([]));
    }

    #[test]
    fn validation_catches_what_would_fail_at_run_time() {
        let base = |trigger: Value, actions: Value| -> RuleSpec {
            serde_json::from_value(json!({ "trigger": trigger, "actions": actions })).unwrap()
        };
        let comment = json!([{ "type": "comment", "body": "x" }]);
        assert!(
            base(json!({ "type": "manual" }), json!([]))
                .validate()
                .is_err()
        );
        assert!(
            base(
                json!({ "type": "scheduled", "intervalMinutes": 1 }),
                comment.clone()
            )
            .validate()
            .is_err()
        );
        assert!(
            base(
                json!({ "type": "scheduled", "intervalMinutes": 60, "aql": "status = = x" }),
                comment.clone()
            )
            .validate()
            .is_err()
        );
        assert!(
            base(
                json!({ "type": "manual" }),
                json!([{ "type": "logWork", "duration": "soon" }])
            )
            .validate()
            .is_err()
        );
        assert!(
            base(
                json!({ "type": "manual" }),
                json!([{ "type": "link", "targetKey": "A-1", "linkType": "loves" }])
            )
            .validate()
            .is_err()
        );
        assert!(
            base(
                json!({ "type": "manual" }),
                json!([{ "type": "setVariable", "name": "a b", "value": "x" }])
            )
            .validate()
            .is_err()
        );
        let regex: RuleSpec = serde_json::from_value(json!({
            "trigger": { "type": "manual" },
            "conditions": [{ "type": "regex", "value": "{{card.summary}}", "pattern": "(" }],
            "actions": comment,
        }))
        .unwrap();
        assert!(regex.validate().is_err());
    }
}

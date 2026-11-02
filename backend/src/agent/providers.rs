//! Provider-specific CLI event streams, normalized to the existing run lifecycle.
//! Raw lines are still persisted verbatim; normalization only interprets the outcome.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::claude_code::{self, Event};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    #[default]
    Claude,
    Codex,
    Antigravity,
}

impl AgentProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }
}

impl std::str::FromStr for AgentProvider {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "antigravity" => Ok(Self::Antigravity),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProviderOutput {
    provider: AgentProvider,
    session_id: String,
    last_message: Option<String>,
}

impl ProviderOutput {
    pub(crate) fn new(provider: AgentProvider, session_id: String) -> Self {
        Self {
            provider,
            session_id,
            last_message: None,
        }
    }

    pub(crate) fn parse(&mut self, line: &str) -> Result<Option<Event>, serde_json::Error> {
        if self.provider == AgentProvider::Claude {
            return claude_code::parse_line(line);
        }
        if line.trim().is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(line)?;
        match self.provider {
            AgentProvider::Codex => match value["type"].as_str() {
                Some("item.completed") => {
                    if value["item"]["type"] == "agent_message" {
                        self.last_message = value["item"]["text"].as_str().map(ToOwned::to_owned);
                    }
                    Ok(Some(Event::Unknown))
                }
                Some("turn.completed") => self.result(
                    false,
                    self.last_message.as_deref(),
                    None,
                    value.get("usage"),
                ),
                Some("turn.failed") => {
                    self.result(true, None, value["error"]["message"].as_str(), None)
                }
                _ => Ok(Some(Event::Unknown)),
            },
            AgentProvider::Antigravity => {
                if value["event"] != "result" {
                    return Ok(Some(Event::Unknown));
                }
                let result = &value["result"];
                self.result(
                    result["status"] != "SUCCESS",
                    result["response"].as_str(),
                    result["error"].as_str(),
                    result.get("usage"),
                )
            }
            AgentProvider::Claude => unreachable!("handled above"),
        }
    }

    fn result(
        &self,
        failed: bool,
        text: Option<&str>,
        error: Option<&str>,
        usage: Option<&Value>,
    ) -> Result<Option<Event>, serde_json::Error> {
        let result = json!({
            "type": "result", "subtype": if failed { "error" } else { "success" },
            "is_error": failed, "result": text, "errors": error.into_iter().collect::<Vec<_>>(),
            "session_id": self.session_id, "num_turns": 1, "total_cost_usd": 0.0,
            "terminal_reason": if failed { "error" } else { "completed" }, "usage": usage,
        });
        serde_json::from_value(result).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::super::claude_code::{Outcome, outcome};
    use super::*;

    #[test]
    fn codex_uses_the_final_assistant_message_and_terminal_outcome() {
        let mut output = ProviderOutput::new(AgentProvider::Codex, "run".to_owned());
        output.parse(r#"{"type":"item.completed","item":{"type":"agent_message","text":"Implemented and tested."}}"#).unwrap();
        let Some(Event::Result(result)) = output
            .parse(r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}"#)
            .unwrap()
        else {
            panic!("missing result")
        };
        assert_eq!(outcome(&result), Outcome::Completed);
        assert_eq!(result.result.as_deref(), Some("Implemented and tested."));
        assert_eq!(result.usage.unwrap()["input_tokens"], 100);
    }

    #[test]
    fn codex_errors_are_not_successes() {
        let mut output = ProviderOutput::new(AgentProvider::Codex, "run".to_owned());
        let Some(Event::Result(result)) = output
            .parse(r#"{"type":"turn.failed","error":{"message":"Authentication required"}}"#)
            .unwrap()
        else {
            panic!("missing result")
        };
        assert_eq!(outcome(&result), Outcome::Failed);
        assert_eq!(result.errors, ["Authentication required"]);
    }

    #[test]
    fn antigravity_terminal_status_is_authoritative() {
        let mut output = ProviderOutput::new(AgentProvider::Antigravity, "run".to_owned());
        for (status, expected) in [
            ("SUCCESS", Outcome::Completed),
            ("ERROR", Outcome::Failed),
            ("WAITING", Outcome::Failed),
        ] {
            let line = json!({"event":"result","result":{"status":status,"response":"Summary"}})
                .to_string();
            let Some(Event::Result(result)) = output.parse(&line).unwrap() else {
                panic!("missing result")
            };
            assert_eq!(outcome(&result), expected);
        }
    }
}

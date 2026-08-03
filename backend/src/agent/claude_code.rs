//! Parsing and interpreting the Claude Code CLI's `stream-json` NDJSON event stream.
//!
//! Pure interpretation only — no process spawning, no I/O. The runner (a later increment in
//! this phase) is the thin glue that actually spawns `claude -p …` and feeds its stdout
//! lines through [`parse_line`]; everything here is tested without a real subprocess, the
//! same split [`crate::integrations::github::client`] draws between its pure interpretation
//! functions and the `reqwest` calls that feed them.
//!
//! See `docs/research/claude-code-cli.md` for the CLI behaviour this encodes. The two traps
//! [`outcome`] exists to close:
//!
//! - `subtype` stays `"success"` on API/auth failures, with `is_error: true` the only real
//!   signal — matching on `subtype` alone reports a 401 as a successful empty run.
//! - `result` is an **absent key**, not `null`, on every error subtype, so it must be
//!   `Option<String>` or deserialising the very event that reports a failure panics.
//!
//! A third, quieter trap [`outcome`] also closes: a run where every tool call was denied
//! still reports `is_error: false` — the model narrates the denial in prose, which reads as
//! a real answer unless `permission_denials` is inspected too.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

/// One line of the CLI's `stream-json` output, already parsed.
///
/// The schema is an open, fast-moving union — the SDK's own type declares roughly three
/// dozen variants (task events, hook events, compact boundaries, notifications…) and the
/// CLI has been observed emitting ones undocumented even there. [`Event::Unknown`] is not a
/// fallback for a bug in this module; it is the documented shape of an event this version of
/// Atlas does not need to understand yet, and every future CLI release is expected to add
/// more of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    System(SystemEvent),
    Assistant(MessageEvent),
    User(MessageEvent),
    StreamEvent(Value),
    RateLimitEvent(Value),
    Result(ResultEvent),
    #[serde(other)]
    Unknown,
}

/// Parses one line of the CLI's stdout.
///
/// `Ok(None)` for a blank line — the CLI itself never emits one, but a caller reading a
/// persisted transcript file line-by-line may see a trailing empty line, and treating that
/// as a parse failure would be a defect in the reader, not a signal about the transcript.
pub fn parse_line(line: &str) -> Result<Option<Event>, serde_json::Error> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(trimmed).map(Some)
}

/// The first event of every run: `system`/`init`.
///
/// Other `system` subtypes (`status`, `api_retry`, `plugin_install`) exist but nothing in
/// Atlas acts on them yet, so `subtype` stays a plain `String` rather than a closed enum —
/// deliberately, since the CLI is documented emitting values (`apiKeySource: "none"`)
/// outside its own SDK's declared unions. Everything not named below lands in `extra` rather
/// than being dropped.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key_source: Option<String>,
    #[serde(default)]
    pub claude_code_version: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// An `assistant` or `user` event.
///
/// Tool **results** arrive as `type: "user"`, not a distinct type — the raw Anthropic
/// message sits under `message` either way, and a tool-result `user` event additionally
/// carries the structured `tool_use_result` alongside it.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageEvent {
    pub session_id: String,
    /// `None` for the main conversation; set to the spawning tool call's id for a
    /// subagent's messages.
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    pub message: Value,
    #[serde(default)]
    pub tool_use_result: Option<Value>,
}

/// The terminal event of a turn: `type: "result"`.
///
/// In one-shot mode this is the last line and the process then exits. In streaming-input
/// mode (one long-lived process, multiple turns) it is the **turn boundary**, not process
/// exit — one `result` event lands per turn, all sharing one `session_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct ResultEvent {
    /// NOT a closed enum — see the module doc. `"success"` alone is never proof of success.
    pub subtype: String,
    pub is_error: bool,
    /// Absent (not `null`) on every error subtype — never remove the `Option`.
    #[serde(default)]
    pub result: Option<String>,
    /// Present only on error subtypes.
    #[serde(default)]
    pub errors: Vec<String>,
    pub session_id: String,
    pub num_turns: u32,
    pub total_cost_usd: f64,
    /// Aggregate token usage. Kept as raw JSON rather than a typed struct until something
    /// in Atlas actually reads individual fields from it (the cost dashboard, later in this
    /// phase) — the shape is documented but wide, and modelling it ahead of a consumer is
    /// exactly the speculative work `CLAUDE.md` asks not to do.
    #[serde(default)]
    pub usage: Option<Value>,
    /// Per-model cost breakdown. `camelCase` keys inside, unlike `usage`'s `snake_case` —
    /// that inconsistency is the CLI's, not a typo here.
    #[serde(rename = "modelUsage", default)]
    pub model_usage: HashMap<String, Value>,
    #[serde(default)]
    pub permission_denials: Vec<PermissionDenial>,
    /// `"completed"` is the only value [`outcome`] treats as clean.
    #[serde(default)]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub api_error_status: Option<u16>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// One tool call the permission layer refused without asking — a `dontAsk`/default-mode run
/// never blocks waiting for a human, it silently denies and keeps going.
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionDenial {
    pub tool_name: String,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
}

/// What a finished run actually amounts to.
///
/// The four cases [`outcome`] actually distinguishes, once `is_error`/`subtype` alone are
/// known to be insufficient. `Completed` and `CompletedWithDenials` both have
/// `is_error: false` on the wire — the difference only exists because `permission_denials`
/// was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Finished cleanly: `terminal_reason == "completed"`, nothing was denied.
    Completed,
    /// Finished cleanly, but at least one tool call was silently denied — the model may
    /// have narrated around a gap rather than actually doing the work.
    CompletedWithDenials,
    /// Hit a limit Atlas itself set (`--max-turns`/`--max-budget-usd`) rather than failing.
    LimitReached,
    /// A real failure: an API/auth error, an aborted run, malformed tool use exhausted, etc.
    Failed,
}

/// Interprets a [`ResultEvent`] per `docs/research/claude-code-cli.md`'s explicit warning:
/// never trust `subtype == "success"` alone — an API/auth failure emits exactly that, with
/// `is_error: true` the only tell — and always check `permission_denials` even when
/// `is_error` is `false`.
#[must_use]
pub fn outcome(result: &ResultEvent) -> Outcome {
    if result.is_error {
        return match result.terminal_reason.as_deref() {
            Some("max_turns" | "budget_exhausted") => Outcome::LimitReached,
            _ => Outcome::Failed,
        };
    }
    if !result.permission_denials.is_empty() {
        return Outcome::CompletedWithDenials;
    }
    Outcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_line_parses_to_none_rather_than_an_error() {
        assert!(parse_line("").unwrap().is_none());
        assert!(parse_line("   \n").unwrap().is_none());
    }

    #[test]
    fn garbage_is_a_parse_error() {
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn an_event_type_this_version_does_not_know_falls_back_to_unknown_not_an_error() {
        // The stream-json schema is open and grows; a card-detail-page-worthy event type
        // added in a future CLI release must not crash every session using it.
        let event = parse_line(r#"{"type":"hook_event","whatever":"future shape"}"#)
            .unwrap()
            .unwrap();
        assert!(matches!(event, Event::Unknown));
    }

    #[test]
    fn a_system_init_event_parses_and_keeps_unmodelled_fields() {
        // Real captured shape (docs/research/claude-code-cli.md), trimmed.
        let line = r#"{
            "type": "system",
            "subtype": "init",
            "cwd": "/home/alli/repo",
            "session_id": "32ed3099-0000-0000-0000-000000000000",
            "model": "claude-opus-4-8[1m]",
            "permissionMode": "default",
            "apiKeySource": "none",
            "claude_code_version": "2.1.211",
            "tools": ["Read", "Edit"]
        }"#;
        let Event::System(system) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a system event");
        };
        assert_eq!(system.subtype, "init");
        assert_eq!(system.session_id, "32ed3099-0000-0000-0000-000000000000");
        assert_eq!(system.model.as_deref(), Some("claude-opus-4-8[1m]"));
        // apiKeySource is deliberately NOT a named field above (the CLI emits values outside
        // its own SDK's declared union) — it must still survive, in `extra`.
        assert_eq!(
            system.extra.get("apiKeySource").and_then(Value::as_str),
            Some("none")
        );
    }

    #[test]
    fn a_tool_result_arrives_as_a_user_event_carrying_tool_use_result() {
        let line = r#"{
            "type": "user",
            "session_id": "s-1",
            "parent_tool_use_id": null,
            "message": {
                "role": "user",
                "content": [{"tool_use_id": "toolu_1", "type": "tool_result", "content": "file contents"}]
            },
            "tool_use_result": {"filePath": "/x", "content": "file contents"}
        }"#;
        let Event::User(user) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a user event");
        };
        assert_eq!(user.session_id, "s-1");
        assert!(user.parent_tool_use_id.is_none());
        assert!(user.tool_use_result.is_some());
    }

    fn success_result_json() -> &'static str {
        // Real captured shape (docs/research/claude-code-cli.md #112), trimmed of the wide
        // `usage`/`modelUsage` objects that this module does not yet model field-by-field.
        r#"{
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "api_error_status": null,
            "duration_ms": 1147,
            "num_turns": 1,
            "result": "hi",
            "session_id": "s-ok",
            "total_cost_usd": 0.0420225,
            "usage": {"input_tokens": 4, "output_tokens": 2},
            "modelUsage": {"claude-opus-4-8[1m]": {"costUSD": 0.04}},
            "permission_denials": [],
            "terminal_reason": "completed"
        }"#
    }

    #[test]
    fn a_clean_success_result_parses_and_reads_as_completed() {
        let Event::Result(result) = parse_line(success_result_json()).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert_eq!(result.subtype, "success");
        assert!(!result.is_error);
        assert_eq!(result.result.as_deref(), Some("hi"));
        assert_eq!(outcome(&result), Outcome::Completed);
    }

    #[test]
    fn a_result_with_a_silent_permission_denial_is_not_reported_as_a_clean_completion() {
        // `is_error` stays false even when every tool call was blocked — the run "succeeds"
        // with an empty-handed answer unless `permission_denials` is inspected too.
        let line = r#"{
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "num_turns": 1,
            "session_id": "s-denied",
            "total_cost_usd": 0.01,
            "permission_denials": [
                {"tool_name": "Bash", "tool_use_id": "toolu_9", "tool_input": {"command": "touch x"}}
            ],
            "terminal_reason": "completed"
        }"#;
        let Event::Result(result) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert_eq!(outcome(&result), Outcome::CompletedWithDenials);
    }

    #[test]
    fn an_auth_failure_keeps_subtype_success_and_is_only_caught_by_is_error() {
        // Real captured shape (docs/research/claude-code-cli.md #114) — the trap this whole
        // module exists to close. A naive `subtype == "success"` check would call this a win.
        let line = r#"{
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "api_error_status": 401,
            "result": "Invalid API key · Fix external API key",
            "session_id": "s-401",
            "num_turns": 0,
            "total_cost_usd": 0,
            "terminal_reason": "api_error"
        }"#;
        let Event::Result(result) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert_eq!(result.subtype, "success", "the trap: subtype lies");
        assert_eq!(outcome(&result), Outcome::Failed);
    }

    #[test]
    fn an_error_result_has_no_result_key_at_all_and_still_parses() {
        // Real captured key set for error_max_turns (docs/research/claude-code-cli.md #116):
        // `result` is ABSENT, not null. A non-Option `result: String` field would fail to
        // deserialise the very event that reports the failure.
        let line = r#"{
            "type": "result",
            "subtype": "error_max_turns",
            "is_error": true,
            "num_turns": 1,
            "session_id": "s-maxturns",
            "total_cost_usd": 0.02,
            "errors": ["Reached maximum number of turns (1)"],
            "terminal_reason": "max_turns"
        }"#;
        let Event::Result(result) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert!(result.result.is_none());
        assert_eq!(result.errors, vec!["Reached maximum number of turns (1)"]);
        assert_eq!(outcome(&result), Outcome::LimitReached);
    }

    #[test]
    fn a_budget_exhausted_result_is_also_a_limit_not_a_failure() {
        let line = r#"{
            "type": "result",
            "subtype": "error_max_budget_usd",
            "is_error": true,
            "num_turns": 3,
            "session_id": "s-budget",
            "total_cost_usd": 5.01,
            "errors": ["Reached maximum budget of $5.00"],
            "terminal_reason": "budget_exhausted"
        }"#;
        let Event::Result(result) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert_eq!(outcome(&result), Outcome::LimitReached);
    }

    #[test]
    fn an_aborted_run_is_a_failure_not_a_limit() {
        let line = r#"{
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "num_turns": 2,
            "session_id": "s-aborted",
            "total_cost_usd": 0.1,
            "errors": ["aborted"],
            "terminal_reason": "aborted_tools"
        }"#;
        let Event::Result(result) = parse_line(line).unwrap().unwrap() else {
            panic!("expected a result event");
        };
        assert_eq!(outcome(&result), Outcome::Failed);
    }
}

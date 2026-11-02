//! Smart values: `{{card.key}}`, `{{card.status.name}}`, `{{changelog.status.fromString}}`,
//! `{{now.plusDays(7)}}`, `{{missing.field|"fallback"}}`, and list sections
//! `{{#card.comments}}{{body}}{{/}}` — Jira's syntax, copied exactly, because people paste
//! rules from Jira documentation and blog posts.
//!
//! # The grammar
//!
//! ```text
//! template := (text | "{{" expr "}}" | "{{#" path "}}" template "{{/}}")*
//! expr     := path ("|" '"' fallback '"')?
//! path     := segment ("." segment)*
//! segment  := name | name "(" arg ")"        -- a method: plusDays(7), toUpperCase()
//! ```
//!
//! A path is looked up in the current scope first (inside a section, the list item), then in
//! the root context. A missing value renders as the fallback, or as nothing. Rendering never
//! fails on data — a typo'd path is an empty string, as in Jira — but every resolution is
//! recorded in a [`Trace`] so the audit log shows exactly what each `{{…}}` became, which is
//! the only thing that makes a misbehaving rule debuggable.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

/// One `{{…}}` and what it resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The expression as written.
    pub expr: String,
    /// What it rendered as.
    pub value: String,
}

/// Every resolution made while rendering, in order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Trace {
    /// The resolutions.
    pub resolutions: Vec<Resolution>,
}

/// Renders `template` against `root`, recording each resolution in `trace`.
pub fn render(template: &str, root: &Value, trace: &mut Trace) -> String {
    render_scoped(template, root, None, trace)
}

fn render_scoped(template: &str, root: &Value, scope: Option<&Value>, trace: &mut Trace) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // An unclosed `{{` is literal text.
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = after[..end].trim();
        let tail = &after[end + 2..];

        if let Some(path) = inner.strip_prefix('#') {
            // A section: find its matching `{{/}}`, honouring nesting.
            let (body, remainder) = split_section(tail);
            let items = lookup(path.trim(), root, scope);
            match items {
                Some(Value::Array(list)) => {
                    for item in &list {
                        out.push_str(&render_scoped(body, root, Some(item), trace));
                    }
                }
                Some(Value::Null | Value::Bool(false)) | None => {}
                Some(other) => out.push_str(&render_scoped(body, root, Some(&other), trace)),
            }
            rest = remainder;
            continue;
        }
        if inner == "/" {
            // A stray close: ignore it.
            rest = tail;
            continue;
        }

        let (path, fallback) = split_fallback(inner);
        let value = lookup(path, root, scope).map_or_else(
            || fallback.clone().unwrap_or_default(),
            |v| match &v {
                Value::Null => fallback.clone().unwrap_or_default(),
                Value::String(s) if s.is_empty() => fallback.clone().unwrap_or_default(),
                _ => display(&v),
            },
        );
        trace.resolutions.push(Resolution {
            expr: inner.to_owned(),
            value: value.clone(),
        });
        out.push_str(&value);
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Splits `body{{/}}rest`, counting nested `{{#…}}` opens.
fn split_section(text: &str) -> (&str, &str) {
    let mut depth = 0usize;
    let mut index = 0usize;
    while let Some(offset) = text[index..].find("{{") {
        let at = index + offset;
        let close = text[at..].find("}}").map(|c| at + c);
        let Some(close) = close else { break };
        let tag = text[at + 2..close].trim();
        if tag.starts_with('#') {
            depth += 1;
        } else if tag == "/" {
            if depth == 0 {
                return (&text[..at], &text[close + 2..]);
            }
            depth -= 1;
        }
        index = close + 2;
    }
    (text, "")
}

/// `path|"fallback"` → (`path`, `Some("fallback")`).
fn split_fallback(expr: &str) -> (&str, Option<String>) {
    match expr.split_once('|') {
        Some((path, fallback)) => {
            let fallback = fallback.trim();
            let fallback = fallback
                .strip_prefix('"')
                .and_then(|f| f.strip_suffix('"'))
                .unwrap_or(fallback);
            (path.trim(), Some(fallback.to_owned()))
        }
        None => (expr.trim(), None),
    }
}

/// Splits a path on dots that are not inside a method's parentheses.
fn segments(path: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in path.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '.' if depth == 0 => {
                parts.push(&path[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&path[start..]);
    parts
}

/// Resolves a path: the scope first (inside a section), then the root.
pub fn lookup(path: &str, root: &Value, scope: Option<&Value>) -> Option<Value> {
    // `{{.}}` is the current list item itself — a list of strings has no fields to name.
    if path == "." {
        return scope.cloned();
    }
    if let Some(scope) = scope
        && let Some(found) = walk(path, scope)
    {
        return Some(found);
    }
    walk(path, root)
}

fn walk(path: &str, start: &Value) -> Option<Value> {
    let mut current = start.clone();
    for segment in segments(path) {
        let segment = segment.trim();
        if segment.is_empty() {
            return None;
        }
        current = match segment.split_once('(') {
            Some((name, rest)) => {
                let arg = rest.strip_suffix(')')?.trim().trim_matches('"');
                method(&current, name, arg)?
            }
            None => match (&current, segment) {
                (Value::Array(items), "size") => Value::from(items.len()),
                (Value::String(s), "size" | "length") => Value::from(s.chars().count()),
                (Value::Object(map), key) => map.get(key)?.clone(),
                (Value::Array(items), index) => items.get(index.parse::<usize>().ok()?)?.clone(),
                _ => return None,
            },
        };
    }
    Some(current)
}

/// The methods a smart value may call: date arithmetic and string case.
fn method(value: &Value, name: &str, arg: &str) -> Option<Value> {
    let amount = || arg.parse::<i64>().ok();
    match name {
        "plusDays" | "minusDays" | "plusHours" | "minusHours" | "plusWeeks" | "minusWeeks" => {
            let text = value.as_str()?;
            let delta = match name {
                "plusDays" => Duration::days(amount()?),
                "minusDays" => Duration::days(-amount()?),
                "plusHours" => Duration::hours(amount()?),
                "minusHours" => Duration::hours(-amount()?),
                "plusWeeks" => Duration::weeks(amount()?),
                _ => Duration::weeks(-amount()?),
            };
            if let Ok(at) = DateTime::parse_from_rfc3339(text) {
                return Some(Value::String((at.with_timezone(&Utc) + delta).to_rfc3339()));
            }
            let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
            Some(Value::String((date + delta).to_string()))
        }
        "toUpperCase" => Some(Value::String(value.as_str()?.to_uppercase())),
        "toLowerCase" => Some(Value::String(value.as_str()?.to_lowercase())),
        "trim" => Some(Value::String(value.as_str()?.trim().to_owned())),
        "jiraDate" | "toDate" => {
            let text = value.as_str()?;
            let date = DateTime::parse_from_rfc3339(text)
                .map(|d| d.date_naive())
                .or_else(|_| NaiveDate::parse_from_str(text, "%Y-%m-%d"))
                .ok()?;
            Some(Value::String(date.to_string()))
        }
        "abbreviate" => {
            let limit = amount().and_then(|n| usize::try_from(n).ok())?;
            let text = value.as_str()?;
            if text.chars().count() <= limit {
                Some(Value::String(text.to_owned()))
            } else {
                let mut cut: String = text.chars().take(limit.saturating_sub(1)).collect();
                cut.push('…');
                Some(Value::String(cut))
            }
        }
        _ => None,
    }
}

/// How a value prints inside text.
fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items.iter().map(display).collect::<Vec<_>>().join(", "),
        Value::Object(map) => {
            // An object prints as its most human field, so `{{card.status}}` reads "Done".
            for key in ["displayName", "name", "key", "summary"] {
                if let Some(Value::String(s)) = map.get(key) {
                    return s.clone();
                }
            }
            value.to_string()
        }
        other => other.to_string(),
    }
}

/// Whether a template contains any smart value at all — a literal needs no rendering.
pub fn is_template(text: &str) -> bool {
    text.contains("{{")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Value {
        json!({
            "card": {
                "key": "JOBS-7",
                "summary": "Acme Corp",
                "status": { "name": "Applied", "category": "in_progress" },
                "assignee": { "displayName": "Alastair", "username": "al" },
                "dueDate": "2026-03-01",
                "comments": [
                    { "body": "first", "author": { "displayName": "A" } },
                    { "body": "second", "author": { "displayName": "B" } },
                ],
                "labels": ["remote", "contract"],
            },
            "changelog": { "status": { "fromString": "Interested", "toString": "Applied" } },
            "now": "2026-01-10T09:00:00+00:00",
        })
    }

    fn r(template: &str) -> String {
        render(template, &ctx(), &mut Trace::default())
    }

    #[test]
    fn paths_resolve_and_objects_print_their_name() {
        assert_eq!(r("{{card.key}}: {{card.summary}}"), "JOBS-7: Acme Corp");
        assert_eq!(r("{{card.status.name}}"), "Applied");
        assert_eq!(r("{{card.status}}"), "Applied");
        assert_eq!(r("{{card.assignee.displayName}}"), "Alastair");
        assert_eq!(
            r("{{changelog.status.fromString}} → {{changelog.status.toString}}"),
            "Interested → Applied"
        );
        assert_eq!(r("{{card.labels}}"), "remote, contract");
        assert_eq!(r("{{card.comments.size}}"), "2");
    }

    #[test]
    fn missing_values_fall_back_or_vanish() {
        assert_eq!(r("[{{missing.field}}]"), "[]");
        assert_eq!(r(r#"{{missing.field|"n/a"}}"#), "n/a");
        assert_eq!(r(r#"{{card.key|"n/a"}}"#), "JOBS-7");
    }

    #[test]
    fn dates_do_arithmetic() {
        assert_eq!(r("{{now.plusDays(7)}}"), "2026-01-17T09:00:00+00:00");
        assert_eq!(r("{{now.plusDays(7).toDate()}}"), "2026-01-17");
        assert_eq!(r("{{card.dueDate.minusDays(1)}}"), "2026-02-28");
        assert_eq!(r("{{card.summary.toUpperCase()}}"), "ACME CORP");
        assert_eq!(r("{{card.summary.abbreviate(4)}}"), "Acm…");
    }

    #[test]
    fn sections_iterate_lists_with_the_item_as_scope() {
        assert_eq!(
            r("{{#card.comments}}<{{body}} by {{author.displayName}} on {{card.key}}>{{/}}"),
            "<first by A on JOBS-7><second by B on JOBS-7>"
        );
        assert_eq!(r("{{#missing}}never{{/}}after"), "after");
        assert_eq!(
            r("{{#card.comments}}[{{#card.labels}}{{.}};{{/}}]{{/}}"),
            "[remote;contract;][remote;contract;]",
            "nested sections close correctly, and {{.}} is the item"
        );
    }

    #[test]
    fn every_resolution_is_traced() {
        let mut trace = Trace::default();
        render(r#"{{card.key}} {{nope|"x"}}"#, &ctx(), &mut trace);
        assert_eq!(
            trace.resolutions,
            vec![
                Resolution {
                    expr: "card.key".into(),
                    value: "JOBS-7".into()
                },
                Resolution {
                    expr: r#"nope|"x""#.into(),
                    value: "x".into()
                },
            ]
        );
    }

    #[test]
    fn malformed_templates_render_literally_rather_than_failing() {
        assert_eq!(r("{{card.key"), "{{card.key");
        assert_eq!(r("a {{/}} b"), "a  b");
        assert_eq!(r("no values"), "no values");
    }
}

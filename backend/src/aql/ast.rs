//! The typed AST, the field/operator vocabulary, and the parse/type error type.
//!
//! Everything a user can type is turned into one of the closed enums here before
//! it is ever compiled. That is the first half of the security model: the parser
//! rejects anything that is not in this grammar, so the compiler only ever sees
//! a `Field` from a fixed list and an `Op` from a fixed enum. The *values* the
//! user supplied ride along as [`Value`]s and become bind parameters and nothing
//! else — see [`crate::aql::compile`].

use std::fmt;

/// A half-open byte range into the original query string.
///
/// Byte offsets rather than character columns, because the lexer walks bytes.
/// [`Span::column_in`] turns one into a 1-based character column for a human
/// message, which is the only place the distinction matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The first byte of the span.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
}

impl Span {
    /// A span covering `start..end`.
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The span from the start of `self` to the end of `other`.
    #[must_use]
    pub fn to(self, other: Self) -> Self {
        Self {
            start: self.start,
            end: other.end,
        }
    }

    /// The 1-based **character** column this span starts at, within `source`.
    ///
    /// Characters, not bytes, so a message about `col 14` lands where a human
    /// counting glyphs would point even when the query contains multibyte text.
    #[must_use]
    pub fn column_in(self, source: &str) -> usize {
        source
            .get(..self.start.min(source.len()))
            .map_or(1, |prefix| prefix.chars().count() + 1)
    }
}

/// A parse or type error, carrying where it happened and why.
///
/// The span is what the frontend underlines and what
/// [`crate::api::search::validate`] returns. [`AqlError::render`] is the
/// human sentence — `unexpected '=' at col 14: the summary field is text; use ~
/// instead` — which is what a bare `POST /search` reports as its `BadRequest`
/// detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AqlError {
    /// The human-readable explanation, without the column prefix.
    pub message: String,
    /// Where in the source it occurred. `None` only for whole-query problems
    /// (an empty filter reference cycle, say) that point at no single token.
    pub span: Option<Span>,
}

impl AqlError {
    /// An error at a span.
    #[must_use]
    pub fn at(span: Span, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: Some(span),
        }
    }

    /// An error with no single position.
    #[must_use]
    pub fn whole(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
        }
    }

    /// The full sentence, with the column worked out against the source query.
    #[must_use]
    pub fn render(&self, source: &str) -> String {
        match self.span {
            Some(span) => format!("{} at col {}", self.message, span.column_in(source)),
            None => self.message.clone(),
        }
    }
}

impl fmt::Display for AqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The whitelisted set of queryable fields.
///
/// This is the *entire* column vocabulary AQL exposes. A field the user names
/// that is not here is rejected at parse time with its span, so no user text
/// ever selects a column. Adding a queryable column is adding a variant here and
/// a match arm in [`crate::aql::compile`] — deliberately two edits, so a new
/// column cannot be reachable without someone having written down how it maps to
/// SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// The owning project, matched by key.
    Project,
    /// The card type, by name.
    Type,
    /// The workflow status, by name.
    Status,
    /// The status category: `todo` / `in_progress` / `done`.
    StatusCategory,
    /// The priority, by name or by rank ordering.
    Priority,
    /// The assignee, by username or `currentUser()`.
    Assignee,
    /// The reporter.
    Reporter,
    /// The creator. Immutable, so no history.
    Creator,
    /// The resolution, by name; `IS EMPTY` for unresolved.
    Resolution,
    /// The parent card, by key; `IS EMPTY` for a root.
    Parent,
    /// When the card was created.
    Created,
    /// When it last changed.
    Updated,
    /// The due date.
    Due,
    /// When it was resolved.
    Resolved,
    /// The start date.
    Started,
    /// The one-line summary. Full-text only.
    Summary,
    /// The markdown description. Full-text only.
    Description,
    /// The all-text pseudo-field: summary + description, `~` only.
    Text,
    /// Tags / labels.
    Labels,
    /// The card key, e.g. `ATLAS-42`.
    Key,
    /// The numeric estimate.
    Estimate,
}

impl Field {
    /// Resolves a field name, case-insensitively. `None` if it is not a field.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let field = match name.to_ascii_lowercase().as_str() {
            "project" => Self::Project,
            "type" | "issuetype" | "cardtype" => Self::Type,
            "status" => Self::Status,
            "statuscategory" => Self::StatusCategory,
            "priority" => Self::Priority,
            "assignee" => Self::Assignee,
            "reporter" => Self::Reporter,
            "creator" => Self::Creator,
            "resolution" => Self::Resolution,
            "parent" => Self::Parent,
            "created" => Self::Created,
            "updated" => Self::Updated,
            "due" | "duedate" => Self::Due,
            "resolved" | "resolutiondate" => Self::Resolved,
            "started" | "startdate" => Self::Started,
            "summary" => Self::Summary,
            "description" => Self::Description,
            "text" => Self::Text,
            "labels" | "label" | "tag" | "tags" => Self::Labels,
            "key" | "id" | "card" => Self::Key,
            "estimate" | "estimation" => Self::Estimate,
            _ => return None,
        };
        Some(field)
    }

    /// The field's canonical spelling, for a normalised echo of the query.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Type => "type",
            Self::Status => "status",
            Self::StatusCategory => "statusCategory",
            Self::Priority => "priority",
            Self::Assignee => "assignee",
            Self::Reporter => "reporter",
            Self::Creator => "creator",
            Self::Resolution => "resolution",
            Self::Parent => "parent",
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Due => "due",
            Self::Resolved => "resolved",
            Self::Started => "started",
            Self::Summary => "summary",
            Self::Description => "description",
            Self::Text => "text",
            Self::Labels => "labels",
            Self::Key => "key",
            Self::Estimate => "estimate",
        }
    }

    /// Whether the field is a date/time column.
    #[must_use]
    pub fn is_date(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Updated | Self::Due | Self::Resolved | Self::Started
        )
    }

    /// Whether the field carries free text matched with `~`/`!~`.
    #[must_use]
    pub fn is_text(self) -> bool {
        matches!(self, Self::Summary | Self::Description | Self::Text)
    }

    /// Whether the field supports the ordering operators `>` `>=` `<` `<=`.
    #[must_use]
    pub fn is_orderable(self) -> bool {
        self.is_date() || matches!(self, Self::Priority | Self::Estimate)
    }

    /// Whether the field is one of the history-indexed six that `WAS`/`CHANGED`
    /// may be used on.
    ///
    /// Scoped deliberately. Generic any-field history forces indexing every
    /// change and wrecks the planner (`TODO.md` Phase 6, `jira-features.md` §5).
    /// These are the reference fields the changelog actually records and that a
    /// human asks history questions about.
    #[must_use]
    pub fn is_historyable(self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::Assignee
                | Self::Priority
                | Self::Reporter
                | Self::Resolution
                | Self::Type
        )
    }
}

/// A comparison / membership / history operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `IN`
    In,
    /// `NOT IN`
    NotIn,
    /// `~`
    Match,
    /// `!~`
    NotMatch,
    /// `IS` (with EMPTY/NULL)
    Is,
    /// `IS NOT` (with EMPTY/NULL)
    IsNot,
    /// `WAS`
    Was,
    /// `WAS NOT`
    WasNot,
    /// `WAS IN`
    WasIn,
    /// `WAS NOT IN`
    WasNotIn,
    /// `CHANGED`
    Changed,
}

impl Op {
    /// The operator's spelling, for messages and the normalised echo.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::In => "IN",
            Self::NotIn => "NOT IN",
            Self::Match => "~",
            Self::NotMatch => "!~",
            Self::Is => "IS",
            Self::IsNot => "IS NOT",
            Self::Was => "WAS",
            Self::WasNot => "WAS NOT",
            Self::WasIn => "WAS IN",
            Self::WasNotIn => "WAS NOT IN",
            Self::Changed => "CHANGED",
        }
    }

    /// Whether this operator negates its underlying test — used so `!=`, `NOT
    /// IN`, `!~`, `IS NOT`, `WAS NOT*` all share one positive builder.
    #[must_use]
    pub fn is_negated(self) -> bool {
        matches!(
            self,
            Self::Ne | Self::NotIn | Self::NotMatch | Self::IsNot | Self::WasNot | Self::WasNotIn
        )
    }
}

/// A literal or a function call the user supplied as a value.
///
/// **The only channel for user data.** A `Value` is compiled to a bind
/// parameter (a literal) or to a subquery over bind parameters (a function).
/// There is no path from here into the SQL string as text.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A word or quoted string.
    Str {
        /// The text, unquoted and unescaped.
        text: String,
        /// Where it was.
        span: Span,
    },
    /// A number.
    Num {
        /// The parsed value.
        value: f64,
        /// The original text, so `3` echoes as `3` and `3.5` as `3.5`.
        raw: String,
        /// Where it was.
        span: Span,
    },
    /// A function call: `currentUser()`, `startOfWeek(-1w)`.
    Func(FuncCall),
}

impl Value {
    /// The span of the whole value.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Str { span, .. } | Self::Num { span, .. } => *span,
            Self::Func(call) => call.span,
        }
    }
}

/// A parsed function call.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncCall {
    /// The function name as written.
    pub name: String,
    /// Where the name is.
    pub name_span: Span,
    /// The arguments, in order.
    pub args: Vec<Value>,
    /// The span of the whole call including its parentheses.
    pub span: Span,
}

/// One modifier on a `WAS`/`CHANGED` clause.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryMod {
    /// `FROM x`
    From(Value),
    /// `TO x`
    To(Value),
    /// `BY user`
    By(Value),
    /// `AFTER date`
    After(Value),
    /// `BEFORE date`
    Before(Value),
    /// `ON date`
    On(Value),
    /// `DURING (start, end)`
    During(Value, Value),
}

/// The right-hand side of a condition, once the operator is known.
#[derive(Debug, Clone, PartialEq)]
pub enum Rhs {
    /// A single value: `=`, `!=`, `>`, `~`, `WAS`, `WAS NOT`.
    Single(Value),
    /// A parenthesised set: `IN`, `NOT IN`, `WAS IN`, `WAS NOT IN`.
    Set(Vec<Value>),
    /// `EMPTY`/`NULL`, for `IS`/`IS NOT`.
    Empty,
    /// No value, for a bare `CHANGED`.
    None,
}

/// One field condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    /// The field.
    pub field: Field,
    /// Where the field name is.
    pub field_span: Span,
    /// The operator.
    pub op: Op,
    /// Where the operator is.
    pub op_span: Span,
    /// The right-hand side.
    pub rhs: Rhs,
    /// `WAS`/`CHANGED` history modifiers, empty for every other operator.
    pub history: Vec<HistoryMod>,
}

/// A reference to a saved filter: `filter = "My Filter"` or `filter = 42`.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterRef {
    /// The name or id of the filter, as a value.
    pub target: Value,
    /// Where the whole `filter = ...` clause is.
    pub span: Span,
}

/// A node of the boolean tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// `a AND b`
    And(Box<Node>, Box<Node>),
    /// `a OR b`
    Or(Box<Node>, Box<Node>),
    /// `NOT a`
    Not(Box<Node>),
    /// A leaf condition.
    Cond(Cond),
    /// A saved-filter reference, inlined at compile time.
    Filter(FilterRef),
    /// Matches every card. Not produced by the parser — [`crate::aql`]
    /// substitutes it when a referenced filter's body is empty, so an empty
    /// filter reads as "everything" rather than a dangling node.
    All,
}

/// The direction of one `ORDER BY` term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

impl Direction {
    /// The keyword.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// One `ORDER BY` term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderField {
    /// The field to sort on.
    pub field: Field,
    /// Where the field name is, so an un-orderable one can be underlined.
    pub span: Span,
    /// Ascending or descending.
    pub direction: Direction,
}

/// A whole parsed query: an optional predicate, and an optional ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The `WHERE` tree. `None` means "every card the caller can see".
    pub predicate: Option<Node>,
    /// The `ORDER BY` terms, in priority order.
    pub order_by: Vec<OrderField>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_column_counts_characters_not_bytes() {
        // `café ` is 6 bytes but 5 characters, so the `=` after it is the 6th
        // character — column 6 — even though it starts at byte 6. Counting bytes
        // would give the wrong caret the moment a query contains any accent.
        let source = "café = x";
        let span = Span::new("café ".len(), "café ".len() + 1);
        assert_eq!(span.column_in(source), 6);
    }

    #[test]
    fn every_field_spelling_round_trips() {
        // A field that parses under its canonical name but does not echo back to
        // something that re-parses would make the normalised query a lie.
        for field in [
            Field::Project,
            Field::Type,
            Field::Status,
            Field::StatusCategory,
            Field::Priority,
            Field::Assignee,
            Field::Reporter,
            Field::Creator,
            Field::Resolution,
            Field::Parent,
            Field::Created,
            Field::Updated,
            Field::Due,
            Field::Resolved,
            Field::Started,
            Field::Summary,
            Field::Description,
            Field::Text,
            Field::Labels,
            Field::Key,
            Field::Estimate,
        ] {
            assert_eq!(
                Field::parse(field.as_str()),
                Some(field),
                "{} does not round-trip",
                field.as_str()
            );
        }
    }

    #[test]
    fn the_history_fields_are_exactly_the_six() {
        let historyable: Vec<&str> = [
            Field::Project,
            Field::Type,
            Field::Status,
            Field::StatusCategory,
            Field::Priority,
            Field::Assignee,
            Field::Reporter,
            Field::Creator,
            Field::Resolution,
            Field::Parent,
            Field::Created,
            Field::Updated,
            Field::Due,
            Field::Resolved,
            Field::Started,
            Field::Summary,
            Field::Description,
            Field::Text,
            Field::Labels,
            Field::Key,
            Field::Estimate,
        ]
        .into_iter()
        .filter(|f| f.is_historyable())
        .map(Field::as_str)
        .collect();
        assert_eq!(
            historyable.len(),
            6,
            "history scope drifted: {historyable:?}"
        );
    }

    #[test]
    fn text_fields_are_not_orderable_and_dates_are() {
        assert!(!Field::Summary.is_orderable());
        assert!(Field::Created.is_orderable());
        assert!(Field::Priority.is_orderable());
        assert!(!Field::Status.is_orderable());
    }
}

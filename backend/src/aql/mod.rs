//! AQL — the Atlas Query Language.
//!
//! Boards, saved filters, quick filters, dashboards, gadgets, reports and
//! automation conditions are all AQL plus a renderer, which is why it is built
//! properly and once (`TODO.md` Phase 6). The pipeline is a straight line:
//!
//! ```text
//! text ──lex──▶ tokens ──parse──▶ AST ──expand filters──▶ AST ──compile──▶ (SQL, binds)
//! ```
//!
//! # The security model, in one sentence
//!
//! There is no code path where user text reaches the SQL string: the parser only
//! ever produces the closed enums of [`ast`], and the compiler's
//! [`compile::SqlBuilder`] can add SQL only from `&'static str` and values only
//! as bind placeholders. See [`compile`]. Everything under here — the lexer's
//! non-panicking guarantee, the compiler's bind-only invariant, the always-on
//! accessibility predicate — exists to keep that sentence true against a fuzzer
//! and an injection attacker.

pub mod ast;
pub mod compile;
pub mod functions;
pub mod lexer;
pub mod parser;

use std::future::Future;

use chrono::{DateTime, Utc};

use crate::auth::role::Role;
use crate::auth::user::User;
use crate::db::Db;
use crate::domain::card::Card;
use crate::domain::filter;
use crate::error::{AppError, AppResult};

pub use ast::{AqlError, Node, Query};
pub use compile::{Bind, CompileCtx, Compiled};

/// The deepest chain of `filter = "…"` references the expander will follow, as a
/// backstop beyond the id-based cycle guard: a chain with no repeats but 10,000
/// links is still not something to expand.
const MAX_FILTER_DEPTH: usize = 32;

/// The deepest the *expanded* predicate tree may nest, counted across filter
/// inlining as well as `AND`/`OR`/`NOT`.
///
/// `MAX_FILTER_DEPTH` bounds the number of `filter = "…"` hops, and the parser's
/// own depth/token limits bound a single body — but expansion *inlines* each
/// referenced body into its parent, so a chain of 32 filters each carrying an
/// 8 KB body would compose into a tree tens of thousands of nodes deep. The
/// recursive walks that follow (`compile::compile`, `normalize`) would then
/// overflow the stack and abort the process. This is the one limit that counts
/// the tree as a whole, so no composition of individually-legal filters can
/// build a tree too deep to walk. It is deliberately the *same* bound the
/// compiler enforces, so a query that expands also compiles.
const MAX_EXPANDED_DEPTH: usize = compile::MAX_NODE_DEPTH;

/// Parses AQL to an AST. Lexing and parsing only — no database, no expansion.
///
/// This is what `POST /search/validate` runs, and what a save-time check runs
/// against a filter's own body.
///
/// # Errors
///
/// [`AqlError`] with a span for any lexical or syntactic problem.
pub fn parse(source: &str) -> Result<Query, AqlError> {
    let tokens = lexer::lex(source)?;
    parser::parse(tokens)
}

/// Parses **and** type-checks against a compile context, without touching the
/// database. Catches field/operator-matrix violations and bad functions on top
/// of syntax, but leaves `filter = "…"` references unresolved.
///
/// # Errors
///
/// [`AqlError`] with a span.
pub fn check(source: &str, ctx: &CompileCtx) -> Result<Query, AqlError> {
    let query = parse(source)?;
    compile::typecheck(&query, ctx)?;
    Ok(query)
}

/// A compiled search: the page query and its matching count, ready to run.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The page query and its binds.
    pub page: Compiled,
    /// The `COUNT(*)` query and its binds — the same predicate, no page window.
    pub count: Compiled,
    /// The query re-rendered canonically, for the round-trip UI and the echo the
    /// search response returns.
    pub normalized: String,
}

/// A page of cards and the total that matched.
#[derive(Debug)]
pub struct SearchResults {
    /// The cards on this page, in the query's order.
    pub cards: Vec<Card>,
    /// How many matched in total, ignoring the page window.
    pub total: i64,
    /// The normalised query.
    pub normalized: String,
}

/// Builds a [`CompileCtx`] for a caller.
#[must_use]
pub fn context(viewer: &User, now: DateTime<Utc>, limit: i64, offset: i64) -> CompileCtx {
    CompileCtx {
        viewer_id: viewer.id.clone(),
        viewer_is_admin: viewer.role == Role::Admin,
        current_user_id: viewer.id.clone(),
        now,
        limit,
        offset,
    }
}

/// Compiles a query all the way to SQL: parse, inline filter references (guarding
/// cycles), compile the page and the count.
///
/// # Errors
///
/// - [`AppError::BadRequest`] for any AQL problem — a syntax error, an unknown
///   field, a forbidden operator, a missing or cyclic filter reference. The
///   detail carries the column so the frontend can underline it.
/// - [`AppError::Internal`] only for an actual database failure while resolving a
///   filter reference.
pub async fn plan(
    db: &Db,
    viewer: &User,
    now: DateTime<Utc>,
    source: &str,
    limit: i64,
    offset: i64,
) -> AppResult<Plan> {
    let query = parse(source).map_err(|err| bad_request(&err, source))?;

    let mut visiting = Vec::new();
    let expanded = expand_query(db, viewer, query, &mut visiting, 0).await?;

    let ctx = context(viewer, now, limit, offset);
    let page = compile::compile(&expanded, &ctx).map_err(|err| bad_request(&err, source))?;
    let count = compile::compile_count(&expanded, &ctx).map_err(|err| bad_request(&err, source))?;

    Ok(Plan {
        page,
        count,
        normalized: normalize(&expanded),
    })
}

/// Compiles and runs a query, returning the page and the total.
///
/// # Errors
///
/// As [`plan`], plus a database error surfacing the query itself.
pub async fn search(
    db: &Db,
    viewer: &User,
    now: DateTime<Utc>,
    source: &str,
    limit: i64,
    offset: i64,
) -> AppResult<SearchResults> {
    let plan = plan(db, viewer, now, source, limit, offset).await?;

    // `AssertSqlSafe` is honest here precisely because this is the only way a
    // value is supplied: the SQL was assembled from the fixed grammar and every
    // user value rode the `binds` channel, which is exactly the precondition the
    // assertion states. The binds are moved in as owned values, so the query
    // owns everything it runs on.
    let mut page = sqlx::query_as::<_, Card>(sqlx::AssertSqlSafe(plan.page.sql));
    for bind in plan.page.binds {
        page = match bind {
            Bind::Text(text) => page.bind(text),
            Bind::Real(value) => page.bind(value),
            Bind::Bool(flag) => page.bind(flag),
        };
    }
    let cards = page.fetch_all(db.reader()).await?;

    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(plan.count.sql));
    for bind in plan.count.binds {
        count = match bind {
            Bind::Text(text) => count.bind(text),
            Bind::Real(value) => count.bind(value),
            Bind::Bool(flag) => count.bind(flag),
        };
    }
    let total = count.fetch_one(db.reader()).await?;

    Ok(SearchResults {
        cards,
        total,
        normalized: plan.normalized,
    })
}

/// Renders an [`AqlError`] as a [`AppError::BadRequest`] with the column filled
/// in against the source it came from.
fn bad_request(err: &AqlError, source: &str) -> AppError {
    AppError::BadRequest(err.render(source))
}

// ---------------------------------------------------------------------------
// Filter composition and the cycle guard
// ---------------------------------------------------------------------------

/// Replaces every `filter = "…"` reference in a query with the referenced
/// filter's (recursively expanded) predicate.
///
/// `visiting` is the chain of filter ids currently being expanded. A reference
/// to an id already on the chain is a cycle and is refused rather than followed
/// forever — the property `tests/aql.rs` pins.
async fn expand_query(
    db: &Db,
    viewer: &User,
    query: Query,
    visiting: &mut Vec<String>,
    depth: usize,
) -> AppResult<Query> {
    let predicate = match query.predicate {
        Some(node) => Some(expand_node(db, viewer, node, visiting, depth).await?),
        None => None,
    };
    Ok(Query {
        predicate,
        order_by: query.order_by,
    })
}

/// The boxed recursive worker. Async recursion needs the explicit `Box::pin`.
///
/// `depth` counts the nesting of the tree being *built*, incremented across
/// `AND`/`OR`/`NOT` and carried through filter inlining (see [`expand_filter`]),
/// so it bounds the depth of the whole composed tree — not just this body —
/// against [`MAX_EXPANDED_DEPTH`]. Refusing here means the deep tree is never
/// constructed, so the synchronous walks downstream cannot overflow the stack.
fn expand_node<'a>(
    db: &'a Db,
    viewer: &'a User,
    node: Node,
    visiting: &'a mut Vec<String>,
    depth: usize,
) -> std::pin::Pin<Box<dyn Future<Output = AppResult<Node>> + Send + 'a>> {
    Box::pin(async move {
        if depth > MAX_EXPANDED_DEPTH {
            return Err(AppError::BadRequest(format!(
                "the query nests deeper than {MAX_EXPANDED_DEPTH} levels once its filter \
                 references are expanded"
            )));
        }
        match node {
            Node::And(l, r) => {
                let left = expand_node(db, viewer, *l, visiting, depth + 1).await?;
                let right = expand_node(db, viewer, *r, visiting, depth + 1).await?;
                Ok(Node::And(Box::new(left), Box::new(right)))
            }
            Node::Or(l, r) => {
                let left = expand_node(db, viewer, *l, visiting, depth + 1).await?;
                let right = expand_node(db, viewer, *r, visiting, depth + 1).await?;
                Ok(Node::Or(Box::new(left), Box::new(right)))
            }
            Node::Not(inner) => {
                let inner = expand_node(db, viewer, *inner, visiting, depth + 1).await?;
                Ok(Node::Not(Box::new(inner)))
            }
            Node::Cond(_) | Node::All => Ok(node),
            Node::Filter(reference) => expand_filter(db, viewer, &reference, visiting, depth).await,
        }
    })
}

/// Resolves one filter reference to its expanded predicate.
async fn expand_filter(
    db: &Db,
    viewer: &User,
    reference: &ast::FilterRef,
    visiting: &mut Vec<String>,
    depth: usize,
) -> AppResult<Node> {
    if visiting.len() >= MAX_FILTER_DEPTH {
        return Err(AppError::BadRequest(format!(
            "filter references nest deeper than {MAX_FILTER_DEPTH} levels"
        )));
    }

    let target = filter_target_text(&reference.target)?;
    let resolved = resolve_filter(db, viewer, &target).await?;

    if visiting.contains(&resolved.id) {
        return Err(AppError::BadRequest(format!(
            "filter reference cycle: filter {:?} refers back to itself, directly or through \
             another filter",
            resolved.name
        )));
    }

    let inner = parse(&resolved.aql).map_err(|err| {
        AppError::BadRequest(format!(
            "the saved filter {:?} no longer parses: {}",
            resolved.name,
            err.render(&resolved.aql)
        ))
    })?;

    // The inlined body replaces the filter node, so it continues from the same
    // depth — this is what makes a chain of shallow filters accumulate rather
    // than each body being measured in isolation.
    visiting.push(resolved.id.clone());
    let expanded = expand_query(db, viewer, inner, visiting, depth).await?;
    visiting.pop();

    // An empty filter body matches everything, so it inlines as `All`.
    Ok(expanded.predicate.unwrap_or(Node::All))
}

/// The text of a filter reference target — a name or an id string.
fn filter_target_text(value: &ast::Value) -> AppResult<String> {
    match value {
        ast::Value::Str { text, .. } => Ok(text.clone()),
        ast::Value::Num { raw, .. } => Ok(raw.clone()),
        ast::Value::Func(_) => Err(AppError::BadRequest(
            "a filter reference must be a name or an id, not a function".to_owned(),
        )),
    }
}

/// Finds the filter a reference names, scoped to the caller.
///
/// By name first (the common `filter = "My Filter"`), then by id. Both are
/// restricted to the caller's own filters: a reference cannot reach into someone
/// else's, so composition cannot become a way to run a query you could not write.
async fn resolve_filter(db: &Db, viewer: &User, target: &str) -> AppResult<filter::Filter> {
    if let Some(found) = filter::find_by_name(db, &viewer.id, target).await? {
        return Ok(found);
    }
    if let Some(found) = filter::find_by_id(db, target).await?
        && found.owner_id == viewer.id
    {
        return Ok(found);
    }
    Err(AppError::BadRequest(format!(
        "no filter named or with id {target:?} belongs to you"
    )))
}

// ---------------------------------------------------------------------------
// Normalisation — the canonical echo, and the basis of the round-trip UI
// ---------------------------------------------------------------------------

/// Renders an AST back to a canonical AQL string.
///
/// Deterministic and re-parseable, so `POST /search` can hand the client a
/// normalised form of what it asked, and the basic⇄advanced editor has one text
/// to diff against. Parentheses are added around every binary node so the echo
/// never relies on the reader knowing the precedence rules.
#[must_use]
pub fn normalize(query: &Query) -> String {
    let mut out = String::new();
    if let Some(node) = &query.predicate {
        render_node(node, &mut out);
    }
    if !query.order_by.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("ORDER BY ");
        for (i, order) in query.order_by.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(order.field.as_str());
            out.push(' ');
            out.push_str(order.direction.as_str());
        }
    }
    out
}

fn render_node(node: &Node, out: &mut String) {
    match node {
        Node::And(l, r) => render_binary(l, "AND", r, out),
        Node::Or(l, r) => render_binary(l, "OR", r, out),
        Node::Not(inner) => {
            out.push_str("NOT ");
            render_node(inner, out);
        }
        Node::All => out.push_str("1 = 1"),
        Node::Filter(reference) => {
            out.push_str("filter = ");
            render_value(&reference.target, out);
        }
        Node::Cond(cond) => render_cond(cond, out),
    }
}

fn render_binary(l: &Node, op: &str, r: &Node, out: &mut String) {
    out.push('(');
    render_node(l, out);
    out.push(' ');
    out.push_str(op);
    out.push(' ');
    render_node(r, out);
    out.push(')');
}

fn render_cond(cond: &ast::Cond, out: &mut String) {
    out.push_str(cond.field.as_str());
    out.push(' ');
    out.push_str(cond.op.as_str());
    match &cond.rhs {
        ast::Rhs::Single(v) => {
            out.push(' ');
            render_value(v, out);
        }
        ast::Rhs::Set(vs) => {
            out.push_str(" (");
            for (i, v) in vs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_value(v, out);
            }
            out.push(')');
        }
        ast::Rhs::Empty => out.push_str(" EMPTY"),
        ast::Rhs::None => {}
    }
    for modifier in &cond.history {
        render_history_mod(modifier, out);
    }
}

fn render_history_mod(modifier: &ast::HistoryMod, out: &mut String) {
    match modifier {
        ast::HistoryMod::From(v) => render_kw_value("FROM", v, out),
        ast::HistoryMod::To(v) => render_kw_value("TO", v, out),
        ast::HistoryMod::By(v) => render_kw_value("BY", v, out),
        ast::HistoryMod::After(v) => render_kw_value("AFTER", v, out),
        ast::HistoryMod::Before(v) => render_kw_value("BEFORE", v, out),
        ast::HistoryMod::On(v) => render_kw_value("ON", v, out),
        ast::HistoryMod::During(a, b) => {
            out.push_str(" DURING (");
            render_value(a, out);
            out.push_str(", ");
            render_value(b, out);
            out.push(')');
        }
    }
}

fn render_kw_value(kw: &str, value: &ast::Value, out: &mut String) {
    out.push(' ');
    out.push_str(kw);
    out.push(' ');
    render_value(value, out);
}

fn render_value(value: &ast::Value, out: &mut String) {
    match value {
        ast::Value::Str { text, .. } => {
            // Quote anything that is not a simple bareword, so the echo re-parses.
            if is_bareword(text) {
                out.push_str(text);
            } else {
                out.push('"');
                out.push_str(&text.replace('\\', "\\\\").replace('"', "\\\""));
                out.push('"');
            }
        }
        ast::Value::Num { raw, .. } => out.push_str(raw),
        ast::Value::Func(call) => {
            out.push_str(&call.name);
            out.push('(');
            for (i, arg) in call.args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_value(arg, out);
            }
            out.push(')');
        }
    }
}

/// Whether a value can be written unquoted in the normalised echo.
fn is_bareword(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CompileCtx {
        CompileCtx {
            viewer_id: "u1".to_owned(),
            viewer_is_admin: false,
            current_user_id: "u1".to_owned(),
            now: Utc::now(),
            limit: 50,
            offset: 0,
        }
    }

    #[test]
    fn check_accepts_a_good_query_and_rejects_a_bad_operator() {
        assert!(check("status = Done AND priority > High", &ctx()).is_ok());
        let err = check("summary = hello", &ctx()).unwrap_err();
        assert!(err.message.contains("text"));
    }

    #[test]
    fn check_accepts_an_unexpanded_filter_reference() {
        // Validation runs before expansion, so a filter reference is not an error
        // there even though it would be at compile time.
        assert!(check("filter = \"Mine\" AND status = Done", &ctx()).is_ok());
    }

    #[test]
    fn normalize_round_trips_through_the_parser() {
        for src in [
            "status = Done",
            "status = A OR status = B AND priority > High",
            "assignee = currentUser() ORDER BY created DESC",
            "summary ~ \"hello world\"",
            "status IN (A, B, C)",
            "resolution IS NOT EMPTY",
            "status CHANGED FROM \"In Progress\" TO Done AFTER -7d",
        ] {
            let once = normalize(&parse(src).unwrap());
            let twice = normalize(&parse(&once).unwrap());
            assert_eq!(once, twice, "normalise is not idempotent for {src:?}");
        }
    }

    #[test]
    fn a_value_with_a_space_is_quoted_in_the_echo() {
        let normalized = normalize(&parse("status = \"In Progress\"").unwrap());
        assert!(normalized.contains("\"In Progress\""), "{normalized}");
    }
}

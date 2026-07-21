//! The compiler: a typed [`Query`] to **parameterised** SQL.
//!
//! # The whole security model, made structural
//!
//! Every string this module appends to the SQL is a `&'static str` — a literal,
//! a column name from the [`Field`] whitelist, or an operator from the [`Op`]
//! enum. There is exactly one way for a user-supplied value to reach the output:
//! [`SqlBuilder::bind`], which pushes a `?` placeholder into the SQL and the
//! value into a separate `Vec<Bind>`. The SQL text and the bind values never mix.
//!
//! [`SqlBuilder::keyword`] takes `&'static str` and nothing else, so there is no
//! method on the builder that could put user text into the query. That is the
//! invariant a fuzzer and an injection attacker come for, and it is enforced by
//! the type of the argument rather than by review: you *cannot* write the unsafe
//! version, because the safe method will not accept a runtime `String`.
//!
//! The compiled statement is always `AND`-ed with an accessible-projects predicate
//! ([`build_access`]) so a query can never read cards in a project the caller
//! cannot see. The query is not trusted to scope itself.

use chrono::{DateTime, Utc};

use super::ast::{
    AqlError, Cond, Direction, Field, FuncCall, HistoryMod, Node, Op, OrderField, Query, Rhs, Value,
};
use super::functions::{self, FnCtx};

/// A single value bound into a parameterised query.
///
/// The only channel through which user data reaches execution. `sqlx` binds
/// these positionally; there is no `Bind` for "raw SQL", by construction.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    /// A text value.
    Text(String),
    /// A real number (an estimate).
    Real(f64),
    /// A boolean (the is-admin flag on the access predicate).
    Bool(bool),
}

/// What the compiler needs beyond the query itself.
#[derive(Debug, Clone)]
pub struct CompileCtx {
    /// The caller, for the accessible-projects predicate.
    pub viewer_id: String,
    /// Whether the caller is an instance admin (sees every project).
    pub viewer_is_admin: bool,
    /// Who `currentUser()` resolves to. The same as the viewer in every real
    /// call; kept separate so a test can tell them apart.
    pub current_user_id: String,
    /// The instant the date functions are relative to.
    pub now: DateTime<Utc>,
    /// The page size.
    pub limit: i64,
    /// The page offset.
    pub offset: i64,
}

impl CompileCtx {
    fn fn_ctx(&self) -> FnCtx {
        FnCtx {
            current_user_id: self.current_user_id.clone(),
            now: self.now,
        }
    }
}

/// A compiled query: the SQL text, and the values to bind into it in order.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// The SQL, built only from the fixed grammar. Every `?` is a value in
    /// [`Self::binds`], in order.
    pub sql: String,
    /// The bind values, in the order their `?`s appear.
    pub binds: Vec<Bind>,
}

/// Accumulates SQL and its binds, keeping the two channels apart by type.
///
/// [`Self::keyword`] is the only way to add SQL text and it takes `&'static
/// str`. [`Self::bind`] is the only way to add a value and it emits a `?`. There
/// is no third door.
#[derive(Debug, Default)]
struct SqlBuilder {
    sql: String,
    binds: Vec<Bind>,
}

impl SqlBuilder {
    /// Appends fixed SQL. `&'static str` so no runtime string can be spliced in.
    fn keyword(&mut self, fragment: &'static str) {
        self.sql.push_str(fragment);
    }

    /// Appends a `?` placeholder and stores its value. The only value channel.
    fn bind(&mut self, value: Bind) {
        self.sql.push('?');
        self.binds.push(value);
    }

    /// Appends a comma-separated list of placeholders for `values`.
    fn bind_list(&mut self, values: Vec<Bind>) {
        for (i, value) in values.into_iter().enumerate() {
            if i > 0 {
                self.keyword(", ");
            }
            self.bind(value);
        }
    }

    fn finish(self) -> Compiled {
        Compiled {
            sql: self.sql,
            binds: self.binds,
        }
    }
}

/// The columns a search returns: every column of `cards`, by name, so
/// `sqlx::query_as::<_, Card>` can read the row. `cards.*` rather than a list so
/// this cannot drift from [`crate::domain::card::Card`].
const SELECT_HEAD: &str = "SELECT cards.* FROM cards WHERE cards.deleted_at IS NULL AND ";

/// The deepest predicate tree the AQL walks will descend — the one bound shared
/// by [`compile_node`], [`typecheck_node`] and the filter expander
/// ([`crate::aql`]).
///
/// The compiler defends itself: whatever a caller hands it — a directly parsed
/// query, or one an expander built by inlining filters — a tree deeper than this
/// is refused with an ordinary error rather than recursed into until the stack
/// overflows and the process aborts. The expander applies the *same* limit while
/// it builds, so a query that validates also runs, and a filter-inflated tree is
/// refused before it is ever constructed. Far below the depth at which a
/// recursive walk runs out of stack; far above any hand-written query.
pub(crate) const MAX_NODE_DEPTH: usize = 512;

/// Compiles a fully-expanded query — no [`Node::Filter`] may remain — into SQL.
///
/// Filter references are inlined by [`crate::aql::expand_filters`] before this
/// runs, so a surviving one is an internal error, not a user error.
///
/// # Errors
///
/// [`AqlError`] for any field/operator combination the matrix forbids, an
/// unknown function, or a bad relative offset — each with the span to underline.
pub fn compile(query: &Query, ctx: &CompileCtx) -> Result<Compiled, AqlError> {
    let mut b = SqlBuilder::default();

    b.keyword(SELECT_HEAD);
    build_filtered(query, &mut b, ctx)?;
    compile_order_by(&query.order_by, &mut b)?;

    b.keyword(" LIMIT ");
    b.bind(Bind::Real(f64::from(
        i32::try_from(ctx.limit.clamp(1, 500)).unwrap_or(50),
    )));
    b.keyword(" OFFSET ");
    b.bind(Bind::Real(f64::from(
        i32::try_from(ctx.offset.max(0)).unwrap_or(0),
    )));

    Ok(b.finish())
}

/// Compiles the `COUNT(*)` twin of [`compile`]: the same predicate and the same
/// accessibility scoping, without the ordering or the page window.
///
/// A page needs its total, and the total must be filtered and scoped exactly as
/// the page is — so it shares [`build_filtered`] rather than restating it, which
/// is what stops the count and the page from ever disagreeing about what matches.
///
/// # Errors
///
/// The same as [`compile`].
pub fn compile_count(query: &Query, ctx: &CompileCtx) -> Result<Compiled, AqlError> {
    let mut b = SqlBuilder::default();
    b.keyword("SELECT COUNT(*) FROM cards WHERE cards.deleted_at IS NULL AND ");
    build_filtered(query, &mut b, ctx)?;
    Ok(b.finish())
}

/// Appends `(access) AND (predicate)` — the shared body of the page and its
/// count. Accessibility is unconditional and always first.
fn build_filtered(query: &Query, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    b.keyword("(");
    build_access(b, ctx);
    b.keyword(") AND (");
    match &query.predicate {
        Some(node) => compile_node(node, b, ctx, 0)?,
        None => b.keyword("1 = 1"),
    }
    b.keyword(")");
    Ok(())
}

/// The accessible-projects predicate: the SQL twin of
/// [`crate::domain::project::list_for`]'s `WHERE`.
///
/// An admin sees everything; otherwise a card is visible iff the caller leads
/// its project or holds a `project_members` row on it. Bound values only.
fn build_access(b: &mut SqlBuilder, ctx: &CompileCtx) {
    b.keyword("");
    b.bind(Bind::Bool(ctx.viewer_is_admin));
    b.keyword(" OR EXISTS (SELECT 1 FROM projects p WHERE p.id = cards.project_id AND (p.lead_id = ");
    b.bind(Bind::Text(ctx.viewer_id.clone()));
    b.keyword(" OR EXISTS (SELECT 1 FROM project_members m WHERE m.project_id = cards.project_id AND m.user_id = ");
    b.bind(Bind::Text(ctx.viewer_id.clone()));
    b.keyword(")))");
}

fn compile_node(
    node: &Node,
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
    depth: usize,
) -> Result<(), AqlError> {
    check_node_depth(node, depth)?;
    match node {
        Node::And(l, r) => {
            b.keyword("(");
            compile_node(l, b, ctx, depth + 1)?;
            b.keyword(" AND ");
            compile_node(r, b, ctx, depth + 1)?;
            b.keyword(")");
            Ok(())
        }
        Node::Or(l, r) => {
            b.keyword("(");
            compile_node(l, b, ctx, depth + 1)?;
            b.keyword(" OR ");
            compile_node(r, b, ctx, depth + 1)?;
            b.keyword(")");
            Ok(())
        }
        Node::Not(inner) => {
            b.keyword("NOT (");
            compile_node(inner, b, ctx, depth + 1)?;
            b.keyword(")");
            Ok(())
        }
        Node::Cond(cond) => compile_cond(cond, b, ctx),
        Node::All => {
            b.keyword("1 = 1");
            Ok(())
        }
        Node::Filter(filter) => Err(AqlError::at(
            filter.span,
            "a filter reference was not expanded before compilation (internal error)",
        )),
    }
}

/// Refuses a tree nested past [`MAX_NODE_DEPTH`] before recursing into it, so a
/// pathological (or filter-inflated) predicate is an error, never a stack
/// overflow. The span points at the node the limit was hit on.
fn check_node_depth(node: &Node, depth: usize) -> Result<(), AqlError> {
    if depth <= MAX_NODE_DEPTH {
        return Ok(());
    }
    let span = match node {
        Node::Cond(cond) => cond.field_span,
        Node::Filter(filter) => filter.span,
        // The binary/unary nodes carry no span of their own; underline the
        // whole query rather than an arbitrary child.
        _ => super::ast::Span::new(0, 0),
    };
    Err(AqlError::at(
        span,
        format!("the query nests deeper than {MAX_NODE_DEPTH} levels"),
    ))
}

/// Type-checks a query without emitting SQL: the field/operator matrix, function
/// names and offsets, and orderable-field rules — everything a user can get
/// wrong that is not pure syntax.
///
/// Unlike [`compile`], a [`Node::Filter`] is **accepted** here: validation runs
/// before filter references are inlined (`POST /search/validate`, and the
/// save-time check on a filter's own body), so an unexpanded reference is
/// expected rather than an error.
///
/// # Errors
///
/// [`AqlError`] with a span for the first problem found.
pub fn typecheck(query: &Query, ctx: &CompileCtx) -> Result<(), AqlError> {
    if let Some(node) = &query.predicate {
        typecheck_node(node, ctx, 0)?;
    }
    // Order-by fields have their own orderability rule; run it through the real
    // builder so validation and execution agree on what is sortable.
    let mut scratch = SqlBuilder::default();
    compile_order_by(&query.order_by, &mut scratch)?;
    Ok(())
}

fn typecheck_node(node: &Node, ctx: &CompileCtx, depth: usize) -> Result<(), AqlError> {
    check_node_depth(node, depth)?;
    match node {
        Node::And(l, r) | Node::Or(l, r) => {
            typecheck_node(l, ctx, depth + 1)?;
            typecheck_node(r, ctx, depth + 1)
        }
        Node::Not(inner) => typecheck_node(inner, ctx, depth + 1),
        Node::All | Node::Filter(_) => Ok(()),
        Node::Cond(cond) => {
            let mut scratch = SqlBuilder::default();
            compile_cond(cond, &mut scratch, ctx)
        }
    }
}

fn compile_cond(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    check_support(cond.field, cond.op, cond.field_span, cond.op_span)?;

    if cond.op.is_negated() {
        b.keyword("NOT (");
        compile_positive(cond, b, ctx)?;
        b.keyword(")");
    } else {
        compile_positive(cond, b, ctx)?;
    }
    Ok(())
}

/// Compiles the positive form of a condition; negation is one `NOT (...)` wrap
/// applied by the caller, so `!=`, `NOT IN`, `!~`, `IS NOT`, `WAS NOT*` all
/// share this single builder.
fn compile_positive(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    match cond.op {
        Op::Eq | Op::In | Op::Ne | Op::NotIn => compile_membership(cond, b, ctx),
        Op::Gt | Op::Ge | Op::Lt | Op::Le => compile_ordering(cond, b, ctx),
        Op::Match | Op::NotMatch => compile_text(cond, b, ctx),
        Op::Is | Op::IsNot => {
            compile_is_empty(cond.field, b);
            Ok(())
        }
        Op::Was | Op::WasNot | Op::WasIn | Op::WasNotIn => compile_was(cond, b, ctx),
        Op::Changed => compile_changed(cond, b, ctx),
    }
}

// ---------------------------------------------------------------------------
// The field / operator support matrix
// ---------------------------------------------------------------------------

/// Rejects an operator a field does not support, with a message that names the
/// field and says *why* — the errors are a feature of the language.
fn check_support(
    field: Field,
    op: Op,
    field_span: super::ast::Span,
    op_span: super::ast::Span,
) -> Result<(), AqlError> {
    // Ordering: dates, numbers, priority only.
    if matches!(op, Op::Gt | Op::Ge | Op::Lt | Op::Le) && !field.is_orderable() {
        return Err(AqlError::at(
            op_span,
            format!(
                "the {} field is not orderable; >, >=, < and <= only work on dates, numbers and priority",
                field.as_str()
            ),
        ));
    }

    // Full-text: summary, description, text only.
    if matches!(op, Op::Match | Op::NotMatch) && !field.is_text() {
        return Err(AqlError::at(
            op_span,
            format!(
                "the {} field is not full-text; ~ and !~ only work on summary, description and text",
                field.as_str()
            ),
        ));
    }

    // Equality on a text field: use ~ instead.
    if matches!(op, Op::Eq | Op::Ne) && field.is_text() {
        return Err(AqlError::at(
            op_span,
            format!(
                "the {} field is text; use ~ instead of {}",
                field.as_str(),
                op.as_str()
            ),
        ));
    }

    // History: the six indexed fields only.
    if matches!(op, Op::Was | Op::WasNot | Op::WasIn | Op::WasNotIn | Op::Changed)
        && !field.is_historyable()
    {
        return Err(AqlError::at(
            field_span,
            format!(
                "history search (WAS/CHANGED) is not supported on the {} field; only status, assignee, priority, reporter, resolution and type are history-indexed",
                field.as_str()
            ),
        ));
    }

    // The all-text pseudo-field is `~` only.
    if field == Field::Text && !matches!(op, Op::Match | Op::NotMatch) {
        return Err(AqlError::at(
            op_span,
            "the text field takes ~ only".to_owned(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Membership: =  !=  IN  NOT IN
// ---------------------------------------------------------------------------

fn compile_membership(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    let values = rhs_values(&cond.rhs);

    // A lone set-function after IN / NOT IN is a subquery, not a value list.
    // With `=`/`!=` it falls through to `scalar_binds`, which rejects it with a
    // message pointing the user at IN — a set is not a single value.
    if matches!(cond.op, Op::In | Op::NotIn)
        && let [Value::Func(call)] = values.as_slice()
        && functions::is_set_function(&call.name)
    {
        return compile_set_function(cond.field, call, b, ctx);
    }

    let binds = scalar_binds(cond.field, &values, ctx)?;
    emit_membership(cond.field, binds, b, cond.field_span)
}

/// Emits `<card column> IN (SELECT ... WHERE <name> IN (?, ...))` for the
/// reference fields, and a direct `IN` for the value fields.
fn emit_membership(
    field: Field,
    binds: Vec<Bind>,
    b: &mut SqlBuilder,
    field_span: super::ast::Span,
) -> Result<(), AqlError> {
    match field {
        Field::Project => subquery_in(b, "cards.project_id", "id", "projects", "key", binds),
        Field::Type => subquery_in(b, "cards.type_id", "id", "card_types", "name", binds),
        Field::Status => subquery_in(b, "cards.status_id", "id", "statuses", "name", binds),
        Field::StatusCategory => {
            subquery_in(b, "cards.status_id", "id", "statuses", "category", binds);
        }
        Field::Priority => subquery_in(b, "cards.priority_id", "id", "priorities", "name", binds),
        Field::Resolution => {
            subquery_in(b, "cards.resolution_id", "id", "resolutions", "name", binds);
        }
        Field::Assignee => user_membership(b, "cards.assignee_id", binds),
        Field::Reporter => user_membership(b, "cards.reporter_id", binds),
        Field::Creator => user_membership(b, "cards.creator_id", binds),
        Field::Parent => subquery_in(b, "cards.parent_id", "id", "cards", "key", binds),
        Field::Key => {
            b.keyword("cards.key IN (");
            b.bind_list(binds);
            b.keyword(")");
        }
        Field::Labels => labels_membership(b, binds),
        _ => {
            return Err(AqlError::at(
                field_span,
                format!("the {} field cannot be matched by equality", field.as_str()),
            ));
        }
    }
    Ok(())
}

/// `<col> IN (SELECT <idcol> FROM <table> WHERE <namecol> IN (?, ...))`.
fn subquery_in(
    b: &mut SqlBuilder,
    col: &'static str,
    idcol: &'static str,
    table: &'static str,
    namecol: &'static str,
    binds: Vec<Bind>,
) {
    b.keyword(col);
    b.keyword(" IN (SELECT ");
    b.keyword(idcol);
    b.keyword(" FROM ");
    b.keyword(table);
    b.keyword(" WHERE ");
    b.keyword(namecol);
    b.keyword(" IN (");
    b.bind_list(binds);
    b.keyword("))");
}

/// A user field matches by username **or** id, so `currentUser()` (an id) and a
/// literal username both work.
fn user_membership(b: &mut SqlBuilder, col: &'static str, binds: Vec<Bind>) {
    b.keyword(col);
    b.keyword(" IN (SELECT id FROM users WHERE username IN (");
    b.bind_list(binds.clone());
    b.keyword(") OR id IN (");
    b.bind_list(binds);
    b.keyword("))");
}

fn labels_membership(b: &mut SqlBuilder, binds: Vec<Bind>) {
    b.keyword("EXISTS (SELECT 1 FROM card_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.card_id = cards.id AND t.name IN (");
    b.bind_list(binds);
    b.keyword("))");
}

// ---------------------------------------------------------------------------
// Ordering: >  >=  <  <=
// ---------------------------------------------------------------------------

fn compile_ordering(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    let value = single_value(&cond.rhs, cond.op_span)?;

    if cond.field == Field::Priority {
        return compile_priority_ordering(cond.op, value, b, ctx);
    }

    let column = date_or_number_column(cond.field);
    b.keyword(column);
    b.keyword(sql_cmp(cond.op));
    if cond.field == Field::Estimate {
        b.bind(number_bind(value)?);
    } else {
        b.bind(Bind::Text(scalar_text(cond.field, value, ctx)?));
    }
    Ok(())
}

/// Priority is ordered by **rank** (lower rank = more urgent), so `priority >
/// High` means "more urgent than High" — a smaller rank. The comparison is
/// resolved per-project against the named priority, because two projects can
/// spell "High" at different ranks.
fn compile_priority_ordering(
    op: Op,
    value: &Value,
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
) -> Result<(), AqlError> {
    // `priority > High` -> the card's rank is *below* High's rank. Invert.
    let inverted = match op {
        Op::Gt => " < ",
        Op::Ge => " <= ",
        Op::Lt => " > ",
        Op::Le => " >= ",
        _ => " = ",
    };
    b.keyword("EXISTS (SELECT 1 FROM priorities self_p JOIN priorities ref_p ON self_p.project_id = ref_p.project_id WHERE self_p.id = cards.priority_id AND ref_p.name = ");
    b.bind(Bind::Text(scalar_text(Field::Priority, value, ctx)?));
    b.keyword(" AND self_p.rank");
    b.keyword(inverted);
    b.keyword("ref_p.rank)");
    Ok(())
}

fn date_or_number_column(field: Field) -> &'static str {
    match field {
        Field::Updated => "cards.updated_at",
        Field::Due => "cards.due_date",
        Field::Resolved => "cards.resolved_at",
        Field::Started => "cards.start_date",
        Field::Estimate => "cards.estimate",
        // Created, plus any field check_support has already rejected ordering on
        // (unreachable), map here.
        _ => "cards.created_at",
    }
}

fn sql_cmp(op: Op) -> &'static str {
    match op {
        Op::Gt => " > ",
        Op::Ge => " >= ",
        Op::Lt => " < ",
        Op::Le => " <= ",
        _ => " = ",
    }
}

// ---------------------------------------------------------------------------
// Full text: ~  !~
// ---------------------------------------------------------------------------

fn compile_text(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    let value = single_value(&cond.rhs, cond.op_span)?;
    let needle = like_pattern(&scalar_text(cond.field, value, ctx)?);

    match cond.field {
        Field::Description => like_col(b, "cards.description", needle),
        Field::Text => {
            b.keyword("(");
            like_col(b, "cards.summary", needle.clone());
            b.keyword(" OR ");
            like_col(b, "cards.description", needle);
            b.keyword(")");
        }
        // Summary, plus any non-text field check_support has already rejected `~`
        // on (unreachable), search the summary.
        _ => like_col(b, "cards.summary", needle),
    }
    Ok(())
}

fn like_col(b: &mut SqlBuilder, col: &'static str, pattern: String) {
    b.keyword(col);
    b.keyword(" LIKE ");
    b.bind(Bind::Text(pattern));
    b.keyword(" ESCAPE '\\'");
}

/// Wraps a needle in `%…%` and escapes LIKE's own wildcards, so a user typing
/// `50%` searches for the literal text rather than "anything". The `%` framing
/// is applied to the *value*, which is still bound — no SQL is built from it.
fn like_pattern(needle: &str) -> String {
    let escaped = needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

// ---------------------------------------------------------------------------
// Emptiness: IS EMPTY / IS NOT EMPTY
// ---------------------------------------------------------------------------

fn compile_is_empty(field: Field, b: &mut SqlBuilder) {
    match field {
        Field::Labels => {
            b.keyword("NOT EXISTS (SELECT 1 FROM card_tags ct WHERE ct.card_id = cards.id)");
        }
        Field::Summary => b.keyword("cards.summary = ''"),
        Field::Description => b.keyword("(cards.description IS NULL OR cards.description = '')"),
        Field::Assignee => b.keyword("cards.assignee_id IS NULL"),
        Field::Reporter => b.keyword("cards.reporter_id IS NULL"),
        Field::Creator => b.keyword("cards.creator_id IS NULL"),
        Field::Resolution => b.keyword("cards.resolution_id IS NULL"),
        Field::Priority => b.keyword("cards.priority_id IS NULL"),
        Field::Parent => b.keyword("cards.parent_id IS NULL"),
        Field::Status => b.keyword("cards.status_id IS NULL"),
        Field::Type => b.keyword("cards.type_id IS NULL"),
        Field::Due => b.keyword("cards.due_date IS NULL"),
        Field::Resolved => b.keyword("cards.resolved_at IS NULL"),
        Field::Started => b.keyword("cards.start_date IS NULL"),
        Field::Estimate => b.keyword("cards.estimate IS NULL"),
        Field::Created => b.keyword("cards.created_at IS NULL"),
        Field::Updated => b.keyword("cards.updated_at IS NULL"),
        // Project, StatusCategory, Text, Key have no meaningful emptiness; a
        // false predicate is the honest answer, and check_support does not gate
        // IS here so the query still runs rather than erroring surprisingly.
        _ => b.keyword("0 = 1"),
    }
}

// ---------------------------------------------------------------------------
// History: WAS / WAS IN / WAS NOT / WAS NOT IN, and the modifiers
// ---------------------------------------------------------------------------

fn compile_was(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    let logical = history_field_name(cond.field);
    let values = rhs_values(&cond.rhs);
    let binds = scalar_binds(cond.field, &values, ctx)?;

    b.keyword("EXISTS (SELECT 1 FROM card_history h WHERE h.card_id = cards.id AND h.field = ");
    b.bind(Bind::Text(logical.to_owned()));
    b.keyword(" AND (");
    // The value could be the id (from currentUser or a status id) or the display
    // name (a status/priority/resolution name), so match all four columns.
    history_value_match(b, &binds);
    b.keyword(")");
    compile_history_mods(&cond.history, b, ctx, /* allow_from_to = */ false)?;
    b.keyword(")");
    Ok(())
}

fn compile_changed(cond: &Cond, b: &mut SqlBuilder, ctx: &CompileCtx) -> Result<(), AqlError> {
    let logical = history_field_name(cond.field);
    b.keyword("EXISTS (SELECT 1 FROM card_history h WHERE h.card_id = cards.id AND h.field = ");
    b.bind(Bind::Text(logical.to_owned()));
    compile_history_mods(&cond.history, b, ctx, /* allow_from_to = */ true)?;
    b.keyword(")");
    Ok(())
}

/// `(h.to_value IN (…) OR h.to_display IN (…) OR h.from_value IN (…) OR
/// h.from_display IN (…))`.
fn history_value_match(b: &mut SqlBuilder, binds: &[Bind]) {
    for (i, column) in ["h.to_value", "h.to_display", "h.from_value", "h.from_display"]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            b.keyword(" OR ");
        }
        b.keyword(column);
        b.keyword(" IN (");
        b.bind_list(binds.to_vec());
        b.keyword(")");
    }
}

fn compile_history_mods(
    mods: &[HistoryMod],
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
    allow_from_to: bool,
) -> Result<(), AqlError> {
    for modifier in mods {
        match modifier {
            HistoryMod::From(value) => {
                guard_from_to(allow_from_to, value)?;
                b.keyword(" AND (h.from_value = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(" OR h.from_display = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(")");
            }
            HistoryMod::To(value) => {
                guard_from_to(allow_from_to, value)?;
                b.keyword(" AND (h.to_value = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(" OR h.to_display = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(")");
            }
            HistoryMod::By(value) => {
                b.keyword(" AND h.author_id IN (SELECT id FROM users WHERE id = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(" OR username = ");
                b.bind(text_bind(value, ctx)?);
                b.keyword(")");
            }
            HistoryMod::After(value) => {
                b.keyword(" AND h.created_at > ");
                b.bind(text_bind(value, ctx)?);
            }
            HistoryMod::Before(value) => {
                b.keyword(" AND h.created_at < ");
                b.bind(text_bind(value, ctx)?);
            }
            HistoryMod::On(value) => {
                b.keyword(" AND date(h.created_at) = date(");
                b.bind(text_bind(value, ctx)?);
                b.keyword(")");
            }
            HistoryMod::During(start, end) => {
                b.keyword(" AND h.created_at >= ");
                b.bind(text_bind(start, ctx)?);
                b.keyword(" AND h.created_at <= ");
                b.bind(text_bind(end, ctx)?);
            }
        }
    }
    Ok(())
}

fn guard_from_to(allowed: bool, value: &Value) -> Result<(), AqlError> {
    if allowed {
        Ok(())
    } else {
        Err(AqlError::at(
            value.span(),
            "FROM and TO are only valid with CHANGED, not WAS",
        ))
    }
}

fn history_field_name(field: Field) -> &'static str {
    match field {
        Field::Assignee => "assignee",
        Field::Priority => "priority",
        Field::Reporter => "reporter",
        Field::Resolution => "resolution",
        Field::Type => "type",
        // Status, plus any field check_support has already rejected history on
        // (unreachable), map to the status changelog name.
        _ => "status",
    }
}

// ---------------------------------------------------------------------------
// Set functions
// ---------------------------------------------------------------------------

fn compile_set_function(
    field: Field,
    call: &FuncCall,
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
) -> Result<(), AqlError> {
    let name = call.name.to_ascii_lowercase();
    match name.as_str() {
        "membersof" => membersof(field, call, b),
        "watchedcards" => watchedcards(field, call, b, ctx),
        "cardhistory" => cardhistory(field, call, b, ctx),
        "linkedcards" => linkedcards(field, call, b),
        // Unreachable: is_set_function gated the dispatch.
        other => Err(AqlError::at(
            call.name_span,
            format!("'{other}' is not a set function"),
        )),
    }
}

/// `assignee IN membersOf("ATLAS")` — members of a project.
fn membersof(field: Field, call: &FuncCall, b: &mut SqlBuilder) -> Result<(), AqlError> {
    let col = match field {
        Field::Assignee => "cards.assignee_id",
        Field::Reporter => "cards.reporter_id",
        Field::Creator => "cards.creator_id",
        _ => {
            return Err(AqlError::at(
                call.span,
                "membersOf() returns users; use it with assignee, reporter or creator",
            ));
        }
    };
    let key = one_string_arg(call, "membersOf")?;
    b.keyword(col);
    b.keyword(" IN (SELECT m.user_id FROM project_members m JOIN projects p ON p.id = m.project_id WHERE p.key = ");
    b.bind(Bind::Text(key));
    b.keyword(")");
    Ok(())
}

/// `key IN watchedCards()` — cards the caller watches.
fn watchedcards(
    field: Field,
    call: &FuncCall,
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
) -> Result<(), AqlError> {
    require_card_field(field, call, "watchedCards")?;
    if !call.args.is_empty() {
        return Err(AqlError::at(call.span, "watchedCards() takes no arguments"));
    }
    b.keyword("cards.id IN (SELECT w.card_id FROM watchers w WHERE w.user_id = ");
    b.bind(Bind::Text(ctx.current_user_id.clone()));
    b.keyword(")");
    Ok(())
}

/// `key IN cardHistory()` — cards the caller has ever changed.
fn cardhistory(
    field: Field,
    call: &FuncCall,
    b: &mut SqlBuilder,
    ctx: &CompileCtx,
) -> Result<(), AqlError> {
    require_card_field(field, call, "cardHistory")?;
    if !call.args.is_empty() {
        return Err(AqlError::at(call.span, "cardHistory() takes no arguments"));
    }
    b.keyword("cards.id IN (SELECT DISTINCT h.card_id FROM card_history h WHERE h.author_id = ");
    b.bind(Bind::Text(ctx.current_user_id.clone()));
    b.keyword(")");
    Ok(())
}

/// `key IN linkedCards("ATLAS-1")` — cards linked to a given card, optionally of
/// one link type.
fn linkedcards(field: Field, call: &FuncCall, b: &mut SqlBuilder) -> Result<(), AqlError> {
    require_card_field(field, call, "linkedCards")?;
    if call.args.is_empty() || call.args.len() > 2 {
        return Err(AqlError::at(
            call.span,
            "linkedCards() takes a card key and an optional link type",
        ));
    }
    let key = string_of(&call.args[0])?.to_ascii_uppercase();
    b.keyword("cards.id IN (SELECT l.to_card_id FROM card_links l JOIN cards src ON src.id = l.from_card_id WHERE src.key = ");
    b.bind(Bind::Text(key));
    if let Some(link_type) = call.args.get(1) {
        b.keyword(" AND l.link_type = ");
        b.bind(Bind::Text(string_of(link_type)?));
    }
    b.keyword(")");
    Ok(())
}

fn require_card_field(field: Field, call: &FuncCall, name: &str) -> Result<(), AqlError> {
    if field == Field::Key {
        Ok(())
    } else {
        Err(AqlError::at(
            call.span,
            format!("{name}() returns cards; use it with key, e.g. key IN {name}(...)"),
        ))
    }
}

fn one_string_arg(call: &FuncCall, name: &str) -> Result<String, AqlError> {
    match call.args.as_slice() {
        [arg] => string_of(arg),
        _ => Err(AqlError::at(
            call.span,
            format!("{name}() takes exactly one argument"),
        )),
    }
}

fn string_of(value: &Value) -> Result<String, AqlError> {
    match value {
        Value::Str { text, .. } => Ok(text.clone()),
        Value::Num { raw, .. } => Ok(raw.clone()),
        Value::Func(call) => Err(AqlError::at(
            call.span,
            "expected a literal here, not a function",
        )),
    }
}

// ---------------------------------------------------------------------------
// ORDER BY
// ---------------------------------------------------------------------------

fn compile_order_by(fields: &[OrderField], b: &mut SqlBuilder) -> Result<(), AqlError> {
    b.keyword(" ORDER BY ");
    for order in fields {
        let column = order_column(order.field, order.span)?;
        b.keyword(column);
        b.keyword(match order.direction {
            Direction::Asc => " ASC, ",
            Direction::Desc => " DESC, ",
        });
    }
    // Always end with a stable tiebreak — the board's own order.
    b.keyword("cards.rank ASC, cards.key ASC");
    Ok(())
}

/// A sortable SQL expression for a field. Reference fields sort by a correlated
/// subquery over the natural key (rank, position, name); everything else by its
/// column. All `&'static str`, so ordering can never carry user text.
fn order_column(field: Field, span: super::ast::Span) -> Result<&'static str, AqlError> {
    let column = match field {
        Field::Created => "cards.created_at",
        Field::Updated => "cards.updated_at",
        Field::Due => "cards.due_date",
        Field::Resolved => "cards.resolved_at",
        Field::Started => "cards.start_date",
        Field::Estimate => "cards.estimate",
        Field::Key => "cards.key",
        Field::Summary => "cards.summary",
        Field::Priority => "(SELECT rank FROM priorities WHERE id = cards.priority_id)",
        Field::Status => "(SELECT position FROM statuses WHERE id = cards.status_id)",
        Field::StatusCategory => "(SELECT category FROM statuses WHERE id = cards.status_id)",
        Field::Type => "(SELECT name FROM card_types WHERE id = cards.type_id)",
        Field::Resolution => "(SELECT position FROM resolutions WHERE id = cards.resolution_id)",
        Field::Project => "(SELECT key FROM projects WHERE id = cards.project_id)",
        Field::Assignee => "(SELECT display_name FROM users WHERE id = cards.assignee_id)",
        Field::Reporter => "(SELECT display_name FROM users WHERE id = cards.reporter_id)",
        Field::Creator => "(SELECT display_name FROM users WHERE id = cards.creator_id)",
        Field::Parent => "cards.parent_id",
        Field::Description | Field::Text | Field::Labels => {
            return Err(AqlError::at(
                span,
                format!("cannot order by {}", field.as_str()),
            ));
        }
    };
    Ok(column)
}

// ---------------------------------------------------------------------------
// Value resolution
// ---------------------------------------------------------------------------

fn rhs_values(rhs: &Rhs) -> Vec<Value> {
    match rhs {
        Rhs::Single(v) => vec![v.clone()],
        Rhs::Set(vs) => vs.clone(),
        Rhs::Empty | Rhs::None => Vec::new(),
    }
}

fn single_value(rhs: &Rhs, op_span: super::ast::Span) -> Result<&Value, AqlError> {
    match rhs {
        Rhs::Single(v) => Ok(v),
        _ => Err(AqlError::at(op_span, "this operator needs a single value")),
    }
}

/// Resolves each value to a text bind, uppercasing key-shaped fields and
/// resolving scalar functions. A set function anywhere in a value list is a
/// user error, not a silent no-op.
fn scalar_binds(field: Field, values: &[Value], ctx: &CompileCtx) -> Result<Vec<Bind>, AqlError> {
    values
        .iter()
        .map(|value| Ok(Bind::Text(scalar_text(field, value, ctx)?)))
        .collect()
}

/// The text form of a value in a scalar position.
fn scalar_text(field: Field, value: &Value, ctx: &CompileCtx) -> Result<String, AqlError> {
    let text = match value {
        Value::Str { text, .. } => text.clone(),
        Value::Num { raw, .. } => raw.clone(),
        Value::Func(call) => {
            if functions::is_set_function(&call.name) {
                return Err(AqlError::at(
                    call.span,
                    format!(
                        "{}() returns a set and cannot be used as a single value; use it after IN",
                        call.name
                    ),
                ));
            }
            functions::resolve_scalar(call, &ctx.fn_ctx())?
        }
    };
    Ok(maybe_uppercase(field, text))
}

/// Resolves a value used in a history modifier — always text.
fn text_bind(value: &Value, ctx: &CompileCtx) -> Result<Bind, AqlError> {
    // History mod values are not key-shaped, so no uppercasing.
    let text = match value {
        Value::Str { text, .. } => text.clone(),
        Value::Num { raw, .. } => raw.clone(),
        Value::Func(call) => {
            if functions::is_set_function(&call.name) {
                return Err(AqlError::at(
                    call.span,
                    "a set function cannot be used as a history modifier value",
                ));
            }
            functions::resolve_scalar(call, &ctx.fn_ctx())?
        }
    };
    Ok(Bind::Text(text))
}

/// Card keys are stored uppercase, so match them uppercase.
fn maybe_uppercase(field: Field, text: String) -> String {
    if matches!(field, Field::Key | Field::Parent) {
        text.to_ascii_uppercase()
    } else {
        text
    }
}

fn number_bind(value: &Value) -> Result<Bind, AqlError> {
    match value {
        Value::Num { value, .. } => Ok(Bind::Real(*value)),
        Value::Str { text, span } => text.parse::<f64>().map(Bind::Real).map_err(|_| {
            AqlError::at(*span, format!("'{text}' is not a number"))
        }),
        Value::Func(call) => Err(AqlError::at(
            call.span,
            "expected a number here, not a function",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aql::lexer::lex;
    use crate::aql::parser::parse;
    use chrono::TimeZone;

    fn ctx() -> CompileCtx {
        CompileCtx {
            viewer_id: "viewer-1".to_owned(),
            viewer_is_admin: false,
            current_user_id: "viewer-1".to_owned(),
            now: Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, 0).unwrap(),
            limit: 50,
            offset: 0,
        }
    }

    fn compile_str(source: &str) -> Result<Compiled, AqlError> {
        let query = parse(lex(source).unwrap()).unwrap();
        compile(&query, &ctx())
    }

    /// Every `?` in the SQL must have exactly one bind, and vice versa — the
    /// structural invariant, checked on every compiled query the tests produce.
    fn assert_balanced(c: &Compiled) {
        let placeholders = c.sql.matches('?').count();
        assert_eq!(
            placeholders,
            c.binds.len(),
            "placeholder/bind mismatch in: {}",
            c.sql
        );
    }

    #[test]
    fn the_access_predicate_is_always_present() {
        let c = compile_str("status = Done").unwrap();
        assert!(c.sql.contains("project_members"), "{}", c.sql);
        assert!(c.sql.contains("cards.deleted_at IS NULL"), "{}", c.sql);
        assert_balanced(&c);
    }

    #[test]
    fn an_empty_query_still_scopes_and_paginates() {
        let c = compile_str("").unwrap();
        assert!(c.sql.contains("1 = 1"), "{}", c.sql);
        assert!(c.sql.contains("LIMIT"), "{}", c.sql);
        assert_balanced(&c);
    }

    #[test]
    fn no_user_value_appears_in_the_sql_text() {
        // The whole invariant. The literal must be a bind, never in the string.
        let c = compile_str("summary ~ \"secret sauce\"").unwrap();
        assert!(!c.sql.contains("secret sauce"), "value leaked: {}", c.sql);
        assert!(c.binds.iter().any(|b| matches!(b, Bind::Text(t) if t.contains("secret sauce"))));
        assert_balanced(&c);
    }

    #[test]
    fn an_injection_payload_is_bound_as_data() {
        let c = compile_str("summary ~ \"'; DROP TABLE cards; --\"").unwrap();
        assert!(!c.sql.contains("DROP TABLE"), "{}", c.sql);
        assert_eq!(c.sql.matches("DROP").count(), 0);
        assert_balanced(&c);
    }

    #[test]
    fn every_operator_compiles_and_stays_balanced() {
        for src in [
            "status = Done",
            "status != Done",
            "status IN (A, B, C)",
            "status NOT IN (A, B)",
            "summary ~ hello",
            "summary !~ hello",
            "resolution IS EMPTY",
            "resolution IS NOT EMPTY",
            "priority > High",
            "priority <= Low",
            "created > now()",
            "due < startOfWeek(-1w)",
            "estimate >= 5",
            "assignee = currentUser()",
            "labels = urgent",
            "labels IS EMPTY",
            "parent IS EMPTY",
            "key = ATLAS-1",
            "status WAS Done",
            "status WAS IN (A, B)",
            "status WAS NOT Done",
            "assignee CHANGED",
            "status CHANGED FROM \"In Progress\" TO Done AFTER -7d",
            "type WAS Bug",
        ] {
            let c = compile_str(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_balanced(&c);
        }
    }

    #[test]
    fn the_matrix_rejections_carry_a_reason() {
        // = on a text field.
        let err = compile_str("summary = hello").unwrap_err();
        assert!(err.message.contains("text"), "{}", err.message);

        // > on a non-orderable field.
        let err = compile_str("status > Done").unwrap_err();
        assert!(err.message.contains("orderable"), "{}", err.message);

        // ~ on a non-text field.
        let err = compile_str("status ~ Done").unwrap_err();
        assert!(err.message.contains("full-text"), "{}", err.message);

        // WAS on a non-history field.
        let err = compile_str("summary WAS hello").unwrap_err();
        assert!(err.message.contains("history"), "{}", err.message);
    }

    #[test]
    fn currentuser_binds_the_context_id_not_the_word() {
        let c = compile_str("assignee = currentUser()").unwrap();
        assert!(!c.sql.contains("currentUser"), "{}", c.sql);
        assert!(c.binds.contains(&Bind::Text("viewer-1".to_owned())));
    }

    #[test]
    fn admin_flips_the_access_bool() {
        let query = parse(lex("status = Done").unwrap()).unwrap();
        let mut ctx = ctx();
        ctx.viewer_is_admin = true;
        let c = compile(&query, &ctx).unwrap();
        assert!(c.binds.contains(&Bind::Bool(true)));
    }

    #[test]
    fn a_set_function_only_works_after_in() {
        assert!(compile_str("assignee = membersOf(\"ATLAS\")").is_err());
        assert!(compile_str("assignee IN membersOf(\"ATLAS\")").is_ok());
        assert!(compile_str("key IN watchedCards()").is_ok());
    }
}

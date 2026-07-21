//! The recursive-descent parser: token stream -> [`Query`].
//!
//! Precedence, lowest binding first: `OR`, then `AND`, then prefix `NOT`, then a
//! primary (a parenthesised expression or a single condition). `ORDER BY` is a
//! trailer parsed once at the end.
//!
//! The parser only ever produces the closed enums in [`super::ast`]: a field is
//! a [`Field`], an operator is an [`Op`]. Everything the user typed as a *value*
//! becomes a [`Value`], which the compiler can only turn into a bind. So "reject
//! anything not in the grammar" and "user data cannot become SQL" are the same
//! property, enforced here.

use super::ast::{
    AqlError, Cond, Direction, Field, FilterRef, FuncCall, HistoryMod, Node, Op, OrderField, Query,
    Rhs, Span, Value,
};
use super::lexer::{Spanned, Tok};

/// The deepest nesting of parentheses / `NOT` the parser will follow.
///
/// A guard against `((((((…` exhausting the stack. [`super::lexer::MAX_TOKENS`]
/// already bounds the token count, but recursion depth is the resource that
/// actually runs out, and the fuzzer targets it directly.
const MAX_DEPTH: usize = 128;

/// Parses a token stream into a [`Query`].
///
/// # Errors
///
/// [`AqlError`] with the span of the offending token for anything the grammar
/// does not accept.
pub fn parse(tokens: Vec<Spanned>) -> Result<Query, AqlError> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let query = parser.parse_query()?;
    Ok(query)
}

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Spanned {
        // The lexer guarantees a trailing Eof, so this never indexes past the
        // end even when `pos` has advanced onto it.
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_tok(&self) -> &Tok {
        &self.peek().tok
    }

    /// The token `offset` positions ahead, clamped to the trailing `Eof`.
    fn peek_at(&self, offset: usize) -> &Tok {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx].tok
    }

    fn peek_span(&self) -> Span {
        self.peek().span
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek_tok(), Tok::Eof)
    }

    fn bump(&mut self) -> Spanned {
        let token = self.tokens[self.pos.min(self.tokens.len() - 1)].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        token
    }

    /// The current token as a keyword, lowercased, if it is a bareword.
    fn peek_keyword(&self) -> Option<String> {
        match self.peek_tok() {
            Tok::Word(w) => Some(w.to_ascii_lowercase()),
            _ => None,
        }
    }

    /// True and consumes if the current token is the named keyword.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.peek_keyword().as_deref() == Some(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn enter(&mut self, span: Span) -> Result<(), AqlError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(AqlError::at(
                span,
                format!("the query nests deeper than {MAX_DEPTH} levels"),
            ));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // -- grammar ------------------------------------------------------------

    fn parse_query(&mut self) -> Result<Query, AqlError> {
        // A query may be empty (match everything) or start straight at ORDER BY.
        let predicate = if self.at_eof() || self.at_order_by() {
            None
        } else {
            Some(self.parse_or()?)
        };

        let order_by = if self.at_order_by() {
            self.parse_order_by()?
        } else {
            Vec::new()
        };

        if !self.at_eof() {
            let token = self.peek();
            return Err(AqlError::at(
                token.span,
                format!("unexpected {} after the query", token.tok.label()),
            ));
        }

        Ok(Query {
            predicate,
            order_by,
        })
    }

    fn at_order_by(&self) -> bool {
        self.peek_keyword().as_deref() == Some("order")
    }

    fn parse_or(&mut self) -> Result<Node, AqlError> {
        let span = self.peek_span();
        self.enter(span)?;
        let mut left = self.parse_and()?;
        while self.eat_keyword("or") {
            let right = self.parse_and()?;
            left = Node::Or(Box::new(left), Box::new(right));
        }
        self.leave();
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Node, AqlError> {
        let mut left = self.parse_not()?;
        while self.eat_keyword("and") {
            let right = self.parse_not()?;
            left = Node::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Node, AqlError> {
        if self.peek_keyword().as_deref() == Some("not") {
            let span = self.peek_span();
            self.bump();
            self.enter(span)?;
            let inner = self.parse_not()?;
            self.leave();
            return Ok(Node::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Node, AqlError> {
        if matches!(self.peek_tok(), Tok::LParen) {
            let open = self.peek_span();
            self.bump();
            self.enter(open)?;
            let inner = self.parse_or()?;
            self.leave();
            if !matches!(self.peek_tok(), Tok::RParen) {
                return Err(AqlError::at(
                    self.peek_span(),
                    format!("expected ')' but found {}", self.peek_tok().label()),
                ));
            }
            self.bump();
            return Ok(inner);
        }
        self.parse_condition()
    }

    fn parse_condition(&mut self) -> Result<Node, AqlError> {
        // The field, which also catches the `filter = ...` composition form.
        let field_tok = self.peek().clone();
        let Tok::Word(word) = &field_tok.tok else {
            return Err(AqlError::at(
                field_tok.span,
                format!("expected a field name but found {}", field_tok.tok.label()),
            ));
        };

        if word.eq_ignore_ascii_case("filter") {
            return self.parse_filter_ref();
        }

        let Some(field) = Field::parse(word) else {
            return Err(AqlError::at(
                field_tok.span,
                format!("unknown field '{word}'; it is not one Atlas can search on"),
            ));
        };
        let field_span = field_tok.span;
        self.bump();

        let (op, op_span) = self.parse_operator()?;
        let rhs = self.parse_rhs(field, op)?;
        let history = if matches!(
            op,
            Op::Was | Op::WasNot | Op::WasIn | Op::WasNotIn | Op::Changed
        ) {
            self.parse_history_mods()?
        } else {
            Vec::new()
        };

        Ok(Node::Cond(Cond {
            field,
            field_span,
            op,
            op_span,
            rhs,
            history,
        }))
    }

    fn parse_filter_ref(&mut self) -> Result<Node, AqlError> {
        let start = self.peek_span();
        self.bump(); // `filter`
        if !matches!(self.peek_tok(), Tok::Eq) {
            return Err(AqlError::at(
                self.peek_span(),
                "a filter reference uses '=' only: filter = \"My Filter\" or filter = 42",
            ));
        }
        self.bump(); // `=`
        let target = self.parse_value()?;
        let span = start.to(target.span());
        Ok(Node::Filter(FilterRef { target, span }))
    }

    fn parse_operator(&mut self) -> Result<(Op, Span), AqlError> {
        let span = self.peek_span();
        let op = match self.peek_tok() {
            Tok::Eq => Op::Eq,
            Tok::Ne => Op::Ne,
            Tok::Gt => Op::Gt,
            Tok::Ge => Op::Ge,
            Tok::Lt => Op::Lt,
            Tok::Le => Op::Le,
            Tok::Match => Op::Match,
            Tok::NotMatch => Op::NotMatch,
            Tok::Word(_) => return self.parse_word_operator(),
            other => {
                return Err(AqlError::at(
                    span,
                    format!("expected an operator but found {}", other.label()),
                ));
            }
        };
        self.bump();
        Ok((op, span))
    }

    /// Parses the keyword operators, including the multi-word ones.
    fn parse_word_operator(&mut self) -> Result<(Op, Span), AqlError> {
        let start = self.peek_span();
        let kw = self.peek_keyword().unwrap_or_default();
        match kw.as_str() {
            "in" => {
                self.bump();
                Ok((Op::In, start))
            }
            "changed" => {
                self.bump();
                Ok((Op::Changed, start))
            }
            "is" => {
                self.bump();
                if self.eat_keyword("not") {
                    Ok((Op::IsNot, start.to(self.previous_span())))
                } else {
                    Ok((Op::Is, start))
                }
            }
            "not" => {
                self.bump();
                if self.eat_keyword("in") {
                    Ok((Op::NotIn, start.to(self.previous_span())))
                } else {
                    Err(AqlError::at(
                        start,
                        "expected 'IN' after 'NOT'; write '!=' for not-equal",
                    ))
                }
            }
            "was" => {
                self.bump();
                if self.eat_keyword("not") {
                    if self.eat_keyword("in") {
                        Ok((Op::WasNotIn, start.to(self.previous_span())))
                    } else {
                        Ok((Op::WasNot, start.to(self.previous_span())))
                    }
                } else if self.eat_keyword("in") {
                    Ok((Op::WasIn, start.to(self.previous_span())))
                } else {
                    Ok((Op::Was, start))
                }
            }
            other => Err(AqlError::at(
                start,
                format!("expected an operator but found '{other}'"),
            )),
        }
    }

    fn previous_span(&self) -> Span {
        let idx = self.pos.saturating_sub(1);
        self.tokens[idx.min(self.tokens.len() - 1)].span
    }

    fn parse_rhs(&mut self, field: Field, op: Op) -> Result<Rhs, AqlError> {
        let _ = field;
        match op {
            Op::In | Op::NotIn | Op::WasIn | Op::WasNotIn => {
                // `IN membersOf(...)` — a set function stands in for the list. A
                // bareword followed by `(` is a call; anything else must be a
                // parenthesised value list.
                if let Tok::Word(_) = self.peek_tok()
                    && matches!(self.peek_at(1), Tok::LParen)
                {
                    return Ok(Rhs::Set(vec![self.parse_value()?]));
                }
                Ok(Rhs::Set(self.parse_value_set()?))
            }
            Op::Is | Op::IsNot => self.parse_empty(op),
            Op::Changed => Ok(Rhs::None),
            Op::Eq
            | Op::Ne
            | Op::Gt
            | Op::Ge
            | Op::Lt
            | Op::Le
            | Op::Match
            | Op::NotMatch
            | Op::Was
            | Op::WasNot => Ok(Rhs::Single(self.parse_value()?)),
        }
    }

    /// `IS`/`IS NOT` take EMPTY or NULL and nothing else — a grammar rule, so it
    /// is enforced here with the offending token's span.
    fn parse_empty(&mut self, op: Op) -> Result<Rhs, AqlError> {
        match self.peek_keyword().as_deref() {
            Some("empty" | "null") => {
                self.bump();
                Ok(Rhs::Empty)
            }
            _ => Err(AqlError::at(
                self.peek_span(),
                format!(
                    "'{}' must be followed by EMPTY or NULL, not {}",
                    op.as_str(),
                    self.peek_tok().label()
                ),
            )),
        }
    }

    fn parse_value_set(&mut self) -> Result<Vec<Value>, AqlError> {
        if !matches!(self.peek_tok(), Tok::LParen) {
            return Err(AqlError::at(
                self.peek_span(),
                format!(
                    "expected '(' to open a value list but found {}",
                    self.peek_tok().label()
                ),
            ));
        }
        self.bump();

        let mut values = Vec::new();
        if !matches!(self.peek_tok(), Tok::RParen) {
            loop {
                values.push(self.parse_value()?);
                if self.eat_comma() {
                    // A trailing comma before `)` is tolerated.
                    if matches!(self.peek_tok(), Tok::RParen) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }

        if !matches!(self.peek_tok(), Tok::RParen) {
            return Err(AqlError::at(
                self.peek_span(),
                format!(
                    "expected ',' or ')' in the value list but found {}",
                    self.peek_tok().label()
                ),
            ));
        }
        self.bump();

        if values.is_empty() {
            return Err(AqlError::at(
                self.previous_span(),
                "an IN list needs at least one value",
            ));
        }
        Ok(values)
    }

    fn eat_comma(&mut self) -> bool {
        if matches!(self.peek_tok(), Tok::Comma) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn parse_history_mods(&mut self) -> Result<Vec<HistoryMod>, AqlError> {
        let mut mods = Vec::new();
        while let Some(kw) = self.peek_keyword() {
            let modifier = match kw.as_str() {
                "from" => {
                    self.bump();
                    HistoryMod::From(self.parse_value()?)
                }
                "to" => {
                    self.bump();
                    HistoryMod::To(self.parse_value()?)
                }
                "by" => {
                    self.bump();
                    HistoryMod::By(self.parse_value()?)
                }
                "after" => {
                    self.bump();
                    HistoryMod::After(self.parse_value()?)
                }
                "before" => {
                    self.bump();
                    HistoryMod::Before(self.parse_value()?)
                }
                "on" => {
                    self.bump();
                    HistoryMod::On(self.parse_value()?)
                }
                "during" => {
                    self.bump();
                    let pair = self.parse_value_set()?;
                    let mut it = pair.into_iter();
                    match (it.next(), it.next(), it.next()) {
                        (Some(start), Some(end), None) => HistoryMod::During(start, end),
                        _ => {
                            return Err(AqlError::at(
                                self.previous_span(),
                                "DURING takes exactly two dates: DURING (start, end)",
                            ));
                        }
                    }
                }
                _ => break,
            };
            mods.push(modifier);
        }
        Ok(mods)
    }

    fn parse_value(&mut self) -> Result<Value, AqlError> {
        let token = self.peek().clone();
        match token.tok {
            Tok::Str(text) => {
                self.bump();
                Ok(Value::Str {
                    text,
                    span: token.span,
                })
            }
            Tok::Num(raw) => {
                self.bump();
                // A duration like `-1w` has no `f64` form; keep it as a string
                // value, which is exactly what `functions.rs` reads it back as.
                match raw.parse::<f64>() {
                    Ok(value) if value.is_finite() => Ok(Value::Num {
                        value,
                        raw,
                        span: token.span,
                    }),
                    _ => Ok(Value::Str {
                        text: raw,
                        span: token.span,
                    }),
                }
            }
            Tok::Word(word) => {
                self.bump();
                // A function call if a `(` follows.
                if matches!(self.peek_tok(), Tok::LParen) {
                    return self.parse_func_call(word, token.span);
                }
                Ok(Value::Str {
                    text: word,
                    span: token.span,
                })
            }
            other => Err(AqlError::at(
                token.span,
                format!("expected a value but found {}", other.label()),
            )),
        }
    }

    fn parse_func_call(&mut self, name: String, name_span: Span) -> Result<Value, AqlError> {
        self.enter(name_span)?;
        self.bump(); // `(`
        let mut args = Vec::new();
        if !matches!(self.peek_tok(), Tok::RParen) {
            loop {
                args.push(self.parse_value()?);
                if self.eat_comma() {
                    if matches!(self.peek_tok(), Tok::RParen) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        if !matches!(self.peek_tok(), Tok::RParen) {
            return Err(AqlError::at(
                self.peek_span(),
                format!(
                    "expected ',' or ')' in {name}(...) but found {}",
                    self.peek_tok().label()
                ),
            ));
        }
        let close = self.peek_span();
        self.bump();
        self.leave();
        Ok(Value::Func(FuncCall {
            name,
            name_span,
            args,
            span: name_span.to(close),
        }))
    }

    fn parse_order_by(&mut self) -> Result<Vec<OrderField>, AqlError> {
        self.bump(); // `order`
        if !self.eat_keyword("by") {
            return Err(AqlError::at(
                self.peek_span(),
                format!(
                    "expected 'BY' after 'ORDER' but found {}",
                    self.peek_tok().label()
                ),
            ));
        }

        let mut fields = Vec::new();
        loop {
            let token = self.peek().clone();
            let Tok::Word(word) = &token.tok else {
                return Err(AqlError::at(
                    token.span,
                    format!(
                        "expected a field to order by but found {}",
                        token.tok.label()
                    ),
                ));
            };
            let Some(field) = Field::parse(word) else {
                return Err(AqlError::at(
                    token.span,
                    format!("cannot order by '{word}'; it is not a field"),
                ));
            };
            self.bump();

            let direction = match self.peek_keyword().as_deref() {
                Some("asc") => {
                    self.bump();
                    Direction::Asc
                }
                Some("desc") => {
                    self.bump();
                    Direction::Desc
                }
                _ => Direction::Asc,
            };

            fields.push(OrderField {
                field,
                span: token.span,
                direction,
            });

            if !self.eat_comma() {
                break;
            }
        }
        Ok(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aql::lexer::lex;

    fn parse_str(source: &str) -> Result<Query, AqlError> {
        parse(lex(source).unwrap())
    }

    fn cond(node: &Node) -> &Cond {
        match node {
            Node::Cond(c) => c,
            other => panic!("expected a condition, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_equality_parses() {
        let query = parse_str("status = Done").unwrap();
        let c = cond(query.predicate.as_ref().unwrap());
        assert_eq!(c.field, Field::Status);
        assert_eq!(c.op, Op::Eq);
        match &c.rhs {
            Rhs::Single(Value::Str { text, .. }) => assert_eq!(text, "Done"),
            other => panic!("expected a single string value, got {other:?}"),
        }
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // `a OR b AND c` must parse as `a OR (b AND c)`.
        let query = parse_str("status = A OR status = B AND status = C").unwrap();
        match query.predicate.unwrap() {
            Node::Or(_, right) => assert!(matches!(*right, Node::And(_, _))),
            other => panic!("expected OR at the root, got {other:?}"),
        }
    }

    #[test]
    fn parentheses_override_precedence() {
        let query = parse_str("(status = A OR status = B) AND status = C").unwrap();
        match query.predicate.unwrap() {
            Node::And(left, _) => assert!(matches!(*left, Node::Or(_, _))),
            other => panic!("expected AND at the root, got {other:?}"),
        }
    }

    #[test]
    fn not_is_prefix_and_nests() {
        let query = parse_str("NOT status = Done").unwrap();
        assert!(matches!(query.predicate.unwrap(), Node::Not(_)));
    }

    #[test]
    fn the_multi_word_operators_parse() {
        for (src, op) in [
            ("status IN (A, B)", Op::In),
            ("status NOT IN (A, B)", Op::NotIn),
            ("resolution IS EMPTY", Op::Is),
            ("resolution IS NOT EMPTY", Op::IsNot),
            ("status WAS Done", Op::Was),
            ("status WAS NOT Done", Op::WasNot),
            ("status WAS IN (A, B)", Op::WasIn),
            ("status WAS NOT IN (A, B)", Op::WasNotIn),
            ("status CHANGED", Op::Changed),
        ] {
            let query = parse_str(src).unwrap();
            assert_eq!(cond(query.predicate.as_ref().unwrap()).op, op, "{src}");
        }
    }

    #[test]
    fn changed_takes_history_modifiers() {
        let query = parse_str("status CHANGED FROM \"In Progress\" TO Done AFTER -7d").unwrap();
        let c = cond(query.predicate.as_ref().unwrap());
        assert_eq!(c.op, Op::Changed);
        assert_eq!(c.history.len(), 3);
        assert!(matches!(c.history[0], HistoryMod::From(_)));
        assert!(matches!(c.history[1], HistoryMod::To(_)));
        assert!(matches!(c.history[2], HistoryMod::After(_)));
    }

    #[test]
    fn a_function_value_parses_with_its_argument() {
        let query = parse_str("assignee = currentUser()").unwrap();
        let c = cond(query.predicate.as_ref().unwrap());
        match &c.rhs {
            Rhs::Single(Value::Func(call)) => {
                assert_eq!(call.name, "currentUser");
                assert!(call.args.is_empty());
            }
            other => panic!("expected a function, got {other:?}"),
        }

        let query = parse_str("due < startOfWeek(-1w)").unwrap();
        let c = cond(query.predicate.as_ref().unwrap());
        match &c.rhs {
            Rhs::Single(Value::Func(call)) => {
                assert_eq!(call.name, "startOfWeek");
                assert_eq!(call.args.len(), 1);
            }
            other => panic!("expected a function, got {other:?}"),
        }
    }

    #[test]
    fn order_by_parses_with_directions_and_defaults_to_asc() {
        let query = parse_str("status = Done ORDER BY priority DESC, created").unwrap();
        assert_eq!(query.order_by.len(), 2);
        assert_eq!(query.order_by[0].field, Field::Priority);
        assert_eq!(query.order_by[0].direction, Direction::Desc);
        assert_eq!(query.order_by[1].field, Field::Created);
        assert_eq!(query.order_by[1].direction, Direction::Asc);
    }

    #[test]
    fn a_query_can_be_order_by_only() {
        let query = parse_str("ORDER BY created DESC").unwrap();
        assert!(query.predicate.is_none());
        assert_eq!(query.order_by.len(), 1);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let query = parse_str("   ").unwrap();
        assert!(query.predicate.is_none());
        assert!(query.order_by.is_empty());
    }

    #[test]
    fn a_filter_reference_parses() {
        let query = parse_str("filter = \"My Filter\" AND status = Done").unwrap();
        match query.predicate.unwrap() {
            Node::And(left, _) => assert!(matches!(*left, Node::Filter(_))),
            other => panic!("expected AND, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_field_is_rejected_with_its_span() {
        let err = parse_str("bogus = 1").unwrap_err();
        assert!(err.message.contains("unknown field 'bogus'"));
        assert_eq!(err.span.unwrap().start, 0);
    }

    #[test]
    fn a_missing_closing_paren_is_reported() {
        let err = parse_str("(status = Done").unwrap_err();
        assert!(err.message.contains("expected ')'"));
    }

    #[test]
    fn trailing_junk_after_the_query_is_rejected() {
        let err = parse_str("status = Done garbage").unwrap_err();
        assert!(err.message.contains("after the query"));
    }

    #[test]
    fn deeply_nested_parens_error_rather_than_overflow_the_stack() {
        let deep = "(".repeat(MAX_DEPTH + 5);
        let src = format!("{deep}status = Done");
        assert!(parse_str(&src).is_err());
    }
}

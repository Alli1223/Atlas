//! A hand-written tokeniser.
//!
//! It never fails on structure — an unterminated string or a stray byte becomes
//! a token the parser can report against with a span, rather than a panic. That
//! is a requirement, not a nicety: `tests/aql_fuzz.rs` throws adversarial bytes
//! at this and asserts it never panics.

use super::ast::{AqlError, Span};

/// A lexical token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// A bareword: a field name, a keyword, a function name, or an unquoted
    /// value. Case is preserved; the parser lowercases where it matters.
    Word(String),
    /// A quoted string, already unescaped.
    Str(String),
    /// A number, kept as text so `3` and `3.0` stay distinct in the echo.
    Num(String),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,
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
    /// `~`
    Match,
    /// `!~`
    NotMatch,
    /// End of input. Always the last token, so the parser never indexes past
    /// the end.
    Eof,
}

impl Tok {
    /// A short label for an error message.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Word(w) => format!("'{w}'"),
            Self::Str(_) => "a quoted string".to_owned(),
            Self::Num(n) => format!("'{n}'"),
            Self::LParen => "'('".to_owned(),
            Self::RParen => "')'".to_owned(),
            Self::Comma => "','".to_owned(),
            Self::Eq => "'='".to_owned(),
            Self::Ne => "'!='".to_owned(),
            Self::Gt => "'>'".to_owned(),
            Self::Ge => "'>='".to_owned(),
            Self::Lt => "'<'".to_owned(),
            Self::Le => "'<='".to_owned(),
            Self::Match => "'~'".to_owned(),
            Self::NotMatch => "'!~'".to_owned(),
            Self::Eof => "the end of the query".to_owned(),
        }
    }
}

/// A token together with where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned {
    /// The token.
    pub tok: Tok,
    /// Its byte span in the source.
    pub span: Span,
}

/// The longest query, in bytes.
///
/// A bound rather than a promise the caller made: the fuzzer and a hostile
/// client both send megabytes, and a parser that keeps working on them is a
/// denial-of-service surface. Refused before a single token is produced.
pub const MAX_QUERY_BYTES: usize = 16 * 1024;

/// The most tokens one query may hold, a second guard on pathological input
/// (`((((((...` is one byte per token).
pub const MAX_TOKENS: usize = 4096;

/// Tokenises a query into a stream ending in [`Tok::Eof`].
///
/// # Errors
///
/// Only for the two whole-input limits above and an unterminated string — every
/// other byte becomes a token. Unknown punctuation is reported here because the
/// parser could not say anything more useful about it than the lexer can.
pub fn lex(source: &str) -> Result<Vec<Spanned>, AqlError> {
    if source.len() > MAX_QUERY_BYTES {
        return Err(AqlError::whole(format!(
            "the query is {} bytes; the limit is {MAX_QUERY_BYTES}",
            source.len()
        )));
    }

    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Whitespace.
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        if tokens.len() >= MAX_TOKENS {
            return Err(AqlError::at(
                Span::new(i, i + 1),
                format!("the query has more than {MAX_TOKENS} tokens"),
            ));
        }

        let (tok, end) = lex_one(source, bytes, i)?;
        tokens.push(spanned(tok, i, end));
        i = end;
    }

    tokens.push(spanned(Tok::Eof, source.len(), source.len()));
    Ok(tokens)
}

/// Lexes the single token starting at `start` (never whitespace), returning it
/// and the offset one past its end.
fn lex_one(source: &str, bytes: &[u8], start: usize) -> Result<(Tok, usize), AqlError> {
    let b = bytes[start];
    match b {
        b'(' => Ok((Tok::LParen, start + 1)),
        b')' => Ok((Tok::RParen, start + 1)),
        b',' => Ok((Tok::Comma, start + 1)),
        b'=' => Ok((Tok::Eq, start + 1)),
        b'~' => Ok((Tok::Match, start + 1)),
        b'!' => match bytes.get(start + 1) {
            Some(&b'=') => Ok((Tok::Ne, start + 2)),
            Some(&b'~') => Ok((Tok::NotMatch, start + 2)),
            _ => Err(AqlError::at(
                Span::new(start, start + 1),
                "unexpected '!'; expected '!=' or '!~'",
            )),
        },
        b'>' if bytes.get(start + 1) == Some(&b'=') => Ok((Tok::Ge, start + 2)),
        b'>' => Ok((Tok::Gt, start + 1)),
        b'<' if bytes.get(start + 1) == Some(&b'=') => Ok((Tok::Le, start + 2)),
        b'<' => Ok((Tok::Lt, start + 1)),
        b'"' | b'\'' => {
            let (text, end) = lex_string(source, bytes, start)?;
            Ok((Tok::Str(text), end))
        }
        _ if is_number_start(bytes, start) => {
            let end = lex_number(bytes, start);
            Ok((Tok::Num(source[start..end].to_owned()), end))
        }
        // `is_word_start`, not `is_word_byte`: `-` and `.` may *continue* a word
        // but cannot begin one, so a lone `-` (found by the fuzzer) falls through
        // to the stray-character arm and is reported, rather than entering
        // `lex_word` against its start invariant.
        _ if is_word_start(b) => {
            let end = lex_word(bytes, start);
            Ok((Tok::Word(source[start..end].to_owned()), end))
        }
        _ => {
            // A byte that starts no token — a stray metacharacter, a control
            // byte, the leading byte of a multibyte glyph in a place a value is
            // not allowed. Consume one *character* so the offset stays on a
            // boundary, and report it. The parser turning this into a BadRequest
            // is the whole "SQL metacharacters are data, not syntax" guarantee:
            // `;` and `--` never reach the compiler.
            let end = next_char_boundary(source, start);
            Err(AqlError::at(
                Span::new(start, end),
                format!("unexpected character {:?}", &source[start..end]),
            ))
        }
    }
}

fn spanned(tok: Tok, start: usize, end: usize) -> Spanned {
    Spanned {
        tok,
        span: Span::new(start, end),
    }
}

/// Whether a byte can appear in a bareword.
///
/// ASCII letters, digits, and the handful of punctuation that shows up in real
/// values: `-` (card keys `ATLAS-42`), `_`, `.`, `@` (usernames/emails), `/`
/// (paths), `+` (only relevant inside numbers, handled there). A bareword never
/// contains a quote, a paren, a comma, or an operator character, so those always
/// break the word — which is what lets `status=Done` lex without spaces.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@' | b'/' | b'*')
}

fn is_word_start(b: u8) -> bool {
    // A word starts with a letter, `_`, `@`, `/` or `*`. Not a digit or sign —
    // those begin a number — and not `-` alone, so `a-b` is one word but a lone
    // `-` is not the start of one.
    b.is_ascii_alphabetic() || matches!(b, b'_' | b'@' | b'/' | b'*')
}

/// Whether position `i` starts a number, including a signed relative duration
/// like `-1w` or `+3d`.
fn is_number_start(bytes: &[u8], i: usize) -> bool {
    let b = bytes[i];
    if b.is_ascii_digit() {
        return true;
    }
    // A sign followed by a digit: a relative offset argument. A sign followed by
    // anything else is not a token start (and will be reported as a stray byte).
    matches!(b, b'+' | b'-') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
}

/// Consumes a number or relative-duration token, returning the end offset.
///
/// Accepts `[+-]?digits(.digits)?[a-zA-Z]*` so that a plain `5`, a decimal
/// `3.5`, and a duration `-2w` all lex as one `Num`. The trailing unit letters
/// are kept in the raw text; [`crate::aql::functions`] interprets them, so the
/// lexer needs no calendar knowledge.
fn lex_number(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    // Trailing unit letters: the `w` in `-1w`, the `d` in `3d`.
    while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
        i += 1;
    }
    i
}

/// Consumes a bareword, returning the end offset.
fn lex_word(bytes: &[u8], start: usize) -> usize {
    debug_assert!(is_word_start(bytes[start]));
    let mut i = start;
    while bytes.get(i).is_some_and(|&b| is_word_byte(b)) {
        i += 1;
    }
    i
}

/// Consumes a quoted string, returning `(unescaped, end_offset)`.
///
/// Supports `\"`, `\'`, `\\`, `\n`, `\t` inside either quote style. An
/// unterminated string is the one lexer error a value can cause, and it is
/// reported at the opening quote so the underline lands somewhere useful.
fn lex_string(source: &str, bytes: &[u8], start: usize) -> Result<(String, usize), AqlError> {
    let quote = bytes[start];
    let mut out = String::new();
    let mut i = start + 1;

    while i < bytes.len() {
        let b = bytes[i];
        if b == quote {
            return Ok((out, i + 1));
        }
        if b == b'\\' {
            match bytes.get(i + 1) {
                Some(&b'\\') => out.push('\\'),
                Some(&b'n') => out.push('\n'),
                Some(&b't') => out.push('\t'),
                Some(&c) if c == quote => out.push(quote as char),
                Some(&b'"') => out.push('"'),
                Some(&b'\'') => out.push('\''),
                // An unknown escape keeps both characters verbatim rather than
                // failing: it is data, and a query is not worth rejecting over a
                // stray backslash.
                Some(_) => {
                    out.push('\\');
                    i += 1;
                    // Push the escaped character respecting UTF-8 boundaries.
                    let end = next_char_boundary(source, i);
                    out.push_str(&source[i..end]);
                    i = end;
                    continue;
                }
                None => break,
            }
            i += 2;
            continue;
        }
        // Copy one whole character so multibyte content survives intact.
        let end = next_char_boundary(source, i);
        out.push_str(&source[i..end]);
        i = end;
    }

    Err(AqlError::at(
        Span::new(start, source.len()),
        "unterminated string; add the closing quote",
    ))
}

/// The end offset of the character starting at `i`, so slices never split a
/// multibyte glyph.
fn next_char_boundary(source: &str, i: usize) -> usize {
    let mut end = i + 1;
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<Tok> {
        lex(source).unwrap().into_iter().map(|s| s.tok).collect()
    }

    #[test]
    fn operators_lex_with_their_two_character_forms() {
        assert_eq!(
            kinds("a >= b <= c != d !~ e ~ f"),
            vec![
                Tok::Word("a".into()),
                Tok::Ge,
                Tok::Word("b".into()),
                Tok::Le,
                Tok::Word("c".into()),
                Tok::Ne,
                Tok::Word("d".into()),
                Tok::NotMatch,
                Tok::Word("e".into()),
                Tok::Match,
                Tok::Word("f".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn a_status_equals_done_needs_no_spaces() {
        assert_eq!(
            kinds("status=Done"),
            vec![
                Tok::Word("status".into()),
                Tok::Eq,
                Tok::Word("Done".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn relative_durations_lex_as_one_number_token() {
        assert_eq!(
            kinds("startOfWeek(-1w)"),
            vec![
                Tok::Word("startOfWeek".into()),
                Tok::LParen,
                Tok::Num("-1w".into()),
                Tok::RParen,
                Tok::Eof,
            ]
        );
        assert_eq!(kinds("3d"), vec![Tok::Num("3d".into()), Tok::Eof]);
        assert_eq!(kinds("3.5"), vec![Tok::Num("3.5".into()), Tok::Eof]);
    }

    #[test]
    fn a_card_key_is_a_single_word() {
        assert_eq!(
            kinds("key = ATLAS-42"),
            vec![
                Tok::Word("key".into()),
                Tok::Eq,
                Tok::Word("ATLAS-42".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn strings_unescape_and_keep_metacharacters_as_data() {
        // The injection story at the lexical level: `;` and `--` inside a string
        // are just bytes in a `Str` token, never syntax.
        assert_eq!(
            kinds(r#"summary ~ "'; DROP TABLE cards; --""#),
            vec![
                Tok::Word("summary".into()),
                Tok::Match,
                Tok::Str("'; DROP TABLE cards; --".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn an_escaped_quote_stays_inside_the_string() {
        assert_eq!(
            kinds(r#""she said \"hi\"""#),
            vec![Tok::Str("she said \"hi\"".into()), Tok::Eof]
        );
    }

    #[test]
    fn an_unterminated_string_is_an_error_not_a_panic() {
        let err = lex("summary ~ \"nope").unwrap_err();
        assert!(err.message.contains("unterminated"));
        assert_eq!(err.span.unwrap().start, "summary ~ ".len());
    }

    #[test]
    fn a_stray_metacharacter_is_reported_with_its_span() {
        let err = lex("a = b; c = d").unwrap_err();
        assert!(err.message.contains("unexpected character"));
        assert_eq!(err.span.unwrap().start, 5);
    }

    #[test]
    fn an_over_long_query_is_refused_before_tokenising() {
        let huge = "a".repeat(MAX_QUERY_BYTES + 1);
        assert!(lex(&huge).is_err());
    }

    #[test]
    fn multibyte_content_never_splits_a_character() {
        // The fuzzer feeds unicode; slicing on a byte index would panic.
        let toks = lex("summary ~ \"café ☕ 日本語\"").unwrap();
        assert_eq!(toks[2].tok, Tok::Str("café ☕ 日本語".into()));
    }

    #[test]
    fn a_lone_bang_is_an_error() {
        assert!(lex("a ! b").is_err());
    }
}

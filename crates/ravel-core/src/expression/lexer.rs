// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hand-written lexer for the expression language.
//!
//! The token set is fixed by the surface syntax recorded in
//! `docs/implementation/expression-language-plan.md`: C/After-Effects-style
//! operators, decimal number literals, `[A-Za-z_][A-Za-z0-9_]*` identifiers,
//! `.` for both context namespaces (`res.width`) and attribute components
//! (`@P.x`), and `@` introducing an attribute reference.
//!
//! There are no string literals (no built-in takes one) and **no comment
//! syntax in v1**. Comments can be added later without breaking a single
//! stored expression, because relaxing the lexer only ever accepts more.

use smol_str::SmolStr;

use super::error::{ExpressionError, ExpressionErrorKind, Span};

/// A lexical token of the expression language.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TokenKind {
    /// A decimal number literal, already parsed.
    Number(f64),
    /// An identifier. Keyword-ness is decided by the parser.
    Ident(SmolStr),
    /// `@`, introducing an attribute reference.
    At,
    /// `.`, a namespace or component separator.
    Dot,
    /// `,`
    Comma,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `?`
    Question,
    /// `:`
    Colon,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `!`
    Bang,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `==`
    EqualEqual,
    /// `!=`
    BangEqual,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// End of input.
    Eof,
}

impl TokenKind {
    /// A short human-readable description used in parser error messages.
    pub(crate) fn describe(&self) -> SmolStr {
        match self {
            TokenKind::Number(_) => SmolStr::new_static("a number"),
            TokenKind::Ident(name) => SmolStr::from(format!("`{name}`")),
            TokenKind::At => SmolStr::new_static("`@`"),
            TokenKind::Dot => SmolStr::new_static("`.`"),
            TokenKind::Comma => SmolStr::new_static("`,`"),
            TokenKind::LParen => SmolStr::new_static("`(`"),
            TokenKind::RParen => SmolStr::new_static("`)`"),
            TokenKind::Question => SmolStr::new_static("`?`"),
            TokenKind::Colon => SmolStr::new_static("`:`"),
            TokenKind::Plus => SmolStr::new_static("`+`"),
            TokenKind::Minus => SmolStr::new_static("`-`"),
            TokenKind::Star => SmolStr::new_static("`*`"),
            TokenKind::Slash => SmolStr::new_static("`/`"),
            TokenKind::Percent => SmolStr::new_static("`%`"),
            TokenKind::Bang => SmolStr::new_static("`!`"),
            TokenKind::Less => SmolStr::new_static("`<`"),
            TokenKind::LessEqual => SmolStr::new_static("`<=`"),
            TokenKind::Greater => SmolStr::new_static("`>`"),
            TokenKind::GreaterEqual => SmolStr::new_static("`>=`"),
            TokenKind::EqualEqual => SmolStr::new_static("`==`"),
            TokenKind::BangEqual => SmolStr::new_static("`!=`"),
            TokenKind::AmpAmp => SmolStr::new_static("`&&`"),
            TokenKind::PipePipe => SmolStr::new_static("`||`"),
            TokenKind::Eof => SmolStr::new_static("the end of the expression"),
        }
    }
}

/// A token and the source range it covers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) span: Span,
}

/// Tokenize `source`, always ending with a [`TokenKind::Eof`] token.
pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, ExpressionError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0usize;

    while position < bytes.len() {
        let start = position;
        let byte = bytes[position];

        if byte.is_ascii_whitespace() {
            position += 1;
            continue;
        }

        let kind = if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(position + 1).is_some_and(u8::is_ascii_digit))
        {
            let (value, end) = lex_number(source, position)?;
            position = end;
            TokenKind::Number(value)
        } else if byte == b'_' || byte.is_ascii_alphabetic() {
            let end = ident_end(bytes, position);
            let name = SmolStr::new(&source[position..end]);
            position = end;
            TokenKind::Ident(name)
        } else {
            let (kind, width) = match byte {
                b'@' => (TokenKind::At, 1),
                b'.' => (TokenKind::Dot, 1),
                b',' => (TokenKind::Comma, 1),
                b'(' => (TokenKind::LParen, 1),
                b')' => (TokenKind::RParen, 1),
                b'?' => (TokenKind::Question, 1),
                b':' => (TokenKind::Colon, 1),
                b'+' => (TokenKind::Plus, 1),
                b'-' => (TokenKind::Minus, 1),
                b'*' => (TokenKind::Star, 1),
                b'/' => (TokenKind::Slash, 1),
                b'%' => (TokenKind::Percent, 1),
                b'<' if bytes.get(position + 1) == Some(&b'=') => (TokenKind::LessEqual, 2),
                b'<' => (TokenKind::Less, 1),
                b'>' if bytes.get(position + 1) == Some(&b'=') => (TokenKind::GreaterEqual, 2),
                b'>' => (TokenKind::Greater, 1),
                b'=' if bytes.get(position + 1) == Some(&b'=') => (TokenKind::EqualEqual, 2),
                // `=` is one of the symbols the surface syntax deliberately
                // leaves free for a later grammar (see the compatibility rule
                // in docs/specifications/expression-language.md). It gets its
                // own message because writing it is nearly always a typo for
                // `==` or a habit from a language that has statements.
                b'=' => {
                    return Err(ExpressionError::new(
                        ExpressionErrorKind::Assignment,
                        Span::new(position as u32, position as u32 + 1),
                        source,
                    ));
                }
                b'!' if bytes.get(position + 1) == Some(&b'=') => (TokenKind::BangEqual, 2),
                b'!' => (TokenKind::Bang, 1),
                b'&' if bytes.get(position + 1) == Some(&b'&') => (TokenKind::AmpAmp, 2),
                b'|' if bytes.get(position + 1) == Some(&b'|') => (TokenKind::PipePipe, 2),
                _ => {
                    let ch = source[position..].chars().next().unwrap_or('\u{fffd}');
                    let span = Span::new(position as u32, (position + ch.len_utf8()) as u32);
                    return Err(ExpressionError::new(
                        ExpressionErrorKind::UnexpectedCharacter(ch),
                        span,
                        source,
                    ));
                }
            };
            position += width;
            kind
        };

        tokens.push(Token {
            kind,
            span: Span::new(start as u32, position as u32),
        });
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::empty_at(bytes.len() as u32),
    });
    Ok(tokens)
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric()) {
        end += 1;
    }
    end
}

/// Lex a decimal literal: `1`, `1.5`, `.5`, `1e-3`, `2.5E6`.
///
/// No hexadecimal form and no digit separators — both can be added later
/// without invalidating any expression that parses today.
fn lex_number(source: &str, start: usize) -> Result<(f64, usize), ExpressionError> {
    let bytes = source.as_bytes();
    let mut end = start;

    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }

    // `1.5`, `1.` and `.5` are all literals. A `.` only joins the number when
    // digits precede it or follow it, so the separator in `res.width` still
    // lexes as its own token.
    let has_integer_part = end > start;
    if bytes.get(end) == Some(&b'.')
        && (has_integer_part || bytes.get(end + 1).is_some_and(u8::is_ascii_digit))
    {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    }

    // An exponent only belongs to the literal when it is complete: `1e` and
    // `1e+` must report a bad number rather than silently lexing `1` followed
    // by the variable `e`, which would turn a typo into a valid expression.
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exponent = end + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            end = exponent;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        } else {
            let bad_end = ident_end(bytes, end).max(exponent);
            let span = Span::new(start as u32, bad_end as u32);
            return Err(ExpressionError::new(
                ExpressionErrorKind::InvalidNumber(SmolStr::new(span.text(source))),
                span,
                source,
            ));
        }
    }

    let text = &source[start..end];
    match text.parse::<f64>() {
        Ok(value) => Ok((value, end)),
        Err(_) => {
            let span = Span::new(start as u32, end as u32);
            Err(ExpressionError::new(
                ExpressionErrorKind::InvalidNumber(SmolStr::new(text)),
                span,
                source,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .expect("tokenizes")
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn numbers_cover_the_documented_forms() {
        assert_eq!(
            kinds("1 1.5 .5 1. 1e-3 2.5E6"),
            vec![
                TokenKind::Number(1.0),
                TokenKind::Number(1.5),
                TokenKind::Number(0.5),
                TokenKind::Number(1.0),
                TokenKind::Number(1e-3),
                TokenKind::Number(2.5e6),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn an_incomplete_exponent_is_a_bad_number_not_a_variable() {
        let error = tokenize("1e").expect_err("rejected");
        assert_eq!(
            error.kind,
            ExpressionErrorKind::InvalidNumber(SmolStr::new("1e"))
        );
        assert_eq!(error.column, 1);
        assert!(tokenize("1e+").is_err());
    }

    #[test]
    fn operators_prefer_the_two_character_form() {
        assert_eq!(
            kinds("<= >= == != && || < > !"),
            vec![
                TokenKind::LessEqual,
                TokenKind::GreaterEqual,
                TokenKind::EqualEqual,
                TokenKind::BangEqual,
                TokenKind::AmpAmp,
                TokenKind::PipePipe,
                TokenKind::Less,
                TokenKind::Greater,
                TokenKind::Bang,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn the_symbols_held_in_reserve_are_lexical_errors() {
        // Kept free on purpose so a later grammar can claim them without
        // changing what any stored expression means.
        for (source, symbol) in [
            ("1 ; 2", ';'),
            ("{1}", '{'),
            ("[1]", '['),
            ("2 ^ 3", '^'),
            ("~1", '~'),
            ("#1", '#'),
        ] {
            assert_eq!(
                tokenize(source).expect_err("held in reserve").kind,
                ExpressionErrorKind::UnexpectedCharacter(symbol),
                "`{source}`"
            );
        }
    }

    #[test]
    fn a_lone_equals_explains_itself() {
        let error = tokenize("x = 1").expect_err("rejected");
        assert_eq!(error.kind, ExpressionErrorKind::Assignment);
        assert_eq!(error.column, 3);
    }

    #[test]
    fn a_dot_separates_names_but_still_starts_a_number() {
        assert_eq!(
            kinds("res.width"),
            vec![
                TokenKind::Ident(SmolStr::new("res")),
                TokenKind::Dot,
                TokenKind::Ident(SmolStr::new("width")),
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            kinds("1 + .5"),
            vec![
                TokenKind::Number(1.0),
                TokenKind::Plus,
                TokenKind::Number(0.5),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn spans_address_the_source_text() {
        let tokens = tokenize("  frame + 1").expect("tokenizes");
        assert_eq!(tokens[0].span.text("  frame + 1"), "frame");
        assert_eq!(tokens[0].span, Span::new(2, 7));
    }

    #[test]
    fn a_stray_character_reports_its_position() {
        let error = tokenize("1 + #").expect_err("rejected");
        assert_eq!(error.kind, ExpressionErrorKind::UnexpectedCharacter('#'));
        assert_eq!((error.line, error.column), (1, 5));
    }

    #[test]
    fn a_lone_ampersand_is_not_a_logical_operator() {
        assert!(tokenize("1 & 2").is_err());
        assert!(tokenize("1 | 2").is_err());
    }
}

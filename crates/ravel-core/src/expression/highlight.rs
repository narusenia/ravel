// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error-tolerant classification of expression source, for editors.
//!
//! [`tokenize`](super::lexer::tokenize) stops at the first bad character,
//! which is right for compiling and useless for colouring a half-typed
//! expression. [`highlight_tokens`] runs the **same scanning rules**
//! ([`lex_token`]) but treats an error as one [`TokenClass::Error`] run and
//! carries on, so it never stops and never disagrees with the real lexer.
//!
//! The language has no comment syntax, so there is no comment class.

use super::builtin::{Builtin, constant};
use super::error::Span;
use super::lexer::{TokenKind, lex_token};

/// What colour family a run of source belongs to.
///
/// Deliberately coarser than the lexer's token kinds: only distinctions an
/// editor paints differently are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenClass {
    /// Spaces, tabs, newlines.
    Whitespace,
    /// A numeric literal.
    Number,
    /// A built-in function name in call position (`sin(`).
    Function,
    /// A language constant (`pi`, `e`).
    Constant,
    /// Any other name: a context variable or a part of its dotted name.
    Variable,
    /// The `@` and the name after it, including `.x` components (`@P.x`).
    Attribute,
    /// Arithmetic, comparison, logical and `?:` operators.
    Operator,
    /// `(` and `)`.
    Paren,
    /// `,` and the `.` that separates names.
    Punctuation,
    /// A character or number the lexer rejects.
    Error,
}

/// Classify every byte of `source`, in order, with no gaps and no overlaps.
///
/// Spans are byte offsets. Whitespace runs are returned as
/// [`TokenClass::Whitespace`]; an unknown character is one
/// [`TokenClass::Error`] run of that one character.
pub fn highlight_tokens(source: &str) -> Vec<(Span, TokenClass)> {
    let bytes = source.as_bytes();
    let mut runs: Vec<(Span, TokenClass)> = Vec::new();
    let mut prev = Prev::Other;
    let mut position = 0;

    while position < bytes.len() {
        let start = position;
        if bytes[position].is_ascii_whitespace() {
            while position < bytes.len() && bytes[position].is_ascii_whitespace() {
                position += 1;
            }
            runs.push((
                Span::new(start as u32, position as u32),
                TokenClass::Whitespace,
            ));
            continue;
        }
        let class = match lex_token(source, position) {
            Ok((kind, end)) => {
                position = end;
                let (class, next) = match &kind {
                    TokenKind::Number(_) => (TokenClass::Number, Prev::Other),
                    TokenKind::Ident(_) if matches!(prev, Prev::At | Prev::AttrDot) => {
                        (TokenClass::Attribute, Prev::AttrName)
                    }
                    TokenKind::Ident(name) if constant(name).is_some() => {
                        (TokenClass::Constant, Prev::Other)
                    }
                    TokenKind::Ident(name)
                        if Builtin::from_name(name).is_some()
                            && source[end..].trim_start().starts_with('(') =>
                    {
                        (TokenClass::Function, Prev::Other)
                    }
                    TokenKind::Ident(_) => (TokenClass::Variable, Prev::Other),
                    TokenKind::At => (TokenClass::Attribute, Prev::At),
                    // `@P.x`: the dot and the component stay attribute-coloured.
                    TokenKind::Dot if prev == Prev::AttrName => {
                        (TokenClass::Attribute, Prev::AttrDot)
                    }
                    TokenKind::Dot | TokenKind::Comma => (TokenClass::Punctuation, Prev::Other),
                    TokenKind::LParen | TokenKind::RParen => (TokenClass::Paren, Prev::Other),
                    // `lex_token` never yields it; spelled out so a new kind is a decision.
                    TokenKind::Eof => (TokenClass::Error, Prev::Other),
                    _ => (TokenClass::Operator, Prev::Other),
                };
                prev = next;
                class
            }
            Err(error) => {
                // Advance past what the lexer rejected: whole chars, at least one.
                let one = source[position..].chars().next().map_or(1, char::len_utf8);
                position = (error.span.end as usize).max(position + one);
                prev = Prev::Other;
                TokenClass::Error
            }
        };
        runs.push((Span::new(start as u32, position as u32), class));
    }
    runs
}

/// What the previous significant token was, for `@name.component` colouring.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    At,
    AttrName,
    AttrDot,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(source: &str) -> Vec<(&str, TokenClass)> {
        highlight_tokens(source)
            .into_iter()
            .map(|(span, class)| (span.text(source), class))
            .collect()
    }

    /// Every byte belongs to exactly one run, on char boundaries.
    fn assert_covers(source: &str) {
        let mut next = 0u32;
        for (span, _) in highlight_tokens(source) {
            assert_eq!(span.start, next, "gap or overlap in `{source}`");
            assert!(span.end > span.start, "empty run in `{source}`");
            assert!(source.is_char_boundary(span.end as usize), "`{source}`");
            next = span.end;
        }
        assert_eq!(next as usize, source.len(), "`{source}`");
    }

    #[test]
    fn broken_input_is_covered_without_gaps() {
        for source in [
            "",
            "sin(frame * (2 + ",
            "1 + #",
            "1e",
            "1e+",
            "2.5e",
            "a = 1",
            "1 & 2",
            "x |",
            "日本語 + 𝔸 é",
            "@",
            "@P.",
            "))((",
            "1 \u{3000} 2",
            "  \n\t ",
        ] {
            assert_covers(source);
        }
    }

    #[test]
    fn unknown_characters_are_errors_one_char_at_a_time() {
        assert_eq!(
            classes("1 # é"),
            [
                ("1", TokenClass::Number),
                (" ", TokenClass::Whitespace),
                ("#", TokenClass::Error),
                (" ", TokenClass::Whitespace),
                ("é", TokenClass::Error),
            ]
        );
    }

    #[test]
    fn an_unfinished_number_is_one_error_run_and_lexing_resumes() {
        assert_eq!(
            classes("1e + 2"),
            [
                ("1e", TokenClass::Error),
                (" ", TokenClass::Whitespace),
                ("+", TokenClass::Operator),
                (" ", TokenClass::Whitespace),
                ("2", TokenClass::Number),
            ]
        );
    }

    #[test]
    fn names_are_told_apart() {
        use TokenClass::*;
        let got: Vec<_> = classes("sin(pi) + frame + res.width + sin + @P.x")
            .into_iter()
            .filter(|(_, c)| *c != Whitespace)
            .collect();
        assert_eq!(
            got,
            [
                ("sin", Function),
                ("(", Paren),
                ("pi", Constant),
                (")", Paren),
                ("+", Operator),
                ("frame", Variable),
                ("+", Operator),
                ("res", Variable),
                (".", Punctuation),
                ("width", Variable),
                ("+", Operator),
                ("sin", Variable),
                ("+", Operator),
                ("@", Attribute),
                ("P", Attribute),
                (".", Attribute),
                ("x", Attribute),
            ]
        );
    }

    #[test]
    fn an_unclosed_paren_still_classifies_the_rest() {
        let got = classes("max(1, ");
        assert_eq!(got[0], ("max", TokenClass::Function));
        assert_eq!(got[2], ("1", TokenClass::Number));
        assert_eq!(got[3], (",", TokenClass::Punctuation));
    }

    #[test]
    fn valid_sources_have_no_error_run() {
        for source in [
            "sin(frame / 24 * 2 * pi) * 100",
            "a ? b : c <= 1 && !d || e != 2",
        ] {
            assert!(
                !highlight_tokens(source)
                    .iter()
                    .any(|(_, c)| *c == TokenClass::Error),
                "`{source}`"
            );
        }
    }
}

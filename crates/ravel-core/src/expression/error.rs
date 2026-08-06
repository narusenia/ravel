// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Source spans and compile-time errors for the expression language.
//!
//! Every failure the language can report happens while **compiling** a source
//! string into a [`Program`](super::Program). Evaluation is total: it returns
//! a value for every input and has no error channel at all (see the module
//! documentation of [`crate::expression`]). So an [`ExpressionError`] always
//! carries a position in the source it was compiled from — a byte [`Span`]
//! plus the 1-based line and column of its start — because the only consumer
//! that can act on it is an editor showing the author where the text is wrong.

use smol_str::SmolStr;

use super::ast::Component;

/// A half-open byte range `[start, end)` into an expression source string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    /// Byte offset of the first character of the range.
    pub start: u32,
    /// Byte offset one past the last character of the range.
    pub end: u32,
}

impl Span {
    /// A range covering `[start, end)`.
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// An empty range positioned at `offset` (used for "expected more input").
    pub const fn empty_at(offset: u32) -> Self {
        Self::new(offset, offset)
    }

    /// The smallest range covering both `self` and `other`.
    pub fn join(self, other: Self) -> Self {
        Self::new(self.start.min(other.start), self.end.max(other.end))
    }

    /// Length of the range in bytes.
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the range covers no bytes.
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// The text this range covers, or `""` when it does not address `source`.
    pub fn text(self, source: &str) -> &str {
        source
            .get(self.start as usize..self.end as usize)
            .unwrap_or("")
    }

    /// The 1-based line and column of [`Span::start`] within `source`.
    ///
    /// Columns count characters, not bytes, so a caret placed at the returned
    /// column lands under the offending character in a proportional editor.
    /// An offset past the end of `source` resolves to the end position rather
    /// than panicking.
    pub fn line_column(self, source: &str) -> (u32, u32) {
        let offset = (self.start as usize).min(source.len());
        let mut line = 1;
        let mut column = 1;
        for (index, ch) in source.char_indices() {
            if index >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        (line, column)
    }
}

/// What went wrong while compiling an expression.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExpressionErrorKind {
    /// A character that cannot begin any token.
    #[error("unexpected character `{0}`")]
    UnexpectedCharacter(char),
    /// A numeric literal that does not parse (`1e`, `1e+`).
    #[error("`{0}` is not a valid number")]
    InvalidNumber(SmolStr),
    /// A `=` was found. The language has no assignment (REQ-INFRA-007 stage 0).
    #[error("assignment is not part of the expression language (did you mean `==`?)")]
    Assignment,
    /// A second comparison or equality operator at the same level.
    ///
    /// `a < b < c` is a bug in a language whose only type is a number, so the
    /// grammar makes these operators non-associative rather than reading it as
    /// `(a < b) < c` the way C would.
    #[error(
        "`{operator}` cannot be chained: add parentheses, or combine the comparisons with `&&`"
    )]
    NonAssociative {
        /// Spelling of the operator that was chained.
        operator: &'static str,
    },
    /// A token appeared where the grammar allows something else.
    #[error("expected {expected}, found {found}")]
    UnexpectedToken {
        /// What the grammar allows at this position.
        expected: &'static str,
        /// A short description of the token that was found.
        found: SmolStr,
    },
    /// The source ended while the grammar still required input.
    #[error("expected {expected}, found the end of the expression")]
    UnexpectedEnd {
        /// What the grammar allows at this position.
        expected: &'static str,
    },
    /// A `(` was never closed. The span points at the opening parenthesis.
    #[error("unclosed `(`")]
    UnclosedParen,
    /// A `)` closes nothing.
    #[error("unmatched `)`")]
    UnmatchedParen,
    /// The expression parsed, but text follows it.
    #[error("unexpected trailing input after the expression")]
    TrailingInput,
    /// A name that the compiling [`Scope`](super::Scope) does not declare.
    ///
    /// Dotted context names (`res.width`) are matched whole, so the reported
    /// name is the complete path — `unknown variable `comp.widht`` rather than
    /// a complaint about an object named `comp`.
    #[error("unknown variable `{0}`")]
    UnknownVariable(SmolStr),
    /// An `@attribute` reference in a context that has no attributes, such as
    /// a parameter expression.
    #[error("attributes are not available here")]
    AttributesUnavailable,
    /// A `.` suffix on an attribute that is not a component name.
    #[error("`{0}` is not a component (expected one of x, y, z, w, r, g, b, a)")]
    UnknownComponent(SmolStr),
    /// A vector attribute used without selecting a component.
    ///
    /// An expression evaluates to one number, so `@P` on its own has no value.
    /// When vector-valued expressions arrive this error is relaxed rather than
    /// replaced, which keeps every expression that compiles today compiling.
    #[error("`@{attribute}` has {components} components: select one, for example `@{attribute}.x`")]
    MissingComponent {
        /// The attribute name, without the `@`.
        attribute: SmolStr,
        /// How many components the attribute has.
        components: u8,
    },
    /// A component the attribute does not have.
    #[error("`@{attribute}` has {available} component(s), so `.{component}` does not exist")]
    InvalidComponent {
        /// The attribute name, without the `@`.
        attribute: SmolStr,
        /// The component that was selected.
        component: Component,
        /// How many components were available at that point in the path.
        available: u8,
    },
    /// A call to a function that is not built in.
    #[error("unknown function `{0}`")]
    UnknownFunction(SmolStr),
    /// A built-in called with the wrong number of arguments.
    #[error("`{name}` takes {expected}, found {found}")]
    WrongArity {
        /// The built-in's name.
        name: &'static str,
        /// Human-readable description of the accepted argument counts.
        expected: SmolStr,
        /// How many arguments the call passed.
        found: usize,
    },
    /// The expression nests deeper than the compiler accepts.
    #[error("the expression nests too deeply (limit {limit})")]
    TooDeep {
        /// The nesting limit, [`super::MAX_NESTING_DEPTH`].
        limit: usize,
    },
    /// The expression contains more tokens than the compiler accepts.
    ///
    /// The size bound rather than the depth bound: a long left-associative
    /// chain is shallow to read but builds a tree that every later pass
    /// recurses through. See [`super::MAX_TOKENS`].
    #[error("the expression is too large (limit {limit} tokens)")]
    TooManyTokens {
        /// The token limit, [`super::MAX_TOKENS`].
        limit: usize,
    },
    /// The expression needs more evaluation stack than the VM provides.
    #[error("the expression is too complex ({needed} stack slots, limit {limit})")]
    TooComplex {
        /// Stack slots the compiled program would need.
        needed: usize,
        /// The limit, [`super::MAX_STACK_SLOTS`].
        limit: usize,
    },
}

/// A compile error together with where in the source it occurred.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind} (line {line}, column {column})")]
pub struct ExpressionError {
    /// What went wrong.
    pub kind: ExpressionErrorKind,
    /// The byte range in the source the error refers to.
    pub span: Span,
    /// 1-based line of [`ExpressionError::span`]'s start.
    pub line: u32,
    /// 1-based column of [`ExpressionError::span`]'s start.
    pub column: u32,
}

impl ExpressionError {
    /// Resolve `span` against `source` and pair it with `kind`.
    pub(crate) fn new(kind: ExpressionErrorKind, span: Span, source: &str) -> Self {
        let (line, column) = span.line_column(source);
        Self {
            kind,
            span,
            line,
            column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_column_is_one_based_and_counts_newlines() {
        let source = "1 +\n  2 *\n3";
        assert_eq!(Span::empty_at(0).line_column(source), (1, 1));
        assert_eq!(Span::empty_at(2).line_column(source), (1, 3));
        assert_eq!(Span::empty_at(4).line_column(source), (2, 1));
        assert_eq!(Span::empty_at(6).line_column(source), (2, 3));
        assert_eq!(Span::empty_at(10).line_column(source), (3, 1));
    }

    #[test]
    fn column_counts_characters_not_bytes() {
        // A caret has to land under the character, so a multi-byte prefix must
        // not push the column past it.
        let source = "αβ + 1";
        assert_eq!(Span::empty_at(4).line_column(source), (1, 3));
    }

    #[test]
    fn out_of_range_positions_resolve_instead_of_panicking() {
        let source = "1";
        assert_eq!(Span::empty_at(99).line_column(source), (1, 2));
        assert_eq!(Span::new(5, 9).text(source), "");
    }

    #[test]
    fn span_text_and_join_address_the_source() {
        let source = "abc + def";
        assert_eq!(Span::new(0, 3).text(source), "abc");
        assert_eq!(Span::new(0, 3).join(Span::new(6, 9)).text(source), source);
        assert_eq!(Span::new(0, 3).len(), 3);
        assert!(Span::empty_at(3).is_empty());
    }
}

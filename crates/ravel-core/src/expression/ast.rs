// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The parsed shape of an expression, before names are resolved.
//!
//! [`Expr`] is the direct product of [`parse`](super::parse): every node keeps
//! its [`Span`], nothing has been folded, and names are still text. It is the
//! representation an editor wants (EXPR-4 underlines a span) and the one a
//! later lowering pass wants (EXPR-6 gives `@attr` its meaning). The compiled,
//! evaluable form is [`Program`](super::Program), which drops spans and text.

use std::fmt;

use smol_str::SmolStr;

use super::error::Span;

/// A component selected from a vector attribute.
///
/// `x`/`y`/`z`/`w` and `r`/`g`/`b`/`a` are two spellings of the same four
/// components; the parser normalizes both to this enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Component {
    /// First component (`x`, `r`).
    X,
    /// Second component (`y`, `g`).
    Y,
    /// Third component (`z`, `b`).
    Z,
    /// Fourth component (`w`, `a`).
    W,
}

impl Component {
    /// Resolve a component suffix, accepting both spellings.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "x" | "r" => Some(Component::X),
            "y" | "g" => Some(Component::Y),
            "z" | "b" => Some(Component::Z),
            "w" | "a" => Some(Component::W),
            _ => None,
        }
    }

    /// The canonical (`x`/`y`/`z`/`w`) spelling.
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Component::X => "x",
            Component::Y => "y",
            Component::Z => "z",
            Component::W => "w",
        }
    }

    /// Zero-based position of the component within a vector.
    pub const fn index(self) -> usize {
        match self {
            Component::X => 0,
            Component::Y => 1,
            Component::Z => 2,
            Component::W => 3,
        }
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.canonical_name())
    }
}

/// An `@attribute` reference and the components selected from it.
///
/// EXPR-1 only carries the reference: which geometry attribute a name means,
/// what its domain is, and what happens when it is absent are all decided by
/// EXPR-6. The syntax is reserved now so that expressions written against
/// EXPR-1 keep parsing when attributes gain meaning.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttributeRef {
    /// Attribute name as written, without the `@`.
    pub name: SmolStr,
    /// Component suffixes in source order; empty for a scalar reference.
    pub components: Vec<Component>,
}

impl AttributeRef {
    /// A reference with no component suffix.
    pub fn scalar(name: impl Into<SmolStr>) -> Self {
        Self {
            name: name.into(),
            components: Vec::new(),
        }
    }

    /// A reference selecting `components`.
    pub fn with_components(
        name: impl Into<SmolStr>,
        components: impl IntoIterator<Item = Component>,
    ) -> Self {
        Self {
            name: name.into(),
            components: components.into_iter().collect(),
        }
    }
}

impl fmt::Display for AttributeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.name)?;
        for component in &self.components {
            write!(f, ".{component}")?;
        }
        Ok(())
    }
}

/// A prefix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x`
    Negate,
    /// `!x` — `1` when `x == 0`, otherwise `0`.
    Not,
}

/// An infix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `a + b`
    Add,
    /// `a - b`
    Subtract,
    /// `a * b`
    Multiply,
    /// `a / b`
    Divide,
    /// `a % b`
    Remainder,
    /// `a < b`
    Less,
    /// `a <= b`
    LessEqual,
    /// `a > b`
    Greater,
    /// `a >= b`
    GreaterEqual,
    /// `a == b`
    Equal,
    /// `a != b`
    NotEqual,
    /// `a && b`
    And,
    /// `a || b`
    Or,
}

impl BinaryOp {
    /// The operator's source spelling.
    pub const fn spelling(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Subtract => "-",
            BinaryOp::Multiply => "*",
            BinaryOp::Divide => "/",
            BinaryOp::Remainder => "%",
            BinaryOp::Less => "<",
            BinaryOp::LessEqual => "<=",
            BinaryOp::Greater => ">",
            BinaryOp::GreaterEqual => ">=",
            BinaryOp::Equal => "==",
            BinaryOp::NotEqual => "!=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        }
    }
}

/// A parsed expression node with its source range.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    /// The node itself.
    pub kind: ExprKind,
    /// The source range the node covers.
    pub span: Span,
}

impl Expr {
    /// Pair `kind` with `span`.
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The shapes an [`Expr`] can take.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// A number literal.
    Number(f64),
    /// A context name, dotted paths (`res.width`) kept whole.
    ///
    /// `res` is not a value: the language has no objects, so a dotted name is
    /// one opaque string that a [`Scope`](super::Scope) either declares or
    /// does not.
    Variable(SmolStr),
    /// An `@attribute` reference.
    Attribute(AttributeRef),
    /// A prefix operator applied to one operand.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// An infix operator applied to two operands.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// `condition ? if_true : if_false`.
    Conditional {
        /// The tested expression; non-zero selects `if_true`.
        condition: Box<Expr>,
        /// Value when the condition is non-zero.
        if_true: Box<Expr>,
        /// Value when the condition is zero.
        if_false: Box<Expr>,
    },
    /// A call to a built-in function.
    Call {
        /// The function name as written.
        name: SmolStr,
        /// Range of the name alone, for pointing an error at the callee.
        name_span: Span,
        /// The argument expressions.
        arguments: Vec<Expr>,
    },
}

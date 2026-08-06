// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The scalar expression language (REQ-CORE-014, REQ-CORE-015).
//!
//! One source string compiles to a [`Program`] that returns one `f64` per
//! evaluation. It is the language behind parameter expressions and field
//! expressions alike; the normative description of its syntax and semantics is
//! `docs/specifications/expression-language.md`.
//!
//! ```
//! use ravel_core::expression::{Scope, compile};
//!
//! let scope = Scope::new().with_variable("frame");
//! let program = compile("sin(frame / 24 * 2 * pi) * 100", &scope)?;
//!
//! assert_eq!(program.evaluate(&[0.0]), 0.0);
//! assert!(program.dependencies().references_variable("frame"));
//! # Ok::<(), ravel_core::expression::ExpressionError>(())
//! ```
//!
//! # The one property everything rests on: evaluation is total
//!
//! [`Program::evaluate`] takes `&self` and a slice and returns an `f64`. It has
//! no error channel, and it does not need one:
//!
//! * **Names** are resolved when the expression is compiled, against a
//!   [`Scope`] that lists every name the surrounding context provides. An
//!   undefined name is an editing-time error with a source position, not a
//!   surprise at frame 900.
//! * **Arity** is checked at compile time for the same reason.
//! * **Arithmetic** is defined on all of `f64`. `1/0` is `inf`, `sqrt(-1)` is
//!   `NaN`, `log(0)` is `-inf` — the IEEE answers, propagated unchanged.
//!   REQ-CORE-014 puts the decision about non-finite results at the *channel
//!   boundary*, in exactly one place, rather than in each of the twenty
//!   functions that could produce one.
//! * **Recursion and stack use** are bounded at compile time, because a stack
//!   overflow is an abort rather than an error and expression sources arrive
//!   from project files. Three limits do it, and each covers a shape the
//!   others miss: [`MAX_TOKENS`] bounds the total size (and so the depth of
//!   the tree a long left-associative chain builds), [`MAX_NESTING_DEPTH`]
//!   bounds how deeply a short source can nest, and [`MAX_STACK_SLOTS`] bounds
//!   the evaluation stack the compiled form needs.
//!
//! That is what allows `ChannelSource::evaluate` to keep returning a plain
//! value. Making it return a `Result` would turn a hundred call sites into
//! propagators of an error none of them could act on.
//!
//! # What is not here yet
//!
//! [`ChannelSource::Expression`](crate::animation::ChannelSource::Expression)
//! evaluates through this module (EXPR-2), but `ExpressionField` still returns
//! its default until EXPR-5. `@attribute` syntax parses and resolves, but no
//! values are bound to it until EXPR-6 — see [`Program::reads_attributes`].

mod ast;
pub mod builtin;
mod context;
mod error;
mod lexer;
mod noise;
mod parser;
mod program;
mod scope;

#[cfg(test)]
mod tests;

pub use ast::{AttributeRef, BinaryOp, Component, Expr, ExprKind, UnaryOp};
pub use builtin::{Arity, Builtin};
pub use context::{PARAMETER_VALUE_COUNT, parameter_values};
pub use error::{ExpressionError, ExpressionErrorKind, Span};
pub use lexer::MAX_TOKENS;
pub use parser::{MAX_NESTING_DEPTH, parse};
pub use program::{CompileOptions, Dependencies, MAX_STACK_SLOTS, Program};
pub use scope::{
    AttributeDecl, FIELD_VARIABLES, PARAMETER_VARIABLES, STANDARD_ATTRIBUTES, Scope, VarSlot,
};

/// Compile `source` against the names `scope` declares.
///
/// An empty or whitespace-only source is not an error: it compiles to
/// [`Program::empty`], because a parameter box someone has just cleared is not
/// a broken expression.
pub fn compile(source: &str, scope: &Scope) -> Result<Program, ExpressionError> {
    program::compile(source, scope, CompileOptions::default())
}

/// Compile with non-default [`CompileOptions`].
pub fn compile_with(
    source: &str,
    scope: &Scope,
    options: CompileOptions,
) -> Result<Program, ExpressionError> {
    program::compile(source, scope, options)
}

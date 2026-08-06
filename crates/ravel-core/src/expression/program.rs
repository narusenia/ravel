// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resolution, constant folding, and the compiled form that gets evaluated.
//!
//! The pipeline is `source → [`Expr`] → resolved tree → folded tree → [`Op`]
//! sequence`. Only the last of those is kept: a [`Program`] is a flat
//! postfix instruction list plus the sets it depends on.
//!
//! # Why a compiled form exists at all
//!
//! What gets persisted in a `.ravprj` is the source string, never this — the
//! internal representation of the language must not leak into the project
//! format. The compiled form is built when a project loads and when the author
//! edits, so that evaluation never parses. That matters most for field
//! expressions, which run once per element per frame: parsing there would be
//! the whole cost.
//!
//! # Evaluation is total
//!
//! [`Program::evaluate`] returns an `f64` for every input and cannot fail.
//! Names were resolved at compile time, arity was checked at compile time,
//! every operator and built-in is defined on all of `f64`, and the stack the
//! instruction list needs was bounded at compile time. Non-finite results
//! propagate as IEEE says (REQ-CORE-014); dropping them onto a default is the
//! channel boundary's job, in one place, not this one's.

use smol_str::SmolStr;

use super::ast::{AttributeRef, BinaryOp, Expr, ExprKind, UnaryOp};
use super::builtin::{self, Builtin};
use super::error::{ExpressionError, ExpressionErrorKind};
use super::scope::Scope;

/// Evaluation stack slots a compiled program may use.
///
/// Exceeded only by expressions that nest multi-argument calls dozens deep,
/// which reports [`ExpressionErrorKind::TooComplex`] rather than growing the
/// scratch space every evaluation pays for. Together with
/// [`MAX_NESTING_DEPTH`](super::MAX_NESTING_DEPTH) this is what makes
/// evaluation allocation-free and panic-free.
pub const MAX_STACK_SLOTS: usize = 64;

/// What a compiled expression reads.
///
/// Both sets are sorted and free of duplicates, and both list only names the
/// program actually evaluates. Constant folding never removes a name, because
/// a subtree containing one is not constant — so this is equally the set the
/// source mentions.
///
/// Variables and attributes are kept apart because their consumers differ: a
/// variable is a context value the cache key can hash directly, while an
/// attribute is geometry EXPR-6 resolves per element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dependencies {
    variables: Vec<SmolStr>,
    attributes: Vec<SmolStr>,
}

impl Dependencies {
    /// Context variables the expression reads, sorted and deduplicated.
    pub fn variables(&self) -> &[SmolStr] {
        &self.variables
    }

    /// Attribute names the expression reads, sorted and deduplicated.
    pub fn attributes(&self) -> &[SmolStr] {
        &self.attributes
    }

    /// Whether the expression reads the named variable.
    pub fn references_variable(&self, name: &str) -> bool {
        self.variables.iter().any(|entry| entry == name)
    }

    /// Whether the expression reads the named attribute.
    pub fn references_attribute(&self, name: &str) -> bool {
        self.attributes.iter().any(|entry| entry == name)
    }

    /// Whether the expression reads nothing at all.
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty() && self.attributes.is_empty()
    }
}

/// How to compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileOptions {
    /// Collapse constant subexpressions (`2 * pi`) at compile time.
    ///
    /// On by default. Turning it off exists so that a test can compare a
    /// folded program against an unfolded one and assert they agree — the
    /// evidence that folding is an optimization and not a second semantics.
    pub fold_constants: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            fold_constants: true,
        }
    }
}

/// One instruction of the compiled program.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Const(f64),
    Variable(u32),
    Attribute(u32),
    Unary(UnaryOp),
    Binary(BinaryOp),
    Select,
    Call(Builtin, u8),
}

/// The resolved, foldable tree. Spans and names are gone by this point.
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Const(f64),
    Variable(u32),
    Attribute(u32),
    Unary(UnaryOp, Box<Node>),
    Binary(BinaryOp, Box<Node>, Box<Node>),
    Select(Box<Node>, Box<Node>, Box<Node>),
    Call(Builtin, Vec<Node>),
}

/// A compiled expression, ready to evaluate.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    ops: Vec<Op>,
    stack_slots: usize,
    variable_count: usize,
    attribute_refs: Vec<AttributeRef>,
    dependencies: Dependencies,
    constant: Option<f64>,
    empty: bool,
}

impl Program {
    /// The program of a source that holds no expression.
    ///
    /// A blank parameter box is not a broken expression: the surface
    /// specification makes an empty or whitespace-only source mean "no
    /// expression here" so the editor does not turn red while a field is being
    /// cleared. It evaluates to `0.0`; a caller with its own default checks
    /// [`Program::is_empty`] and substitutes.
    pub fn empty() -> Self {
        Self {
            ops: Vec::new(),
            stack_slots: 0,
            variable_count: 0,
            attribute_refs: Vec::new(),
            dependencies: Dependencies::default(),
            constant: None,
            empty: true,
        }
    }

    /// Whether the source held no expression (see [`Program::empty`]).
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// The value this program always returns, when it has one.
    ///
    /// A program that reads nothing folds to a single constant, which lets a
    /// caller skip re-evaluation entirely.
    pub fn as_constant(&self) -> Option<f64> {
        self.constant
    }

    /// What the expression reads.
    pub fn dependencies(&self) -> &Dependencies {
        &self.dependencies
    }

    /// The distinct attribute references, in the order their slots are
    /// numbered. EXPR-6 binds values to these.
    pub fn attribute_refs(&self) -> &[AttributeRef] {
        &self.attribute_refs
    }

    /// Whether the program reads any attribute.
    ///
    /// Attribute values are not bound yet: until EXPR-6 lands, every
    /// `@attribute` evaluates to `0.0`. A caller that permits attributes in
    /// its [`Scope`] must check this rather than silently evaluating zeros.
    pub fn reads_attributes(&self) -> bool {
        !self.attribute_refs.is_empty()
    }

    /// How many variable slots the scope had when this was compiled.
    ///
    /// The slice given to [`Program::evaluate`] should be this long.
    pub fn variable_count(&self) -> usize {
        self.variable_count
    }

    /// Evaluate the program.
    ///
    /// `variables` holds one value per [`Scope`] slot, in declaration order.
    /// A slot beyond the end of the slice reads as `0.0`, so a short slice
    /// gives a wrong answer but never a panic.
    ///
    /// Cannot fail and cannot panic. Non-finite results propagate: `1/0` is
    /// `inf` here, and the channel boundary is what turns that into a default.
    pub fn evaluate(&self, variables: &[f64]) -> f64 {
        if let Some(value) = self.constant {
            return value;
        }
        // Unreachable: `compile` rejects a program needing more than this.
        // Checked anyway so that the array indexing below cannot go out of
        // bounds under any Program that somehow exists.
        if self.stack_slots > MAX_STACK_SLOTS {
            return 0.0;
        }

        let mut stack = [0.0f64; MAX_STACK_SLOTS];
        let mut top = 0usize;

        // The invariant is "a push never writes past the end", which is not
        // the same as "there is always room". A program that uses exactly
        // `MAX_STACK_SLOTS` is legal — `compile` only rejects more than that —
        // and after its last push `top == MAX_STACK_SLOTS` until the next
        // instruction pops. Asserting at the top of the loop would fire on
        // that entirely correct state, so the check belongs at each push,
        // where exceeding the bound would actually mean something.
        macro_rules! push {
            ($value:expr) => {{
                debug_assert!(top < MAX_STACK_SLOTS, "compiled stack bound violated");
                stack[top] = $value;
                top += 1;
            }};
        }

        for op in &self.ops {
            match op {
                Op::Const(value) => push!(*value),
                Op::Variable(slot) => {
                    push!(variables.get(*slot as usize).copied().unwrap_or(0.0))
                }
                // EXPR-6 binds these; see `reads_attributes`.
                Op::Attribute(_) => push!(0.0),
                Op::Unary(op) => {
                    let target = top - 1;
                    stack[target] = apply_unary(*op, stack[target]);
                }
                Op::Binary(op) => {
                    top -= 1;
                    let rhs = stack[top];
                    let target = top - 1;
                    stack[target] = apply_binary(*op, stack[target], rhs);
                }
                Op::Select => {
                    top -= 2;
                    let (if_true, if_false) = (stack[top], stack[top + 1]);
                    let target = top - 1;
                    stack[target] = if truthy(stack[target]) {
                        if_true
                    } else {
                        if_false
                    };
                }
                Op::Call(builtin, count) => {
                    let count = *count as usize;
                    top -= count;
                    let value = builtin.call(&stack[top..top + count]);
                    push!(value);
                }
            }
        }

        if top == 0 { 0.0 } else { stack[top - 1] }
    }
}

/// Compile `source` against `scope`.
pub(super) fn compile(
    source: &str,
    scope: &Scope,
    options: CompileOptions,
) -> Result<Program, ExpressionError> {
    if source.trim().is_empty() {
        return Ok(Program::empty());
    }

    let expression = super::parse(source)?;
    let mut attribute_refs = Vec::new();
    let node = resolve(&expression, scope, source, &mut attribute_refs)?;
    let node = if options.fold_constants {
        fold(node)
    } else {
        node
    };

    let stack_slots = stack_slots(&node);
    if stack_slots > MAX_STACK_SLOTS {
        return Err(ExpressionError::new(
            ExpressionErrorKind::TooComplex {
                needed: stack_slots,
                limit: MAX_STACK_SLOTS,
            },
            expression.span,
            source,
        ));
    }

    let mut ops = Vec::new();
    emit(&node, &mut ops);

    let constant = match ops.as_slice() {
        [Op::Const(value)] => Some(*value),
        _ => None,
    };
    let dependencies = dependencies(&node, scope, &attribute_refs);

    Ok(Program {
        ops,
        stack_slots,
        variable_count: scope.variables().len(),
        attribute_refs,
        dependencies,
        constant,
        empty: false,
    })
}

/// Resolve names and check arity, turning an [`Expr`] into a [`Node`].
fn resolve(
    expression: &Expr,
    scope: &Scope,
    source: &str,
    attribute_refs: &mut Vec<AttributeRef>,
) -> Result<Node, ExpressionError> {
    let error = |kind| ExpressionError::new(kind, expression.span, source);

    match &expression.kind {
        ExprKind::Number(value) => Ok(Node::Const(*value)),

        ExprKind::Variable(name) => {
            // Language constants win over a scope declaration of the same
            // name, which is what lets `2 * pi` fold.
            if let Some(value) = builtin::constant(name) {
                return Ok(Node::Const(value));
            }
            match scope.slot(name) {
                Some(slot) => Ok(Node::Variable(slot.index() as u32)),
                None => Err(error(ExpressionErrorKind::UnknownVariable(name.clone()))),
            }
        }

        ExprKind::Attribute(reference) => {
            if !scope.attributes_allowed() {
                return Err(error(ExpressionErrorKind::AttributesUnavailable));
            }
            check_components(reference, scope, source, expression)?;

            let slot = match attribute_refs.iter().position(|known| known == reference) {
                Some(index) => index,
                None => {
                    attribute_refs.push(reference.clone());
                    attribute_refs.len() - 1
                }
            };
            Ok(Node::Attribute(slot as u32))
        }

        ExprKind::Unary { op, operand } => Ok(Node::Unary(
            *op,
            Box::new(resolve(operand, scope, source, attribute_refs)?),
        )),

        ExprKind::Binary { op, lhs, rhs } => Ok(Node::Binary(
            *op,
            Box::new(resolve(lhs, scope, source, attribute_refs)?),
            Box::new(resolve(rhs, scope, source, attribute_refs)?),
        )),

        ExprKind::Conditional {
            condition,
            if_true,
            if_false,
        } => Ok(Node::Select(
            Box::new(resolve(condition, scope, source, attribute_refs)?),
            Box::new(resolve(if_true, scope, source, attribute_refs)?),
            Box::new(resolve(if_false, scope, source, attribute_refs)?),
        )),

        ExprKind::Call {
            name,
            name_span,
            arguments,
        } => {
            let Some(builtin) = Builtin::from_name(name) else {
                return Err(ExpressionError::new(
                    ExpressionErrorKind::UnknownFunction(name.clone()),
                    *name_span,
                    source,
                ));
            };
            let arity = builtin.arity();
            if !arity.accepts(arguments.len()) {
                return Err(error(ExpressionErrorKind::WrongArity {
                    name: builtin.name(),
                    expected: arity.describe(),
                    found: arguments.len(),
                }));
            }
            let arguments = arguments
                .iter()
                .map(|argument| resolve(argument, scope, source, attribute_refs))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Node::Call(builtin, arguments))
        }
    }
}

/// An expression yields one number, so a vector attribute must name a
/// component and a scalar one must not name a component it does not have.
fn check_components(
    reference: &AttributeRef,
    scope: &Scope,
    source: &str,
    expression: &Expr,
) -> Result<(), ExpressionError> {
    let declared = scope.attribute(&reference.name).map(|decl| decl.components);

    if reference.components.is_empty() {
        return match declared {
            Some(components) if components > 1 => Err(ExpressionError::new(
                ExpressionErrorKind::MissingComponent {
                    attribute: reference.name.clone(),
                    components,
                },
                expression.span,
                source,
            )),
            // A scalar attribute, or one this scope does not know about: an
            // undeclared name may well be a scalar, and only EXPR-6 can tell.
            _ => Ok(()),
        };
    }

    // The first component selects from the attribute; any further one selects
    // from the scalar the first produced.
    let mut available = declared;
    for component in &reference.components {
        if let Some(width) = available
            && component.index() >= width as usize
        {
            return Err(ExpressionError::new(
                ExpressionErrorKind::InvalidComponent {
                    attribute: reference.name.clone(),
                    component: *component,
                    available: width,
                },
                expression.span,
                source,
            ));
        }
        available = Some(1);
    }
    Ok(())
}

/// Collapse subtrees whose value is already known.
///
/// Only a node all of whose children are constant folds. That keeps the
/// transformation obviously value-preserving — it runs the very same
/// [`apply_unary`] / [`apply_binary`] / [`Builtin::call`] the evaluator would
/// have run — and it means folding can never drop a name from the dependency
/// set, because a subtree containing one is never constant.
fn fold(node: Node) -> Node {
    match node {
        Node::Const(_) | Node::Variable(_) | Node::Attribute(_) => node,

        Node::Unary(op, operand) => match fold(*operand) {
            Node::Const(value) => Node::Const(apply_unary(op, value)),
            folded => Node::Unary(op, Box::new(folded)),
        },

        Node::Binary(op, lhs, rhs) => match (fold(*lhs), fold(*rhs)) {
            (Node::Const(a), Node::Const(b)) => Node::Const(apply_binary(op, a, b)),
            (lhs, rhs) => Node::Binary(op, Box::new(lhs), Box::new(rhs)),
        },

        Node::Select(condition, if_true, if_false) => {
            match (fold(*condition), fold(*if_true), fold(*if_false)) {
                (Node::Const(condition), Node::Const(a), Node::Const(b)) => {
                    Node::Const(if truthy(condition) { a } else { b })
                }
                (condition, if_true, if_false) => {
                    Node::Select(Box::new(condition), Box::new(if_true), Box::new(if_false))
                }
            }
        }

        Node::Call(builtin, arguments) => {
            let arguments: Vec<Node> = arguments.into_iter().map(fold).collect();
            let constants: Option<Vec<f64>> = arguments
                .iter()
                .map(|argument| match argument {
                    Node::Const(value) => Some(*value),
                    _ => None,
                })
                .collect();
            match constants {
                Some(values) => Node::Const(builtin.call(&values)),
                None => Node::Call(builtin, arguments),
            }
        }
    }
}

/// Stack slots the postfix form of `node` needs at its deepest point.
fn stack_slots(node: &Node) -> usize {
    match node {
        Node::Const(_) | Node::Variable(_) | Node::Attribute(_) => 1,
        Node::Unary(_, operand) => stack_slots(operand),
        Node::Binary(_, lhs, rhs) => stack_slots(lhs).max(1 + stack_slots(rhs)),
        Node::Select(condition, if_true, if_false) => stack_slots(condition)
            .max(1 + stack_slots(if_true))
            .max(2 + stack_slots(if_false)),
        Node::Call(_, arguments) => arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| index + stack_slots(argument))
            .max()
            .unwrap_or(0)
            .max(1),
    }
}

fn emit(node: &Node, ops: &mut Vec<Op>) {
    match node {
        Node::Const(value) => ops.push(Op::Const(*value)),
        Node::Variable(slot) => ops.push(Op::Variable(*slot)),
        Node::Attribute(slot) => ops.push(Op::Attribute(*slot)),
        Node::Unary(op, operand) => {
            emit(operand, ops);
            ops.push(Op::Unary(*op));
        }
        Node::Binary(op, lhs, rhs) => {
            emit(lhs, ops);
            emit(rhs, ops);
            ops.push(Op::Binary(*op));
        }
        Node::Select(condition, if_true, if_false) => {
            emit(condition, ops);
            emit(if_true, ops);
            emit(if_false, ops);
            ops.push(Op::Select);
        }
        Node::Call(builtin, arguments) => {
            for argument in arguments {
                emit(argument, ops);
            }
            ops.push(Op::Call(*builtin, arguments.len() as u8));
        }
    }
}

fn dependencies(node: &Node, scope: &Scope, attribute_refs: &[AttributeRef]) -> Dependencies {
    let mut variables = Vec::new();
    let mut attributes = Vec::new();
    collect(node, &mut variables, &mut attributes);

    let mut variables: Vec<SmolStr> = variables
        .into_iter()
        .filter_map(|slot| scope.variables().get(slot as usize).cloned())
        .collect();
    variables.sort_unstable();
    variables.dedup();

    let mut attributes: Vec<SmolStr> = attributes
        .into_iter()
        .filter_map(|slot| {
            attribute_refs
                .get(slot as usize)
                .map(|reference| reference.name.clone())
        })
        .collect();
    attributes.sort_unstable();
    attributes.dedup();

    Dependencies {
        variables,
        attributes,
    }
}

fn collect(node: &Node, variables: &mut Vec<u32>, attributes: &mut Vec<u32>) {
    match node {
        Node::Const(_) => {}
        Node::Variable(slot) => variables.push(*slot),
        Node::Attribute(slot) => attributes.push(*slot),
        Node::Unary(_, operand) => collect(operand, variables, attributes),
        Node::Binary(_, lhs, rhs) => {
            collect(lhs, variables, attributes);
            collect(rhs, variables, attributes);
        }
        Node::Select(condition, if_true, if_false) => {
            collect(condition, variables, attributes);
            collect(if_true, variables, attributes);
            collect(if_false, variables, attributes);
        }
        Node::Call(_, arguments) => {
            for argument in arguments {
                collect(argument, variables, attributes);
            }
        }
    }
}

/// Truth in a language with no boolean type: anything but zero.
///
/// `NaN != 0.0` is true, so `NaN` is truthy. That is a consequence of the rule
/// rather than a separate decision, and the alternative — treating `NaN` as
/// false — would make `!x` and `x == 0` disagree.
fn truthy(value: f64) -> bool {
    value != 0.0
}

fn boolean(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn apply_unary(op: UnaryOp, value: f64) -> f64 {
    match op {
        UnaryOp::Negate => -value,
        UnaryOp::Not => boolean(!truthy(value)),
    }
}

/// Apply an infix operator.
///
/// `&&` and `||` evaluate both operands. **Short-circuiting would make no
/// observable difference**: the language is total and side-effect free, so
/// there is no error for a guard to prevent and no state for the second
/// operand to touch. `x != 0 && 1 / x > 2` is therefore not a guard — but it
/// does not need to be one, because `1 / 0` is `inf` rather than a fault.
///
/// Both operators yield `1.0` or `0.0` rather than one of their operands,
/// unlike JavaScript. A language whose only type is a number has no reason to
/// distinguish "the value that made this true" from "true".
fn apply_binary(op: BinaryOp, lhs: f64, rhs: f64) -> f64 {
    match op {
        BinaryOp::Add => lhs + rhs,
        BinaryOp::Subtract => lhs - rhs,
        BinaryOp::Multiply => lhs * rhs,
        BinaryOp::Divide => lhs / rhs,
        // Sign follows the dividend, as in WGSL, C's fmod and Rust's `%`.
        BinaryOp::Remainder => lhs % rhs,
        BinaryOp::Less => boolean(lhs < rhs),
        BinaryOp::LessEqual => boolean(lhs <= rhs),
        BinaryOp::Greater => boolean(lhs > rhs),
        BinaryOp::GreaterEqual => boolean(lhs >= rhs),
        BinaryOp::Equal => boolean(lhs == rhs),
        BinaryOp::NotEqual => boolean(lhs != rhs),
        BinaryOp::And => boolean(truthy(lhs) && truthy(rhs)),
        BinaryOp::Or => boolean(truthy(lhs) || truthy(rhs)),
    }
}

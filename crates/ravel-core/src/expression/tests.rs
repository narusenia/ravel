// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Acceptance tests for the expression language as a whole.
//!
//! The per-stage tests live beside their stage (lexing, parsing, built-in
//! values, noise). What is checked here is the language's contract with its
//! callers: what compiles, where an error points, what a program depends on,
//! and — above all — that evaluation always returns.

use super::{
    CompileOptions, ExpressionErrorKind, MAX_STACK_SLOTS, MAX_TOKENS, Program, Scope, compile,
    compile_with, parse,
};

fn scope() -> Scope {
    Scope::new()
        .with_variables(["x", "y", "frame"])
        .with_variable("res.width")
}

fn program(source: &str) -> Program {
    compile(source, &scope()).unwrap_or_else(|error| panic!("`{source}` should compile: {error}"))
}

/// Evaluate against `x`, `y`, `frame`, `res.width` in slot order.
fn eval(source: &str, variables: [f64; 4]) -> f64 {
    program(source).evaluate(&variables)
}

fn value(source: &str) -> f64 {
    eval(source, [0.0; 4])
}

fn error(source: &str) -> super::ExpressionError {
    compile(source, &scope()).expect_err("should not compile")
}

// ---------------------------------------------------------------------------
// Operator values
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_operators_produce_their_values() {
    assert_eq!(value("1 + 2"), 3.0);
    assert_eq!(value("5 - 8"), -3.0);
    assert_eq!(value("3 * 4"), 12.0);
    assert_eq!(value("7 / 2"), 3.5);
    assert_eq!(value("7 % 3"), 1.0);
    assert_eq!(value("-7 % 3"), -1.0, "the sign follows the dividend");
    assert_eq!(value("7 % -3"), 1.0);
    assert_eq!(value("-(2 + 3)"), -5.0);
    assert_eq!(value("+4"), 4.0);
}

#[test]
fn comparison_operators_produce_one_or_zero() {
    assert_eq!(value("1 < 2"), 1.0);
    assert_eq!(value("2 < 1"), 0.0);
    assert_eq!(value("2 <= 2"), 1.0);
    assert_eq!(value("3 > 2"), 1.0);
    assert_eq!(value("2 >= 3"), 0.0);
    assert_eq!(value("2 == 2"), 1.0);
    assert_eq!(value("2 != 2"), 0.0);
}

#[test]
fn logical_operators_produce_one_or_zero_not_their_operands() {
    // Unlike JavaScript, `2 && 3` is 1, not 3.
    assert_eq!(value("2 && 3"), 1.0);
    assert_eq!(value("0 && 3"), 0.0);
    assert_eq!(value("0 || 5"), 1.0);
    assert_eq!(value("0 || 0"), 0.0);
    assert_eq!(value("!0"), 1.0);
    assert_eq!(value("!7"), 0.0);
    assert_eq!(value("!(1 > 2)"), 1.0);
}

#[test]
fn the_conditional_selects_by_truthiness() {
    assert_eq!(value("1 ? 10 : 20"), 10.0);
    assert_eq!(value("0 ? 10 : 20"), 20.0);
    assert_eq!(eval("x > 3 ? x : -x", [5.0, 0.0, 0.0, 0.0]), 5.0);
    assert_eq!(eval("x > 3 ? x : -x", [1.0, 0.0, 0.0, 0.0]), -1.0);
    // Right associative, so this is a chain of tests rather than nonsense.
    assert_eq!(eval("x < 0 ? -1 : x == 0 ? 0 : 1", [-4.0; 4]), -1.0);
    assert_eq!(eval("x < 0 ? -1 : x == 0 ? 0 : 1", [0.0; 4]), 0.0);
    assert_eq!(eval("x < 0 ? -1 : x == 0 ? 0 : 1", [4.0; 4]), 1.0);
}

#[test]
fn variables_resolve_by_slot_order() {
    assert_eq!(eval("x", [1.0, 2.0, 3.0, 4.0]), 1.0);
    assert_eq!(eval("y", [1.0, 2.0, 3.0, 4.0]), 2.0);
    assert_eq!(eval("frame", [1.0, 2.0, 3.0, 4.0]), 3.0);
    assert_eq!(eval("res.width", [1.0, 2.0, 3.0, 4.0]), 4.0);
    assert_eq!(eval("x * 100 + y", [1.0, 2.0, 3.0, 4.0]), 102.0);
}

#[test]
fn a_realistic_expression_evaluates_as_written() {
    // The shape REQ-CORE-014 gives as its example.
    let source = "50 * sin(frame / 24 * 2 * pi)";
    assert!(eval(source, [0.0, 0.0, 0.0, 0.0]).abs() < 1e-12);
    assert!((eval(source, [0.0, 0.0, 6.0, 0.0]) - 50.0).abs() < 1e-9);
    assert!((eval(source, [0.0, 0.0, 18.0, 0.0]) + 50.0).abs() < 1e-9);
}

#[test]
fn constants_are_available_without_being_declared() {
    assert_eq!(value("pi"), std::f64::consts::PI);
    assert_eq!(value("e"), std::f64::consts::E);
}

// ---------------------------------------------------------------------------
// Totality
// ---------------------------------------------------------------------------

#[test]
fn out_of_domain_input_returns_a_value_instead_of_failing() {
    // The four cases the plan names, plus the ones around them. Each must
    // return; what it returns is IEEE's answer, kept for the channel boundary
    // to deal with (REQ-CORE-014).
    assert!(value("sqrt(-1)").is_nan());
    assert_eq!(value("1 / 0"), f64::INFINITY);
    assert_eq!(value("-1 / 0"), f64::NEG_INFINITY);
    assert!(value("0 / 0").is_nan());
    assert_eq!(value("log(0)"), f64::NEG_INFINITY);
    assert!(value("pow(-1, 0.5)").is_nan());
    assert!(value("1 % 0").is_nan());
    assert!(value("asin(2)").is_nan());
    assert_eq!(value("exp(1e308)"), f64::INFINITY);
}

#[test]
fn a_non_finite_value_flows_through_every_operator() {
    // Nothing downstream of a NaN panics or refuses; the value simply
    // propagates, which is what lets the channel boundary be the only place
    // that has to know about it.
    for source in [
        "sqrt(-1) + 1",
        "sqrt(-1) * 0",
        "sqrt(-1) < 1",
        "sqrt(-1) == sqrt(-1)",
        "sqrt(-1) ? 1 : 2",
        "!sqrt(-1)",
        "clamp(sqrt(-1), 0, 1)",
        "noise(1 / 0)",
        "floor(1 / 0)",
        "min(1 / 0, -1 / 0)",
    ] {
        let value = value(source);
        assert!(
            value.is_nan() || value.is_finite() || value.is_infinite(),
            "`{source}` must still produce a value"
        );
    }
    // NaN is truthy, because truth is `x != 0` and NaN is not equal to zero.
    assert_eq!(value("sqrt(-1) ? 1 : 2"), 1.0);
    assert_eq!(value("!sqrt(-1)"), 0.0);
}

#[test]
fn evaluation_survives_a_variable_slice_that_is_too_short() {
    // A caller that builds the wrong-sized slice gets a wrong number, never a
    // panic: "evaluation cannot fail" has to hold against caller mistakes too.
    let program = program("x + y + frame + res.width");
    assert_eq!(program.evaluate(&[]), 0.0);
    assert_eq!(program.evaluate(&[1.0]), 1.0);
    assert_eq!(program.variable_count(), 4);
}

#[test]
fn evaluation_is_deterministic() {
    // REQ-CORE-014: the cache keys results on inputs, so the same inputs must
    // give the same output every time, including through `noise`.
    let program = program("noise(x * 0.37, y, frame) + sin(x) / (y - frame)");
    let variables = [1.25, -3.5, 7.0, 1920.0];
    let first = program.evaluate(&variables);
    for _ in 0..16 {
        assert_eq!(program.evaluate(&variables), first);
    }
}

// ---------------------------------------------------------------------------
// Compile errors and their positions
// ---------------------------------------------------------------------------

#[test]
fn an_undefined_variable_is_a_compile_error_at_its_position() {
    let error = error("x + speed * 2");
    assert_eq!(
        error.kind,
        ExpressionErrorKind::UnknownVariable("speed".into())
    );
    assert_eq!((error.line, error.column), (1, 5));
    assert_eq!(error.span.text("x + speed * 2"), "speed");
}

#[test]
fn a_misspelled_dotted_name_reports_the_whole_path() {
    // `comp` is not an object, so the error is about the name `comp.widht`
    // rather than about a missing field on something.
    let error = error("comp.widht");
    assert_eq!(
        error.kind,
        ExpressionErrorKind::UnknownVariable("comp.widht".into())
    );
    assert_eq!(error.column, 1);
    assert_eq!(error.span.text("comp.widht"), "comp.widht");
}

#[test]
fn an_undefined_variable_is_found_on_the_line_it_appears_on() {
    let source = "x +\n  y *\n  nope";
    let error = compile(source, &scope()).expect_err("should not compile");
    assert_eq!((error.line, error.column), (3, 3));
}

#[test]
fn an_unknown_function_points_at_the_name() {
    let error = error("1 + wobble(2)");
    assert_eq!(
        error.kind,
        ExpressionErrorKind::UnknownFunction("wobble".into())
    );
    assert_eq!(error.column, 5);
}

#[test]
fn the_wrong_number_of_arguments_says_how_many_it_wanted() {
    let arity = error("clamp(1, 2)");
    assert_eq!(
        arity.kind,
        ExpressionErrorKind::WrongArity {
            name: "clamp",
            expected: "3 arguments".into(),
            found: 2,
        }
    );
    assert_eq!(arity.column, 1);

    assert!(matches!(
        error("noise()").kind,
        ExpressionErrorKind::WrongArity {
            name: "noise",
            found: 0,
            ..
        }
    ));
    assert!(matches!(
        error("noise(1, 2, 3, 4)").kind,
        ExpressionErrorKind::WrongArity { name: "noise", .. }
    ));
    // The whole documented range is accepted.
    for source in ["noise(1)", "noise(1, 2)", "noise(1, 2, 3)"] {
        assert!(compile(source, &scope()).is_ok(), "`{source}`");
    }
}

#[test]
fn syntax_errors_point_at_the_right_place() {
    let cases: [(&str, ExpressionErrorKind, u32); 6] = [
        ("(1 + 2", ExpressionErrorKind::UnclosedParen, 1),
        ("sin(1 + 2", ExpressionErrorKind::UnclosedParen, 4),
        ("1 + 2)", ExpressionErrorKind::UnmatchedParen, 6),
        (
            "1 +",
            ExpressionErrorKind::UnexpectedEnd {
                expected: "a value",
            },
            4,
        ),
        ("1 + $", ExpressionErrorKind::UnexpectedCharacter('$'), 5),
        ("1 + 2 3", ExpressionErrorKind::TrailingInput, 7),
    ];
    for (source, kind, column) in cases {
        let error = compile(source, &scope()).expect_err("should not compile");
        assert_eq!(error.kind, kind, "`{source}`");
        assert_eq!(error.column, column, "`{source}`");
    }
}

#[test]
fn chained_comparisons_are_refused_with_advice() {
    let error = error("0 < x < 10");
    assert_eq!(
        error.kind,
        ExpressionErrorKind::NonAssociative { operator: "<" }
    );
    assert_eq!(error.column, 7);
    assert_eq!(value("(0 < 5) < 10"), 1.0, "parenthesizing is accepted");
    assert_eq!(
        eval("0 < x && x < 10", [5.0; 4]),
        1.0,
        "and so is the reading that was meant"
    );
}

#[test]
fn nothing_in_the_language_can_loop() {
    // No word is reserved (a later grammar may claim them without breaking
    // stored expressions), so these fail as syntax errors or unknown names.
    // The point is that none of them compiles into something that iterates.
    for source in [
        "for (i = 0; i < 10; i = i + 1) x",
        "while (x) x",
        "repeat x until 1",
        "loop x",
        "do x end",
        "fn f(x) x",
        "let a = 1",
        "x = 1",
        "x; y",
        "{ x }",
        "[x]",
        "x -> x",
    ] {
        assert!(
            compile(source, &scope()).is_err(),
            "`{source}` must not compile"
        );
    }
}

#[test]
fn an_expression_that_nests_absurdly_is_refused_rather_than_crashing() {
    // Past the depth limit but inside the token limit, so the depth guard is
    // what answers. A source large enough to trip both is covered separately.
    let deep = format!("{}1{}", "(".repeat(256), ")".repeat(256));
    assert!(matches!(
        compile(&deep, &scope()).expect_err("rejected").kind,
        ExpressionErrorKind::TooDeep { .. }
    ));

    // Nested three-argument calls consume two stack slots per level, so they
    // reach the evaluation-stack bound before the nesting bound. The operands
    // are variables so that folding cannot collapse the whole thing first.
    let mut wide = String::from("x");
    for _ in 0..40 {
        wide = format!("mix(x, y, {wide})");
    }
    assert!(matches!(
        compile(&wide, &scope()).expect_err("rejected").kind,
        ExpressionErrorKind::TooComplex {
            limit: MAX_STACK_SLOTS,
            ..
        }
    ));
}

/// Run `body` on a thread with a deliberately small stack.
///
/// 1 MiB is an eighth of the main thread's 8 MiB and half of what a test
/// thread gets by default, so anything that survives here has real margin
/// where it actually runs. A stack overflow aborts the whole process rather
/// than failing a test, which is exactly the failure mode these limits exist
/// to prevent — so it has to be provoked deliberately, at a size that makes
/// the guarantee mean something.
fn on_a_small_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(body)
        .expect("spawns")
        .join()
        .expect("the compiler must not overflow the stack");
}

#[test]
fn a_long_operator_chain_is_refused_instead_of_overflowing_the_stack() {
    // A left-associative chain has no nesting to speak of, but it builds a
    // tree that leans one node deeper per term — and resolution, folding,
    // emission and the tree's own `Drop` all recurse through it. Before the
    // token bound this aborted the process with SIGABRT at a few thousand
    // terms, which no caller can catch.
    on_a_small_stack(|| {
        let scope = Scope::parameter_context();
        for terms in [MAX_TOKENS, 5_000, 50_000] {
            let source = vec!["frame"; terms].join(" + ");
            assert_eq!(
                compile(&source, &scope).expect_err("too large").kind,
                ExpressionErrorKind::TooManyTokens { limit: MAX_TOKENS },
                "{terms} terms"
            );
            // `parse` is the other public entry point into the same walk.
            assert!(parse(&source).is_err(), "{terms} terms");
        }
    });
}

#[test]
fn every_flat_shape_that_grows_without_nesting_is_bounded() {
    // The chain above is one way to build a huge tree without nesting; these
    // are the others the grammar allows. None may reach the recursive passes.
    on_a_small_stack(|| {
        let scope = Scope::field_context();
        let huge = 20_000;
        let sources = [
            vec!["1"; huge].join(" * "),
            vec!["frame"; huge].join(" - "),
            vec!["1"; huge].join(" < 2 && "),
            // A single call with an enormous argument list.
            format!("min({})", vec!["1"; huge].join(", ")),
            // Alternating operators, so no single precedence level dominates.
            vec!["frame"; huge].join(" + 1 * "),
            // Attributes rather than variables, i.e. the other leaf kind.
            vec!["@P.x"; huge].join(" + "),
        ];
        for source in sources {
            assert!(
                matches!(
                    compile(&source, &scope).expect_err("too large").kind,
                    ExpressionErrorKind::TooManyTokens { .. }
                ),
                "a {}-token source reached the recursive passes",
                source.len()
            );
        }
    });
}

#[test]
fn an_expression_at_the_token_limit_still_compiles_and_evaluates() {
    // The limit has to be usable, not merely safe: the largest accepted
    // expression must survive every recursive pass on a small stack, and
    // produce the right answer.
    on_a_small_stack(|| {
        let scope = Scope::parameter_context();
        let terms = MAX_TOKENS / 2;
        let source = vec!["frame"; terms].join(" + ");
        let program = compile(&source, &scope).expect("the largest accepted expression compiles");

        let mut variables = vec![0.0; program.variable_count()];
        variables[0] = 2.0;
        assert_eq!(program.evaluate(&variables), 2.0 * terms as f64);
    });
}

// ---------------------------------------------------------------------------
// The empty expression
// ---------------------------------------------------------------------------

#[test]
fn an_empty_source_is_not_a_broken_expression() {
    // Clearing a parameter box must not turn it red.
    for source in ["", " ", "\n\t  "] {
        let program = compile(source, &scope()).expect("an empty source compiles");
        assert!(program.is_empty(), "`{source:?}`");
        assert_eq!(program.evaluate(&[0.0; 4]), 0.0);
        assert!(program.dependencies().is_empty());
    }
    assert!(!program("0").is_empty(), "`0` is an expression");
}

// ---------------------------------------------------------------------------
// Dependency extraction
// ---------------------------------------------------------------------------

#[test]
fn dependencies_are_exactly_the_names_the_expression_reads() {
    let program = program("x * 2 + x - frame");
    assert_eq!(
        program.dependencies().variables(),
        ["frame", "x"],
        "sorted, and `x` appears once despite being read twice"
    );
    assert!(!program.dependencies().references_variable("y"));
    assert!(!program.dependencies().references_variable("res.width"));
    assert!(program.dependencies().references_variable("frame"));
}

#[test]
fn an_expression_that_reads_nothing_depends_on_nothing() {
    let program = program("2 * pi + sin(0)");
    assert!(program.dependencies().is_empty());
    // Constants are language-level, so `pi` is not a dependency to invalidate.
    assert!(!program.dependencies().references_variable("pi"));
}

#[test]
fn dependencies_distinguish_variables_from_attributes() {
    let scope = Scope::field_context();
    let program = compile(
        "noise(@P.x * 0.1, time) * (1 - @index / elem.count)",
        &scope,
    )
    .expect("the specification's own example compiles");

    assert_eq!(program.dependencies().variables(), ["elem.count", "time"]);
    assert_eq!(program.dependencies().attributes(), ["P", "index"]);
    assert!(program.dependencies().references_attribute("P"));
    assert!(!program.dependencies().references_attribute("Cd"));
    assert!(program.reads_attributes());
}

#[test]
fn repeated_attribute_components_share_one_slot_per_reference() {
    let program = compile("@P.x + @P.x + @P.y", &Scope::field_context()).expect("compiles");
    // Two distinct references, three reads, one dependency.
    assert_eq!(program.attribute_refs().len(), 2);
    assert_eq!(program.dependencies().attributes(), ["P"]);
}

#[test]
fn frame_dependence_is_visible_to_the_caller() {
    // EXPR-3 splits time-invariant expressions from time-varying ones on
    // exactly this question.
    assert!(
        program("frame * 2")
            .dependencies()
            .references_variable("frame")
    );
    assert!(
        !program("res.width / 2")
            .dependencies()
            .references_variable("frame")
    );
}

// ---------------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------------

#[test]
fn attributes_are_unavailable_to_a_parameter_expression() {
    let error = compile("@P.x", &Scope::parameter_context()).expect_err("rejected");
    assert_eq!(error.kind, ExpressionErrorKind::AttributesUnavailable);
    assert_eq!(error.column, 1);
}

#[test]
fn a_vector_attribute_must_name_a_component() {
    let error = compile("@P", &Scope::field_context()).expect_err("rejected");
    assert_eq!(
        error.kind,
        ExpressionErrorKind::MissingComponent {
            attribute: "P".into(),
            components: 3,
        }
    );
    // A scalar attribute needs none, and an unknown name is assumed scalar
    // because only EXPR-6 can know better.
    assert!(compile("@index", &Scope::field_context()).is_ok());
    assert!(compile("@myattr", &Scope::field_context()).is_ok());
}

#[test]
fn a_component_the_attribute_does_not_have_is_refused() {
    let error = compile("@P.w", &Scope::field_context()).expect_err("rejected");
    assert!(matches!(
        error.kind,
        ExpressionErrorKind::InvalidComponent { available: 3, .. }
    ));
    assert!(
        compile("@Cd.a", &Scope::field_context()).is_ok(),
        "Cd has four"
    );
    assert!(compile("@index.y", &Scope::field_context()).is_err());
}

#[test]
fn attribute_values_are_not_bound_yet() {
    // EXPR-6's job. Until then they read as zero, and `reads_attributes` is
    // how a caller notices rather than shipping silent zeros.
    let program = compile("@P.x + 1", &Scope::field_context()).expect("compiles");
    assert!(program.reads_attributes());
    let variables = vec![0.0; program.variable_count()];
    assert_eq!(program.evaluate(&variables), 1.0);
}

// ---------------------------------------------------------------------------
// Constant folding
// ---------------------------------------------------------------------------

/// Expressions that mix constant subexpressions with variables.
const FOLDING_CASES: &[&str] = &[
    "2 * pi",
    "2 * pi * x",
    "x * 2 * pi",
    "sin(pi / 4) + x",
    "clamp(x, 0 - 1, 1 * 1)",
    "(1 + 2) * (3 + 4) - x",
    "x / (2 * 3)",
    "mix(0, 100, 0.25) + y",
    "1 ? x : y",
    "0 ? x : y",
    "noise(2 * pi) + x",
    "sqrt(-1) + x",
    "1 / 0 + x",
    "frame % (2 * 12)",
    "-(2 + 3) * x",
    "!0 && x",
    "res.width / 2 > 100 ? x : y",
];

#[test]
fn folding_does_not_change_any_value() {
    let unfolded = CompileOptions {
        fold_constants: false,
    };
    let samples = [
        [0.0, 0.0, 0.0, 0.0],
        [1.0, 2.0, 3.0, 1920.0],
        [-4.5, 0.5, 24.0, 640.0],
        [1e12, -1e-12, 90000.0, 1.0],
        [f64::INFINITY, f64::NEG_INFINITY, 0.0, 0.0],
    ];

    for source in FOLDING_CASES {
        let folded = compile(source, &scope()).expect("compiles");
        let plain = compile_with(source, &scope(), unfolded).expect("compiles");

        for variables in samples {
            let a = folded.evaluate(&variables);
            let b = plain.evaluate(&variables);
            assert!(
                a == b || (a.is_nan() && b.is_nan()),
                "`{source}` folded to {a} but evaluates to {b} unfolded"
            );
        }
    }
}

#[test]
fn folding_does_not_change_what_an_expression_depends_on() {
    // Only a subtree with no names in it can fold, so the dependency set is
    // the same either way. EXPR-3 hashes this set into the cache key, and a
    // dependency that appeared or vanished with an optimization would make
    // cache hits depend on the optimizer.
    let unfolded = CompileOptions {
        fold_constants: false,
    };
    for source in FOLDING_CASES {
        let folded = compile(source, &scope()).expect("compiles");
        let plain = compile_with(source, &scope(), unfolded).expect("compiles");
        assert_eq!(folded.dependencies(), plain.dependencies(), "`{source}`");
    }
}

#[test]
fn a_constant_expression_folds_to_a_single_value() {
    assert_eq!(program("2 * pi").as_constant(), Some(std::f64::consts::TAU));
    assert_eq!(program("1 + 2 * 3").as_constant(), Some(7.0));
    assert_eq!(program("mix(0, 100, 0.25)").as_constant(), Some(25.0));
    assert_eq!(program("1 ? 10 : 20").as_constant(), Some(10.0));

    // Anything that reads a name is not constant, however small its share.
    assert_eq!(program("x").as_constant(), None);
    assert_eq!(program("2 * pi * x").as_constant(), None);

    let unfolded = compile_with(
        "2 * pi",
        &scope(),
        CompileOptions {
            fold_constants: false,
        },
    )
    .expect("compiles");
    assert_eq!(unfolded.as_constant(), None, "folding is what finds this");
    assert_eq!(unfolded.evaluate(&[0.0; 4]), std::f64::consts::TAU);
}

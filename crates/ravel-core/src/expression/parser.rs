// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recursive-descent parser for the expression language.
//!
//! The grammar is the one recorded in
//! `docs/implementation/expression-language-plan.md`, loosest binding first:
//!
//! ```text
//! expr           := ternary
//! ternary        := logic_or ( '?' ternary ':' ternary )?             // right assoc
//! logic_or       := logic_and ( '||' logic_and )*
//! logic_and      := equality ( '&&' equality )*
//! equality       := relational ( ('==' | '!=') relational )?          // non-assoc
//! relational     := additive ( ('<' | '<=' | '>' | '>=') additive )?  // non-assoc
//! additive       := multiplicative ( ('+' | '-') multiplicative )*
//! multiplicative := unary ( ('*' | '/' | '%') unary )*
//! unary          := ('-' | '+' | '!') unary | primary
//! primary        := NUMBER | attribute | call | path | '(' expr ')'
//! path           := IDENT ( '.' IDENT )*
//! attribute      := '@' IDENT ( '.' IDENT )*
//! call           := IDENT '(' ( expr ( ',' expr )* )? ')'
//! ```
//!
//! One token of lookahead decides everything: `IDENT` followed by `(` is a
//! call, otherwise it is a path.
//!
//! **There is no statement, no assignment, no loop and no definition form.**
//! That is the point rather than an omission (REQ-INFRA-007 stage 0): a
//! grammar that cannot express iteration terminates by construction, so
//! evaluation needs no instruction budget and no sandbox. Nothing is reserved
//! to enforce it — an identifier only ever matches the declared name set, so
//! `for` fails as an unknown name and a keyword added to a later version of
//! the grammar cannot collide with an expression already saved in a project.

use smol_str::SmolStr;

use super::ast::{AttributeRef, BinaryOp, Component, Expr, ExprKind, UnaryOp};
use super::error::{ExpressionError, ExpressionErrorKind, Span};
use super::lexer::{Token, TokenKind, tokenize};

/// Nesting the parser accepts before reporting [`ExpressionErrorKind::TooDeep`].
///
/// One level is one re-entry into a sub-expression: a parenthesized group, a
/// call argument, a branch of `?:`, or a prefix operator.
///
/// The limit is input validation, not tidiness. A recursive-descent parser
/// meets `((((((…` with its own call stack, and overflowing that is an abort
/// no `Result` can carry — which would falsify the whole claim that this
/// language cannot fail at runtime. Expression sources arrive from `.ravprj`
/// files, so they are untrusted input.
pub const MAX_NESTING_DEPTH: usize = 64;

/// Parse `source` into an [`Expr`] tree.
///
/// This resolves nothing: names stay text and `@attr` is accepted everywhere.
/// [`compile`](super::compile) runs this and then resolves against a
/// [`Scope`](super::Scope).
///
/// An empty source is a parse error here. The "an empty box is not a broken
/// expression" rule of the surface spec lives in [`compile`](super::compile),
/// which never reaches the parser for a blank source.
pub fn parse(source: &str) -> Result<Expr, ExpressionError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser {
        source,
        tokens,
        index: 0,
        depth: 0,
    };

    let expression = parser.parse_expression()?;

    match &parser.peek().kind {
        TokenKind::Eof => Ok(expression),
        TokenKind::RParen => {
            let span = parser.peek().span;
            Err(parser.error(ExpressionErrorKind::UnmatchedParen, span))
        }
        _ => {
            let span = parser.peek().span;
            Err(parser.error(ExpressionErrorKind::TrailingInput, span))
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    index: usize,
    depth: usize,
}

impl Parser<'_> {
    /// The current token.
    ///
    /// `tokenize` always appends `Eof` and the parser never advances past it,
    /// so the last token is a correct answer for any index.
    fn peek(&self) -> &Token {
        let last = self.tokens.len().saturating_sub(1);
        &self.tokens[self.index.min(last)]
    }

    fn peek_ahead(&self, offset: usize) -> &Token {
        let last = self.tokens.len().saturating_sub(1);
        &self.tokens[(self.index + offset).min(last)]
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.index += 1;
        }
        token
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if &self.peek().kind == kind {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn error(&self, kind: ExpressionErrorKind, span: Span) -> ExpressionError {
        ExpressionError::new(kind, span, self.source)
    }

    fn expected(&self, expected: &'static str) -> ExpressionError {
        let token = self.peek();
        let kind = if matches!(token.kind, TokenKind::Eof) {
            ExpressionErrorKind::UnexpectedEnd { expected }
        } else {
            ExpressionErrorKind::UnexpectedToken {
                expected,
                found: token.kind.describe(),
            }
        };
        self.error(kind, token.span)
    }

    /// Every re-entry into a sub-expression goes through here, which is what
    /// makes [`MAX_NESTING_DEPTH`] an actual bound on recursion.
    fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ExpressionError>,
    ) -> Result<T, ExpressionError> {
        if self.depth >= MAX_NESTING_DEPTH {
            let span = self.peek().span;
            return Err(self.error(
                ExpressionErrorKind::TooDeep {
                    limit: MAX_NESTING_DEPTH,
                },
                span,
            ));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn parse_expression(&mut self) -> Result<Expr, ExpressionError> {
        self.nested(Self::parse_ternary)
    }

    fn parse_ternary(&mut self) -> Result<Expr, ExpressionError> {
        let condition = self.parse_logical_or()?;
        if !self.eat(&TokenKind::Question) {
            return Ok(condition);
        }

        let if_true = self.nested(Self::parse_ternary)?;
        if !self.eat(&TokenKind::Colon) {
            return Err(self.expected("`:`"));
        }
        // Right associative: `a ? b : c ? d : e` is `a ? b : (c ? d : e)`.
        let if_false = self.nested(Self::parse_ternary)?;

        let span = condition.span.join(if_false.span);
        Ok(Expr::new(
            ExprKind::Conditional {
                condition: Box::new(condition),
                if_true: Box::new(if_true),
                if_false: Box::new(if_false),
            },
            span,
        ))
    }

    fn parse_logical_or(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_left_associative(
            &[(TokenKind::PipePipe, BinaryOp::Or)],
            Self::parse_logical_and,
        )
    }

    fn parse_logical_and(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_left_associative(&[(TokenKind::AmpAmp, BinaryOp::And)], Self::parse_equality)
    }

    fn parse_equality(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_non_associative(
            &[
                (TokenKind::EqualEqual, BinaryOp::Equal),
                (TokenKind::BangEqual, BinaryOp::NotEqual),
            ],
            Self::parse_relational,
        )
    }

    fn parse_relational(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_non_associative(
            &[
                (TokenKind::LessEqual, BinaryOp::LessEqual),
                (TokenKind::Less, BinaryOp::Less),
                (TokenKind::GreaterEqual, BinaryOp::GreaterEqual),
                (TokenKind::Greater, BinaryOp::Greater),
            ],
            Self::parse_additive,
        )
    }

    fn parse_additive(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_left_associative(
            &[
                (TokenKind::Plus, BinaryOp::Add),
                (TokenKind::Minus, BinaryOp::Subtract),
            ],
            Self::parse_multiplicative,
        )
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ExpressionError> {
        self.parse_left_associative(
            &[
                (TokenKind::Star, BinaryOp::Multiply),
                (TokenKind::Slash, BinaryOp::Divide),
                (TokenKind::Percent, BinaryOp::Remainder),
            ],
            Self::parse_unary,
        )
    }

    fn match_operator(&mut self, operators: &[(TokenKind, BinaryOp)]) -> Option<BinaryOp> {
        for (token, op) in operators {
            if self.eat(token) {
                return Some(*op);
            }
        }
        None
    }

    fn parse_left_associative(
        &mut self,
        operators: &[(TokenKind, BinaryOp)],
        mut operand: impl FnMut(&mut Self) -> Result<Expr, ExpressionError>,
    ) -> Result<Expr, ExpressionError> {
        let mut lhs = operand(self)?;
        while let Some(op) = self.match_operator(operators) {
            let rhs = operand(self)?;
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
        Ok(lhs)
    }

    /// Parse one optional operator from `operators`, and refuse a second.
    ///
    /// `a < b < c` reads as a range test and means `(a < b) < c`, which
    /// compares a truth value against a number. In a language whose only type
    /// is a number that is always a mistake, so it is rejected here rather
    /// than silently accepted the way C would accept it.
    fn parse_non_associative(
        &mut self,
        operators: &[(TokenKind, BinaryOp)],
        mut operand: impl FnMut(&mut Self) -> Result<Expr, ExpressionError>,
    ) -> Result<Expr, ExpressionError> {
        let lhs = operand(self)?;
        let Some(op) = self.match_operator(operators) else {
            return Ok(lhs);
        };
        let rhs = operand(self)?;

        let chained = self.peek().span;
        if operators
            .iter()
            .any(|(token, _)| token == &self.peek().kind)
        {
            return Err(self.error(
                ExpressionErrorKind::NonAssociative {
                    operator: op.spelling(),
                },
                chained,
            ));
        }

        let span = lhs.span.join(rhs.span);
        Ok(Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        ))
    }

    fn parse_unary(&mut self) -> Result<Expr, ExpressionError> {
        let token = self.peek().clone();
        let op = match token.kind {
            TokenKind::Minus => Some(UnaryOp::Negate),
            TokenKind::Bang => Some(UnaryOp::Not),
            // Unary `+` is accepted for symmetry and produces no node.
            TokenKind::Plus => None,
            _ => return self.parse_primary(),
        };

        self.index += 1;
        let operand = self.nested(Self::parse_unary)?;
        match op {
            Some(op) => {
                let span = token.span.join(operand.span);
                Ok(Expr::new(
                    ExprKind::Unary {
                        op,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            None => Ok(operand),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ExpressionError> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Number(value) => {
                self.index += 1;
                Ok(Expr::new(ExprKind::Number(value), token.span))
            }
            TokenKind::LParen => {
                self.index += 1;
                let inner = self.parse_expression()?;
                if !self.eat(&TokenKind::RParen) {
                    return Err(self.error(ExpressionErrorKind::UnclosedParen, token.span));
                }
                Ok(inner)
            }
            TokenKind::At => self.parse_attribute(),
            TokenKind::Ident(_) => self.parse_path_or_call(),
            _ => Err(self.expected("a value")),
        }
    }

    fn parse_attribute(&mut self) -> Result<Expr, ExpressionError> {
        let at = self.advance();
        let TokenKind::Ident(name) = self.peek().kind.clone() else {
            return Err(self.expected("an attribute name"));
        };
        let mut span = at.span.join(self.advance().span);

        let mut components = Vec::new();
        while matches!(self.peek().kind, TokenKind::Dot) {
            let dot = self.advance();
            let TokenKind::Ident(suffix) = self.peek().kind.clone() else {
                return Err(self.expected("a component name"));
            };
            let suffix_token = self.advance();
            let Some(component) = Component::from_name(&suffix) else {
                return Err(self.error(
                    ExpressionErrorKind::UnknownComponent(suffix),
                    suffix_token.span,
                ));
            };
            components.push(component);
            span = span.join(dot.span).join(suffix_token.span);
        }

        Ok(Expr::new(
            ExprKind::Attribute(AttributeRef { name, components }),
            span,
        ))
    }

    fn parse_path_or_call(&mut self) -> Result<Expr, ExpressionError> {
        let first = self.advance();
        let TokenKind::Ident(head) = first.kind.clone() else {
            return Err(self.expected("a name"));
        };

        // A call is a bare name followed by `(`. A dotted path is a context
        // variable, never a method, and a call result never takes a `.`.
        if matches!(self.peek().kind, TokenKind::LParen) {
            return self.parse_call(head, first.span);
        }

        let mut path = String::from(head.as_str());
        let mut span = first.span;
        while matches!(self.peek().kind, TokenKind::Dot)
            && matches!(self.peek_ahead(1).kind, TokenKind::Ident(_))
        {
            self.index += 1;
            let segment = self.advance();
            if let TokenKind::Ident(name) = &segment.kind {
                path.push('.');
                path.push_str(name);
            }
            span = span.join(segment.span);
        }

        if matches!(self.peek().kind, TokenKind::Dot) {
            self.index += 1;
            return Err(self.expected("a name after `.`"));
        }

        Ok(Expr::new(ExprKind::Variable(SmolStr::new(path)), span))
    }

    fn parse_call(&mut self, name: SmolStr, name_span: Span) -> Result<Expr, ExpressionError> {
        let open = self.advance();
        let mut arguments = Vec::new();

        if !matches!(self.peek().kind, TokenKind::RParen) {
            loop {
                arguments.push(self.parse_expression()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }

        if !matches!(self.peek().kind, TokenKind::RParen) {
            return Err(self.error(ExpressionErrorKind::UnclosedParen, open.span));
        }
        let close = self.advance();

        Ok(Expr::new(
            ExprKind::Call {
                name,
                name_span,
                arguments,
            },
            name_span.join(close.span),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(source: &str) -> Expr {
        parse(source).unwrap_or_else(|error| panic!("`{source}` should parse: {error}"))
    }

    /// Render the tree in fully parenthesized prefix form so associativity and
    /// precedence are visible as a single string.
    fn shape(expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Number(value) => format!("{value}"),
            ExprKind::Variable(name) => name.to_string(),
            ExprKind::Attribute(reference) => reference.to_string(),
            ExprKind::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Negate => "-",
                    UnaryOp::Not => "!",
                };
                format!("({op} {})", shape(operand))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                format!("({} {} {})", op.spelling(), shape(lhs), shape(rhs))
            }
            ExprKind::Conditional {
                condition,
                if_true,
                if_false,
            } => format!(
                "(?: {} {} {})",
                shape(condition),
                shape(if_true),
                shape(if_false)
            ),
            ExprKind::Call {
                name, arguments, ..
            } => {
                let arguments: Vec<String> = arguments.iter().map(shape).collect();
                format!("({name} {})", arguments.join(" "))
            }
        }
    }

    #[test]
    fn precedence_matches_the_documented_table() {
        assert_eq!(shape(&parse_ok("1 + 2 * 3")), "(+ 1 (* 2 3))");
        assert_eq!(shape(&parse_ok("(1 + 2) * 3")), "(* (+ 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 + 2 < 3")), "(< (+ 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 < 2 == 0")), "(== (< 1 2) 0)");
        assert_eq!(shape(&parse_ok("1 == 2 && 3")), "(&& (== 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 && 2 || 3")), "(|| (&& 1 2) 3)");
        assert_eq!(shape(&parse_ok("-2 * 3")), "(* (- 2) 3)");
        assert_eq!(shape(&parse_ok("!1 + 2")), "(+ (! 1) 2)");
        assert_eq!(shape(&parse_ok("1 % 2 * 3")), "(* (% 1 2) 3)");
        assert_eq!(shape(&parse_ok("+1 - -2")), "(- 1 (- 2))");
    }

    #[test]
    fn additive_and_multiplicative_operators_are_left_associative() {
        assert_eq!(shape(&parse_ok("1 - 2 - 3")), "(- (- 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 / 2 / 3")), "(/ (/ 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 && 2 && 3")), "(&& (&& 1 2) 3)");
    }

    #[test]
    fn comparisons_and_equality_do_not_chain() {
        for (source, operator) in [
            ("1 < 2 < 3", "<"),
            ("1 <= 2 >= 3", "<="),
            ("1 > 2 > 3", ">"),
            ("1 == 2 == 3", "=="),
            ("1 != 2 == 3", "!="),
        ] {
            let error = parse(source).expect_err("chaining is rejected");
            assert_eq!(
                error.kind,
                ExpressionErrorKind::NonAssociative { operator },
                "`{source}`"
            );
        }
        // The position is the second operator, which is what an editor
        // underlines.
        assert_eq!(parse("1 < 2 < 3").expect_err("rejected").column, 7);
        // Parenthesizing, or `&&`, expresses either intended reading.
        assert_eq!(shape(&parse_ok("(1 < 2) < 3")), "(< (< 1 2) 3)");
        assert_eq!(shape(&parse_ok("1 < 2 && 2 < 3")), "(&& (< 1 2) (< 2 3))");
    }

    #[test]
    fn the_conditional_is_right_associative() {
        assert_eq!(shape(&parse_ok("1 ? 2 : 3 ? 4 : 5")), "(?: 1 2 (?: 3 4 5))");
        assert_eq!(shape(&parse_ok("1 || 2 ? 3 : 4")), "(?: (|| 1 2) 3 4)");
    }

    #[test]
    fn a_dotted_context_name_is_one_opaque_path() {
        assert_eq!(shape(&parse_ok("res.width")), "res.width");
        assert_eq!(shape(&parse_ok("comp.aspect * 2")), "(* comp.aspect 2)");
        assert_eq!(shape(&parse_ok("elem.count")), "elem.count");
        // Whitespace is insignificant, including inside a path.
        assert_eq!(shape(&parse_ok("res . height")), "res.height");
    }

    #[test]
    fn attributes_parse_with_either_component_spelling() {
        assert_eq!(shape(&parse_ok("@P")), "@P");
        assert_eq!(shape(&parse_ok("@P.x")), "@P.x");
        assert_eq!(shape(&parse_ok("@Cd.g")), "@Cd.y");
        assert_eq!(shape(&parse_ok("@P.x + @P.y")), "(+ @P.x @P.y)");
        assert_eq!(shape(&parse_ok("@index")), "@index");
    }

    #[test]
    fn an_unknown_component_names_itself() {
        let error = parse("@P.q").expect_err("rejected");
        assert_eq!(
            error.kind,
            ExpressionErrorKind::UnknownComponent(SmolStr::new("q"))
        );
        assert_eq!(error.column, 4);
    }

    #[test]
    fn calls_take_zero_or_more_arguments_and_no_trailing_comma() {
        assert_eq!(shape(&parse_ok("sin(x)")), "(sin x)");
        assert_eq!(shape(&parse_ok("clamp(x, 0, 1)")), "(clamp x 0 1)");
        // Arity is a resolution concern; the parser accepts any count.
        assert_eq!(shape(&parse_ok("sin()")), "(sin )");
        assert_eq!(shape(&parse_ok("sin(cos(x))")), "(sin (cos x))");
        assert!(parse("min(a, b,)").is_err());
    }

    #[test]
    fn a_call_result_has_no_components() {
        // `.` lives inside a path or an attribute and nowhere else.
        assert!(parse("noise(x).y").is_err());
    }

    #[test]
    fn the_grammar_has_no_iteration_or_binding_form() {
        // No word is reserved, so these fail as syntax or as unknown names —
        // what matters is that no arrangement of them parses into something
        // that could loop.
        for source in [
            "for (i = 0; i < 10; i = i + 1) x",
            "while (1) x",
            "repeat x until 1",
            "loop x",
            "do x end",
            "fn f(x) x",
            "let x = 1",
            "x = 1",
            "1; 2",
            "{ 1 }",
        ] {
            assert!(
                parse(source).is_err(),
                "`{source}` must not parse into an expression"
            );
        }
    }

    #[test]
    fn unbalanced_parentheses_report_the_right_side() {
        let unclosed = parse("(1 + 2").expect_err("rejected");
        assert_eq!(unclosed.kind, ExpressionErrorKind::UnclosedParen);
        assert_eq!(
            unclosed.column, 1,
            "points at the `(` that was never closed"
        );

        let unmatched = parse("1 + 2)").expect_err("rejected");
        assert_eq!(unmatched.kind, ExpressionErrorKind::UnmatchedParen);
        assert_eq!(unmatched.column, 6);
    }

    #[test]
    fn an_incomplete_expression_points_at_where_it_stops() {
        let error = parse("1 +").expect_err("rejected");
        assert_eq!(
            error.kind,
            ExpressionErrorKind::UnexpectedEnd {
                expected: "a value"
            }
        );
        assert_eq!(error.column, 4);

        let error = parse("1 ? 2").expect_err("rejected");
        assert_eq!(
            error.kind,
            ExpressionErrorKind::UnexpectedEnd { expected: "`:`" }
        );
    }

    #[test]
    fn nesting_is_bounded_so_the_parser_cannot_overflow() {
        let deep = format!("{}1{}", "(".repeat(4096), ")".repeat(4096));
        assert_eq!(
            parse(&deep).expect_err("rejected").kind,
            ExpressionErrorKind::TooDeep {
                limit: MAX_NESTING_DEPTH
            }
        );

        let unary = format!("{}1", "-".repeat(4096));
        assert_eq!(
            parse(&unary).expect_err("rejected").kind,
            ExpressionErrorKind::TooDeep {
                limit: MAX_NESTING_DEPTH
            }
        );
    }

    #[test]
    fn an_expression_just_inside_the_nesting_limit_still_parses() {
        let depth = MAX_NESTING_DEPTH - 1;
        let source = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        assert_eq!(shape(&parse_ok(&source)), "1");
    }
}

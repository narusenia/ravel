// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The names an expression editor may offer, assembled from the one place
//! each is defined.
//!
//! Nothing here is a second list: functions come from [`Builtin::ALL`],
//! constants from [`CONSTANTS`], variables and attributes from the [`Scope`].
//! Adding a built-in therefore shows up in completion with no further edit.

use smol_str::SmolStr;

use super::builtin::{Arity, Builtin, CONSTANTS};
use super::scope::{AttributeDecl, Scope};

/// A built-in function as a completion candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionEntry {
    /// The name written in the source.
    pub name: &'static str,
    /// How many arguments it takes; [`Arity::describe`] renders it.
    pub arity: Arity,
}

/// Every name available in one [`Scope`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vocabulary {
    /// All built-in functions, in [`Builtin::ALL`] order.
    pub functions: Vec<FunctionEntry>,
    /// The language constants (`pi`, `e`).
    pub constants: Vec<&'static str>,
    /// The scope's variables, spelled as written: dotted names such as
    /// `res.width` are one entry each, not split at the dot.
    pub variables: Vec<SmolStr>,
    /// The scope's declared `@attribute`s; empty when `@` is not allowed.
    pub attributes: Vec<AttributeDecl>,
}

/// The vocabulary an expression in `scope` may use. Pass
/// [`Scope::parameter_context`] or [`Scope::field_context`].
pub fn vocabulary(scope: &Scope) -> Vocabulary {
    Vocabulary {
        functions: Builtin::ALL
            .iter()
            .map(|builtin| FunctionEntry {
                name: builtin.name(),
                arity: builtin.arity(),
            })
            .collect(),
        constants: CONSTANTS.iter().map(|(name, _)| *name).collect(),
        variables: scope.variables().to_vec(),
        attributes: scope.attributes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_offered() {
        let vocab = vocabulary(&Scope::parameter_context());
        let names: Vec<_> = vocab.functions.iter().map(|f| f.name).collect();
        let all: Vec<_> = Builtin::ALL.iter().map(|b| b.name()).collect();
        assert_eq!(names, all);
        assert_eq!(vocab.constants, ["pi", "e"]);
    }

    #[test]
    fn parameter_scope_has_no_attributes_and_field_scope_does() {
        let parameter = vocabulary(&Scope::parameter_context());
        assert!(parameter.attributes.is_empty());
        assert!(parameter.variables.iter().any(|v| v == "res.width"));
        assert!(!parameter.variables.iter().any(|v| v == "elem.count"));

        let field = vocabulary(&Scope::field_context());
        assert!(field.variables.iter().any(|v| v == "elem.count"));
        assert!(field.attributes.iter().any(|a| a.name == "P"));
    }
}

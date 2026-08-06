// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The name set an expression is compiled against.
//!
//! A [`Scope`] is what makes "undefined variable" a **compile** error rather
//! than a runtime one. The names available to an expression are fixed by where
//! it sits — a parameter expression sees the evaluation context, a field
//! expression additionally sees geometry attributes — and both are known
//! before any frame is evaluated. Resolving against a declared set is
//! therefore an editing-time check, which is exactly what REQ-CORE-014 asks
//! for and what leaves evaluation with nothing left to fail at.
//!
//! Two vocabularies exist, fixed by
//! `docs/specifications/expression-language.md`:
//! [`Scope::parameter_context`] and [`Scope::field_context`]. **Their
//! spellings are persisted**: an expression source saved in a `.ravprj` names
//! them, so renaming one is a data migration.

use smol_str::SmolStr;

/// Position of a variable's value in the slice passed to
/// [`Program::evaluate`](super::Program::evaluate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarSlot(u32);

impl VarSlot {
    /// The index into the value slice.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A geometry attribute a field expression may name, and its width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeDecl {
    /// Attribute name without the `@`.
    pub name: SmolStr,
    /// How many components it has; `1` for a scalar attribute.
    pub components: u8,
}

/// The variables of a parameter expression, in slot order.
///
/// `pi` and `e` are not here: they are constants of the language
/// ([`super::builtin::CONSTANTS`]) so that constant folding can collapse
/// `2 * pi` before evaluation runs.
pub const PARAMETER_VARIABLES: &[&str] = &[
    "frame",
    "time",
    "fps",
    "res.width",
    "res.height",
    "res.aspect",
    "comp.width",
    "comp.height",
    "comp.aspect",
];

/// The variables a field expression adds to [`PARAMETER_VARIABLES`].
pub const FIELD_VARIABLES: &[&str] = &["elem.count"];

/// The standard geometry attributes and their widths.
///
/// Any other name is accepted too: whether an attribute exists cannot be
/// decided while compiling (the geometry is not known yet), so EXPR-6 falls
/// back to a default and warns at sample time. Declaring the standard ones
/// buys the errors that *can* be caught early — `@P` without a component, or
/// `@index.y`.
pub const STANDARD_ATTRIBUTES: &[(&str, u8)] = &[("P", 3), ("N", 3), ("Cd", 4), ("index", 1)];

/// The names an expression may use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    variables: Vec<SmolStr>,
    attributes: Option<Vec<AttributeDecl>>,
}

impl Scope {
    /// An empty scope: no variables, and `@attr` rejected.
    pub fn new() -> Self {
        Self::default()
    }

    /// The parameter-expression vocabulary (REQ-CORE-014).
    ///
    /// Attributes are **not** available: a parameter has no geometry, so `@x`
    /// is a compile error with its own message rather than an unknown name.
    pub fn parameter_context() -> Self {
        Self::new().with_variables(PARAMETER_VARIABLES.iter().copied())
    }

    /// The field-expression vocabulary (REQ-CORE-015).
    ///
    /// Everything a parameter expression has, plus `elem.count` and the
    /// geometry attributes.
    pub fn field_context() -> Self {
        let mut scope = Self::parameter_context()
            .with_variables(FIELD_VARIABLES.iter().copied())
            .with_attributes();
        for (name, components) in STANDARD_ATTRIBUTES {
            scope = scope.with_attribute(*name, *components);
        }
        scope
    }

    /// Declare `name`, returning its slot. Re-declaring returns the first slot.
    pub fn declare_variable(&mut self, name: impl Into<SmolStr>) -> VarSlot {
        let name = name.into();
        match self.slot(&name) {
            Some(slot) => slot,
            None => {
                let slot = VarSlot(self.variables.len() as u32);
                self.variables.push(name);
                slot
            }
        }
    }

    /// Builder form of [`Scope::declare_variable`].
    pub fn with_variable(mut self, name: impl Into<SmolStr>) -> Self {
        self.declare_variable(name);
        self
    }

    /// Declare several variables in order.
    pub fn with_variables<N: Into<SmolStr>, I: IntoIterator<Item = N>>(mut self, names: I) -> Self {
        for name in names {
            self.declare_variable(name);
        }
        self
    }

    /// Permit `@attribute` references without declaring any standard ones.
    pub fn with_attributes(mut self) -> Self {
        self.attributes.get_or_insert_with(Vec::new);
        self
    }

    /// Declare a standard attribute and its width, permitting `@` references.
    pub fn with_attribute(mut self, name: impl Into<SmolStr>, components: u8) -> Self {
        let name = name.into();
        let declared = self.attributes.get_or_insert_with(Vec::new);
        match declared.iter_mut().find(|entry| entry.name == name) {
            Some(entry) => entry.components = components,
            None => declared.push(AttributeDecl { name, components }),
        }
        self
    }

    /// The slot `name` resolves to, if it is declared.
    pub fn slot(&self, name: &str) -> Option<VarSlot> {
        self.variables
            .iter()
            .position(|declared| declared == name)
            .map(|index| VarSlot(index as u32))
    }

    /// The declared variables, in slot order.
    pub fn variables(&self) -> &[SmolStr] {
        &self.variables
    }

    /// Whether `@attribute` references are allowed at all.
    pub fn attributes_allowed(&self) -> bool {
        self.attributes.is_some()
    }

    /// The declared attributes; empty when none are declared or allowed.
    pub fn attributes(&self) -> &[AttributeDecl] {
        self.attributes.as_deref().unwrap_or(&[])
    }

    /// The declaration for `name`, if it is one of the standard attributes.
    pub fn attribute(&self, name: &str) -> Option<&AttributeDecl> {
        self.attributes()
            .iter()
            .find(|declared| declared.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_assigned_in_declaration_order_and_are_stable() {
        let mut scope = Scope::new();
        let first = scope.declare_variable("frame");
        let second = scope.declare_variable("time");
        assert_eq!(first.index(), 0);
        assert_eq!(second.index(), 1);
        // Re-declaring must not shift anything: slot indices are the contract
        // between a compiled program and the value slice its caller builds.
        assert_eq!(scope.declare_variable("frame"), first);
        assert_eq!(scope.variables().len(), 2);
    }

    #[test]
    fn the_parameter_vocabulary_is_the_specified_one() {
        let scope = Scope::parameter_context();
        assert_eq!(scope.variables(), PARAMETER_VARIABLES);
        assert!(!scope.attributes_allowed(), "a parameter has no geometry");
        assert_eq!(scope.slot("res.aspect").map(VarSlot::index), Some(5));
        assert_eq!(scope.slot("elem.count"), None);
        assert_eq!(scope.slot("pi"), None, "pi is a language constant");
    }

    #[test]
    fn the_field_vocabulary_extends_the_parameter_one() {
        let scope = Scope::field_context();
        // The parameter slots keep their indices, so the two vocabularies
        // share a prefix and a caller can fill the common part once.
        for (index, name) in PARAMETER_VARIABLES.iter().enumerate() {
            assert_eq!(scope.slot(name).map(VarSlot::index), Some(index));
        }
        assert!(scope.slot("elem.count").is_some());
        assert!(scope.attributes_allowed());
        assert_eq!(scope.attribute("P").map(|decl| decl.components), Some(3));
        assert_eq!(
            scope.attribute("index").map(|decl| decl.components),
            Some(1)
        );
        assert_eq!(scope.attribute("myattr"), None, "declared ones only");
    }

    #[test]
    fn attributes_can_be_allowed_without_declaring_any() {
        let scope = Scope::new().with_attributes();
        assert!(scope.attributes_allowed());
        assert!(scope.attributes().is_empty());
    }
}

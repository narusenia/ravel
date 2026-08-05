// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Exposed parameter declarations — a project's external parameter contract
//! (REQ-PROJ-006).
//!
//! A declaration names a value the outside world may set (a CLI render's
//! `--param name=value`, a subgraph template's public input, a network
//! interface port) and hides where that value lands inside the document. It
//! carries a **name**, a **type**, a **default**, a **description** and a
//! **binding** to one internal parameter.
//!
//! # Why the contract is a name, not a path
//!
//! The declaration is addressed by name and binds to a
//! [`NodeId`] plus a parameter key ([`ExposedBinding`]). Node ids are
//! document-globally unique and survive renaming, reparenting and rewiring, so
//! neither a layer name nor a node path ever becomes part of the external
//! contract — renaming a layer cannot break a caller's `--param` invocation.
//! Resolving and applying a binding is a separate concern (EXPO-2); this
//! module only declares.
//!
//! # Why the value space is smaller than [`ParameterValue`]
//!
//! [`ExposedValue`] is deliberately *not*
//! [`ParameterValue`](crate::graph::ParameterValue). Only values that can be
//! read as constants belong in an external contract: numbers, vectors, colours,
//! booleans, strings, and a media reference. Whole animation channels
//! (keyframes, expressions), `PathPoints` and `Curve` are excluded, because a
//! contract shaped like the internal representation would force every internal
//! change to break the CLI's argument syntax. The media case has no
//! `ParameterValue` counterpart at all — a media node holds an `asset_id` key
//! into [`Document::media_assets`](crate::composition::Document::media_assets)
//! — so the declaration carries an [`AssetPath`] and leaves the mapping onto
//! the asset table to EXPO-4.
//!
//! # Invariants
//!
//! Two invariants hold for every collection of declarations:
//!
//! 1. **names are unique** — the name *is* the contract, so two declarations
//!    of one name make the contract ambiguous;
//! 2. **the default matches the declared type** — a declaration whose default
//!    contradicts its own type cannot produce a usable value.
//!
//! Both are enforced by the constructors *and* by deserialization
//! ([`ExposedParameters`]'s hand-written [`Deserialize`]), because a `.ravprj`
//! is text: it gets hand-edited, merged and truncated, and a derived
//! `Deserialize` would walk straight past the constructors. Following the
//! stance the rest of `.ravprj` loading takes (and
//! [`CurveParam`](crate::param_curve::CurveParam) in particular), a damaged
//! file **loses the damaged declarations, not the project**.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::composition::AssetPath;
use crate::id::NodeId;
use crate::types::{Color, Vec2, Vec3, Vec4};

/// Why a declaration or a set of declarations was rejected.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ExposedParameterError {
    #[error("an exposed parameter declaration must have a name")]
    EmptyName,

    #[error("exposed parameter {name:?} declares type {declared} but its default value is {found}")]
    DefaultTypeMismatch {
        name: String,
        declared: ExposedType,
        found: ExposedType,
    },

    #[error("exposed parameter {0:?} is already declared")]
    DuplicateName(String),
}

/// The type of an exposed parameter: the set of values a caller may supply.
///
/// Every variant is a constant kind. There is deliberately no variant for an
/// animation channel, a path or a curve (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExposedType {
    Float,
    Int,
    Bool,
    String,
    Vec2,
    Vec3,
    Vec4,
    /// RGBA. Distinct from [`ExposedType::Vec4`] so a caller can be offered a
    /// colour syntax (and a colour picker) rather than four bare numbers,
    /// even though both bind to a 4-component parameter.
    Color,
    /// A media reference, given as an [`AssetPath`] (absolute, project
    /// relative, or `${VAR}`-prefixed). Binds to a media node's `asset_id`
    /// parameter; the mapping onto the document's asset table is EXPO-4.
    Media,
}

impl std::fmt::Display for ExposedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Float => "float",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::String => "string",
            Self::Vec2 => "vec2",
            Self::Vec3 => "vec3",
            Self::Vec4 => "vec4",
            Self::Color => "color",
            Self::Media => "media",
        };
        f.write_str(name)
    }
}

/// A constant value of an exposed parameter — its default, and later the
/// value a caller supplies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExposedValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    Color(Color),
    Media(AssetPath),
}

impl ExposedValue {
    /// The type this value belongs to.
    pub fn exposed_type(&self) -> ExposedType {
        match self {
            Self::Float(_) => ExposedType::Float,
            Self::Int(_) => ExposedType::Int,
            Self::Bool(_) => ExposedType::Bool,
            Self::String(_) => ExposedType::String,
            Self::Vec2(_) => ExposedType::Vec2,
            Self::Vec3(_) => ExposedType::Vec3,
            Self::Vec4(_) => ExposedType::Vec4,
            Self::Color(_) => ExposedType::Color,
            Self::Media(_) => ExposedType::Media,
        }
    }
}

/// Where a declaration lands inside the document: the parameter keyed `key` on
/// node `node`.
///
/// [`NodeId`] rather than a path or a name: node ids are document-globally
/// unique (REQ-LAYER-009) and are not rewritten by renaming or rewiring, so a
/// binding outlives edits to the network around it. The key is the node's own
/// parameter key — for a media declaration, the media node's `asset_id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExposedBinding {
    pub node: NodeId,
    pub key: String,
}

impl ExposedBinding {
    pub fn new(node: NodeId, key: impl Into<String>) -> Self {
        Self {
            node,
            key: key.into(),
        }
    }
}

/// One exposed parameter declaration.
///
/// Fields are private because the type/default agreement is an invariant:
/// every way in goes through [`ExposedParameter::new`] or the checked
/// deserializer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExposedParameter {
    name: String,
    value_type: ExposedType,
    default: ExposedValue,
    description: String,
    binding: ExposedBinding,
}

/// The persisted shape of an [`ExposedParameter`], before its invariant is
/// checked.
///
/// The name must match [`ExposedParameter`]: the document is written with
/// RON's `struct_names` enabled, so the serialized form carries it.
#[derive(Deserialize)]
#[serde(rename = "ExposedParameter")]
struct StoredParameter {
    name: String,
    value_type: ExposedType,
    default: ExposedValue,
    /// Absent in a hand-written declaration that documents nothing.
    #[serde(default)]
    description: String,
    binding: ExposedBinding,
}

impl StoredParameter {
    fn check(self) -> Result<ExposedParameter, ExposedParameterError> {
        ExposedParameter::new(self.name, self.value_type, self.default, self.binding)
            .map(|declaration| declaration.with_description(self.description))
    }
}

/// A single declaration is read strictly: it either satisfies the invariant or
/// it is not an [`ExposedParameter`].
///
/// Leniency belongs one level up, where a set of declarations can afford to
/// drop the damaged ones and keep the rest ([`ExposedParameters`]).
impl<'de> Deserialize<'de> for ExposedParameter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        StoredParameter::deserialize(deserializer)?
            .check()
            .map_err(serde::de::Error::custom)
    }
}

impl ExposedParameter {
    /// Declare `name` as `value_type`, defaulting to `default`, bound to
    /// `binding`.
    ///
    /// Fails when the name is blank, or when `default` is not a value of
    /// `value_type`.
    pub fn new(
        name: impl Into<String>,
        value_type: ExposedType,
        default: ExposedValue,
        binding: ExposedBinding,
    ) -> Result<Self, ExposedParameterError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ExposedParameterError::EmptyName);
        }
        let found = default.exposed_type();
        if found != value_type {
            return Err(ExposedParameterError::DefaultTypeMismatch {
                name,
                declared: value_type,
                found,
            });
        }
        Ok(Self {
            name,
            value_type,
            default,
            description: String::new(),
            binding,
        })
    }

    /// Declare `name` with the type its `default` already carries.
    pub fn inferred(
        name: impl Into<String>,
        default: ExposedValue,
        binding: ExposedBinding,
    ) -> Result<Self, ExposedParameterError> {
        let value_type = default.exposed_type();
        Self::new(name, value_type, default, binding)
    }

    /// Builder: attach the human-readable description shown to callers.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The contract name. Unique within a [`ExposedParameters`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared type. Always [`ExposedParameter::default_value`]'s type.
    pub fn value_type(&self) -> ExposedType {
        self.value_type
    }

    /// The value used when a caller supplies nothing.
    pub fn default_value(&self) -> &ExposedValue {
        &self.default
    }

    /// The description shown to callers; empty when undocumented.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The internal parameter this declaration drives.
    pub fn binding(&self) -> &ExposedBinding {
        &self.binding
    }
}

/// A project's declarations, in the order they are presented to callers.
///
/// Order is data, not a derived view: it is the order a declaration was added
/// in and the order a listing shows. Unlike
/// [`Document::media_assets`](crate::composition::Document::media_assets) —
/// an `im::HashMap` that has to be sorted on the way out to keep the file
/// diff-friendly — a `Vec` is already deterministic, so the persisted form is
/// the sequence itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ExposedParameters {
    entries: Vec<ExposedParameter>,
}

/// Deserialization normalizes instead of trusting the input.
///
/// A `.ravprj` is a text file: it can be hand-edited, merged, or truncated,
/// and a derived `Deserialize` would hand [`ExposedParameters`] a list that
/// repeats a name or holds a declaration whose default contradicts its own
/// type — an ambiguous contract, silently resolved differently by whichever
/// lookup runs first. Reading through the same checks the constructors use
/// makes every set of declarations in the process valid by construction:
///
/// * a declaration with a blank name, or a default that is not a value of its
///   declared type, is **dropped**;
/// * a declaration repeating an earlier name is **dropped**, so the first
///   declaration of a name — the one the file's order presents to a caller —
///   is the one that survives.
///
/// A file whose declarations are damaged therefore opens without them rather
/// than failing the load. Each drop is logged: losing part of an external
/// contract must not be silent.
impl<'de> Deserialize<'de> for ExposedParameters {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = Vec::<StoredParameter>::deserialize(deserializer)?;
        let mut set = Self::new();
        for entry in stored {
            let name = entry.name.clone();
            match entry.check() {
                Ok(declaration) => {
                    if let Err(err) = set.insert(declaration) {
                        tracing::warn!(%err, "dropping an exposed parameter declaration");
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        name = %name,
                        "dropping an unreadable exposed parameter declaration"
                    );
                }
            }
        }
        Ok(set)
    }
}

impl ExposedParameters {
    /// No declarations — what every project written before `.ravprj` v7 has.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Collect `declarations`, rejecting the first duplicate name.
    pub fn from_declarations(
        declarations: impl IntoIterator<Item = ExposedParameter>,
    ) -> Result<Self, ExposedParameterError> {
        let mut set = Self::new();
        for declaration in declarations {
            set.insert(declaration)?;
        }
        Ok(set)
    }

    /// Append `declaration`, keeping the presentation order.
    ///
    /// Fails with [`ExposedParameterError::DuplicateName`] when the name is
    /// already declared; the set is left unchanged.
    pub fn insert(&mut self, declaration: ExposedParameter) -> Result<(), ExposedParameterError> {
        if self.contains(declaration.name()) {
            return Err(ExposedParameterError::DuplicateName(
                declaration.name().to_string(),
            ));
        }
        self.entries.push(declaration);
        Ok(())
    }

    /// The declaration named `name`, if any. Names match exactly.
    pub fn get(&self, name: &str) -> Option<&ExposedParameter> {
        self.entries
            .iter()
            .find(|declaration| declaration.name() == name)
    }

    /// Whether `name` is declared.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The declarations, in presentation order.
    pub fn iter(&self) -> std::slice::Iter<'_, ExposedParameter> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'a> IntoIterator for &'a ExposedParameters {
    type Item = &'a ExposedParameter;
    type IntoIter = std::slice::Iter<'a, ExposedParameter>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> ExposedBinding {
        ExposedBinding::new(NodeId::new(42), "text")
    }

    fn headline() -> ExposedParameter {
        ExposedParameter::new(
            "headline",
            ExposedType::String,
            ExposedValue::String("Ravel".into()),
            binding(),
        )
        .expect("the default is a string")
        .with_description("The title card's text")
    }

    #[test]
    fn a_declaration_keeps_what_it_was_given() {
        let declaration = headline();
        assert_eq!(declaration.name(), "headline");
        assert_eq!(declaration.value_type(), ExposedType::String);
        assert_eq!(
            declaration.default_value(),
            &ExposedValue::String("Ravel".into())
        );
        assert_eq!(declaration.description(), "The title card's text");
        assert_eq!(declaration.binding(), &binding());
    }

    #[test]
    fn every_value_reports_its_own_type() {
        let cases = [
            (ExposedValue::Float(1.0), ExposedType::Float),
            (ExposedValue::Int(1), ExposedType::Int),
            (ExposedValue::Bool(true), ExposedType::Bool),
            (ExposedValue::String("x".into()), ExposedType::String),
            (ExposedValue::Vec2(Vec2(1.0, 2.0)), ExposedType::Vec2),
            (ExposedValue::Vec3(Vec3(1.0, 2.0, 3.0)), ExposedType::Vec3),
            (
                ExposedValue::Vec4(Vec4(1.0, 2.0, 3.0, 4.0)),
                ExposedType::Vec4,
            ),
            (ExposedValue::Color(Color::BLACK), ExposedType::Color),
            (
                ExposedValue::Media(AssetPath::Relative("./a.mov".into())),
                ExposedType::Media,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(value.exposed_type(), expected);
            // Every type is declarable with its own value as the default.
            assert!(ExposedParameter::inferred("p", value, binding()).is_ok());
        }
    }

    #[test]
    fn a_default_of_the_wrong_type_is_rejected() {
        let err = ExposedParameter::new(
            "headline",
            ExposedType::String,
            ExposedValue::Float(1.0),
            binding(),
        )
        .expect_err("a float is not a string");
        assert_eq!(
            err,
            ExposedParameterError::DefaultTypeMismatch {
                name: "headline".into(),
                declared: ExposedType::String,
                found: ExposedType::Float,
            }
        );
    }

    /// `Color` and `Vec4` are both 4-component values but distinct contracts:
    /// neither one's value satisfies the other's declaration.
    #[test]
    fn color_and_vec4_are_not_interchangeable() {
        assert!(
            ExposedParameter::new(
                "tint",
                ExposedType::Color,
                ExposedValue::Vec4(Vec4(0.0, 0.0, 0.0, 1.0)),
                binding()
            )
            .is_err()
        );
        assert!(
            ExposedParameter::new(
                "offset",
                ExposedType::Vec4,
                ExposedValue::Color(Color::BLACK),
                binding()
            )
            .is_err()
        );
    }

    #[test]
    fn a_blank_name_is_rejected() {
        for name in ["", "   "] {
            assert_eq!(
                ExposedParameter::inferred(name, ExposedValue::Bool(true), binding()),
                Err(ExposedParameterError::EmptyName)
            );
        }
    }

    #[test]
    fn a_duplicate_name_is_rejected() {
        let mut set = ExposedParameters::new();
        set.insert(headline()).unwrap();
        assert_eq!(
            set.insert(
                ExposedParameter::inferred("headline", ExposedValue::Int(1), binding()).unwrap()
            ),
            Err(ExposedParameterError::DuplicateName("headline".into()))
        );
        // The rejected insert left the set alone.
        assert_eq!(set.len(), 1);
        assert_eq!(set.get("headline"), Some(&headline()));
    }

    /// Names are the contract, matched exactly: a different case is a
    /// different declaration.
    #[test]
    fn names_are_case_sensitive() {
        let set = ExposedParameters::from_declarations([
            headline(),
            ExposedParameter::inferred("Headline", ExposedValue::Int(1), binding()).unwrap(),
        ])
        .expect("the names differ");
        assert_eq!(set.len(), 2);
        assert!(!set.contains("HEADLINE"));
    }

    #[test]
    fn declarations_keep_their_order_and_roundtrip() {
        let set = ExposedParameters::from_declarations([
            headline(),
            ExposedParameter::inferred("scale", ExposedValue::Float(2.0), binding()).unwrap(),
            ExposedParameter::inferred("loop", ExposedValue::Bool(false), binding()).unwrap(),
        ])
        .unwrap();

        let text = ron::to_string(&set).unwrap();
        let back: ExposedParameters = ron::from_str(&text).unwrap();
        assert_eq!(back, set);
        assert_eq!(
            back.iter().map(ExposedParameter::name).collect::<Vec<_>>(),
            ["headline", "scale", "loop"],
            "the persisted order is the presentation order"
        );
        // Serialization is a pure function of the value: same content, same
        // bytes, however many times it is written.
        assert_eq!(ron::to_string(&back).unwrap(), text);
    }

    /// An empty set persists as an empty sequence and reads back empty.
    #[test]
    fn an_empty_set_roundtrips() {
        let text = ron::to_string(&ExposedParameters::new()).unwrap();
        assert_eq!(text, "[]");
        let back: ExposedParameters = ron::from_str(&text).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn a_hand_written_declaration_without_a_description_reads() {
        let declaration: ExposedParameter = ron::from_str(
            r#"(name: "headline", value_type: String, default: String("Ravel"), binding: (node: NodeId(42), key: "text"))"#,
        )
        .expect("description defaults to empty");
        assert_eq!(declaration.description(), "");
        assert_eq!(declaration.name(), "headline");
    }

    /// A hand-edited file can contradict itself. Reading one declaration is
    /// strict — it is not an `ExposedParameter` at all.
    #[test]
    fn reading_a_single_contradictory_declaration_fails() {
        let err = ron::from_str::<ExposedParameter>(
            r#"(name: "headline", value_type: String, default: Float(1.0), description: "", binding: (node: NodeId(42), key: "text"))"#,
        )
        .expect_err("the default contradicts the declared type");
        assert!(
            err.to_string().contains("declares type string"),
            "the error names the mismatch: {err}"
        );
    }

    /// A set drops what it cannot read and keeps the rest: losing the whole
    /// project over one hand-edited declaration would be the worse failure.
    #[test]
    fn a_contradictory_declaration_is_dropped_from_a_set() {
        let set: ExposedParameters = ron::from_str(
            r#"[
                (name: "headline", value_type: String, default: String("Ravel"), description: "", binding: (node: NodeId(42), key: "text")),
                (name: "broken", value_type: String, default: Float(1.0), description: "", binding: (node: NodeId(42), key: "text")),
                (name: "", value_type: Bool, default: Bool(true), description: "", binding: (node: NodeId(42), key: "on")),
                (name: "scale", value_type: Float, default: Float(2.0), description: "", binding: (node: NodeId(7), key: "scale")),
            ]"#,
        )
        .expect("the readable declarations survive");
        assert_eq!(
            set.iter().map(ExposedParameter::name).collect::<Vec<_>>(),
            ["headline", "scale"]
        );
    }

    /// A merge can leave two declarations of one name. The first wins, so the
    /// surviving contract is the one the file's order presents.
    #[test]
    fn a_duplicate_name_is_dropped_from_a_set() {
        let set: ExposedParameters = ron::from_str(
            r#"[
                (name: "headline", value_type: String, default: String("first"), description: "", binding: (node: NodeId(1), key: "text")),
                (name: "headline", value_type: Int, default: Int(2), description: "", binding: (node: NodeId(2), key: "count")),
            ]"#,
        )
        .expect("the duplicate is dropped, not fatal");
        assert_eq!(set.len(), 1);
        assert_eq!(
            set.get("headline").unwrap().default_value(),
            &ExposedValue::String("first".into())
        );
    }
}

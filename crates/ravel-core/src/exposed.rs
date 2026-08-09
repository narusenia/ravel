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
//! Three invariants hold for every collection of declarations:
//!
//! 1. **names are unique** — the name *is* the contract, so two declarations
//!    of one name make the contract ambiguous. Names are stored trimmed, so
//!    surrounding whitespace cannot split one contract in two;
//! 2. **the default matches the declared type** — a declaration whose default
//!    contradicts its own type cannot produce a usable value;
//! 3. **the default is finite** ([`ExposedValue::is_finite`]) — a `NaN`
//!    default is not equal to itself, so it would break the save/load round
//!    trip the contract depends on.
//!
//! All three are enforced by the constructors *and* by deserialization
//! ([`ExposedParameters`]'s hand-written [`Deserialize`]), because a `.ravprj`
//! is text: it gets hand-edited and merged, and a derived `Deserialize` would
//! walk straight past the constructors. Following the stance the rest of
//! `.ravprj` loading takes (and
//! [`CurveParam`](crate::param_curve::CurveParam) in particular), a file whose
//! declarations parse but violate an invariant **loses those declarations, not
//! the project**. Entries that do not parse at all are a different case — see
//! [`ExposedParameters`]'s [`Deserialize`] for exactly what is and is not
//! rescued.

pub mod apply;
pub mod listing;

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

    #[error("exposed parameter {0:?} has a non-finite default value")]
    NonFiniteDefault(String),

    /// Raised by the editing operations ([`ExposedParameters::rename`],
    /// [`ExposedParameters::set_description`], [`ExposedParameters::shift`]),
    /// which name the declaration they act on. Reading and removing answer
    /// with an [`Option`] instead: "there is no such name" is the ordinary
    /// outcome of a lookup, but it is a failed edit.
    #[error("no exposed parameter named {0:?} is declared")]
    UnknownName(String),
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
    /// Whether every component of this value is finite.
    ///
    /// A non-finite default cannot survive a save/load round trip: RON writes
    /// `NaN` and reads it back, but `NaN != NaN`, so the reloaded declaration
    /// is not the one that was written — the round trip silently stops being
    /// one. It is also meaningless as a default a caller may accept. Values
    /// with no float component are trivially finite.
    ///
    /// [`CurveParam`](crate::param_curve::CurveParam) takes the same stance on
    /// non-finite control points.
    pub fn is_finite(&self) -> bool {
        match self {
            Self::Float(v) => v.is_finite(),
            Self::Vec2(v) => v.0.is_finite() && v.1.is_finite(),
            Self::Vec3(v) => v.0.is_finite() && v.1.is_finite() && v.2.is_finite(),
            Self::Vec4(v) => {
                v.0.is_finite() && v.1.is_finite() && v.2.is_finite() && v.3.is_finite()
            }
            Self::Color(c) => {
                c.r.is_finite() && c.g.is_finite() && c.b.is_finite() && c.a.is_finite()
            }
            Self::Int(_) | Self::Bool(_) | Self::String(_) | Self::Media(_) => true,
        }
    }

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

/// A parameter key that moved on one node, and therefore has to move in every
/// binding that names it ([`ExposedParameters::follow_key_rename`]).
///
/// A [`NodeId`] survives renaming and rewiring, so the *node* half of a
/// binding never needs following. The **key** half does: a network interface
/// port and its same-named parameter are one name, and renaming the port
/// rewrites the parameter key
/// ([`network::rename_custom_port`](crate::network::rename_custom_port)). A
/// declaration bound to the old key would be left naming a parameter that no
/// longer exists, so the rename produces this value and the caller's document
/// commit carries it into the declarations — one edit, no half-applied state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRename {
    /// The node whose parameter key moved.
    pub node: NodeId,
    /// The key as it was.
    pub from: String,
    /// The key as it is now.
    pub to: String,
}

impl KeyRename {
    pub fn new(node: NodeId, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            node,
            from: from.into(),
            to: to.into(),
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
    /// Fails when the name is blank, when `default` is not a value of
    /// `value_type`, or when `default` is non-finite
    /// ([`ExposedValue::is_finite`]).
    ///
    /// **The name is stored trimmed.** Surrounding whitespace is not part of
    /// the contract: a caller types the name on a command line, where trailing
    /// blanks cannot be entered reliably and are invisible when they are. The
    /// blank check already treated whitespace as insignificant, so keeping it
    /// in the stored name would make `"title"` and `" title "` two distinct
    /// declarations that no reader could tell apart.
    pub fn new(
        name: impl Into<String>,
        value_type: ExposedType,
        default: ExposedValue,
        binding: ExposedBinding,
    ) -> Result<Self, ExposedParameterError> {
        let name = name.into();
        let name = name.trim();
        if name.is_empty() {
            return Err(ExposedParameterError::EmptyName);
        }
        let name = name.to_string();
        let found = default.exposed_type();
        if found != value_type {
            return Err(ExposedParameterError::DefaultTypeMismatch {
                name,
                declared: value_type,
                found,
            });
        }
        if !default.is_finite() {
            return Err(ExposedParameterError::NonFiniteDefault(name));
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

    /// Builder: point the declaration at a different internal parameter.
    ///
    /// The binding is the one part of a declaration no invariant constrains —
    /// it names a node and a key that may or may not exist, which is a
    /// property of the document rather than of the declaration
    /// ([`apply::resolve`] reports it). So this cannot fail, unlike every
    /// other way into an [`ExposedParameter`].
    pub fn with_binding(mut self, binding: ExposedBinding) -> Self {
        self.binding = binding;
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
/// A `.ravprj` is a text file: it gets hand-edited and merged, and a derived
/// `Deserialize` would hand [`ExposedParameters`] a list that repeats a name or
/// holds a declaration whose default contradicts its own type — an ambiguous
/// contract, silently resolved differently by whichever lookup runs first.
/// Reading through the same checks the constructors use makes every set of
/// declarations in the process valid by construction:
///
/// * a declaration with a blank name, a default that is not a value of its
///   declared type, or a non-finite default is **dropped**;
/// * a declaration repeating an earlier name is **dropped**, so the first
///   declaration of a name — the one the file's order presents to a caller —
///   is the one that survives.
///
/// Each drop is logged: losing part of an external contract must not be silent.
///
/// # What this does *not* rescue
///
/// The leniency is **semantic only**. Every entry still has to deserialize
/// into the stored shape first, and that step is all-or-nothing: a missing
/// field, an unknown `ExposedType` variant, or a truncated file fails the whole
/// document load, because `Vec<StoredParameter>` cannot skip an element it
/// could not parse and resynchronize on the next one. Rescuing those would
/// mean reading entries as raw RON values at the persistence boundary rather
/// than in this `Deserialize`, and a structurally broken file gives no
/// reliable point to resume from anyway. What is guaranteed here is narrower
/// and worth stating exactly: **a file whose declarations parse but violate an
/// invariant opens without those declarations, instead of failing the load.**
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

    /// Drop the declaration named `name`, returning it; `None` when nothing
    /// was declared under that name.
    ///
    /// Removing cannot break an invariant — the remaining names were already
    /// unique and each default already matched its own type — so unlike
    /// [`ExposedParameters::insert`] this never fails. The removed declaration
    /// comes back so a caller can undo the removal by inserting it again, or
    /// report exactly what left the contract.
    pub fn remove(&mut self, name: &str) -> Option<ExposedParameter> {
        let index = self.position(name)?;
        Some(self.entries.remove(index))
    }

    /// Rename the declaration `from` to `to`, keeping its position, type,
    /// default, description and binding.
    ///
    /// The name **is** the external contract, so this is the one edit that can
    /// invalidate the set: renaming onto a name another declaration already
    /// holds would make two declarations answer to one contract name. That is
    /// refused with [`ExposedParameterError::DuplicateName`] and the set is
    /// left unchanged — the caller's job is to report the collision, not to
    /// invent a disambiguated name behind the user's back.
    ///
    /// `to` is trimmed exactly as [`ExposedParameter::new`] trims it, so a
    /// rename cannot introduce a name the constructor would have rejected.
    /// Renaming a declaration to the name it already has succeeds and changes
    /// nothing.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), ExposedParameterError> {
        let to = to.trim();
        if to.is_empty() {
            return Err(ExposedParameterError::EmptyName);
        }
        let index = self
            .position(from)
            .ok_or_else(|| ExposedParameterError::UnknownName(from.to_string()))?;
        if self.entries[index].name == to {
            return Ok(());
        }
        if self.contains(to) {
            return Err(ExposedParameterError::DuplicateName(to.to_string()));
        }
        self.entries[index].name = to.to_string();
        Ok(())
    }

    /// Replace the description shown to callers alongside `name`.
    ///
    /// No invariant constrains a description, so the only way this fails is by
    /// naming a declaration that is not there.
    pub fn set_description(
        &mut self,
        name: &str,
        description: impl Into<String>,
    ) -> Result<(), ExposedParameterError> {
        let index = self
            .position(name)
            .ok_or_else(|| ExposedParameterError::UnknownName(name.to_string()))?;
        self.entries[index].description = description.into();
        Ok(())
    }

    /// Move `name` `offset` places through the presentation order, clamped to
    /// the ends of the list. Returns whether the order changed.
    ///
    /// Order is data (it is what a listing and a `--help` show), so moving a
    /// declaration is an edit like any other. It is clamped rather than
    /// refused because the caller driving it is a pair of up/down buttons:
    /// pressing "up" on the first row is a no-op, not an error, and an
    /// `i32::MIN` offset means "to the top".
    pub fn shift(&mut self, name: &str, offset: i32) -> Result<bool, ExposedParameterError> {
        let index = self
            .position(name)
            .ok_or_else(|| ExposedParameterError::UnknownName(name.to_string()))?;
        if offset == 0 {
            return Ok(false);
        }
        let target = if offset > 0 {
            index
                .saturating_add(offset.unsigned_abs() as usize)
                .min(self.entries.len() - 1)
        } else {
            index.saturating_sub(offset.unsigned_abs() as usize)
        };
        if target == index {
            return Ok(false);
        }
        let moved = self.entries.remove(index);
        self.entries.insert(target, moved);
        Ok(true)
    }

    /// Where `name` sits in the presentation order.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|declaration| declaration.name() == name)
    }

    /// The declaration bound to `key` on `node`, if one is.
    ///
    /// This is the question a parameter editor asks about the row it is about
    /// to draw ("is this parameter already exposed, and under what name?").
    /// A binding is not constrained to be unique — two declarations may drive
    /// one parameter — so this answers with the first, which is the one the
    /// presentation order puts in front of a caller.
    pub fn bound_to(&self, node: NodeId, key: &str) -> Option<&ExposedParameter> {
        self.entries
            .iter()
            .find(|declaration| declaration.binding.node == node && declaration.binding.key == key)
    }

    /// Follow `rename`: every binding naming the key it moved names the new
    /// key afterwards. Returns how many declarations moved.
    ///
    /// Names, types and defaults are untouched, so no invariant can be
    /// disturbed — the external contract is exactly what it was, still
    /// pointing at the parameter it always pointed at.
    pub fn follow_key_rename(&mut self, rename: &KeyRename) -> usize {
        let mut moved = 0;
        for declaration in &mut self.entries {
            if declaration.binding.node == rename.node && declaration.binding.key == rename.from {
                declaration.binding.key = rename.to.clone();
                moved += 1;
            }
        }
        moved
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

    /// Rewrite every declaration's default value in place, keeping names,
    /// types, descriptions and bindings.
    ///
    /// Exists for the `.ravprj` v7 → v8 colour pass, which has to reinterpret
    /// a `color` default that lives outside every node network. A declaration
    /// whose rewritten default no longer satisfies its own declared type is
    /// **kept unchanged** rather than dropped: a migration must not delete a
    /// contract other tools consume by name.
    pub fn map_defaults(self, mut rewrite: impl FnMut(ExposedValue) -> ExposedValue) -> Self {
        let entries = self
            .entries
            .into_iter()
            .map(|declaration| {
                let rewritten = rewrite(declaration.default_value().clone());
                let description = declaration.description().to_string();
                ExposedParameter::new(
                    declaration.name(),
                    declaration.value_type(),
                    rewritten,
                    declaration.binding().clone(),
                )
                .map(|updated| updated.with_description(description))
                .unwrap_or(declaration)
            })
            .collect();
        Self { entries }
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

    /// The blank check treats whitespace as insignificant, so the stored name
    /// has to as well — otherwise `" title "` and `"title"` are two contracts
    /// a caller cannot tell apart, and neither can a reader of the file.
    #[test]
    fn a_name_is_stored_trimmed_so_whitespace_cannot_split_one_contract() {
        let declaration =
            ExposedParameter::inferred("  headline\t", ExposedValue::Bool(true), binding())
                .unwrap();
        assert_eq!(declaration.name(), "headline");

        let mut set = ExposedParameters::new();
        set.insert(
            ExposedParameter::inferred("headline", ExposedValue::Bool(true), binding()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            set.insert(declaration),
            Err(ExposedParameterError::DuplicateName("headline".to_string()))
        );
        assert_eq!(set.len(), 1);
    }

    /// A `NaN` default is not equal to itself, so a declaration carrying one
    /// serializes and deserializes without error yet comes back as a different
    /// declaration — the round trip the external contract depends on quietly
    /// stops being one. Rejected at construction, like `CurveParam`'s
    /// non-finite control points.
    #[test]
    fn a_non_finite_default_is_rejected() {
        let cases = [
            ExposedValue::Float(f32::NAN),
            ExposedValue::Float(f32::INFINITY),
            ExposedValue::Vec2(Vec2(0.0, f32::NAN)),
            ExposedValue::Vec3(Vec3(0.0, f32::NEG_INFINITY, 0.0)),
            ExposedValue::Vec4(Vec4(0.0, 0.0, 0.0, f32::NAN)),
            ExposedValue::Color(Color::new(0.0, f32::NAN, 0.0, 1.0)),
        ];
        for value in cases {
            assert!(!value.is_finite(), "{value:?} should not be finite");
            assert_eq!(
                ExposedParameter::inferred("p", value, binding()),
                Err(ExposedParameterError::NonFiniteDefault("p".to_string()))
            );
        }
    }

    /// Values with no float component are finite by construction, so the new
    /// check cannot reject a declaration that used to be valid.
    #[test]
    fn values_without_floats_are_finite() {
        for value in [
            ExposedValue::Int(-3),
            ExposedValue::Bool(false),
            ExposedValue::String("x".to_string()),
            ExposedValue::Float(0.0),
            ExposedValue::Vec2(Vec2(1.0, 2.0)),
        ] {
            assert!(value.is_finite(), "{value:?} should be finite");
        }
    }

    /// The non-finite rule has to hold on the load path too: a hand-edited
    /// file carrying `NaN` loses that declaration and keeps the rest.
    #[test]
    fn a_non_finite_declaration_is_dropped_from_a_set() {
        let text = r#"[
            (name:"bad",value_type:Float,default:Float(NaN),description:"",binding:(node:(1),key:"a")),
            (name:"good",value_type:Float,default:Float(1.5),description:"",binding:(node:(2),key:"b")),
        ]"#;
        let set: ExposedParameters = ron::from_str(text).unwrap();
        assert_eq!(
            set.iter().map(ExposedParameter::name).collect::<Vec<_>>(),
            ["good"]
        );
    }

    /// A port rename moves a parameter key; the declarations bound to it move
    /// with it, and nothing else does — a same-named key on another node is a
    /// different parameter.
    #[test]
    fn a_key_rename_moves_only_the_bindings_that_named_that_key() {
        let mut set = ExposedParameters::from_declarations([
            ExposedParameter::inferred(
                "headline",
                ExposedValue::Bool(true),
                ExposedBinding::new(NodeId::new(42), "text"),
            )
            .unwrap(),
            ExposedParameter::inferred(
                "elsewhere",
                ExposedValue::Bool(true),
                ExposedBinding::new(NodeId::new(7), "text"),
            )
            .unwrap(),
            ExposedParameter::inferred(
                "other_key",
                ExposedValue::Bool(true),
                ExposedBinding::new(NodeId::new(42), "scale"),
            )
            .unwrap(),
        ])
        .unwrap();

        let moved = set.follow_key_rename(&KeyRename::new(NodeId::new(42), "text", "title"));

        assert_eq!(moved, 1);
        assert_eq!(
            set.get("headline").unwrap().binding(),
            &ExposedBinding::new(NodeId::new(42), "title")
        );
        assert_eq!(
            set.get("elsewhere").unwrap().binding(),
            &ExposedBinding::new(NodeId::new(7), "text"),
            "the same key on another node is another parameter"
        );
        assert_eq!(
            set.get("other_key").unwrap().binding(),
            &ExposedBinding::new(NodeId::new(42), "scale")
        );
    }

    /// Following a rename touches the binding and nothing else: the name, the
    /// type and the default are the contract and must come through unchanged.
    #[test]
    fn following_a_rename_leaves_the_contract_alone() {
        let mut set = ExposedParameters::from_declarations([headline()]).unwrap();
        set.follow_key_rename(&KeyRename::new(NodeId::new(42), "text", "title"));
        let declaration = set.get("headline").unwrap();
        assert_eq!(declaration.value_type(), ExposedType::String);
        assert_eq!(
            declaration.default_value(),
            &ExposedValue::String("Ravel".into())
        );
        assert_eq!(declaration.description(), "The title card's text");
    }

    #[test]
    fn a_rename_of_a_key_nothing_binds_moves_nothing() {
        let mut set = ExposedParameters::from_declarations([headline()]).unwrap();
        let before = set.clone();
        assert_eq!(
            set.follow_key_rename(&KeyRename::new(NodeId::new(42), "scale", "size")),
            0
        );
        assert_eq!(set, before);
    }

    #[test]
    fn a_declaration_can_be_pointed_at_another_parameter() {
        let moved = headline().with_binding(ExposedBinding::new(NodeId::new(7), "caption"));
        assert_eq!(
            moved.binding(),
            &ExposedBinding::new(NodeId::new(7), "caption")
        );
        assert_eq!(moved.name(), "headline");
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

    // =======================================================================
    // Editing (EXPO-5)
    // =======================================================================

    /// Three declarations named `a`, `b`, `c`, in that order.
    fn abc() -> ExposedParameters {
        ExposedParameters::from_declarations(["a", "b", "c"].into_iter().map(|name| {
            ExposedParameter::inferred(name, ExposedValue::Int(0), binding())
                .expect("an int defaults to an int")
        }))
        .expect("the three names differ")
    }

    fn names(set: &ExposedParameters) -> Vec<&str> {
        set.iter().map(ExposedParameter::name).collect()
    }

    #[test]
    fn removing_a_declaration_returns_it_and_keeps_the_rest_in_order() {
        let mut set = abc();
        let removed = set.remove("b").expect("b is declared");
        assert_eq!(removed.name(), "b");
        assert_eq!(names(&set), ["a", "c"]);
        assert_eq!(set.remove("b"), None);
    }

    #[test]
    fn a_renamed_declaration_keeps_its_place_and_everything_but_its_name() {
        let mut set = ExposedParameters::from_declarations([
            headline(),
            ExposedParameter::inferred("scale", ExposedValue::Float(1.0), binding())
                .expect("a float defaults to a float"),
        ])
        .expect("the names differ");
        set.rename("headline", "title").expect("title is free");
        assert_eq!(names(&set), ["title", "scale"]);
        let renamed = set.get("title").expect("the new name is declared");
        assert_eq!(renamed.value_type(), ExposedType::String);
        assert_eq!(
            renamed.default_value(),
            &ExposedValue::String("Ravel".into())
        );
        assert_eq!(renamed.description(), "The title card's text");
        assert_eq!(renamed.binding(), &binding());
        assert!(!set.contains("headline"));
    }

    /// The editing UI's refusal case: the name is the contract, so two
    /// declarations may not answer to one name.
    #[test]
    fn renaming_onto_an_existing_name_is_refused_and_changes_nothing() {
        let mut set = abc();
        let err = set.rename("a", "c").expect_err("c is taken");
        assert_eq!(err, ExposedParameterError::DuplicateName("c".into()));
        assert_eq!(names(&set), ["a", "b", "c"]);
    }

    #[test]
    fn renaming_a_declaration_to_its_own_name_is_accepted_and_changes_nothing() {
        let mut set = abc();
        set.rename("b", "b")
            .expect("a declaration may keep its name");
        assert_eq!(names(&set), ["a", "b", "c"]);
    }

    /// `new` trims, so renaming has to trim too — otherwise the editor could
    /// mint a name the constructor would have refused, and `" a "` and `"a"`
    /// would be two contracts nobody could tell apart.
    #[test]
    fn a_rename_trims_and_refuses_a_blank_name() {
        let mut set = abc();
        set.rename("a", "  spaced  ").expect("the name is trimmed");
        assert_eq!(names(&set), ["spaced", "b", "c"]);
        assert_eq!(
            set.rename("spaced", "   ")
                .expect_err("a blank name is not a name"),
            ExposedParameterError::EmptyName
        );
        // Trimming also means the collision check sees the trimmed form.
        assert_eq!(
            set.rename("spaced", " b ").expect_err("b is taken"),
            ExposedParameterError::DuplicateName("b".into())
        );
    }

    #[test]
    fn editing_a_declaration_that_is_not_declared_says_so() {
        let mut set = abc();
        assert_eq!(
            set.rename("z", "y").expect_err("z is not declared"),
            ExposedParameterError::UnknownName("z".into())
        );
        assert_eq!(
            set.set_description("z", "doc")
                .expect_err("z is not declared"),
            ExposedParameterError::UnknownName("z".into())
        );
        assert_eq!(
            set.shift("z", 1).expect_err("z is not declared"),
            ExposedParameterError::UnknownName("z".into())
        );
    }

    #[test]
    fn a_description_can_be_replaced() {
        let mut set = abc();
        assert_eq!(set.get("a").unwrap().description(), "");
        set.set_description("a", "how wide the plate is")
            .expect("a is declared");
        assert_eq!(set.get("a").unwrap().description(), "how wide the plate is");
    }

    #[test]
    fn shifting_moves_a_declaration_through_the_presentation_order() {
        let mut set = abc();
        assert!(set.shift("c", -1).expect("c is declared"));
        assert_eq!(names(&set), ["a", "c", "b"]);
        assert!(set.shift("a", 2).expect("a is declared"));
        assert_eq!(names(&set), ["c", "b", "a"]);
    }

    /// Up on the first row and down on the last are no-ops, not errors: the
    /// buttons driving this are always pressable.
    #[test]
    fn shifting_past_an_end_clamps_and_reports_no_change() {
        let mut set = abc();
        assert!(!set.shift("a", -1).expect("a is declared"));
        assert!(!set.shift("c", 1).expect("c is declared"));
        assert!(!set.shift("b", 0).expect("b is declared"));
        assert_eq!(names(&set), ["a", "b", "c"]);
        assert!(set.shift("c", i32::MIN).expect("c is declared"));
        assert_eq!(names(&set), ["c", "a", "b"]);
        assert!(set.shift("c", i32::MAX).expect("c is declared"));
        assert_eq!(names(&set), ["a", "b", "c"]);
    }

    #[test]
    fn a_binding_finds_the_declaration_that_drives_it() {
        let mut set = ExposedParameters::new();
        set.insert(headline()).expect("the set is empty");
        assert_eq!(
            set.bound_to(NodeId::new(42), "text")
                .map(ExposedParameter::name),
            Some("headline")
        );
        assert!(set.bound_to(NodeId::new(42), "other").is_none());
        assert!(set.bound_to(NodeId::new(7), "text").is_none());
    }

    #[test]
    fn position_reports_the_presentation_order() {
        let set = abc();
        assert_eq!(set.position("a"), Some(0));
        assert_eq!(set.position("c"), Some(2));
        assert_eq!(set.position("z"), None);
    }
}

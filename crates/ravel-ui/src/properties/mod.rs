// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generic property inspection system.
//!
//! [`PropertySection`] and [`PropertyField`] provide a source-agnostic model
//! for the Properties panel. Any inspectable target (node, clip, project
//! settings) produces a list of sections; the GPUI panel renders them with
//! the appropriate widgets without knowing the source type.

pub mod composition;
pub mod exposed;
pub mod expression;
pub mod layer;
pub mod node;

use exposed::ExposedRow;
use ravel_core::graph::PortSide;
use ravel_core::network::CustomPortType;
use ravel_core::param_curve::CurveParam;
use std::ops::RangeInclusive;

/// One row of a [`PropertyField::PortList`]: a port that exists on the
/// interface node right now.
///
/// **Fixed ports are rows too.** `net.in`'s `base_geometry` / `t` / `f` /
/// `source` and `net.out`'s `frame` are part of the interface the user reads,
/// and hiding them would make the list disagree with the node on the canvas;
/// `fixed` is what tells the host to render the row read-only instead
/// (`ravel_core::network::is_fixed_port` is the authority, and the same
/// predicate refuses the edit if a stale row ever reaches the graph).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortRow {
    /// Port name — also the parameter key on an In node's custom port.
    pub name: String,
    /// The type the port declares today
    /// (`ravel_core::network::custom_port_type`). `None` for a wire type no
    /// custom port can have, which only a hand-built graph can produce; the
    /// host shows such a row read-only because no menu entry describes it.
    pub port_type: Option<CustomPortType>,
    /// The shell owns this port: it cannot be renamed, retyped, reordered or
    /// removed.
    pub fixed: bool,
}

/// A single editable (or read-only) field in a property section.
///
/// Numeric fields carry two ranges: `range` is the hard clamp boundary a
/// value can never leave, `ui_range` is the comfortable editing span widgets
/// present by default (slider bounds, scrub sensitivity).
#[derive(Clone, Debug)]
pub enum PropertyField {
    Float {
        key: String,
        value: f32,
        range: Option<RangeInclusive<f32>>,
        ui_range: Option<RangeInclusive<f32>>,
        step: Option<f32>,
    },
    Int {
        key: String,
        value: i32,
        range: Option<RangeInclusive<i32>>,
        ui_range: Option<RangeInclusive<i32>>,
        step: Option<i32>,
    },
    Bool {
        key: String,
        value: bool,
    },
    String {
        key: String,
        value: String,
    },
    Enum {
        key: String,
        value: String,
        options: Vec<String>,
    },
    Color {
        key: String,
        r: f32,
        g: f32,
        b: f32,
        a: f32,
    },
    /// Multi-component vector (2-4 components) edited as one scrub input per
    /// component. The optional ranges apply to every component (registry
    /// templates declare one range per parameter).
    Vector {
        key: String,
        components: Vec<f32>,
        range: Option<RangeInclusive<f32>>,
        ui_range: Option<RangeInclusive<f32>>,
        step: Option<f32>,
    },
    /// A scalar transfer curve edited by an inline curve editor: the row
    /// shows a thumbnail while collapsed and expands the editor underneath
    /// itself. The expansion is host view state, never part of the field —
    /// it must not reach the Document (and therefore undo).
    Curve {
        key: String,
        curve: CurveParam,
    },
    /// A value the panel shows but cannot edit.
    ///
    /// `value` is displayed verbatim unless it names a locale key (a state
    /// word such as [`layer::VALUE_ON`]) or carries a count appended with
    /// [`counted_value`]; this crate has no i18n dependency, so both are
    /// resolved at the display boundary.
    ReadOnly {
        key: String,
        value: String,
    },
    /// The port list of a network interface node (`net.in` / `net.out`,
    /// REQ-LAYER-002/003): every port the node declares, plus the type menu a
    /// new or retyped port may pick from.
    ///
    /// Unlike every other variant this is not a *value* — it describes the
    /// node's shape, so it never travels through [`PropertyValue`] and the
    /// host routes its edits to the dedicated `network` operations rather than
    /// to a parameter write.
    PortList {
        key: String,
        /// Which port list the rows address: In declares its custom ports as
        /// outputs, Out as inputs. The host needs it to name the side in the
        /// graph call.
        side: PortSide,
        rows: Vec<PortRow>,
        /// The types the add / retype menu offers, in menu order, already
        /// narrowed to what this node may declare in its
        /// [`ravel_core::network::NetworkContext`].
        options: Vec<CustomPortType>,
    },
    /// The project's exposed parameter declarations (REQ-PROJ-006): the
    /// external contract a CLI render or a template instantiation may set.
    ///
    /// Like [`PropertyField::PortList`] this is not a *value* — it is a list
    /// whose shape the user edits — so it never travels through
    /// [`PropertyValue`] and the host routes its edits to the declaration
    /// operations rather than to a parameter write.
    ///
    /// A declaration is **created by exposing a parameter**, not by an add row
    /// here: a declaration with no binding would be a contract name that
    /// reaches nothing, and picking a binding from a list of every parameter in
    /// the document is a worse way to say "this one" than clicking the
    /// parameter itself.
    ExposedList {
        key: String,
        rows: Vec<ExposedRow>,
    },
}

impl PropertyField {
    pub fn key(&self) -> &str {
        match self {
            Self::Float { key, .. }
            | Self::Int { key, .. }
            | Self::Bool { key, .. }
            | Self::String { key, .. }
            | Self::Enum { key, .. }
            | Self::Color { key, .. }
            | Self::Vector { key, .. }
            | Self::Curve { key, .. }
            | Self::ReadOnly { key, .. }
            | Self::PortList { key, .. }
            | Self::ExposedList { key, .. } => key,
        }
    }
}

/// Separates a [`PropertyField::ReadOnly`] value's locale key from the count
/// substituted into the translated phrase.
///
/// U+001F (unit separator) is a control character: no layer name, node label,
/// id or formatted number contains one, so a counted value can never be
/// mistaken for a literal one — or the other way round.
const COUNT_SEP: char = '\u{1f}';

/// A read-only value that is a locale key plus a number to substitute into
/// its `{count}` placeholder ("Network (3 nodes)", "300 frames").
///
/// The whole phrase has to be one locale key: word order around a number is
/// language-specific, so a translated fragment concatenated with a digit
/// would stay English-shaped. This crate cannot translate, so it emits the
/// key and the count together and the host substitutes at the display
/// boundary ([`split_counted_value`] is the reader).
pub fn counted_value(key: &str, count: u64) -> String {
    format!("{key}{COUNT_SEP}{count}")
}

/// Splits a value written by [`counted_value`] into its locale key and the
/// count's decimal text. `None` for a plain value, which is either a literal
/// or a bare locale key.
pub fn split_counted_value(value: &str) -> Option<(&str, &str)> {
    value.split_once(COUNT_SEP)
}

/// A titled group of property fields.
///
/// `title` is a locale key (e.g. `properties.section.transform`); the host
/// resolves it through `ravel-i18n` at render time. Field `key`s are stable
/// identifiers, likewise translated only for display.
#[derive(Clone, Debug)]
pub struct PropertySection {
    pub title: String,
    pub fields: Vec<PropertyField>,
}

/// A node parameter currently driven by a connected parameter port
/// (param-input-ports-plan Phase 4). Produced by the node editor (which
/// owns the graph) and carried on the properties target so section
/// builders can render driven parameters read-only.
#[derive(Clone, Debug, PartialEq)]
pub struct DrivenParam {
    /// Parameter key (also the port name).
    pub key: String,
    /// Display label of the driving node (label or type key).
    pub source: String,
    /// Display value when statically known (constant / constant.color
    /// sources); `None` renders as "connected". Live evaluated values for
    /// arbitrary sources are a known follow-up (see the plan).
    pub value: Option<String>,
}

/// The value half of a [`PropertyField`], used in change notifications.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    Color {
        r: f32,
        g: f32,
        b: f32,
        a: f32,
    },
    Vector(Vec<f32>),
    /// A whole edited curve. Curve edits replace the control-point set
    /// rather than a scalar, so the gesture granularity is the same as a
    /// scrub's: live edits apply uncommitted and the gesture's last value
    /// records one undo step.
    Curve(CurveParam),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_key_accessor() {
        let f = PropertyField::Float {
            key: "brightness".into(),
            value: 0.5,
            range: Some(-1.0..=1.0),
            ui_range: Some(-1.0..=1.0),
            step: Some(0.01),
        };
        assert_eq!(f.key(), "brightness");

        let r = PropertyField::ReadOnly {
            key: "type".into(),
            value: "blur".into(),
        };
        assert_eq!(r.key(), "type");
    }

    #[test]
    fn property_section_holds_fields() {
        let section = PropertySection {
            title: "Parameters".into(),
            fields: vec![
                PropertyField::Float {
                    key: "radius".into(),
                    value: 5.0,
                    range: Some(0.0..=100.0),
                    ui_range: Some(0.0..=50.0),
                    step: None,
                },
                PropertyField::Bool {
                    key: "enabled".into(),
                    value: true,
                },
            ],
        };
        assert_eq!(section.title, "Parameters");
        assert_eq!(section.fields.len(), 2);
    }
}

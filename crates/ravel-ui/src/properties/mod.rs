// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generic property inspection system.
//!
//! [`PropertySection`] and [`PropertyField`] provide a source-agnostic model
//! for the Properties panel. Any inspectable target (node, clip, project
//! settings) produces a list of sections; the GPUI panel renders them with
//! the appropriate widgets without knowing the source type.

pub mod layer;
pub mod node;

use std::ops::RangeInclusive;

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
    ReadOnly {
        key: String,
        value: String,
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
            | Self::ReadOnly { key, .. } => key,
        }
    }
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

/// The value half of a [`PropertyField`], used in change notifications.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    Color { r: f32, g: f32, b: f32, a: f32 },
    Vector(Vec<f32>),
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

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The machine-readable listing of a document's exposed parameters
//! (REQ-RENDER-005: "the list of declarations can be obtained in a
//! machine-readable form").
//!
//! This is what a headless caller reads *before* it passes any values: a
//! renderer asked for `--list-params` discovers the names, the types, the
//! defaults and the descriptions here, then supplies values through
//! [`apply`](super::apply::apply).
//!
//! # Why the listing is not the persisted form
//!
//! [`ExposedParameters`](super::ExposedParameters) already serializes — that
//! is how a `.ravprj` stores it — but the persisted form is the wrong contract
//! to hand a caller twice over:
//!
//! * it carries the **binding** (a node id and a parameter key), which is
//!   exactly the internal detail the declaration exists to hide. A caller that
//!   read it would start depending on the network's shape, and renaming a node
//!   would break the caller the design promised it could not break;
//! * it is shaped by Rust's enum representation. `ExposedValue::Float(1.5)`
//!   persists as an externally tagged variant, so a caller reading defaults
//!   from it would break the day a variant is renamed. This module writes the
//!   value **natively** instead — a float is a number, a `Vec3` is an array of
//!   three, a media reference is its path string — so the JSON a CLI prints is
//!   stable against changes to the internal representation, which is the same
//!   argument that keeps `ExposedValue` smaller than `ParameterValue`
//!   ([`super`]).
//!
//! The type name is written the way [`ExposedType`]'s `Display` writes it
//! (`"float"`, `"vec2"`, `"media"`), which is the spelling a caller uses on a
//! command line.
//!
//! # Where it lives
//!
//! In `ravel-core`, so nothing about reading a project's contract needs a
//! window: `ravel-project` loads a `.ravprj` without `gpui`, and a headless
//! caller goes document → listing → JSON with no part of the application in
//! the way.

use serde::{Serialize, Serializer, ser::SerializeSeq};

use crate::composition::Document;
use crate::exposed::apply;
use crate::exposed::{ExposedParameter, ExposedType, ExposedValue};

/// One declaration, as the outside world sees it.
///
/// Deliberately without the binding: the contract is the name (see the module
/// documentation).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExposedListingEntry {
    /// The name a caller passes a value under.
    pub name: String,
    /// The declared type, spelled as [`ExposedType`]'s `Display`.
    #[serde(rename = "type", serialize_with = "serialize_type")]
    pub value_type: ExposedType,
    /// The value in force when the caller supplies nothing, written natively.
    #[serde(serialize_with = "serialize_value")]
    pub default: ExposedValue,
    /// The human-readable description; empty when undocumented.
    pub description: String,
    /// Whether the declaration's binding currently reaches a parameter it can
    /// drive ([`apply::resolve`]).
    ///
    /// `false` means supplying a value would change nothing — the node was
    /// deleted, the parameter was retyped, or the parameter is animated rather
    /// than constant. The declaration is still part of the contract and still
    /// listed: hiding it would tell a caller the name does not exist, when
    /// what is true is that the project behind it is broken.
    pub resolved: bool,
}

/// A document's declarations in presentation order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExposedListing {
    pub parameters: Vec<ExposedListingEntry>,
}

impl ExposedListing {
    /// The listing for `document`.
    pub fn of(document: &Document) -> Self {
        let unresolved = apply::resolve(document);
        let parameters = document
            .exposed_parameters
            .iter()
            .map(|declaration| ExposedListingEntry {
                name: declaration.name().to_string(),
                value_type: declaration.value_type(),
                default: declaration.default_value().clone(),
                description: declaration.description().to_string(),
                resolved: !unresolved
                    .iter()
                    .any(|issue| issue.name == declaration.name()),
            })
            .collect();
        Self { parameters }
    }

    /// The declarations alone, with no document to resolve them against.
    ///
    /// Every entry reports `resolved: false`, because nothing here can know
    /// whether a binding lands — use [`ExposedListing::of`] whenever a
    /// document is at hand. This exists for a caller holding declarations that
    /// are not (yet) part of one, such as a subgraph template's.
    pub fn of_declarations<'a>(
        declarations: impl IntoIterator<Item = &'a ExposedParameter>,
    ) -> Self {
        let parameters = declarations
            .into_iter()
            .map(|declaration| ExposedListingEntry {
                name: declaration.name().to_string(),
                value_type: declaration.value_type(),
                default: declaration.default_value().clone(),
                description: declaration.description().to_string(),
                resolved: false,
            })
            .collect();
        Self { parameters }
    }

    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parameters.len()
    }
}

fn serialize_type<S: Serializer>(value: &ExposedType, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// Write the value the way the type it belongs to is written, not the way the
/// Rust enum is (see the module documentation).
fn serialize_value<S: Serializer>(value: &ExposedValue, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        ExposedValue::Float(v) => serializer.serialize_f32(*v),
        ExposedValue::Int(v) => serializer.serialize_i32(*v),
        ExposedValue::Bool(v) => serializer.serialize_bool(*v),
        ExposedValue::String(v) => serializer.serialize_str(v),
        // A path is a string in every form it takes — the same string the
        // persisted document holds (`AssetPath`'s own `Serialize`).
        ExposedValue::Media(path) => serializer.collect_str(path),
        ExposedValue::Vec2(v) => components(serializer, &[v.0, v.1]),
        ExposedValue::Vec3(v) => components(serializer, &[v.0, v.1, v.2]),
        ExposedValue::Vec4(v) => components(serializer, &[v.0, v.1, v.2, v.3]),
        ExposedValue::Color(c) => components(serializer, &[c.r, c.g, c.b, c.a]),
    }
}

fn components<S: Serializer>(serializer: S, values: &[f32]) -> Result<S::Ok, S::Error> {
    let mut seq = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        seq.serialize_element(value)?;
    }
    seq.end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{AssetPath, Composition, Layer};
    use crate::exposed::{ExposedBinding, ExposedParameters};
    use crate::graph::{Graph, Node, ParameterValue};
    use crate::id::{CompId, DataTypeId, LayerId, NodeId};
    use crate::types::{Color, FrameRate, Vec3};

    fn bound() -> NodeId {
        NodeId::new(1)
    }

    fn document(declarations: ExposedParameters) -> Document {
        let network = Graph::new()
            .add_node(
                Node::new(bound(), "test")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("text", ParameterValue::String("Ravel".into()))
                    .with_param("scale", ParameterValue::Float(1.0)),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Title", network).with_time(0, 0, 100));
        Document::default()
            .with_composition(comp)
            .with_exposed_parameters(declarations)
    }

    fn declaration(name: &str, default: ExposedValue, key: &str) -> ExposedParameter {
        ExposedParameter::inferred(name, default, ExposedBinding::new(bound(), key)).unwrap()
    }

    #[test]
    fn a_listing_reports_name_type_default_and_description() {
        let document = document(
            ExposedParameters::from_declarations([
                declaration("headline", ExposedValue::String("Ravel".into()), "text")
                    .with_description("The title card's text"),
                declaration("scale", ExposedValue::Float(2.5), "scale"),
            ])
            .unwrap(),
        );

        let listing = ExposedListing::of(&document);
        assert_eq!(listing.len(), 2);
        assert_eq!(
            listing.parameters[0],
            ExposedListingEntry {
                name: "headline".to_string(),
                value_type: ExposedType::String,
                default: ExposedValue::String("Ravel".into()),
                description: "The title card's text".to_string(),
                resolved: true,
            }
        );
        assert_eq!(listing.parameters[1].name, "scale");
        assert_eq!(listing.parameters[1].description, "");
    }

    /// Declaration order is the presentation order, all the way out.
    #[test]
    fn a_listing_keeps_the_declaration_order() {
        let document = document(
            ExposedParameters::from_declarations([
                declaration("b", ExposedValue::Float(1.0), "scale"),
                declaration("a", ExposedValue::String("x".into()), "text"),
            ])
            .unwrap(),
        );
        assert_eq!(
            ExposedListing::of(&document)
                .parameters
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    /// The serialized form is the contract a CLI prints: type names as a
    /// caller spells them, values written natively rather than as tagged Rust
    /// variants, and no binding anywhere.
    #[test]
    fn a_listing_serializes_without_the_internal_representation() {
        let document = document(
            ExposedParameters::from_declarations([
                declaration("headline", ExposedValue::String("Ravel".into()), "text")
                    .with_description("The title"),
                declaration("scale", ExposedValue::Float(2.5), "scale"),
                declaration("offset", ExposedValue::Vec3(Vec3(1.0, 2.0, 3.0)), "missing"),
                declaration(
                    "tint",
                    ExposedValue::Color(Color::new(1.0, 0.5, 0.25, 1.0)),
                    "missing",
                ),
                declaration(
                    "plate",
                    ExposedValue::Media(AssetPath::Relative("./footage/plate.mov".into())),
                    "missing",
                ),
                declaration("loop_it", ExposedValue::Bool(true), "missing"),
                declaration("count", ExposedValue::Int(3), "missing"),
            ])
            .unwrap(),
        );

        let text = ron::to_string(&ExposedListing::of(&document)).unwrap();
        assert!(
            !text.contains("binding") && !text.contains("NodeId"),
            "the listing must not leak where the value lands: {text}"
        );
        assert!(
            text.contains(r#"type:"string""#) && text.contains(r#"type:"vec3""#),
            "types are spelled the way a caller spells them: {text}"
        );
        assert!(
            text.contains("default:2.5") && text.contains("default:[1.0,2.0,3.0]"),
            "values are written natively, not as tagged variants: {text}"
        );
        assert!(
            text.contains(r#"default:"./footage/plate.mov""#),
            "a media reference is its path: {text}"
        );
        assert!(
            text.contains("default:true") && text.contains("default:3,"),
            "a bool is a bool and an int is an int: {text}"
        );
    }

    /// A declaration whose binding no longer lands stays in the listing and
    /// says so — a caller has to be able to tell "no such name" from "the
    /// project behind that name is broken".
    #[test]
    fn a_listing_marks_a_declaration_whose_binding_does_not_land() {
        let document = document(
            ExposedParameters::from_declarations([
                declaration("headline", ExposedValue::String("Ravel".into()), "text"),
                declaration("gone", ExposedValue::Float(1.0), "no_such_key"),
            ])
            .unwrap(),
        );
        let listing = ExposedListing::of(&document);
        assert!(listing.parameters[0].resolved);
        assert!(!listing.parameters[1].resolved);
        assert_eq!(
            listing.len(),
            2,
            "an unresolved declaration is still listed"
        );
    }

    /// A declaration whose value only reaches part of its parameter is not a
    /// resolved one: `resolved: true` has to mean the whole value takes
    /// effect, or a CLI reading the listing is being told something false.
    #[test]
    fn a_listing_does_not_call_a_partial_application_resolved() {
        use crate::animation::channel::AnimationChannel;
        use crate::animation::curve::KeyframeCurve;
        use crate::animation::interpolation::Interpolation;
        use crate::types::Vec2;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 1.0, Interpolation::Linear);
        curve.insert(30, 5.0, Interpolation::Linear);
        let network = Graph::new()
            .add_node(
                Node::new(bound(), "test")
                    .with_output("out", DataTypeId::SCALAR)
                    // `x` is keyframed, `y` is constant.
                    .with_param(
                        "offset",
                        ParameterValue::Channel2([
                            AnimationChannel::keyframes(curve),
                            AnimationChannel::constant(0.0),
                        ]),
                    ),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Title", network).with_time(0, 0, 100));
        let document = Document::default()
            .with_composition(comp)
            .with_exposed_parameters(
                ExposedParameters::from_declarations([declaration(
                    "offset",
                    ExposedValue::Vec2(Vec2(0.0, 0.0)),
                    "offset",
                )])
                .unwrap(),
            );

        let listing = ExposedListing::of(&document);
        assert!(!listing.parameters[0].resolved);
    }

    #[test]
    fn a_document_without_declarations_lists_nothing() {
        assert!(ExposedListing::of(&document(ExposedParameters::new())).is_empty());
    }

    #[test]
    fn declarations_can_be_listed_without_a_document() {
        let declarations = ExposedParameters::from_declarations([declaration(
            "headline",
            ExposedValue::Bool(true),
            "text",
        )])
        .unwrap();
        let listing = ExposedListing::of_declarations(&declarations);
        assert_eq!(listing.parameters[0].name, "headline");
        assert!(
            !listing.parameters[0].resolved,
            "nothing resolved it, so nothing may claim it resolves"
        );
    }
}

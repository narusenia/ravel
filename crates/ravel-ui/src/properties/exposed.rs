// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The project's exposed parameter declarations as a property section
//! (REQ-PROJ-006, EXPO-5).
//!
//! The declarations are the project's *external* contract — what a CLI render
//! or a template instantiation may set — so they belong to the project rather
//! than to any node, and the section is built for
//! [`PropertiesTarget::Project`](crate::properties) rather than appended to a
//! node's sections.
//!
//! # Nothing here decides whether a declaration resolves
//!
//! Whether a binding lands is
//! [`ravel_core::exposed::apply::resolve`]'s answer, and the reason it gives is
//! a [`BindingIssueReason`]. This module maps that reason onto a locale key and
//! stops. Re-deriving "is this declaration usable?" from the document here
//! would give the panel a second opinion that drifts from the one a CLI render
//! acts on, and the whole point of the mechanism is that both read the same
//! contract.
//!
//! [`BindingIssue`](ravel_core::exposed::apply::BindingIssue)'s own `Display`
//! is deliberately not used: it is English prose for a log or a `--check`
//! report, and this crate carries no translations, so the row names a key the
//! host resolves (the same stance [`layer::VALUE_ON`](super::layer::VALUE_ON)
//! takes).

use ravel_core::composition::Document;
use ravel_core::exposed::apply::{BindingIssueReason, resolve};
use ravel_core::exposed::{ExposedParameter, ExposedValue};

use super::{PropertyField, PropertySection};

/// Field key of the declarations list. One list per project, so the key names
/// the section's single field rather than any declaration.
pub const FIELD_EXPOSED: &str = "exposed";

/// Section title of the declarations list.
pub const SECTION_EXPOSED: &str = "properties.section.exposed";

/// Locale keys for the reasons a declaration does not (fully) reach its
/// parameter. One key per [`BindingIssueReason`] variant, so the vocabulary the
/// user reads is the vocabulary the core reports — a panel that invented
/// "broken" for all six would hide the difference between "the node is gone"
/// and "the value is keyframed, so a render will not overwrite it".
pub const ISSUE_NODE_MISSING: &str = "properties.exposed.issue.node_missing";
pub const ISSUE_PARAMETER_MISSING: &str = "properties.exposed.issue.parameter_missing";
pub const ISSUE_KIND_MISMATCH: &str = "properties.exposed.issue.kind_mismatch";
pub const ISSUE_ANIMATED_COMPONENTS: &str = "properties.exposed.issue.animated_components";
pub const ISSUE_NOT_A_MEDIA_NODE: &str = "properties.exposed.issue.not_a_media_node";
pub const ISSUE_NOT_AN_ASSET_REFERENCE: &str = "properties.exposed.issue.not_an_asset_reference";

/// The locale key describing `reason`.
pub fn issue_key(reason: &BindingIssueReason) -> &'static str {
    match reason {
        BindingIssueReason::NodeMissing => ISSUE_NODE_MISSING,
        BindingIssueReason::ParameterMissing => ISSUE_PARAMETER_MISSING,
        BindingIssueReason::KindMismatch { .. } => ISSUE_KIND_MISMATCH,
        BindingIssueReason::AnimatedComponents { .. } => ISSUE_ANIMATED_COMPONENTS,
        BindingIssueReason::NotAMediaNode { .. } => ISSUE_NOT_A_MEDIA_NODE,
        BindingIssueReason::NotAnAssetReference { .. } => ISSUE_NOT_AN_ASSET_REFERENCE,
    }
}

/// One row of a [`PropertyField::ExposedList`]: a declaration as the panel
/// shows it.
///
/// The binding is not a field. A declaration's binding is set by exposing a
/// parameter and followed automatically afterwards; showing a node id would put
/// an internal identifier — the thing the contract exists to hide — in front of
/// the user, and offering to edit it would be a second way to create the
/// binding that the "expose" affordance already creates correctly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExposedRow {
    /// The contract name, unique among the rows.
    pub name: String,
    /// The declared type in the spelling a caller types on a command line
    /// (`ExposedType`'s `Display`). **Not translated** — it is syntax, like
    /// the unit glyphs the timeline leaves alone.
    pub value_type: String,
    /// The default, rendered the way a caller would write it.
    pub default: String,
    /// The description shown to callers; empty when undocumented.
    pub description: String,
    /// Locale key naming why this declaration's binding does not (fully) land,
    /// or `None` when it resolves.
    pub issue: Option<&'static str>,
}

/// Render `value` the way a caller supplies it, not the way it is persisted.
///
/// The listing that a CLI reads takes the same stance
/// ([`ravel_core::exposed::listing`]): a default shown as `Vec2(1.0, 2.0)`
/// would teach the user a Rust enum spelling that no command line accepts.
pub fn format_default(value: &ExposedValue) -> String {
    fn floats(components: &[f32]) -> String {
        components
            .iter()
            .map(|component| component.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
    match value {
        ExposedValue::Float(v) => v.to_string(),
        ExposedValue::Int(v) => v.to_string(),
        ExposedValue::Bool(v) => v.to_string(),
        ExposedValue::String(v) => v.clone(),
        ExposedValue::Vec2(v) => floats(&[v.0, v.1]),
        ExposedValue::Vec3(v) => floats(&[v.0, v.1, v.2]),
        ExposedValue::Vec4(v) => floats(&[v.0, v.1, v.2, v.3]),
        ExposedValue::Color(c) => floats(&[c.r, c.g, c.b, c.a]),
        ExposedValue::Media(path) => path.to_string(),
    }
}

/// The rows for `declarations`, with `issues` already resolved against the
/// document they came from.
///
/// Split out from [`exposed_section`] so a caller holding declarations that are
/// not part of a document — a subgraph template's, EXPO-6 — can render them
/// with no issues rather than pretending they resolve against a document they
/// have never met.
pub fn rows<'a>(
    declarations: impl IntoIterator<Item = &'a ExposedParameter>,
    issues: &[(String, &'static str)],
) -> Vec<ExposedRow> {
    declarations
        .into_iter()
        .map(|declaration| ExposedRow {
            name: declaration.name().to_string(),
            value_type: declaration.value_type().to_string(),
            default: format_default(declaration.default_value()),
            description: declaration.description().to_string(),
            issue: issues
                .iter()
                .find(|(name, _)| name == declaration.name())
                .map(|(_, key)| *key),
        })
        .collect()
}

/// The project's declarations section: one [`PropertyField::ExposedList`]
/// holding every declaration in presentation order.
///
/// The section exists even when the project declares nothing, so the panel has
/// somewhere to say so rather than falling back to the generic "nothing
/// selected" state.
pub fn exposed_section(document: &Document) -> PropertySection {
    // One `resolve` pass for the whole list. The first issue reported for a
    // name is the one shown: `resolve` reports at most one per declaration
    // today, and taking the first keeps that true if it ever reports more.
    let mut issues: Vec<(String, &'static str)> = Vec::new();
    for issue in resolve(document) {
        if !issues.iter().any(|(name, _)| name == &issue.name) {
            issues.push((issue.name.clone(), issue_key(&issue.reason)));
        }
    }
    PropertySection {
        title: SECTION_EXPOSED.into(),
        fields: vec![PropertyField::ExposedList {
            key: FIELD_EXPOSED.into(),
            rows: rows(&document.exposed_parameters, &issues),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{Composition, Layer};
    use ravel_core::exposed::{ExposedBinding, ExposedParameters};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{CompId, DataTypeId, LayerId, NodeId};
    use ravel_core::types::{FrameRate, Vec2};

    fn title() -> NodeId {
        NodeId::new(1)
    }

    /// A document whose one node holds `text`, with `declarations` over it.
    fn document(declarations: ExposedParameters) -> Document {
        let network = Graph::new()
            .add_node(
                Node::new(title(), "test")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("text", ParameterValue::String("Ravel".into())),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Title", network).with_time(0, 0, 100));
        Document::default()
            .with_composition(comp)
            .with_exposed_parameters(declarations)
    }

    fn declaration(name: &str, default: ExposedValue, key: &str) -> ExposedParameter {
        ExposedParameter::inferred(name, default, ExposedBinding::new(title(), key)).unwrap()
    }

    fn list(section: &PropertySection) -> &[ExposedRow] {
        match &section.fields[0] {
            PropertyField::ExposedList { rows, .. } => rows,
            other => panic!("the section holds one list, found {other:?}"),
        }
    }

    #[test]
    fn a_project_without_declarations_still_has_a_section() {
        let section = exposed_section(&document(ExposedParameters::new()));
        assert_eq!(section.title, SECTION_EXPOSED);
        assert!(list(&section).is_empty());
    }

    #[test]
    fn a_row_carries_the_declaration_in_the_spelling_a_caller_uses() {
        let section = exposed_section(&document(
            ExposedParameters::from_declarations([declaration(
                "headline",
                ExposedValue::String("Ravel".into()),
                "text",
            )
            .with_description("The title card's text")])
            .unwrap(),
        ));
        assert_eq!(
            list(&section),
            [ExposedRow {
                name: "headline".into(),
                value_type: "string".into(),
                default: "Ravel".into(),
                description: "The title card's text".into(),
                issue: None,
            }]
        );
    }

    #[test]
    fn rows_keep_the_presentation_order() {
        let section = exposed_section(&document(
            ExposedParameters::from_declarations([
                declaration("b", ExposedValue::String("x".into()), "text"),
                declaration("a", ExposedValue::String("y".into()), "text"),
            ])
            .unwrap(),
        ));
        assert_eq!(
            list(&section)
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    /// The panel never decides this for itself: an unreachable binding is
    /// whatever `resolve` says it is, named by the key for that reason.
    #[test]
    fn an_unresolved_declaration_carries_the_core_s_reason() {
        let section = exposed_section(&document(
            ExposedParameters::from_declarations([
                // The node is there, the key is not.
                declaration("missing_key", ExposedValue::String("x".into()), "absent"),
                // Neither is there.
                ExposedParameter::inferred(
                    "missing_node",
                    ExposedValue::String("x".into()),
                    ExposedBinding::new(NodeId::new(404), "text"),
                )
                .unwrap(),
                // The key is there and holds something else.
                declaration("wrong_kind", ExposedValue::Vec2(Vec2(0.0, 0.0)), "text"),
            ])
            .unwrap(),
        ));
        assert_eq!(
            list(&section)
                .iter()
                .map(|row| (row.name.as_str(), row.issue))
                .collect::<Vec<_>>(),
            [
                ("missing_key", Some(ISSUE_PARAMETER_MISSING)),
                ("missing_node", Some(ISSUE_NODE_MISSING)),
                ("wrong_kind", Some(ISSUE_KIND_MISMATCH)),
            ]
        );
    }

    #[test]
    fn defaults_read_as_a_caller_would_write_them() {
        assert_eq!(format_default(&ExposedValue::Float(1.5)), "1.5");
        assert_eq!(format_default(&ExposedValue::Int(-2)), "-2");
        assert_eq!(format_default(&ExposedValue::Bool(true)), "true");
        assert_eq!(format_default(&ExposedValue::String("a, b".into())), "a, b");
        assert_eq!(format_default(&ExposedValue::Vec2(Vec2(1.0, 2.0))), "1, 2");
        assert_eq!(
            format_default(&ExposedValue::Media(
                ravel_core::composition::AssetPath::Relative("./a.mov".into())
            )),
            "./a.mov"
        );
    }

    #[test]
    fn every_issue_reason_names_a_distinct_key() {
        let keys = [
            issue_key(&BindingIssueReason::NodeMissing),
            issue_key(&BindingIssueReason::ParameterMissing),
            issue_key(&BindingIssueReason::KindMismatch {
                declared: ravel_core::exposed::ExposedType::Float,
                parameter_kind: "string",
            }),
            issue_key(&BindingIssueReason::AnimatedComponents {
                components: vec![0],
            }),
            issue_key(&BindingIssueReason::NotAMediaNode {
                type_key: "test".into(),
            }),
            issue_key(&BindingIssueReason::NotAnAssetReference {
                expected: "asset_id",
            }),
        ];
        let mut unique = keys.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "each reason needs its own key");
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `text.font` — a font family, weight, and style resolved to one face
//! (REQ-MOGRAPH-004).
//!
//! The selection itself lives in [`ravel_core::text`], which owns the face
//! index and the caches; this is the node wrapper around it. The only thing
//! the processor decides is that a font is **never** an evaluation failure: a
//! family the machine does not have resolves to the built-in face with
//! [`FontRef::is_fallback`] set, so opening a project authored elsewhere
//! renders text in the wrong font instead of failing the graph
//! (`docs/implementation/typography-plan.md`, unit 1).

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::text::{self, FontQuery};
use ravel_core::types::NodeData;
use std::sync::Arc;

/// Resolves the node's `family` / `weight` / `style` parameters to a face.
///
/// Stateless: the shared [`text::FontLibrary`] holds the index and both
/// caches, so a parameter edit is a dirty mark rather than a rebuild, and two
/// `text.font` nodes asking for one family share the same bytes.
pub struct FontProcessor;

impl FontProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for FontProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let query = FontQuery::new(
            params.str_or("family", text::DEFAULT_FAMILY),
            text::weight_from_name(params.str_or("weight", "regular")),
            text::style_is_italic(params.str_or("style", "normal")),
        );
        Ok(text::shared().resolve(&query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::registry::{NodeRegistry, builtin::register_builtins};
    use ravel_core::text::{DEFAULT_FAMILY, FontRef};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    /// The node as the registry builds it, with `family` overridden. Going
    /// through the template rather than hand-building a node keeps the
    /// declared output type and the parameter keys in the test's path.
    fn font_node(family: &str) -> Node {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let mut node = registry
            .create_node("text.font", NodeId::new(1))
            .expect("text.font is registered");
        set_param(&mut node, "family", family);
        node
    }

    /// Overwrite a template parameter in place.
    ///
    /// Not `Node::with_param`: that one *appends*, and a resolved lookup reads
    /// the first entry for a key — so an appended override is silently ignored
    /// and every assertion below would be made against the template default.
    fn set_param(node: &mut Node, key: &str, value: &str) {
        let param = node
            .parameters
            .iter_mut()
            .find(|param| param.key == key)
            .unwrap_or_else(|| panic!("the template declares no {key} parameter"));
        param.value = ParameterValue::String(value.into());
    }

    fn evaluate(family: &str) -> anyhow::Result<Arc<dyn NodeData>> {
        let node = font_node(family);
        let graph = Graph::new().add_node(node)?;
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(FontProcessor));
        Ok(evaluator.evaluate(&graph, NodeId::new(1), &ctx())?)
    }

    #[test]
    fn the_template_declares_a_font_output() {
        let node = font_node(DEFAULT_FAMILY);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].data_type, DataTypeId::FONT);
    }

    #[test]
    fn an_installed_family_evaluates_to_its_face() {
        let value = evaluate(DEFAULT_FAMILY).expect("an installed family must resolve");
        let font = value
            .downcast_ref::<FontRef>()
            .expect("text.font produces a FontRef");
        assert_eq!(font.family, DEFAULT_FAMILY);
        assert!(!font.is_fallback);
    }

    /// The completion criterion of typography-plan unit 1: an unresolved
    /// family is a warning and a fallback face, **not** an `Err`. A render of
    /// someone else's project must not stop at a font that is not installed.
    #[test]
    fn an_unresolved_family_evaluates_to_a_fallback_rather_than_an_error() {
        let value = evaluate("No Such Family ZZZ")
            .expect("an unresolved family must not fail the evaluation");
        let font = value
            .downcast_ref::<FontRef>()
            .expect("text.font produces a FontRef even when it falls back");
        assert!(font.is_fallback, "the fallback has to be reported as one");
        assert!(
            font.data.len() > 1024,
            "the fallback has to carry usable font bytes"
        );
    }

    /// Weight and style values that are not dropdown options — a hand-edited
    /// document — resolve to the defaults instead of failing.
    #[test]
    fn unknown_weight_and_style_values_fall_back_to_the_defaults() {
        let mut node = font_node(DEFAULT_FAMILY);
        set_param(&mut node, "weight", "chunky");
        set_param(&mut node, "style", "sideways");
        let graph = Graph::new().add_node(node).expect("a single-node graph");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(FontProcessor));
        let value = evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("a nonsense weight must not fail the evaluation");
        let font = value
            .downcast_ref::<FontRef>()
            .expect("text.font produces a FontRef");
        assert_eq!(font.weight, 400);
        assert!(!font.italic);
    }
}

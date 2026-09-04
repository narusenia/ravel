// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `text.*` node processors (REQ-MOGRAPH-004).
//!
//! `text.font` resolves a family, weight, and style to one face;
//! `text.layout` shapes a string in that face into one instance per
//! character; `text.to_path` flattens those instances into one geometry of
//! outline paths, which is what puts the letter shapes themselves within
//! reach of a Point-domain field.
//!
//! The selection itself lives in [`ravel_core::text`], which owns the face
//! index and the caches; this is the node wrapper around it. The only thing
//! the processor decides is that a font is **never** an evaluation failure: a
//! family the machine does not have resolves to the built-in face with
//! [`FontRef::is_fallback`] set, so opening a project authored elsewhere
//! renders text in the wrong font instead of failing the graph
//! (`docs/implementation/typography-plan.md`, unit 1).

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{Geometry, ops};
use ravel_core::graph::Node;
use ravel_core::text::{self, Align, FontQuery, FontRef, LayoutParams, VerticalAnchor};
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

/// Shapes the node's `text` in the face on its `font` input and produces the
/// per-character instance geometry (typography-plan unit 2).
///
/// Stateless for the same reason [`FontProcessor`] is: the parameters arrive
/// resolved per frame, so editing the string is a dirty mark rather than a
/// rebuilt processor.
///
/// An unconnected `font` input is **not** an error. It resolves the default
/// family — the same face a fresh `text.font` node answers with — so dropping
/// a `text.layout` into a graph and typing shows text immediately, and adding
/// a `text.font` later only changes which face it is. The same reasoning as
/// unit 1's fallback: nothing about a font may stop an evaluation.
pub struct LayoutProcessor;

impl LayoutProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for LayoutProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let default_font;
        let font = match inputs
            .first()
            .and_then(Option::as_ref)
            .and_then(|input| input.downcast_ref::<FontRef>())
        {
            Some(font) => font,
            None => {
                default_font = text::shared().resolve(&FontQuery::new(
                    text::DEFAULT_FAMILY,
                    text::weight_from_name("regular"),
                    false,
                ));
                &default_font
            }
        };
        let layout = LayoutParams {
            size: params.f32_or("size", text::DEFAULT_SIZE),
            tracking: params.f32_or("tracking", 0.0),
            leading: params.f32_or("leading", 0.0),
            align: Align::from_name(params.str_or("align", text::TEXT_ALIGNS[0])),
            wrap_width: params.f32_or("wrap_width", 0.0),
            anchor: VerticalAnchor::from_name(params.str_or("anchor", text::TEXT_ANCHORS[0])),
        };
        let geometry = text::layout_text(font, params.str_or("text", ""), &layout)
            .with_context(|| format!("laying text out in {}", font.family))?;
        Ok(Arc::new(geometry))
    }
}

/// Flattens the character instances on its input into one geometry of
/// outline paths (typography-plan unit 5).
///
/// The whole node is [`ops::expand_instances`]: each character's `P` / `rot`
/// / `scale` is baked into its outline points, the per-character attributes
/// descend onto the Point and Primitive domains, and the bezier tangents are
/// carried through as the differences they are. What that buys is the
/// acceptance criterion "the converted geometry is affected by fields" —
/// `field.apply` on the **Point** domain now reaches the control points of
/// the letters instead of the character origins.
///
/// Stateless, like the other two `text.*` processors: there is nothing to
/// read off the node.
pub struct ToPathProcessor;

impl ToPathProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ToPathProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // An unconnected input is an empty geometry rather than an error: a
        // node the user has just dropped in has nothing to convert yet, and
        // failing the evaluation would blank the whole frame instead of that
        // one branch.
        let Some(geometry) = inputs
            .first()
            .and_then(Option::as_ref)
            .map(|input| {
                input
                    .downcast_ref::<Geometry>()
                    .ok_or_else(|| anyhow::anyhow!("text.to_path: input 0 is not Geometry"))
            })
            .transpose()?
        else {
            return Ok(Arc::new(Geometry::new()));
        };
        Ok(Arc::new(
            ops::expand_instances(geometry).context("converting a text layout to paths")?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{Geometry, names};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::{NodeRegistry, builtin::register_builtins};
    use ravel_core::text::DEFAULT_FAMILY;
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

    /// Overwrite a template float parameter in place, for the same reason
    /// [`set_param`] exists.
    fn set_float(node: &mut Node, key: &str, value: f32) {
        let param = node
            .parameters
            .iter_mut()
            .find(|param| param.key == key)
            .unwrap_or_else(|| panic!("the template declares no {key} parameter"));
        param.value = ParameterValue::Float(value);
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

    // -----------------------------------------------------------------------
    // text.layout
    // -----------------------------------------------------------------------

    /// A `text.layout` node as the registry builds it, with `text` set.
    fn layout_node(id: u64, text: &str) -> Node {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let mut node = registry
            .create_node("text.layout", NodeId::new(id))
            .expect("text.layout is registered");
        set_param(&mut node, "text", text);
        node
    }

    #[test]
    fn the_layout_template_declares_a_font_input_and_a_geometry_output() {
        let node = layout_node(1, "");
        assert_eq!(node.inputs.len(), 1);
        assert_eq!(node.inputs[0].accepted_types, vec![DataTypeId::FONT]);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].data_type, DataTypeId::GEOMETRY);
    }

    /// The wiring the whole unit exists for: a `text.font` feeding a
    /// `text.layout` produces one instance per character, with the glyph
    /// outlines as instance sources.
    #[test]
    fn a_font_node_feeding_a_layout_node_produces_character_instances() {
        let font = font_node(DEFAULT_FAMILY);
        let layout = layout_node(2, "Ravel");
        let graph = Graph::new()
            .add_node(font)
            .expect("the font node")
            .add_node(layout)
            .expect("the layout node")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("font connects to layout");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(FontProcessor));
        evaluator.register(NodeId::new(2), Arc::new(LayoutProcessor));
        let value = evaluator
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the graph evaluates");
        let geometry = value
            .downcast_ref::<Geometry>()
            .expect("text.layout produces geometry");
        assert_eq!(geometry.instance_count(), 5);
        assert_eq!(
            geometry.sources().len(),
            5,
            "`Ravel` has five distinct characters"
        );
        assert!(
            geometry
                .instances()
                .get(names::CHAR_PROGRESS)
                .is_some_and(|column| column.len() == 5),
            "the per-character attributes have to reach the output"
        );
    }

    /// An unconnected `font` input resolves the default family rather than
    /// failing, so a `text.layout` dropped on its own shows text.
    #[test]
    fn an_unconnected_font_input_still_lays_text_out() {
        let node = layout_node(1, "ab");
        let graph = Graph::new().add_node(node).expect("a single-node graph");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(LayoutProcessor));
        let value = evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("an unconnected font must not fail the evaluation");
        let geometry = value
            .downcast_ref::<Geometry>()
            .expect("text.layout produces geometry");
        assert_eq!(geometry.instance_count(), 2);
    }

    /// The parameters have to be read from the node rather than defaulted:
    /// changing `size` has to move the characters.
    #[test]
    fn the_layout_parameters_reach_the_geometry() {
        let advance = |size: f32| {
            let mut node = layout_node(1, "ab");
            set_float(&mut node, "size", size);
            let graph = Graph::new().add_node(node).expect("a single-node graph");
            let mut evaluator = Evaluator::new();
            evaluator.register(NodeId::new(1), Arc::new(LayoutProcessor));
            let value = evaluator
                .evaluate(&graph, NodeId::new(1), &ctx())
                .expect("the graph evaluates");
            value
                .downcast_ref::<Geometry>()
                .expect("geometry")
                .instances()
                .get(names::ADVANCE)
                .expect("advance")
                .as_f32(names::ADVANCE)
                .expect("an F32 column")[0]
        };
        let small = advance(20.0);
        let large = advance(80.0);
        assert!(
            (large / small - 4.0).abs() < 0.01,
            "advance has to scale with size: {small} then {large}"
        );
    }

    // -----------------------------------------------------------------------
    // text.to_path
    // -----------------------------------------------------------------------

    /// A `text.to_path` node as the registry builds it.
    fn to_path_node(id: u64) -> Node {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        registry
            .create_node("text.to_path", NodeId::new(id))
            .expect("text.to_path is registered")
    }

    #[test]
    fn the_to_path_template_declares_a_geometry_input_and_a_geometry_output() {
        let node = to_path_node(1);
        assert_eq!(node.inputs.len(), 1);
        assert_eq!(node.inputs[0].accepted_types, vec![DataTypeId::GEOMETRY]);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].data_type, DataTypeId::GEOMETRY);
        assert!(
            node.parameters.is_empty(),
            "there is nothing to decide about a conversion"
        );
    }

    /// `text.layout -> text.to_path`: the character instances become one
    /// geometry of outline paths, with the per-character attributes on the
    /// Point domain where a field can read them.
    #[test]
    fn a_layout_node_feeding_to_path_produces_one_outline_geometry() {
        let layout = layout_node(1, "Ravel");
        let to_path = to_path_node(2);
        let graph = Graph::new()
            .add_node(layout)
            .expect("the layout node")
            .add_node(to_path)
            .expect("the to_path node")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("layout connects to to_path");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(LayoutProcessor));
        evaluator.register(NodeId::new(2), Arc::new(ToPathProcessor));

        let laid_out = evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("the layout evaluates");
        let laid_out = laid_out
            .downcast_ref::<Geometry>()
            .expect("text.layout produces geometry");
        // The point count the conversion has to reproduce: every outline
        // point of every character, counted from the sources the layout
        // shares between repeated glyphs.
        let source_indices = laid_out
            .instances()
            .get(names::SOURCE_INDEX)
            .expect("the layout writes source_index")
            .as_i32(names::SOURCE_INDEX)
            .expect("an I32 column")
            .to_vec();
        let expected_points: usize = source_indices
            .iter()
            .map(|index| {
                laid_out.sources()[*index as usize]
                    .geometry()
                    .expect("a glyph outline")
                    .point_count()
            })
            .sum();
        assert!(expected_points > 0, "`Ravel` has ink");

        let value = evaluator
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the conversion evaluates");
        let paths = value
            .downcast_ref::<Geometry>()
            .expect("text.to_path produces geometry");
        assert_eq!(paths.point_count(), expected_points);
        assert_eq!(paths.instance_count(), 0, "the answer is flat geometry");
        for name in [
            names::CHAR_INDEX,
            names::WORD_INDEX,
            names::LINE_INDEX,
            names::CHAR_PROGRESS,
            names::ADVANCE,
        ] {
            assert!(
                paths
                    .points()
                    .get(name)
                    .is_some_and(|column| column.len() == expected_points),
                "{name} has to reach every outline point"
            );
        }
        // Curves stay curves: the tangents unit 2 wrote are still there.
        assert!(
            paths.points().get(names::IN_TAN).is_some(),
            "the bezier tangents have to survive the conversion"
        );
    }

    /// An unconnected input is an empty geometry, not an error: a node just
    /// dropped into a graph must not blank the frame.
    #[test]
    fn an_unconnected_input_converts_to_an_empty_geometry() {
        let graph = Graph::new()
            .add_node(to_path_node(1))
            .expect("a single-node graph");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(ToPathProcessor));
        let value = evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("an unconnected input must not fail the evaluation");
        let geometry = value
            .downcast_ref::<Geometry>()
            .expect("text.to_path produces geometry");
        assert_eq!(geometry.point_count(), 0);
        assert_eq!(geometry.instance_count(), 0);
    }
}

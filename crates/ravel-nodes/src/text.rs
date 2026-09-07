// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `text.*` node processors (REQ-MOGRAPH-004).
//!
//! `text.font` resolves a family, weight, and style to one face;
//! `text.layout` shapes a string in that face into one instance per
//! character; `text.on_path` re-places those instances along a path;
//! `text.to_path` flattens them into one geometry of outline paths, which is
//! what puts the letter shapes themselves within reach of a Point-domain
//! field.
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
use ravel_core::geometry::{AttributeArray, Domain, Geometry, names, ops};
use ravel_core::graph::Node;
use ravel_core::text::{
    self, Align, FontQuery, FontRef, LayoutParams, VerticalAnchor, WritingMode,
};
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
            writing_mode: WritingMode::from_name(
                params.str_or("writing_mode", text::TEXT_WRITING_MODES[0]),
            ),
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

/// Where the whole run of characters sits along the path
/// ([`OnPathProcessor`]'s `align`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathAlign {
    /// The run starts at `offset` along the path.
    Start,
    /// The middle of the run meets the middle of the path.
    Center,
    /// The end of the run meets the end of the path.
    End,
}

impl PathAlign {
    fn from_name(name: &str) -> Self {
        match name {
            "center" => Self::Center,
            "end" => Self::End,
            // `start`, and anything else: the parameter is a dropdown, so an
            // unrecognised value means a project from another build rather
            // than a reason to fail the frame.
            _ => Self::Start,
        }
    }
}

/// Re-places the character instances on its `text` input along the path on
/// its `path` input (typography-plan unit 4).
///
/// **Only `P` and `rot` are rewritten.** The glyph outlines in
/// `instance_sources`, the `source_index` that addresses them, and every
/// per-character attribute (`char_index` / `word_index` / `line_index` /
/// `char_progress` / `advance`) come through untouched, so the field
/// modulation path is the same before and after the node.
///
/// # The arc-length coordinate
///
/// A character's place on the path is the **running sum of the `advance`
/// column**, not a component of its `P`. `advance` is documented as the pen
/// step along the writing axis whichever way the text runs
/// (`names::ADVANCE`), while `P` splits into a writing-axis component and a
/// cross-axis one that swap places with the writing mode: `P.x` is the pen in
/// horizontal text but the *column* coordinate in vertical text, so keying
/// off it would stack every character of a vertical run at one arc length.
/// Reading `advance` needs no writing mode and works for both.
///
/// Two consequences worth naming. `text.layout`'s own `align` / `anchor`
/// drop out — the run always begins at the path's start plus `offset`, and
/// this node's `align` is what decides where it sits. And a multi-line
/// layout becomes **one continuous run** along the path rather than
/// collapsing its lines on top of each other; the cross-axis component of
/// `P`, which is what separated the lines, has no place on a path.
///
/// # Running off the end
///
/// Arc lengths are clamped to the path (`PathArcTable::sample`), so a run
/// longer than the path piles its remaining characters up on the final
/// point instead of extrapolating past it or dropping them.
pub struct OnPathProcessor;

impl OnPathProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for OnPathProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(text) = optional_geometry(inputs, 0, "text.on_path")? else {
            return Ok(Arc::new(Geometry::new()));
        };
        // An unconnected `path` passes the text through unchanged rather
        // than failing: the same reason the other `text.*` processors give
        // for tolerating a missing input, which is that a node the user has
        // just dropped in must not blank the frame.
        let Some(path) = optional_geometry(inputs, 1, "text.on_path")? else {
            return Ok(Arc::new(text.clone()));
        };
        if text.instance_count() == 0 {
            return Ok(Arc::new(text.clone()));
        }
        // A 3D placement has no meaning against a planar path's frame, so it
        // is refused the way `path_sample` refuses a 3D path rather than
        // having `P` quietly demoted to `Vec2` on the way out.
        text.positions(Domain::Instance)
            .context("text.on_path: the instance domain has no P")??
            .require_planar("text.on_path")?;
        let advances = text
            .instances()
            .get(names::ADVANCE)
            .context(
                "text.on_path: input 0 has no `advance` column — \
                 the text input wants a `text.layout`",
            )?
            .as_f32(names::ADVANCE)?;

        // Built once, outside the loop below: the walk is over every path
        // vertex, so a `path_sample` call per character would be
        // O(characters x vertices).
        let table = ops::PathArcTable::build(path, "text.on_path")?;
        let spacing = params.f32_or("spacing", 0.0);
        // Where each character starts, and — after the loop — the run's
        // total pen length. `spacing` follows every character including the
        // last, the way `text.layout`'s `tracking` does, so the run's length
        // and a character's step stay the same quantity.
        let mut starts = Vec::with_capacity(advances.len());
        let mut span = 0.0;
        for advance in advances {
            starts.push(span);
            span += advance + spacing;
        }
        let base = params.f32_or("offset", 0.0)
            + match PathAlign::from_name(params.str_or("align", "start")) {
                PathAlign::Start => 0.0,
                PathAlign::Center => (table.length() - span) / 2.0,
                PathAlign::End => table.length() - span,
            };
        // Turning the tangent 180 degrees is what puts the characters on the
        // other side of the path: it both flips them over and swaps which
        // way the normal points.
        let flip = if params.bool_or("flip", false) {
            std::f32::consts::PI
        } else {
            0.0
        };

        let mut positions = Vec::with_capacity(starts.len());
        let mut rotations = Vec::with_capacity(starts.len());
        for start in starts {
            let sample = table.sample(base + start);
            positions.push(sample.position);
            rotations.push(sample.tangent.1.atan2(sample.tangent.0) + flip);
        }

        let mut result = text.clone();
        let instances = result.instances_mut();
        instances.insert(names::P, AttributeArray::Vec2(positions))?;
        instances.insert(names::ROT, AttributeArray::F32(rotations))?;
        Ok(Arc::new(result))
    }
}

/// The `Geometry` on `inputs[index]`, or `None` when nothing is connected.
///
/// Distinguishes "not connected" from "connected to the wrong type": the
/// first is a normal state of a half-built graph, the second is a bug the
/// caller wants to hear about.
fn optional_geometry<'a>(
    inputs: &'a [Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<Option<&'a Geometry>> {
    inputs
        .get(index)
        .and_then(Option::as_ref)
        .map(|input| {
            input
                .downcast_ref::<Geometry>()
                .ok_or_else(|| anyhow::anyhow!("{processor}: input {index} is not Geometry"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{Geometry, Primitive, names};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::{NodeRegistry, builtin::register_builtins};
    use ravel_core::text::DEFAULT_FAMILY;
    use ravel_core::types::{FrameRate, Vec2};

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

    /// `writing_mode` has to be read off the node, not defaulted: a vertical
    /// layout steps in Y and a horizontal one in X. Nothing in the layout
    /// tests would notice an unwired parameter — they build `LayoutParams`
    /// themselves — so the wiring gets its own assertion here.
    #[test]
    fn the_writing_mode_parameter_reaches_the_layout() {
        let step = |mode: &str| {
            let mut node = layout_node(1, "ab");
            set_param(&mut node, "writing_mode", mode);
            let graph = Graph::new().add_node(node).expect("a single-node graph");
            let mut evaluator = Evaluator::new();
            evaluator.register(NodeId::new(1), Arc::new(LayoutProcessor));
            let value = evaluator
                .evaluate(&graph, NodeId::new(1), &ctx())
                .expect("the graph evaluates");
            let placed = value
                .downcast_ref::<Geometry>()
                .expect("geometry")
                .instances()
                .get(names::P)
                .expect("the instance domain carries P")
                .as_vec2(names::P)
                .expect("a Vec2 column")
                .to_vec();
            (placed[1].0 - placed[0].0, placed[1].1 - placed[0].1)
        };

        let (dx, dy) = step("horizontal");
        assert!(
            dx > 0.0 && dy.abs() < 1e-4,
            "horizontal text steps in X: {dx}, {dy}"
        );
        let (dx, dy) = step("vertical");
        assert!(
            dy > 0.0 && dx.abs() < 1e-4,
            "vertical text steps in Y: {dx}, {dy}"
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

    // -----------------------------------------------------------------------
    // text.on_path
    // -----------------------------------------------------------------------

    /// Overwrite a template bool parameter in place, for the same reason
    /// [`set_param`] exists.
    fn set_bool(node: &mut Node, key: &str, value: bool) {
        let param = node
            .parameters
            .iter_mut()
            .find(|param| param.key == key)
            .unwrap_or_else(|| panic!("the template declares no {key} parameter"));
        param.value = ParameterValue::Bool(value);
    }

    /// A source node that hands the same geometry out every frame, so a
    /// hand-built path can be an *input* rather than something the processor
    /// is called with directly — the parameter resolution and the port
    /// declarations stay in the test's path that way.
    struct Fixed(Arc<Geometry>);

    impl NodeProcessor for Fixed {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(self.0.clone())
        }
    }

    /// An open path from the origin along +X, `length` long.
    fn straight_path(length: f32) -> Geometry {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(length, 0.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geometry
    }

    /// A quarter circle of `radius` about the origin as `steps` chords,
    /// walked anticlockwise from `(radius, 0)`.
    fn arc_path(radius: f32, steps: usize) -> Geometry {
        let points = (0..=steps)
            .map(|step| {
                let angle = std::f32::consts::FRAC_PI_2 * step as f32 / steps as f32;
                Vec2(radius * angle.cos(), radius * angle.sin())
            })
            .collect::<Vec<_>>();
        let mut geometry = Geometry::from_points(points);
        geometry.push_primitive(Primitive::Path {
            verts: 0..steps + 1,
            closed: false,
        });
        geometry
    }

    /// `text.layout(text)` feeding a `text.on_path` whose second input is
    /// `path`, with `tweak` applied to the on_path node's parameters.
    ///
    /// Returns the layout's own geometry alongside the placed one, so a test
    /// can state its expectation in terms of the advances the layout
    /// actually produced rather than a hard-coded metric of the bundled face.
    fn on_path(
        text: &str,
        path: Option<Geometry>,
        tweak: impl FnOnce(&mut Node),
    ) -> (Geometry, Geometry) {
        on_path_of(text, path, |_| {}, tweak)
    }

    /// [`on_path`] with the `text.layout` node's parameters open to a tweak
    /// too, for the tests that need a particular writing mode.
    fn on_path_of(
        text: &str,
        path: Option<Geometry>,
        layout_tweak: impl FnOnce(&mut Node),
        tweak: impl FnOnce(&mut Node),
    ) -> (Geometry, Geometry) {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let mut node = registry
            .create_node("text.on_path", NodeId::new(2))
            .expect("text.on_path is registered");
        tweak(&mut node);
        let mut layout = layout_node(1, text);
        layout_tweak(&mut layout);
        let mut graph = Graph::new()
            .add_node(layout)
            .expect("the layout node")
            .add_node(node)
            .expect("the on_path node")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("layout connects to on_path");
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(LayoutProcessor));
        evaluator.register(NodeId::new(2), Arc::new(OnPathProcessor));
        if let Some(path) = path {
            graph = graph
                .add_node(
                    Node::new(NodeId::new(3), "test.path")
                        .with_output("output", DataTypeId::GEOMETRY),
                )
                .expect("the path source")
                .add_edge(
                    EdgeId::new(2),
                    NodeId::new(3),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(1),
                )
                .expect("the path connects to on_path");
            evaluator.register(NodeId::new(3), Arc::new(Fixed(Arc::new(path))));
        }
        let laid_out = evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("the layout evaluates");
        let placed = evaluator
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the placement evaluates");
        (
            laid_out
                .downcast_ref::<Geometry>()
                .expect("text.layout produces geometry")
                .clone(),
            placed
                .downcast_ref::<Geometry>()
                .expect("text.on_path produces geometry")
                .clone(),
        )
    }

    fn placements(geometry: &Geometry) -> Vec<Vec2> {
        geometry
            .instances()
            .get(names::P)
            .expect("the instance domain carries P")
            .as_vec2(names::P)
            .expect("a Vec2 column")
            .to_vec()
    }

    fn instance_floats(geometry: &Geometry, name: &str) -> Vec<f32> {
        geometry
            .instances()
            .get(name)
            .unwrap_or_else(|| panic!("the instance domain carries {name}"))
            .as_f32(name)
            .expect("an F32 column")
            .to_vec()
    }

    /// The run's total pen length, `spacing` included after every character:
    /// what `align` positions along the path.
    fn run_span(laid_out: &Geometry, spacing: f32) -> f32 {
        instance_floats(laid_out, names::ADVANCE)
            .iter()
            .map(|advance| advance + spacing)
            .sum()
    }

    #[test]
    fn the_on_path_template_declares_two_geometry_inputs_and_four_parameters() {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let node = registry
            .create_node("text.on_path", NodeId::new(1))
            .expect("text.on_path is registered");
        assert_eq!(node.inputs.len(), 2);
        for input in &node.inputs {
            assert_eq!(input.accepted_types, vec![DataTypeId::GEOMETRY]);
        }
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].data_type, DataTypeId::GEOMETRY);
        let keys: Vec<&str> = node
            .parameters
            .iter()
            .map(|param| param.key.as_str())
            .collect();
        assert_eq!(keys, ["offset", "spacing", "align", "flip"]);
    }

    /// The completion criterion "a straight path reproduces the plain
    /// placement": along +X from the origin, every character lands exactly
    /// where `text.layout` put it, unrotated.
    #[test]
    fn a_straight_path_reproduces_the_plain_layout() {
        let (laid_out, placed) = on_path("Ravel", Some(straight_path(2000.0)), |_| {});
        let expected = placements(&laid_out);
        let actual = placements(&placed);
        assert_eq!(actual.len(), 5);
        for (index, (want, got)) in expected.iter().zip(&actual).enumerate() {
            assert!(
                (want.0 - got.0).abs() < 1e-3 && got.1.abs() < 1e-3,
                "character {index}: laid out at {want:?}, placed at {got:?}"
            );
        }
        for (index, rot) in instance_floats(&placed, names::ROT).iter().enumerate() {
            assert!(rot.abs() < 1e-4, "character {index} turned by {rot}");
        }
    }

    /// The completion criterion "`rot` follows the tangent on an arc": every
    /// character sits on the circle and faces along it, checked against the
    /// circle rather than against another call of the same sampler — the
    /// radius is perpendicular to the tangent, so a `normal`-for-`tangent`
    /// slip or a quarter-turn shows up as a non-zero dot product.
    #[test]
    fn an_arc_path_turns_every_character_to_the_tangent() {
        let radius = 400.0;
        let (_, placed) = on_path("Ravel", Some(arc_path(radius, 256)), |_| {});
        let rotations = instance_floats(&placed, names::ROT);
        for (index, (place, rot)) in placements(&placed).iter().zip(&rotations).enumerate() {
            let distance = (place.0 * place.0 + place.1 * place.1).sqrt();
            assert!(
                (distance - radius).abs() < 0.5,
                "character {index} left the circle: {distance}"
            );
            let facing = (rot.cos() * place.0 + rot.sin() * place.1) / radius;
            assert!(
                facing.abs() < 0.01,
                "character {index} is not tangent to the circle: {facing}"
            );
            // Anticlockwise, so the heading grows along the run.
            if index > 0 {
                assert!(
                    *rot > rotations[index - 1],
                    "character {index} turned backwards"
                );
            }
        }
    }

    /// A path shorter than the run clamps: the characters that do not fit
    /// pile up on the path's last point instead of extrapolating past it or
    /// disappearing.
    #[test]
    fn a_path_shorter_than_the_text_clamps_to_its_end() {
        let (laid_out, placed) = on_path("Ravel", Some(straight_path(120.0)), |_| {});
        assert!(
            run_span(&laid_out, 0.0) > 120.0,
            "the fixture only means anything with a run longer than the path"
        );
        let actual = placements(&placed);
        assert_eq!(actual.len(), 5);
        assert!(actual[0].0 < 1e-3, "the run still starts at the path start");
        assert!(
            (actual.last().expect("five characters").0 - 120.0).abs() < 1e-3,
            "the overflowing characters sit on the path's end: {actual:?}"
        );
        assert!(
            actual.windows(2).all(|pair| pair[0].0 <= pair[1].0 + 1e-3),
            "clamping must not reorder the run: {actual:?}"
        );
    }

    /// `align` decides where the whole run sits, so the three values have to
    /// be told apart by the run's *total* length — a `center` or `end` that
    /// mismeasured the span would still pass a start-only assertion.
    #[test]
    fn align_places_the_run_at_the_start_the_middle_or_the_end() {
        let length = 2000.0;
        let first_of = |align: &'static str| {
            let (laid_out, placed) = on_path("Ravel", Some(straight_path(length)), |node| {
                set_param(node, "align", align);
            });
            (run_span(&laid_out, 0.0), placements(&placed)[0].0)
        };

        let (span, start) = first_of("start");
        assert!(
            start.abs() < 1e-3,
            "start begins at the path start: {start}"
        );
        let (_, center) = first_of("center");
        assert!(
            (center - (length - span) / 2.0).abs() < 1e-3,
            "center centres the run: {center} against a span of {span}"
        );
        let (_, end) = first_of("end");
        assert!(
            (end - (length - span)).abs() < 1e-3,
            "end lands the run's end on the path's end: {end} against a span of {span}"
        );
    }

    /// `offset` slides the run along the path.
    #[test]
    fn offset_slides_the_run_along_the_path() {
        let (_, plain) = on_path("Ravel", Some(straight_path(2000.0)), |_| {});
        let (_, shifted) = on_path("Ravel", Some(straight_path(2000.0)), |node| {
            set_float(node, "offset", 250.0);
        });
        for (index, (before, after)) in placements(&plain)
            .iter()
            .zip(placements(&shifted))
            .enumerate()
        {
            assert!(
                (after.0 - before.0 - 250.0).abs() < 1e-3,
                "character {index}: {} then {}",
                before.0,
                after.0
            );
        }
    }

    /// `spacing` is tracking measured along the path: it opens the gap
    /// between the characters without moving the first one.
    #[test]
    fn spacing_opens_the_gaps_along_the_path() {
        let (_, plain) = on_path("Ravel", Some(straight_path(2000.0)), |_| {});
        let (_, spaced) = on_path("Ravel", Some(straight_path(2000.0)), |node| {
            set_float(node, "spacing", 30.0);
        });
        for (index, (before, after)) in placements(&plain)
            .iter()
            .zip(placements(&spaced))
            .enumerate()
        {
            assert!(
                (after.0 - before.0 - 30.0 * index as f32).abs() < 1e-3,
                "character {index}: {} then {}",
                before.0,
                after.0
            );
        }
    }

    /// `flip` turns the tangent half a turn, which is what puts the run on
    /// the other side of the path.
    #[test]
    fn flip_turns_every_character_half_a_turn() {
        let (_, plain) = on_path("Ravel", Some(arc_path(400.0, 256)), |_| {});
        let (_, flipped) = on_path("Ravel", Some(arc_path(400.0, 256)), |node| {
            set_bool(node, "flip", true);
        });
        for (index, (before, after)) in instance_floats(&plain, names::ROT)
            .iter()
            .zip(instance_floats(&flipped, names::ROT))
            .enumerate()
        {
            assert!(
                (after - before - std::f32::consts::PI).abs() < 1e-4,
                "character {index}: {before} then {after}"
            );
        }
        assert_eq!(
            placements(&plain),
            placements(&flipped),
            "flipping turns the characters, it does not move them"
        );
    }

    /// An unconnected `path` input passes the text through **unchanged** —
    /// not an empty geometry and not an error, because a node the user has
    /// just dropped in must not blank the frame.
    #[test]
    fn an_unconnected_path_input_passes_the_text_through() {
        let (laid_out, placed) = on_path("Ravel", None, |_| {});
        assert_eq!(placements(&placed), placements(&laid_out));
        assert_eq!(
            instance_floats(&placed, names::ROT),
            instance_floats(&laid_out, names::ROT)
        );
    }

    /// The placement rewrites `P` and `rot` and **nothing else**. The glyph
    /// outlines are what the character *is*, and the per-character columns
    /// are the entry point a field modulates through, so losing either would
    /// break the node while every position assertion above still passed.
    #[test]
    fn the_placement_keeps_the_outlines_and_the_per_character_columns() {
        let (laid_out, placed) = on_path("Ravel one", Some(arc_path(400.0, 256)), |node| {
            set_param(node, "align", "center");
            set_float(node, "offset", 40.0);
        });

        assert_eq!(
            placed.sources().len(),
            laid_out.sources().len(),
            "the glyph outlines have to come through"
        );
        assert!(!placed.sources().is_empty(), "`Ravel one` has ink");
        for (index, (before, after)) in laid_out.sources().iter().zip(placed.sources()).enumerate()
        {
            assert_eq!(
                before.geometry().expect("a glyph outline").point_count(),
                after.geometry().expect("a glyph outline").point_count(),
                "source {index} lost its outline points"
            );
        }

        let int_column = |geometry: &Geometry, name: &str| {
            geometry
                .instances()
                .get(name)
                .unwrap_or_else(|| panic!("the instance domain carries {name}"))
                .as_i32(name)
                .expect("an I32 column")
                .to_vec()
        };
        for name in [
            names::SOURCE_INDEX,
            names::INDEX,
            names::CHAR_INDEX,
            names::WORD_INDEX,
            names::LINE_INDEX,
        ] {
            assert_eq!(
                int_column(&placed, name),
                int_column(&laid_out, name),
                "{name} has to survive the placement"
            );
        }
        for name in [names::CHAR_PROGRESS, names::ADVANCE] {
            assert_eq!(
                instance_floats(&placed, name),
                instance_floats(&laid_out, name),
                "{name} has to survive the placement"
            );
        }
        assert_eq!(
            placed.validate(),
            Ok(()),
            "the placed geometry has to stay well formed"
        );
    }

    /// Vertical text runs *along* the path rather than stacking on one
    /// point: the arc-length coordinate is the `advance` column, which is
    /// the writing-axis step in either mode, and not a component of `P`
    /// (whose x is the *column* coordinate once the text runs downwards).
    #[test]
    fn vertical_text_runs_along_the_path_too() {
        let (_, placed) = on_path_of(
            "Ravel",
            Some(straight_path(2000.0)),
            |layout| set_param(layout, "writing_mode", "vertical"),
            |_| {},
        );
        let placed = placements(&placed);
        assert!(
            placed.windows(2).all(|pair| pair[1].0 - pair[0].0 > 1.0),
            "a vertical run has to spread along the path: {placed:?}"
        );
    }

    /// Instance geometry with no `advance` column — a `scatter.*` output
    /// wired in by mistake — is an explicit error naming what the input
    /// wants, not a silent no-op.
    #[test]
    fn instances_without_an_advance_column_are_an_explicit_error() {
        let mut instances = Geometry::new();
        instances
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .expect("the first instance column");
        let node = Node::new(NodeId::new(1), "text.on_path");
        let mut scope = Evaluator::new();
        let error = OnPathProcessor
            .process(
                &node,
                &ctx(),
                &[
                    Some(Arc::new(instances)),
                    Some(Arc::new(straight_path(100.0))),
                ],
                &ResolvedParams::default(),
                &mut scope,
            )
            .err()
            .expect("a geometry with no advance column cannot be placed");
        assert!(
            format!("{error}").contains("advance"),
            "the error has to name the missing column: {error}"
        );
    }
}

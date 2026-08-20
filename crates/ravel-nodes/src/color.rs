// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value-domain colour nodes.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use std::sync::Arc;

/// `color.ramp`: maps one scalar to one colour through a colour ramp.
///
/// Blender's ColorRamp in the value domain. `value` is normalized through
/// `[in_min, in_max]` and the shared [`RampParam::evaluate`] answers the colour
/// at that position — the same function `field.ramp` samples per element, so
/// the two agree stop for stop. Alpha comes out of the stops with the rest of
/// the colour; `Color` carries it, so there is no separate port.
///
/// Out-of-range input clamps to the end stops, which is what the ramp's own
/// evaluation already does. Unlike `math.curve` there is therefore no
/// `extrapolation` parameter: a curve ends in a slope that can be repeated or
/// extended, a ramp ends in a colour and nothing else is defined.
///
/// [`RampParam::evaluate`]: ravel_core::param_ramp::RampParam::evaluate
pub struct ColorRampProcessor;

impl ColorRampProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ColorRampProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // A missing or wrongly typed `stops` falls back to the default ramp,
        // which is what the template declares.
        let ramp = params.ramp("stops").cloned().unwrap_or_default();
        let position = ramp_position(
            params.f32_or("value", 0.0),
            params.f32_or("in_min", 0.0),
            params.f32_or("in_max", 1.0),
        );
        Ok(Arc::new(ramp.evaluate(position)))
    }
}

/// Input value → ramp position, the same mapping `RampField::normalized`
/// performs for the field domain.
///
/// `in_min` / `in_max` are ordinary Float parameters, so a parameter port can
/// drive them from a computed value and hand this a `NaN` or an infinity. A
/// span that is not a finite non-zero width degenerates to a hard step at
/// `in_min` rather than dividing into `NaN`, which would read as the ramp's
/// last colour everywhere. `in_max < in_min` is legal and reverses the ramp.
///
/// The two domains normalize separately (the field one is a method on the
/// field object, which needs a source field this node does not have);
/// `the_value_ramp_and_the_field_ramp_agree` pins them to the same answers.
fn ramp_position(value: f32, in_min: f32, in_max: f32) -> f32 {
    let span = in_max - in_min;
    if !span.is_finite() || span == 0.0 {
        return if value < in_min { 0.0 } else { 1.0 };
    }
    (value - in_min) / span
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{
        AttributeArray, ConstantField, Field, FieldSample, FieldValue, RampField,
    };
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::param_ramp::RampParam;
    use ravel_core::types::{Color, FrameRate, Vec2};

    const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn ramp_node(id: u64, stops: RampParam, value: f32, in_min: f32, in_max: f32) -> Node {
        Node::new(NodeId::new(id), "color.ramp")
            .with_output("output", DataTypeId::COLOR)
            .with_param("value", ParameterValue::Float(value))
            .with_param("in_min", ParameterValue::Float(in_min))
            .with_param("in_max", ParameterValue::Float(in_max))
            .with_param("stops", ParameterValue::Ramp(stops))
    }

    fn eval_ramp(stops: RampParam, value: f32, in_min: f32, in_max: f32) -> Color {
        let node = ramp_node(1, stops, value, in_min, in_max);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ColorRampProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        *out.downcast_ref::<Color>().unwrap()
    }

    fn assert_close(left: Color, right: Color) {
        let close = (left.r - right.r).abs() < 1e-6
            && (left.g - right.g).abs() < 1e-6
            && (left.b - right.b).abs() < 1e-6
            && (left.a - right.a).abs() < 1e-6;
        assert!(close, "{left:?} != {right:?}");
    }

    /// Known stops, known input, known colour — including the alpha, which
    /// rides along in the same `Color` rather than a second output.
    #[test]
    fn known_stops_map_an_input_to_the_expected_colour() {
        let stops = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        assert_close(eval_ramp(stops.clone(), 0.0, 0.0, 1.0), RED);
        assert_close(eval_ramp(stops.clone(), 1.0, 0.0, 1.0), BLUE);
        assert_close(
            eval_ramp(stops.clone(), 0.25, 0.0, 1.0),
            Color::new(0.75, 0.0, 0.25, 1.0),
        );
        // The input range is what places the value on the ramp: 4 of 0..8 is
        // the midpoint.
        assert_close(
            eval_ramp(stops, 4.0, 0.0, 8.0),
            Color::new(0.5, 0.0, 0.5, 1.0),
        );

        let translucent = RampParam::linear([
            (0.0, Color::new(1.0, 1.0, 1.0, 0.0)),
            (1.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        ]);
        assert_close(
            eval_ramp(translucent, 0.5, 0.0, 1.0),
            Color::new(1.0, 1.0, 1.0, 0.5),
        );
    }

    /// Outside `[in_min, in_max]` the end stops hold. That is the whole
    /// out-of-range story for this node — there is no repeat or extend mode.
    #[test]
    fn out_of_range_input_clamps_to_the_end_stops() {
        let stops = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        assert_close(eval_ramp(stops.clone(), -5.0, 0.0, 1.0), RED);
        assert_close(eval_ramp(stops.clone(), 5.0, 0.0, 1.0), BLUE);
        // A reversed range swaps which end each side clamps to.
        assert_close(eval_ramp(stops.clone(), -5.0, 1.0, 0.0), BLUE);
        assert_close(eval_ramp(stops, 5.0, 1.0, 0.0), RED);
    }

    /// The value domain and the field domain answer the same colour for the
    /// same stops and the same input range, degenerate ranges included.
    ///
    /// `RampField` is exactly what `field.ramp`'s processor builds from its
    /// three parameters, so comparing against it compares the two nodes.
    #[test]
    fn the_value_ramp_and_the_field_ramp_agree() {
        let stops = RampParam::linear([(0.0, RED), (0.5, Color::WHITE), (1.0, BLUE)]);
        for (value, in_min, in_max) in [
            (0.0, 0.0, 1.0),
            (0.25, 0.0, 1.0),
            (0.5, 0.0, 1.0),
            (4.0, 0.0, 8.0),
            (-2.0, 0.0, 1.0),
            (7.0, 0.0, 1.0),
            (0.75, 1.0, 0.0),
            // Zero-width and non-finite ranges: a hard step at `in_min`.
            (0.4, 0.5, 0.5),
            (0.6, 0.5, 0.5),
            (0.5, 0.0, f32::INFINITY),
        ] {
            let field = RampField::new(FieldValue::new(ConstantField(value)), stops.clone())
                .with_range(in_min, in_max);
            let sampled =
                match field.sample(&FieldSample::positions_only(&[Vec2(0.0, 0.0)], &ctx())) {
                    AttributeArray::Color(colors) => colors[0],
                    other => panic!("field.ramp must answer Color, got {:?}", other.attr_type()),
                };
            assert_close(eval_ramp(stops.clone(), value, in_min, in_max), sampled);
        }
    }

    /// A node whose `stops` is missing entirely falls back to the default
    /// ramp — black to white — rather than failing, the way `field.ramp` does.
    #[test]
    fn a_missing_ramp_is_the_default_ramp() {
        let node = Node::new(NodeId::new(1), "color.ramp").with_output("output", DataTypeId::COLOR);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ColorRampProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        assert_close(*out.downcast_ref::<Color>().unwrap(), Color::BLACK);
    }

    /// `math.scalar → color.ramp`: a driven `value` makes the colour follow an
    /// upstream number, which is the point of the node — a per-layer index or
    /// a clock reaches it the same way once a source for one exists.
    ///
    /// The plan writes this chain as `layer.info(index) → color.ramp`;
    /// `layer.info` is not implemented yet, so the driving scalar comes from
    /// `math.scalar` here.
    #[test]
    fn a_driven_value_changes_the_colour() {
        let stops = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        let colour_at = |index: f32| {
            let source = Node::new(NodeId::new(1), "math.scalar")
                .with_output("output", DataTypeId::SCALAR)
                .with_param("op", ParameterValue::String("multiply".into()))
                .with_param("a", ParameterValue::Float(index))
                .with_param("b", ParameterValue::Float(0.25));
            let ramp = ramp_node(2, stops.clone(), 0.0, 0.0, 1.0);
            let graph = Graph::new()
                .add_node(source)
                .unwrap()
                .add_node(ramp)
                .unwrap()
                .expose_param_port(NodeId::new(2), "value")
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(0),
                )
                .unwrap();
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(1), Arc::new(crate::math::MathScalarProcessor));
            ev.register(NodeId::new(2), Arc::new(ColorRampProcessor));
            let out = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
            *out.downcast_ref::<Color>().unwrap()
        };

        assert_close(colour_at(0.0), RED);
        assert_close(colour_at(2.0), Color::new(0.5, 0.0, 0.5, 1.0));
        assert_close(colour_at(4.0), BLUE);
    }
}

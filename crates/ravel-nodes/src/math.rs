// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scalar math nodes (CPU-only): `math.scalar`, `math.remap`, and `math.curve`.
//!
//! These nodes have no fixed input ports — their scalar operands are Float
//! parameters, so they are editable in the Properties panel when unconnected
//! and drivable through an exposed parameter port (e.g. `net.in`'s `t` or
//! `f`) when connected. Curve parameters remain structural Properties rows.

use ravel_core::animation::interpolation::Interpolation;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::param_curve::{CurveParam, CurvePoint};
use ravel_core::types::{NodeData, Scalar};
use std::sync::Arc;

/// `math.scalar`: one binary or unary operation over `a` and `b`.
///
/// Unary ops ignore `b`. Trigonometry is in radians. Degenerate inputs
/// yield 0 instead of non-finite values: division and modulo by zero, and
/// the square root of a negative number.
pub struct MathScalarProcessor;

impl MathScalarProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for MathScalarProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let a = params.f32_or("a", 0.0);
        let b = params.f32_or("b", 1.0);
        let out = match params.str_or("op", "add") {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" if b != 0.0 => a / b,
            "divide" => 0.0,
            "min" => a.min(b),
            "max" => a.max(b),
            // Euclidean remainder: always non-negative for b != 0, which
            // keeps frame/time cycling monotonic across negative inputs.
            "mod" if b != 0.0 => a.rem_euclid(b),
            "mod" => 0.0,
            "pow" => a.powf(b),
            "abs" => a.abs(),
            "negate" => -a,
            "floor" => a.floor(),
            "ceil" => a.ceil(),
            "round" => a.round(),
            "sqrt" if a >= 0.0 => a.sqrt(),
            "sqrt" => 0.0,
            "sin" => a.sin(),
            "cos" => a.cos(),
            other => anyhow::bail!("math.scalar: unknown op {other:?}"),
        };
        Ok(Arc::new(Scalar(out)))
    }
}

/// `math.remap`: linear fit of `value` from `[in_min, in_max]` to
/// `[out_min, out_max]`, optionally clamped to the output range.
///
/// A degenerate input range (`in_min == in_max`) maps everything to
/// `out_min` instead of producing non-finite values.
pub struct MathRemapProcessor;

impl MathRemapProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for MathRemapProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let value = params.f32_or("value", 0.0);
        let in_min = params.f32_or("in_min", 0.0);
        let in_max = params.f32_or("in_max", 1.0);
        let out_min = params.f32_or("out_min", 0.0);
        let out_max = params.f32_or("out_max", 1.0);
        let clamp = params.bool_or("clamp", false);

        let span = in_max - in_min;
        let mut t = if span == 0.0 {
            0.0
        } else {
            (value - in_min) / span
        };
        if clamp {
            t = t.clamp(0.0, 1.0);
        }
        Ok(Arc::new(Scalar(out_min + t * (out_max - out_min))))
    }
}

/// `math.curve`: maps a scalar through a normalized transfer curve.
///
/// `value` is first mapped to a normalized input coordinate, then the shared
/// [`CurveParam::evaluate`] implementation supplies the curve value, and that
/// value is mapped to the output range. The curve itself retains its existing
/// clamping behavior; the node chooses how values outside the normalized
/// `[0, 1]` range reach it.
pub struct MathCurveProcessor;

impl MathCurveProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for MathCurveProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let value = params.f32_or("value", 0.0);
        let in_min = params.f32_or("in_min", 0.0);
        let in_max = params.f32_or("in_max", 1.0);
        let out_min = params.f32_or("out_min", 0.0);
        let out_max = params.f32_or("out_max", 1.0);
        let extrapolation = params.str_or("extrapolation", "clamp");
        let curve = params.curve("curve").cloned().unwrap_or_default();

        let input_span = in_max - in_min;
        let normalized = if input_span == 0.0 {
            0.0
        } else {
            (value - in_min) / input_span
        };
        let curve_value = match extrapolation {
            "repeat" if !(0.0..=1.0).contains(&normalized) => {
                curve.evaluate(normalized.rem_euclid(1.0))
            }
            "extend" if !(0.0..=1.0).contains(&normalized) => {
                extend_curve_value(&curve, normalized)
            }
            _ => curve.evaluate(normalized.clamp(0.0, 1.0)),
        };
        Ok(Arc::new(Scalar(
            out_min + curve_value * (out_max - out_min),
        )))
    }
}

/// Extend a curve past its normalized domain using the tangent at the nearest
/// endpoint. Linear and step segments use their exact endpoint slope; Bezier
/// segments use the endpoint handle's slope. A vertical or unavailable handle
/// falls back to the adjacent secant so the result remains a scalar function.
fn extend_curve_value(curve: &CurveParam, normalized: f32) -> f32 {
    let points = curve.points();
    if points.is_empty() {
        return normalized;
    }
    if points.len() < 2 || (0.0..=1.0).contains(&normalized) {
        return curve.evaluate(normalized.clamp(0.0, 1.0));
    }

    if normalized < 0.0 {
        let first = points[0];
        let next = points[1];
        let slope = endpoint_slope(first, next, true);
        first.y + slope * (normalized - first.x)
    } else {
        let previous = points[points.len() - 2];
        let last = points[points.len() - 1];
        let slope = endpoint_slope(previous, last, false);
        last.y + slope * (normalized - last.x)
    }
}

fn endpoint_slope(previous: CurvePoint, endpoint: CurvePoint, at_start: bool) -> f32 {
    let slope = match previous.interpolation {
        Interpolation::Step => 0.0,
        Interpolation::Linear => secant_slope(previous, endpoint),
        Interpolation::Bezier => {
            let tangent = if at_start {
                previous.tangent_out
            } else {
                endpoint.tangent_in
            };
            if tangent.0 != 0.0 {
                tangent.1 / tangent.0
            } else {
                secant_slope(previous, endpoint)
            }
        }
    };
    if slope.is_finite() { slope } else { 0.0 }
}

fn secant_slope(previous: CurvePoint, endpoint: CurvePoint) -> f32 {
    let width = endpoint.x - previous.x;
    if width == 0.0 {
        0.0
    } else {
        (endpoint.y - previous.y) / width
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    fn eval_op(op: &str, a: f32, b: f32) -> f32 {
        let node = Node::new(NodeId::new(1), "math.scalar")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("op", ParameterValue::String(op.into()))
            .with_param("a", ParameterValue::Float(a))
            .with_param("b", ParameterValue::Float(b));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathScalarProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        out.downcast_ref::<Scalar>().unwrap().0
    }

    #[test]
    fn binary_ops() {
        assert_eq!(eval_op("add", 2.0, 3.0), 5.0);
        assert_eq!(eval_op("subtract", 2.0, 3.0), -1.0);
        assert_eq!(eval_op("multiply", 2.0, 3.0), 6.0);
        assert_eq!(eval_op("divide", 6.0, 3.0), 2.0);
        assert_eq!(eval_op("min", 2.0, 3.0), 2.0);
        assert_eq!(eval_op("max", 2.0, 3.0), 3.0);
        assert_eq!(eval_op("mod", 7.0, 3.0), 1.0);
        assert_eq!(eval_op("pow", 2.0, 10.0), 1024.0);
    }

    #[test]
    fn unary_ops_ignore_b() {
        assert_eq!(eval_op("abs", -2.5, 99.0), 2.5);
        assert_eq!(eval_op("negate", 2.5, 99.0), -2.5);
        assert_eq!(eval_op("floor", 2.7, 99.0), 2.0);
        assert_eq!(eval_op("ceil", 2.2, 99.0), 3.0);
        assert_eq!(eval_op("round", 2.5, 99.0), 3.0);
        assert_eq!(eval_op("sqrt", 9.0, 99.0), 3.0);
        assert!((eval_op("sin", std::f32::consts::FRAC_PI_2, 99.0) - 1.0).abs() < 1e-6);
        assert!((eval_op("cos", 0.0, 99.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_inputs_yield_zero_not_nan() {
        assert_eq!(eval_op("divide", 1.0, 0.0), 0.0);
        assert_eq!(eval_op("mod", 1.0, 0.0), 0.0);
        assert_eq!(eval_op("sqrt", -1.0, 0.0), 0.0);
    }

    #[test]
    fn mod_is_euclidean() {
        // -1 mod 3 = 2 (not -1): monotonic cycling for negative frames.
        assert_eq!(eval_op("mod", -1.0, 3.0), 2.0);
    }

    #[test]
    fn unknown_op_is_an_error() {
        let node = Node::new(NodeId::new(1), "math.scalar")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("op", ParameterValue::String("nope".into()));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathScalarProcessor));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx()).is_err());
    }

    #[test]
    fn is_not_time_dependent() {
        assert!(!MathScalarProcessor.is_time_dependent());
        assert!(!MathRemapProcessor.is_time_dependent());
        assert!(!MathCurveProcessor.is_time_dependent());
    }

    fn eval_curve(value: f32, extrapolation: &str, curve: CurveParam) -> f32 {
        let node = Node::new(NodeId::new(1), "math.curve")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
            .with_param("in_min", ParameterValue::Float(0.0))
            .with_param("in_max", ParameterValue::Float(1.0))
            .with_param("out_min", ParameterValue::Float(0.0))
            .with_param("out_max", ParameterValue::Float(1.0))
            .with_param(
                "extrapolation",
                ParameterValue::String(extrapolation.into()),
            )
            .with_param("curve", ParameterValue::Curve(curve));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathCurveProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        out.downcast_ref::<Scalar>().unwrap().0
    }

    #[test]
    fn curve_identity_maps_input_to_output() {
        let curve = CurveParam::identity();
        assert_eq!(eval_curve(0.0, "clamp", curve.clone()), 0.0);
        assert_eq!(eval_curve(0.25, "clamp", curve.clone()), 0.25);
        assert_eq!(eval_curve(1.0, "clamp", curve), 1.0);

        let node = Node::new(NodeId::new(1), "math.curve")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(5.0))
            .with_param("in_min", ParameterValue::Float(0.0))
            .with_param("in_max", ParameterValue::Float(10.0))
            .with_param("out_min", ParameterValue::Float(-2.0))
            .with_param("out_max", ParameterValue::Float(2.0))
            .with_param("curve", ParameterValue::Curve(CurveParam::identity()));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathCurveProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        assert_eq!(out.downcast_ref::<Scalar>().unwrap().0, 0.0);
    }

    #[test]
    fn curve_extend_uses_bezier_endpoint_tangents() {
        let curve = CurveParam::from_points([
            CurvePoint::new(0.0, 0.0, Interpolation::Bezier).with_tangents(
                ravel_core::types::Vec2(0.0, 0.0),
                ravel_core::types::Vec2(0.25, 1.0),
            ),
            CurvePoint::new(1.0, 1.0, Interpolation::Bezier).with_tangents(
                ravel_core::types::Vec2(-0.25, -0.5),
                ravel_core::types::Vec2(0.0, 0.0),
            ),
        ]);
        assert_eq!(eval_curve(-0.25, "extend", curve.clone()), -1.0);
        assert_eq!(eval_curve(1.25, "extend", curve), 1.5);
    }

    #[test]
    fn curve_out_of_range_modes_are_distinct_and_defined() {
        let curve = CurveParam::linear([(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);

        assert_eq!(eval_curve(-0.25, "clamp", curve.clone()), 0.0);
        assert_eq!(eval_curve(1.25, "clamp", curve.clone()), 0.0);
        assert_eq!(eval_curve(-0.25, "repeat", curve.clone()), 0.5);
        assert_eq!(eval_curve(1.25, "repeat", curve.clone()), 0.5);
        assert_eq!(eval_curve(-0.25, "extend", curve.clone()), -0.5);
        assert_eq!(eval_curve(1.25, "extend", curve), -0.5);
    }

    #[test]
    fn curve_matches_field_curve_remap_for_the_same_control_points() {
        use ravel_core::geometry::{
            ConstantField, CurveRemapField, Field, FieldSample, FieldValue,
        };
        use ravel_core::types::Vec2;

        let curve = CurveParam::linear([(0.0, 0.0), (0.25, 0.8), (1.0, 0.2)]);
        let input = 0.5;
        let field = CurveRemapField::new(FieldValue::new(ConstantField(input)), curve.clone());
        let field_value = field
            .sample(&FieldSample::positions_only(&[Vec2(0.0, 0.0)], &ctx()))
            .as_f32("sample")
            .unwrap()[0];

        let math_value = eval_curve(input, "clamp", curve);
        assert_eq!(math_value.to_bits(), field_value.to_bits());
    }

    fn eval_remap(value: f32, clamp: bool) -> f32 {
        let node = Node::new(NodeId::new(1), "math.remap")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
            .with_param("in_min", ParameterValue::Float(0.0))
            .with_param("in_max", ParameterValue::Float(2.0))
            .with_param("out_min", ParameterValue::Float(10.0))
            .with_param("out_max", ParameterValue::Float(20.0))
            .with_param("clamp", ParameterValue::Bool(clamp));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathRemapProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        out.downcast_ref::<Scalar>().unwrap().0
    }

    #[test]
    fn remap_fits_linearly() {
        assert_eq!(eval_remap(0.0, false), 10.0);
        assert_eq!(eval_remap(1.0, false), 15.0);
        assert_eq!(eval_remap(2.0, false), 20.0);
        // Unclamped extrapolation.
        assert_eq!(eval_remap(4.0, false), 30.0);
        // Clamped to the output range.
        assert_eq!(eval_remap(4.0, true), 20.0);
        assert_eq!(eval_remap(-2.0, true), 10.0);
    }

    #[test]
    fn remap_degenerate_in_range_yields_out_min() {
        let node = Node::new(NodeId::new(1), "math.remap")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(5.0))
            .with_param("in_min", ParameterValue::Float(1.0))
            .with_param("in_max", ParameterValue::Float(1.0))
            .with_param("out_min", ParameterValue::Float(10.0))
            .with_param("out_max", ParameterValue::Float(20.0));
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MathRemapProcessor));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        assert_eq!(out.downcast_ref::<Scalar>().unwrap().0, 10.0);
    }

    /// The plan's full acceptance path: `f` → `math.scalar` (mod 30) →
    /// `shape.rect` `width` parameter port, re-evaluated per frame.
    #[test]
    fn f_drives_a_shape_param_through_math() {
        use ravel_core::geometry::{Geometry, names};

        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR);
        let math = Node::new(NodeId::new(2), "math.scalar")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("op", ParameterValue::String("mod".into()))
            .with_param("a", ParameterValue::Float(0.0))
            .with_param("b", ParameterValue::Float(30.0));
        let rect = Node::new(NodeId::new(3), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("center", ParameterValue::vec2(32.0, 32.0))
            .with_param("width", ParameterValue::Float(4.0))
            .with_param("height", ParameterValue::Float(4.0));
        let graph = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(math)
            .unwrap()
            .add_node(rect)
            .unwrap()
            .expose_param_port(NodeId::new(2), "a")
            .unwrap()
            .expose_param_port(NodeId::new(3), "width")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(crate::net::NetInProcessor));
        ev.register(NodeId::new(2), Arc::new(MathScalarProcessor));
        ev.register(NodeId::new(3), Arc::new(crate::shape::RectProcessor));

        let fps = FrameRate::new(30, 1);
        let width_at = |ev: &mut Evaluator, frame: u64| -> f32 {
            let ctx = EvalContext::new(frame, fps, (64, 64));
            let out = ev.evaluate(&graph, NodeId::new(3), &ctx).unwrap();
            let geo = out.downcast_ref::<Geometry>().unwrap();
            let xs: Vec<f32> = geo
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .iter()
                .map(|p| p.0)
                .collect();
            xs.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - xs.iter().copied().fold(f32::INFINITY, f32::min)
        };
        assert_eq!(width_at(&mut ev, 10), 10.0, "f=10 mod 30 → width 10");
        // The frame index cycles through mod.
        assert_eq!(width_at(&mut ev, 40), 10.0, "f=40 mod 30 → width 10");
        assert_eq!(width_at(&mut ev, 25), 25.0, "f=25 mod 30 → width 25");
    }

    /// The motivating use case: `net.in`'s `t` (seconds) driving `a`
    /// through an exposed parameter port, scaled by `b` per frame.
    #[test]
    fn t_times_coefficient_through_param_port() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR);
        let math = Node::new(NodeId::new(2), "math.scalar")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("op", ParameterValue::String("multiply".into()))
            .with_param("a", ParameterValue::Float(0.0))
            .with_param("b", ParameterValue::Float(90.0));
        let graph = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(math)
            .unwrap()
            .expose_param_port(NodeId::new(2), "a")
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
        ev.register(NodeId::new(1), Arc::new(crate::net::NetInProcessor));
        ev.register(NodeId::new(2), Arc::new(MathScalarProcessor));

        let fps = FrameRate::new(30, 1);
        let at = |ev: &mut Evaluator, frame: u64| {
            let ctx = EvalContext::new(frame, fps, (64, 64));
            let out = ev.evaluate(&graph, NodeId::new(2), &ctx).unwrap();
            out.downcast_ref::<Scalar>().unwrap().0
        };
        assert_eq!(at(&mut ev, 0), 0.0);
        // t = 1s at frame 30 → 1 × 90 = 90.
        assert_eq!(at(&mut ev, 30), 90.0);
    }
}

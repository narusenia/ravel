// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden test for `geometry.repeat`: a spiral.
//!
//! ```text
//! shape.rect -> geometry.repeat -> geometry.transform -> rasterize
//! ```
//!
//! built from the real registry templates and evaluated through a real
//! [`Evaluator`] on the CPU reference rasterizer (no GPU adapter needed).
//!
//! The step is `translate (7, 0)`, `rotate 25 deg`, `scale 0.93`. Copy `i`
//! therefore sits at `t * (1 - z^i) / (1 - z)` with `z = 0.93 * e^(i 25 deg)`,
//! the closed form of the cumulative placement. The expected centres are
//! computed here from that formula in complex arithmetic, independently of
//! the node's own iterated composition, and no pixel value is copied from a
//! render: each copy's centre pixel must be covered, the spots between the
//! arms must not be.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_nodes::geometry::GeometryTransformProcessor;
use ravel_nodes::geometry_ops::GeometryRepeatProcessor;
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::shape::RectProcessor;
use std::sync::Arc;

const CANVAS: (u32, u32) = (96, 96);
const ORIGIN: f64 = 48.0;
const COUNT: usize = 9;
const STEP: (f64, f64) = (7.0, 0.0);
const TURN_DEGREES: f64 = 25.0;
const SHRINK: f64 = 0.93;

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(24, 1), CANVAS)
}

fn node(
    registry: &NodeRegistry,
    type_key: &str,
    id: u64,
    params: &[(&str, ParameterValue)],
) -> Node {
    let mut node = registry
        .create_node(type_key, NodeId::new(id))
        .unwrap_or_else(|| panic!("{type_key} is not registered"));
    for (key, value) in params {
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == *key)
            .unwrap_or_else(|| panic!("{type_key} has no {key} parameter"))
            .value = value.clone();
    }
    node
}

fn spiral() -> FrameBuffer {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let nodes = [
        node(
            &registry,
            "shape.rect",
            1,
            &[
                ("center", ParameterValue::vec2(0.0, 0.0)),
                ("width", ParameterValue::Float(6.0)),
                ("height", ParameterValue::Float(6.0)),
            ],
        ),
        node(
            &registry,
            "geometry.repeat",
            2,
            &[
                ("count", ParameterValue::Int(COUNT as i32)),
                (
                    "translate",
                    ParameterValue::vec2(STEP.0 as f32, STEP.1 as f32),
                ),
                ("rotate", ParameterValue::Float(TURN_DEGREES as f32)),
                ("scale", ParameterValue::vec2(SHRINK as f32, SHRINK as f32)),
            ],
        ),
        // Moves the whole spiral to the middle of the canvas: the repeat
        // turns and scales about the source's own origin.
        node(
            &registry,
            "geometry.transform",
            3,
            &[
                (
                    "translate",
                    ParameterValue::vec3(ORIGIN as f32, ORIGIN as f32, 0.0),
                ),
                ("use_centroid", ParameterValue::Bool(false)),
            ],
        ),
        node(&registry, "rasterize", 4, &[]),
    ];
    let mut graph = Graph::new();
    let mut evaluator = Evaluator::new();
    for node in &nodes {
        graph = graph.add_node(node.clone()).unwrap();
        let processor: Arc<dyn NodeProcessor> = match node.type_key.as_str() {
            "shape.rect" => Arc::new(RectProcessor),
            "geometry.repeat" => Arc::new(GeometryRepeatProcessor),
            "geometry.transform" => Arc::new(GeometryTransformProcessor),
            "rasterize" => Arc::new(RasterizeProcessor::from_node(node)),
            other => panic!("no CPU processor wired for {other}"),
        };
        evaluator.register(node.id, processor);
    }
    for (i, (from, to)) in [(1u64, 2u64), (2, 3), (3, 4)].into_iter().enumerate() {
        graph = graph
            .add_edge(
                EdgeId::new(i as u64 + 1),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(0),
            )
            .unwrap();
    }
    evaluator
        .evaluate(&graph, NodeId::new(4), &ctx())
        .expect("evaluation succeeds")
        .downcast_ref::<FrameBuffer>()
        .expect("the CPU rasterizer answers a FrameBuffer")
        .clone()
}

fn alpha(frame: &FrameBuffer, x: f64, y: f64) -> f32 {
    let (px, py) = (x.floor() as u32, y.floor() as u32);
    frame.as_f32()[((py * frame.width + px) * 4 + 3) as usize]
}

/// Copy `i` of the spiral by the closed form, as canvas coordinates.
fn centre(i: usize) -> (f64, f64) {
    let theta = TURN_DEGREES.to_radians();
    let z = (SHRINK * theta.cos(), SHRINK * theta.sin());
    let mul = |a: (f64, f64), b: (f64, f64)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let mut zi = (1.0, 0.0);
    for _ in 0..i {
        zi = mul(zi, z);
    }
    let numerator = (1.0 - zi.0, -zi.1);
    let denominator = (1.0 - z.0, -z.1);
    let norm = denominator.0 * denominator.0 + denominator.1 * denominator.1;
    let quotient = (
        (numerator.0 * denominator.0 + numerator.1 * denominator.1) / norm,
        (numerator.1 * denominator.0 - numerator.0 * denominator.1) / norm,
    );
    let p = mul(STEP, quotient);
    (ORIGIN + p.0, ORIGIN + p.1)
}

#[test]
fn a_repeat_lays_a_spiral_of_cumulative_placements() {
    let frame = spiral();
    // The copies are 6 * 0.93^i wide, so even the last is wider than a pixel
    // diagonal and its centre pixel is fully inside.
    for i in 0..COUNT {
        let (x, y) = centre(i);
        assert!(
            alpha(&frame, x, y) > 0.99,
            "copy {i} should cover its centre ({x:.2}, {y:.2})"
        );
    }
    // The spiral is the closed form, not a ring or a line: the spot a copy
    // would occupy if the turn did not accumulate (a straight run along the
    // step direction) is empty, as is the canvas corner.
    for i in 4..COUNT {
        let (x, y) = (ORIGIN + i as f64 * STEP.0, ORIGIN);
        assert!(
            alpha(&frame, x, y) == 0.0,
            "({x}, {y}) lies on the straight line, not on the spiral"
        );
    }
    assert_eq!(alpha(&frame, 1.0, 1.0), 0.0);
}

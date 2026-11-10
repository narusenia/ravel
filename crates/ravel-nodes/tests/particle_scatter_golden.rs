// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden test for particles as another node's source (REQ-MOGRAPH-002: the
//! particle points can feed other nodes): a shape stands at each particle.
//!
//! ```text
//! particle.simulate -> scatter.points <- shape.rect
//!                          |
//!                          v
//!                  geometry.transform -> rasterize
//! ```
//!
//! built from the real registry templates and evaluated through a real
//! [`Evaluator`] on the CPU reference rasterizer (no GPU adapter needed).
//!
//! Nothing is wired into the emitter, so every particle is born at the origin
//! with the same launch velocity of `(240, 0)` px/s: at 30 fps that is 8 px
//! per frame, and one particle is born per frame. At frame 5 the six
//! particles therefore stand at `x = 0, 8, ... 40` on the line `y = 0`. The
//! expected positions come from that arithmetic, not from a render: a 6 x 6
//! square must cover each particle's pixel, and the spots between them (4 px
//! on, outside a square's half width of 3) must stay empty.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, stateful_processor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_nodes::geometry::GeometryTransformProcessor;
use ravel_nodes::particle::ParticleSimulate;
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::scatter::PointsProcessor;
use ravel_nodes::shape::RectProcessor;
use std::sync::Arc;

const CANVAS: (u32, u32) = (96, 96);
const ORIGIN: f64 = 48.0;
const FRAME: u64 = 5;
/// Pixels a particle travels per frame: 240 px/s at 30 fps.
const STEP: f64 = 8.0;

fn ctx() -> EvalContext {
    EvalContext::new(FRAME, FrameRate::new(30, 1), CANVAS)
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

fn shapes_on_particles() -> FrameBuffer {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let nodes = [
        node(
            &registry,
            "particle.simulate",
            1,
            &[
                ("rate", ParameterValue::Float(30.0)),
                ("life", ParameterValue::Float(1000.0)),
                (
                    "initial_velocity",
                    ParameterValue::vec2((STEP * 30.0) as f32, 0.0),
                ),
            ],
        ),
        node(
            &registry,
            "shape.rect",
            2,
            &[
                ("center", ParameterValue::vec2(0.0, 0.0)),
                ("width", ParameterValue::Float(6.0)),
                ("height", ParameterValue::Float(6.0)),
            ],
        ),
        node(&registry, "scatter.points", 3, &[]),
        node(
            &registry,
            "geometry.transform",
            4,
            &[
                (
                    "translate",
                    ParameterValue::vec3(ORIGIN as f32, ORIGIN as f32, 0.0),
                ),
                ("use_centroid", ParameterValue::Bool(false)),
            ],
        ),
        node(&registry, "rasterize", 5, &[]),
    ];
    let mut graph = Graph::new();
    let mut evaluator = Evaluator::new();
    for node in &nodes {
        graph = graph.add_node(node.clone()).unwrap();
        let processor: Arc<dyn NodeProcessor> = match node.type_key.as_str() {
            "particle.simulate" => stateful_processor(ParticleSimulate),
            "shape.rect" => Arc::new(RectProcessor),
            "scatter.points" => Arc::new(PointsProcessor::from_node(node)),
            "geometry.transform" => Arc::new(GeometryTransformProcessor),
            "rasterize" => Arc::new(RasterizeProcessor::from_node(node)),
            other => panic!("no CPU processor wired for {other}"),
        };
        evaluator.register(node.id, processor);
    }
    // (from, to, input slot): the particles are the points, the square is the
    // source in the variadic group after them.
    for (i, (from, to, slot)) in [(1u64, 3u64, 0u32), (2, 3, 1), (3, 4, 0), (4, 5, 0)]
        .into_iter()
        .enumerate()
    {
        graph = graph
            .add_edge(
                EdgeId::new(i as u64 + 1),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(slot),
            )
            .unwrap();
    }
    evaluator
        .evaluate(&graph, NodeId::new(5), &ctx())
        .expect("evaluation succeeds")
        .downcast_ref::<FrameBuffer>()
        .expect("the CPU rasterizer answers a FrameBuffer")
        .clone()
}

fn alpha(frame: &FrameBuffer, x: f64, y: f64) -> f32 {
    let (px, py) = (x.floor() as u32, y.floor() as u32);
    frame.as_f32()[((py * frame.width + px) * 4 + 3) as usize]
}

#[test]
fn a_shape_stands_at_each_particle_position() {
    let frame = shapes_on_particles();
    for k in 0..=FRAME {
        let x = ORIGIN + k as f64 * STEP;
        // The square is 6 px wide, so it covers its own centre and the pixel
        // 2 px to either side of it...
        for dx in [-2.0, 0.0, 2.0] {
            assert!(
                alpha(&frame, x + dx, ORIGIN) > 0.99,
                "particle {k}: the square should cover x = {}",
                x + dx
            );
        }
        // ...and nothing stands between two squares, 4 px on from the centre.
        if k < FRAME {
            assert_eq!(
                alpha(&frame, x + STEP / 2.0, ORIGIN),
                0.0,
                "nothing between particles {k} and {}",
                k + 1
            );
        }
    }
    // The square has height as well, so it is a shape and not a sprite line,
    // and nothing is drawn off the line, past the oldest, or in a corner.
    assert!(alpha(&frame, ORIGIN, ORIGIN + 2.0) > 0.99);
    assert_eq!(alpha(&frame, ORIGIN, ORIGIN + 8.0), 0.0);
    assert_eq!(
        alpha(&frame, ORIGIN + (FRAME + 1) as f64 * STEP, ORIGIN),
        0.0
    );
    assert_eq!(alpha(&frame, 1.0, 1.0), 0.0);
}

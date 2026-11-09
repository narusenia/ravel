// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden test for particles as another node's source (REQ-MOGRAPH-002: the
//! particle points can feed other nodes).
//!
//! ```text
//! particle.simulate -> scatter.scatter -> geometry.transform -> rasterize
//! ```
//!
//! built from the real registry templates and evaluated through a real
//! [`Evaluator`] on the CPU reference rasterizer (no GPU adapter needed).
//!
//! Nothing is wired into the emitter, so every particle is born at the origin
//! with the same launch velocity of `(240, 0)` px/s: at 30 fps that is 8 px
//! per frame, and one particle is born per frame. At frame 5 the six
//! particles therefore stand at `x = 0, 8, ... 40` on the line `y = 0`. The
//! expected positions come from that arithmetic, not from a render: each
//! particle's pixel must be covered, and the spots between them must not.
//!
//! `scatter.scatter` with one instance at the origin stamps the whole particle
//! cloud (its `instance_source`) once, which is what the particles being a
//! source for another node means. The instance carries the node's seeded
//! random turn, so the line of particles is rotated about the origin by it;
//! `geometry.transform` then moves the result to the middle of the canvas.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, stateful_processor};
use ravel_core::geometry::element_hash;
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_nodes::geometry::GeometryTransformProcessor;
use ravel_nodes::particle::ParticleSimulate;
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::scatter::ScatterProcessor;
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

fn particles_through_scatter() -> FrameBuffer {
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
        // One instance at the origin; the source stays where it is.
        node(
            &registry,
            "scatter.scatter",
            2,
            &[
                ("count", ParameterValue::Int(1)),
                ("area", ParameterValue::vec2(0.0, 0.0)),
                ("center", ParameterValue::vec2(0.0, 0.0)),
                ("center_input", ParameterValue::Bool(false)),
            ],
        ),
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
            "particle.simulate" => stateful_processor(ParticleSimulate),
            "scatter.scatter" => Arc::new(ScatterProcessor::from_node(node)),
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

/// The turn `scatter.scatter` gives its instance `index` with seed 0: the
/// third draw of the element hash, as a fraction of a full turn. The hash is
/// the shared, documented one, so the expectation does not read the render.
fn scatter_rotation(index: u32) -> f64 {
    let h = element_hash(element_hash(0, index), 2);
    f64::from(h & 0x00FF_FFFF) / f64::from(0x0100_0000u32) * std::f64::consts::TAU
}

#[test]
fn particle_points_stand_where_the_simulation_put_them() {
    let frame = particles_through_scatter();
    let turn = scatter_rotation(0);
    let (sin, cos) = turn.sin_cos();
    // Where a point `distance` along the particles' line lands: the cloud is
    // stamped by an instance that turned by `turn` about the origin.
    let land = |distance: f64| (ORIGIN + distance * cos, ORIGIN + distance * sin);

    // The default sprite is two pixels in radius: a particle covers its own
    // pixel, and the spot half a step from it on the line is empty.
    for k in 0..=FRAME {
        let (x, y) = land(k as f64 * STEP);
        assert!(
            alpha(&frame, x, y) > 0.5,
            "particle {k} should be drawn at ({x:.1}, {y:.1})"
        );
        if k < FRAME {
            let (x, y) = land((k as f64 + 0.5) * STEP);
            assert_eq!(
                alpha(&frame, x, y),
                0.0,
                "nothing between, at ({x:.1}, {y:.1})"
            );
        }
    }
    // Past the oldest particle, off the line, and in a corner: empty.
    let (x, y) = land((FRAME + 1) as f64 * STEP);
    assert_eq!(alpha(&frame, x, y), 0.0);
    let (x, y) = land(2.0 * STEP);
    assert_eq!(alpha(&frame, x - 12.0 * sin, y + 12.0 * cos), 0.0);
    assert_eq!(alpha(&frame, 1.0, 1.0), 0.0);
}

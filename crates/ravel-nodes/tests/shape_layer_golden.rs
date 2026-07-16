// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel test for the compiled Shape layer chain (TASK-042/043).
//!
//! Compiles a Composition holding a Shape layer that references a
//! `shape.rect` node, evaluates the resulting DAG through the CPU path
//! (`comp.source.shape` passthrough → synthetic `rasterize` → comp chain),
//! and verifies coverage at the pixel level.

use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{Composition, Layer, LayerSource};
use ravel_core::eval::{EvalContext, Evaluator};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{CompId, DataTypeId, LayerId, NodeId};
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};

fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.data[idx..idx + 4].try_into().unwrap()
}

#[test]
fn shape_layer_compiles_and_rasterizes_rect_pixels() {
    // 64x64 comp; rect centered at (32, 32) with size 32x32 → interior
    // covers [16, 48) on both axes.
    let shape_node = Node::new(NodeId::new(500), "shape.rect")
        .with_output("output", DataTypeId::GEOMETRY)
        .with_param("center_x", ParameterValue::Float(32.0))
        .with_param("center_y", ParameterValue::Float(32.0))
        .with_param("width", ParameterValue::Float(32.0))
        .with_param("height", ParameterValue::Float(32.0));
    let graph = Graph::new().add_node(shape_node).unwrap();

    let comp = Composition::new(
        CompId::new(1),
        "Golden",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(
        Layer::new(
            LayerId::new(1),
            "Rect",
            LayerSource::Shape {
                node_id: NodeId::new(500),
            },
        )
        .with_time(0, 0, 300),
    );

    let result = compile_composition(&comp, graph).expect("compile succeeds");

    let gpu = GpuContext::new_blocking().expect("GPU adapter required for registration");
    let mut shaders = ShaderManager::new(gpu.clone());
    let mut evaluator = Evaluator::new();
    let pool = shared_texture_pool(&gpu);
    register_all_processors(&mut evaluator, &result.graph, &gpu, &mut shaders, &pool);

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = out
        .downcast_ref::<FrameBuffer>()
        .expect("output is a FrameBuffer");

    assert_eq!(fb.width, 64);
    assert_eq!(fb.height, 64);

    // Interior: opaque white (rasterize default Cd).
    for (x, y) in [(32, 32), (20, 20), (44, 44)] {
        let p = pixel(fb, x, y);
        assert!(p[3] > 0.9, "interior ({x},{y}) covered: {p:?}");
        assert!(p[0] > 0.9 && p[1] > 0.9 && p[2] > 0.9, "default white fill");
    }

    // Exterior: fully transparent.
    for (x, y) in [(4, 4), (60, 4), (4, 60), (60, 60), (32, 8), (8, 32)] {
        let p = pixel(fb, x, y);
        assert!(p[3] < 1e-6, "exterior ({x},{y}) transparent: {p:?}");
    }

    // Edge rows just inside/outside the rect boundary (y = 16 boundary).
    assert!(pixel(fb, 32, 17)[3] > 0.5, "just inside top edge");
    assert!(pixel(fb, 32, 14)[3] < 0.1, "just outside top edge");
}

#[test]
fn shape_layer_missing_shape_node_evaluates_to_empty_frame() {
    // A Shape layer whose node_id is absent from the graph: the source
    // emits empty Geometry and the chain must still produce a transparent
    // FrameBuffer instead of failing.
    let comp = Composition::new(
        CompId::new(1),
        "Empty",
        (16, 16),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(
        Layer::new(
            LayerId::new(1),
            "Ghost",
            LayerSource::Shape {
                node_id: NodeId::new(999),
            },
        )
        .with_time(0, 0, 300),
    );

    let result = compile_composition(&comp, Graph::new()).expect("compile succeeds");

    let gpu = GpuContext::new_blocking().expect("GPU adapter required for registration");
    let mut shaders = ShaderManager::new(gpu.clone());
    let mut evaluator = Evaluator::new();
    let pool = shared_texture_pool(&gpu);
    register_all_processors(&mut evaluator, &result.graph, &gpu, &mut shaders, &pool);

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = out
        .downcast_ref::<FrameBuffer>()
        .expect("output is a FrameBuffer");

    assert!(
        fb.data.iter().skip(3).step_by(4).all(|a| *a < 1e-6),
        "every pixel transparent"
    );
}

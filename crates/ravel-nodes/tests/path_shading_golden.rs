// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel tests for shading a path from its own geometry: the gradient
//! along a line (vertex colours) and, below it, stroke alignment.
//!
//! The graph is the roadmap's phase-D picture, built from the real registry
//! templates and evaluated through a real [`Evaluator`]:
//!
//! ```text
//! shape.line -> attribute.curveu -> field.attribute("u")
//!   -> field.ramp -> field.apply("Cd") -> rasterize
//! ```
//!
//! `STYLE-6` could only check this at the attribute level (the Point `Cd`
//! column holds a ramp); these tests read the **pixels**, on the CPU
//! reference rasterizer and on the GPU one, which must agree.
//!
//! Every expected colour is worked out by hand from the line's endpoints —
//! `t = (x + 0.5 - 8) / 48` along a line from `x = 8` to `x = 56` — and none
//! is copied from a render.

use ravel_core::eval::{EvalContext, EvalScope, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Geometry, Primitive, names};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::param_ramp::RampParam;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{Color, FrameBuffer, FrameRate, NodeData, Vec2};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use ravel_nodes::attribute::CurveUProcessor;
use ravel_nodes::field::{ApplyFieldProcessor, AttributeFieldProcessor, RampFieldProcessor};
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::shape::LineProcessor;
use std::sync::{Arc, Mutex};

const CANVAS: (u32, u32) = (64, 32);

const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

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

/// Which rasterizer the last node of the chain runs on.
#[derive(Clone, Copy)]
enum Backend {
    Cpu,
    Gpu,
}

/// Render `shape.line -> curveu -> attribute("u") -> ramp -> apply(target) ->
/// rasterize` and read the frame back. `target` is the attribute the ramp is
/// written to (`Cd` or `stroke_color`).
fn render_gradient_line(backend: Backend, target: &str, stroke_width: f32) -> FrameBuffer {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let line = node(
        &registry,
        "shape.line",
        1,
        &[
            ("start", ParameterValue::vec2(8.0, 16.0)),
            ("end", ParameterValue::vec2(56.0, 16.0)),
            ("segments", ParameterValue::Int(4)),
        ],
    );
    let curveu = node(&registry, "attribute.curveu", 2, &[]);
    let attribute = node(
        &registry,
        "field.attribute",
        3,
        &[("name", ParameterValue::String("u".into()))],
    );
    let ramp = node(
        &registry,
        "field.ramp",
        4,
        &[(
            "stops",
            ParameterValue::Ramp(RampParam::linear([(0.0, RED), (1.0, BLUE)])),
        )],
    );
    let apply = node(
        &registry,
        "field.apply",
        5,
        &[("target", ParameterValue::String(target.into()))],
    );
    let rasterize = node(
        &registry,
        "rasterize",
        6,
        &[
            ("fill", ParameterValue::Bool(false)),
            ("stroke_width", ParameterValue::Float(stroke_width)),
        ],
    );

    let mut graph = Graph::new();
    for n in [&line, &curveu, &attribute, &ramp, &apply, &rasterize] {
        graph = graph.add_node(n.clone()).unwrap();
    }
    for (edge, from, to, port) in [
        (1u64, 1u64, 2u64, 0u32),
        (2, 3, 4, 0),
        (3, 2, 5, 0),
        (4, 4, 5, 1),
        (5, 5, 6, 0),
    ] {
        graph = graph
            .add_edge(
                EdgeId::new(edge),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(port),
            )
            .unwrap();
    }

    let mut evaluator = Evaluator::new();
    evaluator.register(NodeId::new(1), Arc::new(LineProcessor));
    evaluator.register(
        NodeId::new(2),
        Arc::new(CurveUProcessor::from_node(&curveu)),
    );
    evaluator.register(
        NodeId::new(3),
        Arc::new(AttributeFieldProcessor::from_node(&attribute)),
    );
    evaluator.register(
        NodeId::new(4),
        Arc::new(RampFieldProcessor::from_node(&ramp)),
    );
    evaluator.register(
        NodeId::new(5),
        Arc::new(ApplyFieldProcessor::from_node(&apply)),
    );
    let processor: Arc<dyn NodeProcessor> = match backend {
        Backend::Cpu => Arc::new(RasterizeProcessor::from_node(&rasterize)),
        Backend::Gpu => {
            let gpu = GpuContext::new_blocking().expect("GPU required");
            let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
            let mut shaders = ShaderManager::new(gpu.clone());
            Arc::new(RasterizeProcessor::new(gpu, &mut shaders, pool, &rasterize))
        }
    };
    evaluator.register(NodeId::new(6), processor);

    let out = evaluator.evaluate(&graph, NodeId::new(6), &ctx()).unwrap();
    match backend {
        Backend::Cpu => out.downcast_ref::<FrameBuffer>().unwrap().clone(),
        Backend::Gpu => out
            .downcast_ref::<GpuFrameBuffer>()
            .expect("the GPU rasterizer's output stays resident")
            .to_frame_buffer()
            .expect("readback"),
    }
}

fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let i = ((y * fb.width + x) * 4) as usize;
    fb.as_f32()[i..i + 4].try_into().unwrap()
}

fn assert_rgb(actual: [f32; 4], expected: [f32; 3], tolerance: f32, label: &str) {
    for (channel, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - e).abs() <= tolerance,
            "{label}: channel {channel} is {a}, expected {e} (pixel {actual:?})"
        );
    }
}

/// The two ends of the line come out different hues, and the colour in
/// between follows `t = (x + 0.5 - 8) / 48`.
fn assert_gradient(fb: &FrameBuffer, tolerance: f32, label: &str) {
    // x = 10: t = 2.5 / 48; x = 31: t = 23.5 / 48; x = 53: t = 45.5 / 48.
    let t = |x: f32| (x + 0.5 - 8.0) / 48.0;
    for x in [10u32, 31, 53] {
        let t = t(x as f32);
        assert_rgb(
            pixel(fb, x, 16),
            [1.0 - t, 0.0, t],
            tolerance,
            &format!("{label} at x = {x}"),
        );
    }
    let start = pixel(fb, 10, 16);
    let end = pixel(fb, 53, 16);
    assert!(
        start[0] > 0.9 && start[2] < 0.1 && end[0] < 0.1 && end[2] > 0.9,
        "{label}: the start is red and the end blue, got {start:?} / {end:?}"
    );
    // Off the line there is nothing.
    assert_eq!(
        pixel(fb, 31, 2)[3],
        0.0,
        "{label}: the canvas is empty off the line"
    );
}

#[test]
fn a_ramp_paints_a_gradient_along_the_line_on_the_cpu() {
    let fb = render_gradient_line(Backend::Cpu, "Cd", 6.0);
    assert_gradient(&fb, 0.01, "CPU");
}

/// Aimed at `stroke_color` instead of `Cd` the picture is the same: the
/// Point-domain stroke colour is asked first, and it is the stroke that is
/// being shaded either way.
#[test]
fn the_same_ramp_aimed_at_stroke_color_paints_the_same_line() {
    let cd = render_gradient_line(Backend::Cpu, "Cd", 6.0);
    let stroke = render_gradient_line(Backend::Cpu, "stroke_color", 6.0);
    assert_eq!(cd.as_f32(), stroke.as_f32());
}

#[test]
fn a_zero_width_line_draws_nothing_whatever_its_vertex_colours() {
    let fb = render_gradient_line(Backend::Cpu, "Cd", 0.0);
    assert!(fb.as_f32().iter().all(|v| *v == 0.0));
}

/// Both rasterizers shade the line alike: the interior pixels agree to half
/// float rounding, and each matches the hand-derived gradient.
#[test]
fn the_gpu_paints_the_same_gradient_as_the_cpu() {
    let cpu = render_gradient_line(Backend::Cpu, "Cd", 6.0);
    let gpu = render_gradient_line(Backend::Gpu, "Cd", 6.0);
    assert_gradient(&gpu, 0.02, "GPU");
    let mut compared = 0;
    for (c, g) in cpu
        .as_f32()
        .chunks_exact(4)
        .zip(gpu.as_f32().chunks_exact(4))
    {
        if c[3] > 0.99 && g[3] > 0.99 {
            compared += 1;
            for channel in 0..3 {
                assert!(
                    (c[channel] - g[channel]).abs() < 0.02,
                    "CPU {c:?} vs GPU {g:?}"
                );
            }
        }
    }
    assert!(
        compared > 200,
        "only {compared} opaque pixels were compared"
    );
}

// ---------------------------------------------------------------------------
// Stroke alignment together with vertex colours
// ---------------------------------------------------------------------------

/// Emits a fixed geometry, standing in for whatever node would write the
/// attributes (`style.stroke` writes `stroke_align`, but this test pins the
/// rasterizer on its own, not the style node).
struct Source(Geometry);

impl NodeProcessor for Source {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(self.0.clone()))
    }
}

/// A 28 px square at (10, 10) whose points run red, red, blue, blue and whose
/// stroke lies **outside** it.
fn outside_coloured_square() -> Geometry {
    let mut geo = Geometry::from_points(vec![
        Vec2(10.0, 10.0),
        Vec2(38.0, 10.0),
        Vec2(38.0, 38.0),
        Vec2(10.0, 38.0),
    ]);
    geo.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geo.points_mut()
        .insert(names::CD, AttributeArray::Color(vec![RED, RED, BLUE, BLUE]))
        .unwrap();
    geo.primitive_attrs_mut()
        .insert(
            names::STROKE_ALIGN,
            AttributeArray::I32(vec![names::STROKE_ALIGN_OUTSIDE]),
        )
        .unwrap();
    geo
}

fn render_geometry(backend: Backend, geometry: Geometry, stroke_width: f32) -> FrameBuffer {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let source = registry
        .create_node("shape.rect", NodeId::new(1))
        .expect("a geometry-typed node to stand in");
    let rasterize = node(
        &registry,
        "rasterize",
        2,
        &[
            ("fill", ParameterValue::Bool(false)),
            ("stroke_width", ParameterValue::Float(stroke_width)),
        ],
    );
    let graph = Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(rasterize.clone())
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(1),
            OutputPortIndex(0),
            NodeId::new(2),
            InputPortIndex(0),
        )
        .unwrap();
    let mut evaluator = Evaluator::new();
    evaluator.register(NodeId::new(1), Arc::new(Source(geometry)));
    let processor: Arc<dyn NodeProcessor> = match backend {
        Backend::Cpu => Arc::new(RasterizeProcessor::from_node(&rasterize)),
        Backend::Gpu => {
            let gpu = GpuContext::new_blocking().expect("GPU required");
            let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
            let mut shaders = ShaderManager::new(gpu.clone());
            Arc::new(RasterizeProcessor::new(gpu, &mut shaders, pool, &rasterize))
        }
    };
    evaluator.register(NodeId::new(2), processor);
    let ctx = EvalContext::new(0, FrameRate::new(24, 1), (48, 48));
    let out = evaluator.evaluate(&graph, NodeId::new(2), &ctx).unwrap();
    match backend {
        Backend::Cpu => out.downcast_ref::<FrameBuffer>().unwrap().clone(),
        Backend::Gpu => out
            .downcast_ref::<GpuFrameBuffer>()
            .expect("resident")
            .to_frame_buffer()
            .expect("readback"),
    }
}

/// Outside alignment puts the 6 px stroke at x = 4..10 left of the square's
/// left edge, shaded from blue (the last vertex) to red (the first):
/// pixel (5, 24) is `t = (38 - 24.5) / 28` of the way. Nothing is drawn inside.
/// Both rasterizers draw it.
#[test]
fn an_outside_stroke_is_shaded_by_vertex_colours_on_both_rasterizers() {
    let cpu = render_geometry(Backend::Cpu, outside_coloured_square(), 6.0);
    let gpu = render_geometry(Backend::Gpu, outside_coloured_square(), 6.0);
    let t = 13.5 / 28.0;
    for (fb, tolerance, label) in [(&cpu, 1e-3, "CPU"), (&gpu, 0.02, "GPU")] {
        assert_rgb(pixel(fb, 5, 24), [t, 0.0, 1.0 - t], tolerance, label);
        assert_eq!(
            pixel(fb, 12, 24)[3],
            0.0,
            "{label}: nothing inside the square"
        );
        assert_eq!(pixel(fb, 2, 24)[3], 0.0, "{label}: nothing past the width");
    }
    let mut compared = 0;
    for (c, g) in cpu
        .as_f32()
        .chunks_exact(4)
        .zip(gpu.as_f32().chunks_exact(4))
    {
        if c[3] > 0.99 && g[3] > 0.99 {
            compared += 1;
            for channel in 0..3 {
                assert!(
                    (c[channel] - g[channel]).abs() < 0.02,
                    "CPU {c:?} vs GPU {g:?}"
                );
            }
        }
    }
    assert!(
        compared > 400,
        "only {compared} opaque pixels were compared"
    );
}

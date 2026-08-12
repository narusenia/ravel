// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel test for a Shape layer in the layer-network model
//! (REQ-LAYER-007).
//!
//! The layer's owned network is `shape.rect → rasterize → net.out(frame)`;
//! the shell compiler wraps it in the synthetic chain
//! `Background → boundary → Transform → Opacity → Merge`, and the boundary
//! evaluates the network through the scoped evaluator.
//!
//! Every picture here is rendered **twice** — once with the CPU reference
//! rasterizer pinned onto the network's `rasterize` node, once with the
//! registered GPU rasterizer — and the two are required to agree
//! ([`assert_paths_agree`]). The assertions on the picture itself then run
//! against both frames, so neither implementation can drift from the
//! established shape on its own. Pinning only the CPU pixels, which is what
//! this file used to do, would have made moving the viewer and the shell
//! chain onto the GPU rasterizer look like a regression.

use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::{EvalContext, Evaluator};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager};
use ravel_media::frame_cache::MediaFrameCache;
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

/// Which rasterizer the network's `rasterize` node runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Path {
    /// The zeno reference rasterizer, pinned over the registered processor.
    Cpu,
    /// Whatever `register_all_processors` chose — the resident GPU path.
    Gpu,
}

/// The GPU context every test in this file needs, or `None` on a machine
/// without an adapter (the Linux containers in `docs/dev/testing.md`).
///
/// There is no CPU-only mode to fall back to: `register_all_processors`
/// builds GPU processors for the shell chain whichever rasterizer the network
/// uses, so a missing adapter skips the whole test rather than half of it.
fn gpu_or_skip() -> Option<GpuContext> {
    match GpuContext::new_blocking() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            eprintln!("skipping: no GPU adapter available");
            None
        }
    }
}

fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.as_f32()[idx..idx + 4].try_into().unwrap()
}

fn output_frame(value: &Arc<dyn ravel_core::types::NodeData>) -> FrameBuffer {
    if let Some(frame) = value.downcast_ref::<FrameBuffer>() {
        frame.clone()
    } else {
        value
            .downcast_ref::<GpuFrameBuffer>()
            .expect("output is a resident or CPU FrameBuffer")
            .to_frame_buffer()
            .expect("resident output reads back")
    }
}

/// The two rasterizers agree on the picture.
///
/// They cannot agree bit for bit, and the tolerance is not a fudge factor —
/// each term names a documented difference between the implementations:
///
/// * **Alpha convention.** The CPU path blends straight-alpha Porter-Duff
///   src-over per pixel (`blend_pixel`). The GPU path blends *premultiplied*
///   into an `Rgba16Float` attachment and divides the alpha back out in the
///   `unpremultiply` compute pass. On a partially covered pixel the two
///   orders of operation differ in the last bits of the colour channels.
/// * **Filter.** zeno computes exact area coverage per scanline cell; the GPU
///   fragment shader evaluates an analytic distance to the path and feathers
///   it over one pixel. Interior and exterior pixels are identical; the
///   disagreement is confined to the antialiased boundary, which for a
///   64×64 rect is a few hundred pixels at most.
/// * **Rounding.** The intermediate attachment is `Rgba16Float` (10-bit
///   mantissa, ~1e-3 relative), while the CPU path stays `f32` throughout.
///
/// So: **every** pixel is required to match within 0.1 per channel, which is
/// far tighter than an antialiasing difference and cannot hide a shape that
/// moved, and the total alpha coverage — the integral the filter difference
/// actually shows up in — is required to match within 1%.
fn assert_paths_agree(cpu: &FrameBuffer, gpu: &FrameBuffer, label: &str) {
    assert_eq!(
        (cpu.width, cpu.height),
        (gpu.width, gpu.height),
        "{label}: resolutions differ"
    );
    let cpu_data = cpu.as_f32();
    let gpu_data = gpu.as_f32();

    let worst = cpu_data
        .chunks_exact(4)
        .zip(gpu_data.chunks_exact(4))
        .enumerate()
        .map(|(i, (a, b))| {
            let delta = a
                .iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max);
            (delta, i, a, b)
        })
        .max_by(|l, r| l.0.total_cmp(&r.0))
        .expect("a non-empty frame");
    let (delta, index, cpu_px, gpu_px) = worst;
    let (x, y) = (index as u32 % cpu.width, index as u32 / cpu.width);
    assert!(
        delta < 0.1,
        "{label}: CPU and GPU disagree by {delta} at ({x},{y}): CPU {cpu_px:?} GPU {gpu_px:?}",
    );

    let cpu_coverage: f32 = cpu_data.iter().skip(3).step_by(4).sum();
    let gpu_coverage: f32 = gpu_data.iter().skip(3).step_by(4).sum();
    let coverage_delta = (cpu_coverage - gpu_coverage).abs() / cpu_coverage.max(1.0);
    eprintln!("{label}: worst channel delta {delta:.5}, coverage delta {coverage_delta:.5}");
    assert!(
        coverage_delta < 0.01,
        "{label}: total coverage differs by {:.3}% (CPU {cpu_coverage}, GPU {gpu_coverage})",
        coverage_delta * 100.0,
    );
}

/// `shape.rect → rasterize → net.out(frame)`, plus the conventional
/// `net.in` (unused by this network).
fn shape_rect_network(center: f32, size: f32) -> (Graph, NodeId) {
    let shape = Node::new(NodeId::new(500), "shape.rect")
        .with_output("output", DataTypeId::GEOMETRY)
        .with_param("center", ParameterValue::vec2(center, center))
        .with_param("width", ParameterValue::Float(size))
        .with_param("height", ParameterValue::Float(size));
    let rasterize = Node::new(NodeId::new(501), "rasterize")
        .with_input("geometry", &[DataTypeId::GEOMETRY])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let in_node = Node::new(NodeId::new(502), net::NET_IN_TYPE_KEY)
        .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
        .with_output(net::PORT_TIME, DataTypeId::SCALAR);
    let out_node = Node::new(NodeId::new(503), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);

    let network = Graph::new()
        .add_node(shape)
        .unwrap()
        .add_node(rasterize)
        .unwrap()
        .add_node(in_node)
        .unwrap()
        .add_node(out_node)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(500),
            OutputPortIndex(0),
            NodeId::new(501),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            NodeId::new(501),
            OutputPortIndex(0),
            NodeId::new(503),
            InputPortIndex(0),
        )
        .unwrap();
    (network, NodeId::new(501))
}

fn build_evaluator(
    gpu: &GpuContext,
    comp: &Composition,
    networks: &[&Graph],
    cpu_rasterize: Option<(NodeId, &Graph)>,
) -> (
    Evaluator,
    ravel_core::composition::compile::CompilationResult,
) {
    let result = compile_composition(comp, Graph::new()).expect("compile succeeds");

    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(gpu);
    let media_frames = MediaFrameCache::standalone();
    let mut evaluator = Evaluator::new();
    register_all_processors(
        &mut evaluator,
        &result.graph,
        gpu,
        &mut shaders,
        &pool,
        &media_frames,
    );
    for network in networks {
        register_all_processors(
            &mut evaluator,
            network,
            gpu,
            &mut shaders,
            &pool,
            &media_frames,
        );
    }
    // Override the registered processor with the CPU reference rasterizer.
    if let Some((rasterize_id, network)) = cpu_rasterize {
        let node = network.node(rasterize_id).unwrap().as_ref().clone();
        evaluator.register(
            rasterize_id,
            Arc::new(ravel_nodes::rasterize::RasterizeProcessor::from_node(&node)),
        );
    }
    (evaluator, result)
}

/// Render `comp`'s single layer network with `path`'s rasterizer.
fn render(
    gpu: &GpuContext,
    comp: &Composition,
    network: &Graph,
    rasterize_id: NodeId,
    ctx: &EvalContext,
    path: Path,
) -> FrameBuffer {
    let doc = Document::default().with_composition(comp.clone());
    let pin = (path == Path::Cpu).then_some((rasterize_id, network));
    let (mut evaluator, result) = build_evaluator(gpu, comp, &[network], pin);
    evaluator.set_document(Arc::new(doc));
    output_frame(
        &evaluator
            .evaluate(&result.graph, result.output_node, ctx)
            .expect("evaluation succeeds"),
    )
}

/// Render with both rasterizers and assert they agree; the returned pair is
/// `(cpu, gpu)` so a caller can assert the picture on each.
fn render_both(
    gpu: &GpuContext,
    comp: &Composition,
    network: &Graph,
    rasterize_id: NodeId,
    ctx: &EvalContext,
    label: &str,
) -> [FrameBuffer; 2] {
    let cpu_frame = render(gpu, comp, network, rasterize_id, ctx, Path::Cpu);
    let gpu_frame = render(gpu, comp, network, rasterize_id, ctx, Path::Gpu);
    assert_paths_agree(&cpu_frame, &gpu_frame, label);
    [cpu_frame, gpu_frame]
}

fn rect_comp(name: &str, resolution: (u32, u32), network: &Graph) -> Composition {
    let mut comp = Composition::new(CompId::new(1), name, resolution, FrameRate::new(30, 1), 300)
        .add_layer(Layer::new(LayerId::new(1), "Rect", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    comp
}

#[test]
fn shape_layer_network_rasterizes_rect_pixels() {
    let Some(gpu) = gpu_or_skip() else { return };
    // 64x64 comp; rect centered at (32, 32) with size 32x32 → interior
    // covers [16, 48) on both axes.
    let (network, rasterize_id) = shape_rect_network(32.0, 32.0);
    let comp = rect_comp("Golden", (64, 64), &network);
    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));

    for fb in render_both(&gpu, &comp, &network, rasterize_id, &ctx, "rect pixels") {
        assert_eq!(fb.width, 64);
        assert_eq!(fb.height, 64);

        // Interior: opaque white (rasterize default Cd).
        for (x, y) in [(32, 32), (20, 20), (44, 44)] {
            let p = pixel(&fb, x, y);
            assert!(p[3] > 0.9, "interior ({x},{y}) covered: {p:?}");
            assert!(p[0] > 0.9 && p[1] > 0.9 && p[2] > 0.9, "default white fill");
        }

        // Exterior: fully transparent.
        for (x, y) in [(4, 4), (60, 4), (4, 60), (60, 60), (32, 8), (8, 32)] {
            let p = pixel(&fb, x, y);
            assert!(p[3] < 1e-6, "exterior ({x},{y}) transparent: {p:?}");
        }

        // Edge rows just inside/outside the rect boundary (y = 16 boundary).
        assert!(pixel(&fb, 32, 17)[3] > 0.5, "just inside top edge");
        assert!(pixel(&fb, 32, 14)[3] < 0.1, "just outside top edge");
    }
}

/// The same network in the `.ravprj` v4 shape: `center_x` / `center_y` as
/// separate Floats, which is what `Document::fold_component_params` upgrades.
fn v4_shape_rect_network(center: f32, size: f32) -> (Graph, NodeId) {
    let (network, rasterize_id) = shape_rect_network(center, size);
    let mut shape = (**network.node(NodeId::new(500)).unwrap()).clone();
    shape.parameters.retain(|p| p.key != "center");
    shape.parameters.insert(
        0,
        ravel_core::graph::Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(center),
        },
    );
    shape.parameters.insert(
        1,
        ravel_core::graph::Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(center),
        },
    );
    (network.replace_node(Arc::new(shape)), rasterize_id)
}

/// A v4 document folded on load renders exactly what the same network written
/// in the v5 shape renders — the fold is a representation change, not a
/// behaviour change. Asserted per rasterizer: the two paths differ from each
/// other by an antialiasing filter, but each is deterministic, so the fold
/// has to reproduce its own path's pixels exactly.
#[test]
fn a_folded_v4_network_renders_the_same_pixels_as_a_v5_one() {
    let Some(gpu) = gpu_or_skip() else { return };
    let (v5_network, rasterize_id) = shape_rect_network(32.0, 32.0);
    let (v4_network, _) = v4_shape_rect_network(32.0, 32.0);

    let comp = Composition::new(
        CompId::new(1),
        "Legacy",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", v4_network).with_time(0, 0, 300));
    let folded = Document::default()
        .with_composition(comp)
        .fold_component_params();
    let folded_network = folded.get_composition(CompId::new(1)).expect("comp").layers[0]
        .network
        .clone();

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
    for path in [Path::Cpu, Path::Gpu] {
        let expected = render(
            &gpu,
            &rect_comp("Golden", (64, 64), &v5_network),
            &v5_network,
            rasterize_id,
            &ctx,
            path,
        );
        let actual = render(
            &gpu,
            &rect_comp("Golden", (64, 64), &folded_network),
            &folded_network,
            rasterize_id,
            &ctx,
            path,
        );
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(
            actual.as_f32(),
            expected.as_f32(),
            "folded v4 pixels match v5 on {path:?}"
        );
        // Guard against both sides rendering an empty frame.
        assert!(
            pixel(&expected, 32, 32)[3] > 0.9,
            "the rect is drawn at all on {path:?}"
        );
    }
}

#[test]
fn shape_layer_scales_comp_coordinates_without_cropping() {
    let Some(gpu) = gpu_or_skip() else { return };
    let (network, rasterize_id) = shape_rect_network(64.0, 32.0);
    let comp = rect_comp("Scaled", (128, 128), &network);
    let ctx =
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64)).with_comp_resolution(comp.resolution);

    for fb in render_both(&gpu, &comp, &network, rasterize_id, &ctx, "scaled rect") {
        assert_eq!((fb.width, fb.height), (64, 64));
        assert!(
            pixel(&fb, 32, 32)[3] > 0.9,
            "comp center lands at canvas center"
        );
        assert!(
            pixel(&fb, 25, 32)[3] > 0.9,
            "scaled rect interior is preserved"
        );
        assert!(pixel(&fb, 15, 32)[3] < 0.1, "rect is not left-top cropped");
    }
}

#[test]
fn unconnected_frame_port_evaluates_to_empty_frame() {
    let Some(gpu) = gpu_or_skip() else { return };
    // A network whose Out `frame` port is unconnected produces a transparent
    // FrameBuffer instead of failing.
    let out_node = Node::new(NodeId::new(510), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
    let network = Graph::new().add_node(out_node).unwrap();

    let mut comp = Composition::new(
        CompId::new(1),
        "Empty",
        (16, 16),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Ghost", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) = build_evaluator(&gpu, &comp, &[&network], None);
    evaluator.set_document(Arc::new(doc));

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = output_frame(&out);

    assert!(
        fb.as_f32().iter().skip(3).step_by(4).all(|a| *a < 1e-6),
        "every pixel transparent"
    );
}

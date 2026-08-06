// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless performance baseline for the evaluation path (Phase 0 of
//! `docs/implementation/done/eval-render-performance-plan.md`).
//!
//! Replays the UI-thread work performed by `NodeEditorPanel` for the plan's
//! measurement scenarios and aggregates the `tracing` span timings that the
//! instrumented crates emit. Run with:
//!
//! ```sh
//! cargo run -p ravel-nodes --release --example perf_baseline
//! ```
//!
//! Requires a GPU adapter. Results are recorded in
//! `docs/implementation/perf-baseline.md`.

use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{BlendMode, Composition, Document, Layer as CompositionLayer};
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ProcessorRegistry as _};
use ravel_core::geometry::{AttributeArray, Geometry};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::{
    EvalRequest, EvalService, EvalWorkerHooks, InvalidationHint, ProcessorSync,
};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData, Vec2};
use ravel_gpu::{GpuContext, ShaderManager, TexturePool};
use ravel_nodes::rasterize::RasterizeProcessor;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context as LayerContext, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

// ---------------------------------------------------------------------------
// Span timing aggregation
// ---------------------------------------------------------------------------

/// Span names this benchmark aggregates; everything else is ignored.
const TRACKED_SPANS: &[&str] = &[
    "evaluate",
    "node_process",
    "gpu_upload",
    "gpu_readback",
    "cpu_rasterize",
    "register_processors",
    "gpu_rasterize",
    "raster_flatten",
    "raster_upload",
    "raster_submit",
];

#[derive(Clone, Copy, Default)]
struct Agg {
    calls: u64,
    total: Duration,
}

#[derive(Clone, Default)]
struct Timings(Arc<Mutex<BTreeMap<String, Agg>>>);

impl Timings {
    fn drain(&self) -> BTreeMap<String, Agg> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

struct SpanTiming {
    start: Instant,
    key: String,
}

struct TimingLayer {
    timings: Timings,
}

#[derive(Default)]
struct TypeKeyVisitor {
    type_key: Option<String>,
}

impl tracing::field::Visit for TypeKeyVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "type_key" {
            self.type_key = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "type_key" {
            self.type_key = Some(value.to_string());
        }
    }
}

impl<S> Layer<S> for TimingLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let name = attrs.metadata().name();
        if !TRACKED_SPANS.contains(&name) {
            return;
        }
        let mut visitor = TypeKeyVisitor::default();
        attrs.record(&mut visitor);
        let key = match visitor.type_key {
            Some(t) => format!("{name}:{t}"),
            None => name.to_string(),
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanTiming {
                start: Instant::now(),
                key,
            });
        }
    }

    fn on_close(&self, id: Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();
        let Some(timing) = extensions.get::<SpanTiming>() else {
            return;
        };
        let elapsed = timing.start.elapsed();
        let mut map = self.timings.0.lock().unwrap();
        let agg = map.entry(timing.key.clone()).or_default();
        agg.calls += 1;
        agg.total += elapsed;
    }
}

// ---------------------------------------------------------------------------
// Benchmark harness
// ---------------------------------------------------------------------------

const RESOLUTION: (u32, u32) = (512, 512);
const SHELL_LAYERS: usize = 10;
/// Layer counts the shell-chain scenarios run at. Two counts in one pass is
/// what makes "readbacks scale with the layer count" checkable from a single
/// run — the completion criterion of `gpu-compositing-plan.md` GPUCOMP-1 —
/// instead of requiring the constant to be edited between runs.
const SHELL_LAYER_COUNTS: [usize; 2] = [3, SHELL_LAYERS];
/// Display resolutions the frame-readback scenario measures. `HIGH-04` asks
/// for the per-frame readback cost at exactly 1080p and 4K.
///
/// `1024x576` is the third one because it is the scale the interactive viewer
/// reads back at: it was a hidden long-edge cap when this baseline was first
/// recorded, and it is now what the default `Half` preview factor
/// (`ViewerResolution`, `crates/ravel-ui/src/panels/viewer.rs`) works out to
/// for a 16:9 1080p composition — 960x540, which is 14% less area than the
/// figure measured here. Keeping the measured figure unchanged is deliberate:
/// the recorded numbers stay comparable run to run, and the pair still answers
/// the question they were added for, which is what full resolution costs
/// against the scale the viewer normally runs at. Read the reduced-scale
/// numbers as a slight over-estimate of what `Half` costs at 1080p, and
/// re-measure per factor when `VRES-5` records the factor comparison.
const READBACK_RESOLUTIONS: [(u32, u32); 3] = [(1024, 576), (1920, 1080), (3840, 2160)];
/// Frames per resolution in the readback scenario.
const READBACK_FRAMES: usize = 20;
/// Resolutions the viewer-path scenario measures: the reduced scale the
/// viewer runs at by default and the full 1080p a user gets by choosing
/// `ViewerResolution::Full` (see [`READBACK_RESOLUTIONS`]).
const VIEWER_PATH_RESOLUTIONS: [(u32, u32); 2] = [(1024, 576), (1920, 1080)];
/// Frames per resolution in the viewer-path scenario, mirroring
/// [`READBACK_FRAMES`].
const VIEWER_PATH_FRAMES: usize = 20;

fn eval_ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), RESOLUTION)
}

/// Emits a fixed gradient FrameBuffer; stand-in for a media/source node.
struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ravel_core::eval::ResolvedParams,
        _scope: &mut dyn ravel_core::eval::EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(self.0.clone()))
    }
}

fn gradient_fb(width: u32, height: u32) -> FrameBuffer {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.25, 1.0]);
        }
    }
    FrameBuffer::from_f32(width, height, data)
}

fn set_float_param(graph: &Graph, node_id: NodeId, key: &str, value: f32) -> Graph {
    let node = graph.node(node_id).expect("node exists");
    let mut updated = (**node).clone();
    set_node_float(&mut updated, key, value);
    graph.clone().replace_node(Arc::new(updated))
}

/// Locates a parameter, panicking when the key is unknown.
///
/// A benchmark that silently kept the default value would report a number for a
/// graph it never actually built, so a renamed key must fail loudly here.
fn param_mut<'a>(node: &'a mut Node, key: &str) -> &'a mut ParameterValue {
    let type_key = node.type_key.clone();
    &mut node
        .parameters
        .iter_mut()
        .find(|p| p.key == key)
        .unwrap_or_else(|| panic!("{type_key} has no parameter {key}"))
        .value
}

fn set_int_param(node: &mut Node, key: &str, value: i32) {
    *param_mut(node, key) = ParameterValue::Int(value);
}

fn set_node_float(node: &mut Node, key: &str, value: f32) {
    *param_mut(node, key) = ParameterValue::Float(value);
}

fn set_node_vec2(node: &mut Node, key: &str, x: f32, y: f32) {
    *param_mut(node, key) = ParameterValue::vec2(x, y);
}

fn set_vec2_param(graph: &Graph, node_id: NodeId, key: &str, x: f32, y: f32) -> Graph {
    let node = graph.node(node_id).expect("node exists");
    let mut updated = (**node).clone();
    set_node_vec2(&mut updated, key, x, y);
    graph.clone().replace_node(Arc::new(updated))
}

fn set_str_param(node: &mut Node, key: &str, value: &str) {
    *param_mut(node, key) = ParameterValue::String(value.into());
}

fn set_bool_param(node: &mut Node, key: &str, value: bool) {
    *param_mut(node, key) = ParameterValue::Bool(value);
}

struct WallStats {
    iterations: usize,
    mean: Duration,
    min: Duration,
    max: Duration,
}

fn wall_stats(samples: &[Duration]) -> WallStats {
    let total: Duration = samples.iter().sum();
    WallStats {
        iterations: samples.len(),
        mean: total / samples.len().max(1) as u32,
        min: samples.iter().min().copied().unwrap_or_default(),
        max: samples.iter().max().copied().unwrap_or_default(),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn report(
    scenario: &str,
    wall: &WallStats,
    timings: BTreeMap<String, Agg>,
    transfers: ravel_gpu::transfer::stats::TransferSnapshot,
) {
    println!("\n## {scenario}");
    println!(
        "wall/iter: mean {:.2} ms, min {:.2} ms, max {:.2} ms ({} iters)",
        ms(wall.mean),
        ms(wall.min),
        ms(wall.max),
        wall.iterations
    );
    println!(
        "transfers: {} uploads ({:.1} MB), {} readbacks ({:.1} MB), \
         {} staging buffers created",
        transfers.uploads,
        transfers.upload_bytes as f64 / 1e6,
        transfers.readbacks,
        transfers.readback_bytes as f64 / 1e6,
        transfers.staging_buffers_created,
    );
    println!("| span | calls | total ms | mean ms |");
    println!("|------|-------|----------|---------|");
    for (key, agg) in timings {
        println!(
            "| {key} | {} | {:.2} | {:.3} |",
            agg.calls,
            ms(agg.total),
            ms(agg.total) / agg.calls.max(1) as f64
        );
    }
}

/// Runs `iters` iterations of `f`, returning per-iteration wall durations.
fn run_scenario(iters: usize, mut f: impl FnMut(usize)) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(iters);
    for i in 0..iters {
        let start = Instant::now();
        f(i);
        samples.push(start.elapsed());
    }
    samples
}

// ---------------------------------------------------------------------------
// Graphs
// ---------------------------------------------------------------------------

const SRC: u64 = 1;
const BLUR: u64 = 2;
const CC: u64 = 3;
const MERGE: u64 = 4;
const SHAPE: u64 = 10;
const GRID: u64 = 11;
const FALLOFF: u64 = 12;
const NOISE: u64 = 13;
const ATTR_FIELD: u64 = 14;
const FIELD_ADD: u64 = 15;
const FIELD_MUL: u64 = 16;
const APPLY: u64 = 17;
const RAST: u64 = 18;

fn nid(raw: u64) -> NodeId {
    NodeId::new(raw)
}

/// source → blur → color_correct → merge.A, source → merge.B
fn effect_graph(registry: &NodeRegistry) -> Graph {
    let source =
        Node::new(nid(SRC), "bench.source").with_output("output", DataTypeId::FRAME_BUFFER);
    let blur = registry.create_node("blur", nid(BLUR)).unwrap();
    let cc = registry.create_node("color_correct", nid(CC)).unwrap();
    let merge = registry.create_node("merge", nid(MERGE)).unwrap();

    Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(blur)
        .unwrap()
        .add_node(cc)
        .unwrap()
        .add_node(merge)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            nid(SRC),
            OutputPortIndex(0),
            nid(BLUR),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            nid(BLUR),
            OutputPortIndex(0),
            nid(CC),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(3),
            nid(CC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(4),
            nid(SRC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(1),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// Geometry scaling (GPU-0: `gpu-resident-geometry-plan.md` phase 0)
// ---------------------------------------------------------------------------

/// Element counts the geometry scenarios sweep. The 100k row is the one the
/// plan's decision criterion is written against; 1M is there to show the slope
/// past it.
const GEO_COUNTS: [usize; 4] = [500, 10_000, 100_000, 1_000_000];

/// Frames measured per geometry scenario. Every frame is deliberately uncached,
/// so the large counts cost whole seconds each and get fewer samples.
fn geo_frames(count: usize) -> usize {
    match count {
        ..=10_000 => 30,
        10_001..=100_000 => 15,
        _ => 5,
    }
}

/// Factors `count` into `scatter.grid` dimensions inside the template's
/// 1..=1000 parameter range.
fn grid_dims(count: usize) -> (i32, i32) {
    let mut rows = (count as f64).sqrt().round() as usize;
    while rows > 1 && (!count.is_multiple_of(rows) || count / rows > 1000) {
        rows -= 1;
    }
    ((count / rows) as i32, rows as i32)
}

/// The four chains the plan's phase 0 table names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GeoStage {
    /// (A) `shape.rect → scatter.grid`
    Scatter,
    /// (B) A + `field.falloff → field.apply`
    OneField,
    /// (C) B + `field.noise` and `field.attribute`, composed three deep
    ThreeFields,
    /// (D) C + GPU `rasterize`, end to end
    EndToEnd,
}

impl GeoStage {
    const ALL: [Self; 4] = [
        Self::Scatter,
        Self::OneField,
        Self::ThreeFields,
        Self::EndToEnd,
    ];

    fn tag(self) -> &'static str {
        match self {
            Self::Scatter => "A",
            Self::OneField => "B",
            Self::ThreeFields => "C",
            Self::EndToEnd => "D",
        }
    }

    fn chain(self) -> &'static str {
        match self {
            Self::Scatter => "shape.rect → scatter.grid",
            Self::OneField => "+ field.falloff → field.apply(P)",
            Self::ThreeFields => "+ (falloff + noise) × attribute(index) → field.apply(P)",
            Self::EndToEnd => "+ rasterize (GPU), end to end",
        }
    }

    fn output(self) -> NodeId {
        match self {
            Self::Scatter => nid(GRID),
            Self::OneField | Self::ThreeFields => nid(APPLY),
            Self::EndToEnd => nid(RAST),
        }
    }
}

/// Builds the phase-0 chain for `stage` at `count` instances.
///
/// `field.apply` writes into the instance `P` column, which is what
/// per-instance modulation does in practice (`per-instance-modulation-plan.md`)
/// and what forces every element through the field evaluator.
fn geo_graph(registry: &NodeRegistry, count: usize, stage: GeoStage) -> Graph {
    let (count_x, count_y) = grid_dims(count);
    let shape = registry.create_node("shape.rect", nid(SHAPE)).unwrap();
    let mut grid = registry.create_node("scatter.grid", nid(GRID)).unwrap();
    set_int_param(&mut grid, "count_x", count_x);
    set_int_param(&mut grid, "count_y", count_y);
    set_node_vec2(&mut grid, "spacing", 8.0, 8.0);

    let mut graph = Graph::new()
        .add_node(shape)
        .unwrap()
        .add_node(grid)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            nid(SHAPE),
            OutputPortIndex(0),
            nid(GRID),
            InputPortIndex(0),
        )
        .unwrap();

    if stage == GeoStage::Scatter {
        return graph;
    }

    let mut falloff = registry.create_node("field.falloff", nid(FALLOFF)).unwrap();
    // The default 1.0 outer radius would leave every element outside the
    // falloff; a radius on the order of the grid extent keeps the sampled
    // values non-degenerate.
    set_node_float(&mut falloff, "outer_radius", 500.0);
    let mut apply = registry.create_node("field.apply", nid(APPLY)).unwrap();
    set_str_param(&mut apply, "domain", "instance");
    set_str_param(&mut apply, "target", ravel_core::geometry::names::P);
    set_str_param(&mut apply, "combine", "add");

    graph = graph
        .add_node(falloff)
        .unwrap()
        .add_node(apply)
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            nid(GRID),
            OutputPortIndex(0),
            nid(APPLY),
            InputPortIndex(0),
        )
        .unwrap();

    let field_output = if stage == GeoStage::OneField {
        nid(FALLOFF)
    } else {
        let mut noise = registry.create_node("field.noise", nid(NOISE)).unwrap();
        set_node_float(&mut noise, "frequency", 0.01);
        let mut attribute = registry
            .create_node("field.attribute", nid(ATTR_FIELD))
            .unwrap();
        set_str_param(&mut attribute, "name", ravel_core::geometry::names::INDEX);
        set_bool_param(&mut attribute, "normalize", true);
        let add = registry.create_node("field.add", nid(FIELD_ADD)).unwrap();
        let multiply = registry
            .create_node("field.multiply", nid(FIELD_MUL))
            .unwrap();

        graph = graph
            .add_node(noise)
            .unwrap()
            .add_node(attribute)
            .unwrap()
            .add_node(add)
            .unwrap()
            .add_node(multiply)
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                nid(FALLOFF),
                OutputPortIndex(0),
                nid(FIELD_ADD),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                nid(NOISE),
                OutputPortIndex(0),
                nid(FIELD_ADD),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(5),
                nid(FIELD_ADD),
                OutputPortIndex(0),
                nid(FIELD_MUL),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(6),
                nid(ATTR_FIELD),
                OutputPortIndex(0),
                nid(FIELD_MUL),
                InputPortIndex(1),
            )
            .unwrap();
        nid(FIELD_MUL)
    };

    graph = graph
        .add_edge(
            EdgeId::new(7),
            field_output,
            OutputPortIndex(0),
            nid(APPLY),
            InputPortIndex(1),
        )
        .unwrap();

    if stage == GeoStage::EndToEnd {
        let rasterize = registry.create_node("rasterize", nid(RAST)).unwrap();
        graph = graph
            .add_node(rasterize)
            .unwrap()
            .add_edge(
                EdgeId::new(8),
                nid(APPLY),
                OutputPortIndex(0),
                nid(RAST),
                InputPortIndex(0),
            )
            .unwrap();
    }

    graph
}

/// Per-frame mean of one tracked span.
fn span_ms(timings: &BTreeMap<String, Agg>, key: &str, frames: usize) -> f64 {
    timings
        .get(key)
        .map_or(0.0, |agg| ms(agg.total) / frames as f64)
}

/// Per-frame mean of every `node_process` span, i.e. CPU-side node work.
fn node_process_ms(timings: &BTreeMap<String, Agg>, frames: usize) -> f64 {
    timings
        .iter()
        .filter(|(key, _)| key.starts_with("node_process"))
        .map(|(_, agg)| ms(agg.total))
        .sum::<f64>()
        / frames as f64
}

/// shape.rect → scatter.grid (25 × 20 = 500 instances)
fn scatter_graph(registry: &NodeRegistry) -> Graph {
    let shape = registry.create_node("shape.rect", nid(SHAPE)).unwrap();
    let mut grid = registry.create_node("scatter.grid", nid(GRID)).unwrap();
    set_int_param(&mut grid, "count_x", 25);
    set_int_param(&mut grid, "count_y", 20);

    Graph::new()
        .add_node(shape)
        .unwrap()
        .add_node(grid)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            nid(SHAPE),
            OutputPortIndex(0),
            nid(GRID),
            InputPortIndex(0),
        )
        .unwrap()
}

/// N layer-local `source -> blur -> net.out` networks wrapped by the shell
/// compiler. Every layer ends GPU-resident, while the non-identity transform,
/// opacity and mixed merges force the current CPU shell chain to read it back.
///
/// `resolution` is the composition's own resolution. Every scenario recorded in
/// `docs/implementation/perf-baseline.md` passes [`RESOLUTION`]; the viewer-path
/// scenario is the one that varies it, so those numbers stay comparable.
fn shell_composition(
    registry: &NodeRegistry,
    layers: usize,
    resolution: (u32, u32),
) -> (Graph, NodeId, Arc<Document>) {
    let blend_modes = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
    ];
    let mut comp = Composition::new(
        CompId::new(1),
        "Shell benchmark",
        resolution,
        FrameRate::new(30, 1),
        300,
    );

    for i in 0..layers {
        let base = 1_000 + i as u64 * 10;
        let source_id = nid(base);
        let blur_id = nid(base + 1);
        let out_id = nid(base + 2);
        let source =
            Node::new(source_id, "bench.source").with_output("output", DataTypeId::FRAME_BUFFER);
        let blur = registry.create_node("blur", blur_id).unwrap();
        let out = Node::new(out_id, net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(blur)
            .unwrap()
            .add_node(out)
            .unwrap()
            .add_edge(
                EdgeId::new(base),
                source_id,
                OutputPortIndex(0),
                blur_id,
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(base + 1),
                blur_id,
                OutputPortIndex(0),
                out_id,
                InputPortIndex(0),
            )
            .unwrap();

        let mut layer = CompositionLayer::new(
            LayerId::new(i as u64 + 1),
            format!("GPU layer {}", i + 1),
            network,
        )
        .with_time(0, 0, 300)
        .with_blend_mode(blend_modes[i % blend_modes.len()]);
        layer.transform.position[0] = AnimationChannel::constant(1.0 + i as f32);
        layer.opacity = AnimationChannel::constant(0.8);
        comp = comp.add_layer(layer);
    }

    let compiled = compile_composition(&comp, Graph::new()).expect("composition compiles");
    let document = Arc::new(Document::default().with_composition(comp));
    (compiled.graph, compiled.output_node, document)
}

/// Mirrors `NodeEditorPanel::sync_processors`: fresh evaluator, re-register
/// every processor (GPU pipelines included), plus the bench source.
fn build_evaluator(
    graph: &Graph,
    gpu: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
    source_fb: Option<&FrameBuffer>,
) -> Evaluator {
    let mut evaluator = Evaluator::new();
    ravel_nodes::register_all_processors(&mut evaluator, graph, gpu, shaders, pool);
    if let Some(fb) = source_fb {
        evaluator.register(nid(SRC), Arc::new(FbSource(fb.clone())));
    }
    evaluator
}

/// Evaluator for a compiled shell composition, driven without the
/// `EvalService`: processors for the compiled graph and for every layer
/// network, the bench source inside each network, and the `Document` the shell
/// nodes read their layer state from.
///
/// This is the registration policy `BenchHooks` applies on a structural sync,
/// performed once up front instead — the viewer-path scenario measures the
/// evaluator itself, so routing it through the worker would put request
/// posting and latest-wins coalescing inside the number.
fn build_shell_evaluator(
    graph: &Graph,
    document: &Arc<Document>,
    gpu: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
    source_fb: &FrameBuffer,
) -> Evaluator {
    let mut evaluator = Evaluator::new();
    ravel_nodes::register_all_processors(&mut evaluator, graph, gpu, shaders, pool);
    for comp in document.compositions.values() {
        for layer in &comp.layers {
            ravel_nodes::register_all_processors(
                &mut evaluator,
                &layer.network,
                gpu,
                shaders,
                pool,
            );
            for node in layer.network.nodes() {
                if node.type_key == "bench.source" {
                    evaluator.register(node.id, Arc::new(FbSource(source_fb.clone())));
                }
            }
        }
    }
    evaluator.set_document(document.clone());
    evaluator
}

/// The viewer's exit from the GPU, exactly as `GpuEvalHooks::finalize`
/// (`crates/ravel-app/src/eval_hooks.rs`) performs it: one `to_frame_buffer()`
/// per displayed frame.
///
/// A non-GPU output would mean the scenario is timing a chain that never became
/// GPU-resident — a number for the wrong path — so that case panics instead of
/// being reported.
fn viewer_readback(value: &dyn NodeData) -> anyhow::Result<FrameBuffer> {
    let frame = value.downcast_ref::<ravel_gpu::GpuFrameBuffer>().expect(
        "viewer path output must be GPU-resident: the shell chain returned a \
         non-GpuFrameBuffer value, so this scenario would be measuring a CPU path",
    );
    Ok(frame.to_frame_buffer()?)
}

/// Mirrors the ad-hoc Geometry rasterize in `evaluate_for_viewer`.
fn adhoc_rasterize(data: &dyn NodeData, ctx: &EvalContext) -> Option<FrameBuffer> {
    let geo = data
        .downcast_ref::<ravel_core::geometry::Geometry>()?
        .clone();
    let rast_node = Node::new(NodeId::new(u64::MAX), "rasterize")
        .with_param("fill", ParameterValue::Bool(true))
        .with_param("stroke_width", ParameterValue::Float(0.0));
    let proc = RasterizeProcessor::from_node(&rast_node);
    let input: Arc<dyn NodeData> = Arc::new(geo);
    let mut scope = ravel_core::eval::Evaluator::new();
    proc.process(
        &rast_node,
        ctx,
        &[Some(input)],
        &ravel_core::eval::ResolvedParams::default(),
        &mut scope,
    )
    .ok()
    .and_then(|d| d.downcast_ref::<FrameBuffer>().cloned())
}

/// CPU-side replica of the Viewer's `paint_framebuffer` run-merge loop.
/// Returns the number of quads that would be submitted to GPUI.
fn count_paint_quads(fb: &FrameBuffer, avail: (f32, f32)) -> usize {
    let (avail_w, avail_h) = avail;
    let scale = (avail_w / fb.width as f32)
        .min(avail_h / fb.height as f32)
        .min(1.0);
    let step = 1.0 / scale;
    let pixel = scale.max(1.0);
    let cols = ((fb.width as f32 * scale) / pixel).ceil() as usize;
    let rows = ((fb.height as f32 * scale) / pixel).ceil() as usize;

    let px = fb.as_f32();
    let mut quads = 0usize;
    for row in 0..rows {
        let src_y = (row as f32 * step) as u32;
        if src_y >= fb.height {
            continue;
        }
        let mut run_color: Option<[f32; 4]> = None;
        for col in 0..cols {
            let src_x = (col as f32 * step) as u32;
            let color = if src_x < fb.width {
                let idx = ((src_y * fb.width + src_x) * 4) as usize;
                [px[idx], px[idx + 1], px[idx + 2], px[idx + 3]]
            } else {
                [0.0; 4]
            };
            match run_color {
                Some(current) if current == color => {}
                Some(current) => {
                    if current[3] >= 1e-6 {
                        quads += 1;
                    }
                    run_color = Some(color);
                }
                None => run_color = Some(color),
            }
        }
        if let Some(current) = run_color
            && current[3] >= 1e-6
        {
            quads += 1;
        }
    }
    quads
}

/// Worker hooks used by the `EvalService` scenarios: mirrors
/// `GpuEvalHooks`' processor registration policy against the bench graphs.
struct BenchHooks {
    gpu: GpuContext,
    shaders: ShaderManager,
    pool: Arc<Mutex<TexturePool>>,
    source_fb: FrameBuffer,
}

impl EvalWorkerHooks for BenchHooks {
    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    ) {
        match hint {
            InvalidationHint::None => {}
            InvalidationHint::Params(ids) => {
                for id in ids {
                    if let Some(node) = graph.node(*id)
                        && let Some(proc) = ravel_nodes::processor_for_node(
                            node,
                            &self.gpu,
                            &mut self.shaders,
                            &self.pool,
                        )
                    {
                        evaluator.register(*id, proc);
                    }
                }
            }
            InvalidationHint::Structural => {
                // The service has already reset the evaluator.
                ravel_nodes::register_all_processors(
                    evaluator,
                    graph,
                    &self.gpu,
                    &mut self.shaders,
                    &self.pool,
                );
                evaluator.register(nid(SRC), Arc::new(FbSource(self.source_fb.clone())));
                if let Some(document) = document {
                    for comp in document.compositions.values() {
                        for layer in &comp.layers {
                            ravel_nodes::register_all_processors(
                                evaluator,
                                &layer.network,
                                &self.gpu,
                                &mut self.shaders,
                                &self.pool,
                            );
                            for node in layer.network.nodes() {
                                if node.type_key == "bench.source" {
                                    evaluator.register(
                                        node.id,
                                        Arc::new(FbSource(self.source_fb.clone())),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Particle proxy (`particle-plan.md` units 2 and 6)
// ---------------------------------------------------------------------------

/// Point counts the particle proxy sweeps. `particle-plan.md` states the target
/// as "100k points or more", which is why the middle row is the decisive one.
const PARTICLE_COUNTS: [usize; 3] = [10_000, 100_000, 1_000_000];
/// Frames per particle scenario (3 s at 30 fps would be 90; a sim step is
/// identical frame to frame, so fewer samples suffice).
const PARTICLE_FRAMES: usize = 30;
const PARTICLE_DT: f32 = 1.0 / 30.0;

/// Point-domain geometry standing in for particle state: free points with no
/// primitives, which the rasterizer draws as circle sprites.
///
/// This is a **proxy**: `particle.simulate` (`particle-plan.md` unit 2) does
/// not exist yet, so the step below is a hand-written explicit-Euler
/// integration over the same columns a real solver would own.
struct ParticleState {
    geometry: Geometry,
    velocity: Vec<Vec2>,
}

impl ParticleState {
    fn new(count: usize) -> Self {
        let side = (count as f64).sqrt().max(1.0) as usize;
        let positions: Vec<Vec2> = (0..count)
            .map(|i| {
                let x = (i % side) as f32 * 0.7 - 180.0;
                let y = (i / side) as f32 * 0.7 - 180.0;
                Vec2(x, y)
            })
            .collect();
        let velocity: Vec<Vec2> = (0..count)
            .map(|i| Vec2((i % 13) as f32 - 6.0, (i % 7) as f32 - 3.0))
            .collect();

        let mut geometry = Geometry::from_points(positions);
        geometry
            .points_mut()
            .insert(
                ravel_core::geometry::names::PSCALE,
                AttributeArray::F32(vec![1.5; count]),
            )
            .expect("pscale column matches the point count");
        Self { geometry, velocity }
    }

    fn count(&self) -> usize {
        self.velocity.len()
    }

    /// One explicit-Euler step: sample an analytic force, integrate velocity,
    /// integrate position. Six flops per axis, which is the order a real force
    /// stack (`field.*` sampled per point) costs.
    fn step(&mut self, time: f32) {
        let positions = self
            .geometry
            .points_mut()
            .make_mut(ravel_core::geometry::names::P)
            .expect("P column exists")
            .as_vec2_mut(ravel_core::geometry::names::P)
            .expect("P is Vec2");
        for (position, velocity) in positions.iter_mut().zip(self.velocity.iter_mut()) {
            let force = particle_force(*position, time);
            velocity.0 += force.0 * PARTICLE_DT;
            velocity.1 += force.1 * PARTICLE_DT;
            position.0 += velocity.0 * PARTICLE_DT;
            position.1 += velocity.1 * PARTICLE_DT;
        }
    }

    /// The same step over rayon's thread pool, to quantify how much headroom a
    /// CPU solver still has before the GPU is the only option left.
    fn step_parallel(&mut self, time: f32) {
        use rayon::prelude::*;
        let positions = self
            .geometry
            .points_mut()
            .make_mut(ravel_core::geometry::names::P)
            .expect("P column exists")
            .as_vec2_mut(ravel_core::geometry::names::P)
            .expect("P is Vec2");
        positions
            .par_iter_mut()
            .zip(self.velocity.par_iter_mut())
            .for_each(|(position, velocity)| {
                let force = particle_force(*position, time);
                velocity.0 += force.0 * PARTICLE_DT;
                velocity.1 += force.1 * PARTICLE_DT;
                position.0 += velocity.0 * PARTICLE_DT;
                position.1 += velocity.1 * PARTICLE_DT;
            });
    }
}

/// Analytic swirl plus a sinusoidal component; a stand-in for a force stack.
fn particle_force(position: Vec2, time: f32) -> Vec2 {
    let swirl = Vec2(-position.1 * 0.02, position.0 * 0.02);
    let wobble = Vec2(
        (position.1 * 0.05 + time).sin() * 12.0,
        (position.0 * 0.05 - time).cos() * 12.0,
    );
    Vec2(swirl.0 + wobble.0, swirl.1 + wobble.1)
}

// The GPU-resident particle state read-back proxy used to live here. It
// measured a *buffer* read-back (`copy_buffer_to_buffer` into a mappable
// buffer, mapped range walked sparsely, staging allocated outside the timed
// region) by driving `wgpu` directly through `GpuContext::device()`.
//
// `GPUBK-4` closed those accessors, and `ravel-gpu`'s transfer abstraction
// covers textures only — there is no buffer read-back to re-express it
// against. Rewriting it as a texture read-back would change the measured
// quantity (a detiling `copy_texture_to_buffer`, row padding, and a full copy
// out of the mapped range instead of a sparse walk), so the numbers would no
// longer be comparable with the ones already recorded in
// `docs/implementation/perf-baseline.md`. The harness was removed rather than
// silently redefined; the section there records what re-instating it needs.

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    let timings = Timings::default();
    tracing_subscriber::registry()
        .with(TimingLayer {
            timings: timings.clone(),
        })
        .init();

    let gpu = GpuContext::new_blocking()?;
    let mut shaders = ShaderManager::new(gpu.clone());
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let ctx = eval_ctx();
    let source_fb = gradient_fb(RESOLUTION.0, RESOLUTION.1);
    let pool = ravel_nodes::shared_texture_pool(&gpu);
    let transfer_stats = || gpu.transfer_stats();

    println!("# perf_baseline ({}x{})", RESOLUTION.0, RESOLUTION.1);

    // -- Scenario (a): selection switching over an unchanged graph ----------
    // First evaluation warms the cache; the loop then alternates the pulled
    // output node like clicking between two nodes in the editor.
    {
        let graph = effect_graph(&registry);
        let mut evaluator = build_evaluator(&graph, &gpu, &mut shaders, &pool, Some(&source_fb));
        evaluator.evaluate(&graph, nid(MERGE), &ctx)?;
        timings.drain();
        let before = transfer_stats();
        let samples = run_scenario(20, |i| {
            let target = if i % 2 == 0 { nid(BLUR) } else { nid(MERGE) };
            evaluator.evaluate(&graph, target, &ctx).unwrap();
        });
        report(
            "(a) selection switch, warm cache",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
    }

    // -- Scenario (b): blur radius scrub, current UI path -------------------
    // Mirrors the evaluation-heavy subset of apply_property_change: replace
    // node, rebuild the evaluator and every processor, re-evaluate the
    // viewer output. 90 ticks ≈ 3 s scrub. Excluded UI-side work (cheap,
    // needs a window): node-size recompute, undo push, ViewerFrame
    // publication, GPUI notify/paint.
    {
        let mut graph = effect_graph(&registry);
        timings.drain();
        let before = transfer_stats();
        let start_all = Instant::now();
        let samples = run_scenario(90, |i| {
            graph = set_float_param(&graph, nid(BLUR), "radius", 1.0 + i as f32 * 0.25);
            let mut evaluator =
                build_evaluator(&graph, &gpu, &mut shaders, &pool, Some(&source_fb));
            evaluator.evaluate(&graph, nid(MERGE), &ctx).unwrap();
        });
        // Since Phase 2, evaluation submits GPU work without waiting for it;
        // include completion so the numbers cover finished frames.
        gpu.wait();
        let total = start_all.elapsed();
        report(
            "(b) blur radius scrub — current path (evaluator rebuilt per change)",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!(
            "end-to-end incl. GPU completion: {:.2} ms total, {:.2} ms/tick",
            ms(total),
            ms(total) / 90.0
        );
    }

    // -- Scenario (b'): blur radius scrub, re-register changed node only ----
    // Hypothetical cheaper path: keep the evaluator and its cache, rebuild
    // only the edited node's processor (processors capture parameter values
    // at construction), and re-evaluate. `register` marks the node dirty, so
    // downstream freshness propagation recomputes cc/merge but not the
    // source. Quantifies how much of (b) is the full evaluator rebuild.
    {
        let mut graph = effect_graph(&registry);
        let mut evaluator = build_evaluator(&graph, &gpu, &mut shaders, &pool, Some(&source_fb));
        evaluator.evaluate(&graph, nid(MERGE), &ctx)?;
        timings.drain();
        let before = transfer_stats();
        let start_all = Instant::now();
        let samples = run_scenario(90, |i| {
            graph = set_float_param(&graph, nid(BLUR), "radius", 1.0 + i as f32 * 0.25);
            let blur_node = graph.node(nid(BLUR)).unwrap().clone();
            evaluator.register(
                nid(BLUR),
                Arc::new(ravel_nodes::blur::BlurProcessor::new(
                    gpu.clone(),
                    &mut shaders,
                    pool.clone(),
                    &blur_node,
                )),
            );
            evaluator.evaluate(&graph, nid(MERGE), &ctx).unwrap();
        });
        gpu.wait();
        let total = start_all.elapsed();
        report(
            "(b') blur radius scrub — re-register changed node only",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!(
            "end-to-end incl. GPU completion: {:.2} ms total, {:.2} ms/tick",
            ms(total),
            ms(total) / 90.0
        );
    }

    // -- Scenario (b''): blur radius scrub via EvalService (Phase 1) --------
    // The UI thread only posts requests; the worker evaluates latest-wins.
    // `wall/iter` here is the UI-thread cost per scrub tick; the summary
    // line reports end-to-end completion and how many evaluations actually
    // ran after coalescing.
    {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let evaluations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let evaluations_worker = evaluations.clone();
        let mut service = EvalService::spawn(
            BenchHooks {
                gpu: gpu.clone(),
                shaders: ShaderManager::new(gpu.clone()),
                pool: ravel_nodes::shared_texture_pool(&gpu),
                source_fb: source_fb.clone(),
            },
            move |update| {
                if update.result.is_ok() {
                    evaluations_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = done_tx.send(update.generation);
            },
        );

        let mut graph = effect_graph(&registry);
        timings.drain();
        let before = transfer_stats();
        let start_all = Instant::now();
        let samples = run_scenario(90, |i| {
            graph = set_float_param(&graph, nid(BLUR), "radius", 1.0 + i as f32 * 0.25);
            service.request(EvalRequest {
                graph: graph.clone(),
                node: nid(MERGE),
                path: Vec::new(),
                ctx,
                document: None,
                hint: InvalidationHint::Params(vec![nid(BLUR)]),
            });
        });
        let final_generation = service.latest_generation();
        loop {
            let generation = done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("eval service completion");
            if generation == final_generation {
                break;
            }
        }
        gpu.wait();
        let total = start_all.elapsed();
        report(
            "(b'') blur radius scrub — EvalService background path (UI-thread cost)",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!(
            "end-to-end: {:.2} ms for 90 ticks; {} evaluations after latest-wins coalescing",
            ms(total),
            evaluations.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    // -- Scenario (e): 30 fps playback via PlaybackClock + EvalService ------
    // Mirrors the PlaybackController tick loop
    // (`docs/implementation/done/playback-foundation-plan.md`, unit 3): wake every
    // frame interval, post one request whenever the clock's frame advanced.
    // The blur radius is animated per frame so every frame does real GPU
    // work (the demo graph has no time-dependent node yet); latest-wins
    // coalescing absorbs any backlog. `wall/iter` is the UI-thread cost of
    // one published frame (frame derivation + request posting).
    {
        use ravel_core::runtime::playback::{PlaybackClock, PlaybackState};

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let evaluations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let evaluations_worker = evaluations.clone();
        let mut service = EvalService::spawn(
            BenchHooks {
                gpu: gpu.clone(),
                shaders: ShaderManager::new(gpu.clone()),
                pool: ravel_nodes::shared_texture_pool(&gpu),
                source_fb: source_fb.clone(),
            },
            move |update| {
                if update.result.is_ok() {
                    evaluations_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = done_tx.send(update.generation);
            },
        );

        const PLAY_FRAMES: u64 = 90; // 3 s at 30 fps
        let fps = FrameRate::new(30, 1);
        let interval = Duration::from_nanos(1_000_000_000 / 30);
        let mut graph = effect_graph(&registry);
        let mut clock = PlaybackClock::new(fps, PLAY_FRAMES);

        timings.drain();
        let before = transfer_stats();
        let start = Instant::now();
        clock.play(start);
        let mut last_frame = 0u64;
        let mut published = 1u64;
        let mut tick_skipped = 0u64;
        let mut samples = Vec::new();
        graph = set_float_param(&graph, nid(BLUR), "radius", 1.0);
        service.request(EvalRequest {
            graph: graph.clone(),
            node: nid(MERGE),
            path: Vec::new(),
            ctx: EvalContext::new(0, fps, RESOLUTION),
            document: None,
            hint: InvalidationHint::Params(vec![nid(BLUR)]),
        });
        loop {
            std::thread::sleep(interval);
            let tick_start = Instant::now();
            let frame = clock.current_frame(tick_start);
            if frame != last_frame {
                if frame > last_frame + 1 {
                    tick_skipped += frame - last_frame - 1;
                }
                last_frame = frame;
                published += 1;
                graph = set_float_param(&graph, nid(BLUR), "radius", 1.0 + frame as f32 * 0.25);
                service.request(EvalRequest {
                    graph: graph.clone(),
                    node: nid(MERGE),
                    path: Vec::new(),
                    ctx: EvalContext::new(frame, fps, RESOLUTION),
                    document: None,
                    hint: InvalidationHint::Params(vec![nid(BLUR)]),
                });
                samples.push(tick_start.elapsed());
            }
            if clock.state() != PlaybackState::Playing {
                break;
            }
        }
        let final_generation = service.latest_generation();
        loop {
            let generation = done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("eval service completion");
            if generation == final_generation {
                break;
            }
        }
        gpu.wait();
        let total = start.elapsed();
        let evals = evaluations.load(std::sync::atomic::Ordering::SeqCst) as u64;
        report(
            "(e) 30 fps playback — clock-paced EvalService requests (UI-thread cost)",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!(
            "playback: {PLAY_FRAMES} frames in {:.2} s → {:.1} fps evaluated; \
             {published} frames published, {tick_skipped} skipped by tick jitter, \
             {} coalesced by latest-wins",
            total.as_secs_f64(),
            evals as f64 / total.as_secs_f64(),
            published.saturating_sub(evals),
        );
    }

    // -- Shell chain: N GPU-ending layers via EvalService ------------------
    // The unpaced scrub demonstrates latest-wins request posting, while the
    // clock-paced form exercises sustained evaluation at playback cadence.
    for layers in SHELL_LAYER_COUNTS {
        let (graph, output, document) = shell_composition(&registry, layers, RESOLUTION);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let evaluations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let evaluations_worker = evaluations.clone();
        let mut service = EvalService::spawn(
            BenchHooks {
                gpu: gpu.clone(),
                shaders: ShaderManager::new(gpu.clone()),
                pool: ravel_nodes::shared_texture_pool(&gpu),
                source_fb: source_fb.clone(),
            },
            move |update| {
                if update.result.is_ok() {
                    evaluations_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = done_tx.send(update.generation);
            },
        );

        timings.drain();
        let before = transfer_stats();
        let before_dispatch = gpu.dispatch_stats();
        let start_all = Instant::now();
        let samples = run_scenario(90, |frame| {
            service.request(EvalRequest {
                graph: graph.clone(),
                node: output,
                path: Vec::new(),
                ctx: EvalContext::new(frame as u64, FrameRate::new(30, 1), RESOLUTION),
                document: Some(document.clone()),
                hint: InvalidationHint::None,
            });
        });
        let final_generation = service.latest_generation();
        loop {
            let generation = done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("shell scrub completion");
            if generation == final_generation {
                break;
            }
        }
        gpu.wait();
        let total = start_all.elapsed();
        report(
            &format!("(f) {layers}-layer shell chain scrub — EvalService"),
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        let evaluations = evaluations.load(std::sync::atomic::Ordering::SeqCst);
        let dispatch = before_dispatch.delta(&gpu.dispatch_stats());
        println!(
            "end-to-end: {:.2} ms for 90 ticks; {} evaluations after latest-wins coalescing",
            ms(total),
            evaluations
        );
        println!(
            "dispatch submits: {} ({:.2} / completed evaluation), \
             recorded passes: {} ({:.2} / completed evaluation)",
            dispatch.submits,
            dispatch.submits as f64 / evaluations.max(1) as f64,
            dispatch.dispatches,
            dispatch.dispatches as f64 / evaluations.max(1) as f64
        );
    }

    for layers in SHELL_LAYER_COUNTS {
        use ravel_core::runtime::playback::{PlaybackClock, PlaybackState};

        let (graph, output, document) = shell_composition(&registry, layers, RESOLUTION);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let evaluations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let evaluations_worker = evaluations.clone();
        let mut service = EvalService::spawn(
            BenchHooks {
                gpu: gpu.clone(),
                shaders: ShaderManager::new(gpu.clone()),
                pool: ravel_nodes::shared_texture_pool(&gpu),
                source_fb: source_fb.clone(),
            },
            move |update| {
                if update.result.is_ok() {
                    evaluations_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = done_tx.send(update.generation);
            },
        );

        const PLAY_FRAMES: u64 = 90;
        let fps = FrameRate::new(30, 1);
        let interval = Duration::from_nanos(1_000_000_000 / 30);
        let mut clock = PlaybackClock::new(fps, PLAY_FRAMES);
        timings.drain();
        let before = transfer_stats();
        let before_dispatch = gpu.dispatch_stats();
        let start = Instant::now();
        clock.play(start);
        let mut last_frame = u64::MAX;
        let mut published = 0u64;
        let mut tick_skipped = 0u64;
        let mut samples = Vec::new();
        loop {
            let tick_start = Instant::now();
            let frame = clock.current_frame(tick_start);
            if frame != last_frame {
                if last_frame != u64::MAX && frame > last_frame + 1 {
                    tick_skipped += frame - last_frame - 1;
                }
                last_frame = frame;
                published += 1;
                service.request(EvalRequest {
                    graph: graph.clone(),
                    node: output,
                    path: Vec::new(),
                    ctx: EvalContext::new(frame, fps, RESOLUTION),
                    document: Some(document.clone()),
                    hint: InvalidationHint::None,
                });
                samples.push(tick_start.elapsed());
            }
            if clock.state() != PlaybackState::Playing {
                break;
            }
            std::thread::sleep(interval);
        }
        let final_generation = service.latest_generation();
        loop {
            let generation = done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("shell playback completion");
            if generation == final_generation {
                break;
            }
        }
        gpu.wait();
        let total = start.elapsed();
        let evals = evaluations.load(std::sync::atomic::Ordering::SeqCst) as u64;
        report(
            &format!("(g) {layers}-layer shell chain — 30 fps playback"),
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!(
            "playback: {PLAY_FRAMES} frames in {:.2} s -> {:.1} fps evaluated; \
             {published} frames published, {tick_skipped} skipped by tick jitter, \
             {} coalesced by latest-wins",
            total.as_secs_f64(),
            evals as f64 / total.as_secs_f64(),
            published.saturating_sub(evals),
        );
        let dispatch = before_dispatch.delta(&gpu.dispatch_stats());
        println!(
            "dispatch submits: {} ({:.2} / completed evaluation), \
             recorded passes: {} ({:.2} / completed evaluation)",
            dispatch.submits,
            dispatch.submits as f64 / evals.max(1) as f64,
            dispatch.dispatches,
            dispatch.dispatches as f64 / evals.max(1) as f64
        );
    }

    // -- Scenario (c): scatter count=500 geometry chain ---------------------
    // Selecting the scatter output pulls the (warm) geometry chain and runs
    // the Viewer's ad-hoc rasterize, which is never cached. The evaluator is
    // built once, as in the app (selection does not rebuild processors).
    {
        let graph = scatter_graph(&registry);
        let mut evaluator = build_evaluator(&graph, &gpu, &mut shaders, &pool, None);
        timings.drain();
        let before = transfer_stats();
        let mut quads = 0usize;
        let samples = run_scenario(10, |_| {
            let out = evaluator.evaluate(&graph, nid(GRID), &ctx).unwrap();
            let fb = adhoc_rasterize(out.as_ref(), &ctx).expect("rasterize");
            quads = count_paint_quads(&fb, (512.0, 512.0));
        });
        report(
            "(c) scatter grid 500 instances → ad-hoc rasterize",
            &wall_stats(&samples),
            timings.drain(),
            before.delta(&transfer_stats()),
        );
        println!("paint quads (run-merged): {quads}");
    }

    // -- GPU-0: geometry chain scaling, uncached every frame ----------------
    // `gpu-resident-geometry-plan.md` phase 0. The existing warm 0.007 ms
    // number measured a cache hit; here a scatter parameter moves every frame,
    // which is what an animated modulation does, so nothing is cached.
    {
        println!("\n# GPU-0: geometry chain scaling (uncached every frame)");
        println!("adapter: {:?}", gpu.adapter_info());
        let mut summary = Vec::new();
        for stage in GeoStage::ALL {
            for count in GEO_COUNTS {
                let frames = geo_frames(count);
                let mut graph = geo_graph(&registry, count, stage);
                let mut evaluator =
                    build_evaluator(&graph, &gpu, &mut shaders, &pool, Some(&source_fb));
                // Warm-up outside the timed region: first-touch allocation and,
                // for stage D, pipeline creation.
                if let Err(error) = evaluator.evaluate(&graph, stage.output(), &ctx) {
                    println!(
                        "\n## ({}) {count} instances — SKIPPED: {error}",
                        stage.tag()
                    );
                    continue;
                }
                gpu.wait();
                timings.drain();
                let before = transfer_stats();
                let samples = run_scenario(frames, |i| {
                    graph =
                        set_vec2_param(&graph, nid(GRID), "spacing", 8.0 + i as f32 * 0.01, 8.0);
                    evaluator.mark_dirty(&graph, nid(GRID));
                    evaluator.evaluate(&graph, stage.output(), &ctx).unwrap();
                    // Stage D submits without waiting; a frame budget only
                    // means something once the GPU has finished.
                    gpu.wait();
                });
                let wall = wall_stats(&samples);
                let spans = timings.drain();
                summary.push(format!(
                    "| {} | {count} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} |",
                    stage.tag(),
                    ms(wall.mean),
                    node_process_ms(&spans, frames),
                    span_ms(&spans, "raster_flatten", frames),
                    span_ms(&spans, "raster_upload", frames),
                    span_ms(&spans, "raster_submit", frames),
                    span_ms(&spans, "gpu_readback", frames),
                ));
                report(
                    &format!("({}) {} — {count} instances", stage.tag(), stage.chain()),
                    &wall,
                    spans,
                    before.delta(&transfer_stats()),
                );

                if stage == GeoStage::EndToEnd {
                    // The loop above waits for the GPU every frame, which
                    // serializes CPU and GPU work. A real playback loop can
                    // overlap frame N's GPU work with frame N+1's CPU work, so
                    // measure that too: the honest floor is the larger of the
                    // two, not their sum.
                    let start = Instant::now();
                    for i in 0..frames {
                        graph = set_vec2_param(
                            &graph,
                            nid(GRID),
                            "spacing",
                            9.0 + i as f32 * 0.01,
                            8.0,
                        );
                        evaluator.mark_dirty(&graph, nid(GRID));
                        evaluator.evaluate(&graph, stage.output(), &ctx).unwrap();
                    }
                    gpu.wait();
                    println!(
                        "pipelined (one wait for the whole run): {:.2} ms/frame",
                        ms(start.elapsed()) / frames as f64
                    );
                }
            }
        }

        println!("\n## GPU-0 summary (ms per frame, all uncached)");
        println!(
            "| stage | elements | wall | node_process | raster_flatten \
             | raster_upload | raster_submit | gpu_readback |"
        );
        println!("|---|---|---|---|---|---|---|---|");
        for row in &summary {
            println!("{row}");
        }
        println!("60 fps budget: 16.60 ms/frame");
    }

    // -- Particle proxy: per-frame CPU step + GPU draw ----------------------
    // `particle-plan.md` decides CPU (unit 2) versus GPU (unit 6) on whether a
    // per-frame step at 100k points fits the frame budget, and on what a
    // read-back costs if the state lives in VRAM. `particle.simulate` does not
    // exist yet, so the step here is a hand-written stand-in.
    {
        println!("\n# Particle proxy: per-frame step + GPU draw (no cache by construction)");
        let rast_node = Node::new(nid(RAST), "rasterize")
            .with_param("fill", ParameterValue::Bool(true))
            .with_param("stroke_width", ParameterValue::Float(0.0));
        let rasterizer =
            RasterizeProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &rast_node);
        let params = ravel_core::eval::ResolvedParams::default();

        // Step cost measured on state that is never handed to the rasterizer.
        // In the draw loop below the geometry is cloned into an `Arc` each
        // frame, which leaves the `P` column shared; if that `Arc` outlived the
        // frame, the next `make_mut` would deep-copy the column and charge the
        // memcpy to the step. Measuring the step in isolation removes the
        // question from the number the plans cite.
        println!("\n## Particle step in isolation (state never shared)");
        println!("| points | serial ms | rayon ms |");
        println!("|---|---|---|");
        for count in PARTICLE_COUNTS {
            let mut isolated = Vec::new();
            for parallel in [false, true] {
                let mut state = ParticleState::new(count);
                let samples = run_scenario(PARTICLE_FRAMES, |frame| {
                    let time = frame as f32 * PARTICLE_DT;
                    if parallel {
                        state.step_parallel(time);
                    } else {
                        state.step(time);
                    }
                });
                isolated.push(ms(wall_stats(&samples).mean));
            }
            println!("| {count} | {:.2} | {:.2} |", isolated[0], isolated[1]);
        }

        let mut summary = Vec::new();
        for count in PARTICLE_COUNTS {
            for parallel in [false, true] {
                let mut state = ParticleState::new(count);
                let mut step_total = Duration::ZERO;
                let mut scope = Evaluator::new();
                timings.drain();
                let before = transfer_stats();
                let samples = run_scenario(PARTICLE_FRAMES, |frame| {
                    let time = frame as f32 * PARTICLE_DT;
                    let step_start = Instant::now();
                    if parallel {
                        state.step_parallel(time);
                    } else {
                        state.step(time);
                    }
                    step_total += step_start.elapsed();
                    let geometry: Arc<dyn NodeData> = Arc::new(state.geometry.clone());
                    rasterizer
                        .process(&rast_node, &ctx, &[Some(geometry)], &params, &mut scope)
                        .expect("rasterize succeeds");
                    gpu.wait();
                });
                let wall = wall_stats(&samples);
                let spans = timings.drain();
                let step_ms = ms(step_total) / PARTICLE_FRAMES as f64;
                let upload_ms = span_ms(&spans, "raster_upload", PARTICLE_FRAMES);
                // One `DrawItem` (64 B) per point is what the rasterizer
                // uploads for free points; free points contribute no path
                // vertices.
                let upload_mb = (state.count() * 64) as f64 / 1e6;
                summary.push(format!(
                    "| {count} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.1} | {:.0} |",
                    if parallel { "rayon" } else { "serial" },
                    ms(wall.mean),
                    step_ms,
                    span_ms(&spans, "raster_flatten", PARTICLE_FRAMES),
                    upload_ms,
                    upload_mb,
                    upload_mb / upload_ms.max(1e-6) * 1000.0,
                ));
                report(
                    &format!(
                        "particles {count} — {} step + GPU draw",
                        if parallel { "rayon" } else { "serial" }
                    ),
                    &wall,
                    spans,
                    before.delta(&transfer_stats()),
                );
                println!("step only: {step_ms:.2} ms/frame");

                // Same pipelining question as stage D: without a per-frame
                // wait, the CPU step and flatten overlap the previous frame's
                // draw.
                let start = Instant::now();
                for frame in 0..PARTICLE_FRAMES {
                    let time = frame as f32 * PARTICLE_DT;
                    if parallel {
                        state.step_parallel(time);
                    } else {
                        state.step(time);
                    }
                    let geometry: Arc<dyn NodeData> = Arc::new(state.geometry.clone());
                    rasterizer
                        .process(&rast_node, &ctx, &[Some(geometry)], &params, &mut scope)
                        .expect("rasterize succeeds");
                }
                gpu.wait();
                println!(
                    "pipelined (one wait for the whole run): {:.2} ms/frame",
                    ms(start.elapsed()) / PARTICLE_FRAMES as f64
                );
            }
        }

        println!("\n## Particle proxy summary (ms per frame)");
        println!(
            "| points | step | wall | step only | raster_flatten | raster_upload \
             | upload MB | MB/s |"
        );
        println!("|---|---|---|---|---|---|---|---|");
        for row in &summary {
            println!("{row}");
        }
    }

    // -- Frame readback at display resolutions (HIGH-04) --------------------
    // The viewer's exit from the GPU: `GpuEvalHooks::finalize` calls
    // `to_frame_buffer` once per displayed frame, so this is the cost that sits
    // between an evaluated frame and a visible one. Two things are recorded —
    // the per-frame cost at 1080p and 4K, and that the staging buffers behind
    // it stop being allocated after the first frame of each size.
    {
        println!("\n## Frame readback at display resolutions ({READBACK_FRAMES} frames each)");
        println!(
            "| resolution | MB/frame | mean ms | min ms | max ms | GPU copy ms | CPU copy ms \
             | checks/frame | staging buffers created |"
        );
        println!("|---|---|---|---|---|---|---|---|---|");
        for (width, height) in READBACK_RESOLUTIONS {
            let frame = ravel_gpu::GpuFrameBuffer::from_frame_buffer(
                gpu.clone(),
                &pool,
                &gradient_fb(width, height),
            )?;
            let frame_bytes = (width as usize) * (height as usize) * 16;
            // The first readback of a size allocates its staging buffer; that
            // is the allocation the pool exists to amortize, so it is warm-up
            // rather than part of the per-frame number.
            assert_eq!(frame.to_frame_buffer()?.data.len(), frame_bytes);

            let before = transfer_stats();
            let samples = run_scenario(READBACK_FRAMES, |_| {
                let cpu = frame.to_frame_buffer().expect("readback");
                std::hint::black_box(cpu.data.len());
            });
            let staging = before.delta(&transfer_stats()).staging_buffers_created;
            let wall = wall_stats(&samples);

            // The same work split in two: waiting for the GPU copy, which an
            // asynchronous readback could overlap with the next frame's
            // evaluation, and copying out of the mapping, which it could not.
            // This is the split `GPUCOMP-10` has to be decided on.
            //
            // `is_complete()` is a zero-timeout device query, so this spins to
            // get the finest resolution the API offers — acceptable in a
            // benchmark, not a pattern for production code. `checks` records
            // how many it took, without which a "0.00 ms" GPU wait could not be
            // told apart from a copy that was already finished on the first
            // check.
            let mut gpu_total = Duration::ZERO;
            let mut cpu_total = Duration::ZERO;
            let mut checks = 0u64;
            for _ in 0..READBACK_FRAMES {
                let start = Instant::now();
                let mut pending = frame.begin_readback()?;
                loop {
                    checks += 1;
                    if pending.is_complete()? {
                        break;
                    }
                }
                let ready = Instant::now();
                std::hint::black_box(pending.wait_shared()?.len());
                gpu_total += ready.duration_since(start);
                cpu_total += ready.elapsed();
            }

            println!(
                "| {width}x{height} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.1} | \
                 {staging} |",
                frame_bytes as f64 / 1e6,
                ms(wall.mean),
                ms(wall.min),
                ms(wall.max),
                ms(gpu_total) / READBACK_FRAMES as f64,
                ms(cpu_total) / READBACK_FRAMES as f64,
                checks as f64 / READBACK_FRAMES as f64,
            );
        }
    }

    // -- Viewer path at display resolutions (GPUBK-9) -----------------------
    // The section above measures `to_frame_buffer()` on a frame that is already
    // sitting in VRAM. What the interactive viewer pays per frame is an
    // evaluation *plus* that readback, and by default it pays it at a reduced
    // scale: the preview factor (`ViewerResolution`, default `Half`) divides
    // the composition resolution before any node runs, so a 16:9 1080p
    // composition evaluates 960x540 — 1024x576 here, the figure this baseline
    // was first recorded at.
    //
    // The scenario reports two things: the whole per-frame cost at the reduced
    // scale next to the same cost at full 1080p — which is what a user buys
    // when they switch to `ViewerResolution::Full` — and the split between
    // evaluation and readback inside it, since only the readback half is what
    // a zero-copy viewer would remove, so a frame dominated by evaluation
    // would not be rescued by device sharing at all.
    //
    // Ten layers is the heavier of the two shell-chain counts, i.e. the case
    // where full resolution is least likely to be affordable. The frame number
    // increases every iteration so the layer shells resample their animated
    // transform and opacity channels and nothing is served from the eval cache.
    {
        println!(
            "\n## Viewer path at display resolutions \
             ({SHELL_LAYERS}-layer shell chain + readback)"
        );
        println!(
            "| resolution | frames | MB/frame | mean ms | min ms | max ms \
             | readback mean ms | submits/frame | readbacks/frame |"
        );
        println!("|---|---|---|---|---|---|---|---|---|");
        for (width, height) in VIEWER_PATH_RESOLUTIONS {
            let (graph, output, document) =
                shell_composition(&registry, SHELL_LAYERS, (width, height));
            let source = gradient_fb(width, height);
            let mut evaluator =
                build_shell_evaluator(&graph, &document, &gpu, &mut shaders, &pool, &source);
            let frame_ctx =
                |frame: u64| EvalContext::new(frame, FrameRate::new(30, 1), (width, height));

            // Untimed warm-up, the same discipline as the readback scenario:
            // the first frame of a size creates the pipelines, first-touches
            // its pooled textures and allocates its readback staging buffer,
            // none of which recur per frame.
            match evaluator.evaluate(&graph, output, &frame_ctx(0)) {
                Ok(value) => {
                    viewer_readback(value.as_ref()).expect("warm-up readback");
                }
                Err(error) => {
                    println!("| {width}x{height} | SKIPPED: {error} |");
                    continue;
                }
            }
            gpu.wait();

            let before = transfer_stats();
            let before_dispatch = gpu.dispatch_stats();
            // `to_frame_buffer` blocks until the copy lands, so the readback
            // total also accounts for the GPU work the evaluation submitted.
            let mut readback_total = Duration::ZERO;
            let samples = run_scenario(VIEWER_PATH_FRAMES, |i| {
                let value = evaluator
                    .evaluate(&graph, output, &frame_ctx(i as u64 + 1))
                    .expect("viewer path evaluation");
                let readback_start = Instant::now();
                let cpu = viewer_readback(value.as_ref()).expect("viewer readback");
                readback_total += readback_start.elapsed();
                std::hint::black_box(cpu.data.len());
            });
            let wall = wall_stats(&samples);
            let transfers = before.delta(&transfer_stats());
            let submits = before_dispatch.delta(&gpu.dispatch_stats()).submits;
            let frames = VIEWER_PATH_FRAMES as f64;
            println!(
                "| {width}x{height} | {} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} \
                 | {:.2} |",
                wall.iterations,
                ((width as usize) * (height as usize) * 16) as f64 / 1e6,
                ms(wall.mean),
                ms(wall.min),
                ms(wall.max),
                ms(readback_total) / frames,
                submits as f64 / frames,
                transfers.readbacks as f64 / frames,
            );
        }
    }

    // -- Paint proxy: run-merge scan cost over the merge output -------------
    {
        let graph = effect_graph(&registry);
        let mut evaluator = build_evaluator(&graph, &gpu, &mut shaders, &pool, Some(&source_fb));
        let out = evaluator.evaluate(&graph, nid(MERGE), &ctx)?;
        let fb = out
            .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .expect("merge output is GPU-resident")
            .to_frame_buffer()
            .expect("readback for paint proxy");
        let mut quads = 0usize;
        let samples = run_scenario(20, |_| {
            quads = count_paint_quads(&fb, (512.0, 512.0));
        });
        let wall = wall_stats(&samples);
        println!("\n## paint proxy: run-merge scan of merge output (512x512)");
        println!(
            "scan wall/iter: mean {:.2} ms (quads {quads}; GPUI paint_quad cost excluded)",
            ms(wall.mean)
        );
    }

    Ok(())
}

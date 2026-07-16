// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless performance baseline for the evaluation path (Phase 0 of
//! `docs/implementation/eval-render-performance-plan.md`).
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

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::{EvalService, EvalWorkerHooks, InvalidationHint};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
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

fn eval_ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), RESOLUTION)
}

/// Emits a fixed gradient FrameBuffer; stand-in for a media/source node.
struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(self.0.clone()))
    }
}

fn gradient_fb(width: u32, height: u32) -> FrameBuffer {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.25, 1.0]);
        }
    }
    FrameBuffer {
        width,
        height,
        data: Arc::from(data),
    }
}

fn set_float_param(graph: &Graph, node_id: NodeId, key: &str, value: f32) -> Graph {
    let node = graph.node(node_id).expect("node exists");
    let mut updated = (**node).clone();
    if let Some(param) = updated.parameters.iter_mut().find(|p| p.key == key) {
        param.value = ParameterValue::Float(value);
    }
    graph.clone().replace_node(Arc::new(updated))
}

fn set_int_param(node: &mut Node, key: &str, value: i32) {
    if let Some(param) = node.parameters.iter_mut().find(|p| p.key == key) {
        param.value = ParameterValue::Int(value);
    }
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
        "transfers: {} uploads ({:.1} MB), {} readbacks ({:.1} MB)",
        transfers.uploads,
        transfers.upload_bytes as f64 / 1e6,
        transfers.readbacks,
        transfers.readback_bytes as f64 / 1e6,
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

/// Mirrors the ad-hoc Geometry rasterize in `evaluate_for_viewer`.
fn adhoc_rasterize(data: &dyn NodeData, ctx: &EvalContext) -> Option<FrameBuffer> {
    let geo = data.downcast_ref::<ravel_core::geometry::Geometry>()?;
    let rast_node = Node::new(NodeId::new(u64::MAX), "rasterize")
        .with_param("fill", ParameterValue::Bool(true))
        .with_param("stroke_width", ParameterValue::Float(0.0));
    let proc = RasterizeProcessor::from_node(&rast_node);
    let inputs: Vec<&dyn NodeData> = vec![geo];
    proc.process(ctx, &inputs)
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
                [
                    fb.data[idx],
                    fb.data[idx + 1],
                    fb.data[idx + 2],
                    fb.data[idx + 3],
                ]
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
        struct BenchHooks {
            gpu: GpuContext,
            shaders: ShaderManager,
            pool: Arc<Mutex<TexturePool>>,
            source_fb: FrameBuffer,
        }
        impl EvalWorkerHooks for BenchHooks {
            fn sync(&mut self, evaluator: &mut Evaluator, graph: &Graph, hint: &InvalidationHint) {
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
                        *evaluator = Evaluator::new();
                        ravel_nodes::register_all_processors(
                            evaluator,
                            graph,
                            &self.gpu,
                            &mut self.shaders,
                            &self.pool,
                        );
                        evaluator.register(nid(SRC), Arc::new(FbSource(self.source_fb.clone())));
                    }
                }
            }
        }

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
                evaluations_worker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = done_tx.send(update.generation);
            },
        );

        let mut graph = effect_graph(&registry);
        timings.drain();
        let before = transfer_stats();
        let start_all = Instant::now();
        let samples = run_scenario(90, |i| {
            graph = set_float_param(&graph, nid(BLUR), "radius", 1.0 + i as f32 * 0.25);
            service.request(
                graph.clone(),
                nid(MERGE),
                ctx,
                InvalidationHint::Params(vec![nid(BLUR)]),
            );
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

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The render worker driving the real image-sequence encoder.
//!
//! `ravel-core`'s own tests cover the worker against a recording encoder, and
//! `ravel-media`'s cover the encoder against hand-made frames. Neither can see
//! the seam between them, which is where the plan's guarantees actually live:
//! that a ten-frame job leaves ten readable PNGs named by absolute frame
//! number, and that re-rendering onto an existing sequence is refused with
//! every one of the user's files still exactly as it was.

use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::{
    EvalContext, EvalScope, Evaluator, NodeProcessor, ProcessorRegistry as _, ResolvedParams,
};
use ravel_core::graph::{Graph, Node};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::media::encode::{ImageSequenceOutput, PngDepth, SequenceCodec};
use ravel_core::network;
use ravel_core::runtime::eval_service::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use ravel_core::runtime::{
    OverwritePolicy, RenderError, RenderEvent, RenderJob, RenderJobId, RenderOutput, RenderQueue,
};
use ravel_core::types::{Color, FrameBuffer, FrameRate, NodeData};
use ravel_media::encode::ImageSequenceEncoder;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;

const FPS: FrameRate = FrameRate { num: 24, den: 1 };
const RES: (u32, u32) = (8, 4);
const TIMEOUT: Duration = Duration::from_secs(30);

fn comp_id() -> CompId {
    CompId::new(1)
}

/// A one-layer composition. The layer network is a source feeding `net.out`,
/// which is what makes the layer produce a frame and so gives the compiled
/// shell chain something to composite.
fn document() -> Arc<Document> {
    let source = NodeId::new(1);
    let out = NodeId::new(2);
    let network = Graph::new()
        .add_node(Node::new(source, "test.frame").with_output("out", DataTypeId::FRAME_BUFFER))
        .expect("source node")
        .add_node(
            Node::new(out, network::NET_OUT_TYPE_KEY)
                .with_input(network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
        )
        .expect("out node")
        .add_edge(
            EdgeId::new(1),
            source,
            OutputPortIndex(0),
            out,
            InputPortIndex(0),
        )
        .expect("network edge");

    let layer = Layer::new(LayerId::new(1), "layer", network).with_time(0, 0, 100);
    let mut comp = Composition::new(comp_id(), "comp", RES, FPS, 100);
    comp.background_color = Color::new(0.0, 0.0, 0.0, 1.0);
    comp.layers.push_back(layer);
    Arc::new(Document::new(Graph::new()).with_composition(comp))
}

/// A ramp whose brightness follows the frame number, so two frames of one
/// sequence are distinguishable after they have been through PNG.
struct Ramp;

impl NodeProcessor for Ramp {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = ctx.resolution;
        let level = ctx.frame as f32 / 32.0;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[level, level, level, 1.0]);
        }
        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// Registers [`Ramp`] for the compiled shell and every layer network — the
/// shape the application's GPU hooks have, minus the GPU.
struct Hooks;

impl EvalWorkerHooks for Hooks {
    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    ) {
        if !matches!(hint, InvalidationHint::Structural) {
            return;
        }
        let mut ids: Vec<NodeId> = graph.nodes().map(|node| node.id).collect();
        if let Some(document) = document {
            for comp in document.compositions.values() {
                for layer in &comp.layers {
                    ids.extend(layer.network.nodes().map(|node| node.id));
                }
            }
        }
        for id in ids {
            evaluator.register(id, Arc::new(Ramp));
        }
    }
}

fn sequence(dir: &std::path::Path) -> ImageSequenceOutput {
    ImageSequenceOutput::new(dir, "beauty_", "", SequenceCodec::Png(PngDepth::Eight), 4)
        .expect("fixed test name is valid")
}

fn job(output: &ImageSequenceOutput, range: std::ops::Range<u64>) -> RenderJob {
    RenderJob::new(
        document(),
        comp_id(),
        range,
        Box::new(ImageSequenceEncoder::new(output.clone())),
        RenderOutput::Sequence(output.clone()),
    )
}

struct Runner {
    queue: RenderQueue,
    events: mpsc::Receiver<RenderEvent>,
}

fn runner() -> Runner {
    let (tx, events) = mpsc::channel();
    let queue = RenderQueue::spawn(Hooks, move |event| {
        let _ = tx.send(event);
    });
    Runner { queue, events }
}

impl Runner {
    fn terminal(&self, job: RenderJobId) -> RenderEvent {
        loop {
            let event = self.events.recv_timeout(TIMEOUT).expect("render event");
            if event.job() == job && event.is_terminal() {
                return event;
            }
        }
    }
}

/// The plan's first completion criterion, end to end: ten frames in, ten
/// readable PNGs out, named by absolute frame number.
#[test]
fn a_ten_frame_job_writes_ten_readable_pngs() {
    let dir = TempDir::new().unwrap();
    let output = sequence(dir.path());
    let mut runner = runner();
    let id = runner.queue.submit(job(&output, 20..30));

    match runner.terminal(id) {
        RenderEvent::Completed { frames, .. } => assert_eq!(frames, 10),
        other => panic!("expected completion, got {other:?}"),
    }

    for frame in 20..30 {
        let path = output.frame_path(frame);
        assert!(path.exists(), "{} is missing", path.display());
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        10,
        "the job must leave exactly its ten frames — no temporaries",
    );
    assert_eq!(
        output.frame_path(20),
        dir.path().join("beauty_0020.png"),
        "file names carry the absolute frame number",
    );

    let first = image::open(output.frame_path(20)).expect("frame 20 is a readable PNG");
    assert_eq!((first.width(), first.height()), RES);
    let last = image::open(output.frame_path(29)).expect("frame 29 is a readable PNG");
    assert_ne!(
        first.to_rgba8().into_raw(),
        last.to_rgba8().into_raw(),
        "every frame was rendered at its own time, not once and copied",
    );
}

/// The hazard this unit closes, end to end. Re-rendering onto an existing
/// sequence used to be allowed, and a cancellation part-way through then left
/// a sequence that was new up to the frame reached and old after it. The job
/// is now refused before anything is written, with the user's files untouched.
#[test]
fn re_rendering_over_an_existing_sequence_is_refused_and_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let output = sequence(dir.path());

    let mut runner = runner();
    let first = runner.queue.submit(job(&output, 0..5));
    assert!(matches!(
        runner.terminal(first),
        RenderEvent::Completed { .. }
    ));
    let before: Vec<Vec<u8>> = (0..5)
        .map(|frame| std::fs::read(output.frame_path(frame)).unwrap())
        .collect();

    // The same range again, with no opt-in.
    let second = runner.queue.submit(job(&output, 0..5));
    match runner.terminal(second) {
        RenderEvent::Failed {
            error: RenderError::OutputExists { total, .. },
            ..
        } => assert_eq!(total, 5, "every frame of the range conflicts"),
        other => panic!("expected a conflict refusal, got {other:?}"),
    }
    let after: Vec<Vec<u8>> = (0..5)
        .map(|frame| std::fs::read(output.frame_path(frame)).unwrap())
        .collect();
    assert_eq!(before, after, "a refused job must not touch the output");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        5,
        "and must not leave anything new behind either",
    );

    // With the opt-in it goes through.
    let third = runner
        .queue
        .submit(job(&output, 0..5).with_overwrite(OverwritePolicy::Replace));
    assert!(matches!(
        runner.terminal(third),
        RenderEvent::Completed { .. }
    ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 5);
}

/// The `--range` split the plan guarantees: two runs writing disjoint ranges
/// into one directory make one sequence, and neither trips the conflict check.
#[test]
fn two_disjoint_ranges_build_one_sequence() {
    let dir = TempDir::new().unwrap();
    let output = sequence(dir.path());
    let mut runner = runner();

    for range in [0..4, 4..8] {
        let id = runner.queue.submit(job(&output, range.clone()));
        match runner.terminal(id) {
            RenderEvent::Completed { frames, .. } => assert_eq!(frames, 4),
            other => panic!("range {range:?} should have completed, got {other:?}"),
        }
    }

    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8);
    for frame in 0..8 {
        assert!(output.frame_path(frame).exists(), "frame {frame} missing");
    }
}

/// The render worker's evaluator is its own: nothing it does reaches an
/// evaluator the application is using interactively. Checked here on the
/// integration side by rendering with a shared budget-free queue while a
/// separate evaluator holds a cached value, which must survive.
#[test]
fn a_render_does_not_disturb_a_separate_evaluator() {
    let dir = TempDir::new().unwrap();
    let output = sequence(dir.path());
    let document = document();
    let comp = document.get_composition(comp_id()).unwrap().clone();
    let compiled =
        ravel_core::composition::compile::compile_composition(&comp, Graph::new()).unwrap();

    let mut interactive = Evaluator::new();
    for node in compiled.graph.nodes() {
        interactive.register(node.id, Arc::new(Ramp));
    }
    interactive.set_document(document.clone());
    let ctx = EvalContext::new(0, FPS, RES);
    interactive
        .evaluate_at(&[], &compiled.graph, compiled.output_node, &ctx)
        .expect("interactive evaluation");
    let hits_before = interactive.cache_stats().hits;

    let mut runner = runner();
    let id = runner.queue.submit(job(&output, 0..3));
    assert!(matches!(runner.terminal(id), RenderEvent::Completed { .. }));

    // Same pull again: still a cache hit, so the render neither evicted nor
    // dirtied anything the interactive evaluator was holding.
    interactive
        .evaluate_at(&[], &compiled.graph, compiled.output_node, &ctx)
        .expect("interactive re-evaluation");
    assert!(
        interactive.cache_stats().hits > hits_before,
        "the render disturbed a separate evaluator's cache",
    );
}

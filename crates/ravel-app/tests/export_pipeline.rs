// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The export path from the dialog's settings to files on disk
//! (`render-export-plan.md`, unit 5).
//!
//! **This is the unit's "same worker, same encoder as the CLI" criterion.**
//! `EXPORT-3` and `EXPORT-4` moved `GpuEvalHooks` into `ravel-nodes` and the
//! mixdown into `ravel-audio` so that the CLI and the GUI could run the same
//! parts; what pins that here is that the GUI's own submission path —
//! [`ravel_app::export::build_render_job`], the function the dialog's OK
//! button calls — produces a job for `ravel_core::runtime::RenderQueue`
//! carrying `ravel_media::encode::ImageSequenceEncoder`, and that running it
//! leaves the same files under the same names `ravel-cli` would write.
//!
//! Held against `crates/ravel-cli/src/execute.rs`, which builds its job from
//! `plan_render`'s output in the same three lines. If the GUI ever grows its
//! own encoder or its own worker, the assertions below stop describing the
//! CLI and this test is where that shows up.
//!
//! Headless on purpose. The dialog's widgets are covered by the `gpui::test`s
//! in `export_dialog`; everything from the settings onward needs no window,
//! and `.agents/rules/gpui.md` reserves GPUI integration tests for behaviour
//! that actually depends on focus, actions, input, or rendering.
//!
//! The hooks are a stub rather than `GpuEvalHooks`: the claim under test is
//! about the worker and the encoder, and requiring a GPU adapter would make
//! it unrunnable on the machines that most need to check it.

use ravel_app::export::build_render_job;
use ravel_app::export_dialog::initial_settings;
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::{
    EvalContext, EvalScope, NodeProcessor, ProcessorRegistry as _, ResolvedParams,
};
use ravel_core::graph::{Graph, Node};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::media::encode::{PngDepth, SequenceCodec};
use ravel_core::network;
use ravel_core::runtime::{
    EvalWorkerHooks, InvalidationHint, JobProgress, OverwritePolicy, ProcessorSync, RenderEvent,
    RenderJobId, RenderOutput, RenderQueue,
};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_ui::export::{DEFAULT_PADDING, ExportError, ExportSettings};
use std::sync::Arc;
use std::time::Duration;

const FPS: FrameRate = FrameRate { num: 30, den: 1 };
const RESOLUTION: (u32, u32) = (32, 18);
const DURATION: u64 = 12;
const TIMEOUT: Duration = Duration::from_secs(30);

fn comp_id() -> CompId {
    CompId::new(1)
}

/// A one-layer composition whose network is a single frame source.
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
    let layer = Layer::new(LayerId::new(1), "layer", network).with_time(0, 0, DURATION);
    let mut comp = Composition::new(comp_id(), "shot 010", RESOLUTION, FPS, DURATION);
    comp.layers.push_back(layer);
    Arc::new(Document::new(Graph::new()).with_composition(comp))
}

/// Emits an opaque grey frame at the requested resolution.
struct FrameSource;

impl NodeProcessor for FrameSource {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = ctx.resolution;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[0.5, 0.5, 0.5, 1.0]);
        }
        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// Registers [`FrameSource`] for every node of the compiled shell and of
/// every layer network — the shape `GpuEvalHooks` has in the application.
struct StubHooks;

impl EvalWorkerHooks for StubHooks {
    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        doc: Option<&Document>,
        hint: &InvalidationHint,
    ) {
        if !matches!(hint, InvalidationHint::Structural) {
            return;
        }
        let mut ids: Vec<NodeId> = graph.nodes().map(|node| node.id).collect();
        if let Some(doc) = doc {
            for comp in doc.compositions.values() {
                for layer in &comp.layers {
                    ids.extend(layer.network.nodes().map(|node| node.id));
                }
            }
        }
        for id in ids {
            evaluator.register(id, Arc::new(FrameSource));
        }
    }
}

/// The dialog's opening settings for a temporary output directory.
fn settings(directory: &std::path::Path) -> ExportSettings {
    let doc = document();
    let comp = doc.get_composition(comp_id()).expect("comp");
    let mut settings = initial_settings(comp_id(), &comp.name, comp.duration_frames, None);
    settings.directory = directory.to_string_lossy().into_owned();
    // The dialog opens on whatever it decided the project directory is; the
    // test names its own.
    settings
}

/// Run `settings` through the production submission path and return the
/// terminal event plus the frames actually written.
fn render(settings: &ExportSettings) -> (RenderEvent, JobProgress) {
    let request = settings.resolve().expect("the settings resolve");
    // The job the dialog's OK button builds, unchanged.
    let job = build_render_job(&request, document());
    assert_eq!(
        job.output,
        RenderOutput::Sequence(request.output.clone()),
        "the encoder and the job's output must describe the same files",
    );
    assert_eq!(job.overwrite, request.overwrite);
    assert_eq!(job.range, request.range);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut queue = RenderQueue::spawn(StubHooks, move |event| {
        let _ = tx.send(event);
    });
    let id = queue.submit(job);
    let mut progress: Option<JobProgress> = None;
    loop {
        let event = rx.recv_timeout(TIMEOUT).expect("a render event");
        match &mut progress {
            Some(progress) => {
                progress.observe(&event);
            }
            slot => *slot = JobProgress::started(&event),
        }
        if event.job() == id && event.is_terminal() {
            queue.shutdown();
            return (
                event,
                progress.expect("a Started event precedes the terminal one"),
            );
        }
    }
}

fn frame_names(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .expect("output directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The unit's first and third completion criteria in one: the settings the
/// dialog collects become a job, the job goes through `ravel-core`'s render
/// worker driving `ravel-media`'s image-sequence encoder, and the files that
/// land are the ones `ravel-cli` would have written.
#[test]
fn the_dialogs_settings_are_rendered_by_the_same_worker_and_encoder_as_the_cli() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = settings(dir.path());
    let request = settings.resolve().expect("the settings resolve");

    // The resolved description, held against what `ravel-cli`'s `plan_render`
    // builds from `--range 0:11 --format png --output <dir>`: a half-open
    // absolute range, PNG at eight bits, and the shared default padding.
    assert_eq!(request.comp, comp_id());
    assert_eq!(request.range, 0..DURATION);
    assert_eq!(request.output.codec(), SequenceCodec::Png(PngDepth::Eight));
    assert_eq!(request.output.padding(), DEFAULT_PADDING);
    assert_eq!(request.output.directory(), dir.path());
    assert_eq!(request.overwrite, OverwritePolicy::Refuse);

    let (event, progress) = render(&settings);
    match event {
        RenderEvent::Completed { frames, .. } => assert_eq!(frames, DURATION),
        other => panic!("expected completion, got {other:?}"),
    }
    assert_eq!(progress.rendered(), DURATION);
    assert_eq!(progress.total_frames(), DURATION);
    assert_eq!(progress.fraction(), 1.0);

    // One file per frame, named from the **absolute** frame number, which is
    // what lets a range be split across processes.
    let names = frame_names(dir.path());
    assert_eq!(names.len(), DURATION as usize);
    assert_eq!(names.first().map(String::as_str), Some("shot 010_0000.png"));
    assert_eq!(names.last().map(String::as_str), Some("shot 010_0011.png"));

    // Real PNGs at the composition resolution, not placeholder bytes: the
    // encoder under test is `ravel-media`'s, so its output must decode.
    let first = image::open(request.output.frame_path(0)).expect("the frame is a readable PNG");
    assert_eq!((first.width(), first.height()), RESOLUTION);
}

/// The unit's second completion criterion, at the seam the dialog uses: an
/// inverted range never becomes a job.
#[test]
fn an_inverted_range_never_reaches_the_queue() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut settings = settings(dir.path());
    settings.start = "10".into();
    settings.end = "4".into();
    assert_eq!(settings.resolve(), Err(ExportError::EmptyRange));
    assert!(
        frame_names(dir.path()).is_empty(),
        "a refused form must not have written anything",
    );
}

/// Existing output is refused before a frame is evaluated, and the refusal
/// arrives as a failed job rather than as a mixed sequence.
#[test]
fn an_export_over_existing_frames_is_refused_unless_it_asked_to_replace() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = settings(dir.path());
    let request = settings.resolve().expect("resolves");
    std::fs::write(request.output.frame_path(3), b"not a frame").expect("existing frame");

    let (event, _) = render(&settings);
    match event {
        RenderEvent::Failed { error, .. } => assert!(
            error.to_string().contains("already exist"),
            "unexpected refusal: {error}",
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(request.output.frame_path(3)).expect("the existing file"),
        b"not a frame",
        "the pre-flight check must run before anything is written",
    );

    // The same export with the dialog's overwrite box ticked goes through.
    let mut replacing = settings.clone();
    replacing.overwrite = true;
    let (event, _) = render(&replacing);
    assert!(matches!(event, RenderEvent::Completed { .. }));
    assert_eq!(frame_names(dir.path()).len(), DURATION as usize);
}

/// Cancelling a queued job leaves nothing behind — what the render queue
/// panel's cancel button reaches.
#[test]
fn a_cancelled_job_leaves_no_output() {
    let dir = tempfile::tempdir().expect("temp dir");
    let settings = settings(dir.path());
    let request = settings.resolve().expect("resolves");

    let (tx, rx) = std::sync::mpsc::channel();
    let mut queue = RenderQueue::spawn(StubHooks, move |event| {
        let _ = tx.send(event);
    });
    let id: RenderJobId = queue.submit(build_render_job(&request, document()));
    queue.cancel(id);
    let event = loop {
        let event = rx.recv_timeout(TIMEOUT).expect("a render event");
        if event.is_terminal() {
            break event;
        }
    };
    queue.shutdown();
    assert!(
        matches!(event, RenderEvent::Cancelled { .. }),
        "expected a cancellation, got {event:?}",
    );
    assert!(
        frame_names(dir.path()).is_empty(),
        "a cancelled job removes its partial output",
    );
}

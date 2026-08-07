// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The guarantees `ravel-cli render` makes, driven through the library entry
//! point with stub evaluation hooks.
//!
//! # Why not the binary
//!
//! The binary needs a GPU adapter, because its hooks build the real
//! processors. Everything asserted here — how many files a range produces,
//! what they are called, that two half-renders equal one whole one, that a
//! refusal happens before a frame is evaluated, that an interrupt leaves
//! nothing — is decided by the CLI's planning and by the worker, not by what
//! draws the pixels. Substituting a deterministic CPU processor therefore
//! tests the same code paths and gets bit-exact answers on every machine.
//! `cli_binary.rs` covers the seam these cannot: that the shipped binary
//! wires it all together.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ravel_cli::args::{OutputFormat, PngBits, ProgressMode, RenderArgs};
use ravel_cli::error::{
    CliError, EXIT_CANCELLED, EXIT_CODEC, EXIT_LOAD, EXIT_OUTPUT_EXISTS, EXIT_PARAM, EXIT_USAGE,
};
use ravel_cli::execute::CancelFlag;
use ravel_cli::render_with_hooks;
use ravel_cli::report::{Reporter, Summary};
use ravel_core::composition::{AudioSource, Composition, Document, Layer};
use ravel_core::eval::{
    EvalContext, EvalScope, NodeProcessor, ProcessorRegistry as _, ResolvedParams,
};
use ravel_core::exposed::{ExposedBinding, ExposedParameter, ExposedParameters, ExposedValue};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network;
use ravel_core::runtime::JobProgress;
use ravel_core::runtime::eval_service::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use ravel_core::types::{Color, FrameBuffer, FrameRate, NodeData};
use ravel_project::ProjectFile;
use tempfile::TempDir;

const COMP: u64 = 1;
const DURATION: u64 = 300;
fn source() -> NodeId {
    NodeId::new(1)
}

// ===========================================================================
// Fixture
// ===========================================================================

/// A layer network that feeds one source node into `net.out(frame)`, which
/// is what makes the layer produce a picture for the compiled shell chain to
/// composite.
///
/// `base` offsets the node ids: a document validates only when every node id
/// is unique across every graph it holds, so a second layer needs its own.
fn layer_network(base: u64) -> Graph {
    let out = NodeId::new(base + 1);
    Graph::new()
        .add_node(
            Node::new(NodeId::new(base), "test.frame")
                .with_output("out", DataTypeId::FRAME_BUFFER)
                .with_param("scale", ParameterValue::Float(1.0)),
        )
        .expect("source node")
        .add_node(
            Node::new(out, network::NET_OUT_TYPE_KEY)
                .with_input(network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
        )
        .expect("out node")
        .add_edge(
            EdgeId::new(base),
            NodeId::new(base),
            OutputPortIndex(0),
            out,
            InputPortIndex(0),
        )
        .expect("network edge")
}

/// A one-composition document, optionally with an audio-carrying layer.
fn document(with_audio: bool) -> Document {
    let mut comp = Composition::new(
        CompId::new(COMP),
        "Main",
        (8, 4),
        FrameRate::new(24, 1),
        DURATION,
    );
    comp.background_color = Color::new(0.0, 0.0, 0.0, 1.0);
    comp = comp.add_layer(
        Layer::new(LayerId::new(1), "picture", layer_network(source().raw()))
            .with_time(0, 0, DURATION),
    );
    if with_audio {
        let mut layer =
            Layer::new(LayerId::new(2), "voice", layer_network(100)).with_time(0, 0, DURATION);
        layer.audio = Some(AudioSource {
            asset_id: "voice".into(),
            ..Default::default()
        });
        comp = comp.add_layer(layer);
    }

    Document::default()
        .with_composition(comp)
        .with_exposed_parameters(
            ExposedParameters::from_declarations([ExposedParameter::inferred(
                "scale",
                ExposedValue::Float(1.0),
                ExposedBinding::new(source(), "scale"),
            )
            .expect("declaration")])
            .expect("unique names"),
        )
}

/// Write a `.ravprj` holding `document` and return its path.
fn project_file(dir: &Path, document: Document) -> PathBuf {
    let path = dir.join("fixture.ravprj");
    ProjectFile::from_document("Fixture", "2026-01-01T00:00:00Z", document)
        .save(&path)
        .expect("the fixture project saves");
    path
}

/// Rewrite an archive's manifest to claim an older format version, so a load
/// has to migrate. The document entries are untouched: the manifest is the
/// only thing the version chain rewrites.
fn downgrade_format_version(path: &Path, version: u32) {
    use ravel_project::container;
    let mut archive = container::read_file(path).expect("fixture is readable");
    let manifest = archive
        .require_text(container::entry::MANIFEST)
        .expect("manifest entry");
    let mut value: serde_json::Value = serde_json::from_str(manifest).expect("manifest is JSON");
    value["format_version"] = serde_json::Value::from(version);
    archive.insert(
        container::entry::MANIFEST,
        serde_json::to_vec_pretty(&value).expect("manifest re-serializes"),
    );
    container::write_file(path, &archive).expect("fixture is writable");
}

fn args(project: &Path, output: &Path) -> RenderArgs {
    RenderArgs {
        project: project.to_path_buf(),
        comp: None,
        range: Some("0-9".parse().expect("range")),
        format: OutputFormat::Png,
        png_depth: PngBits::Eight,
        output: output.to_path_buf(),
        prefix: "frame_".into(),
        suffix: String::new(),
        padding: 4,
        params: Vec::new(),
        overwrite: false,
        progress: ProgressMode::Quiet,
    }
}

// ===========================================================================
// Stub evaluation
// ===========================================================================

/// A frame whose every channel is the frame number over 256, so two frames
/// of one sequence survive PNG quantisation as different images.
struct Ramp {
    /// Slows each frame down so a cancellation has somewhere to land.
    delay: Duration,
}

impl NodeProcessor for Ramp {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let (width, height) = ctx.resolution;
        let level = (ctx.frame % 256) as f32 / 255.0;
        let pixels = std::iter::repeat_n([level, level, level, 1.0], (width * height) as usize)
            .flatten()
            .collect();
        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// Registers [`Ramp`] for the compiled shell and every layer network — the
/// shape the GPU hooks have, minus the device.
struct StubHooks {
    delay: Duration,
}

impl StubHooks {
    fn new() -> Self {
        Self {
            delay: Duration::ZERO,
        }
    }

    fn slow() -> Self {
        Self {
            delay: Duration::from_millis(15),
        }
    }
}

impl EvalWorkerHooks for StubHooks {
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
            evaluator.register(id, Arc::new(Ramp { delay: self.delay }));
        }
    }
}

// ===========================================================================
// Reporting
// ===========================================================================

/// Records what was said, and can pull the cancellation lever partway.
#[derive(Default)]
struct Recorder {
    notes: Vec<String>,
    updates: usize,
    cancel_after: Option<(usize, CancelFlag)>,
}

impl Recorder {
    fn cancelling(after: usize, flag: &CancelFlag) -> Self {
        Self {
            cancel_after: Some((after, flag.clone())),
            ..Default::default()
        }
    }
}

impl Reporter for Recorder {
    fn note(&mut self, id: &str, _message: &str) {
        self.notes.push(id.to_string());
    }

    fn update(&mut self, progress: &JobProgress) {
        self.updates += 1;
        if let Some((after, flag)) = &self.cancel_after
            && progress.rendered() >= *after as u64
        {
            flag.request();
        }
    }

    fn success(&mut self, _summary: &Summary) {}
    fn failure(&mut self, _error: &CliError) {}
}

// ===========================================================================
// Running
// ===========================================================================

struct Run {
    result: Result<Summary, CliError>,
    recorder: Recorder,
}

impl Run {
    fn code(&self) -> u8 {
        match &self.result {
            Ok(_) => 0,
            Err(error) => error.code(),
        }
    }

    fn summary(&self) -> &Summary {
        self.result.as_ref().expect("the render succeeded")
    }
}

fn run(args: &RenderArgs) -> Run {
    run_with(
        args,
        StubHooks::new(),
        Recorder::default(),
        &CancelFlag::new(),
    )
}

fn run_with(
    args: &RenderArgs,
    hooks: StubHooks,
    mut recorder: Recorder,
    cancel: &CancelFlag,
) -> Run {
    let result = render_with_hooks(args, || Ok(hooks), cancel, &mut recorder);
    Run { result, recorder }
}

/// The sequence files in `dir`, sorted by name.
fn frames(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    paths.sort();
    paths
}

fn names(dir: &Path) -> Vec<String> {
    frames(dir)
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

// ===========================================================================
// The plan's completion conditions
// ===========================================================================

#[test]
fn a_ten_frame_range_writes_ten_files() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let run = run(&args(&project, &out));
    assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());
    assert_eq!(run.summary().frames, 10);
    assert_eq!(frames(&out).len(), 10);
}

/// The file names are absolute frame numbers, which is what lets N processes
/// split one range across one directory and still produce one sequence.
#[test]
fn file_names_are_absolute_frame_numbers() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.range = Some("100-199".parse().expect("range"));
    let run = run(&args);
    assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());

    let names = names(&out);
    assert_eq!(names.len(), 100);
    assert_eq!(names.first().map(String::as_str), Some("frame_0100.png"));
    assert_eq!(names.last().map(String::as_str), Some("frame_0199.png"));
    assert_eq!(
        run.summary().first,
        out.join("frame_0100.png"),
        "the summary names the frames that were written"
    );
    assert_eq!(run.summary().last, out.join("frame_0199.png"));
}

/// Splitting a range across two runs must produce the same bytes as one run
/// over the whole of it — the property the render-farm workflow rests on.
#[test]
fn two_half_renders_equal_one_whole_render() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let whole = dir.path().join("whole");
    let split = dir.path().join("split");

    let mut one = args(&project, &whole);
    one.range = Some("0-9".parse().expect("range"));
    assert_eq!(run(&one).code(), 0);

    for range in ["0-4", "5-9"] {
        let mut part = args(&project, &split);
        part.range = Some(range.parse().expect("range"));
        let run = run(&part);
        assert_eq!(
            run.code(),
            0,
            "a disjoint range into the same directory is not a conflict: {:?}",
            run.result.as_ref().err()
        );
    }

    let whole_names = names(&whole);
    assert_eq!(whole_names, names(&split), "the same files, by name");
    for name in whole_names {
        assert_eq!(
            std::fs::read(whole.join(&name)).expect("whole"),
            std::fs::read(split.join(&name)).expect("split"),
            "{name} differs between the whole render and the split one"
        );
    }
}

/// A `--param` naming something the project does not declare fails before a
/// frame exists, and the refusal comes from `ravel-core`'s contract check.
#[test]
fn an_undeclared_parameter_fails_before_the_render_starts() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.params = vec!["nosuch=1".to_string()];
    let run = run(&args);

    assert_eq!(run.code(), EXIT_PARAM);
    assert!(matches!(
        run.result,
        Err(CliError::ParamRejected(
            ravel_core::exposed::apply::ExposedApplyError::Undeclared(_)
        ))
    ));
    assert!(frames(&out).is_empty(), "nothing may have been written");
}

#[test]
fn a_parameter_of_the_wrong_type_fails_before_the_render_starts() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.params = vec!["scale=large".to_string()];
    let run = run(&args);

    assert_eq!(run.code(), EXIT_PARAM);
    assert!(matches!(run.result, Err(CliError::ParamValue { .. })));
    assert!(frames(&out).is_empty(), "nothing may have been written");
}

/// A declared value that fits is applied and the render proceeds.
#[test]
fn a_declared_parameter_is_accepted() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.params = vec!["scale=2.5".to_string()];
    let run = run(&args);
    assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());
    assert_eq!(frames(&out).len(), 10);
}

/// A render is a read. Neither a current project nor one that has to be
/// migrated on the way in may be written back.
#[test]
fn the_project_file_is_never_written() {
    for version in [7, 6] {
        let dir = TempDir::new().expect("tempdir");
        let project = project_file(dir.path(), document(false));
        if version != 7 {
            downgrade_format_version(&project, version);
        }
        let before = std::fs::read(&project).expect("fixture readable");
        let modified_before = std::fs::metadata(&project)
            .and_then(|meta| meta.modified())
            .expect("mtime");
        let siblings_before = names(dir.path());

        let out = dir.path().join("out");
        let run = run(&args(&project, &out));
        assert_eq!(
            run.code(),
            0,
            "a v{version} project renders: {:?}",
            run.result.as_ref().err()
        );

        assert_eq!(
            std::fs::read(&project).expect("fixture still readable"),
            before,
            "rendering a v{version} project rewrote it"
        );
        assert_eq!(
            std::fs::metadata(&project)
                .and_then(|meta| meta.modified())
                .expect("mtime"),
            modified_before,
            "rendering a v{version} project touched it"
        );
        assert_eq!(
            names(dir.path()),
            siblings_before,
            "a render must not add a file beside the project (a backup, a lock)"
        );
    }
}

/// Existing output is refused before anything is evaluated, and the files
/// that are already there are left exactly as they were.
#[test]
fn existing_output_is_refused_until_overwrite_is_asked_for() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    assert_eq!(run(&args(&project, &out)).code(), 0);
    let before: Vec<(PathBuf, Vec<u8>)> = frames(&out)
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).expect("frame");
            (path, bytes)
        })
        .collect();
    assert_eq!(before.len(), 10);

    let refused = run(&args(&project, &out));
    assert_eq!(refused.code(), EXIT_OUTPUT_EXISTS);
    match &refused.result {
        Err(CliError::OutputExists { total, .. }) => assert_eq!(*total, 10),
        other => panic!("expected an OutputExists refusal, got {other:?}"),
    }
    for (path, bytes) in &before {
        assert_eq!(
            &std::fs::read(path).expect("frame survives"),
            bytes,
            "{} was disturbed by a refused render",
            path.display()
        );
    }

    let mut replace = args(&project, &out);
    replace.overwrite = true;
    let replaced = run(&replace);
    assert_eq!(replaced.code(), 0, "{:?}", replaced.result.as_ref().err());
    assert_eq!(frames(&out).len(), 10);
}

/// An interrupt stops at a frame boundary and takes the partial sequence
/// with it: half a render is not a deliverable.
#[test]
fn an_interrupted_render_leaves_nothing_behind() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.range = Some("0-199".parse().expect("range"));

    let cancel = CancelFlag::new();
    let recorder = Recorder::cancelling(1, &cancel);
    let run = run_with(&args, StubHooks::slow(), recorder, &cancel);

    assert_eq!(run.code(), EXIT_CANCELLED);
    assert!(matches!(run.result, Err(CliError::Cancelled)));
    assert!(
        run.recorder.updates < 200,
        "the render must have stopped early, not finished"
    );
    assert!(
        frames(&out).is_empty(),
        "a cancelled render left {} file(s) behind",
        frames(&out).len()
    );
}

/// A format this build cannot write is refused before the project is even
/// planned, with the reason attached.
#[test]
fn an_unwritable_format_is_refused_before_the_render_starts() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.format = OutputFormat::Vp9;
    let run = run(&args);

    // Without FFmpeg the reason is the environment; with it, Ravel's missing
    // container writer. Both are the same class to a caller, and both must
    // carry a sentence rather than a bare refusal.
    assert_eq!(run.code(), EXIT_CODEC);
    let error = run.result.as_ref().expect_err("refused");
    assert!(
        matches!(
            error,
            CliError::CodecUnavailable { .. } | CliError::CodecNoWriter { .. }
        ),
        "unexpected refusal: {error:?}"
    );
    assert!(frames(&out).is_empty());
}

/// H.265 is refused as policy on every machine and in every build, which
/// makes it the one format whose refusal does not depend on the environment.
#[test]
fn a_format_ravel_does_not_offer_is_refused_with_that_reason() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));

    let mut args = args(&project, &dir.path().join("out"));
    args.format = OutputFormat::H265;
    let run = run(&args);
    assert_eq!(run.code(), EXIT_CODEC);
    assert!(matches!(
        run.result,
        Err(CliError::CodecUnavailable {
            reason: ravel_core::media::encode::UnavailableReason::NotOffered,
            ..
        })
    ));
}

/// Audio is not rendered until `EXPORT-4`, so a project that has some must
/// be told rather than handed a silent deliverable without comment.
#[test]
fn a_project_with_audio_warns_that_the_render_is_silent() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(true));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.range = Some("0-1".parse().expect("range"));
    let run = run(&args);

    assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());
    assert!(
        run.recorder
            .notes
            .contains(&"audio-not-rendered".to_string()),
        "expected an audio warning, got {:?}",
        run.recorder.notes
    );
}

#[test]
fn a_project_without_audio_says_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.range = Some("0-1".parse().expect("range"));
    let run = run(&args);
    assert_eq!(run.code(), 0);
    assert!(run.recorder.notes.is_empty(), "{:?}", run.recorder.notes);
}

/// A composition the project does not have is an argument mistake, not a
/// render failure, and is caught before anything is opened.
#[test]
fn an_unknown_composition_is_an_argument_error() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let mut args = args(&project, &out);
    args.comp = Some("Nope".into());
    let run = run(&args);
    assert_eq!(run.code(), EXIT_USAGE);
    assert!(frames(&out).is_empty());
}

/// A project that is not there fails as a load, which is its own class:
/// a script has to be able to tell "bad path" from "bad render".
#[test]
fn a_missing_project_fails_as_a_load() {
    let dir = TempDir::new().expect("tempdir");
    let missing = dir.path().join("nothing.ravprj");
    let run = run(&args(&missing, &dir.path().join("out")));
    assert_eq!(run.code(), EXIT_LOAD);
    assert!(matches!(run.result, Err(CliError::Load { .. })));
}

// ===========================================================================
// Through `render` itself
// ===========================================================================

/// Everything above goes through `render_with_hooks`, which is the seam the
/// stub processor plugs into — and which therefore never exercises the order
/// `render` puts things in. That order is load-bearing: a headless runner or
/// a render-farm node may have no adapter at all, and if the GPU context is
/// built first then a misspelled `--param`, an unreadable project, a codec
/// this build cannot write and an output that is already there all come back
/// as "no usable GPU adapter" with exit code 1, collapsing every classified
/// code the plan promises.
///
/// These call `render` — the real entry point, GPU factory and all — and
/// assert the classification. They pass on a machine with an adapter and on
/// one without, because none of them may reach the device.
#[test]
fn the_real_entry_point_classifies_before_it_builds_a_device() {
    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let out = dir.path().join("out");

    let render = |args: &RenderArgs| {
        let mut recorder = Recorder::default();
        ravel_cli::render(args, &CancelFlag::new(), &mut recorder)
            .err()
            .map(|error| (error.code(), error.id()))
    };

    let missing = args(&dir.path().join("nothing.ravprj"), &out);
    assert_eq!(render(&missing), Some((EXIT_LOAD, "load-failed")));

    let mut undeclared = args(&project, &out);
    undeclared.params = vec!["nosuch=1".to_string()];
    assert_eq!(render(&undeclared), Some((EXIT_PARAM, "param-rejected")));

    let mut mistyped = args(&project, &out);
    mistyped.params = vec!["scale=large".to_string()];
    assert_eq!(render(&mistyped), Some((EXIT_PARAM, "param-type")));

    let mut unknown_comp = args(&project, &out);
    unknown_comp.comp = Some("Nope".into());
    assert_eq!(
        render(&unknown_comp),
        Some((EXIT_USAGE, "unknown-composition"))
    );

    // H.265 is refused as policy everywhere, so this one refusal does not
    // depend on what FFmpeg the machine has.
    let mut unwritable = args(&project, &out);
    unwritable.format = OutputFormat::H265;
    assert_eq!(render(&unwritable), Some((EXIT_CODEC, "codec-unavailable")));

    // An output that is already there, without a render having to produce it:
    // the file name is the one the plan would write for frame 0.
    let occupied = dir.path().join("occupied");
    std::fs::create_dir_all(&occupied).expect("output directory");
    std::fs::write(occupied.join("frame_0000.png"), b"not a png").expect("an existing frame");
    assert_eq!(
        render(&args(&project, &occupied)),
        Some((EXIT_OUTPUT_EXISTS, "output-exists"))
    );

    assert!(
        !out.exists(),
        "not one of these refusals may have created an output directory"
    );
}

/// The same ordering, stated as the invariant rather than as its symptoms:
/// nothing expensive is built until the render is known to be worth starting.
#[test]
fn the_evaluation_hooks_are_not_built_until_the_render_is_decided() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let built = AtomicUsize::new(0);
    // Captures only `&built`, so the closure is `Copy` and can be handed to
    // both calls even though the parameter takes it by value.
    let hooks = || -> Result<StubHooks, CliError> {
        built.fetch_add(1, Ordering::SeqCst);
        Ok(StubHooks::new())
    };

    let mut refused = args(&project, &dir.path().join("refused"));
    refused.params = vec!["nosuch=1".to_string()];
    let result = render_with_hooks(
        &refused,
        hooks,
        &CancelFlag::new(),
        &mut Recorder::default(),
    );
    assert_eq!(result.err().map(|error| error.code()), Some(EXIT_PARAM));
    assert_eq!(
        built.load(Ordering::SeqCst),
        0,
        "a refused render built the evaluation hooks anyway"
    );

    let accepted = args(&project, &dir.path().join("accepted"));
    render_with_hooks(
        &accepted,
        hooks,
        &CancelFlag::new(),
        &mut Recorder::default(),
    )
    .expect("the render runs");
    assert_eq!(
        built.load(Ordering::SeqCst),
        1,
        "a render that does start has to build them exactly once"
    );
}

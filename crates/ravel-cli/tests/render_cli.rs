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
        no_audio: true,
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
    // `None`, never the platform settings path: a test that read the
    // developer's own `settings.toml` would render against a cache budget
    // nobody here chose, and would answer differently on the next machine.
    let result = render_with_hooks(args, None, |_budget| Ok(hooks), cancel, &mut recorder);
    Run { result, recorder }
}

/// The **picture** files in `dir`, sorted by name.
///
/// The companion WAV shares the directory and is deliberately not counted:
/// "the render wrote ten frames" and "the render wrote nothing" are both
/// statements about pictures, and a soundtrack sitting beside them must not
/// change either answer.
fn frames(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().is_none_or(|ext| ext != "wav"))
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
    // Derived rather than written down: the point is "the current format and
    // one that has to be migrated", and a literal goes stale the next time
    // the format is raised.
    let current = ravel_project::manifest::CURRENT_FORMAT_VERSION;
    for version in [current, current - 1] {
        let dir = TempDir::new().expect("tempdir");
        let project = project_file(dir.path(), document(false));
        if version != current {
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

/// `--no-audio` on a project that has some must be told rather than handed a
/// silent deliverable without comment. (The audio path itself needs a decoder
/// and lives in the `sound` module below.)
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
/// `CACHE-3` / `CACHE-8`: the budget handed to the hooks factory is the one
/// the render worker's evaluator reserves against.
///
/// The hooks own caches of their own — the texture pool and the shared decode
/// cache — and the worker owns the node-result cache. If the CLI built them
/// from separate budgets, "one authority for the memory limit" would hold in
/// the GUI and quietly not hold in `ravel-cli render`. Nothing else here
/// notices: a render with three unrelated ceilings still writes correct
/// frames.
#[test]
fn the_render_worker_reserves_against_the_budget_the_hooks_were_given() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Samples the budget the factory saw, every time the render moves.
    struct Watcher {
        budget: Arc<Mutex<Option<ravel_core::cache_budget::SharedCacheBudget>>>,
        peak: Arc<AtomicUsize>,
    }

    impl Reporter for Watcher {
        fn update(&mut self, _progress: &JobProgress) {
            let budget = self.budget.lock().expect("budget");
            if let Some(budget) = budget.as_ref() {
                self.peak
                    .fetch_max(budget.stats().entries, Ordering::SeqCst);
            }
        }
    }

    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    // A global settings file of this test's own — the one budget both halves
    // share is also the one the settings layers asked for, and the figure is
    // this file's rather than the machine's.
    let settings = dir.path().join("settings.toml");
    std::fs::write(&settings, "[cache]\nram_limit_mb = 512\n").expect("global settings");
    let seen: Arc<Mutex<Option<ravel_core::cache_budget::SharedCacheBudget>>> = Arc::default();
    let peak = Arc::new(AtomicUsize::new(0));

    let mut watcher = Watcher {
        budget: Arc::clone(&seen),
        peak: Arc::clone(&peak),
    };
    render_with_hooks(
        &args(&project, &dir.path().join("out")),
        Some(&settings),
        |budget| {
            *seen.lock().expect("budget") = Some(budget.clone());
            Ok(StubHooks::new())
        },
        &CancelFlag::new(),
        &mut watcher,
    )
    .expect("the render runs");

    assert!(
        peak.load(Ordering::SeqCst) > 0,
        "the render worker cached node results without reserving on the \
         budget the hooks were built with"
    );
    let budget = seen.lock().expect("budget");
    assert_eq!(
        budget
            .as_ref()
            .expect("the factory ran")
            .stats()
            .limit(ravel_core::cache_budget::Tier::Ram),
        512 * 1024 * 1024,
        "the render resolved its budget from the settings file it was given"
    );
}

/// nothing expensive is built until the render is known to be worth starting.
#[test]
fn the_evaluation_hooks_are_not_built_until_the_render_is_decided() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = TempDir::new().expect("tempdir");
    let project = project_file(dir.path(), document(false));
    let built = AtomicUsize::new(0);
    // Captures only `&built`, so the closure is `Copy` and can be handed to
    // both calls even though the parameter takes it by value.
    let hooks = |_budget: &_| -> Result<StubHooks, CliError> {
        built.fetch_add(1, Ordering::SeqCst);
        Ok(StubHooks::new())
    };

    let mut refused = args(&project, &dir.path().join("refused"));
    refused.params = vec!["nosuch=1".to_string()];
    let result = render_with_hooks(
        &refused,
        None,
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
        None,
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

// ===========================================================================
// Sound
// ===========================================================================

/// The soundtrack an image sequence cannot carry, written beside it.
///
/// **Every test here needs a decoder**, so the module is behind the `ffmpeg`
/// feature and neither CI nor a plain `cargo test` runs it — the reason
/// `cargo test --workspace --features ffmpeg` is the only way to see this
/// unit verified. What they check is the part that is *not* the mixer's:
/// that the range of sound matches the range of picture, that a split render
/// joins back up, and that an asset at another sample rate arrives where it
/// belongs.
#[cfg(feature = "ffmpeg")]
mod sound {
    use super::*;
    use ravel_core::composition::MediaAssetEntry;

    /// Sample rate of the fixture asset: deliberately **not** the render's
    /// 48 kHz, so every test here also exercises the conversion.
    const SOURCE_RATE: u32 = 44_100;
    /// The render's own rate (`ravel_cli::audio::OUTPUT_SAMPLE_RATE`).
    const OUT_RATE: u32 = 48_000;
    /// Composition frame rate: 24 fps divides both rates evenly, so the
    /// expected sample counts are exact rather than approximate.
    const FPS: u32 = 24;
    /// Seconds of source audio, and of composition.
    const SECONDS: u32 = 4;
    /// Where the fixture's marker sits, in seconds. Off a frame-range
    /// boundary so a test can tell "the range started here" from "the range
    /// happened to be empty".
    const MARKER_SECS: f32 = 1.5;
    /// Length of the marker in source sample frames — long enough to survive
    /// resampling as an unmistakable peak, short enough to locate.
    const MARKER_FRAMES: usize = 441;

    // -----------------------------------------------------------------------
    // Fixture
    // -----------------------------------------------------------------------

    /// Write a 44.1 kHz stereo WAV that is silent except for a full-scale
    /// burst at [`MARKER_SECS`].
    ///
    /// Silence-with-a-marker rather than a tone: "where did this end up" is
    /// answerable by finding one peak, without reimplementing the resampler
    /// in the assertions.
    fn source_asset(dir: &Path) -> PathBuf {
        use ravel_media::encode::WavWriter;

        let path = dir.join("voice.wav");
        let frames = (SOURCE_RATE * SECONDS) as usize;
        let marker_at = (MARKER_SECS * SOURCE_RATE as f32) as usize;
        let mut samples = vec![0.0_f32; frames * 2];
        for frame in marker_at..marker_at + MARKER_FRAMES {
            samples[frame * 2] = 1.0;
            samples[frame * 2 + 1] = 1.0;
        }
        let mut writer = WavWriter::create(&path, SOURCE_RATE, 2).expect("fixture WAV");
        writer.write_samples(&samples).expect("fixture samples");
        writer.finish().expect("fixture finishes");
        path
    }

    /// The picture document, plus one layer whose sound is `asset`.
    fn document_with_sound(asset: &Path) -> Document {
        let mut comp = Composition::new(
            CompId::new(COMP),
            "Main",
            (8, 4),
            FrameRate::new(FPS, 1),
            (FPS * SECONDS) as u64,
        );
        comp.background_color = Color::new(0.0, 0.0, 0.0, 1.0);
        comp = comp.add_layer(
            Layer::new(LayerId::new(1), "picture", layer_network(source().raw())).with_time(
                0,
                0,
                (FPS * SECONDS) as u64,
            ),
        );
        let mut voice = Layer::new(LayerId::new(2), "voice", layer_network(100)).with_time(
            0,
            0,
            (FPS * SECONDS) as u64,
        );
        voice.audio = Some(AudioSource::new("voice", 0));
        comp = comp.add_layer(voice);

        let mut document = Document::default().with_composition(comp);
        document
            .media_assets
            .insert("voice".into(), MediaAssetEntry::from_absolute(asset));
        document
    }

    /// A project on disk whose audio layer resolves to a real file.
    fn sounding_project(dir: &Path) -> PathBuf {
        let asset = source_asset(dir);
        project_file(dir, document_with_sound(&asset))
    }

    fn sound_args(project: &Path, output: &Path, range: &str) -> RenderArgs {
        let mut args = args(project, output);
        args.range = Some(range.parse().expect("range"));
        args.no_audio = false;
        args
    }

    // -----------------------------------------------------------------------
    // Reading a WAV back
    // -----------------------------------------------------------------------

    struct Wav {
        rate: u32,
        channels: u32,
        /// Interleaved samples.
        samples: Vec<f32>,
    }

    impl Wav {
        fn frame_count(&self) -> usize {
            self.samples.len() / self.channels as usize
        }

        /// The first sample frame at or above `threshold` on channel 0.
        fn peak_frame(&self, threshold: f32) -> Option<usize> {
            self.samples
                .chunks_exact(self.channels as usize)
                .position(|frame| frame[0] >= threshold)
        }
    }

    /// Walk the RIFF chunks rather than assuming a header size, so this
    /// reader would notice the writer changing the layout under it.
    fn read_wav(path: &Path) -> Wav {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());

        let (mut rate, mut channels, mut samples) = (0, 0, Vec::new());
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size = u32_at(at + 4) as usize;
            let body = at + 8;
            match id {
                b"fmt " => {
                    assert_eq!(u16_at(body), 3, "WAVE_FORMAT_IEEE_FLOAT");
                    channels = u16_at(body + 2) as u32;
                    rate = u32_at(body + 4);
                    assert_eq!(u16_at(body + 14), 32, "32 bits per sample");
                }
                b"data" => {
                    samples = bytes[body..body + size]
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                        .collect();
                }
                _ => {}
            }
            at = body + size + (size % 2);
        }
        assert!(rate > 0 && channels > 0, "the WAV has a fmt chunk");
        Wav {
            rate,
            channels,
            samples,
        }
    }

    /// Every name in `dir`, so a test can say "and nothing else" — the
    /// soundtrack's temporary file included, which [`frames`] would count and
    /// [`read_wav`] would never look for.
    fn entries(dir: &Path) -> Vec<String> {
        let Ok(read) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = read
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// The headline: sound and picture cover the same span, and the file is
    /// where a reader would look for it — beside the frames, named after the
    /// same absolute range.
    #[test]
    fn the_soundtrack_covers_exactly_the_rendered_frames() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        let run = run(&sound_args(&project, &out, "0-9"));

        assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());
        assert_eq!(frames(&out).len(), 10, "ten pictures");

        let path = out.join("frame_0000-0009.wav");
        assert_eq!(
            run.summary().audio.as_deref(),
            Some(path.as_path()),
            "the summary names the soundtrack it wrote"
        );
        let wav = read_wav(&path);
        assert_eq!(wav.rate, OUT_RATE);
        assert_eq!(wav.channels, 2);
        assert_eq!(
            wav.frame_count(),
            (10 * OUT_RATE / FPS) as usize,
            "ten frames of 24 fps is ten frames of 48 kHz sound"
        );
        assert!(
            run.recorder.notes.is_empty(),
            "a render that carries its sound has nothing to warn about: {:?}",
            run.recorder.notes
        );
    }

    /// A 44.1 kHz asset has to arrive at the output rate with its content in
    /// the right place — which is what `MED-MED-03`'s decoder-delay
    /// compensation buys. The marker is at 1.5 s of the source, so it must
    /// be at 1.5 s of the render, not at `1.5 × 48000/44100` s.
    #[test]
    fn a_source_at_another_rate_lands_at_the_same_moment() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        let run = run(&sound_args(&project, &out, "0-95"));
        assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());

        let wav = read_wav(&out.join("frame_0000-0095.wav"));
        assert_eq!(wav.rate, OUT_RATE);
        let expected = (MARKER_SECS * OUT_RATE as f32) as usize;
        let found = wav.peak_frame(0.5).expect("the marker survives the render");
        assert!(
            found.abs_diff(expected) < OUT_RATE as usize / 100,
            "the 1.5 s marker landed at {found} instead of {expected} \
             (a rate error would put it near {})",
            (MARKER_SECS * SOURCE_RATE as f32) as usize
        );
    }

    /// `--range` moves the window over the sound exactly as it moves it over
    /// the picture: the marker at 1.5 s belongs to the second of the four
    /// one-second slices, half a second into it, and to no other.
    #[test]
    fn a_range_starts_the_sound_at_its_own_position() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());

        let mut where_found = Vec::new();
        for (index, range) in ["0-23", "24-47", "48-71", "72-95"].iter().enumerate() {
            let out = dir.path().join(format!("slice{index}"));
            let run = run(&sound_args(&project, &out, range));
            assert_eq!(run.code(), 0, "{:?}", run.result.as_ref().err());

            let wav = read_wav(run.summary().audio.as_ref().expect("a soundtrack"));
            assert_eq!(
                wav.frame_count(),
                (24 * OUT_RATE / FPS) as usize,
                "every slice is one second long"
            );
            where_found.push(wav.peak_frame(0.5));
        }

        assert_eq!(where_found[0], None, "nothing in the first second");
        assert_eq!(where_found[2], None, "nor in the third");
        assert_eq!(where_found[3], None, "nor in the fourth");
        let found = where_found[1].expect("the marker is in the second second");
        let expected = OUT_RATE as usize / 2;
        assert!(
            found.abs_diff(expected) < OUT_RATE as usize / 100,
            "the marker is half a second into the slice, not at {found}"
        );
    }

    /// The split guarantee, for sound: concatenating the WAVs of two
    /// disjoint ranges reproduces the WAV of the whole, sample for sample.
    /// Without boundary-converted sample positions this drifts.
    #[test]
    fn split_soundtracks_concatenate_into_the_whole_one() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());

        let whole_dir = dir.path().join("whole");
        assert_eq!(run(&sound_args(&project, &whole_dir, "0-47")).code(), 0);
        let whole = read_wav(&whole_dir.join("frame_0000-0047.wav"));

        // Split at a frame that is not a round number of seconds, so an
        // off-by-one in the boundary arithmetic has somewhere to show.
        let first_dir = dir.path().join("first");
        let second_dir = dir.path().join("second");
        assert_eq!(run(&sound_args(&project, &first_dir, "0-18")).code(), 0);
        assert_eq!(run(&sound_args(&project, &second_dir, "19-47")).code(), 0);
        let first = read_wav(&first_dir.join("frame_0000-0018.wav"));
        let second = read_wav(&second_dir.join("frame_0019-0047.wav"));

        assert_eq!(
            first.frame_count() + second.frame_count(),
            whole.frame_count(),
            "the halves are exactly the whole, with no sample lost or repeated"
        );
        let joined: Vec<f32> = first
            .samples
            .iter()
            .chain(second.samples.iter())
            .copied()
            .collect();
        assert_eq!(
            joined, whole.samples,
            "and they are the same samples, in the same places"
        );
    }

    /// A soundtrack for frames that are not there is exactly the partial
    /// output an interrupted render promises not to leave.
    #[test]
    fn an_interrupted_render_takes_the_soundtrack_with_it() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        let cancel = CancelFlag::new();

        let run = run_with(
            &sound_args(&project, &out, "0-9"),
            StubHooks::slow(),
            Recorder::cancelling(2, &cancel),
            &cancel,
        );

        assert_eq!(run.code(), EXIT_CANCELLED);
        assert!(frames(&out).is_empty(), "no frames survive");
        assert!(
            !out.join("frame_0000-0009.wav").exists(),
            "and neither does the sound that was written before them"
        );
        assert!(
            entries(&out).is_empty(),
            "nor the temporary file it was written through: {:?}",
            entries(&out)
        );
    }

    /// The failure that happens **before** a frame rather than during one: no
    /// GPU adapter, which is the ordinary state of a render node that has been
    /// handed the wrong job. The sound is decoded first, so the only thing
    /// keeping a soundtrack for a picture that does not exist off the disk is
    /// that the device is built before the mix and the mix lands on a
    /// temporary name.
    #[test]
    fn a_render_that_cannot_build_its_evaluator_writes_no_soundtrack() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");

        let error = render_with_hooks(
            &sound_args(&project, &out, "0-9"),
            None,
            |_budget| Err::<StubHooks, _>(CliError::Gpu("no adapter on this machine".into())),
            &CancelFlag::new(),
            &mut Recorder::default(),
        )
        .expect_err("a render with no evaluator cannot succeed");

        assert_eq!(error.id(), "no-gpu");
        assert!(
            !out.exists(),
            "not even a directory: nothing may be written before the render can start, \
             and least of all a soundtrack — {:?}",
            entries(&out)
        );
    }

    /// `--overwrite` is permission to replace the previous soundtrack with a
    /// new one. It is not permission to destroy it on behalf of a render that
    /// then does not finish — which is what truncating the real name up front
    /// would do, leaving neither the old sound nor the new.
    #[test]
    fn an_interrupted_overwrite_leaves_the_previous_soundtrack_whole() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("output directory");
        let soundtrack = out.join("frame_0000-0009.wav");
        std::fs::write(&soundtrack, b"the previous render").expect("an earlier soundtrack");

        let cancel = CancelFlag::new();
        let mut overwriting = sound_args(&project, &out, "0-9");
        overwriting.overwrite = true;
        let run = run_with(
            &overwriting,
            StubHooks::slow(),
            Recorder::cancelling(2, &cancel),
            &cancel,
        );

        assert_eq!(run.code(), EXIT_CANCELLED);
        assert!(frames(&out).is_empty(), "no frames survive");
        assert_eq!(
            std::fs::read(&soundtrack).expect("the previous soundtrack is still readable"),
            b"the previous render".to_vec(),
            "an interrupted overwrite must not cost the old sound as well as the new"
        );
        assert_eq!(
            entries(&out),
            vec!["frame_0000-0009.wav".to_string()],
            "and the temporary file the new mix went to is gone"
        );
    }

    /// An asset the render cannot load is a warning, not a failure — the
    /// picture is still worth having — but it is never silence without a
    /// word. This is the visible consequence `MAX_DECODE_BYTES` and every
    /// other decode refusal come out through.
    #[test]
    fn a_source_that_cannot_be_loaded_is_reported_and_the_render_goes_on() {
        let dir = TempDir::new().expect("tempdir");
        let asset = dir.path().join("gone.wav");
        let mut document = document_with_sound(&asset);
        // Present in the document, absent from the disk: the shape every
        // decode refusal reaches the CLI in.
        document
            .media_assets
            .get_mut("voice")
            .expect("the fixture asset")
            .resolved = None;
        let project = project_file(dir.path(), document);
        let out = dir.path().join("out");

        let run = run(&sound_args(&project, &out, "0-9"));
        assert_eq!(run.code(), 0, "the picture still renders");
        assert!(
            run.recorder
                .notes
                .contains(&"audio-source-skipped".to_string()),
            "the missing sound has to be said out loud: {:?}",
            run.recorder.notes
        );
        // Still the right length, so the silent stretch stays in sync with
        // the picture rather than shortening the deliverable.
        let wav = read_wav(&out.join("frame_0000-0009.wav"));
        assert_eq!(wav.frame_count(), (10 * OUT_RATE / FPS) as usize);
        assert!(wav.samples.iter().all(|s| *s == 0.0));
    }

    /// A soundtrack already on disk is output like any other, so it is
    /// refused before a frame is evaluated unless overwriting was asked for.
    #[test]
    fn an_existing_soundtrack_is_refused_like_an_existing_frame() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("output directory");
        std::fs::write(out.join("frame_0000-0009.wav"), b"previous render")
            .expect("the earlier soundtrack");

        let refused = run(&sound_args(&project, &out, "0-9"));
        assert_eq!(refused.code(), EXIT_OUTPUT_EXISTS);
        assert!(frames(&out).is_empty(), "nothing was evaluated");

        let mut overwriting = sound_args(&project, &out, "0-9");
        overwriting.overwrite = true;
        assert_eq!(run(&overwriting).code(), 0, "asked for, it is replaced");
        assert!(read_wav(&out.join("frame_0000-0009.wav")).frame_count() > 0);
    }

    /// A link to nothing is not a free name: `WavWriter::create` follows it,
    /// so calling it free is a render that starts and then truncates a file
    /// outside its own output directory. The frames have always counted such a
    /// link as occupied; the sound now asks the same question.
    #[cfg(unix)]
    #[test]
    fn a_soundtrack_path_that_is_a_dangling_symlink_is_refused() {
        let dir = TempDir::new().expect("tempdir");
        let project = sounding_project(dir.path());
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("output directory");
        let elsewhere = dir.path().join("someone-elses.wav");
        std::os::unix::fs::symlink(&elsewhere, out.join("frame_0000-0009.wav")).expect("symlink");

        let refused = run(&sound_args(&project, &out, "0-9"));
        assert_eq!(refused.code(), EXIT_OUTPUT_EXISTS);
        assert!(frames(&out).is_empty(), "nothing was evaluated");
        assert!(
            !elsewhere.exists(),
            "and nothing was written through the link"
        );

        // Asked for, the link is *replaced* rather than written through:
        // publication is a rename, which swaps the name and does not follow
        // it.
        let mut overwriting = sound_args(&project, &out, "0-9");
        overwriting.overwrite = true;
        assert_eq!(run(&overwriting).code(), 0);
        assert!(
            !elsewhere.exists(),
            "the render replaced the link, not what it pointed at"
        );
        assert!(read_wav(&out.join("frame_0000-0009.wav")).frame_count() > 0);
    }
}

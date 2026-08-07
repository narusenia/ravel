// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The shipped binary, end to end.
//!
//! `render_cli.rs` pins the guarantees with a stub processor, which is the
//! only way to get bit-exact answers on every machine. What it cannot see is
//! whether the binary wires the pieces together: real processors, a real GPU
//! context, real argument parsing, real exit codes, real JSON on stdout.
//! That seam is what these cover, so they are deliberately few.
//!
//! **The render here needs a GPU adapter**, the same requirement
//! `ravel-nodes`' own integration tests carry.

use std::path::{Path, PathBuf};
use std::process::Command;

use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::types::FrameRate;
use ravel_project::ProjectFile;
use tempfile::TempDir;

/// `shape.rect → rasterize → net.out(frame)` — a network of real built-in
/// nodes, so the binary's own processor registration is what draws.
fn layer_network() -> Graph {
    Graph::new()
        .add_node(
            Node::new(NodeId::new(500), "shape.rect")
                .with_output("output", DataTypeId::GEOMETRY)
                .with_param("center", ParameterValue::vec2(32.0, 32.0))
                .with_param("width", ParameterValue::Float(32.0))
                .with_param("height", ParameterValue::Float(32.0)),
        )
        .expect("rect")
        .add_node(
            Node::new(NodeId::new(501), "rasterize")
                .with_input("geometry", &[DataTypeId::GEOMETRY])
                .with_output("output", DataTypeId::FRAME_BUFFER),
        )
        .expect("rasterize")
        .add_node(
            Node::new(NodeId::new(502), net::NET_IN_TYPE_KEY)
                .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                .with_output(net::PORT_TIME, DataTypeId::SCALAR),
        )
        .expect("net.in")
        .add_node(
            Node::new(NodeId::new(503), net::NET_OUT_TYPE_KEY)
                .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
        )
        .expect("net.out")
        .add_edge(
            EdgeId::new(1),
            NodeId::new(500),
            OutputPortIndex(0),
            NodeId::new(501),
            InputPortIndex(0),
        )
        .expect("rect → rasterize")
        .add_edge(
            EdgeId::new(2),
            NodeId::new(501),
            OutputPortIndex(0),
            NodeId::new(503),
            InputPortIndex(0),
        )
        .expect("rasterize → out")
}

fn project(dir: &Path) -> PathBuf {
    let comp = Composition::new(CompId::new(1), "Main", (64, 64), FrameRate::new(24, 1), 100)
        .add_layer(Layer::new(LayerId::new(1), "shape", layer_network()).with_time(0, 0, 100));
    let document = Document::default().with_composition(comp);
    let path = dir.join("binary.ravprj");
    ProjectFile::from_document("Binary", "2026-01-01T00:00:00Z", document)
        .save(&path)
        .expect("fixture saves");
    path
}

/// Run the binary with the repository's locale catalogs, so the messages it
/// prints are the real ones rather than raw keys.
fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ravel-cli"));
    command.env(
        "RAVEL_LOCALE_DIR",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales"),
    );
    command
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!(
            "expected JSON, got {error}: {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

#[test]
fn list_codecs_reports_every_target_with_a_verdict() {
    let output = cli().args(["list", "codecs"]).output().expect("runs");
    assert!(output.status.success(), "{output:?}");

    let value = json(&output.stdout);
    let codecs = value["codecs"].as_array().expect("codecs array");
    assert!(codecs.len() >= 2, "{value}");

    let png = codecs
        .iter()
        .find(|row| row["format"] == "png")
        .expect("png is always enumerated");
    assert_eq!(png["available"], true);
    assert_eq!(png["writable"], true);
    assert_eq!(png["route"], "native");

    for row in codecs {
        assert_eq!(
            row["available"].as_bool().expect("available"),
            row.get("reason").is_none(),
            "every row carries exactly one of a route and a reason: {row}"
        );
    }
}

#[test]
fn list_comps_and_params_describe_the_project() {
    let dir = TempDir::new().expect("tempdir");
    let project = project(dir.path());

    let comps = cli()
        .args(["list", "comps"])
        .arg(&project)
        .output()
        .expect("runs");
    assert!(comps.status.success(), "{comps:?}");
    let value = json(&comps.stdout);
    let first = &value["compositions"][0];
    assert_eq!(first["name"], "Main");
    assert_eq!(first["duration_frames"], 100);
    assert_eq!(first["root"], true);

    let params = cli()
        .args(["list", "params"])
        .arg(&project)
        .output()
        .expect("runs");
    assert!(params.status.success(), "{params:?}");
    assert!(
        json(&params.stdout)["parameters"]
            .as_array()
            .expect("parameters array")
            .is_empty(),
        "this fixture declares none"
    );
}

/// The headline: ten frames in, ten readable PNGs out, through the real
/// evaluator. **Requires a GPU adapter.**
#[test]
fn a_ten_frame_render_writes_ten_readable_pngs() {
    let dir = TempDir::new().expect("tempdir");
    let project = project(dir.path());
    let out = dir.path().join("frames");

    let output = cli()
        .arg("render")
        .arg(&project)
        .args(["--range", "0-9", "--progress", "json", "-o"])
        .arg(&out)
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut frames: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("output directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    frames.sort();
    assert_eq!(frames.len(), 10, "{frames:?}");
    assert_eq!(
        frames[0].file_name().unwrap().to_string_lossy(),
        "frame_0000.png"
    );

    let image = image::open(&frames[0]).expect("the frames are readable PNGs");
    assert_eq!((image.width(), image.height()), (64, 64));

    // Every line of `--progress json` is a JSON object, ending in the
    // completion record a script waits for.
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    let last = lines.last().expect("at least one line");
    assert_eq!(last["event"], "completed");
    assert_eq!(last["frames"], 10);
}

/// The classified exit codes are the contract with whatever drives the CLI,
/// so the binary has to actually return them.
#[test]
fn failures_return_their_classified_exit_codes() {
    let dir = TempDir::new().expect("tempdir");
    let project = project(dir.path());
    let out = dir.path().join("frames");

    // A project that is not there.
    let missing = cli()
        .arg("render")
        .arg(dir.path().join("nothing.ravprj"))
        .args(["--progress", "quiet", "-o"])
        .arg(&out)
        .output()
        .expect("runs");
    assert_eq!(
        missing.status.code(),
        Some(ravel_cli::error::EXIT_LOAD as i32)
    );

    // A format this build cannot write.
    let codec = cli()
        .arg("render")
        .arg(&project)
        .args(["--format", "h265", "--progress", "quiet", "-o"])
        .arg(&out)
        .output()
        .expect("runs");
    assert_eq!(
        codec.status.code(),
        Some(ravel_cli::error::EXIT_CODEC as i32),
        "{}",
        String::from_utf8_lossy(&codec.stderr)
    );
    assert!(!out.exists(), "a refused render must create nothing");

    // A malformed command line is clap's own code 2, which is the same
    // "arguments are wrong" class.
    let usage = cli().args(["render", "--range", "nonsense"]).output();
    assert_eq!(
        usage.expect("runs").status.code(),
        Some(ravel_cli::error::EXIT_USAGE as i32)
    );
}

/// Every message a person can see has to come from the catalogs. A missing
/// key shows as the key itself, which is exactly what this catches.
#[test]
fn user_facing_messages_come_from_the_locale_catalogs() {
    let dir = TempDir::new().expect("tempdir");
    let output = cli()
        .arg("render")
        .arg(dir.path().join("nothing.ravprj"))
        .args(["--progress", "quiet", "-o"])
        .arg(dir.path().join("frames"))
        .output()
        .expect("runs");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cli.error."),
        "a raw locale key reached the user: {stderr}"
    );
    assert!(
        stderr.contains("nothing.ravprj"),
        "the message names the file: {stderr}"
    );
}

/// The CLI's whole point is that a headless host cannot link a window
/// toolkit. Cargo enforces it by compiling, but only as long as nobody adds
/// the dependency — so say it out loud where a reviewer of that change would
/// see it fail.
#[test]
fn the_cli_does_not_depend_on_the_gui_stack() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("the crate's own manifest");
    for forbidden in [
        "gpui",
        "gpui_platform",
        "gpui-component",
        "ravel-ui",
        "ravel-dock",
        "ravel-app",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "ravel-cli must not depend on {forbidden}: the GUI-free guarantee is the crate's reason to exist"
        );
    }
}

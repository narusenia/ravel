// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ravel-cli mcp` as a child process speaking over pipes. Nothing here
//! needs a GPU: the calls that would render are refused or fail before one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::types::FrameRate;
use ravel_project::ProjectFile;
use serde_json::{Value, json};
use tempfile::TempDir;

fn project(dir: &Path) -> PathBuf {
    let network = Graph::new()
        .add_node(
            Node::new(NodeId::new(5), "shape.rect").with_param("width", ParameterValue::Float(3.0)),
        )
        .expect("node");
    let comp = Composition::new(CompId::new(1), "Main", (8, 4), FrameRate::new(24, 1), 10)
        .add_layer(Layer::new(LayerId::new(1), "shape", network));
    let path = dir.join("p.ravprj");
    ProjectFile::from_document(
        "P",
        "2026-01-01T00:00:00Z",
        Document::default().with_composition(comp),
    )
    .save(&path)
    .expect("save");
    path
}

fn command(name: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ravel-cli"));
    command.env(
        "RAVEL_LOCALE_DIR",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales"),
    );
    command.arg(name);
    command
}

/// Send `requests` (one per line), close stdin, and return the responses.
fn session(allow: &[&Path], requests: &[Value]) -> Vec<Value> {
    let mut cmd = command("mcp");
    for dir in allow {
        cmd.arg("--allow-dir").arg(dir);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawns");
    let mut stdin = child.stdin.take().expect("stdin");
    for request in requests {
        writeln!(stdin, "{request}").expect("write");
    }
    drop(stdin);
    let output = child.wait_with_output().expect("exits on EOF");
    assert!(output.status.success(), "EOF is a clean exit: {output:?}");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout is protocol only: {e}: {line}"))
        })
        .collect()
}

fn call(id: u64, tool: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}})
}

/// What `ravel-cli list <args>` prints.
fn cli_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ravel-cli"))
        .arg("list")
        .args(args)
        .env(
            "RAVEL_LOCALE_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales"),
        )
        .output()
        .expect("runs");
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).expect("json")
}

#[test]
fn initialize_list_and_call() {
    let responses = session(
        &[],
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            call(3, "list_codecs", json!({})),
        ],
    );
    assert_eq!(responses.len(), 3, "the notification is not answered");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "list_comps",
            "list_params",
            "list_codecs",
            "inspect_document",
            "check_warnings",
            "render_preview",
            "render"
        ]
    );
    assert_eq!(responses[2]["id"], 3);
}

#[test]
fn listings_equal_the_cli_json() {
    let dir = TempDir::new().expect("tempdir");
    let project = project(dir.path());
    let project_arg = project.to_str().expect("utf8 path");
    let responses = session(
        &[],
        &[
            call(1, "list_comps", json!({"project": project})),
            call(2, "list_params", json!({"project": project})),
            call(3, "list_codecs", json!({})),
        ],
    );
    let structured = |i: usize| responses[i]["result"]["structuredContent"].clone();
    assert_eq!(structured(0), cli_json(&["comps", project_arg]));
    assert_eq!(structured(1), cli_json(&["params", project_arg]));
    assert_eq!(structured(2), cli_json(&["codecs"]));
    assert!(
        structured(0)["compositions"][0]["name"] == "Main",
        "the comparison is not between two empty values"
    );
    // The text content is the same value, as a string.
    let text: Value = serde_json::from_str(
        responses[0]["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(text, structured(0));
}

#[test]
fn failures_carry_stable_ids() {
    let dir = TempDir::new().expect("tempdir");
    let project = project(dir.path());
    let out = dir.path().join("out");
    let responses = session(
        &[dir.path()],
        &[
            call(
                1,
                "list_comps",
                json!({"project": dir.path().join("absent.ravprj")}),
            ),
            call(
                2,
                "render",
                json!({"project": project, "comp": "Nope", "output": out}),
            ),
            call(
                3,
                "check_warnings",
                json!({"project": project, "comp": "Nope"}),
            ),
        ],
    );
    assert_eq!(responses.len(), 3);
    for (reply, id) in
        responses
            .iter()
            .zip(["load-failed", "unknown-composition", "unknown-composition"])
    {
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["id"], id, "{reply}");
        assert!(reply["result"]["structuredContent"]["message"].is_string());
    }
    assert!(!out.exists(), "a refused render writes nothing");
}

#[test]
fn check_warnings_returns_a_list() {
    let dir = TempDir::new().expect("tempdir");
    let responses = session(
        &[],
        &[call(
            1,
            "check_warnings",
            json!({"project": project(dir.path())}),
        )],
    );
    assert!(
        responses[0]["result"]["structuredContent"]["warnings"].is_array(),
        "{}",
        responses[0]
    );
}

#[test]
fn output_outside_the_allowed_directories_is_refused() {
    let dir = TempDir::new().expect("tempdir");
    let allowed = dir.path().join("allowed");
    std::fs::create_dir(&allowed).expect("dir");
    let project = project(dir.path());
    let outside = dir.path().join("outside");
    let escape = allowed.join("../outside");
    let responses = session(
        &[&allowed],
        &[
            call(1, "render", json!({"project": project, "output": outside})),
            call(
                2,
                "render_preview",
                json!({"project": project, "frame": 0, "output": escape}),
            ),
        ],
    );
    assert_eq!(responses.len(), 2);
    for reply in &responses {
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["structuredContent"]["id"], "output-not-allowed",
            "{reply}"
        );
        assert_eq!(reply["result"]["structuredContent"]["code"], 2);
    }
    assert!(!outside.exists());
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Assembling a project through `ravel-cli mcp`, in process, with stub
//! evaluation hooks so no GPU is needed: build in the draft, save to a new
//! file, read it back, and render it through the same planning and writer the
//! `render` command uses.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ravel_cli::args::{OutputFormat, PngBits, ProgressMode, RenderArgs};
use ravel_cli::execute::CancelFlag;
use ravel_cli::mcp::Server;
use ravel_cli::report::Reporter;
use ravel_cli::{render_draft_with_hooks, render_with_hooks};
use ravel_core::composition::Document;
use ravel_core::eval::{
    EvalContext, EvalScope, NodeProcessor, ProcessorRegistry as _, ResolvedParams,
};
use ravel_core::graph::{Graph, Node};
use ravel_core::id::NodeId;
use ravel_core::runtime::eval_service::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_project::ProjectFile;
use serde_json::{Value, json};
use tempfile::TempDir;

/// A flat frame for every node: what the render path is asked, not what a
/// node draws, is under test.
struct Flat;

impl NodeProcessor for Flat {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = ctx.resolution;
        let pixels = std::iter::repeat_n([0.5, 0.5, 0.5, 1.0], (width * height) as usize)
            .flatten()
            .collect();
        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }
}

struct StubHooks;

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
            evaluator.register(id, Arc::new(Flat));
        }
    }
}

fn server(allow: &[PathBuf]) -> Server {
    Server::with_renderers(
        allow,
        Box::new(|args, cancel, rep| render_with_hooks(args, None, |_| Ok(StubHooks), cancel, rep)),
        Box::new(|args, draft, root, cancel, rep| {
            render_draft_with_hooks(args, draft, root, None, |_| Ok(StubHooks), cancel, rep)
        }),
    )
}

fn call(server: &Server, tool: &str, args: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    server.handle_line(&request.to_string()).expect("answered")
}

fn ok(reply: &Value) -> &Value {
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    &reply["result"]["structuredContent"]
}

fn failure(reply: &Value) -> &Value {
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    &reply["result"]["structuredContent"]
}

/// One composition, one solid layer, a rectangle wired into a transform.
fn assemble() -> Value {
    json!([
        {"op": "add_composition", "as": "c", "settings": {
            "name": "Main", "resolution": [8, 4], "frame_rate": {"num": 24, "den": 1},
            "duration_frames": 4, "background_color": {"r": 0, "g": 0, "b": 0, "a": 1}}},
        {"op": "add_layer", "comp": "c", "template": "solid", "as": "l"},
        {"op": "add_node", "path": {"comp": "c", "layer": "l"}, "type_key": "shape.rect",
         "params": {"width": {"type": "float", "value": 150.0}}, "as": "r"},
        {"op": "add_node", "path": {"comp": "c", "layer": "l"},
         "type_key": "geometry.transform", "as": "t"},
        {"op": "connect", "path": {"comp": "c", "layer": "l"},
         "from": {"node": "r", "port": 0}, "to": {"node": "t", "port": 0}},
    ])
}

fn render_args(project: &Path, output: &Path) -> RenderArgs {
    RenderArgs {
        project: project.to_path_buf(),
        comp: None,
        range: Some("0-1".parse().expect("range")),
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

struct Quiet;
impl Reporter for Quiet {}

fn pngs(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn an_assembled_project_saves_loads_and_renders() {
    let dir = TempDir::new().expect("tempdir");
    let allowed = dir.path().to_path_buf();
    let server = server(std::slice::from_ref(&allowed));

    ok(&call(&server, "new_project", json!({"name": "Built"})));
    let edited = call(&server, "edit", json!({"ops": assemble()}));
    assert_eq!(ok(&edited)["created"].as_array().expect("created").len(), 5);
    assert_eq!(ok(&edited)["aliases"]["c"]["kind"], "composition");

    // The draft reads back through the existing tools.
    let inspected = call(&server, "inspect_document", json!({"draft": true}));
    let layer = &ok(&inspected)["compositions"][0]["layers"][0];
    let types: Vec<&str> = layer["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["type_key"].as_str().expect("type"))
        .collect();
    assert!(types.contains(&"shape.rect") && types.contains(&"geometry.transform"));
    // The solid template brings a rectangle of its own: pick ours by its alias.
    let rect_id = &ok(&edited)["aliases"]["r"]["id"];
    let rect = layer["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| &n["id"] == rect_id)
        .expect("the aliased node");
    assert_eq!(rect["parameters"]["width"], 150.0);
    ok(&call(&server, "check_warnings", json!({"draft": true})));

    // A preview of the unsaved draft goes through the draft render path.
    let previews = allowed.join("previews");
    let preview = call(
        &server,
        "render_preview",
        json!({"draft": true, "frame": 0, "output": previews}),
    );
    assert_eq!(ok(&preview)["frames"], 1, "{preview}");
    assert_eq!(pngs(&previews), 1);

    let path = allowed.join("built.ravprj");
    ok(&call(&server, "save_project", json!({"path": path})));
    let loaded = ProjectFile::load(&path).expect("the saved file loads");
    assert_eq!(loaded.document.compositions.len(), 1);

    // And the file renders, through the same planning and writer.
    let out = allowed.join("frames");
    let summary = render_with_hooks(
        &render_args(&path, &out),
        None,
        |_| Ok(StubHooks),
        &CancelFlag::new(),
        &mut Quiet,
    )
    .expect("the saved project renders");
    assert_eq!(summary.frames, 2);
    assert_eq!(pngs(&out), 2);
}

#[test]
fn a_path_that_exists_is_never_written() {
    let dir = TempDir::new().expect("tempdir");
    let server = server(&[dir.path().to_path_buf()]);
    ok(&call(&server, "new_project", json!({})));
    ok(&call(&server, "edit", json!({"ops": assemble()})));

    let taken = dir.path().join("taken.ravprj");
    std::fs::write(&taken, b"precious").expect("write");
    let reply = call(&server, "save_project", json!({"path": taken}));
    assert_eq!(failure(&reply)["id"], "project-exists");
    assert_eq!(std::fs::read(&taken).expect("read"), b"precious");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("dir")
        .flatten()
        .collect();
    assert_eq!(leftovers.len(), 1, "no temporary or backup file appeared");

    // A dangling symlink is refused as well (the allow-dir check cannot resolve
    // it), and nothing is created behind it.
    #[cfg(unix)]
    {
        let link = dir.path().join("link.ravprj");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).expect("symlink");
        let reply = call(&server, "save_project", json!({"path": link}));
        failure(&reply);
        assert!(!dir.path().join("nowhere").exists());
    }
}

#[test]
fn a_save_outside_the_allowed_directories_or_without_the_extension_writes_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let allowed = dir.path().join("allowed");
    std::fs::create_dir(&allowed).expect("dir");
    let server = server(std::slice::from_ref(&allowed));
    ok(&call(&server, "new_project", json!({})));

    for path in [
        dir.path().join("outside.ravprj"),
        allowed.join("../outside.ravprj"),
    ] {
        let reply = call(&server, "save_project", json!({"path": path}));
        assert_eq!(failure(&reply)["id"], "output-not-allowed", "{path:?}");
    }
    let reply = call(
        &server,
        "save_project",
        json!({"path": allowed.join("a.txt")}),
    );
    assert_eq!(failure(&reply)["id"], "bad-project-path");
    assert!(!dir.path().join("outside.ravprj").exists());
    assert_eq!(std::fs::read_dir(&allowed).expect("dir").count(), 0);
    // Control: the same draft saves where it is allowed.
    ok(&call(
        &server,
        "save_project",
        json!({"path": allowed.join("ok.ravprj")}),
    ));
}

#[test]
fn a_failed_edit_leaves_the_draft_and_names_the_operation() {
    let dir = TempDir::new().expect("tempdir");
    let server = server(&[dir.path().to_path_buf()]);
    let reply = call(&server, "edit", json!({"ops": []}));
    assert_eq!(failure(&reply)["id"], "no-draft");
    ok(&call(&server, "new_project", json!({})));
    ok(&call(&server, "edit", json!({"ops": assemble()})));
    let before = call(&server, "inspect_document", json!({"draft": true}));

    let bad = json!([
        {"op": "add_composition", "as": "d", "settings": {
            "name": "B", "resolution": [8, 4], "frame_rate": {"num": 24, "den": 1},
            "duration_frames": 4, "background_color": {"r": 0, "g": 0, "b": 0, "a": 1}}},
        {"op": "add_layer", "comp": "ghost", "template": "solid"},
    ]);
    let reply = call(&server, "edit", json!({"ops": bad}));
    assert_eq!(failure(&reply)["id"], "unknown-alias");
    assert_eq!(failure(&reply)["index"], 1);
    let after = call(&server, "inspect_document", json!({"draft": true}));
    assert_eq!(ok(&before), ok(&after), "the first operation was not kept");

    // An unknown parameter key is an error, not a silent no-op.
    let comp_id = ok(&before)["compositions"][0]["id"].clone();
    let layer_id = ok(&before)["compositions"][0]["layers"][0]["id"].clone();
    let node_id = ok(&before)["compositions"][0]["layers"][0]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["type_key"] == "shape.rect")
        .unwrap()["id"]
        .clone();
    let reply = call(
        &server,
        "edit",
        json!({"ops": [{"op": "set_parameter", "path": {"comp": comp_id, "layer": layer_id},
            "node": node_id, "key": "nope", "value": {"type": "float", "value": 1.0}}]}),
    );
    assert_eq!(failure(&reply)["id"], "unknown-parameter");
}

#[test]
fn the_draft_is_one_and_a_saved_one_can_be_replaced() {
    let dir = TempDir::new().expect("tempdir");
    let server = server(&[dir.path().to_path_buf()]);
    ok(&call(&server, "new_project", json!({})));
    assert_eq!(
        failure(&call(&server, "new_project", json!({})))["id"],
        "draft-exists"
    );
    ok(&call(&server, "new_project", json!({"discard": true})));

    // Saved, then edited again: the edit is unsaved work.
    ok(&call(
        &server,
        "save_project",
        json!({"path": dir.path().join("a.ravprj")}),
    ));
    ok(&call(&server, "new_project", json!({})));
    ok(&call(&server, "edit", json!({"ops": assemble()})));
    assert_eq!(
        failure(&call(&server, "new_project", json!({})))["id"],
        "draft-exists"
    );
    // The draft survives a save and goes on to another new path.
    ok(&call(
        &server,
        "save_project",
        json!({"path": dir.path().join("b.ravprj")}),
    ));
    assert!(dir.path().join("b.ravprj").exists());
    ok(&call(&server, "new_project", json!({})));
}

#[test]
fn read_tools_take_a_project_or_the_draft_not_both_or_neither() {
    let server = server(&[]);
    for args in [json!({}), json!({"draft": true, "project": "/x.ravprj"})] {
        let reply = call(&server, "inspect_document", args);
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    let reply = call(&server, "inspect_document", json!({"draft": true}));
    assert_eq!(failure(&reply)["id"], "no-draft");
}

#[test]
fn node_types_are_listed_and_described_from_the_registry() {
    let server = server(&[]);
    let listed = call(&server, "list_node_types", json!({}));
    let types = ok(&listed)["node_types"].as_array().expect("types");
    assert!(types.iter().any(|t| t["type_key"] == "shape.rect"));

    let described = call(&server, "describe_node", json!({"type_key": "shape.rect"}));
    let node = ok(&described);
    assert_eq!(node["type_key"], "shape.rect");
    let width = node["parameters"]
        .as_array()
        .expect("parameters")
        .iter()
        .find(|p| p["key"] == "width")
        .expect("a width parameter");
    assert_eq!(width["type"], "float");
    assert!(width["default"].is_number(), "{width}");
    assert!(!node["outputs"].as_array().expect("outputs").is_empty());

    let reply = call(&server, "describe_node", json!({"type_key": "no.such"}));
    assert_eq!(failure(&reply)["id"], "unknown-node-type");
}

#[test]
fn keyframes_and_the_layer_shell_are_edited_over_the_wire() {
    let server = server(&[]);
    ok(&call(&server, "new_project", json!({})));
    let target = json!({"comp": "c", "layer": "l"});
    let mut ops = assemble().as_array().unwrap().clone();
    ops.extend([
        // frame is the composition's frame; the layer starts at 2 below.
        json!({"op": "set_layer", "comp": "c", "layer": "l", "start_frame": 2, "name": "Keyed",
               "opacity": 0.5}),
        json!({"op": "set_keyframes", "path": target, "node": "r", "key": "width",
               "keys": [{"frame": 2, "value": 10, "interpolation": "linear"},
                        {"frame": 4, "value": 50, "interpolation": "bezier"}]}),
        json!({"op": "set_keyframes", "comp": "c", "layer": "l", "property": "position",
               "component": 1, "keys": [{"frame": 2, "value": 0, "interpolation": "step"}]}),
        json!({"op": "clear_animation", "comp": "c", "layer": "l", "property": "position",
               "component": 1, "value": 3}),
    ]);
    ok(&call(&server, "edit", json!({"ops": ops})));

    // A key before the layer's start is refused with its code and index.
    let doc = call(&server, "inspect_document", json!({"draft": true}));
    let comp_id = ok(&doc)["compositions"][0]["id"].clone();
    let layer_id = ok(&doc)["compositions"][0]["layers"][0]["id"].clone();
    let reply = call(
        &server,
        "edit",
        json!({"ops": [{"op": "set_keyframes", "comp": comp_id, "layer": layer_id,
            "property": "opacity",
            "keys": [{"frame": 0, "value": 1, "interpolation": "linear"}]}]}),
    );
    assert_eq!(failure(&reply)["id"], "key-before-layer-start");
    assert_eq!(failure(&reply)["index"], 0);
}

/// `live: true` against a fake app: the read tools see a copy of the app's
/// document through the very path `draft: true` takes.
#[cfg(unix)]
mod live {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    /// An app that answers each connection with `reply(request)`, or hangs up
    /// without a word when that is `None`.
    fn fake_app(dir: &Path, reply: impl Fn(&Value) -> Option<Value> + Send + 'static) -> PathBuf {
        let path = dir.join("live-edit.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).expect("line");
                let request: Value = serde_json::from_str(&line).expect("json");
                if let Some(answer) = reply(&request) {
                    let _ = writeln!(stream, "{answer}");
                }
            }
        });
        path
    }

    /// An app that has `project` open at `at`.
    fn app_with(dir: &Path, project: &ProjectFile, at: &Path) -> PathBuf {
        let text = ravel_project::document_to_ron(&project.document).expect("ron");
        let at = at.to_string_lossy().into_owned();
        fake_app(dir, move |request| {
            assert_eq!(request["read"], "document", "{request}");
            assert!(request.get("ops").is_none(), "a read carries no ops");
            Some(json!({"id": request["id"], "ok": {"project_path": at, "document": text}}))
        })
    }

    /// A saved project with one composition and one layer, and where it is.
    fn saved(dir: &Path) -> (PathBuf, ProjectFile) {
        let server = server(&[dir.to_path_buf()]);
        ok(&call(&server, "new_project", json!({"name": "Live"})));
        ok(&call(&server, "edit", json!({"ops": assemble()})));
        let path = dir.join("open-in-app.ravprj");
        ok(&call(&server, "save_project", json!({"path": path})));
        (path.clone(), ProjectFile::load(&path).expect("loads"))
    }

    #[test]
    fn inspect_live_equals_inspect_of_the_same_file() {
        let dir = TempDir::new().expect("tempdir");
        let (path, project) = saved(dir.path());
        let socket = app_with(dir.path(), &project, &path);
        let server = server(&[]).with_attach(socket);

        let from_file = call(&server, "inspect_document", json!({"project": path}));
        let from_app = call(&server, "inspect_document", json!({"live": true}));
        assert!(
            !ok(&from_file)["compositions"][0]["layers"][0]["nodes"]
                .as_array()
                .expect("nodes")
                .is_empty(),
            "the comparison must be about something"
        );
        assert_eq!(ok(&from_file), ok(&from_app));
        // The warnings plan reads the same copy.
        assert_eq!(
            ok(&call(&server, "check_warnings", json!({"live": true}))),
            ok(&call(&server, "check_warnings", json!({"project": path})))
        );
    }

    #[test]
    fn a_live_preview_is_a_png_in_the_allowed_directory() {
        let dir = TempDir::new().expect("tempdir");
        let (path, project) = saved(dir.path());
        let socket = app_with(dir.path(), &project, &path);
        let allowed = dir.path().join("allowed");
        std::fs::create_dir(&allowed).expect("dir");
        let server = server(std::slice::from_ref(&allowed)).with_attach(socket);

        let previews = allowed.join("previews");
        let reply = call(
            &server,
            "render_preview",
            json!({"live": true, "frame": 0, "output": previews}),
        );
        assert_eq!(ok(&reply)["frames"], 1, "{reply}");
        assert_eq!(pngs(&previews), 1);

        // Outside the allowed directories nothing is written, and the app is
        // not even asked (no read request reaches a closed door).
        let outside = dir.path().join("outside");
        let reply = call(
            &server,
            "render_preview",
            json!({"live": true, "frame": 0, "output": outside}),
        );
        assert_eq!(failure(&reply)["id"], "output-not-allowed");
        assert!(!outside.exists());
    }

    #[test]
    fn live_without_an_app_or_with_one_that_hangs_up_has_the_edit_codes() {
        let dir = TempDir::new().expect("tempdir");
        let gone = server(&[]).with_attach(dir.path().join("none.sock"));
        let hung_up = server(&[]).with_attach(fake_app(dir.path(), |_| None));
        for tool in ["inspect_document", "check_warnings"] {
            let reply = call(&gone, tool, json!({"live": true}));
            assert_eq!(failure(&reply)["id"], "app-not-running", "{tool}");
            let reply = call(&hung_up, tool, json!({"live": true}));
            assert_eq!(failure(&reply)["id"], "app-disconnected", "{tool}");
        }
    }

    #[test]
    fn live_excludes_project_and_draft_and_needs_attach() {
        let plain = server(&[]);
        for args in [
            json!({"live": true, "draft": true}),
            json!({"live": true, "project": "/x.ravprj"}),
            json!({}),
        ] {
            let reply = call(&plain, "inspect_document", args.clone());
            assert!(reply["error"].is_object(), "{args}: {reply}");
        }
        // No `--attach`: there is no door to ask.
        let reply = call(&plain, "inspect_document", json!({"live": true}));
        assert_eq!(failure(&reply)["id"], "app-not-running");
    }
}

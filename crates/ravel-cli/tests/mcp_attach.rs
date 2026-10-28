// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ravel-cli mcp --attach`: the `live_edit` tool against a fake app.
//!
//! The app side is a listener on a temporary Unix socket, so this is the
//! bridge's own behaviour (what is offered, what each failure is called), not
//! the app's: the app's half is tested in `ravel-app`.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use ravel_cli::mcp::Server;
use serde_json::{Value, json};

fn rpc(server: &Server, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    server.handle_line(&request.to_string()).expect("answered")
}

fn tool_names(server: &Server) -> Vec<String> {
    rpc(server, "tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn live_edit(server: &Server, ops: Value) -> Value {
    rpc(
        server,
        "tools/call",
        json!({"name": "live_edit", "arguments": {"ops": ops}}),
    )
}

/// An app that answers one line per connection with `reply(request)`, or hangs
/// up without answering when that is `None`.
fn fake_app(dir: &Path, reply: impl Fn(&Value) -> Option<Value> + Send + 'static) -> PathBuf {
    let path = dir.join("live-edit.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            if let Some(answer) = reply(&request) {
                let _ = writeln!(stream, "{answer}");
            }
        }
    });
    path
}

#[test]
fn live_edit_is_offered_only_with_attach() {
    let plain = Server::new(&[]);
    assert!(!tool_names(&plain).contains(&"live_edit".to_string()));
    // Calling a tool that is not offered is not a way to reach the app.
    let reply = live_edit(&plain, json!([]));
    assert!(reply["error"].is_object(), "{reply}");

    let attached = Server::new(&[]).with_attach(PathBuf::from("/nonexistent/live-edit.sock"));
    assert!(tool_names(&attached).contains(&"live_edit".to_string()));
    assert!(tool_names(&attached).contains(&"edit".to_string()));
}

#[test]
fn a_successful_live_edit_returns_what_the_app_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_app(dir.path(), |request| {
        assert_eq!(request["ops"][0]["op"], "add_layer");
        Some(json!({"id": request["id"], "ok": {"created": [{"kind": "composition", "id": 9}]}}))
    });
    let server = Server::new(&[]).with_attach(path);
    let ops = json!([{"op": "add_layer", "comp": 1, "template": "solid"}]);
    let reply = live_edit(&server, ops);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert_eq!(reply["result"]["structuredContent"]["created"][0]["id"], 9);
}

#[test]
fn the_apps_refusal_is_a_tool_error_under_its_own_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_app(dir.path(), |request| {
        Some(json!({"id": request["id"], "error": {
            "code": "confirmation-denied", "index": 0, "message": "declined"}}))
    });
    let server = Server::new(&[]).with_attach(path);
    let reply = live_edit(&server, json!([]));
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(
        reply["result"]["structuredContent"]["id"],
        "confirmation-denied"
    );
    assert_eq!(reply["result"]["structuredContent"]["index"], 0);
}

#[test]
fn an_app_that_quits_mid_call_is_app_disconnected_and_the_next_call_tries_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_app(dir.path(), |_| None);
    let server = Server::new(&[]).with_attach(path.clone());
    let reply = live_edit(&server, json!([]));
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(
        reply["result"]["structuredContent"]["id"],
        "app-disconnected"
    );
    // The app is gone entirely: the next call reconnects, finds nothing.
    std::fs::remove_file(&path).unwrap();
    let reply = live_edit(&server, json!([]));
    assert_eq!(
        reply["result"]["structuredContent"]["id"],
        "app-not-running"
    );
}

#[test]
fn without_an_app_the_other_tools_still_work() {
    let server = Server::new(&[]).with_attach(PathBuf::from("/nonexistent/live-edit.sock"));
    let reply = live_edit(&server, json!([]));
    assert_eq!(
        reply["result"]["structuredContent"]["id"],
        "app-not-running"
    );
    let reply = rpc(
        &server,
        "tools/call",
        json!({"name": "new_project", "arguments": {}}),
    );
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let reply = rpc(
        &server,
        "tools/call",
        json!({"name": "list_node_types", "arguments": {}}),
    );
    assert_eq!(reply["result"]["isError"], false, "{reply}");
}

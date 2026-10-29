// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The client of the app's live-edit door (`ravel-cli mcp --attach`).
//!
//! One call is one connection: connect, send one request line, read one
//! response line. Not keeping the connection means an app that was restarted
//! between two calls needs no recovery logic, and the two failures an agent can
//! act on stay apart:
//!
//! - nothing to connect to: [`CliError::AppNotRunning`], the request was never
//!   sent;
//! - the connection ended after the request went out and before an answer came:
//!   [`CliError::AppDisconnected`], the app may or may not have applied it, so
//!   look at the document before repeating it.
//!
//! A read ([`live_read`]) is the same round trip with a different request and a
//! larger response bound: the answer is the whole document.
//!
//! The wire format is in `crates/ravel-app/src/live_edit/protocol.rs`; this
//! crate cannot depend on the app, so it speaks the same few JSON fields.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use ravel_core::edit::EditOp;
use ravel_project::ProjectFile;
use serde_json::{Value, json};

use crate::error::CliError;

/// The longest response line read, in bytes: a bound on what a misbehaving
/// peer can make this process buffer.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// The bound for a read, whose answer is the whole document as text. A
/// document of a few hundred layers is several MiB of RON, so it gets its own
/// limit instead of raising the one every edit answer is held to. The request
/// side keeps 4 MiB: a read request is a few bytes.
const MAX_READ_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(unix)]
fn connect(endpoint: &Path) -> std::io::Result<impl Read + Write> {
    std::os::unix::net::UnixStream::connect(endpoint)
}

/// A named pipe opens like a file. `ERROR_PIPE_BUSY` is the moment between the
/// app taking one connection and opening the next instance, not an absent app,
/// so it is retried for a short while.
#[cfg(windows)]
fn connect(endpoint: &Path) -> std::io::Result<impl Read + Write> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut attempts = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)
        {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn connect(_: &Path) -> std::io::Result<std::io::Cursor<Vec<u8>>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Send `ops` to the app listening on `endpoint`; the `created` list it
/// answers with.
pub fn live_edit(endpoint: &Path, ops: &[EditOp]) -> Result<Value, CliError> {
    let stream = connect(endpoint).map_err(|error| {
        tracing::debug!(%error, endpoint = %endpoint.display(), "live-edit connect failed");
        CliError::AppNotRunning
    })?;
    exchange(stream, ops)
}

/// A copy of the document open in the app, and where its project lives.
pub struct LiveDocument {
    pub project: ProjectFile,
    /// The directory relative asset paths resolve against; `None` for a
    /// project that was never saved.
    pub root: Option<PathBuf>,
}

/// Ask the app listening on `endpoint` for a copy of its open document. The
/// app changes nothing and records no undo step.
pub fn live_read(endpoint: &Path) -> Result<LiveDocument, CliError> {
    let stream = connect(endpoint).map_err(|error| {
        tracing::debug!(%error, endpoint = %endpoint.display(), "live read connect failed");
        CliError::AppNotRunning
    })?;
    read_exchange(stream)
}

fn read_exchange(stream: impl Read + Write) -> Result<LiveDocument, CliError> {
    let ok = round_trip(
        stream,
        &json!({ "id": 1, "read": "document" }),
        MAX_READ_RESPONSE_BYTES,
    )?;
    let text = ok["document"]
        .as_str()
        .ok_or_else(|| CliError::Internal("the app's read reply has no `document`".into()))?;
    let document = ravel_project::document_from_ron(text)
        .map_err(|e| CliError::Internal(format!("the app's document did not parse: {e}")))?;
    let path = ok["project_path"].as_str().map(PathBuf::from);
    let root = path.as_deref().and_then(ravel_project::project_root_of);
    // What a load does to a file: references measured against the project.
    let document = document.with_resolved_assets(root.as_deref(), &Default::default());
    Ok(LiveDocument {
        project: ProjectFile::from_document(
            "live",
            ravel_project::timestamp::rfc3339_now(),
            document,
        ),
        root,
    })
}

/// The request / response round trip of an edit on an open connection.
fn exchange(stream: impl Read + Write, ops: &[EditOp]) -> Result<Value, CliError> {
    round_trip(stream, &json!({ "id": 1, "ops": ops }), MAX_RESPONSE_BYTES)
}

/// One request line out, one response line in: the `ok` payload, or the
/// app's refusal.
fn round_trip(
    mut stream: impl Read + Write,
    request: &Value,
    max_response: u64,
) -> Result<Value, CliError> {
    writeln!(stream, "{request}")
        .and_then(|()| stream.flush())
        .map_err(|_| CliError::AppDisconnected)?;
    let mut line = String::new();
    match BufReader::new(stream.take(max_response)).read_line(&mut line) {
        Ok(n) if n > 0 && line.ends_with('\n') => {}
        // The bound was hit before the end of the line: not a disconnect.
        Ok(n) if n as u64 >= max_response => {
            return Err(CliError::Internal(format!(
                "the app's reply is longer than {} MiB",
                max_response / (1024 * 1024)
            )));
        }
        _ => return Err(CliError::AppDisconnected),
    }
    let mut response: Value = serde_json::from_str(&line)
        .map_err(|e| CliError::Internal(format!("the app's reply is not JSON: {e}")))?;
    if let Some(ok) = response.get_mut("ok") {
        return Ok(ok.take());
    }
    let error = response
        .get("error")
        .ok_or_else(|| CliError::Internal("the app's reply has neither `ok` nor `error`".into()))?;
    Err(CliError::LiveEdit {
        code: error["code"].as_str().unwrap_or("unknown").to_string(),
        index: error["index"]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok()),
        message: error["message"].as_str().unwrap_or_default().to_string(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A fake app: answers each connection with `answer(request line)`, or
    /// closes it without a word when that is `None`.
    fn fake_app(
        dir: &Path,
        answer: impl Fn(&str) -> Option<String> + Send + 'static,
    ) -> std::path::PathBuf {
        let path = dir.join("live-edit.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                if let Some(reply) = answer(&line) {
                    let _ = writeln!(stream, "{reply}");
                }
            }
        });
        path
    }

    #[test]
    fn an_ok_answer_is_the_created_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = fake_app(dir.path(), |request| {
            let request: Value = serde_json::from_str(request).unwrap();
            assert_eq!(request["ops"], json!([]));
            Some(json!({"id": 1, "ok": {"created": [{"kind": "layer", "id": 4}]}}).to_string())
        });
        let created = live_edit(&path, &[]).unwrap();
        assert_eq!(created["created"][0]["id"], 4);
    }

    #[test]
    fn a_refusal_keeps_the_apps_code_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = fake_app(dir.path(), |_| {
            Some(
                json!({"id": 1, "error": {"code": "confirmation-denied", "index": 2, "message": "no"}})
                    .to_string(),
            )
        });
        match live_edit(&path, &[]) {
            Err(CliError::LiveEdit {
                code,
                index,
                message,
            }) => {
                assert_eq!(
                    (code.as_str(), index, message.as_str()),
                    ("confirmation-denied", Some(2), "no")
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_connection_closed_before_the_answer_is_app_disconnected() {
        let dir = tempfile::tempdir().unwrap();
        let path = fake_app(dir.path(), |_| None);
        let error = live_edit(&path, &[]).unwrap_err();
        assert_eq!(error.id(), "app-disconnected");
        // A half line is no answer either.
        assert!(matches!(
            exchange(std::io::Cursor::new(b"{\"id\":1".to_vec()), &[]),
            Err(CliError::AppDisconnected)
        ));
    }

    /// What the app said, and a sink for what was sent.
    struct Duplex(std::io::Cursor<Vec<u8>>);
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A document answer past the edit limit still arrives, and the same line
    /// is refused for an edit, as what it is rather than as a disconnect.
    #[test]
    fn a_read_answer_may_exceed_the_edit_limit() {
        let big = "x".repeat(5 * 1024 * 1024);
        let line = format!("{}\n", json!({"id": 1, "ok": {"document": big}}));
        let answer = || Duplex(std::io::Cursor::new(line.clone().into_bytes()));
        let ok = round_trip(answer(), &json!({}), MAX_READ_RESPONSE_BYTES).unwrap();
        assert_eq!(ok["document"].as_str().unwrap().len(), big.len());
        match round_trip(answer(), &json!({}), MAX_RESPONSE_BYTES) {
            Err(CliError::Internal(message)) => assert!(message.contains("4 MiB"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_socket_is_app_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let error = live_edit(&dir.path().join("none.sock"), &[]).unwrap_err();
        assert_eq!(error.id(), "app-not-running");
    }
}

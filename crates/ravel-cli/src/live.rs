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
//! The wire format is in `crates/ravel-app/src/live_edit/protocol.rs`; this
//! crate cannot depend on the app, so it speaks the same few JSON fields.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use ravel_core::edit::EditOp;
use serde_json::{Value, json};

use crate::error::CliError;

/// The longest response line read, in bytes: a bound on what a misbehaving
/// peer can make this process buffer.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

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

/// The request / response round trip on an open connection.
fn exchange(mut stream: impl Read + Write, ops: &[EditOp]) -> Result<Value, CliError> {
    let request = json!({ "id": 1, "ops": ops });
    writeln!(stream, "{request}")
        .and_then(|()| stream.flush())
        .map_err(|_| CliError::AppDisconnected)?;
    let mut line = String::new();
    match BufReader::new(stream.take(MAX_RESPONSE_BYTES)).read_line(&mut line) {
        Ok(n) if n > 0 && line.ends_with('\n') => {}
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

    #[test]
    fn no_socket_is_app_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let error = live_edit(&dir.path().join("none.sock"), &[]).unwrap_err();
        assert_eq!(error.id(), "app-not-running");
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Unix-domain-socket transport (macOS / Linux).

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::{JobSender, serve_connection};

/// Bind the socket and start accepting. The path it listens on, or `None`
/// (logged) when it could not be opened or another instance owns it.
pub fn open(endpoint: Option<PathBuf>, tx: JobSender) -> Option<PathBuf> {
    let path = endpoint.or_else(ravel_project::paths::live_edit_endpoint)?;
    match bind(&path) {
        Ok(Some(listener)) => {
            std::thread::Builder::new()
                .name("live-edit-accept".into())
                .spawn(move || accept_loop(listener, tx))
                .ok()?;
            Some(path)
        }
        Ok(None) => {
            tracing::warn!(path = %path.display(), "the live-edit socket is in use by another instance");
            None
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not open the live-edit socket");
            None
        }
    }
}

/// `Ok(None)` when a live instance already listens there.
fn bind(path: &Path) -> std::io::Result<Option<UnixListener>> {
    let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    // An existing directory keeps its mode from `create`; make it ours alone.
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Ok(None);
        }
        // Nobody answers: a leftover from a crashed instance.
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    // The 0700 parent keeps others out between bind and this call.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

fn accept_loop(listener: UnixListener, tx: JobSender) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("live-edit-conn".into())
            .spawn(move || serve_connection(stream, tx));
    }
}

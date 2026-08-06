// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Crash-safe file replacement: write a temporary file beside the
//! destination, sync it, then swap the name.
//!
//! Extracted from the `.ravprj` writer (`CRIT-03`) so every file Ravel
//! rewrites in place can share it — the project archive
//! ([`super::container::write_file`]) and the global settings layer
//! (`ravel-app`'s `app_settings`). The Windows replacement primitive in
//! particular must exist exactly once: `fs::rename` cannot replace an
//! existing destination there, so a second copy of this logic would be a
//! second chance to get that wrong.
//!
//! The invariant is that an interrupted write leaves the previous file
//! intact. Nothing here appends, truncates, or opens the destination.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `bytes` to `path`, replacing an existing file atomically.
///
/// The destination directory is created when missing.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_with(path, bytes, |temporary, destination| {
        temporary.persist(destination)
    })
}

/// [`write`] with the publication step injected, so a test can fail between
/// the synced temporary file and the name swap.
pub fn write_with(
    path: &Path,
    bytes: &[u8],
    publish: impl FnOnce(TemporaryFile, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let mut temporary = TemporaryFile::new(parent)?;
    temporary.file_mut().write_all(bytes)?;
    temporary.file_mut().flush()?;
    temporary.file().sync_all()?;
    publish(temporary, path)?;

    // Persist the directory entry as well as the file contents where the
    // platform supports opening directories. Windows' replacement primitive
    // already provides the required atomic name swap but directories cannot
    // be opened through std::fs::File there.
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

static TEMP_FILE_SERIAL: AtomicU64 = AtomicU64::new(0);

/// A uniquely named scratch file in the destination directory. Dropping it
/// without publishing removes it, so a failed write leaves no debris.
pub struct TemporaryFile {
    path: PathBuf,
    file: Option<fs::File>,
}

impl TemporaryFile {
    fn new(directory: &Path) -> std::io::Result<Self> {
        loop {
            let serial = TEMP_FILE_SERIAL.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".ravel-save-{}-{serial}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn file(&self) -> &fs::File {
        self.file.as_ref().expect("temporary file is open")
    }

    fn file_mut(&mut self) -> &mut fs::File {
        self.file.as_mut().expect("temporary file is open")
    }

    /// Publish the synced temporary file under `destination`. This is what
    /// [`write`] hands to [`write_with`]; a caller that injects its own
    /// publication step calls it to succeed.
    pub fn persist(mut self, destination: &Path) -> std::io::Result<()> {
        // Windows cannot replace a destination while our std File handle is
        // open. The bytes were synced by the caller before this close.
        self.file.take();
        replace_file(&self.path, destination)?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        self.file.take();
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both pointers refer to live, NUL-terminated UTF-16 buffers for
    // the duration of the call. Flags request an atomic same-volume replace
    // and synchronous metadata publication; no handles escape this boundary.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");
        write(&path, b"locale = \"ja\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "locale = \"ja\"\n");
    }

    #[test]
    fn a_write_replaces_the_previous_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write(&path, b"first").unwrap();
        write(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }

    /// The reason this indirection exists: a failure before publication must
    /// leave the previous file exactly as it was, and leave no scratch file
    /// behind either.
    #[test]
    fn a_failure_before_publication_leaves_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write(&path, b"first").unwrap();

        let error = write_with(&path, b"second", |temporary, _destination| {
            assert!(temporary.file().metadata()?.len() > 0);
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "simulated interruption before atomic replace",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "settings.toml")
            .collect();
        assert!(leftovers.is_empty(), "scratch files: {leftovers:?}");
    }
}

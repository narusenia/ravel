// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Zip-container read/write for `.ravprj` files.
//!
//! A `.ravprj` is a zip archive with a fixed internal layout:
//!
//! ```text
//! manifest.json      top-level metadata + format version
//! document/main.ron  the whole Document (RON, format v3+; current v4)
//! graph/main.ron     legacy flat node graph (RON, format v1–v2 only)
//! assets/refs.json   legacy empty asset table (read only; removed in v4)
//! settings.toml           project-level settings layer
//! ui_state.json           optional persisted UI state
//! workspace_layout.toml   optional workspace layout the project opts into
//! ```
//!
//! This module deals only in **raw bytes** keyed by entry name
//! ([`RawArchive`]); higher-level parsing lives in
//! the [crate root](crate). Reading is intentionally defensive — a truncated
//! or non-zip input yields a [`ContainerError`] rather than a panic, satisfying
//! the robustness requirement for corrupt files and fuzz input.
//!
//! Writing performs an automatic backup: if the destination already exists it
//! is copied to `<path>.bak` before the new archive is written, so the previous
//! revision is always recoverable.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::atomic_write::{self, TemporaryFile};

/// Canonical archive entry names.
pub mod entry {
    pub const MANIFEST: &str = "manifest.json";
    /// The whole [`ravel_core::composition::Document`] as RON (format v3+).
    pub const DOCUMENT: &str = "document/main.ron";
    /// Legacy flat graph (format v1–v2); read only, never written by v3+.
    pub const GRAPH: &str = "graph/main.ron";
    /// Legacy empty asset table; read only and ignored by format v4.
    pub const ASSETS: &str = "assets/refs.json";
    pub const SETTINGS: &str = "settings.toml";
    /// Persisted UI state (REQ-UI-013). Optional in every format version —
    /// see [`crate::ui_state`].
    pub const UI_STATE: &str = "ui_state.json";
    /// The workspace layout a project opts into shipping. Optional in every
    /// format version, and written only when the user turned the opt-in on —
    /// see `ravel-app`'s `layout_persist`. Deliberately separate from
    /// [`UI_STATE`]: what the user was looking at travels with every project,
    /// while how their screen was arranged is a choice.
    pub const WORKSPACE_LAYOUT: &str = "workspace_layout.toml";
}

/// Suffix appended to create the automatic backup file.
pub const BACKUP_SUFFIX: &str = ".bak";

/// Errors raised while reading or writing a `.ravprj` container.
#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("zip container error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("required entry `{0}` is missing from the archive")]
    MissingEntry(&'static str),

    #[error("entry `{name}` is not valid UTF-8")]
    NotUtf8 { name: String },
}

/// In-memory view of a `.ravprj` archive: entry name → raw bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawArchive {
    entries: BTreeMap<String, Vec<u8>>,
}

impl RawArchive {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an entry.
    pub fn insert(&mut self, name: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.entries.insert(name.into(), bytes.into());
    }

    /// Borrow an entry's raw bytes.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(|v| v.as_slice())
    }

    /// Fetch a required entry's bytes or fail with [`ContainerError::MissingEntry`].
    pub fn require(&self, name: &'static str) -> Result<&[u8], ContainerError> {
        self.get(name).ok_or(ContainerError::MissingEntry(name))
    }

    /// Fetch a required entry decoded as UTF-8 text.
    pub fn require_text(&self, name: &'static str) -> Result<&str, ContainerError> {
        let bytes = self.require(name)?;
        std::str::from_utf8(bytes).map_err(|_| ContainerError::NotUtf8 {
            name: name.to_string(),
        })
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialize all entries into an in-memory zip byte buffer.
    pub fn to_zip_bytes(&self) -> Result<Vec<u8>, ContainerError> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in &self.entries {
            writer.start_file(name.as_str(), options)?;
            writer.write_all(bytes)?;
        }
        let cursor = writer.finish()?;
        Ok(cursor.into_inner())
    }

    /// Parse a zip archive from any seekable reader into a [`RawArchive`].
    ///
    /// Defensive: malformed input returns [`ContainerError::Zip`]; it never
    /// panics. Used directly by the fuzz tests.
    pub fn from_reader<R: Read + Seek>(reader: R) -> Result<Self, ContainerError> {
        let mut archive = ZipArchive::new(reader)?;
        let mut entries = BTreeMap::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            // Skip directory entries.
            if file.name().ends_with('/') {
                continue;
            }
            let name = file.name().to_string();
            const MAX_PRE_ALLOC: usize = 64 * 1024 * 1024; // 64 MiB
            let hint = (file.size() as usize).min(MAX_PRE_ALLOC);
            let mut buf = Vec::with_capacity(hint);
            file.read_to_end(&mut buf)?;
            entries.insert(name, buf);
        }
        Ok(Self { entries })
    }

    /// Parse a zip archive from a raw byte slice.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ContainerError> {
        Self::from_reader(Cursor::new(bytes))
    }
}

/// Compute the backup path for a given project path (`foo.ravprj` →
/// `foo.ravprj.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

/// Read a `.ravprj` file from disk into a [`RawArchive`].
pub fn read_file(path: &Path) -> Result<RawArchive, ContainerError> {
    let file = fs::File::open(path)?;
    RawArchive::from_reader(file)
}

/// Write a [`RawArchive`] to disk as a `.ravprj` file.
///
/// If `path` already exists, the current file is first copied to
/// [`backup_path`] so the previous revision is retained. The new archive is
/// written and synced in the destination directory before an atomic replace,
/// so a crash before publication cannot truncate the current project.
pub fn write_file(path: &Path, archive: &RawArchive) -> Result<(), ContainerError> {
    write_file_with(path, archive, |temporary, destination| {
        temporary.persist(destination)
    })
}

fn write_file_with(
    path: &Path,
    archive: &RawArchive,
    publish: impl FnOnce(TemporaryFile, &Path) -> std::io::Result<()>,
) -> Result<(), ContainerError> {
    if path.exists() {
        let backup = backup_path(path);
        fs::copy(path, &backup)?;
    }
    let bytes = archive.to_zip_bytes()?;
    atomic_write::write_with(path, &bytes, publish)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_archive() -> RawArchive {
        let mut a = RawArchive::new();
        a.insert(entry::MANIFEST, br#"{"format_version":2}"#.to_vec());
        a.insert(entry::GRAPH, b"GraphDoc(nodes:[],edges:[])".to_vec());
        a.insert(entry::SETTINGS, b"[color]\n".to_vec());
        a
    }

    #[test]
    fn zip_bytes_roundtrip() {
        let archive = sample_archive();
        let bytes = archive.to_zip_bytes().unwrap();
        let back = RawArchive::from_bytes(&bytes).unwrap();
        assert_eq!(archive, back);
        assert_eq!(
            back.require_text(entry::MANIFEST).unwrap(),
            r#"{"format_version":2}"#
        );
    }

    #[test]
    fn missing_entry_reports_name() {
        let archive = RawArchive::new();
        let err = archive.require(entry::MANIFEST).unwrap_err();
        assert!(matches!(err, ContainerError::MissingEntry(entry::MANIFEST)));
    }

    #[test]
    fn corrupt_bytes_error_not_panic() {
        assert!(RawArchive::from_bytes(b"not a zip file at all").is_err());
        assert!(RawArchive::from_bytes(&[]).is_err());
        // A truncated local-file-header signature.
        assert!(RawArchive::from_bytes(&[0x50, 0x4b, 0x03, 0x04, 0x00]).is_err());
    }

    #[test]
    fn backup_path_appends_suffix() {
        let p = Path::new("/tmp/demo.ravprj");
        assert_eq!(backup_path(p), PathBuf::from("/tmp/demo.ravprj.bak"));
    }

    #[test]
    fn write_then_read_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ravel_container_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("proj.ravprj");
        let _ = fs::remove_file(&path);

        let archive = sample_archive();
        write_file(&path, &archive).unwrap();
        assert!(path.exists());
        let back = read_file(&path).unwrap();
        assert_eq!(archive, back);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(backup_path(&path));
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn second_write_creates_backup() {
        let dir = std::env::temp_dir().join(format!("ravel_backup_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("proj.ravprj");
        let backup = backup_path(&path);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);

        // First write: no backup yet.
        write_file(&path, &sample_archive()).unwrap();
        assert!(!backup.exists());

        // Second write: previous revision preserved as .bak.
        let mut updated = sample_archive();
        updated.insert(entry::ASSETS, b"{\"assets\":[]}".to_vec());
        write_file(&path, &updated).unwrap();
        assert!(backup.exists());

        // The backup holds the *previous* (first) archive.
        let restored = read_file(&backup).unwrap();
        assert_eq!(restored, sample_archive());

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn publish_failure_leaves_existing_project_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.ravprj");
        let original = sample_archive();
        write_file(&path, &original).unwrap();

        let mut updated = sample_archive();
        updated.insert(entry::ASSETS, b"{\"assets\":[\"new\"]}".to_vec());
        let error = write_file_with(&path, &updated, |temporary, _destination| {
            assert!(temporary.file().metadata()?.len() > 0);
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "simulated interruption before atomic replace",
            ))
        })
        .unwrap_err();
        assert!(
            matches!(error, ContainerError::Io(ref io) if io.kind() == std::io::ErrorKind::Interrupted)
        );

        assert_eq!(read_file(&path).unwrap(), original);
        assert_eq!(read_file(&backup_path(&path)).unwrap(), original);
    }
}

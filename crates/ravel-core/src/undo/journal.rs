// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Append-only operation journal for crash recovery.
//!
//! Each [`JournalEntry`] records a mutation with a sequence number and
//! timestamp. The journal is written to a file in a length-prefixed binary
//! format.
//!
//! # File format
//!
//! Files start with an 8-byte header: a 4-byte magic (`RVLJ`) followed by a
//! u32 little-endian format version ([`JOURNAL_FORMAT_VERSION`]). Entries
//! (`[u32 LE length][payload bytes]`) follow the header. Files without a
//! header (written before versioning was introduced) count as version 0.
//!
//! The journal is volatile crash-recovery state, not long-term storage.
//! When a file's version does not match (old format, future version, or
//! corrupt), [`JournalReader::read_all`] reports
//! [`JournalError::UnsupportedVersion`] once and returns no entries so
//! recovery can continue from the base graph, and [`JournalWriter::open`]
//! discards the file and starts a fresh journal — mixing entries of
//! different bincode layouts in one file would be worse than losing them.
//!
//! Two [`JournalCodec`] implementations are provided:
//! - [`BincodeCodec`] — fast and compact (default)
//! - [`RonCodec`] — human-readable, useful for debugging

use super::mutation::GraphMutation;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ---------------------------------------------------------------------------
// File format
// ---------------------------------------------------------------------------

/// Current journal format version. Files without a header (written before
/// versioning was introduced) count as version 0 and are discarded on open.
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

/// Magic bytes at the start of every journal file.
const JOURNAL_MAGIC: [u8; 4] = *b"RVLJ";

/// Length of the journal file header: 4-byte magic + u32 LE version.
const JOURNAL_HEADER_LEN: usize = 8;

/// The 8-byte journal file header: magic + u32 LE [`JOURNAL_FORMAT_VERSION`].
fn journal_header() -> [u8; JOURNAL_HEADER_LEN] {
    let mut header = [0u8; JOURNAL_HEADER_LEN];
    header[..4].copy_from_slice(&JOURNAL_MAGIC);
    header[4..].copy_from_slice(&JOURNAL_FORMAT_VERSION.to_le_bytes());
    header
}

/// Extract the format version from a journal header, or `None` if the magic
/// does not match (pre-version or corrupt file).
fn probe_journal_version(header: &[u8; JOURNAL_HEADER_LEN]) -> Option<u32> {
    if header[..4] == JOURNAL_MAGIC {
        Some(u32::from_le_bytes(
            header[4..].try_into().expect("header tail is 4 bytes"),
        ))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Journal entry
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub sequence: u64,
    pub timestamp_secs: u64,
    pub mutation: GraphMutation,
}

// ---------------------------------------------------------------------------
// Codec trait
// ---------------------------------------------------------------------------

/// Serialization strategy for journal entries.
pub trait JournalCodec: Send + Sync {
    fn encode(&self, entry: &JournalEntry) -> Result<Vec<u8>, JournalError>;
    fn decode(&self, data: &[u8]) -> Result<JournalEntry, JournalError>;
}

/// bincode — fast, compact, default.
pub struct BincodeCodec;

impl JournalCodec for BincodeCodec {
    fn encode(&self, entry: &JournalEntry) -> Result<Vec<u8>, JournalError> {
        bincode::serialize(entry).map_err(|e| JournalError::Codec(e.to_string()))
    }

    fn decode(&self, data: &[u8]) -> Result<JournalEntry, JournalError> {
        bincode::deserialize(data).map_err(|e| JournalError::Codec(e.to_string()))
    }
}

/// RON — human-readable, good for debugging.
pub struct RonCodec;

impl JournalCodec for RonCodec {
    fn encode(&self, entry: &JournalEntry) -> Result<Vec<u8>, JournalError> {
        ron::ser::to_string(entry)
            .map(|s| s.into_bytes())
            .map_err(|e| JournalError::Codec(e.to_string()))
    }

    fn decode(&self, data: &[u8]) -> Result<JournalEntry, JournalError> {
        let s = std::str::from_utf8(data).map_err(|e| JournalError::Codec(e.to_string()))?;
        ron::from_str(s).map_err(|e| JournalError::Codec(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("corrupt entry at sequence {sequence}: {reason}")]
    CorruptEntry { sequence: u64, reason: String },

    #[error("unsupported journal format version: {found}")]
    UnsupportedVersion { found: u32 },
}

// ---------------------------------------------------------------------------
// Journal writer
// ---------------------------------------------------------------------------

/// Append-only journal file writer.
///
/// Entries are length-prefixed: `[u32 LE length][payload bytes]`.
pub struct JournalWriter {
    writer: BufWriter<File>,
    codec: Box<dyn JournalCodec>,
    next_sequence: u64,
    path: PathBuf,
}

impl JournalWriter {
    /// Open (or create) a journal file for appending.
    ///
    /// New or empty files start with the format header. If the file already
    /// contains entries in the current format, the sequence counter is
    /// auto-detected by scanning them so that new appends continue with the
    /// correct sequence number. A file in any other format (pre-versioning,
    /// future version, or corrupt) is discarded — the crash journal is
    /// volatile, and mixing bincode layouts would risk misreading entries —
    /// and reopened with a fresh header.
    pub fn open(
        path: impl Into<PathBuf>,
        codec: Box<dyn JournalCodec>,
    ) -> Result<Self, JournalError> {
        let path = path.into();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Truncation is decided below, after inspecting the header.
            .truncate(false)
            .open(&path)?;
        let mut next_sequence = 0;

        if file.metadata()?.len() == 0 {
            // Fresh journal: start with just the format header.
            file.write_all(&journal_header())?;
        } else {
            let mut header = [0u8; JOURNAL_HEADER_LEN];
            let version = match file.read_exact(&mut header) {
                Ok(()) => probe_journal_version(&header),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
                Err(e) => return Err(e.into()),
            };
            match version {
                Some(v) if v == JOURNAL_FORMAT_VERSION => {
                    // Detect next sequence from existing entries. A corrupt
                    // tail (truncated length/payload) would silently orphan
                    // anything appended after it — recovery stops at the
                    // damage — so the journal is discarded instead, like an
                    // incompatible one.
                    let reader = JournalReader::new(Box::new(BincodeCodec));
                    let mut corrupt = false;
                    let entries = reader.read_all(&path, |_| corrupt = true)?;
                    if corrupt {
                        tracing::warn!(
                            path = %path.display(),
                            "discarding journal with a corrupt tail"
                        );
                        file.set_len(0)?;
                        file.seek(SeekFrom::Start(0))?;
                        file.write_all(&journal_header())?;
                    } else {
                        next_sequence = entries.last().map_or(0, |e| e.sequence + 1);
                    }
                }
                found => {
                    tracing::warn!(
                        path = %path.display(),
                        found = found.unwrap_or(0),
                        supported = JOURNAL_FORMAT_VERSION,
                        "discarding incompatible journal file"
                    );
                    file.set_len(0)?;
                    file.seek(SeekFrom::Start(0))?;
                    file.write_all(&journal_header())?;
                }
            }
        }

        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            writer: BufWriter::new(file),
            codec,
            next_sequence,
            path,
        })
    }

    /// Append a mutation to the journal.
    pub fn append(
        &mut self,
        mutation: GraphMutation,
        timestamp_secs: u64,
    ) -> Result<u64, JournalError> {
        let seq = self.next_sequence;
        let entry = JournalEntry {
            sequence: seq,
            timestamp_secs,
            mutation,
        };
        let payload = self.codec.encode(&entry)?;
        let len = payload.len() as u32;
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.next_sequence += 1;
        Ok(seq)
    }

    /// Current next sequence number.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Path to the journal file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Journal reader
// ---------------------------------------------------------------------------

/// Reads journal entries sequentially, skipping corrupt entries.
pub struct JournalReader {
    codec: Box<dyn JournalCodec>,
}

impl JournalReader {
    pub fn new(codec: Box<dyn JournalCodec>) -> Self {
        Self { codec }
    }

    /// Read all entries from a journal file.
    ///
    /// Corrupt entries are skipped with a warning via `on_error`. Returns
    /// successfully decoded entries in order. A file in an unsupported
    /// format (pre-versioning, future version, or corrupt header) reports
    /// [`JournalError::UnsupportedVersion`] once via `on_error` and yields
    /// no entries, so recovery can continue from the base graph.
    pub fn read_all(
        &self,
        path: &Path,
        mut on_error: impl FnMut(JournalError),
    ) -> Result<Vec<JournalEntry>, JournalError> {
        let file = File::open(path)?;
        if file.metadata()?.len() == 0 {
            // A fresh journal: no header, no entries.
            return Ok(Vec::new());
        }
        let mut reader = BufReader::new(file);

        // Format header: magic + u32 LE version (see module docs).
        let mut header = [0u8; JOURNAL_HEADER_LEN];
        let version = match reader.read_exact(&mut header) {
            Ok(()) => probe_journal_version(&header),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
            Err(e) => return Err(e.into()),
        };
        match version {
            Some(v) if v == JOURNAL_FORMAT_VERSION => {}
            found => {
                on_error(JournalError::UnsupportedVersion {
                    found: found.unwrap_or(0),
                });
                return Ok(Vec::new());
            }
        }

        let mut entries = Vec::new();

        loop {
            // Length prefix, read byte-exactly so a partial 1–3 byte tail is
            // told apart from a clean EOF.
            let mut len_buf = [0u8; 4];
            let mut filled = 0;
            while filled < len_buf.len() {
                match reader.read(&mut len_buf[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => return Err(e.into()),
                }
            }
            if filled == 0 {
                break; // clean end of journal
            }
            if filled < len_buf.len() {
                on_error(JournalError::CorruptEntry {
                    sequence: entries.len() as u64,
                    reason: format!("truncated length prefix ({filled} of 4 bytes)"),
                });
                break;
            }
            let len = u32::from_le_bytes(len_buf) as usize;

            if len > 64 * 1024 * 1024 {
                on_error(JournalError::CorruptEntry {
                    sequence: entries.len() as u64,
                    reason: format!("entry length {len} exceeds 64 MiB limit"),
                });
                break;
            }

            let mut payload = vec![0u8; len];
            match reader.read_exact(&mut payload) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    on_error(JournalError::CorruptEntry {
                        sequence: entries.len() as u64,
                        reason: "truncated payload".into(),
                    });
                    break;
                }
                Err(e) => return Err(e.into()),
            }

            match self.codec.decode(&payload) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    on_error(JournalError::CorruptEntry {
                        sequence: entries.len() as u64,
                        reason: e.to_string(),
                    });
                }
            }
        }

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Compaction
// ---------------------------------------------------------------------------

/// Delete the journal file. Called after a successful graph save.
pub fn compact(journal_path: &Path) -> Result<(), JournalError> {
    match std::fs::remove_file(journal_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::id::{DataTypeId, NodeId};
    use std::io::Write;

    fn sample_mutation() -> GraphMutation {
        GraphMutation::AddNode(
            Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR),
        )
    }

    fn roundtrip_codec(codec: &dyn JournalCodec) {
        let entry = JournalEntry {
            sequence: 42,
            timestamp_secs: 1700000000,
            mutation: sample_mutation(),
        };
        let data = codec.encode(&entry).unwrap();
        let decoded = codec.decode(&data).unwrap();
        assert_eq!(decoded.sequence, 42);
        assert_eq!(decoded.timestamp_secs, 1700000000);
    }

    #[test]
    fn bincode_roundtrip() {
        roundtrip_codec(&BincodeCodec);
    }

    #[test]
    fn ron_roundtrip() {
        roundtrip_codec(&RonCodec);
    }

    #[test]
    fn write_and_read_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.journal");

        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            writer.append(sample_mutation(), 1000).unwrap();
            writer
                .append(GraphMutation::RemoveNode(NodeId::new(1)), 1001)
                .unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let entries = reader
            .read_all(&path, |e| panic!("unexpected error: {e}"))
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sequence, 0);
        assert_eq!(entries[1].sequence, 1);
    }

    #[test]
    fn writer_reopen_continues_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reopen.journal");

        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            writer.append(sample_mutation(), 1000).unwrap();
        }
        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            assert_eq!(writer.next_sequence(), 1);
            writer.append(sample_mutation(), 1001).unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let entries = reader
            .read_all(&path, |e| panic!("unexpected error: {e}"))
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].sequence, 1);
        assert_eq!(entries[1].timestamp_secs, 1001);
    }

    /// Write a journal file in the pre-versioning format: raw length-prefixed
    /// bincode entries with no header.
    fn write_legacy_journal(path: &Path) {
        let entry = JournalEntry {
            sequence: 0,
            timestamp_secs: 1000,
            mutation: sample_mutation(),
        };
        let payload = BincodeCodec.encode(&entry).unwrap();
        let mut file = File::create(path).unwrap();
        file.write_all(&(payload.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(&payload).unwrap();
    }

    #[test]
    fn read_legacy_headerless_journal_returns_empty_with_version_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.journal");
        write_legacy_journal(&path);

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let mut errors = Vec::new();
        let entries = reader.read_all(&path, |e| errors.push(e)).unwrap();
        assert!(entries.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            JournalError::UnsupportedVersion { found: 0 }
        ));
    }

    #[test]
    fn writer_discards_legacy_journal_and_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.journal");
        write_legacy_journal(&path);

        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            assert_eq!(writer.next_sequence(), 0);
            writer.append(sample_mutation(), 2000).unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let entries = reader
            .read_all(&path, |e| panic!("unexpected error: {e}"))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sequence, 0);
        assert_eq!(entries[0].timestamp_secs, 2000);
    }

    #[test]
    fn future_version_journal_is_rejected_then_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.journal");

        let mut header = [0u8; JOURNAL_HEADER_LEN];
        header[..4].copy_from_slice(b"RVLJ");
        header[4..].copy_from_slice(&(JOURNAL_FORMAT_VERSION + 1).to_le_bytes());
        std::fs::write(&path, header).unwrap();

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let mut errors = Vec::new();
        let entries = reader.read_all(&path, |e| errors.push(e)).unwrap();
        assert!(entries.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            JournalError::UnsupportedVersion { found }
                if found == JOURNAL_FORMAT_VERSION + 1
        ));

        // Opening for write discards the file and starts a fresh journal.
        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            assert_eq!(writer.next_sequence(), 0);
            writer.append(sample_mutation(), 3000).unwrap();
        }
        let entries = reader
            .read_all(&path, |e| panic!("unexpected error: {e}"))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp_secs, 3000);
    }

    #[test]
    fn read_skips_corrupt_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.journal");

        // Write a valid entry, then garbage
        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            writer.append(sample_mutation(), 1000).unwrap();

            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            // Write a valid length but corrupt payload
            file.write_all(&8u32.to_le_bytes()).unwrap();
            file.write_all(b"GARBAGE!").unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let mut errors = Vec::new();
        let entries = reader.read_all(&path, |e| errors.push(e)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(errors.len(), 1);
    }

    /// A journal with a corrupt tail is discarded on open: appending after
    /// the damage would orphan the new entries (recovery stops reading at
    /// the corruption).
    #[test]
    fn writer_discards_journal_with_corrupt_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt_tail.journal");

        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            writer.append(sample_mutation(), 1000).unwrap();

            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&8u32.to_le_bytes()).unwrap();
            file.write_all(b"GARBAGE!").unwrap();
        }

        {
            let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            assert_eq!(writer.next_sequence(), 0, "restarted from scratch");
            writer.append(sample_mutation(), 4000).unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let entries = reader
            .read_all(&path, |e| panic!("unexpected error: {e}"))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp_secs, 4000);
    }

    /// A trailing partial length prefix (1–3 bytes) is corruption, not a
    /// clean EOF — the writer would otherwise append after it and orphan the
    /// new entries.
    #[test]
    fn partial_length_prefix_tail_is_corruption() {
        let dir = tempfile::tempdir().unwrap();
        for trailing in [
            b"\x01".as_slice(),
            b"\x01\x02".as_slice(),
            b"\x01\x02\x03".as_slice(),
        ] {
            let path = dir
                .path()
                .join(format!("partial_{}.journal", trailing.len()));
            {
                let mut writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
                writer.append(sample_mutation(), 1000).unwrap();
                let mut file = OpenOptions::new().append(true).open(&path).unwrap();
                file.write_all(trailing).unwrap();
            }

            let reader = JournalReader::new(Box::new(BincodeCodec));
            let mut errors = Vec::new();
            let entries = reader.read_all(&path, |e| errors.push(e)).unwrap();
            assert_eq!(entries.len(), 1, "valid prefix entries still read");
            assert_eq!(errors.len(), 1, "partial tail reported ({trailing:?})");
            assert!(matches!(errors[0], JournalError::CorruptEntry { .. }));

            // The writer discards the damaged journal and starts fresh.
            let writer = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            assert_eq!(writer.next_sequence(), 0);
        }
    }

    #[test]
    fn compact_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("to_compact.journal");
        std::fs::write(&path, b"data").unwrap();
        compact(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn compact_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.journal");
        compact(&path).unwrap();
    }
}

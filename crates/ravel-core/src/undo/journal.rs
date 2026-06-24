// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Append-only operation journal for crash recovery.
//!
//! Each [`JournalEntry`] records a mutation with a sequence number and
//! timestamp. The journal is written to a file in a length-prefixed binary
//! format.
//!
//! Two [`JournalCodec`] implementations are provided:
//! - [`BincodeCodec`] — fast and compact (default)
//! - [`RonCodec`] — human-readable, useful for debugging

use super::mutation::GraphMutation;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

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
        let s = std::str::from_utf8(data)
            .map_err(|e| JournalError::Codec(e.to_string()))?;
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
    /// If the file already contains entries, the sequence counter is
    /// auto-detected by scanning existing entries so that new appends
    /// continue with the correct sequence number.
    pub fn open(path: impl Into<PathBuf>, codec: Box<dyn JournalCodec>) -> Result<Self, JournalError> {
        let path = path.into();

        // Detect next sequence from existing entries.
        let next_sequence = if path.exists() && std::fs::metadata(&path)?.len() > 0 {
            let reader = JournalReader::new(Box::new(BincodeCodec));
            let entries = reader.read_all(&path, |_| {})?;
            entries.last().map_or(0, |e| e.sequence + 1)
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
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
    /// successfully decoded entries in order.
    pub fn read_all(
        &self,
        path: &Path,
        mut on_error: impl FnMut(JournalError),
    ) -> Result<Vec<JournalEntry>, JournalError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();

        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
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
                .append(
                    GraphMutation::RemoveNode(NodeId::new(1)),
                    1001,
                )
                .unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let entries = reader.read_all(&path, |e| panic!("unexpected error: {e}")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sequence, 0);
        assert_eq!(entries[1].sequence, 1);
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

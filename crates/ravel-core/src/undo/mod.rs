// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Undo/redo system with structural sharing and crash recovery.
//!
//! The system has three layers:
//!
//! 1. **[`UndoStack`]** — in-memory stack of `Graph` snapshots. Because `Graph`
//!    uses `im::HashMap`, snapshots that share structure pay only for the
//!    changed entries. Undo/redo is O(1) pointer swap.
//!
//! 2. **[`GraphMutation`]** — a discrete, serializable change (add node, remove
//!    edge, etc.). Mutations are applied to a `Graph` to produce a new version
//!    and simultaneously appended to the journal.
//!
//! 3. **[`JournalWriter`] / [`JournalReader`]** — append-only log of mutations.
//!    On crash, the journal is replayed on top of the last-saved graph to
//!    recover unsaved work. On normal exit, the graph is saved and the journal
//!    is compacted (deleted).

pub mod journal;
pub mod mutation;
pub mod recovery;
pub mod stack;

pub use journal::{
    BincodeCodec, JOURNAL_FORMAT_VERSION, JournalCodec, JournalEntry, JournalError, JournalReader,
    JournalWriter, RonCodec, compact,
};
pub use mutation::GraphMutation;
pub use recovery::{RecoveryResult, SkippedEntry, recover};
pub use stack::UndoStack;

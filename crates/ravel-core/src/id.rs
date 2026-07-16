// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type-safe newtype identifiers for nodes, edges, and data types.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonically increasing counter shared across all [`NodeId`] allocations.
static NODE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Monotonically increasing counter shared across all [`EdgeId`] allocations.
static EDGE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Monotonically increasing counter shared across all [`CompId`] allocations.
static COMP_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Monotonically increasing counter shared across all [`LayerId`] allocations.
static LAYER_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

/// A unique, type-safe identifier for a node in the graph.
///
/// `NodeId` and `EdgeId` are distinct newtypes so the compiler prevents
/// accidental mixing of the two.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(u64);

impl NodeId {
    /// Create a `NodeId` from a raw `u64` value.
    ///
    /// Prefer [`NodeId::next`] for production code; use this constructor for
    /// tests and deserialization.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Allocate the next globally unique `NodeId`.
    pub fn next() -> Self {
        Self(NODE_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Return the inner `u64` value.
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// EdgeId
// ---------------------------------------------------------------------------

/// A unique, type-safe identifier for an edge in the graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeId(u64);

impl EdgeId {
    /// Create an `EdgeId` from a raw `u64` value.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Allocate the next globally unique `EdgeId`.
    pub fn next() -> Self {
        Self(EDGE_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Return the inner `u64` value.
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EdgeId({})", self.0)
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edge:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// CompId
// ---------------------------------------------------------------------------

/// A unique, type-safe identifier for a composition.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CompId(u64);

impl CompId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub fn next() -> Self {
        Self(COMP_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for CompId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CompId({})", self.0)
    }
}

impl fmt::Display for CompId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "comp:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// LayerId
// ---------------------------------------------------------------------------

/// A unique, type-safe identifier for a layer within a composition.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LayerId(u64);

impl LayerId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub fn next() -> Self {
        Self(LAYER_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for LayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LayerId({})", self.0)
    }
}

impl fmt::Display for LayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "layer:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// DataTypeId
// ---------------------------------------------------------------------------

/// Identifies the runtime data type flowing through a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DataTypeId(u32);

impl DataTypeId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

// Well-known data type identifiers.
impl DataTypeId {
    pub const FRAME_BUFFER: Self = Self(1);
    pub const SCALAR: Self = Self(10);
    pub const VEC2: Self = Self(11);
    pub const VEC3: Self = Self(12);
    pub const VEC4: Self = Self(13);
    pub const COLOR: Self = Self(14);
    pub const TIME_CODE: Self = Self(20);
    pub const AUDIO_BUFFER: Self = Self(30);
    pub const PLAIN_TEXT: Self = Self(40);
}

// ---------------------------------------------------------------------------
// Port indices
// ---------------------------------------------------------------------------

/// Index of an input port on a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InputPortIndex(pub u32);

/// Index of an output port on a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OutputPortIndex(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_and_edge_id_are_distinct_types() {
        let n = NodeId::new(1);
        let e = EdgeId::new(1);
        // They share the same raw value but are different types —
        // the following would be a compile error:
        // let _: NodeId = e;
        assert_eq!(n.raw(), e.raw());
    }

    #[test]
    fn next_ids_are_monotonic() {
        let a = NodeId::next();
        let b = NodeId::next();
        assert!(b.raw() > a.raw());

        let ea = EdgeId::next();
        let eb = EdgeId::next();
        assert!(eb.raw() > ea.raw());
    }

    #[test]
    fn data_type_id_well_known_constants() {
        assert_ne!(DataTypeId::FRAME_BUFFER, DataTypeId::SCALAR);
        assert_ne!(DataTypeId::VEC2, DataTypeId::VEC3);
        assert_ne!(DataTypeId::COLOR, DataTypeId::AUDIO_BUFFER);
    }

    #[test]
    fn display_formatting() {
        let n = NodeId::new(42);
        assert_eq!(format!("{n}"), "node:42");
        let e = EdgeId::new(7);
        assert_eq!(format!("{e}"), "edge:7");
        let c = CompId::new(3);
        assert_eq!(format!("{c}"), "comp:3");
        let l = LayerId::new(5);
        assert_eq!(format!("{l}"), "layer:5");
    }

    #[test]
    fn comp_and_layer_ids_are_monotonic() {
        let ca = CompId::next();
        let cb = CompId::next();
        assert!(cb.raw() > ca.raw());

        let la = LayerId::next();
        let lb = LayerId::next();
        assert!(lb.raw() > la.raw());
    }
}

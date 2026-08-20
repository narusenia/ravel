// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type-safe newtype identifiers for nodes, edges, and data types.

use serde::{Deserialize, Deserializer, Serialize};
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

/// Monotonically increasing counter shared across all [`AssetId`] allocations.
///
/// Starts at 1 so that 0 is free to mean [`AssetId::UNSET`].
static ASSET_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

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

    /// Advance the allocation counter past `raw` (idempotent; used after
    /// deserializing documents so fresh ids never collide with loaded ones).
    pub fn advance_counter_past(raw: u64) {
        NODE_ID_COUNTER.fetch_max(raw.saturating_add(1), Ordering::Relaxed);
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

    /// Advance the allocation counter past `raw` (idempotent; used after
    /// deserializing documents so fresh ids never collide with loaded ones).
    pub fn advance_counter_past(raw: u64) {
        EDGE_ID_COUNTER.fetch_max(raw.saturating_add(1), Ordering::Relaxed);
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

    /// Advance the allocation counter past `raw` (idempotent; used after
    /// deserializing documents so fresh ids never collide with loaded ones).
    pub fn advance_counter_past(raw: u64) {
        COMP_ID_COUNTER.fetch_max(raw.saturating_add(1), Ordering::Relaxed);
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

    /// Advance the allocation counter past `raw` (idempotent; used after
    /// deserializing documents so fresh ids never collide with loaded ones).
    pub fn advance_counter_past(raw: u64) {
        LAYER_ID_COUNTER.fetch_max(raw.saturating_add(1), Ordering::Relaxed);
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
// AssetId
// ---------------------------------------------------------------------------

/// A unique, type-safe identifier for one media asset of a document
/// (`Document::media_assets`).
///
/// Separate from the asset's display name so that the name can be edited and
/// the identity cannot. An id is **never reused**: deleting an asset and
/// importing another file with the same name mints a fresh id, so references
/// left behind by the deletion surface as offline instead of silently
/// attaching to the new file. The same property is what makes a layer copied
/// between projects fail to resolve rather than resolve to something else
/// (`docs/implementation/asset-identity-plan.md`).
///
/// Before `.ravprj` v9 an asset was keyed by that display string directly;
/// [`Deserialize`] still accepts the old form, which is what makes the v8 → v9
/// upgrade possible (see [`crate::composition::asset_legacy`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct AssetId(u64);

impl AssetId {
    /// The id no asset ever has: a reference that resolves to nothing.
    ///
    /// [`Self::next`] starts at 1, so this is not merely unused today but
    /// unreachable by allocation. It is what an audio source with no asset
    /// holds, and what the v8 → v9 upgrade writes for a reference that named
    /// an asset the document does not contain.
    pub const UNSET: Self = Self(0);

    /// Create an `AssetId` from a raw `u64` value.
    ///
    /// Prefer [`AssetId::next`] for production code; use this constructor for
    /// tests and deserialization.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Allocate the next globally unique `AssetId`.
    pub fn next() -> Self {
        Self(ASSET_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Advance the allocation counter past `raw` (idempotent; used after
    /// deserializing documents so fresh ids never collide with loaded ones).
    pub fn advance_counter_past(raw: u64) {
        ASSET_ID_COUNTER.fetch_max(raw.saturating_add(1), Ordering::Relaxed);
    }

    /// Return the inner `u64` value.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// The spelling a `media` node's `asset_id` parameter holds.
    ///
    /// That parameter is a [`ParameterValue::String`](crate::graph::ParameterValue),
    /// and stays one: giving `ParameterValue` an asset variant would reach the
    /// property editors, the expression language, the undo journal's
    /// positional variant indexes, and every parameter migration, for a
    /// reference the processor immediately turns back into an id. Plain
    /// decimal — not the `Display` form — so the value stays readable in
    /// `document/main.ron` and parses with one `str::parse`.
    pub fn to_param_value(self) -> String {
        self.0.to_string()
    }

    /// Read back [`Self::to_param_value`]. `None` when the text is not a
    /// decimal id, which is how a reference left by an older build — or by a
    /// hand edit — is told apart from one that merely points at a missing
    /// asset.
    pub fn from_param_value(text: &str) -> Option<Self> {
        text.parse().ok().map(Self)
    }
}

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetId({})", self.0)
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "asset:{}", self.0)
    }
}

impl<'de> Deserialize<'de> for AssetId {
    /// Accept both the current numeric form and the pre-v9 display string.
    ///
    /// A hand-written visitor rather than `#[serde(untagged)]`: untagged
    /// buffers the value and reports "data did not match any variant" for
    /// anything unexpected, which is the wrong message for a document whose
    /// asset table has been hand-edited.
    ///
    /// It has to be `deserialize_any` — only the input can say whether this is
    /// a number or a name — and that costs the newtype spelling the derived
    /// deserializers of the ids beside this one get for free: RON writes a
    /// newtype struct as `(1)` (or `AssetId(1)` with `struct_names`), which
    /// arrives here as a one-element sequence rather than an integer. Hence
    /// [`Visitor::visit_seq`] and [`Visitor::visit_newtype_struct`]: all four
    /// spellings are the same id.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AssetIdVisitor;

        impl<'de> serde::de::Visitor<'de> for AssetIdVisitor {
            type Value = AssetId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an asset id (a u64, or a pre-v9 asset key string)")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<AssetId, E> {
                Ok(AssetId(value))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<AssetId, E> {
                u64::try_from(value)
                    .map(AssetId)
                    .map_err(|_| E::custom(format!("asset id {value} is negative")))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<AssetId, E> {
                Ok(crate::composition::asset_legacy::intern(value))
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<AssetId, D::Error> {
                AssetId::deserialize(deserializer)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<AssetId, A::Error> {
                let id = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::custom("an asset id holds one value"))?;
                Ok(id)
            }
        }

        deserializer.deserialize_any(AssetIdVisitor)
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
    pub const GEOMETRY: Self = Self(50);
    pub const FIELD: Self = Self(51);
    /// A 3D scene ([`crate::scene::Scene`]): objects, their transforms, and
    /// cameras. Numbered inside the 50-block with the other structured
    /// geometry-domain types rather than starting a new decade, because a
    /// scene is what the geometry domain feeds into.
    pub const SCENE: Self = Self(52);
    /// Internal tag for [`crate::types::PortRecord`], the value carried by
    /// multi-output nodes. Never appears on a port.
    pub const RECORD: Self = Self(60);
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

    #[test]
    fn advance_counter_past_moves_next_beyond_raw() {
        // Use distinct, widely separated bases so concurrent tests allocating
        // ids cannot interfere with the one-directional assertions.
        NodeId::advance_counter_past(1_000_000);
        assert!(NodeId::next().raw() > 1_000_000);

        EdgeId::advance_counter_past(1_000_000);
        assert!(EdgeId::next().raw() > 1_000_000);

        CompId::advance_counter_past(1_000_000);
        assert!(CompId::next().raw() > 1_000_000);

        LayerId::advance_counter_past(1_000_000);
        assert!(LayerId::next().raw() > 1_000_000);
    }

    #[test]
    fn advance_counter_past_is_idempotent() {
        NodeId::advance_counter_past(2_000_000);
        // Re-advancing to a lower watermark must not lower the counter.
        NodeId::advance_counter_past(10);
        assert!(NodeId::next().raw() > 2_000_000);
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pre-v9 asset keys, and the ids that stand in for them while a legacy
//! `.ravprj` is being read.
//!
//! Before `.ravprj` v9 a media asset *was* its display string: the key of
//! `Document::media_assets` and the value every reference held
//! (`docs/implementation/asset-identity-plan.md`). v9 splits the two, so a v8
//! document has to hand its strings to an [`AssetId`] somewhere, and the only
//! place that sees them is deserialization.
//!
//! # Why a scoped table instead of a field
//!
//! Two independent sites carry those strings, and they are read in the order
//! the RON happens to store them: `Document::compositions` (a layer's
//! [`AudioSource`](crate::composition::AudioSource)) comes **before**
//! `Document::media_assets`. A reference and the asset it names must end up
//! with the *same* id, so whichever is read first has to mint it and the other
//! has to find it — which is what interning by string does, in either order.
//!
//! The alternative — keeping each legacy string in a `#[serde(skip)]` field
//! until an upgrade pass consumes it — would leave a migration-only field on
//! `AudioSource` permanently, in a type that is compared for undo equality.
//!
//! # Scope
//!
//! [`scoped`] installs a fresh table for exactly one deserialization and hands
//! it back. It is **not** an optimisation to skip: two legacy documents that
//! both name an asset `"plate"` must not share an id, or a layer copied from
//! one into the other would resolve to the other project's file — the precise
//! failure v9 exists to remove.
//!
//! A current (v9+) document never reaches this module: its keys and references
//! are numbers, so [`AssetId`]'s deserializer never sees a string.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::id::AssetId;

thread_local! {
    /// The table [`intern`] fills, for the deserialization [`scoped`] wraps.
    ///
    /// Thread-local rather than passed down because serde has no place to put
    /// it: the deserializers that need it are `impl Deserialize for AssetId`
    /// and everything that contains one, none of which take a context.
    static LEGACY_KEYS: RefCell<HashMap<String, AssetId>> = RefCell::new(HashMap::new());
}

/// The pre-v9 asset keys seen during one deserialization, and the ids minted
/// for them.
///
/// Empty for a current document, so `ravel_project`'s upgrade pass has
/// nothing to do when an archive turns out to carry no legacy key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyAssetKeys {
    by_name: HashMap<String, AssetId>,
}

impl LegacyAssetKeys {
    /// The id minted for the pre-v9 key `name`, if the document carried one.
    pub fn id_of(&self, name: &str) -> Option<AssetId> {
        self.by_name.get(name).copied()
    }

    /// The pre-v9 key that produced `id` — the string that must remain the
    /// asset's display name so the upgrade does not rename anything.
    pub fn name_of(&self, id: AssetId) -> Option<&str> {
        self.by_name
            .iter()
            .find_map(|(name, minted)| (*minted == id).then_some(name.as_str()))
    }

    /// Whether the deserialization saw no pre-v9 key at all.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Mint — or reuse — the [`AssetId`] standing in for the pre-v9 key `name`.
///
/// The empty string is the pre-v9 spelling of "no asset" (an `AudioSource` on
/// a layer that has none), so it maps to [`AssetId::UNSET`] rather than
/// consuming an id.
///
/// Ids come from the global counter, not from a per-document sequence: two
/// projects must not accidentally agree on an id, because a layer pasted
/// across them has to fail to resolve rather than resolve to the wrong file.
pub(crate) fn intern(name: &str) -> AssetId {
    if name.is_empty() {
        return AssetId::UNSET;
    }
    LEGACY_KEYS
        .with_borrow_mut(|table| *table.entry(name.to_string()).or_insert_with(AssetId::next))
}

/// Run `f` with an empty legacy-key table, returning its result and the table.
///
/// Wrap the deserialization of one document. The table is cleared on the way
/// in as well as taken on the way out, so a panic part-way through a parse
/// cannot leak keys into the next one.
pub fn scoped<R>(f: impl FnOnce() -> R) -> (R, LegacyAssetKeys) {
    LEGACY_KEYS.with_borrow_mut(HashMap::clear);
    let result = f();
    let by_name = LEGACY_KEYS.with_borrow_mut(std::mem::take);
    (result, LegacyAssetKeys { by_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_legacy_key_interns_to_one_id() {
        let ((first, second), keys) = scoped(|| (intern("plate"), intern("plate")));
        assert_eq!(first, second, "one key, one asset");
        assert_eq!(keys.id_of("plate"), Some(first));
        assert_eq!(keys.name_of(first), Some("plate"));
    }

    #[test]
    fn different_legacy_keys_intern_to_different_ids() {
        let ((plate, still), _) = scoped(|| (intern("plate"), intern("still")));
        assert_ne!(plate, still);
    }

    #[test]
    fn the_empty_key_is_the_unset_id_and_is_not_recorded() {
        let (id, keys) = scoped(|| intern(""));
        assert_eq!(id, AssetId::UNSET);
        assert!(keys.is_empty(), "\"no asset\" is not an asset");
    }

    /// Two documents naming the same asset must not share its id: the whole
    /// point of v9 is that a reference carried across projects fails to
    /// resolve instead of resolving to a different file.
    #[test]
    fn separate_scopes_do_not_share_ids() {
        let (first, _) = scoped(|| intern("plate"));
        let (second, _) = scoped(|| intern("plate"));
        assert_ne!(first, second);
    }

    #[test]
    fn a_scope_that_saw_no_legacy_key_yields_an_empty_table() {
        let ((), keys) = scoped(|| ());
        assert!(keys.is_empty());
    }
}

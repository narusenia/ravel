// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v8 → v9: point every asset reference at an
//! [`AssetId`](crate::id::AssetId) instead of a display string.
//!
//! Before v9 an asset *was* its name: the key of
//! [`Document::media_assets`](super::Document) and the value each of the three
//! reference systems held — a `media` node's `asset_id` parameter, an
//! [`AudioSource`](super::AudioSource), and the `media` node parameter an
//! exposed declaration is bound to. Deleting an asset freed the name, so the
//! next import of a file with that stem quietly adopted every reference the
//! deletion left behind (`docs/implementation/asset-identity-plan.md`).
//!
//! Deserialization has already done the first half: [`AssetId`]'s deserializer
//! accepts a pre-v9 key and mints an id for it, recording the pairing in
//! [`LegacyAssetKeys`]. This pass does the second half over the loaded
//! document, like the v4 → v5 fold and the v5 → v6 curve upgrade beside it and
//! for the same reason — the `manifest.json` migration chain never sees
//! `document/main.ron`.
//!
//! Three steps:
//!
//! 1. give each asset the name it was keyed by, so nothing on screen changes;
//! 2. record, on each asset a pre-v9 exposed apply had created, the
//!    declaration that created it
//!    ([`MediaAssetEntry::exposed_owner`](super::MediaAssetEntry::exposed_owner));
//! 3. rewrite each `media` node's `asset_id` parameter to the decimal id.
//!
//! An [`AudioSource`](super::AudioSource) needs no step of its own: its
//! `asset_id` is an [`AssetId`], so the interner resolved it while the document
//! was being read, to the same id as the table entry of that name. A `media`
//! node's reference cannot go that way — it is an untyped
//! [`ParameterValue::String`], which the interner never sees.
//!
//! # References that name nothing
//!
//! A pre-v9 document can already hold a reference to an asset its table does
//! not contain — that is what deleting an asset left behind. Such a reference
//! **is not carried over as a string**: it becomes
//! [`AssetId::UNSET`](crate::id::AssetId::UNSET), the id no asset ever has.
//! Leaving the name would make it a reference that resolves again the moment
//! somebody imports a file with that name, which is the bug this version
//! removes; and a `media` node holding an unresolvable id is not an error but
//! an offline node — `ravel_nodes::media` yields a transparent frame for it.
//!
//! # Idempotence
//!
//! Running twice leaves the document alone: [`resolve`] recognises a reference
//! that already names a live asset by id and does not touch it.
//!
//! It still resolves **names first**, so a v8 asset that happened to be called
//! `7` wins over the id `7` — in a v8 document a reference was a name and
//! nothing else, and matching the old reader is what makes an upgrade correct.
//! The two orders can only disagree for a document that was already both, which
//! is why the version stamp, not this ordering, is what keeps the pass to one
//! run over a project's life.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::{Graph, ParameterValue};
use crate::id::AssetId;

use crate::exposed::apply::EXPOSED_ASSET_NAME_PREFIX;

use super::asset_legacy::LegacyAssetKeys;
use super::{Document, MEDIA_ASSET_PARAM_KEY, MEDIA_TYPE_KEYS};

/// Apply the v8 → v9 asset reference upgrade to `document`.
///
/// Runs once, gated on the archive's version by the caller. A **v9** file that
/// carries a pre-v9 string anyway — hand-edited, or merged by hand from two
/// projects — is deliberately not repaired here: this pass reads a reference
/// that is neither a name in the table nor a live id as offline, and re-running
/// it over a current document would turn a legitimate reference to a *deleted*
/// asset (`AssetId` present, entry gone — the normal offline state after v9)
/// into [`AssetId::UNSET`], losing the difference between "named something
/// that is gone" and "named nothing". The mixed file resolves to offline
/// instead, which is the safe direction and visible in the log.
pub(super) fn upgrade(mut document: Document, legacy: &LegacyAssetKeys) -> Document {
    // No early exit on an empty `legacy`: a v8 project whose assets were all
    // deleted still holds references to their names, and leaving those as
    // strings would put a value in a v9 document that is neither a name nor an
    // id. They belong at `AssetId::UNSET` like any other reference to
    // something that is not there.

    // Step 1: the key becomes the name. An entry that already has one keeps
    // it — a v9 document does not reach this pass, but a hand-merged file
    // could carry both.
    let named: Vec<(AssetId, String)> = document
        .media_assets
        .iter()
        .filter(|(_, entry)| entry.name.is_empty())
        .filter_map(|(id, _)| legacy.name_of(*id).map(|name| (*id, name.to_string())))
        .collect();
    for (id, name) in named {
        if let Some(entry) = document.media_assets.get_mut(&id) {
            entry.name = name;
        }
    }

    // Step 2: a pre-v9 apply recorded ownership only in the name it derived
    // (`exposed::apply::asset_name_for`) — the one document state where that
    // name is authoritative, because nothing could rename an asset yet. Carry
    // it into the explicit field, or the next apply of that declaration would
    // see an unowned entry and add a second one beside it.
    let owners: Vec<(AssetId, String)> = document
        .media_assets
        .iter()
        .filter(|(_, entry)| entry.exposed_owner.is_none())
        .filter_map(|(id, entry)| {
            let declared = entry.name.strip_prefix(EXPOSED_ASSET_NAME_PREFIX)?;
            document
                .exposed_parameters
                .iter()
                .any(|parameter| parameter.name() == declared)
                .then(|| (*id, declared.to_string()))
        })
        .collect();
    for (id, owner) in owners {
        if let Some(entry) = document.media_assets.get_mut(&id) {
            entry.exposed_owner = Some(owner);
        }
    }

    // The reference systems resolve against the names the table now carries,
    // not against `legacy`: an interned key that no asset uses is a reference
    // to something the document does not have, and must stay unresolved.
    let by_name: HashMap<&str, AssetId> = document
        .media_assets
        .iter()
        .map(|(id, entry)| (entry.name.as_str(), *id))
        .collect();

    // Collected before anything is written back: every graph resolves against
    // the same table.
    let graph = upgrade_graph(&document.graph, &by_name);
    let comp_ids: Vec<_> = document.compositions.keys().copied().collect();
    let mut compositions = document.compositions.clone();
    for comp_id in comp_ids {
        let Some(comp) = compositions.get(&comp_id) else {
            continue;
        };
        let mut updated = (**comp).clone();
        for layer in updated.layers.iter_mut() {
            layer.network = upgrade_graph(&layer.network, &by_name);
        }
        compositions.insert(comp_id, Arc::new(updated));
    }
    document.graph = graph;
    document.compositions = compositions;
    document
}

/// Rewrite every `media` node's asset reference in `graph`, descending into
/// subnets.
fn upgrade_graph(graph: &Graph, by_name: &HashMap<&str, AssetId>) -> Graph {
    super::graph_walk::map_subnets(graph, &|graph| upgrade_level(graph, by_name))
}

/// Rewrite one graph's own nodes, ignoring its subnets — the shared walk
/// visits those separately.
fn upgrade_level(graph: &Graph, by_name: &HashMap<&str, AssetId>) -> Graph {
    let mut upgraded = graph.clone();
    for id in upgraded.node_ids().collect::<Vec<_>>() {
        let Some(node) = upgraded.node(id) else {
            continue;
        };
        if !MEDIA_TYPE_KEYS.contains(&node.type_key.as_str()) {
            continue;
        }
        let mut updated = (**node).clone();
        let mut changed = false;
        for param in updated.parameters.iter_mut() {
            if param.key != MEDIA_ASSET_PARAM_KEY {
                continue;
            }
            // Any other kind is a reference that has been replaced by
            // something that is not one; leave it for the processor to
            // reject rather than inventing an id for it.
            let ParameterValue::String(stored) = &param.value else {
                continue;
            };
            // The empty string is the template default: a `media` node nobody
            // has pointed at an asset yet. It is not a broken reference and
            // must not be reported as one.
            if stored.is_empty() {
                continue;
            }
            param.value = ParameterValue::String(resolve(stored, by_name, "a media node"));
            changed = true;
        }
        if changed {
            upgraded = upgraded.replace_node(Arc::new(updated));
        }
    }
    upgraded
}

/// The decimal id the pre-v9 reference `stored` becomes.
///
/// [`AssetId::UNSET`] when nothing in the table answers it, which is the
/// reference the deletion-then-reimport hazard left behind. Logged: an asset
/// reference going offline is a visible change to what the project renders,
/// and it is better traced to the upgrade than discovered later.
fn resolve(stored: &str, by_name: &HashMap<&str, AssetId>, holder: &str) -> String {
    // A name first: that is all a reference was before v9.
    if let Some(id) = by_name.get(stored) {
        return id.to_param_value();
    }
    // Already an id naming a live asset — a second run over an upgraded
    // document, which must change nothing.
    if let Some(id) = AssetId::from_param_value(stored)
        && by_name.values().any(|live| *live == id)
    {
        return stored.to_string();
    }
    tracing::warn!(
        asset = stored,
        "{holder} references an asset the project does not contain; \
         it is offline from now on rather than adopting a later import"
    );
    AssetId::UNSET.to_param_value()
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The output-stage frame cache (`docs/implementation/cache-plan.md`,
//! unit `CACHE-5`).
//!
//! The [`Evaluator`](crate::eval::Evaluator) caches one value per node. That
//! is what makes a re-pull of an unchanged graph cheap, but it is *not* what
//! makes scrubbing back over a frame free: moving the playhead changes the
//! evaluation's [`TimeKey`], every time-varying node misses, and the whole
//! composition is rebuilt. This layer sits **outside** evaluation and keeps
//! the finished picture of a composition, keyed by time:
//!
//! ```text
//!   request (comp, CacheIdentity) ─▶ FrameCache ─hit─▶ finished frame
//!                                        │ miss
//!                                        ▼
//!                                   Evaluator (node results)
//! ```
//!
//! # Why it is outside the evaluator, not inside it
//!
//! Evaluation stays pure: a `process()` call has no idea this exists, and the
//! cache is consulted and filled by the worker around it. That is the
//! decision `cache-plan.md` records — multi-entry node caching would drag the
//! whole invalidation path (`drop_scope_owner_caches` and friends) into
//! re-verification, while everything the user *feels* — scrubbing, playback,
//! the timeline's cache band — lives at the output stage.
//!
//! # What a key is
//!
//! `(CompId, TimeKey)` selects the slot; the full [`CacheIdentity`] stored
//! with the entry decides whether that slot answers the request. This mirrors
//! the evaluator exactly (one entry plus an identity check) rather than
//! inventing a second matching rule, so `resolution`, `fps` and `quality`
//! match by equality and `precision` by order: an entry produced under a
//! reduced floor cannot answer an export's `F32` request, which is the one
//! structural reason a render can never pick up a preview-grade picture.
//!
//! The node id is deliberately **not** part of the key. Compiled shell graphs
//! get fresh node ids on every recompile, so a node-keyed slot would leave
//! unreachable entries behind that
//! [`cached_ranges`](FrameCache::cached_ranges) would still report — a
//! timeline band that lies.
//!
//! # Invalidation
//!
//! **Which compositions** is driven by the same document diff the evaluator
//! uses: `Document::compositions` holds `Arc<Composition>`, so an untouched
//! composition is pointer-equal across snapshots and an edited one is not.
//! Doing it this way rather than from [`InvalidationHint`] is not a
//! refinement — many document commits pass `InvalidationHint::None` and rely
//! on the evaluator's diff, and a hint-driven frame cache would serve those
//! edits a stale picture.
//!
//! **Which frames of those compositions** is `CACHE-7`. The document diff has
//! no idea *what* changed, so on its own it can only drop everything. An
//! [`InvalidationHint::Params`] does know, and a layer only reaches the
//! composition output inside its own `[start_frame, start_frame + duration)`
//! (`comp/mod.rs` gates the layer network on exactly that span, and
//! `layer.ref` gates on the *target's* span, so a reference cannot widen it).
//! So when — and only when — every coalesced request carried a `Params` hint
//! and every named node resolves to an owning layer, the drop is narrowed to
//! those layers' spans. Anything else keeps the whole-composition drop:
//! narrowing is an optimisation on top of a safe default, never a
//! replacement for it. See [`invalidation_plan`].
//!
//! [`InvalidationHint`]: super::InvalidationHint
//! [`InvalidationHint::Params`]: super::InvalidationHint::Params

use crate::cache_budget::{
    CacheKind, Evicted, Reservation, ReservationId, SharedCacheBudget, Tier,
};
use crate::composition::validate::{PRECOMP_COMP_ID_PARAM, PRECOMP_TYPE_KEY};
use crate::composition::{Composition, Document, Layer};
use crate::eval::{CacheMiss, EvalContext, Precision, TimeKey};
use crate::graph::{Graph, ParameterValue};
use crate::id::{CompId, LayerId, NodeId};
use crate::types::{FrameBuffer, NodeData};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) use crate::eval::CacheIdentity;

/// Which finished frame an entry is. See the module documentation for why the
/// node id is absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FrameSlot {
    comp: CompId,
    time: TimeKey,
}

struct FrameEntry {
    identity: CacheIdentity,
    value: Arc<dyn NodeData>,
    /// The budget claim. Dropping the entry releases it, so no removal path
    /// has to remember to. `None` when the cache runs without a budget.
    reservation: Option<Reservation>,
    bytes: u64,
    tier: Tier,
}

/// Hit / miss tallies and the bytes the frame cache is holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCacheStats {
    /// Frame requests served from the cache.
    pub hits: u64,
    /// Frame requests that had to be evaluated, per reason
    /// ([`CacheMiss::index`] order).
    pub misses_by_reason: [u64; CacheMiss::COUNT],
    /// Frames currently cached.
    pub entries: usize,
    /// Bytes currently cached per tier, in [`Tier::ALL`] order.
    pub bytes_by_tier: [u64; 3],
}

impl FrameCacheStats {
    /// Requests that missed, for any reason.
    pub fn misses(&self) -> u64 {
        self.misses_by_reason.iter().sum()
    }

    /// Requests recorded, hits and misses together.
    pub fn requests(&self) -> u64 {
        self.hits + self.misses()
    }

    /// Misses attributed to `reason`.
    pub fn misses_for(&self, reason: CacheMiss) -> u64 {
        self.misses_by_reason[reason.index()]
    }

    /// Share of requests served from cache, or `None` when nothing was asked.
    pub fn hit_rate(&self) -> Option<f64> {
        let requests = self.requests();
        (requests > 0).then(|| self.hits as f64 / requests as f64)
    }

    /// Bytes cached in `tier`.
    pub fn bytes(&self, tier: Tier) -> u64 {
        self.bytes_by_tier[match tier {
            Tier::Vram => 0,
            Tier::Ram => 1,
            Tier::Disk => 2,
        }]
    }
}

fn tier_index(tier: Tier) -> usize {
    match tier {
        Tier::Vram => 0,
        Tier::Ram => 1,
        Tier::Disk => 2,
    }
}

/// The cache itself. Reach it through [`SharedFrameCache`].
#[derive(Default)]
pub struct FrameCache {
    entries: HashMap<FrameSlot, FrameEntry>,
    /// Reservation → the slot that owns it, so an eviction list can be turned
    /// back into cache keys.
    by_reservation: HashMap<ReservationId, FrameSlot>,
    /// Eviction entries handed to this cache that belong elsewhere, drained
    /// by the worker (see [`Self::take_foreign_evictions`]).
    foreign_evictions: Vec<Evicted>,
    budget: Option<SharedCacheBudget>,
    used: [u64; 3],
    hits: u64,
    misses: [u64; CacheMiss::COUNT],
    /// Bumped by every insert and every drop.
    ///
    /// Lets a consumer skip work when nothing can have changed: recomputing
    /// [`Self::cached_ranges`] walks every entry and sorts, and the UI thread
    /// asks after *each* evaluation — including the hits, which by definition
    /// added nothing (`CACHE-6`).
    version: u64,
}

impl FrameCache {
    /// A cache that reports to `budget`. Without one it is unbounded — the
    /// shape every test and example without a budget keeps.
    fn new(budget: Option<SharedCacheBudget>) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    // ----- reads -----------------------------------------------------------

    /// The finished frame of `comp` that answers `wanted`, if one is cached.
    ///
    /// Records the hit or the reason for the miss, so "the frame cache
    /// stopped working" is observable in CI rather than only as a slower
    /// scrub.
    fn get(&mut self, comp: CompId, wanted: &CacheIdentity) -> Option<Arc<dyn NodeData>> {
        let slot = FrameSlot {
            comp,
            time: wanted.time,
        };
        let Some(entry) = self.entries.get(&slot) else {
            self.misses[CacheMiss::NoEntry.index()] += 1;
            return None;
        };
        if let Some(miss) = entry.identity.mismatch(wanted) {
            self.misses[miss.index()] += 1;
            return None;
        }
        if let (Some(budget), Some(reservation)) = (&self.budget, &entry.reservation) {
            budget.touch(reservation.id());
        }
        self.hits += 1;
        Some(entry.value.clone())
    }

    /// Frame ranges of `comp` a request built from `wanted` would hit, as
    /// half-open `[start, end)` spans of integer frames (`CACHE-6`).
    ///
    /// **The filter is [`CacheIdentity::mismatch`] itself**, not a subset of
    /// its axes. `quality`, `fps` and `comp_resolution` decide a hit exactly
    /// as `resolution` and `precision` do, so leaving any of them out would
    /// paint frames green that a scrub then recomputes — the same reason the
    /// slot key omits the node id.
    ///
    /// The time axis is supplied per entry: `wanted`'s own frame position is
    /// irrelevant, the question is which *other* positions are cached.
    ///
    /// Sub-frame entries (motion-blur shutter samples) are not frames a
    /// playhead can land on, so they are excluded rather than rounded — a
    /// band drawn from rounded samples would claim frames that would miss.
    pub fn cached_ranges(&self, comp: CompId, wanted: &EvalContext) -> Vec<Range<u64>> {
        let scale = TimeKey::SUBFRAME_SCALE as i64;
        let mut wanted = CacheIdentity::of_frame(wanted);
        let mut frames: Vec<u64> = Vec::new();
        for (slot, entry) in &self.entries {
            if slot.comp != comp || slot.time.is_timeless() {
                continue;
            }
            let ticks = slot.time.ticks();
            if ticks < 0 || ticks % scale != 0 {
                continue;
            }
            wanted.time = slot.time;
            if entry.identity.mismatch(&wanted).is_none() {
                frames.push((ticks / scale) as u64);
            }
        }
        frames.sort_unstable();
        frames.dedup();

        let mut ranges: Vec<Range<u64>> = Vec::new();
        for frame in frames {
            match ranges.last_mut() {
                Some(range) if range.end == frame => range.end = frame + 1,
                _ => ranges.push(frame..frame + 1),
            }
        }
        ranges
    }

    /// Whether `wanted` would be answered, **without recording anything**.
    ///
    /// Read-ahead asks before evaluating a frame (`CACHE-9`), and speculation
    /// is not a request a user made: counting its probes as hits and misses
    /// would move the hit rate the logs and the tests read.
    fn contains(&self, comp: CompId, wanted: &CacheIdentity) -> bool {
        self.entries
            .get(&FrameSlot {
                comp,
                time: wanted.time,
            })
            .is_some_and(|entry| entry.identity.mismatch(wanted).is_none())
    }

    /// A counter that changes exactly when the set of cached frames does.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Hit / miss tallies and the bytes held.
    pub fn stats(&self) -> FrameCacheStats {
        FrameCacheStats {
            hits: self.hits,
            misses_by_reason: self.misses,
            entries: self.entries.len(),
            bytes_by_tier: self.used,
        }
    }

    // ----- writes ----------------------------------------------------------

    /// Store `value` as the finished frame of `comp` for `identity`.
    ///
    /// A value the request declared a reduced precision floor for is stored
    /// reduced (see [`store_value`]), so an entry never promises more than it
    /// holds.
    ///
    /// `speculative` marks a frame produced by read-ahead rather than by a
    /// request a user waited for (`CACHE-9`). It changes nothing about the
    /// entry except its eviction rank: under pressure the budget empties
    /// speculation first.
    fn insert(
        &mut self,
        comp: CompId,
        identity: CacheIdentity,
        value: Arc<dyn NodeData>,
        speculative: bool,
    ) {
        let slot = FrameSlot {
            comp,
            time: identity.time,
        };
        // Replacing an entry releases the old claim first, so a composition
        // re-evaluated at the same time does not accumulate reservations.
        self.drop_slot(&slot);

        let value = store_value(value, identity.precision);
        let bytes = value.byte_size();
        // The tier is a property of the *stored* value. Today
        // `GpuEvalHooks::finalize` reads a GPU frame back for the viewer, so
        // what lands here is host memory; the moment display happens from a
        // texture the same value arrives GPU-resident and the VRAM tier
        // starts accounting for it without a change to this code.
        let tier = if value.is_gpu_resident() {
            Tier::Vram
        } else {
            Tier::Ram
        };
        let (reservation, evicted) = match &self.budget {
            Some(budget) => {
                let kind = CacheKind::Frame(tier);
                let (reservation, evicted) = match speculative {
                    true => budget.reserve_speculative(kind, bytes),
                    false => budget.reserve(kind, bytes),
                };
                (Some(reservation), evicted)
            }
            None => (None, Vec::new()),
        };
        if let Some(reservation) = &reservation {
            self.by_reservation.insert(reservation.id(), slot);
        }
        self.used[tier_index(tier)] += bytes;
        self.version += 1;
        self.entries.insert(
            slot,
            FrameEntry {
                identity,
                value,
                reservation,
                bytes,
                tier,
            },
        );
        self.drop_evicted(&evicted);
    }

    /// Drop the frames `evicted` names that this cache owns, and park the
    /// rest for their owner.
    ///
    /// A dropped entry releases the value it holds — for a GPU-resident frame
    /// that is what returns the texture to the pool, through
    /// `GpuFrameBuffer`'s own `Drop`.
    fn drop_evicted(&mut self, evicted: &[Evicted]) {
        for entry in evicted {
            let Some(victim) = self.by_reservation.remove(&entry.id) else {
                // Not ours: the node-result cache's. Buffered rather than
                // skipped — the budget has already released these bytes, so
                // an entry nobody drops makes the limit stop being a limit.
                self.foreign_evictions.push(*entry);
                continue;
            };
            self.drop_slot(&victim);
        }
    }

    /// Take the eviction entries this cache was told about but does not own.
    fn take_foreign_evictions(&mut self) -> Vec<Evicted> {
        std::mem::take(&mut self.foreign_evictions)
    }

    /// Drop every frame of `comp`.
    fn invalidate_comp(&mut self, comp: CompId) {
        let slots: Vec<FrameSlot> = self
            .entries
            .keys()
            .filter(|slot| slot.comp == comp)
            .copied()
            .collect();
        for slot in slots {
            self.drop_slot(&slot);
        }
    }

    /// Drop everything.
    fn clear(&mut self) {
        for slot in self.entries.keys().copied().collect::<Vec<_>>() {
            self.drop_slot(&slot);
        }
        debug_assert_eq!(self.used, [0; 3], "frame cache byte accounting drifted");
    }

    /// Drop the frames of `comp` that `spans` (in composition frames) covers.
    fn invalidate_spans(&mut self, comp: CompId, spans: &[Range<i64>]) {
        let scale = TimeKey::SUBFRAME_SCALE as i64;
        let victims: Vec<FrameSlot> = self
            .entries
            .keys()
            .filter(|slot| slot.comp == comp && !slot.time.is_timeless())
            .filter(|slot| {
                let ticks = slot.time.ticks();
                spans.iter().any(|span| {
                    ticks >= span.start.saturating_mul(scale)
                        && ticks < span.end.saturating_mul(scale)
                })
            })
            .copied()
            .collect();
        for slot in victims {
            self.drop_slot(&slot);
        }
    }

    /// Drop the frames the step from `old` to `new` invalidates.
    ///
    /// Reads the same signals [`Evaluator::set_document`] does: media assets
    /// compared as a whole (a path swap is invisible to the composition
    /// diff), then per composition by `Arc` identity, which structural
    /// sharing makes free for untouched ones. `None` for `old` is the first
    /// document the worker ever sees and invalidates nothing — there is no
    /// earlier snapshot the cache could hold frames from.
    ///
    /// `params` carries the node ids of an [`InvalidationHint::Params`] when
    /// **every** request the worker coalesced into this step carried one; it
    /// is what lets [`invalidation_plan`] narrow the drop to the frames those
    /// nodes' layers reach. `None` keeps the whole-composition drop.
    ///
    /// [`Evaluator::set_document`]: crate::eval::Evaluator::set_document
    /// [`InvalidationHint::Params`]: super::InvalidationHint::Params
    fn sync_document(&mut self, old: Option<&Document>, new: &Document, params: Option<&[NodeId]>) {
        let Some(old) = old else {
            return;
        };
        if old.media_assets != new.media_assets {
            self.clear();
            return;
        }
        let stale: Vec<CompId> = old
            .compositions
            .iter()
            .filter(|(id, comp)| match new.compositions.get(id) {
                None => true,
                Some(new_comp) => !Arc::ptr_eq(comp, new_comp),
            })
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return;
        }
        for (comp, spans) in invalidation_plan(new, &stale, params) {
            match spans {
                Some(spans) => self.invalidate_spans(comp, &spans),
                None => self.invalidate_comp(comp),
            }
        }
    }

    /// Remove `slot`'s entry and its accounting, if it has one.
    fn drop_slot(&mut self, slot: &FrameSlot) {
        let Some(entry) = self.entries.remove(slot) else {
            return;
        };
        self.version += 1;
        if let Some(reservation) = &entry.reservation {
            self.by_reservation.remove(&reservation.id());
        }
        self.used[tier_index(entry.tier)] =
            self.used[tier_index(entry.tier)].saturating_sub(entry.bytes);
    }
}

// ===========================================================================
// Scoped invalidation (`CACHE-7`)
// ===========================================================================

/// How much of one composition an edit takes out.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    /// Every frame — the `CACHE-5` default, and still what any edit the hint
    /// cannot account for gets.
    Whole,
    /// Only the frames these layers reach.
    Layers(Vec<LayerId>),
}

/// Record that `scope` also applies to `comp`, reporting whether that added
/// anything. [`Scope::Whole`] absorbs; layer sets union.
fn merge_scope(scopes: &mut HashMap<CompId, Scope>, comp: CompId, scope: Scope) -> bool {
    match scopes.entry(comp) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(scope);
            true
        }
        std::collections::hash_map::Entry::Occupied(mut slot) => match (slot.get_mut(), scope) {
            (Scope::Whole, _) => false,
            (held @ Scope::Layers(_), Scope::Whole) => {
                *held = Scope::Whole;
                true
            }
            (Scope::Layers(held), Scope::Layers(added)) => {
                let mut changed = false;
                for id in added {
                    if !held.contains(&id) {
                        held.push(id);
                        changed = true;
                    }
                }
                changed
            }
        },
    }
}

/// Which frames of which compositions the step that made `stale` invalidates.
///
/// `None` in the returned span list means the whole composition. The
/// narrowing rules, in the order they decide:
///
/// 1. **No `params`** (the hint was `None`, `Structural`, or the coalesced
///    requests disagreed): every stale composition loses everything. This is
///    `CACHE-5` unchanged, and it is the branch every edit that does not name
///    its nodes still takes.
/// 2. **A node that resolves to no layer** — a synthetic node of a compiled
///    shell graph, or one the document no longer holds — disables narrowing
///    entirely. The hint is then describing something this function cannot
///    place, and placing it wrong serves a stale picture.
/// 3. A stale composition **no named node lives in** also loses everything:
///    its `Arc` moved for a reason the hint does not explain.
/// 4. Otherwise the drop is the union of the owning layers' spans, plus the
///    spans of every layer parented to one of them (a node in a layer's
///    network can drive that layer's transform channels, which its children
///    inherit outside the parent's own span).
///
/// Then propagation: a composition placed inside another through a `precomp`
/// node invalidates the **whole span of the layer holding that node**, up the
/// chain. The child's affected range is deliberately *not* mapped onto the
/// parent's timeline: no processor implements `precomp` yet, so a mapping
/// would encode a time relationship nothing in the codebase actually
/// performs, while the containing layer's own span is a bound the shell's
/// time gate already guarantees.
fn invalidation_plan(
    document: &Document,
    stale: &[CompId],
    params: Option<&[NodeId]>,
) -> Vec<(CompId, Option<Vec<Range<i64>>>)> {
    let owners = params.and_then(|ids| owning_layers(document, ids));
    let mut scopes: HashMap<CompId, Scope> = HashMap::new();
    for &comp in stale {
        let scope = match &owners {
            Some(owners) => {
                let layers: Vec<LayerId> = owners
                    .iter()
                    .filter(|(owner, _)| *owner == comp)
                    .map(|(_, layer)| *layer)
                    .collect();
                if layers.is_empty() {
                    Scope::Whole
                } else {
                    Scope::Layers(layers)
                }
            }
            None => Scope::Whole,
        };
        merge_scope(&mut scopes, comp, scope);
    }
    propagate_to_containers(document, &mut scopes);

    scopes
        .into_iter()
        .map(|(comp, scope)| {
            let spans = match (scope, document.compositions.get(&comp)) {
                (Scope::Layers(layers), Some(composition)) => affected_spans(composition, &layers),
                _ => None,
            };
            (comp, spans)
        })
        .collect()
}

/// Extend `scopes` up the `precomp` chain: a composition placed inside
/// another one takes the containing layer's frames with it.
///
/// Runs to a fixed point. Scopes only ever grow and each pass that changes
/// anything adds a composition or a layer, so the bound is reached; it is
/// stated explicitly rather than relying on
/// [`validate_precomp_cycles`](crate::composition::validate::validate_precomp_cycles),
/// because a cache must terminate on documents that never met the validator.
fn propagate_to_containers(document: &Document, scopes: &mut HashMap<CompId, Scope>) {
    for _ in 0..document.compositions.len() {
        let targets: HashSet<CompId> = scopes.keys().copied().collect();
        let mut changed = false;
        for (parent, composition) in &document.compositions {
            let layers: Vec<LayerId> = composition
                .layers
                .iter()
                .filter(|layer| references_any(&layer.network, &targets))
                .map(|layer| layer.id)
                .collect();
            if layers.is_empty() {
                continue;
            }
            changed |= merge_scope(scopes, *parent, Scope::Layers(layers));
        }
        if !changed {
            return;
        }
    }
}

/// Whether `network` (subnets included) holds a `precomp` node pointing at
/// one of `targets`.
fn references_any(network: &Graph, targets: &HashSet<CompId>) -> bool {
    network.nodes().any(|node| {
        if node.type_key == PRECOMP_TYPE_KEY
            && node
                .parameters
                .iter()
                .find(|param| param.key == PRECOMP_COMP_ID_PARAM)
                .and_then(|param| match &param.value {
                    ParameterValue::Int(id) if *id >= 0 => Some(CompId::new(*id as u64)),
                    _ => None,
                })
                .is_some_and(|id| targets.contains(&id))
        {
            return true;
        }
        node.subnet
            .as_ref()
            .is_some_and(|subnet| references_any(subnet, targets))
    })
}

/// The `(composition, layer)` pairs owning `ids`, or `None` when any id
/// belongs to no layer network at all.
fn owning_layers(document: &Document, ids: &[NodeId]) -> Option<Vec<(CompId, LayerId)>> {
    let mut owners: Vec<(CompId, LayerId)> = Vec::new();
    for &id in ids {
        let mut found = false;
        for (comp, composition) in &document.compositions {
            for layer in &composition.layers {
                if !contains_node(&layer.network, id) {
                    continue;
                }
                found = true;
                if !owners.contains(&(*comp, layer.id)) {
                    owners.push((*comp, layer.id));
                }
            }
        }
        if !found {
            return None;
        }
    }
    Some(owners)
}

/// Whether `network` or one of its subnets holds `id`.
fn contains_node(network: &Graph, id: NodeId) -> bool {
    network.node(id).is_some()
        || network
            .nodes()
            .filter_map(|node| node.subnet.as_ref())
            .any(|subnet| contains_node(subnet, id))
}

/// The composition frames `layers` reach, merged and sorted.
///
/// `None` when a named layer is not in the composition, which is the same
/// "cannot place it" answer as an unresolvable node.
fn affected_spans(composition: &Composition, layers: &[LayerId]) -> Option<Vec<Range<i64>>> {
    let mut reached: HashSet<LayerId> = HashSet::new();
    for id in layers {
        composition.get_layer(*id)?;
        reached.insert(*id);
    }
    // Transform inheritance: a node inside a layer's network can drive that
    // layer's transform channels (`ChannelSource::NodeOutput`), and every
    // layer parented to it inherits the result — at frames the parent's own
    // span does not cover. The children's spans are therefore part of the
    // reach.
    loop {
        let mut grew = false;
        for layer in &composition.layers {
            if let Some(parent) = layer.parent
                && reached.contains(&parent)
                && reached.insert(layer.id)
            {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let mut spans: Vec<Range<i64>> = composition
        .layers
        .iter()
        .filter(|layer| reached.contains(&layer.id))
        .map(layer_span)
        .collect();
    spans.sort_unstable_by_key(|span| span.start);
    let mut merged: Vec<Range<i64>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    Some(merged)
}

/// Composition frames an edit to `layer` can reach.
///
/// The layer's own `[start_frame, start_frame + duration)` plus one frame of
/// slack on each side: motion-blur shutter samples sit between frames, so the
/// evaluation of the frame next to the span can sample inside it.
fn layer_span(layer: &Layer) -> Range<i64> {
    (layer.start_frame - 1)..(layer.end_frame() + 1)
}

/// The form `value` is stored in for an entry that promises `precision`.
///
/// An entry records the floor the request declared, and the frame cache holds
/// it *at* that floor rather than above it: a preview that asked for
/// [`Precision::F16`] gets a half-float buffer, which halves what the RAM tier
/// spends on it (`cache-plan.md`: "RAM: f16 バイト列"). Nothing is ever stored
/// below the floor it promises, so the ordered precision match stays sound.
///
/// Dormant while every path requests [`Precision::F32`] — lowering the
/// viewer's floor is a product decision, not this unit's.
fn store_value(value: Arc<dyn NodeData>, precision: Precision) -> Arc<dyn NodeData> {
    if precision > Precision::F16 {
        return value;
    }
    match value.downcast_ref::<FrameBuffer>() {
        Some(frame) => Arc::new(frame.to_rgba_f16()),
        None => value,
    }
}

/// The frame cache as the worker and the UI share it.
///
/// The evaluation worker fills it; the UI thread reads
/// [`cached_ranges`](Self::cached_ranges) to draw the timeline's cache band.
/// Cloning shares one cache.
///
/// # Locking
///
/// Every method locks internally and returns with the lock released. Dropping
/// a cached GPU frame reaches the texture pool, and the pool reads the cache
/// budget, so the permitted order stays *frame cache → pool → budget*: no
/// method holds this lock while calling into the evaluator.
#[derive(Clone, Default)]
pub struct SharedFrameCache(Arc<Mutex<FrameCache>>);

impl SharedFrameCache {
    /// A cache whose entries are accounted for by `budget`.
    pub fn new(budget: Option<SharedCacheBudget>) -> Self {
        Self(Arc::new(Mutex::new(FrameCache::new(budget))))
    }

    fn lock(&self) -> MutexGuard<'_, FrameCache> {
        // Counters and an entry map: a poisoned lock holds no invariant a
        // later reader can be misled by, so the guard is recovered rather
        // than propagating the panic into the evaluation worker.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The finished frame of `comp` answering `wanted`, if cached.
    pub(crate) fn get(&self, comp: CompId, wanted: &CacheIdentity) -> Option<Arc<dyn NodeData>> {
        self.lock().get(comp, wanted)
    }

    /// Store `value` as the finished frame of `comp` for `identity`.
    ///
    /// `speculative` ranks the entry below anything an interaction paid for
    /// when the budget has to evict (`CACHE-9`).
    pub(crate) fn insert(
        &self,
        comp: CompId,
        identity: CacheIdentity,
        value: Arc<dyn NodeData>,
        speculative: bool,
    ) {
        self.lock().insert(comp, identity, value, speculative);
    }

    /// Whether a request for `wanted` would be answered, without recording a
    /// hit or a miss — the probe read-ahead uses before evaluating a frame.
    pub(crate) fn contains(&self, comp: CompId, wanted: &CacheIdentity) -> bool {
        self.lock().contains(comp, wanted)
    }

    /// Drop the frames `evicted` names that this cache owns.
    pub(crate) fn drop_evicted(&self, evicted: &[Evicted]) {
        self.lock().drop_evicted(evicted);
    }

    /// Take the eviction entries this cache does not own, for routing to the
    /// evaluator. **The caller must act on them** — see
    /// [`crate::eval::Evaluator::drop_evicted`].
    pub(crate) fn take_foreign_evictions(&self) -> Vec<Evicted> {
        self.lock().take_foreign_evictions()
    }

    /// Drop the frames the step from `old` to `new` invalidates, narrowed to
    /// the layers `params` names when the worker could establish one
    /// (`CACHE-7`).
    pub(crate) fn sync_document(
        &self,
        old: Option<&Document>,
        new: &Document,
        params: Option<&[NodeId]>,
    ) {
        self.lock().sync_document(old, new, params);
    }

    /// Drop every frame of `comp`.
    pub fn invalidate_comp(&self, comp: CompId) {
        self.lock().invalidate_comp(comp);
    }

    /// Drop everything.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Frame ranges of `comp` a request built from `wanted` would hit — the
    /// timeline's cache band (`CACHE-6`).
    pub fn cached_ranges(&self, comp: CompId, wanted: &EvalContext) -> Vec<Range<u64>> {
        self.lock().cached_ranges(comp, wanted)
    }

    /// Hit / miss tallies and the bytes held.
    pub fn stats(&self) -> FrameCacheStats {
        self.lock().stats()
    }

    /// A counter that changes exactly when the set of cached frames does.
    ///
    /// The UI thread reads it before recomputing the band: an evaluation
    /// served from the cache added nothing, and walking every entry to
    /// discover that is work the repaint budget should not pay (`CACHE-6`).
    pub fn version(&self) -> u64 {
        self.lock().version()
    }
}

impl std::fmt::Debug for SharedFrameCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedFrameCache")
            .field("stats", &self.stats())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_budget::{CacheBudgetConfig, CacheKind};
    use crate::eval::EvalContext;
    use crate::id::DataTypeId;
    use crate::types::{FrameRate, PixelFormat};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    fn comp_a() -> CompId {
        CompId::new(1)
    }

    fn comp_b() -> CompId {
        CompId::new(2)
    }

    fn ctx(frame: u64) -> EvalContext {
        EvalContext::new(frame, FPS, (4, 4))
    }

    fn frame_value() -> Arc<dyn NodeData> {
        Arc::new(FrameBuffer::from_f32(4, 4, vec![0.5; 4 * 4 * 4]))
    }

    /// A value of a declared size that reports when it is dropped — the
    /// headless stand-in for a GPU frame returning to the texture pool.
    struct Sized {
        bytes: u64,
        gpu: bool,
        drops: Arc<AtomicUsize>,
    }

    impl NodeData for Sized {
        fn data_type_id(&self) -> DataTypeId {
            DataTypeId::FRAME_BUFFER
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn is_gpu_resident(&self) -> bool {
            self.gpu
        }
        fn byte_size(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for Sized {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn budget(vram: u64, ram: u64) -> SharedCacheBudget {
        SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: vram,
            ram_bytes: ram,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        })
    }

    fn document(comps: &[CompId]) -> Document {
        let mut document = Document::default();
        for id in comps {
            document
                .compositions
                .insert(*id, Arc::new(Composition::new(*id, "c", (4, 4), FPS, 100)));
        }
        document
    }

    #[test]
    fn a_stored_frame_answers_the_same_request() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(7));
        assert!(cache.get(comp_a(), &identity).is_none());
        cache.insert(comp_a(), identity, frame_value(), false);
        assert!(cache.get(comp_a(), &identity).is_some());

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses_for(CacheMiss::NoEntry), 1);
    }

    #[test]
    fn another_composition_does_not_answer_for_this_one() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(7));
        cache.insert(comp_a(), identity, frame_value(), false);
        assert!(cache.get(comp_b(), &identity).is_none());
    }

    #[test]
    fn a_different_resolution_misses() {
        let cache = SharedFrameCache::new(None);
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0)),
            frame_value(),
            false,
        );
        let wide = CacheIdentity::of_frame(&EvalContext::new(0, FPS, (8, 8)));
        assert!(cache.get(comp_a(), &wide).is_none());
        assert_eq!(
            cache.stats().misses_for(CacheMiss::ResolutionChanged),
            1,
            "the miss was not attributed to the resolution axis"
        );
    }

    /// The structural guarantee an export depends on: a preview entry
    /// promises `F16` and an `F32` request may not be served it.
    #[test]
    fn an_export_grade_request_does_not_pick_up_a_preview_entry() {
        let cache = SharedFrameCache::new(None);
        let preview = CacheIdentity::of_frame(&ctx(3).with_min_precision(Precision::F16));
        cache.insert(comp_a(), preview, frame_value(), false);

        let export = CacheIdentity::of_frame(&ctx(3).with_min_precision(Precision::F32));
        assert!(cache.get(comp_a(), &export).is_none());
        assert_eq!(
            cache.stats().misses_for(CacheMiss::PrecisionInsufficient),
            1
        );
        // The reverse direction is a hit: a stored F32 covers an F16 floor.
        assert!(cache.get(comp_a(), &preview).is_some());
    }

    #[test]
    fn a_reduced_floor_stores_a_reduced_buffer() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(0).with_min_precision(Precision::F16));
        cache.insert(comp_a(), identity, frame_value(), false);

        let stored = cache.get(comp_a(), &identity).expect("hit");
        let frame = stored.downcast_ref::<FrameBuffer>().expect("frame buffer");
        assert_eq!(frame.format, PixelFormat::RgbaF16);
        // Still readable as f32, and still the same picture.
        assert!(frame.as_f32().iter().all(|v| (*v - 0.5).abs() < 1e-3));
    }

    #[test]
    fn an_f32_floor_stores_the_value_untouched() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(0));
        cache.insert(comp_a(), identity, frame_value(), false);
        let stored = cache.get(comp_a(), &identity).expect("hit");
        assert_eq!(
            stored.downcast_ref::<FrameBuffer>().expect("frame").format,
            PixelFormat::RgbaF32
        );
    }

    // ----- invalidation ----------------------------------------------------

    #[test]
    fn an_edited_composition_loses_every_frame_and_the_others_keep_theirs() {
        let cache = SharedFrameCache::new(None);
        for frame in 0..3 {
            cache.insert(
                comp_a(),
                CacheIdentity::of_frame(&ctx(frame)),
                frame_value(),
                false,
            );
            cache.insert(
                comp_b(),
                CacheIdentity::of_frame(&ctx(frame)),
                frame_value(),
                false,
            );
        }

        let old = document(&[comp_a(), comp_b()]);
        let mut new = old.clone();
        // An edit to A only: B's `Arc` is untouched by structural sharing.
        let mut edited = (*new.compositions[&comp_a()]).clone();
        edited.layers.push_back(crate::composition::Layer::new(
            LayerId::new(1),
            "l",
            crate::graph::Graph::new(),
        ));
        new.compositions.insert(comp_a(), Arc::new(edited));

        cache.sync_document(Some(&old), &new, None);

        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            Vec::<Range<u64>>::new(),
            "the edited composition kept frames"
        );
        assert_eq!(
            cache.cached_ranges(comp_b(), &ctx(0)),
            vec![0..3],
            "an untouched composition lost its frames"
        );
    }

    // ----- scoped invalidation (`CACHE-7`) ---------------------------------

    /// A layer whose network holds `node`, placed at `start` for `duration`
    /// composition frames.
    fn timed_layer(layer: u64, node: u64, start: i64, duration: u64) -> crate::composition::Layer {
        let network = crate::graph::Graph::new()
            .add_node(crate::graph::Node::new(NodeId::new(node), "test.value"))
            .expect("node added");
        crate::composition::Layer::new(LayerId::new(layer), "l", network)
            .with_time(start, 0, duration)
    }

    /// A layer holding a `precomp` node that places `target` inside it.
    fn precomp_layer(
        layer: u64,
        node: u64,
        target: CompId,
        start: i64,
        duration: u64,
    ) -> crate::composition::Layer {
        let network = crate::graph::Graph::new()
            .add_node(
                crate::graph::Node::new(NodeId::new(node), PRECOMP_TYPE_KEY).with_param(
                    PRECOMP_COMP_ID_PARAM,
                    ParameterValue::Int(target.raw() as i32),
                ),
            )
            .expect("node added");
        crate::composition::Layer::new(LayerId::new(layer), "p", network)
            .with_time(start, 0, duration)
    }

    fn document_with(comps: &[(CompId, Vec<crate::composition::Layer>)]) -> Document {
        let mut document = Document::default();
        for (id, layers) in comps {
            let mut comp = Composition::new(*id, "c", (4, 4), FPS, 100);
            for layer in layers {
                comp.layers.push_back(layer.clone());
            }
            document.compositions.insert(*id, Arc::new(comp));
        }
        document
    }

    /// Replace `comp` with a fresh `Arc` — what any edit looks like to the
    /// document diff.
    fn touch_comp(document: &Document, comp: CompId) -> Document {
        let mut next = document.clone();
        let mut edited = (*next.compositions[&comp]).clone();
        edited.name = format!("{}!", edited.name);
        next.compositions.insert(comp, Arc::new(edited));
        next
    }

    fn fill(cache: &SharedFrameCache, comp: CompId, frames: Range<u64>) {
        for frame in frames {
            cache.insert(
                comp,
                CacheIdentity::of_frame(&ctx(frame)),
                frame_value(),
                false,
            );
        }
    }

    /// The point of the unit: an edit that names its nodes only costs the
    /// frames the owning layer actually reaches. The span is the layer's
    /// `[start, start + duration)` with one frame of slack on each side for
    /// shutter samples, so a layer at `[5, 10)` takes `[4, 11)`.
    #[test]
    fn a_layer_edit_drops_its_span_and_leaves_the_rest() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..20);

        let old = document_with(&[(comp_a(), vec![timed_layer(1, 100, 5, 5)])]);
        let new = touch_comp(&old, comp_a());
        cache.sync_document(Some(&old), &new, Some(&[NodeId::new(100)]));

        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            vec![0..4, 11..20],
            "the drop did not follow the layer's span"
        );
    }

    /// The safe default is unchanged: an edit that names nothing still costs
    /// the whole composition (`CACHE-5`).
    #[test]
    fn an_edit_without_named_nodes_still_drops_the_whole_composition() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..20);

        let old = document_with(&[(comp_a(), vec![timed_layer(1, 100, 5, 5)])]);
        let new = touch_comp(&old, comp_a());
        cache.sync_document(Some(&old), &new, None);

        assert_eq!(cache.stats().entries, 0);
    }

    /// A node the document cannot place — a synthetic node of a compiled
    /// shell graph, say — disables narrowing rather than narrowing wrongly.
    #[test]
    fn a_node_that_belongs_to_no_layer_disables_narrowing() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..20);

        let old = document_with(&[(comp_a(), vec![timed_layer(1, 100, 5, 5)])]);
        let new = touch_comp(&old, comp_a());
        cache.sync_document(Some(&old), &new, Some(&[NodeId::new(999)]));

        assert_eq!(cache.stats().entries, 0, "an unplaceable node narrowed");
    }

    /// A composition whose `Arc` moved for a reason none of the named nodes
    /// explains keeps nothing: the hint describes an edit somewhere else.
    #[test]
    fn a_stale_composition_the_hint_does_not_explain_loses_everything() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..20);
        fill(&cache, comp_b(), 0..20);

        let old = document_with(&[
            (comp_a(), vec![timed_layer(1, 100, 5, 5)]),
            (comp_b(), vec![timed_layer(2, 200, 5, 5)]),
        ]);
        // Both compositions changed, but only A's node is named.
        let new = touch_comp(&touch_comp(&old, comp_a()), comp_b());
        cache.sync_document(Some(&old), &new, Some(&[NodeId::new(100)]));

        assert_eq!(cache.cached_ranges(comp_a(), &ctx(0)), vec![0..4, 11..20]);
        assert_eq!(
            cache.cached_ranges(comp_b(), &ctx(0)),
            Vec::<Range<u64>>::new(),
            "the unexplained composition kept frames"
        );
    }

    /// Transform inheritance: a node in a layer's network can drive that
    /// layer's transform channels, and a child layer inherits the result at
    /// frames the parent's own span never covers.
    #[test]
    fn a_parented_child_extends_the_edited_layers_span() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..30);

        let child = timed_layer(2, 200, 20, 5).with_parent(LayerId::new(1));
        let old = document_with(&[(comp_a(), vec![timed_layer(1, 100, 5, 5), child])]);
        let new = touch_comp(&old, comp_a());
        cache.sync_document(Some(&old), &new, Some(&[NodeId::new(100)]));

        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            vec![0..4, 11..19, 26..30],
            "the child layer's span survived its parent's edit"
        );
    }

    /// Propagation through `precomp`: editing a composition placed inside
    /// another one costs the container the frames of the layer holding it.
    /// The parent is not in the document diff at all — its own `Arc` never
    /// moved — so without this the container serves a pre-edit picture.
    #[test]
    fn an_edit_inside_a_precomp_reaches_the_containing_layers_span() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..30);
        fill(&cache, comp_b(), 0..30);

        let old = document_with(&[
            // A contains B through a layer at [20, 25).
            (comp_a(), vec![precomp_layer(1, 100, comp_b(), 20, 5)]),
            (comp_b(), vec![timed_layer(2, 200, 0, 10)]),
        ]);
        let new = touch_comp(&old, comp_b());
        cache.sync_document(Some(&old), &new, Some(&[NodeId::new(200)]));

        assert_eq!(
            cache.cached_ranges(comp_b(), &ctx(0)),
            vec![11..30],
            "the edited composition kept frames inside the layer's span"
        );
        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            vec![0..19, 26..30],
            "the containing composition was not invalidated"
        );
    }

    /// The same propagation with no narrowing available: the container still
    /// loses only the span of the layer holding the child, because that span
    /// is the only place the child can reach.
    #[test]
    fn a_precomp_container_is_invalidated_even_without_a_hint() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..30);

        let old = document_with(&[
            (comp_a(), vec![precomp_layer(1, 100, comp_b(), 20, 5)]),
            (comp_b(), vec![timed_layer(2, 200, 0, 10)]),
        ]);
        let new = touch_comp(&old, comp_b());
        cache.sync_document(Some(&old), &new, None);

        assert_eq!(cache.cached_ranges(comp_a(), &ctx(0)), vec![0..19, 26..30]);
    }

    /// A composition switch moves no `Arc`, so the band survives it — the
    /// property `CACHE-5` records and `CACHE-7` must not break, since a
    /// switch arrives as a `Structural` hint with no narrowing.
    #[test]
    fn an_unchanged_document_drops_nothing() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..5);
        let document = document_with(&[(comp_a(), vec![timed_layer(1, 100, 0, 5)])]);
        cache.sync_document(Some(&document), &document.clone(), None);
        assert_eq!(cache.stats().entries, 5);
    }

    /// Cycles are the validator's job, not the cache's: propagation must
    /// still terminate on a document that never met it.
    #[test]
    fn a_precomp_cycle_does_not_hang_propagation() {
        let cache = SharedFrameCache::new(None);
        fill(&cache, comp_a(), 0..5);
        fill(&cache, comp_b(), 0..5);

        let old = document_with(&[
            (comp_a(), vec![precomp_layer(1, 100, comp_b(), 0, 5)]),
            (comp_b(), vec![precomp_layer(2, 200, comp_a(), 0, 5)]),
        ]);
        let new = touch_comp(&old, comp_a());
        cache.sync_document(Some(&old), &new, None);
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn a_media_asset_change_clears_everything() {
        let cache = SharedFrameCache::new(None);
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0)),
            frame_value(),
            false,
        );

        let old = document(&[comp_a()]);
        let mut new = old.clone();
        new.media_assets.insert(
            "a".into(),
            crate::composition::MediaAssetEntry::from_absolute("/tmp/clip.mov"),
        );
        cache.sync_document(Some(&old), &new, None);
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn the_first_document_invalidates_nothing() {
        let cache = SharedFrameCache::new(None);
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0)),
            frame_value(),
            false,
        );
        cache.sync_document(None, &document(&[comp_a()]), None);
        assert_eq!(cache.stats().entries, 1);
    }

    // ----- budget ----------------------------------------------------------

    #[test]
    fn a_frame_past_the_limit_drops_the_oldest_and_frees_its_value() {
        let drops = Arc::new(AtomicUsize::new(0));
        let budget = budget(0, 100);
        let cache = SharedFrameCache::new(Some(budget.clone()));

        for frame in 0..3u64 {
            cache.insert(
                comp_a(),
                CacheIdentity::of_frame(&ctx(frame)),
                Arc::new(Sized {
                    bytes: 40,
                    gpu: false,
                    drops: drops.clone(),
                }),
                false,
            );
        }

        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "the evicted frame was still being held"
        );
        assert!(
            cache
                .get(comp_a(), &CacheIdentity::of_frame(&ctx(0)))
                .is_none()
        );
        assert!(
            cache
                .get(comp_a(), &CacheIdentity::of_frame(&ctx(2)))
                .is_some()
        );
        assert!(
            budget.stats().used(Tier::Ram) <= 100,
            "the tier stayed over its limit: {:?}",
            budget.stats()
        );
    }

    /// A GPU-resident frame is accounted against VRAM, and evicting it drops
    /// the value — which is what returns the texture to the pool.
    #[test]
    fn a_gpu_resident_frame_is_evicted_from_the_vram_tier() {
        let drops = Arc::new(AtomicUsize::new(0));
        let budget = budget(100, 1_000_000);
        let cache = SharedFrameCache::new(Some(budget.clone()));

        for frame in 0..3u64 {
            cache.insert(
                comp_a(),
                CacheIdentity::of_frame(&ctx(frame)),
                Arc::new(Sized {
                    bytes: 40,
                    gpu: true,
                    drops: drops.clone(),
                }),
                false,
            );
        }

        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(budget.stats().used(Tier::Vram) <= 100);
        assert_eq!(
            budget.stats().used(Tier::Ram),
            0,
            "a GPU-resident frame was charged to host memory"
        );
    }

    /// Read-ahead ranks below interaction (`CACHE-9`): a frame nobody asked
    /// for is given up before one a user waited for, whatever their ages.
    ///
    /// The interactive entry goes in **first**, so plain least-recently-used
    /// order would pick it — the speculative rank is the only thing that
    /// spares it.
    #[test]
    fn a_speculative_frame_is_given_up_before_an_interactive_one() {
        let budget = budget(0, 100);
        let cache = SharedFrameCache::new(Some(budget));
        let sized = || {
            Arc::new(Sized {
                bytes: 40,
                gpu: false,
                drops: Arc::new(AtomicUsize::new(0)),
            }) as Arc<dyn NodeData>
        };

        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(0)), sized(), false);
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(1)), sized(), true);
        // The third entry does not fit: something has to go.
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(2)), sized(), false);

        assert!(
            cache
                .get(comp_a(), &CacheIdentity::of_frame(&ctx(0)))
                .is_some(),
            "the interactive frame was evicted before the speculative one"
        );
        assert!(
            cache
                .get(comp_a(), &CacheIdentity::of_frame(&ctx(1)))
                .is_none(),
            "the speculative frame survived the pressure"
        );
    }

    /// The budget's pots are shared. A frame-cache insert that pushes a node
    /// result out must hand the id back rather than swallow it.
    #[test]
    fn a_foreign_eviction_is_reported_rather_than_dropped_on_the_floor() {
        let budget = budget(0, 100);
        let cache = SharedFrameCache::new(Some(budget.clone()));
        // Someone else's reservation, of the kind the evaluator makes.
        let node_result = budget.reserve(CacheKind::NodeResult(Tier::Ram), 80).0;

        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0)),
            Arc::new(Sized {
                bytes: 40,
                gpu: false,
                drops: Arc::new(AtomicUsize::new(0)),
            }),
            false,
        );

        let foreign = cache.take_foreign_evictions();
        assert_eq!(foreign.len(), 1, "the node result was not named");
        assert_eq!(foreign[0].id, node_result.id());
        assert_eq!(foreign[0].kind, CacheKind::NodeResult(Tier::Ram));
        drop(node_result);
    }

    #[test]
    fn an_eviction_list_for_someone_else_leaves_our_frames_alone() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(0));
        cache.insert(comp_a(), identity, frame_value(), false);
        let elsewhere = budget(0, 1000);
        let foreign = elsewhere.reserve(CacheKind::NodeResult(Tier::Ram), 1).0;
        cache.drop_evicted(&[Evicted {
            id: foreign.id(),
            kind: CacheKind::NodeResult(Tier::Ram),
            bytes: 1,
        }]);
        assert!(cache.get(comp_a(), &identity).is_some());
    }

    // ----- cached_ranges ---------------------------------------------------

    #[test]
    fn contiguous_frames_merge_into_one_range() {
        let cache = SharedFrameCache::new(None);
        for frame in [0u64, 1, 2, 5, 6, 9] {
            cache.insert(
                comp_a(),
                CacheIdentity::of_frame(&ctx(frame)),
                frame_value(),
                false,
            );
        }
        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            vec![0..3, 5..7, 9..10]
        );
    }

    #[test]
    fn ranges_follow_the_precision_and_resolution_they_are_asked_for() {
        let cache = SharedFrameCache::new(None);
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0).with_min_precision(Precision::F16)),
            frame_value(),
            false,
        );
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(1)),
            frame_value(),
            false,
        );

        // An export-grade query sees only the F32 entry.
        assert_eq!(cache.cached_ranges(comp_a(), &ctx(0)), vec![1..2]);
        // A preview-grade query sees both.
        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0).with_min_precision(Precision::F16)),
            vec![0..2]
        );
        // Another resolution has nothing.
        assert_eq!(
            cache.cached_ranges(
                comp_a(),
                &EvalContext::new(0, FPS, (8, 8)).with_min_precision(Precision::F16)
            ),
            Vec::<Range<u64>>::new()
        );
    }

    /// The band must agree with the hit test on **every** axis, not just the
    /// two the first version filtered on. A cache holding entries produced
    /// under other qualities, frame rates and coordinate bases must report
    /// exactly the frames a request would actually be served.
    #[test]
    fn every_reported_frame_is_a_frame_the_cache_would_serve() {
        let cache = SharedFrameCache::new(None);
        let wanted = ctx(0);

        // Frame 0: matches on every axis.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(0)),
            frame_value(),
            false,
        );
        // Frame 1: a different quality stage — a different picture, never a
        // substitute (`Quality` has no order).
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(1).with_quality(crate::eval::Quality::Preview)),
            frame_value(),
            false,
        );
        // Frame 2: another frame rate.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&EvalContext::new(2, FrameRate { num: 24, den: 1 }, (4, 4))),
            frame_value(),
            false,
        );
        // Frame 3: another composition-space coordinate basis.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(3).with_comp_resolution((8, 8))),
            frame_value(),
            false,
        );
        // Frame 4: matches again, so the band is not simply empty.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(4)),
            frame_value(),
            false,
        );

        let reported = cache.cached_ranges(comp_a(), &wanted);
        assert_eq!(reported, vec![0..1, 4..5]);

        // The band's claim, checked against the hit test frame by frame.
        for frame in 0..5u64 {
            let claimed = reported.iter().any(|range| range.contains(&frame));
            let served = cache
                .get(comp_a(), &CacheIdentity::of_frame(&ctx(frame)))
                .is_some();
            assert_eq!(
                claimed, served,
                "frame {frame}: band says {claimed}, the cache says {served}"
            );
        }
    }

    /// A shutter sample sits between frames; the band must not claim the
    /// frame it rounds to, because a request for that frame would miss.
    #[test]
    fn sub_frame_entries_are_not_reported_as_cached_frames() {
        let cache = SharedFrameCache::new(None);
        let mut sub = ctx(4);
        sub.time += 0.25 / FPS.as_f64();
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&sub),
            frame_value(),
            false,
        );
        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            Vec::<Range<u64>>::new()
        );
    }
}

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
//! Whole compositions, driven by the same document diff the evaluator uses:
//! `Document::compositions` holds `Arc<Composition>`, so an untouched
//! composition is pointer-equal across snapshots and an edited one is not.
//! Doing it this way rather than from [`InvalidationHint`] is not a
//! refinement — many document commits pass `InvalidationHint::None` and rely
//! on the evaluator's diff, and a hint-driven frame cache would serve those
//! edits a stale picture.
//!
//! Narrowing invalidation to the frames a layer's time range actually covers
//! is `CACHE-7`. Correctness first.
//!
//! [`InvalidationHint`]: super::InvalidationHint

use crate::cache_budget::{
    CacheKind, Evicted, Reservation, ReservationId, SharedCacheBudget, Tier,
};
use crate::composition::Document;
use crate::eval::{CacheMiss, EvalContext, Precision, TimeKey};
use crate::id::CompId;
use crate::types::{FrameBuffer, NodeData};
use std::collections::HashMap;
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
    fn insert(&mut self, comp: CompId, identity: CacheIdentity, value: Arc<dyn NodeData>) {
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
                let (reservation, evicted) = budget.reserve(CacheKind::Frame(tier), bytes);
                (Some(reservation), evicted)
            }
            None => (None, Vec::new()),
        };
        if let Some(reservation) = &reservation {
            self.by_reservation.insert(reservation.id(), slot);
        }
        self.used[tier_index(tier)] += bytes;
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

    /// Drop the frames the step from `old` to `new` invalidates.
    ///
    /// Reads the same signals [`Evaluator::set_document`] does: media assets
    /// compared as a whole (a path swap is invisible to the composition
    /// diff), then per composition by `Arc` identity, which structural
    /// sharing makes free for untouched ones. `None` for `old` is the first
    /// document the worker ever sees and invalidates nothing — there is no
    /// earlier snapshot the cache could hold frames from.
    ///
    /// [`Evaluator::set_document`]: crate::eval::Evaluator::set_document
    fn sync_document(&mut self, old: Option<&Document>, new: &Document) {
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
        for comp in stale {
            self.invalidate_comp(comp);
        }
    }

    /// Remove `slot`'s entry and its accounting, if it has one.
    fn drop_slot(&mut self, slot: &FrameSlot) {
        let Some(entry) = self.entries.remove(slot) else {
            return;
        };
        if let Some(reservation) = &entry.reservation {
            self.by_reservation.remove(&reservation.id());
        }
        self.used[tier_index(entry.tier)] =
            self.used[tier_index(entry.tier)].saturating_sub(entry.bytes);
    }
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
    pub(crate) fn insert(&self, comp: CompId, identity: CacheIdentity, value: Arc<dyn NodeData>) {
        self.lock().insert(comp, identity, value);
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

    /// Drop the frames the step from `old` to `new` invalidates.
    pub(crate) fn sync_document(&self, old: Option<&Document>, new: &Document) {
        self.lock().sync_document(old, new);
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
    use crate::composition::Composition;
    use crate::eval::EvalContext;
    use crate::id::{DataTypeId, LayerId};
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
        cache.insert(comp_a(), identity, frame_value());
        assert!(cache.get(comp_a(), &identity).is_some());

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses_for(CacheMiss::NoEntry), 1);
    }

    #[test]
    fn another_composition_does_not_answer_for_this_one() {
        let cache = SharedFrameCache::new(None);
        let identity = CacheIdentity::of_frame(&ctx(7));
        cache.insert(comp_a(), identity, frame_value());
        assert!(cache.get(comp_b(), &identity).is_none());
    }

    #[test]
    fn a_different_resolution_misses() {
        let cache = SharedFrameCache::new(None);
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(0)), frame_value());
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
        cache.insert(comp_a(), preview, frame_value());

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
        cache.insert(comp_a(), identity, frame_value());

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
        cache.insert(comp_a(), identity, frame_value());
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
            );
            cache.insert(
                comp_b(),
                CacheIdentity::of_frame(&ctx(frame)),
                frame_value(),
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

        cache.sync_document(Some(&old), &new);

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

    #[test]
    fn a_media_asset_change_clears_everything() {
        let cache = SharedFrameCache::new(None);
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(0)), frame_value());

        let old = document(&[comp_a()]);
        let mut new = old.clone();
        new.media_assets.insert(
            "a".into(),
            crate::composition::MediaAssetEntry::from_absolute("/tmp/clip.mov"),
        );
        cache.sync_document(Some(&old), &new);
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn the_first_document_invalidates_nothing() {
        let cache = SharedFrameCache::new(None);
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(0)), frame_value());
        cache.sync_document(None, &document(&[comp_a()]));
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
        cache.insert(comp_a(), identity, frame_value());
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
        );
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(1)), frame_value());

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
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(0)), frame_value());
        // Frame 1: a different quality stage — a different picture, never a
        // substitute (`Quality` has no order).
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(1).with_quality(crate::eval::Quality::Preview)),
            frame_value(),
        );
        // Frame 2: another frame rate.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&EvalContext::new(2, FrameRate { num: 24, den: 1 }, (4, 4))),
            frame_value(),
        );
        // Frame 3: another composition-space coordinate basis.
        cache.insert(
            comp_a(),
            CacheIdentity::of_frame(&ctx(3).with_comp_resolution((8, 8))),
            frame_value(),
        );
        // Frame 4: matches again, so the band is not simply empty.
        cache.insert(comp_a(), CacheIdentity::of_frame(&ctx(4)), frame_value());

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
        cache.insert(comp_a(), CacheIdentity::of_frame(&sub), frame_value());
        assert_eq!(
            cache.cached_ranges(comp_a(), &ctx(0)),
            Vec::<Range<u64>>::new()
        );
    }
}

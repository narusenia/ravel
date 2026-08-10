// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The decoded-frame cache every media consumer shares
//! (`docs/implementation/cache-plan.md`, unit `CACHE-8`).
//!
//! Before this module the only memory of a decode was per-consumer and one
//! entry deep: the `media` node kept the reader it had opened plus the last
//! still it had read. Any backward scrub, any re-request of the frame already
//! on screen, and any second layer pointing at the same clip paid a flush, a
//! seek to the preceding keyframe and a forward decode of the whole GOP
//! (`issues/high/HIGH-16-no-decoded-frame-cache.md`).
//!
//! [`MediaFrameCache`] is that memory, held **per asset rather than per
//! consumer**: the key names the file, so two layers reading one clip share
//! one decode, and a sequence keeps as many frames as the budget allows
//! instead of exactly one.
//!
//! # What the key says the value depends on
//!
//! ```text
//! (resolved path, input colour space, stream index, frame number)
//! ```
//!
//! - **Path**, because that is the identity of the footage. A relinked asset
//!   builds keys under its new path and therefore cannot hit an entry decoded
//!   from the old one — the stale-hit failure the unit is required to
//!   prevent.
//! - **Input colour space**, because a cached frame is *already converted*.
//!   `CM-2` made the decoder remove the file's transfer function on the way
//!   in ([`crate::decoder::FfmpegDecoder::with_input_color_space`]), so the
//!   same bytes on disk read as sRGB and read as linear are two different
//!   pictures, not one picture and a stale copy of it.
//! - **Stream index**, because that is the argument the memoized call takes
//!   ([`ravel_core::media::MediaReader::decode_video_frame`]). A container
//!   with two video streams would otherwise serve one for the other.
//! - **Frame number**, the position within the stream. Stills and image
//!   sequence frames are single-frame files, so they use frame `0` of stream
//!   `0` and the path alone separates them.
//!
//! The file's modification time is deliberately **not** in the key. Reading
//! it means a `stat` on the decode path for every frame, and the caches this
//! one replaces did not do it either: a file overwritten in place while the
//! project is open keeps serving the frames already decoded from it.
//!
//! # Budget
//!
//! The cache holds no limit of its own. Every entry carries a
//! [`Reservation`] for [`CacheKind::MediaFrame`] and the shared
//! [`CacheBudget`](ravel_core::cache_budget::CacheBudget) decides what has to
//! go — a private eviction policy here would be exactly the second authority
//! `cache-plan.md` exists to prevent.
//!
//! # Eviction lists name entries this cache does not own
//!
//! `CacheKind::MediaFrame`, the evaluator's `CacheKind::NodeResult` and the
//! output-stage `CacheKind::Frame` share the host-memory pot, and the budget
//! orders eviction by tier, not by kind. So a decode can be told to drop an
//! evaluator entry, and an evaluation can be told to drop a decoded frame.
//!
//! **Skipping an id is not a safe default.** `reserve` removes an entry from
//! the accounting *before* returning it, so an id nobody acts on is never
//! offered again: its `Arc` stays resident while the budget believes those
//! bytes are free — the failure `ravel_core::cache_budget`'s module
//! documentation calls the unsafe direction.
//!
//! This cache therefore **parks** what it does not own
//! ([`MediaFrameCache::take_foreign_evictions`]) instead of skipping it, the
//! same shape `CACHE-5` gave the evaluator and the frame cache. The
//! evaluation worker settles the three buffers against each other once per
//! evaluation (`EvalWorkerHooks::reconcile_evictions`), so every id reaches
//! its owner and only ids no cache present owns are discarded.

use ravel_core::cache_budget::{CacheKind, Evicted, Reservation, ReservationId, SharedCacheBudget};
use ravel_core::color::ColorSpace;
use ravel_core::types::{FrameBuffer, NodeData as _};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What a cached decode is a decode *of*. See the module documentation for
/// why each component is part of the identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    /// The resolved path on disk — never the persisted, possibly relative,
    /// asset path.
    pub path: PathBuf,
    /// The colour space the samples were read *from*. The cached frame is
    /// already in the working space.
    pub color_space: ColorSpace,
    /// The stream the frame was decoded from.
    pub stream_index: usize,
    /// The frame's position in that stream.
    pub frame: u64,
}

impl FrameKey {
    /// A frame decoded out of a container stream.
    pub fn video(path: &Path, color_space: ColorSpace, stream_index: usize, frame: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            color_space,
            stream_index,
            frame,
        }
    }

    /// A single-image file — a still, or one frame of an image sequence.
    /// The file holds one picture, so the path carries the position.
    pub fn image(path: &Path, color_space: ColorSpace) -> Self {
        Self::video(path, color_space, 0, 0)
    }
}

struct Entry {
    frame: Arc<FrameBuffer>,
    /// Dropped with the entry, which is what releases the bytes however the
    /// entry goes away.
    reservation: Reservation,
}

struct Inner {
    budget: SharedCacheBudget,
    entries: HashMap<FrameKey, Entry>,
    /// The reverse index an eviction list is resolved through.
    by_reservation: HashMap<ReservationId, FrameKey>,
    /// Ids from an eviction list that belong to another cache, held until
    /// the worker drains them (see the module documentation).
    foreign_evictions: Vec<Evicted>,
}

/// Decoded frames, shared by every consumer that was handed the same handle.
///
/// Cloning shares one cache. The application builds one per evaluation worker
/// and hands it to each `media` processor, which is what makes two layers on
/// one clip a single decode.
///
/// # Locking
///
/// The cache's own lock is taken first and the budget's inside it, never the
/// other way round. Decoding happens outside both: a caller [`Self::get`]s,
/// decodes on a miss, then [`Self::insert`]s, so a slow read never holds the
/// lock. Two consumers racing on the same miss both decode and the second
/// insert replaces the first — cheaper than serializing every decode behind
/// one mutex.
#[derive(Clone)]
pub struct MediaFrameCache(Arc<Mutex<Inner>>);

impl MediaFrameCache {
    /// A cache accounted for by `budget` — the production entry point.
    pub fn new(budget: SharedCacheBudget) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            budget,
            entries: HashMap::new(),
            by_reservation: HashMap::new(),
            foreign_evictions: Vec::new(),
        })))
    }

    /// A cache with a budget of its own, at the canonical default limits.
    ///
    /// For tests, examples and any host that never built a process budget —
    /// the same role `ravel_nodes::shared_texture_pool` plays for the texture
    /// pool. Production code takes [`Self::new`] so that one budget sees
    /// every byte.
    pub fn standalone() -> Self {
        Self::new(SharedCacheBudget::new(Default::default()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Poisoning here means a panic in bookkeeping that holds no invariant
        // a later reader can be misled by; recovering keeps one panicking
        // decode from taking every other consumer of the cache with it.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The frame decoded for `key`, if it is still resident.
    ///
    /// A hit is also a use: the budget's eviction order is
    /// least-recently-*used*, so a frame the playhead keeps returning to
    /// outlives one decoded and abandoned.
    pub fn get(&self, key: &FrameKey) -> Option<Arc<FrameBuffer>> {
        let inner = self.lock();
        let entry = inner.entries.get(key)?;
        let frame = Arc::clone(&entry.frame);
        inner.budget.touch(entry.reservation.id());
        Some(frame)
    }

    /// Remember `frame` as the decode of `key`, dropping whatever the budget
    /// says has to go to make room.
    pub fn insert(&self, key: FrameKey, frame: Arc<FrameBuffer>) {
        let bytes = frame.byte_size();
        let mut inner = self.lock();
        // Replacing an entry releases the old claim first, so re-decoding the
        // same frame does not accumulate reservations.
        inner.remove(&key);

        let (reservation, evicted) = inner.budget.reserve(CacheKind::MediaFrame, bytes);
        inner.by_reservation.insert(reservation.id(), key.clone());
        inner.entries.insert(key, Entry { frame, reservation });

        inner.drop_evicted(&evicted);
    }

    /// Drop the frames `evicted` names, parking the ids this cache does not
    /// own for [`Self::take_foreign_evictions`].
    ///
    /// The counterpart the evaluation worker calls when another cache's
    /// reservation named a decoded frame.
    pub fn drop_evicted(&self, evicted: &[Evicted]) {
        self.lock().drop_evicted(evicted);
    }

    /// Eviction ids this cache was told about but does not own.
    ///
    /// **The caller must hand these to the cache that does** — the budget has
    /// already released their bytes, so an entry nobody drops makes the limit
    /// stop being a limit.
    pub fn take_foreign_evictions(&self) -> Vec<Evicted> {
        std::mem::take(&mut self.lock().foreign_evictions)
    }

    /// Bytes of decoded frames this cache is really holding — what the
    /// budget's accounting for [`CacheKind::MediaFrame`] must agree with.
    pub fn resident_bytes(&self) -> u64 {
        self.lock()
            .entries
            .values()
            .map(|entry| entry.frame.byte_size())
            .sum()
    }

    /// How many frames are resident. Diagnostics and tests.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `key`'s frame is resident, without counting as a use.
    pub fn contains(&self, key: &FrameKey) -> bool {
        self.lock().entries.contains_key(key)
    }
}

impl std::fmt::Debug for MediaFrameCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaFrameCache")
            .field("entries", &self.len())
            .finish()
    }
}

impl Inner {
    /// Forget `key`, releasing its reservation.
    fn remove(&mut self, key: &FrameKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.by_reservation.remove(&entry.reservation.id());
        }
    }

    fn drop_evicted(&mut self, evicted: &[Evicted]) {
        for entry in evicted {
            let Some(victim) = self.by_reservation.remove(&entry.id) else {
                self.foreign_evictions.push(*entry);
                continue;
            };
            self.entries.remove(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::cache_budget::{CacheBudgetConfig, Tier};

    fn budget(ram_bytes: u64) -> SharedCacheBudget {
        SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        })
    }

    fn frame(value: f32) -> Arc<FrameBuffer> {
        Arc::new(FrameBuffer::from_f32(4, 4, vec![value; 4 * 4 * 4]))
    }

    fn key(path: &str, frame: u64) -> FrameKey {
        FrameKey::video(Path::new(path), ColorSpace::SRGB, 0, frame)
    }

    #[test]
    fn a_decoded_frame_is_served_again_without_decoding() {
        let cache = MediaFrameCache::standalone();
        cache.insert(key("/clip.mov", 7), frame(0.5));
        let hit = cache
            .get(&key("/clip.mov", 7))
            .expect("the frame is resident");
        assert!((hit.as_f32()[0] - 0.5).abs() < 1e-6);
    }

    /// Every component of the key separates entries. The colour space is the
    /// one that is easy to get wrong: the stored frame is already converted,
    /// so reading the same file in another space must miss.
    #[test]
    fn every_key_component_separates_entries() {
        let cache = MediaFrameCache::standalone();
        let base = key("/clip.mov", 7);
        cache.insert(base.clone(), frame(0.5));

        assert!(cache.get(&key("/other.mov", 7)).is_none(), "path");
        assert!(cache.get(&key("/clip.mov", 8)).is_none(), "frame number");
        assert!(
            cache
                .get(&FrameKey::video(
                    Path::new("/clip.mov"),
                    ColorSpace::SRGB,
                    1,
                    7
                ))
                .is_none(),
            "stream index"
        );
        assert!(
            cache
                .get(&FrameKey::video(
                    Path::new("/clip.mov"),
                    ColorSpace::LINEAR_REC709,
                    0,
                    7
                ))
                .is_none(),
            "input colour space"
        );
        assert!(cache.get(&base).is_some(), "the original entry survived");
    }

    #[test]
    fn overflowing_the_budget_drops_the_oldest_frame() {
        let one = frame(0.5).byte_size();
        // Room for two frames, not three.
        let cache = MediaFrameCache::new(budget(one * 2 + one / 2));
        cache.insert(key("/clip.mov", 0), frame(0.0));
        cache.insert(key("/clip.mov", 1), frame(0.1));
        cache.insert(key("/clip.mov", 2), frame(0.2));

        assert_eq!(cache.len(), 2);
        assert!(!cache.contains(&key("/clip.mov", 0)), "the oldest stayed");
        assert!(cache.contains(&key("/clip.mov", 1)));
        assert!(cache.contains(&key("/clip.mov", 2)));
    }

    #[test]
    fn a_frame_that_keeps_being_read_outlives_an_abandoned_one() {
        let one = frame(0.5).byte_size();
        let cache = MediaFrameCache::new(budget(one * 2 + one / 2));
        cache.insert(key("/clip.mov", 0), frame(0.0));
        cache.insert(key("/clip.mov", 1), frame(0.1));
        // Frame 0 is the oldest by insertion, but the playhead came back.
        assert!(cache.get(&key("/clip.mov", 0)).is_some());
        cache.insert(key("/clip.mov", 2), frame(0.2));

        assert!(cache.contains(&key("/clip.mov", 0)), "the re-read frame");
        assert!(!cache.contains(&key("/clip.mov", 1)), "the abandoned one");
    }

    #[test]
    fn re_inserting_a_frame_does_not_accumulate_reservations() {
        let shared = budget(u64::MAX);
        let cache = MediaFrameCache::new(shared.clone());
        for _ in 0..4 {
            cache.insert(key("/clip.mov", 0), frame(0.5));
        }
        assert_eq!(cache.len(), 1);
        assert_eq!(shared.stats().entries, 1);
        assert_eq!(shared.stats().used(Tier::Ram), frame(0.5).byte_size());
    }

    /// Another consumer of the same host-memory pot, holding values the way
    /// the evaluator's node cache does: a reservation per value, dropped when
    /// the budget names it, foreign ids parked for their owner.
    ///
    /// A stand-in and not the real `Evaluator` because what is under test is
    /// the *protocol* — park, hand over, drop — and the evaluator's half of
    /// it is pinned by `ravel_core`'s own tests. What no other test can see
    /// is whether both halves together keep real memory inside the limit.
    #[derive(Default)]
    struct Neighbour {
        held: Vec<(Reservation, Arc<FrameBuffer>, u64)>,
        foreign: Vec<Evicted>,
    }

    impl Neighbour {
        fn insert(&mut self, budget: &SharedCacheBudget, value: Arc<FrameBuffer>, bytes: u64) {
            let (reservation, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), bytes);
            self.held.push((reservation, value, bytes));
            self.drop_evicted(&evicted);
        }

        fn drop_evicted(&mut self, evicted: &[Evicted]) {
            for entry in evicted {
                match self
                    .held
                    .iter()
                    .position(|(reservation, _, _)| reservation.id() == entry.id)
                {
                    Some(index) => {
                        self.held.swap_remove(index);
                    }
                    None => self.foreign.push(*entry),
                }
            }
        }

        fn take_foreign(&mut self) -> Vec<Evicted> {
            std::mem::take(&mut self.foreign)
        }

        fn resident_bytes(&self) -> u64 {
            self.held.iter().map(|(_, _, bytes)| bytes).sum()
        }
    }

    /// The invariant the whole hand-off exists for: two consumers of one pot
    /// keep **real** memory inside the limit.
    ///
    /// `reserve` releases an entry's bytes before naming it, so an id its
    /// owner never sees is memory the budget can no longer account for. When
    /// this cache skipped foreign ids, the neighbour's values stayed resident
    /// for good and the total climbed without `used()` ever moving — which is
    /// why the assertion is on the bytes the two consumers still hold, not on
    /// the budget's own counters.
    ///
    /// The two insert at **different rates**, which is what makes the least
    /// recently used entry belong to the other consumer. Alternating one for
    /// one never crosses: each insert evicts the entry the same consumer made
    /// two rounds ago, and the leak stays invisible. The counters at the end
    /// keep the test honest about having exercised both directions.
    #[test]
    fn two_consumers_of_one_pot_keep_real_memory_inside_the_limit() {
        let one = frame(0.5).byte_size();
        let limit = one * 6;
        let shared = budget(limit);
        let cache = MediaFrameCache::new(shared.clone());
        let mut neighbour = Neighbour::default();
        let mut routed = Routed::default();

        for round in 0..32u64 {
            // Two decodes per node result, so neither consumer's own entries
            // are reliably the oldest.
            cache.insert(key("/clip.mov", round * 2), frame(0.1));
            settle(&cache, &mut neighbour, &mut routed);
            cache.insert(key("/clip.mov", round * 2 + 1), frame(0.2));
            settle(&cache, &mut neighbour, &mut routed);
            neighbour.insert(&shared, frame(0.3), one * 2);
            settle(&cache, &mut neighbour, &mut routed);

            let resident = cache.resident_bytes() + neighbour.resident_bytes();
            assert!(
                resident <= limit,
                "round {round}: the two caches hold {resident} bytes of a {limit} byte pot"
            );
            assert_eq!(
                resident,
                shared.stats().used(Tier::Ram),
                "round {round}: the budget's count drifted from what is really held"
            );
        }

        assert!(
            routed.to_neighbour > 0,
            "no decode insert ever evicted a neighbour entry"
        );
        assert!(
            routed.to_cache > 0,
            "no neighbour insert ever evicted a decoded frame"
        );
    }

    /// How many ids each direction of the hand-off actually carried.
    #[derive(Default)]
    struct Routed {
        to_neighbour: usize,
        to_cache: usize,
    }

    /// What the evaluation worker does after every evaluation: hand each
    /// parked id to the cache that owns it.
    fn settle(cache: &MediaFrameCache, neighbour: &mut Neighbour, routed: &mut Routed) {
        let from_cache = cache.take_foreign_evictions();
        let from_neighbour = neighbour.take_foreign();
        routed.to_neighbour += from_cache.len();
        routed.to_cache += from_neighbour.len();

        if !from_cache.is_empty() {
            neighbour.drop_evicted(&from_cache);
        }
        if !from_neighbour.is_empty() {
            cache.drop_evicted(&from_neighbour);
        }
        assert!(
            neighbour.take_foreign().is_empty() && cache.take_foreign_evictions().is_empty(),
            "an eviction id belonged to neither consumer"
        );
    }

    /// Dropping the cache must give every byte back — the entries release
    /// through their reservations, not through an explicit teardown.
    #[test]
    fn dropping_the_cache_releases_its_bytes() {
        let shared = budget(u64::MAX);
        let cache = MediaFrameCache::new(shared.clone());
        cache.insert(key("/clip.mov", 0), frame(0.0));
        cache.insert(key("/clip.mov", 1), frame(0.1));
        assert!(shared.stats().used(Tier::Ram) > 0);
        drop(cache);
        assert_eq!(shared.stats().used(Tier::Ram), 0);
        assert_eq!(shared.stats().entries, 0);
    }
}

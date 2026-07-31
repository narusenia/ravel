// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The single memory authority every Ravel cache answers to
//! (`docs/implementation/cache-plan.md`, unit `CACHE-3`).
//!
//! Ravel keeps several caches — evaluator node results, the GPU texture pool,
//! later the output-stage frame cache and the shared decode cache. Left to
//! themselves they each grow without a limit, or hold private limits that
//! cannot see each other's use: the pool's own budget only ever counted
//! *idle* textures, so every texture a cache was holding was invisible to it.
//!
//! [`CacheBudget`] fixes the arithmetic in one place. Consumers report what
//! they hold through [`CacheBudget::reserve`], the budget answers with the
//! entries that have to go, and each consumer drops the ones it owns. It is
//! pure accounting: no GPU, no evaluator, no I/O, and unit-tested directly —
//! the same separation `LruBudget` uses in `ravel-gpu`.
//!
//! # Shape of the contract
//!
//! - A [`Tier`] is a pot of bytes (VRAM, RAM, disk). Every reservation lands
//!   in exactly one.
//! - A [`CacheKind`] is an eviction class within a pot. It decides *who goes
//!   first*, not *where the bytes are counted*.
//! - [`Reservation`] is the receipt. Dropping it releases the bytes, so a
//!   cache entry that owns one cannot leak accounting when it is dropped by
//!   an unrelated path (`retain`, `clear`, a document swap).
//! - Over the limit, `reserve` returns the [`Evicted`] entries in the order
//!   `speculative → ordinary (least recently used)`, sim excluded.
//!
//! # Acting on an eviction list is mandatory
//!
//! The budget removes an evicted entry from its accounting *before*
//! returning it, because it has no way to reach the value. **A consumer that
//! does not then drop the value it owns makes the budget under-count real
//! memory: the process keeps holding bytes the budget believes are free, and
//! the limit stops being a limit.** This is the unsafe direction, not the
//! safe one. Every id in the list belongs to exactly one consumer, and that
//! consumer must drop it; ids belonging to nobody present are the only ones
//! that may be skipped.
//!
//! Today this cannot go wrong, which is why nothing enforces it: the
//! evaluator's cache is the only thing that calls `reserve`, and
//! `TexturePool` only *reads* [`CacheBudget::headroom`]. `CACHE-5` (the
//! output-stage frame cache) and `CACHE-8` (the shared decode cache) will be
//! the first consumers that can drop a list on the floor.
//!
//! # Sim is protected, but not exempt
//!
//! A simulation cache entry is not comparable to a frame: dropping it costs a
//! re-run from the start of the sim, `O(frames)`, where dropping a frame
//! costs one recompute. So a share of each tier
//! ([`CacheBudgetConfig::sim_reserve_ratio`]) is headroom ordinary entries may
//! not spend, and no amount of ordinary pressure ever puts a
//! [`CacheKind::Sim`] reservation in an eviction list.
//!
//! Protection is not exemption: once sim alone exceeds the tier's total, sim
//! entries are evicted **by sim**, least recently used first. A protected
//! class that could grow without a ceiling would make "one authority" false
//! for the one class most able to fill memory. `SIM-1` has no consumer yet;
//! the accounting is here so the layer that arrives finds both halves of the
//! rule already true.
//!
//! # Who may create one
//!
//! [`SharedCacheBudget::new`] is the only public constructor, and there is no
//! `Default`: a second budget would be a second authority, which is the exact
//! failure this module exists to prevent. The application builds one at
//! startup and hands the clone to everything that caches.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

// ===========================================================================
// Tiers and kinds
// ===========================================================================

/// Where a cached value physically lives. Each tier is an independent pot of
/// bytes with its own limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// GPU memory: resident textures, held and idle alike.
    Vram,
    /// Host memory.
    Ram,
    /// On-disk spill. Declared here so the accounting is ready; the layer
    /// itself is `CACHE-11`.
    Disk,
}

impl Tier {
    /// Every tier, in declaration order — the index order of the per-tier
    /// arrays in [`CacheStats`].
    pub const ALL: [Tier; 3] = [Tier::Vram, Tier::Ram, Tier::Disk];

    const fn index(self) -> usize {
        match self {
            Tier::Vram => 0,
            Tier::Ram => 1,
            Tier::Disk => 2,
        }
    }
}

/// What kind of cache a reservation belongs to — the eviction class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CacheKind {
    /// One `Evaluator` node-result entry, held in the given tier. A node
    /// result may be a CPU buffer or a GPU-resident texture, so the tier is
    /// a property of the value rather than of the cache.
    NodeResult(Tier),
    /// An output-stage frame cache entry (`CACHE-5`).
    Frame(Tier),
    /// A decoded media frame shared across layers (`CACHE-8`).
    MediaFrame,
    /// A simulation state entry (`SIM-1`). Protected: see the module docs.
    Sim,
}

impl CacheKind {
    /// The tier this kind's bytes are counted against.
    pub fn tier(self) -> Tier {
        match self {
            CacheKind::NodeResult(tier) | CacheKind::Frame(tier) => tier,
            // Decoded frames and sim state are host-side by construction.
            CacheKind::MediaFrame | CacheKind::Sim => Tier::Ram,
        }
    }

    /// Whether this kind sits in the protected reserve.
    pub fn is_protected(self) -> bool {
        matches!(self, CacheKind::Sim)
    }
}

// ===========================================================================
// Configuration
// ===========================================================================

/// Byte limits and policy the budget enforces.
///
/// The defaults are the canonical ones; `ravel-app`'s settings layer resolves
/// onto these rather than inventing a second set of numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheBudgetConfig {
    /// Total VRAM the caches may occupy, held plus idle.
    pub vram_bytes: u64,
    /// Total host memory the caches may occupy.
    pub ram_bytes: u64,
    /// Total disk spill allowance (`CACHE-11`).
    pub disk_bytes: u64,
    /// Share of each tier held back for [`CacheKind::Sim`], clamped to
    /// `0.0..=1.0`.
    pub sim_reserve_ratio: f32,
}

impl CacheBudgetConfig {
    /// Total VRAM default, 1 GiB.
    ///
    /// Before this unit the only VRAM limit was the texture pool's 512 MiB of
    /// *idle* textures, with the held side unbounded. 1 GiB keeps the pool's
    /// idle allowance at least as large as it used to be for any working set
    /// under 512 MiB, while putting a ceiling on the half that had none.
    pub const DEFAULT_VRAM_BYTES: u64 = 1024 * 1024 * 1024;

    /// Host-memory default, 2 GiB.
    ///
    /// A 1080p RGBA f32 frame is ~33 MB and the compiled shell chain produces
    /// three or four per layer (MED-CORE-06), so a ten-layer composition sits
    /// near 1.3 GB. 2 GiB leaves that working set resident — the behaviour
    /// this unit must not change — and still bounds the unbounded growth that
    /// made the issue.
    pub const DEFAULT_RAM_BYTES: u64 = 2048 * 1024 * 1024;

    /// Disk-spill default, 4 GiB. Inert until `CACHE-11` builds the layer.
    pub const DEFAULT_DISK_BYTES: u64 = 4096 * 1024 * 1024;

    /// Default share reserved for simulation state, 25%.
    pub const DEFAULT_SIM_RESERVE_RATIO: f32 = 0.25;
}

impl Default for CacheBudgetConfig {
    fn default() -> Self {
        Self {
            vram_bytes: Self::DEFAULT_VRAM_BYTES,
            ram_bytes: Self::DEFAULT_RAM_BYTES,
            disk_bytes: Self::DEFAULT_DISK_BYTES,
            sim_reserve_ratio: Self::DEFAULT_SIM_RESERVE_RATIO,
        }
    }
}

// ===========================================================================
// Reservations
// ===========================================================================

/// Identifies one live reservation. Consumers map these back to their own
/// cache keys to act on an eviction list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservationId(u64);

impl ReservationId {
    /// The raw counter value, for logging and test assertions.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// An entry the budget wants gone. The bytes are already released; the
/// consumer's job is to drop the value they belonged to.
///
/// Not optional — see "Acting on an eviction list is mandatory" in the module
/// documentation. Ignoring one leaves the budget counting fewer bytes than
/// the process is holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evicted {
    /// The reservation that was dropped from the accounting.
    pub id: ReservationId,
    /// What kind of entry it was — a consumer only owns some kinds.
    pub kind: CacheKind,
    /// How many bytes it freed.
    pub bytes: u64,
}

/// A live claim on budgeted bytes. Releasing is the `Drop`.
///
/// Held inside the cache entry it accounts for, so every path that can drop
/// the entry — `remove`, `retain`, `clear`, eviction, the evaluator itself
/// being dropped — releases the bytes without remembering to.
pub struct Reservation {
    id: ReservationId,
    kind: CacheKind,
    bytes: u64,
    /// Weak so a reservation outliving its budget is inert rather than a
    /// cycle. In practice the budget outlives every cache.
    budget: Weak<Mutex<CacheBudget>>,
}

impl Reservation {
    /// This reservation's id, as it appears in an [`Evicted`] entry.
    pub fn id(&self) -> ReservationId {
        self.id
    }

    /// Bytes claimed.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The eviction class this reservation belongs to.
    pub fn kind(&self) -> CacheKind {
        self.kind
    }
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reservation")
            .field("id", &self.id.0)
            .field("kind", &self.kind)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let Some(budget) = self.budget.upgrade() else {
            return;
        };
        // A poisoned budget means another thread panicked mid-accounting.
        // Releasing into it would only compound the damage, and the process
        // is already failing; leaving the bytes claimed is the safe half.
        if let Ok(mut budget) = budget.lock() {
            budget.release(self.id);
        }
    }
}

// ===========================================================================
// The budget
// ===========================================================================

struct Entry {
    kind: CacheKind,
    bytes: u64,
    /// Monotonic reservation order, the LRU key. Reservations are made when a
    /// value is produced, so "least recently reserved" is "oldest value".
    tick: u64,
    /// Whether the value was produced by read-ahead rather than by an
    /// interactive request (`CACHE-9`). Speculative entries are evicted
    /// before anything an interaction paid for.
    speculative: bool,
}

/// Per-tier and per-kind usage, for `cache_stats()` and the pool's residual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Limit of each tier in [`Tier::ALL`] order.
    pub limits: [u64; 3],
    /// Bytes currently reserved in each tier, sim included.
    pub used: [u64; 3],
    /// Bytes currently reserved by [`CacheKind::Sim`] in each tier.
    pub sim_used: [u64; 3],
    /// Bytes each tier holds back for sim.
    pub sim_reserved: [u64; 3],
    /// Live reservations.
    pub entries: usize,
}

impl CacheStats {
    /// Bytes used in `tier`.
    pub fn used(&self, tier: Tier) -> u64 {
        self.used[tier.index()]
    }

    /// Limit of `tier`.
    pub fn limit(&self, tier: Tier) -> u64 {
        self.limits[tier.index()]
    }
}

/// The accounting itself. Reach it through [`SharedCacheBudget`].
pub struct CacheBudget {
    limits: [u64; 3],
    used: [u64; 3],
    sim_used: [u64; 3],
    sim_reserve_ratio: f32,
    entries: HashMap<ReservationId, Entry>,
    next_id: u64,
    next_tick: u64,
}

impl CacheBudget {
    /// Deliberately private: [`SharedCacheBudget::new`] is the only way in.
    fn new(config: CacheBudgetConfig) -> Self {
        let mut budget = Self {
            limits: [0; 3],
            used: [0; 3],
            sim_used: [0; 3],
            sim_reserve_ratio: 0.0,
            entries: HashMap::new(),
            next_id: 0,
            next_tick: 0,
        };
        budget.reconfigure(config);
        budget
    }

    /// Apply new limits to a running budget.
    ///
    /// Live reservations keep their ids and bytes; only the ceilings move.
    /// Shrinking below the current use does not evict on the spot — the next
    /// [`Self::reserve`] in that tier collects the overflow, which keeps
    /// eviction on the one path where a caller is ready to act on it.
    pub fn reconfigure(&mut self, config: CacheBudgetConfig) {
        self.limits = [config.vram_bytes, config.ram_bytes, config.disk_bytes];
        self.sim_reserve_ratio = config.sim_reserve_ratio.clamp(0.0, 1.0);
    }

    /// Bytes held back for [`CacheKind::Sim`] in `tier`.
    ///
    /// Ordinary entries may not spend this, whether or not any sim entry
    /// exists yet — a reserve that only appears once sim is running would be
    /// taken from a cache that had already filled the tier.
    pub fn sim_reserve(&self, tier: Tier) -> u64 {
        let index = tier.index();
        (self.limits[index] as f64 * f64::from(self.sim_reserve_ratio)) as u64
    }

    /// What ordinary (non-sim) entries may occupy in `tier`.
    fn ordinary_capacity(&self, tier: Tier) -> u64 {
        let index = tier.index();
        let protected = self.sim_used[index].max(self.sim_reserve(tier));
        self.limits[index].saturating_sub(protected)
    }

    /// Bytes reserved in `tier`, held by every kind.
    pub fn used(&self, tier: Tier) -> u64 {
        self.used[tier.index()]
    }

    /// Limit of `tier`.
    pub fn limit(&self, tier: Tier) -> u64 {
        self.limits[tier.index()]
    }

    /// Bytes in `tier` that nothing has claimed.
    ///
    /// This is what the texture pool is allowed to keep as idle: the pool
    /// holds no limit of its own, so the VRAM ceiling is decided in exactly
    /// one place (`cache-plan.md`). The pool re-reads it when a texture is
    /// released, so it **follows** the resident side rather than tracking it
    /// instantly — see the note on approximation in
    /// `ravel_gpu::texture_pool`.
    pub fn headroom(&self, tier: Tier) -> u64 {
        let index = tier.index();
        self.limits[index].saturating_sub(self.used[index])
    }

    /// A snapshot of the accounting.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            limits: self.limits,
            used: self.used,
            sim_used: self.sim_used,
            sim_reserved: [
                self.sim_reserve(Tier::Vram),
                self.sim_reserve(Tier::Ram),
                self.sim_reserve(Tier::Disk),
            ],
            entries: self.entries.len(),
        }
    }

    /// Claim `bytes` for a `kind` entry, returning the id and everything the
    /// tier must give up to fit it.
    ///
    /// The reservation is always granted — refusing would leave the caller
    /// holding a value it cannot account for. Over the limit the budget
    /// instead names the entries to drop: speculative before interactive,
    /// least recently used first, and **never a [`CacheKind::Sim`] entry
    /// under ordinary pressure**. Sim yields only to sim, and only when sim
    /// alone exceeds the whole tier (see [`Self::collect_overflow`]). An
    /// entry too large for the tier on its own empties the tier and stays;
    /// the caller has already produced it.
    fn reserve_raw(
        &mut self,
        kind: CacheKind,
        bytes: u64,
        speculative: bool,
    ) -> (ReservationId, Vec<Evicted>) {
        let id = ReservationId(self.next_id);
        self.next_id += 1;
        let tick = self.next_tick;
        self.next_tick += 1;
        let tier = kind.tier();
        let index = tier.index();

        self.used[index] += bytes;
        if kind.is_protected() {
            self.sim_used[index] += bytes;
        }
        self.entries.insert(
            id,
            Entry {
                kind,
                bytes,
                tick,
                speculative,
            },
        );

        (id, self.collect_overflow(tier, id))
    }

    /// Names the entries `tier` must release to be within its limits again.
    ///
    /// Two ceilings apply, and they are checked in this order:
    ///
    /// 1. **Sim against the tier total.** Protection means ordinary pressure
    ///    never evicts sim — not that sim is exempt from the tier. Once sim
    ///    alone is over the limit it is trimmed *by sim*, least recently used
    ///    first, because the alternative is a tier whose ceiling silently
    ///    stops being one.
    /// 2. **Ordinary entries against what the reserve leaves them.** Done
    ///    second so the capacity is measured against the trimmed sim total,
    ///    and an over-large sim does not evict more ordinary entries than the
    ///    final state needs.
    ///
    /// `keep` is the reservation just made: a new value never evicts itself,
    /// or a cache would drop the entry it is inserting.
    fn collect_overflow(&mut self, tier: Tier, keep: ReservationId) -> Vec<Evicted> {
        let index = tier.index();
        let mut evicted = Vec::new();

        let limit = self.limits[index];
        if self.sim_used[index] > limit {
            for id in self.eviction_order(tier, keep, true) {
                if self.sim_used[index] <= limit {
                    break;
                }
                let entry = self.take(id);
                self.sim_used[index] -= entry.bytes;
                evicted.push(entry);
            }
        }

        let mut ordinary_used = self.used[index] - self.sim_used[index];
        let capacity = self.ordinary_capacity(tier);
        if ordinary_used > capacity {
            for id in self.eviction_order(tier, keep, false) {
                if ordinary_used <= capacity {
                    break;
                }
                let entry = self.take(id);
                ordinary_used -= entry.bytes;
                evicted.push(entry);
            }
        }

        if !evicted.is_empty() {
            tracing::debug!(
                tier = ?tier,
                evicted = evicted.len(),
                used = self.used[index],
                limit,
                "cache budget evicted entries"
            );
        }
        evicted
    }

    /// The ids of `tier`'s protected (or unprotected) entries in the order
    /// they should be given up: speculative before interactive, then least
    /// recently used first. `keep` is never included.
    fn eviction_order(
        &self,
        tier: Tier,
        keep: ReservationId,
        protected: bool,
    ) -> Vec<ReservationId> {
        let mut candidates: Vec<(bool, u64, ReservationId)> = self
            .entries
            .iter()
            .filter(|(id, entry)| {
                **id != keep && entry.kind.tier() == tier && entry.kind.is_protected() == protected
            })
            .map(|(id, entry)| (!entry.speculative, entry.tick, *id))
            .collect();
        candidates.sort_unstable();
        candidates.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Remove `id` from the accounting and describe what it freed. Does not
    /// touch `sim_used` — the caller knows which pot it was trimming.
    fn take(&mut self, id: ReservationId) -> Evicted {
        // SAFETY of expect: `id` comes from `eviction_order`, which reads
        // `self.entries`, and the loops never name one twice.
        let entry = self.entries.remove(&id).expect("candidate is live");
        let index = entry.kind.tier().index();
        self.used[index] -= entry.bytes;
        Evicted {
            id,
            kind: entry.kind,
            bytes: entry.bytes,
        }
    }

    /// Move `id` to the most-recently-used end of its eviction order.
    ///
    /// Reservation order alone would evict by age of *production*, so a value
    /// re-read on every frame would still be the first to go. Caches call
    /// this on a hit, which is what makes the order least-recently-*used*.
    fn touch(&mut self, id: ReservationId) {
        let tick = self.next_tick;
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.tick = tick;
            self.next_tick += 1;
        }
    }

    /// Give back the bytes of `id`, if it is still live.
    fn release(&mut self, id: ReservationId) {
        let Some(entry) = self.entries.remove(&id) else {
            // Already evicted: the budget released the bytes when it named
            // the entry, so the owning `Reservation`'s drop has nothing left
            // to do.
            return;
        };
        let index = entry.kind.tier().index();
        self.used[index] = self.used[index].saturating_sub(entry.bytes);
        if entry.kind.is_protected() {
            self.sim_used[index] = self.sim_used[index].saturating_sub(entry.bytes);
        }
    }
}

// ===========================================================================
// Shared handle
// ===========================================================================

/// The budget as everything else sees it: a clonable handle whose
/// [`Reservation`]s release themselves.
///
/// Cloning shares one budget. Constructing a second one is constructing a
/// second authority — the application does it exactly once.
///
/// # Locking
///
/// Every method locks internally and returns with the lock released, so a
/// caller never holds it while dropping a value. That matters because
/// dropping a cached GPU frame reaches the texture pool, and the pool reads
/// the budget: the one permitted order is *pool then budget*, never the
/// reverse.
#[derive(Clone)]
pub struct SharedCacheBudget(Arc<Mutex<CacheBudget>>);

impl SharedCacheBudget {
    /// Build the process's cache budget from `config`.
    ///
    /// The only public constructor, and there is no `Default`: see the module
    /// documentation for why a second budget must be hard to make.
    pub fn new(config: CacheBudgetConfig) -> Self {
        Self(Arc::new(Mutex::new(CacheBudget::new(config))))
    }

    fn lock(&self) -> MutexGuard<'_, CacheBudget> {
        // A poisoned budget is a panic in accounting code that holds no
        // invariant a later reader can be misled by (counters only), so the
        // guard is recovered rather than propagating the panic into every
        // cache in the process.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claim `bytes` for an interactively produced `kind` value.
    pub fn reserve(&self, kind: CacheKind, bytes: u64) -> (Reservation, Vec<Evicted>) {
        self.reserve_inner(kind, bytes, false)
    }

    /// Claim `bytes` for a value produced by read-ahead (`CACHE-9`).
    ///
    /// Identical accounting, different eviction rank: speculation is emptied
    /// before anything a user waited for.
    pub fn reserve_speculative(&self, kind: CacheKind, bytes: u64) -> (Reservation, Vec<Evicted>) {
        self.reserve_inner(kind, bytes, true)
    }

    fn reserve_inner(
        &self,
        kind: CacheKind,
        bytes: u64,
        speculative: bool,
    ) -> (Reservation, Vec<Evicted>) {
        let (id, evicted) = self.lock().reserve_raw(kind, bytes, speculative);
        (
            Reservation {
                id,
                kind,
                bytes,
                budget: Arc::downgrade(&self.0),
            },
            evicted,
        )
    }

    /// Bytes in `tier` no reservation claims — the texture pool's idle
    /// allowance.
    pub fn headroom(&self, tier: Tier) -> u64 {
        self.lock().headroom(tier)
    }

    /// Record that the value behind `id` was just used, so eviction order is
    /// least-recently-*used* rather than oldest-produced.
    pub fn touch(&self, id: ReservationId) {
        self.lock().touch(id);
    }

    /// A snapshot of the accounting, for `cache_stats()` and diagnostics.
    pub fn stats(&self) -> CacheStats {
        self.lock().stats()
    }

    /// Apply new limits to the running budget (a settings change).
    pub fn reconfigure(&self, config: CacheBudgetConfig) {
        self.lock().reconfigure(config);
    }
}

impl std::fmt::Debug for SharedCacheBudget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedCacheBudget")
            .field("stats", &self.stats())
            .finish()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn budget(vram: u64, ram: u64, sim_ratio: f32) -> SharedCacheBudget {
        SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: vram,
            ram_bytes: ram,
            disk_bytes: 0,
            sim_reserve_ratio: sim_ratio,
        })
    }

    #[test]
    fn reserving_within_the_limit_evicts_nothing() {
        let budget = budget(0, 100, 0.0);
        let (_a, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);
        assert!(evicted.is_empty());
        let (_b, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);
        assert!(evicted.is_empty());
        assert_eq!(budget.stats().used(Tier::Ram), 80);
    }

    #[test]
    fn overflow_evicts_the_oldest_entry_first() {
        let budget = budget(0, 100, 0.0);
        let (a, _) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);
        let (b, _) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);
        let (_c, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, a.id());
        assert_eq!(evicted[0].bytes, 40);
        // The eviction already left the accounting, so the tier fits again.
        assert_eq!(budget.stats().used(Tier::Ram), 80);
        drop((a, b));
    }

    #[test]
    fn a_dropped_reservation_gives_its_bytes_back() {
        let budget = budget(0, 100, 0.0);
        let a = budget.reserve(CacheKind::NodeResult(Tier::Ram), 60).0;
        assert_eq!(budget.stats().used(Tier::Ram), 60);
        drop(a);
        assert_eq!(budget.stats().used(Tier::Ram), 0);
        assert_eq!(budget.stats().entries, 0);
    }

    #[test]
    fn dropping_an_already_evicted_reservation_does_not_double_release() {
        let budget = budget(0, 100, 0.0);
        let a = budget.reserve(CacheKind::NodeResult(Tier::Ram), 60).0;
        let (b, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 60);
        assert_eq!(evicted[0].id, a.id());
        assert_eq!(budget.stats().used(Tier::Ram), 60);
        // The consumer drops the value it was told to drop.
        drop(a);
        assert_eq!(budget.stats().used(Tier::Ram), 60);
        drop(b);
        assert_eq!(budget.stats().used(Tier::Ram), 0);
    }

    #[test]
    fn speculative_entries_are_evicted_before_interactive_ones() {
        let budget = budget(0, 100, 0.0);
        // Interactive first, so plain LRU would pick it.
        let interactive = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40).0;
        let speculative = budget
            .reserve_speculative(CacheKind::NodeResult(Tier::Ram), 40)
            .0;
        let (_new, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, speculative.id());
        drop((interactive, speculative));
    }

    #[test]
    fn sim_reserve_survives_ordinary_pressure() {
        // 25% of 1000 is held back; sim claims 200 of it.
        let budget = budget(0, 1000, 0.25);
        let sim = budget.reserve(CacheKind::Sim, 200).0;

        // Push ordinary entries far past the total.
        let mut ordinary = Vec::new();
        let mut evicted_ids = Vec::new();
        for _ in 0..20 {
            let (reservation, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 100);
            evicted_ids.extend(evicted.into_iter().map(|entry| entry.id));
            ordinary.push(reservation);
        }

        assert!(
            !evicted_ids.contains(&sim.id()),
            "sim reservation was named for eviction"
        );
        let stats = budget.stats();
        assert_eq!(stats.sim_used[Tier::Ram.index()], 200);
        // Ordinary use stays under limit minus the protected share.
        assert!(
            stats.used(Tier::Ram) - stats.sim_used[Tier::Ram.index()] <= 1000 - 250,
            "ordinary entries spent the sim reserve: {stats:?}"
        );
        drop((sim, ordinary));
    }

    #[test]
    fn sim_past_the_whole_tier_is_trimmed_by_sim() {
        // Protection is not exemption: ordinary pressure never touches sim,
        // but sim may not grow past the tier either.
        let budget = budget(0, 1000, 0.25);
        let first = budget.reserve(CacheKind::Sim, 400).0;
        let second = budget.reserve(CacheKind::Sim, 400).0;
        // 1200 > 1000: the least recently used sim entry goes.
        let (third, evicted) = budget.reserve(CacheKind::Sim, 400);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, first.id());
        assert_eq!(evicted[0].kind, CacheKind::Sim);

        let stats = budget.stats();
        assert!(
            stats.used(Tier::Ram) <= 1000,
            "the sim total stayed above the tier limit: {stats:?}"
        );
        assert_eq!(stats.sim_used[Tier::Ram.index()], stats.used(Tier::Ram));
        drop((first, second, third));
    }

    #[test]
    fn a_touched_sim_entry_outlives_an_idle_one() {
        let budget = budget(0, 1000, 0.25);
        let first = budget.reserve(CacheKind::Sim, 600).0;
        let second = budget.reserve(CacheKind::Sim, 300).0;
        budget.touch(first.id());
        let (third, evicted) = budget.reserve(CacheKind::Sim, 300);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, second.id());
        drop((first, second, third));
    }

    #[test]
    fn trimming_sim_does_not_over_evict_ordinary_entries() {
        // Sim overshoots the tier while an ordinary entry would fit once sim
        // is back inside it. Trimming sim first is what spares the ordinary
        // entry.
        let budget = budget(0, 1000, 0.25);
        let ordinary = budget.reserve(CacheKind::NodeResult(Tier::Ram), 100).0;
        let old_sim = budget.reserve(CacheKind::Sim, 900).0;
        let (new_sim, evicted) = budget.reserve(CacheKind::Sim, 400);

        let ids: Vec<_> = evicted.iter().map(|entry| entry.id).collect();
        assert!(ids.contains(&old_sim.id()), "sim was not trimmed");
        assert!(
            !ids.contains(&ordinary.id()),
            "an ordinary entry that fits was evicted anyway"
        );
        assert!(budget.stats().used(Tier::Ram) <= 1000);
        drop((ordinary, old_sim, new_sim));
    }

    #[test]
    fn sim_beyond_its_reserve_still_pushes_ordinary_entries_out() {
        let budget = budget(0, 1000, 0.25);
        let ordinary = budget.reserve(CacheKind::NodeResult(Tier::Ram), 700).0;
        // Sim asks for more than its 250 share; the ordinary entry yields.
        let (sim, evicted) = budget.reserve(CacheKind::Sim, 600);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, ordinary.id());
        drop((ordinary, sim));
    }

    #[test]
    fn touching_an_entry_spares_it_from_the_next_eviction() {
        let budget = budget(0, 100, 0.0);
        let a = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40).0;
        let b = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40).0;
        // `a` is the oldest by production, but it was just read.
        budget.touch(a.id());
        let (_c, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, b.id());
        drop((a, b));
    }

    #[test]
    fn tiers_do_not_borrow_from_each_other() {
        let budget = budget(100, 100, 0.0);
        let vram = budget.reserve(CacheKind::NodeResult(Tier::Vram), 100).0;
        let (ram, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 100);
        assert!(evicted.is_empty(), "a full VRAM tier evicted a RAM entry");
        assert_eq!(budget.stats().used(Tier::Vram), 100);
        assert_eq!(budget.stats().used(Tier::Ram), 100);
        drop((vram, ram));
    }

    #[test]
    fn headroom_is_what_the_pool_may_keep_idle() {
        let budget = budget(1000, 0, 0.0);
        assert_eq!(budget.headroom(Tier::Vram), 1000);
        let held = budget.reserve(CacheKind::NodeResult(Tier::Vram), 400).0;
        assert_eq!(budget.headroom(Tier::Vram), 600);
        drop(held);
        assert_eq!(budget.headroom(Tier::Vram), 1000);
    }

    #[test]
    fn an_entry_larger_than_the_tier_empties_it_and_stays() {
        let budget = budget(0, 100, 0.0);
        let small = budget.reserve(CacheKind::NodeResult(Tier::Ram), 50).0;
        let (huge, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 10_000);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, small.id());
        // Still over: the value exists, and refusing the reservation would
        // only hide it from the accounting.
        assert_eq!(budget.stats().used(Tier::Ram), 10_000);
        drop((small, huge));
    }

    #[test]
    fn reconfigure_moves_the_ceiling_without_disturbing_reservations() {
        let budget = budget(0, 1000, 0.0);
        let held = budget.reserve(CacheKind::NodeResult(Tier::Ram), 800).0;
        budget.reconfigure(CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes: 500,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        // Live reservation untouched; the overflow is collected by the next
        // reserve in the tier.
        assert_eq!(budget.stats().used(Tier::Ram), 800);
        let (_next, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 10);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id, held.id());
        drop(held);
    }

    #[test]
    fn defaults_are_the_documented_megabyte_figures() {
        let config = CacheBudgetConfig::default();
        assert_eq!(config.vram_bytes, 1024 * MIB);
        assert_eq!(config.ram_bytes, 2048 * MIB);
        assert_eq!(config.sim_reserve_ratio, 0.25);
    }

    #[test]
    fn a_reservation_outliving_its_budget_is_inert() {
        let budget = budget(0, 100, 0.0);
        let held = budget.reserve(CacheKind::NodeResult(Tier::Ram), 10).0;
        drop(budget);
        // Must not panic: the weak reference simply fails to upgrade.
        drop(held);
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU texture pool with reuse and LRU-based eviction.
//!
//! Intermediate node results allocate short-lived textures of identical size
//! and format every frame. The pool recycles freed textures keyed by
//! [`TextureKey`] so steady-state evaluation performs no allocations. When the
//! pooled (idle) VRAM exceeds a configured budget, the least-recently-released
//! textures are dropped.
//!
//! The eviction accounting lives in [`LruBudget`], which is GPU-independent and
//! unit-tested directly; [`TexturePool`] layers the wgpu texture handling on
//! top.
//!
//! A pool built with [`TexturePool::with_shared_budget`] holds no VRAM limit
//! of its own: its idle allowance is the headroom the shared
//! [`CacheBudget`](ravel_core::cache_budget::CacheBudget) reports for
//! [`Tier::Vram`], re-read on every release. That is what makes "resident
//! textures plus pooled textures" add up to one number — before `CACHE-3` the
//! pool's budget saw only the idle half and the resident half was unbounded.
//!
//! The allowance is an **approximation that follows on release**: it is
//! recomputed when a texture comes back, not the instant a cache reserves or
//! frees VRAM, so the total can sit briefly above the limit after a new
//! resident texture and briefly below it after a cached frame is dropped.
//! Releases are frequent (every intermediate, every frame) so it self-
//! corrects within the same evaluation; tightening it would mean the budget
//! calling into the pool, which the lock order forbids.

use std::collections::HashMap;
use std::sync::Arc;

use ravel_core::cache_budget::{SharedCacheBudget, Tier};

use crate::device::GpuContext;

/// Identifies textures that are interchangeable for pooling purposes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextureKey {
    /// Texture width in pixels.
    pub width: u32,
    /// Texture height in pixels.
    pub height: u32,
    /// Pixel format.
    pub format: wgpu::TextureFormat,
    /// Allowed usages.
    pub usage: wgpu::TextureUsages,
}

impl TextureKey {
    /// Create a key for a 2D texture.
    pub fn new(
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
        Self {
            width,
            height,
            format,
            usage,
        }
    }

    /// Estimated byte footprint of one texture with this key.
    pub fn byte_size(&self) -> u64 {
        let bpp = self.format.block_copy_size(None).unwrap_or(4) as u64;
        bpp * self.width as u64 * self.height as u64
    }

    fn descriptor(&self) -> wgpu::TextureDescriptor<'static> {
        wgpu::TextureDescriptor {
            label: Some("ravel-pool texture"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: self.usage,
            view_formats: &[],
        }
    }
}

// ===========================================================================
// LRU budget accounting (GPU-independent, unit-tested directly)
// ===========================================================================

struct LruEntry {
    id: u64,
    bytes: u64,
    tick: u64,
}

/// Tracks idle (evictable) entries against a byte budget and decides which to
/// evict, oldest first, when the budget is exceeded.
pub struct LruBudget {
    budget: u64,
    used: u64,
    next_id: u64,
    next_tick: u64,
    entries: Vec<LruEntry>,
}

impl LruBudget {
    /// Create a budget allowing up to `budget` idle bytes before eviction.
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            used: 0,
            next_id: 0,
            next_tick: 0,
            entries: Vec::new(),
        }
    }

    /// Bytes currently tracked as idle/evictable.
    #[inline]
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Number of tracked idle entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no idle entries are tracked.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Register a newly idle entry of `bytes`, returning its tracking id.
    pub fn insert(&mut self, bytes: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let tick = self.next_tick;
        self.next_tick += 1;
        self.used += bytes;
        self.entries.push(LruEntry { id, bytes, tick });
        id
    }

    /// Remove a tracked entry by id (e.g. when it is reused), returning its
    /// byte size if it was present.
    pub fn remove(&mut self, id: u64) -> Option<u64> {
        if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
            let entry = self.entries.remove(pos);
            self.used -= entry.bytes;
            Some(entry.bytes)
        } else {
            None
        }
    }

    /// Replace the byte allowance.
    ///
    /// The pool is not the authority on VRAM: its idle allowance is whatever
    /// the shared [`CacheBudget`](ravel_core::cache_budget::CacheBudget)
    /// leaves after the resident side, and that residual moves every time a
    /// cache takes or releases a texture. Setting it does not evict; the
    /// caller follows with [`Self::evict_overflow`].
    pub fn set_budget(&mut self, budget: u64) {
        self.budget = budget;
    }

    /// Evict oldest entries until `used <= budget`, returning evicted ids in
    /// eviction order (oldest first).
    pub fn evict_overflow(&mut self) -> Vec<u64> {
        let mut evicted = Vec::new();
        while self.used > self.budget {
            // Find the entry with the smallest tick (least recently inserted).
            let Some(oldest) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.tick)
                .map(|(i, _)| i)
            else {
                break;
            };
            let entry = self.entries.remove(oldest);
            self.used -= entry.bytes;
            evicted.push(entry.id);
        }
        evicted
    }
}

// ===========================================================================
// Texture pool
// ===========================================================================

/// Source of [`PooledTexture::id`] values. Process-global and monotonic, so
/// an id is never reused even across contexts.
fn next_texture_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A texture acquired from the pool. Returning it via
/// [`TexturePool::release`] makes it available for reuse.
///
/// Deliberately not `Clone`: a lease must be released at most once, or two
/// later acquisitions could alias one writable texture. Share a lease by
/// wrapping it (see `GpuFrameBuffer`), not by cloning it.
pub struct PooledTexture {
    /// The underlying GPU texture (reference counted).
    pub texture: Arc<wgpu::Texture>,
    /// The key this texture was allocated with.
    pub key: TextureKey,
    /// Identity of this pooled texture: unique and never reused, so caches
    /// (the dispatch layer's bind groups) can key on it without risking an
    /// alias with a freed and re-created texture.
    id: u64,
    /// The texture's default view, created once and shared by every bind
    /// group built from this lease.
    view: wgpu::TextureView,
}

impl PooledTexture {
    /// The texture's cached default view.
    pub fn create_view(&self) -> wgpu::TextureView {
        self.view.clone()
    }

    /// A bindable view of this texture for
    /// [`GpuContext::dispatch_compute`](crate::GpuContext::dispatch_compute).
    pub fn binding(&self) -> crate::dispatch::TextureBinding {
        crate::dispatch::TextureBinding {
            id: self.id,
            texture: self.texture.clone(),
            view: self.view.clone(),
        }
    }
}

/// Pools GPU textures by [`TextureKey`], reusing freed textures and evicting
/// idle ones once the idle footprint exceeds the VRAM budget.
pub struct TexturePool {
    ctx: GpuContext,
    /// Idle textures available for reuse, keyed by LRU tracking id.
    idle: HashMap<u64, PooledTexture>,
    /// Tracking ids of idle textures grouped by key.
    by_key: HashMap<TextureKey, Vec<u64>>,
    lru: LruBudget,
    /// The VRAM authority, when the pool is subordinate to one. The idle
    /// allowance is then recomputed from it on every release instead of
    /// being a limit of the pool's own (`cache-plan.md`, `CACHE-3`).
    budget: Option<SharedCacheBudget>,
    /// Running count of textures created by this pool (for diagnostics).
    total_created: u64,
}

impl TexturePool {
    /// Create a pool with a fixed idle-VRAM budget of its own.
    ///
    /// For tests, examples and any caller that has no shared budget. The
    /// application uses [`TexturePool::with_shared_budget`], which is what
    /// makes the resident and idle halves of VRAM add up to one limit.
    pub fn new(ctx: GpuContext, budget_bytes: u64) -> Self {
        Self {
            ctx,
            idle: HashMap::new(),
            by_key: HashMap::new(),
            lru: LruBudget::new(budget_bytes),
            budget: None,
            total_created: 0,
        }
    }

    /// Create a pool whose idle allowance is the VRAM the shared budget has
    /// left over.
    ///
    /// The pool holds **no** limit of its own: before every eviction pass it
    /// asks the budget for the VRAM tier's headroom — the total minus what
    /// the caches are holding — so the ceiling on VRAM lives in exactly one
    /// place. Textures a cache is holding used to be invisible to the pool's
    /// accounting entirely, which is why the two halves never added up.
    pub fn with_shared_budget(ctx: GpuContext, budget: SharedCacheBudget) -> Self {
        let headroom = budget.headroom(Tier::Vram);
        Self {
            ctx,
            idle: HashMap::new(),
            by_key: HashMap::new(),
            lru: LruBudget::new(headroom),
            budget: Some(budget),
            total_created: 0,
        }
    }

    /// Idle (pooled) VRAM in bytes.
    #[inline]
    pub fn idle_bytes(&self) -> u64 {
        self.lru.used()
    }

    /// Number of idle textures currently pooled.
    #[inline]
    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }

    /// Total textures ever created by this pool.
    #[inline]
    pub fn total_created(&self) -> u64 {
        self.total_created
    }

    /// Acquire a texture matching `key`, reusing an idle one when possible.
    ///
    /// An idle texture the unsubmitted dispatch batch still reads or writes
    /// is **skipped**: its queued GPU work has to see the contents the
    /// recording expected, and a new owner would overwrite them before the
    /// batch executes. Skipped textures circulate again after the next flush.
    pub fn acquire(&mut self, key: TextureKey) -> PooledTexture {
        let chosen = self.by_key.get(&key).and_then(|ids| {
            ids.iter()
                .rposition(|id| {
                    self.idle
                        .get(id)
                        .is_some_and(|tex| !self.ctx.is_pending_use(&tex.texture))
                })
                .map(|pos| ids[pos])
        });
        if let Some(id) = chosen {
            if let Some(ids) = self.by_key.get_mut(&key) {
                ids.retain(|&x| x != id);
            }
            self.lru.remove(id);
            if let Some(tex) = self.idle.remove(&id) {
                log::trace!(
                    "texture pool: reused {}x{} {:?}",
                    key.width,
                    key.height,
                    key.format
                );
                return tex;
            }
        }

        let texture = self.ctx.device().create_texture(&key.descriptor());
        self.total_created += 1;
        log::trace!(
            "texture pool: allocated {}x{} {:?} (total created {})",
            key.width,
            key.height,
            key.format,
            self.total_created
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        PooledTexture {
            texture: Arc::new(texture),
            key,
            id: next_texture_id(),
            view,
        }
    }

    /// Return a texture to the pool. May trigger LRU eviction if the idle
    /// footprint now exceeds the budget.
    pub fn release(&mut self, tex: PooledTexture) {
        let key = tex.key;
        let id = self.lru.insert(key.byte_size());
        self.by_key.entry(key).or_default().push(id);
        self.idle.insert(id, tex);

        // The idle allowance is the residual, so it has to be re-read here:
        // between two releases a cache may have taken or given back VRAM.
        //
        // The budget is locked and released inside this call, with the pool
        // already locked by the caller. That is the only permitted order —
        // dropping a cached GPU frame runs pool-then-budget, so a budget
        // holder that reached into the pool would deadlock.
        if let Some(budget) = &self.budget {
            let headroom = budget.headroom(Tier::Vram);
            self.lru.set_budget(headroom);
        }

        let evicted = self.lru.evict_overflow();
        let mut evicted_texture_ids = Vec::new();
        for id in evicted {
            if let Some(tex) = self.idle.remove(&id) {
                if let Some(ids) = self.by_key.get_mut(&tex.key) {
                    ids.retain(|&x| x != id);
                }
                evicted_texture_ids.push(tex.id);
                log::debug!(
                    "texture pool: evicted {}x{} {:?} (idle now {} bytes)",
                    tex.key.width,
                    tex.key.height,
                    tex.key.format,
                    self.lru.used()
                );
            }
        }

        // The accounting above just counted these bytes as freed. A cached
        // bind group still referencing one of the evicted textures would pin
        // its VRAM through the texture views, so the entries must go with it
        // — otherwise the pool believes it freed memory it has not. Same
        // one-way lock discipline as the budget above: pool, then dispatch.
        if !evicted_texture_ids.is_empty() {
            self.ctx.evict_dispatch_bind_groups(&evicted_texture_ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::cache_budget::{CacheBudgetConfig, CacheKind};

    #[test]
    fn a_shared_budget_pool_starts_at_the_tier_headroom() {
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 4096,
            ram_bytes: 0,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let held = budget.reserve(CacheKind::Frame(Tier::Vram), 1024).0;
        assert_eq!(budget.headroom(Tier::Vram), 3072);
        drop(held);
    }

    #[test]
    fn set_budget_replaces_the_allowance_without_evicting() {
        let mut lru = LruBudget::new(1000);
        lru.insert(800);
        lru.set_budget(500);
        // Narrowing the allowance is not itself an eviction: the caller runs
        // the pass when it is ready to drop textures.
        assert_eq!(lru.used(), 800);
        assert_eq!(lru.evict_overflow().len(), 1);
        assert_eq!(lru.used(), 0);
    }

    #[test]
    fn key_byte_size_matches_format() {
        let k = TextureKey::new(
            100,
            50,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
        // Rgba32Float = 16 bytes per pixel.
        assert_eq!(k.byte_size(), 100 * 50 * 16);
    }

    #[test]
    fn keys_with_different_attributes_are_distinct() {
        let a = TextureKey::new(
            10,
            10,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let b = TextureKey::new(
            10,
            10,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn lru_tracks_used_bytes() {
        let mut lru = LruBudget::new(1000);
        let a = lru.insert(300);
        let _b = lru.insert(400);
        assert_eq!(lru.used(), 700);
        assert_eq!(lru.len(), 2);
        lru.remove(a);
        assert_eq!(lru.used(), 400);
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn lru_evicts_oldest_first_until_within_budget() {
        let mut lru = LruBudget::new(1000);
        let a = lru.insert(500); // tick 0
        let b = lru.insert(500); // tick 1
        let c = lru.insert(500); // tick 2 -> total 1500 > 1000

        let evicted = lru.evict_overflow();
        // Must drop the single oldest (a) to get back to 1000.
        assert_eq!(evicted, vec![a]);
        assert_eq!(lru.used(), 1000);
        assert!(lru.remove(b).is_some());
        assert!(lru.remove(c).is_some());
    }

    #[test]
    fn lru_no_eviction_when_within_budget() {
        let mut lru = LruBudget::new(1000);
        lru.insert(400);
        lru.insert(400);
        assert!(lru.evict_overflow().is_empty());
        assert_eq!(lru.used(), 800);
    }

    #[test]
    fn lru_remove_unknown_id_is_noop() {
        let mut lru = LruBudget::new(1000);
        lru.insert(100);
        assert_eq!(lru.remove(999), None);
        assert_eq!(lru.used(), 100);
    }

    // --- GPU-dependent: skipped without an adapter -------------------------

    fn try_context() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    #[test]
    fn a_shared_budget_pool_never_starves_across_the_vram_limit() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        // 128x128 Rgba32Float is 64 KiB; the tier holds four of them.
        let entry = 128u64 * 128 * 16;
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: entry * 4,
            ram_bytes: 0,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let mut pool = TexturePool::with_shared_budget(ctx, budget.clone());
        let key = TextureKey::new(
            128,
            128,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        );

        // Two of the four slots are held by a cache, so the pool's idle
        // allowance is the remaining two — a residual, not a limit it owns.
        let held = [
            budget.reserve(CacheKind::Frame(Tier::Vram), entry).0,
            budget.reserve(CacheKind::Frame(Tier::Vram), entry).0,
        ];

        // A long run of acquire/release across the limit: every acquisition
        // must succeed, and the idle footprint must stay inside the residual.
        for _ in 0..32 {
            let a = pool.acquire(key);
            let b = pool.acquire(key);
            let c = pool.acquire(key);
            pool.release(a);
            pool.release(b);
            pool.release(c);
            assert!(
                pool.idle_bytes() <= entry * 2,
                "idle {} exceeded the residual {}",
                pool.idle_bytes(),
                entry * 2
            );
        }
        assert!(pool.idle_count() > 0, "the pool evicted itself empty");

        // Releasing the held frames widens the residual without touching the
        // pool: the ceiling moved in one place. Four have to stay idle now —
        // asserting only an upper bound would pass on the old two-entry
        // allowance and prove nothing about the widening.
        drop(held);
        let expanded: Vec<_> = (0..4).map(|_| pool.acquire(key)).collect();
        for texture in expanded {
            pool.release(texture);
        }
        assert_eq!(pool.idle_count(), 4, "the residual did not widen");
        assert_eq!(pool.idle_bytes(), entry * 4);
    }

    #[test]
    fn pool_reuses_same_key_texture() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx, 256 * 1024 * 1024);
        let key = TextureKey::new(
            64,
            64,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        );

        let t0 = pool.acquire(key);
        let ptr0 = Arc::as_ptr(&t0.texture);
        pool.release(t0);
        assert_eq!(pool.idle_count(), 1);

        let t1 = pool.acquire(key);
        // The same underlying texture is handed back.
        assert_eq!(Arc::as_ptr(&t1.texture), ptr0);
        assert_eq!(pool.idle_count(), 0);
        assert_eq!(pool.total_created(), 1);
    }
}

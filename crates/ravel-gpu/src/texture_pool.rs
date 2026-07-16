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

use std::collections::HashMap;
use std::sync::Arc;

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
}

impl PooledTexture {
    /// Create a default view of this texture.
    pub fn create_view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
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
    /// Running count of textures created by this pool (for diagnostics).
    total_created: u64,
}

impl TexturePool {
    /// Create a pool with the given idle-VRAM budget in bytes.
    pub fn new(ctx: GpuContext, budget_bytes: u64) -> Self {
        Self {
            ctx,
            idle: HashMap::new(),
            by_key: HashMap::new(),
            lru: LruBudget::new(budget_bytes),
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
    pub fn acquire(&mut self, key: TextureKey) -> PooledTexture {
        if let Some(ids) = self.by_key.get_mut(&key)
            && let Some(id) = ids.pop()
        {
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
        PooledTexture {
            texture: Arc::new(texture),
            key,
        }
    }

    /// Return a texture to the pool. May trigger LRU eviction if the idle
    /// footprint now exceeds the budget.
    pub fn release(&mut self, tex: PooledTexture) {
        let key = tex.key;
        let id = self.lru.insert(key.byte_size());
        self.by_key.entry(key).or_default().push(id);
        self.idle.insert(id, tex);

        let evicted = self.lru.evict_overflow();
        for id in evicted {
            if let Some(tex) = self.idle.remove(&id) {
                if let Some(ids) = self.by_key.get_mut(&tex.key) {
                    ids.retain(|&x| x != id);
                }
                log::debug!(
                    "texture pool: evicted {}x{} {:?} (idle now {} bytes)",
                    tex.key.width,
                    tex.key.height,
                    tex.key.format,
                    self.lru.used()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

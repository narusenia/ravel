// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU buffer pool: the [`TexturePool`](crate::TexturePool) of storage,
//! vertex and index buffers.
//!
//! The first user is [`GpuGeometry`](crate::GpuGeometry), whose attribute
//! columns are storage buffers, but the pool is deliberately not
//! geometry-shaped: any holder that wants a buffer for longer than one
//! dispatch leases one here. That covers two lifetimes.
//!
//! * **Per-frame scratch** — a lease is returned when its last handle drops,
//!   so a steady frame allocates nothing after the first.
//! * **Long-lived, reused by identity** — a lease that is simply *kept* (a
//!   static mesh's vertex buffer, a particle state column) is never returned
//!   and so is never recycled under its holder. The pool does not need to know
//!   the difference; the caller's `Arc` is the identity.
//!
//! ## Reuse key
//!
//! Buffers are interchangeable when they share a [`BufferKey`]: the
//! **power-of-two size class** that holds the request, and the usage set. A
//! column's element count changes (a scatter's count is a parameter), and
//! keying on the exact byte size would turn every edit into a fresh
//! allocation, which is the churn the pool exists to remove. Classes waste up
//! to half a buffer in exchange; the *logical* length is the holder's to
//! remember ([`GpuGeometry`](crate::GpuGeometry) keeps it per column), and a
//! shader must be told the element count rather than ask the binding.
//!
//! The idle ledger is the same [`LruBudget`] the texture and staging pools use.
//! As with textures, a pool built with [`BufferPool::with_shared_budget`]
//! charges its idle bytes to the shared budget's VRAM tier.

use std::collections::HashMap;
use std::sync::Arc;

use ravel_core::cache_budget::{SharedCacheBudget, Tier};

use crate::device::GpuContext;
use crate::texture_pool::LruBudget;

/// The smallest class: wgpu's storage-binding offset alignment is 256, and a
/// class below it would only multiply near-empty buffers.
const MIN_CLASS_BYTES: u64 = 256;

/// What a buffer must support, as a set of flags (this crate's own
/// vocabulary, see [`TextureUsage`](crate::TextureUsage)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferUsage(u8);

impl BufferUsage {
    /// Bindable as a storage buffer a shader reads or writes.
    pub const STORAGE: Self = Self(0b00_0001);
    /// Bindable as a uniform buffer.
    pub const UNIFORM: Self = Self(0b00_0010);
    /// Usable as a vertex buffer.
    pub const VERTEX: Self = Self(0b00_0100);
    /// Usable as an index buffer.
    pub const INDEX: Self = Self(0b00_1000);
    /// Usable as the source of a copy (readback).
    pub const COPY_SRC: Self = Self(0b01_0000);
    /// Usable as the destination of a copy (upload).
    pub const COPY_DST: Self = Self(0b10_0000);

    /// Whether every flag of `other` is set in `self`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The usage a resident attribute column needs: a storage binding that
    /// can be filled from the CPU and read back.
    pub const COLUMN: Self = Self(Self::STORAGE.0 | Self::COPY_SRC.0 | Self::COPY_DST.0);

    pub(crate) fn to_wgpu(self) -> wgpu::BufferUsages {
        let mut usages = wgpu::BufferUsages::empty();
        for (flag, wgpu_flag) in [
            (Self::STORAGE, wgpu::BufferUsages::STORAGE),
            (Self::UNIFORM, wgpu::BufferUsages::UNIFORM),
            (Self::VERTEX, wgpu::BufferUsages::VERTEX),
            (Self::INDEX, wgpu::BufferUsages::INDEX),
            (Self::COPY_SRC, wgpu::BufferUsages::COPY_SRC),
            (Self::COPY_DST, wgpu::BufferUsages::COPY_DST),
        ] {
            if self.contains(flag) {
                usages |= wgpu_flag;
            }
        }
        usages
    }
}

impl std::ops::BitOr for BufferUsage {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Identifies buffers that are interchangeable for pooling purposes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BufferKey {
    /// Byte capacity: a power of two, at least 256.
    pub capacity: u64,
    /// Allowed usages.
    pub usage: BufferUsage,
}

impl BufferKey {
    /// The key of the smallest class holding `bytes`.
    pub fn for_bytes(bytes: u64, usage: BufferUsage) -> Self {
        Self {
            capacity: bytes.max(MIN_CLASS_BYTES).next_power_of_two(),
            usage,
        }
    }
}

/// Source of [`PooledBuffer::id`] values: process-global and never reused.
fn next_buffer_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A buffer acquired from the pool. Return it with [`BufferPool::release`].
///
/// Deliberately not `Clone`: a lease released twice would let two later
/// acquisitions alias one writable buffer. Share one by wrapping it (see
/// [`GpuGeometry`](crate::GpuGeometry)).
pub struct PooledBuffer {
    buffer: Arc<wgpu::Buffer>,
    /// The key this buffer was allocated with.
    pub key: BufferKey,
    id: u64,
}

impl PooledBuffer {
    /// Identity of this buffer: unique and never reused.
    #[inline]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Byte capacity (the size class, not the logical length).
    #[inline]
    pub fn capacity(&self) -> u64 {
        self.key.capacity
    }

    #[inline]
    pub(crate) fn raw(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

/// Pools GPU buffers by [`BufferKey`], reusing freed buffers and evicting idle
/// ones once the idle footprint exceeds the budget.
pub struct BufferPool {
    ctx: GpuContext,
    idle: HashMap<u64, PooledBuffer>,
    by_key: HashMap<BufferKey, Vec<u64>>,
    lru: LruBudget,
    budget: Option<SharedCacheBudget>,
    total_created: u64,
}

impl BufferPool {
    /// A pool with a fixed idle-VRAM budget of its own (tests, examples).
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

    /// A pool whose idle allowance is the VRAM the shared budget has left,
    /// re-read on every release (see
    /// [`TexturePool::with_shared_budget`](crate::TexturePool::with_shared_budget)).
    pub fn with_shared_budget(ctx: GpuContext, budget: SharedCacheBudget) -> Self {
        let headroom = budget.headroom(Tier::Vram);
        Self {
            budget: Some(budget),
            ..Self::new(ctx, headroom)
        }
    }

    /// Idle (pooled) bytes.
    #[inline]
    pub fn idle_bytes(&self) -> u64 {
        self.lru.used()
    }

    /// Number of idle buffers currently pooled.
    #[inline]
    pub fn idle_count(&self) -> usize {
        self.idle.len()
    }

    /// Total buffers ever created by this pool.
    #[inline]
    pub fn total_created(&self) -> u64 {
        self.total_created
    }

    /// Acquire a buffer of at least `bytes` bytes, reusing an idle one of the
    /// same class when possible.
    ///
    /// Contents are unspecified: a reused buffer holds its previous owner's
    /// bytes. Nothing in this unit binds a pooled buffer to a batched
    /// dispatch, so — unlike a texture — no pending-use check is needed yet;
    /// the first consumer that does must add the same skip
    /// [`TexturePool::acquire`](crate::TexturePool::acquire) has.
    pub fn acquire(&mut self, bytes: u64, usage: BufferUsage) -> PooledBuffer {
        let key = BufferKey::for_bytes(bytes, usage);
        if let Some(id) = self.by_key.get_mut(&key).and_then(Vec::pop) {
            self.lru.remove(id);
            if let Some(buffer) = self.idle.remove(&id) {
                log::trace!("buffer pool: reused {} bytes", key.capacity);
                return buffer;
            }
        }
        self.total_created += 1;
        log::trace!(
            "buffer pool: allocated {} bytes (total created {})",
            key.capacity,
            self.total_created
        );
        let buffer = self.ctx.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("ravel-pool buffer"),
            size: key.capacity,
            usage: key.usage.to_wgpu(),
            mapped_at_creation: false,
        });
        PooledBuffer {
            buffer: Arc::new(buffer),
            key,
            id: next_buffer_id(),
        }
    }

    /// Return a buffer to the pool. May evict the oldest idle buffers.
    pub fn release(&mut self, buffer: PooledBuffer) {
        let key = buffer.key;
        let id = self.lru.insert(key.capacity);
        self.by_key.entry(key).or_default().push(id);
        self.idle.insert(id, buffer);

        if let Some(budget) = &self.budget {
            self.lru.set_budget(budget.headroom(Tier::Vram));
        }
        for id in self.lru.evict_overflow() {
            if let Some(evicted) = self.idle.remove(&id) {
                if let Some(ids) = self.by_key.get_mut(&evicted.key) {
                    ids.retain(|&x| x != id);
                }
                log::debug!(
                    "buffer pool: evicted {} bytes (idle now {})",
                    evicted.key.capacity,
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
    fn size_classes_are_powers_of_two_with_a_floor() {
        let class = |bytes| BufferKey::for_bytes(bytes, BufferUsage::COLUMN).capacity;
        assert_eq!(class(0), 256);
        assert_eq!(class(256), 256);
        assert_eq!(class(257), 512);
        assert_eq!(class(1000), 1024);
        assert_eq!(class(1 << 20), 1 << 20);
    }

    #[test]
    fn usage_flags_combine_and_test() {
        assert!(BufferUsage::COLUMN.contains(BufferUsage::STORAGE));
        assert!(!BufferUsage::COLUMN.contains(BufferUsage::VERTEX));
        assert!((BufferUsage::VERTEX | BufferUsage::COPY_DST).contains(BufferUsage::COPY_DST));
    }

    #[test]
    fn a_released_buffer_is_reused_within_its_class_only() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = BufferPool::new(ctx, 64 * 1024 * 1024);
        let first = pool.acquire(1000, BufferUsage::COLUMN);
        let first_id = first.id();
        pool.release(first);
        assert_eq!(pool.idle_count(), 1);

        // 600 bytes is the same 1024 class: reused, nothing created.
        let again = pool.acquire(600, BufferUsage::COLUMN);
        assert_eq!(again.id(), first_id);
        assert_eq!(pool.total_created(), 1);
        assert_eq!(pool.idle_count(), 0);
        pool.release(again);

        // Another class, and another usage, are different buffers.
        let other_class = pool.acquire(2000, BufferUsage::COLUMN);
        let other_usage = pool.acquire(1000, BufferUsage::STORAGE | BufferUsage::COPY_DST);
        assert_ne!(other_class.id(), first_id);
        assert_ne!(other_usage.id(), first_id);
        assert_eq!(pool.total_created(), 3);
    }

    #[test]
    fn the_idle_allowance_evicts_the_oldest_buffer() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = BufferPool::new(ctx, 1024);
        let old = pool.acquire(1024, BufferUsage::COLUMN);
        let new = pool.acquire(512, BufferUsage::COLUMN);
        pool.release(old);
        pool.release(new);
        assert_eq!(pool.idle_count(), 1, "the older, larger buffer was dropped");
        assert_eq!(pool.idle_bytes(), 512);
        assert!(
            !pool
                .by_key
                .contains_key(&BufferKey::for_bytes(1024, BufferUsage::COLUMN))
                || pool.by_key[&BufferKey::for_bytes(1024, BufferUsage::COLUMN)].is_empty()
        );
    }
}

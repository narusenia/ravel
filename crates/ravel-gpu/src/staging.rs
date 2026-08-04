// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Size-keyed pool of readback staging buffers.
//!
//! A GPU→CPU texture read needs a `COPY_DST | MAP_READ` buffer to copy into.
//! Creating one per readback made every displayed frame allocate — and free —
//! a full frame's worth of host-visible memory (`issues/high/HIGH-04`). The
//! resolutions a session reads back are few and repeat every frame, so a pool
//! keyed by the buffer's byte size recycles them with no bookkeeping beyond
//! what [`TexturePool`](crate::TexturePool) already does for textures.
//!
//! The ledger is deliberately the same one: idle buffers are tracked in an
//! [`LruBudget`], so "how many idle bytes may sit unused, and which one goes
//! first" is answered by one implementation for both pools.
//!
//! **This pool is for readback only.** Its buffers are created
//! `COPY_DST | MAP_READ`; nothing else may borrow them, and the drawing side's
//! vertex / uniform buffers are a separate concern.
//!
//! ## Why the idle allowance is not the VRAM budget
//!
//! [`TexturePool`](crate::TexturePool) charges its idle textures to the shared
//! [`CacheBudget`](ravel_core::cache_budget::CacheBudget)'s
//! [`Tier::Vram`](ravel_core::cache_budget::Tier::Vram). Staging buffers are
//! `MAP_READ`, which means host-visible memory: system RAM on a discrete GPU,
//! and on a unified-memory device the same pages the CPU reads directly. Filing
//! them under VRAM would make the texture pool evict *device* textures to make
//! room for *host* buffers, which is the wrong trade in both directions. They
//! are not on the RAM tier either — the tier is fed by the frame caches through
//! `SharedCacheBudget`, and `GpuContext` has no budget handle to consult (that
//! would mean threading one through every context construction, including the
//! application's shared-device path). So the allowance below is the pool's own,
//! and folding it into the RAM tier is left to whoever gives the context a
//! budget.

use std::collections::HashMap;

use crate::texture_pool::LruBudget;
use crate::transfer::stats::TransferCounters;

/// Idle staging bytes kept for reuse before the oldest buffer is dropped.
///
/// A 4K RGBA32F frame stages ~127 MiB and a 1080p one ~32 MiB, so this holds
/// both at once. A smaller allowance would drop the 4K buffer between frames
/// and reintroduce exactly the per-frame allocation the pool exists to remove.
/// It is a cap, not a reservation: nothing is allocated until a readback of
/// that size actually happens.
const IDLE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// A staging buffer borrowed from the pool.
///
/// Return it with [`GpuContext::release_staging`](crate::GpuContext) once the
/// buffer is unmapped. Deliberately not `Clone`: two owners would map the same
/// buffer twice.
pub(crate) struct StagingLease {
    buffer: wgpu::Buffer,
    /// Byte capacity, which is also this buffer's pool key.
    size: u64,
}

impl StagingLease {
    /// The buffer to copy into and map.
    pub(crate) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

/// Recycles readback staging buffers, keyed by their exact byte size.
///
/// Exact sizes rather than rounded-up size classes: a readback's size is
/// `padded_bytes_per_row * height`, which a given output resolution reproduces
/// exactly every frame, so classes would buy nothing and cost a partially used
/// mapping. Distinct sizes each keep their own buffer, bounded by the idle
/// allowance above.
pub(crate) struct StagingPool {
    /// Idle buffers by LRU tracking id.
    idle: HashMap<u64, StagingLease>,
    /// Tracking ids of idle buffers grouped by byte size.
    by_size: HashMap<u64, Vec<u64>>,
    lru: LruBudget,
}

impl Default for StagingPool {
    fn default() -> Self {
        Self {
            idle: HashMap::new(),
            by_size: HashMap::new(),
            lru: LruBudget::new(IDLE_BUDGET_BYTES),
        }
    }
}

impl StagingPool {
    /// A buffer of exactly `size` bytes, created only when none is idle.
    ///
    /// `counters` records the creations, so a test can assert that a steady
    /// stream of readbacks allocates nothing
    /// (`TransferSnapshot::staging_buffers_created`).
    pub(crate) fn acquire(
        &mut self,
        device: &wgpu::Device,
        counters: &TransferCounters,
        size: u64,
    ) -> StagingLease {
        if let Some(ids) = self.by_size.get_mut(&size) {
            let reused = ids.pop();
            if ids.is_empty() {
                self.by_size.remove(&size);
            }
            if let Some(id) = reused {
                self.lru.remove(id);
                if let Some(lease) = self.idle.remove(&id) {
                    log::trace!("staging pool: reused {size} bytes");
                    return lease;
                }
            }
        }

        counters.record_staging_buffer_created();
        log::debug!("staging pool: allocated {size} bytes");
        StagingLease {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ravel readback staging"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            size,
        }
    }

    /// Take a buffer back for reuse. The caller must have unmapped it: the
    /// next borrower maps it again, and wgpu panics on a double map.
    pub(crate) fn release(&mut self, lease: StagingLease) {
        let size = lease.size;
        let id = self.lru.insert(size);
        self.by_size.entry(size).or_default().push(id);
        self.idle.insert(id, lease);

        for id in self.lru.evict_overflow() {
            if let Some(lease) = self.idle.remove(&id) {
                if let Some(ids) = self.by_size.get_mut(&lease.size) {
                    ids.retain(|&x| x != id);
                    if ids.is_empty() {
                        self.by_size.remove(&lease.size);
                    }
                }
                log::debug!(
                    "staging pool: evicted {} bytes (idle now {})",
                    lease.size,
                    self.lru.used()
                );
            }
        }
    }

    /// Idle staging bytes held for reuse (test observation point).
    #[cfg(test)]
    pub(crate) fn idle_bytes(&self) -> u64 {
        self.lru.used()
    }

    /// Number of idle buffers held for reuse (test observation point).
    #[cfg(test)]
    pub(crate) fn idle_count(&self) -> usize {
        self.idle.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::GpuContext;

    fn try_context() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    #[test]
    fn the_same_size_is_reused_and_creates_nothing_new() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let counters = TransferCounters::default();
        let mut pool = StagingPool::default();

        let first = pool.acquire(ctx.device(), &counters, 4096);
        assert_eq!(counters.snapshot().staging_buffers_created, 1);
        pool.release(first);
        assert_eq!(pool.idle_bytes(), 4096);

        for _ in 0..8 {
            let lease = pool.acquire(ctx.device(), &counters, 4096);
            pool.release(lease);
        }
        assert_eq!(
            counters.snapshot().staging_buffers_created,
            1,
            "a repeated size must not allocate again"
        );
        assert_eq!(pool.idle_count(), 1, "one buffer covers the whole run");
    }

    #[test]
    fn distinct_sizes_do_not_share_a_buffer() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let counters = TransferCounters::default();
        let mut pool = StagingPool::default();

        let small = pool.acquire(ctx.device(), &counters, 1024);
        pool.release(small);
        let large = pool.acquire(ctx.device(), &counters, 2048);
        assert_eq!(
            counters.snapshot().staging_buffers_created,
            2,
            "a 2048-byte readback must not be handed a 1024-byte buffer"
        );
        assert_eq!(large.buffer().size(), 2048);
        pool.release(large);
        assert_eq!(pool.idle_count(), 2);
    }

    #[test]
    fn the_idle_allowance_evicts_the_oldest_buffer() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let counters = TransferCounters::default();
        let mut pool = StagingPool {
            lru: LruBudget::new(4096),
            ..Default::default()
        };

        // 4096 fills the allowance exactly; the 2048 that follows pushes it
        // over, so the older (larger) buffer is the one dropped.
        let first = pool.acquire(ctx.device(), &counters, 4096);
        pool.release(first);
        assert_eq!(pool.idle_bytes(), 4096);
        let second = pool.acquire(ctx.device(), &counters, 2048);
        pool.release(second);

        assert_eq!(pool.idle_count(), 1, "the oldest idle buffer was dropped");
        assert_eq!(pool.idle_bytes(), 2048);
        assert!(
            !pool.by_size.contains_key(&4096),
            "an evicted size must not leave an empty entry behind"
        );
    }
}

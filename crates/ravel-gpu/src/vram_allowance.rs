// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! One VRAM idle allowance shared by every pool drawing on the same budget.
//!
//! [`TexturePool`](crate::TexturePool) and [`BufferPool`](crate::BufferPool)
//! each keep an idle set the shared
//! [`CacheBudget`](ravel_core::cache_budget::CacheBudget) cannot see. Reading
//! `headroom(Tier::Vram)` independently in each would let both hold the whole
//! residual at once — twice the budget, idle. A [`VramAllowance`] is the
//! budget plus a running total of the idle bytes every pool built from it
//! holds, so a pool's allowance is the headroom **minus what the other pools
//! hold**.
//!
//! Like the single-pool allowance it replaces, this is an approximation that
//! follows on release: a pool shrinks itself when *it* releases, so the other
//! pool may sit briefly over until its own next release. The sum never drifts,
//! because each pool publishes its exact idle size after every change and
//! withdraws it on drop.
//!
//! Lock order is unchanged: a pool (locked by its caller) reads the budget,
//! never the reverse; the total itself is a lock-free atomic.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ravel_core::cache_budget::{SharedCacheBudget, Tier};

/// The VRAM budget and the idle bytes of every pool sharing it. Cloning shares
/// both.
#[derive(Clone)]
pub struct VramAllowance {
    budget: SharedCacheBudget,
    idle: Arc<AtomicU64>,
}

impl VramAllowance {
    /// A new allowance over `budget`, with no pool holding anything yet.
    pub fn new(budget: SharedCacheBudget) -> Self {
        Self {
            budget,
            idle: Arc::default(),
        }
    }

    /// Idle bytes held by all pools sharing this allowance.
    pub fn idle_bytes(&self) -> u64 {
        self.idle.load(Ordering::Relaxed)
    }

    /// What a pool currently holding `own` idle bytes may keep.
    fn allowance_for(&self, own: u64) -> u64 {
        let others = self.idle_bytes().saturating_sub(own);
        self.budget.headroom(Tier::Vram).saturating_sub(others)
    }
}

/// One pool's membership in a [`VramAllowance`].
pub(crate) struct IdleShare {
    allowance: VramAllowance,
    /// What this pool last published to the total.
    published: u64,
}

impl IdleShare {
    pub(crate) fn allowance(&self) -> VramAllowance {
        self.allowance.clone()
    }

    pub(crate) fn new(allowance: VramAllowance) -> Self {
        Self {
            allowance,
            published: 0,
        }
    }

    /// The allowance for a pool with nothing idle yet.
    pub(crate) fn initial_allowance(&self) -> u64 {
        self.allowance.allowance_for(0)
    }

    /// Publish this pool's idle size `used`, and return what it may keep.
    pub(crate) fn refresh(&mut self, used: u64) -> u64 {
        self.publish(used);
        self.allowance.allowance_for(used)
    }

    /// Publish this pool's idle size `used`.
    pub(crate) fn publish(&mut self, used: u64) {
        if used >= self.published {
            self.allowance
                .idle
                .fetch_add(used - self.published, Ordering::Relaxed);
        } else {
            self.allowance
                .idle
                .fetch_sub(self.published - used, Ordering::Relaxed);
        }
        self.published = used;
    }
}

impl Drop for IdleShare {
    fn drop(&mut self) {
        self.publish(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::cache_budget::CacheBudgetConfig;

    fn allowance(vram: u64) -> VramAllowance {
        VramAllowance::new(SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: vram,
            ram_bytes: 0,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        }))
    }

    #[test]
    fn a_pools_allowance_is_the_headroom_minus_its_peers() {
        let a = allowance(1000);
        let (mut first, mut second) = (IdleShare::new(a.clone()), IdleShare::new(a.clone()));
        assert_eq!(first.refresh(600), 1000);
        assert_eq!(second.refresh(0), 400, "the peer's 600 is not available");
        assert_eq!(a.idle_bytes(), 600);
        assert_eq!(first.refresh(100), 1000);
        assert_eq!(second.refresh(0), 900);
    }

    #[test]
    fn dropping_a_pool_withdraws_its_idle_bytes() {
        let a = allowance(1000);
        let mut share = IdleShare::new(a.clone());
        share.publish(700);
        assert_eq!(a.idle_bytes(), 700);
        drop(share);
        assert_eq!(a.idle_bytes(), 0);
    }
}

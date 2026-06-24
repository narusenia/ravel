// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rayon-based thread pool for DAG node evaluation.

use rayon::{ThreadPool, ThreadPoolBuilder};
use std::num::NonZeroUsize;

/// Configuration for the evaluation thread pool.
#[derive(Clone, Debug, Default)]
pub struct EvalPoolConfig {
    /// Number of worker threads. `None` = auto (CPU core count).
    pub num_threads: Option<NonZeroUsize>,
}

/// Rayon thread pool dedicated to node graph evaluation.
///
/// Wraps a custom `rayon::ThreadPool` so evaluation work is isolated from
/// other rayon users (decode, UI) and thread count can be tuned at startup.
pub struct EvalPool {
    pool: ThreadPool,
}

impl EvalPool {
    pub fn new(config: EvalPoolConfig) -> anyhow::Result<Self> {
        let mut builder = ThreadPoolBuilder::new();
        if let Some(n) = config.num_threads {
            builder = builder.num_threads(n.get());
        }
        builder = builder.thread_name(|i| format!("ravel-eval-{i}"));
        let pool = builder.build()?;
        tracing::info!(threads = pool.current_num_threads(), "eval pool started");
        Ok(Self { pool })
    }

    /// Run `f` on the eval pool and block until it returns.
    pub fn install<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.pool.install(f)
    }

    /// Spawn a fire-and-forget task on the eval pool.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.pool.spawn(f);
    }

    /// Number of worker threads in the pool.
    pub fn num_threads(&self) -> usize {
        self.pool.current_num_threads()
    }
}

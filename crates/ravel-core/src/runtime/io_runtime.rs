// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tokio async runtime for file I/O, network, and plugin host control.

use std::time::Duration;
use tokio::runtime::{Handle, Runtime};

/// Configuration for the async I/O runtime.
#[derive(Clone, Debug)]
pub struct IoRuntimeConfig {
    /// Number of Tokio worker threads (default: 2).
    pub worker_threads: usize,
    /// Timeout for graceful shutdown (default: 5s).
    pub shutdown_timeout: Duration,
}

impl Default for IoRuntimeConfig {
    fn default() -> Self {
        Self {
            worker_threads: 2,
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

/// Tokio multi-thread runtime for async I/O operations.
///
/// File I/O, network requests, and plugin host control run here.
/// The runtime is owned by [`RuntimeManager`](super::RuntimeManager) and
/// exposed via its [`Handle`] so callers can spawn futures without owning
/// the runtime directly.
pub struct IoRuntime {
    runtime: Runtime,
    shutdown_timeout: Duration,
}

impl IoRuntime {
    pub fn new(config: IoRuntimeConfig) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.worker_threads)
            .thread_name("ravel-io")
            .enable_all()
            .build()?;
        tracing::info!(threads = config.worker_threads, "io runtime started");
        Ok(Self {
            runtime,
            shutdown_timeout: config.shutdown_timeout,
        })
    }

    /// Get a [`Handle`] to spawn futures on this runtime.
    pub fn handle(&self) -> &Handle {
        self.runtime.handle()
    }

    /// Spawn a future on the I/O runtime.
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    /// Run a future to completion on the I/O runtime (blocking).
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Gracefully shut down the runtime within the configured timeout.
    pub fn shutdown(self) {
        self.runtime.shutdown_timeout(self.shutdown_timeout);
        tracing::info!("io runtime shut down");
    }
}

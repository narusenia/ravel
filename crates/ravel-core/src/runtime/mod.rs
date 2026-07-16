// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime infrastructure: thread pools, async runtime, and inter-pool channels.
//!
//! Ravel splits work across three execution contexts:
//!
//! | Pool          | Backend              | Purpose                      |
//! |---------------|----------------------|------------------------------|
//! | **Eval**      | `rayon`              | Node graph parallel eval     |
//! | **Decode**    | `std::thread` + bounded queue | FFmpeg / HW decode   |
//! | **I/O**       | `tokio`              | File I/O, network, plugins   |
//!
//! [`RuntimeManager`] owns all three and wires up the
//! [`crossbeam_channel`]-based message pipes between them.

pub mod channels;
pub mod decode_pool;
pub mod eval_pool;
pub mod eval_service;
pub mod io_runtime;
pub mod playback;

pub use channels::{
    DecodedFrame, EvalRequest, EvalResponse, decode_channel, eval_channel, reply_channel,
};
pub use decode_pool::{DecodePool, DecodePoolConfig};
pub use eval_pool::{EvalPool, EvalPoolConfig};
pub use eval_service::{EvalService, EvalUpdate, EvalWorkerHooks, InvalidationHint};
pub use io_runtime::{IoRuntime, IoRuntimeConfig};
pub use playback::{PlaybackClock, PlaybackState};

/// Top-level configuration for all runtime pools.
#[derive(Clone, Debug, Default)]
pub struct RuntimeConfig {
    pub eval: EvalPoolConfig,
    pub decode: DecodePoolConfig,
    pub io: IoRuntimeConfig,
}

/// Owns every thread pool and the Tokio runtime.
///
/// Created once at application startup and held for the lifetime of the process.
/// Subsystems receive handles/senders rather than owning the pools.
pub struct RuntimeManager {
    pub eval: EvalPool,
    pub decode: DecodePool,
    pub io: IoRuntime,
}

impl RuntimeManager {
    pub fn new(config: RuntimeConfig) -> anyhow::Result<Self> {
        let eval = EvalPool::new(config.eval)?;
        let decode = DecodePool::new(config.decode);
        let io = IoRuntime::new(config.io)?;
        tracing::info!("runtime manager ready");
        Ok(Self { eval, decode, io })
    }

    /// Shut down all pools in order. Consumes `self`.
    pub fn shutdown(self) {
        self.decode.shutdown();
        self.io.shutdown();
        // rayon pool shuts down cleanly on drop
        drop(self.eval);
        tracing::info!("runtime manager shut down");
    }
}

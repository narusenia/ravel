// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for the GPU compute pipeline.

use thiserror::Error;

/// Errors produced while setting up or driving the GPU compute pipeline.
#[derive(Debug, Error)]
pub enum GpuError {
    /// No GPU adapter matched the requested options.
    #[error("no compatible GPU adapter found")]
    NoAdapter,

    /// The selected adapter could not provide a logical device/queue.
    #[error("failed to create GPU device: {0}")]
    DeviceRequest(String),

    /// WGSL shader compilation failed. The message is human-readable and
    /// points at the offending source span.
    #[error("shader '{name}' failed to compile:\n{message}")]
    ShaderCompile {
        /// Logical name of the shader (file stem or registered key).
        name: String,
        /// Human-readable, span-annotated diagnostic.
        message: String,
    },

    /// A GPU buffer mapping / readback operation failed.
    #[error("GPU buffer readback failed: {0}")]
    Readback(String),

    /// A requested shader was not present in the manager.
    #[error("shader '{0}' is not registered")]
    ShaderNotFound(String),

    /// A filesystem or watcher error from the hot-reload subsystem.
    #[error("shader hot-reload error: {0}")]
    HotReload(String),
}

/// Convenience result alias for GPU operations.
pub type GpuResult<T> = Result<T, GpuError>;

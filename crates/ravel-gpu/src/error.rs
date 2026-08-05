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

    /// Valid WGSL could not be expressed in a backend shading language.
    ///
    /// Distinct from [`Self::ShaderCompile`] on purpose: the source is fine, it
    /// is the translation that has no answer, and the target is part of the
    /// reason.
    #[error("shader '{name}' could not be translated to {target}:\n{message}")]
    ShaderTranslate {
        /// Logical name of the shader.
        name: String,
        /// The shading language the translation was for.
        target: crate::translate::ShaderTarget,
        /// Why the translation failed.
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

    /// A CPU frame could not be uploaded because its pixel layout does not
    /// match the target texture (a single-channel buffer, or a length that
    /// disagrees with the declared size).
    #[error("frame buffer layout not uploadable: {0}")]
    FrameLayout(String),
}

/// Convenience result alias for GPU operations.
pub type GpuResult<T> = Result<T, GpuError>;

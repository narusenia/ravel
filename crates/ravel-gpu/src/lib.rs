// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! wgpu compute pipeline and shader management for Ravel.
//!
//! This crate provides the shared GPU infrastructure used by node evaluation:
//!
//! * [`GpuContext`] — wgpu device/queue initialization, sharable with GPUI.
//! * [`ComputePipeline`] / [`ComputeDispatch`] — compute shader dispatch,
//!   batched per frame (see [`dispatch`]).
//! * [`TexturePool`] — texture reuse with LRU eviction under a VRAM budget.
//! * [`ShaderManager`] — WGSL compilation, caching, validation, hot reload.
//! * [`transfer`] — GPU <-> CPU texture upload / readback helpers.
//!
//! All internal image processing uses 32-bit float formats with no artificial
//! resolution limits, matching Ravel's architecture.

pub mod binding;
pub mod compute;
pub mod device;
pub mod dispatch;
pub mod error;
pub mod frame;
pub mod raster;
pub mod shader;
pub mod texture_desc;
pub mod texture_pool;
pub mod transfer;

pub use binding::{BindingDesc, BindingKind, ShaderVisibility};
pub use compute::{ComputePipeline, workgroup_count, workgroup_count_2d};
pub use device::GpuContext;
pub use dispatch::{ComputeDispatch, DispatchSnapshot, TextureBinding};
pub use error::{GpuError, GpuResult};
pub use frame::GpuFrameBuffer;
pub use raster::RasterPipeline;
pub use shader::{CompiledShader, ShaderManager, validate_wgsl};
pub use texture_desc::{TextureFormat, TextureUsage};
pub use texture_pool::{LruBudget, PooledTexture, TextureKey, TexturePool};
pub use transfer::{padded_bytes_per_row, read_texture, upload_texture};

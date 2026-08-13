// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! wgpu compute pipeline and shader management for Ravel.
//!
//! This crate provides the shared GPU infrastructure used by node evaluation:
//!
//! * [`GpuContext`] — wgpu device/queue initialization, sharable with GPUI.
//! * [`ComputePipeline`] / [`ComputeDispatch`] — compute shader dispatch,
//!   batched per frame (see [`dispatch`]).
//! * [`RasterPipeline`] / [`QuadDraw`] — the instanced render pass the
//!   rasterizer draws with, batched into the same encoder.
//! * [`TexturePool`] — texture reuse with LRU eviction under a VRAM budget.
//! * [`ShaderManager`] — WGSL compilation, caching, validation, hot reload.
//! * [`translate`] — WGSL to MSL / HLSL / SPIR-V, for the backends that do not
//!   speak WGSL.
//! * [`transfer`] — GPU <-> CPU texture upload / readback helpers, backed by a
//!   size-keyed pool of readback staging buffers.
//! * [`interop`] — where the backend is named on purpose, in two directions.
//!   Backend-native device and texture handles go *out*, for the OpenFX host
//!   (REQ-PLUGIN-001) and hardware decode (REQ-GPU-001) only, and nothing else
//!   may use them (lint `gpu-native-handle-escape`). The UI toolkit's wgpu
//!   device comes *in*, which is the device-sharing contract REQ-GPU-001 rests
//!   on and is the application host's call alone (lint `gpu-device-sharing`).
//!
//! All internal image processing uses 32-bit float formats with no artificial
//! resolution limits, matching Ravel's architecture.

pub mod binding;
pub mod compute;
pub mod device;
pub mod dispatch;
pub mod error;
pub mod frame;
pub mod interop;
pub mod raster;
pub mod shader;
pub(crate) mod staging;
pub mod texture_desc;
pub mod texture_pool;
pub mod transfer;
pub mod translate;

// `interop` is deliberately absent from the re-exports below: reaching the
// backend, in either direction, should read as leaving the abstraction, so
// every use site spells `ravel_gpu::interop`. `scripts/lint-patterns.sh` keys
// on the symbols rather than on this path, because the module holds two
// concerns with two different allowed sets (`gpu-native-handle-escape`,
// `gpu-device-sharing`).
pub use binding::{BindingDesc, BindingKind, ShaderVisibility};
pub use compute::{ComputePipeline, workgroup_count, workgroup_count_2d};
pub use device::{
    AdapterInfo, DeviceType, GpuBackend, GpuContext, GpuDeviceSnapshot, GpuDeviceState,
    GpuLossReason,
};
pub use dispatch::{ComputeDispatch, DispatchSnapshot, QuadDraw, QuadRun, TextureBinding};
pub use error::{GpuError, GpuResult};
pub use frame::GpuFrameBuffer;
pub use raster::{BlendMode, ColorTarget, RasterPipeline};
pub use shader::{CompiledShader, ShaderManager, validate_wgsl};
pub use texture_desc::{TextureFormat, TextureUsage};
pub use texture_pool::{LruBudget, PooledTexture, TextureKey, TexturePool};
pub use transfer::{
    PendingReadback, begin_read_texture, padded_bytes_per_row, read_texture, read_texture_shared,
    upload_texture,
};
pub use translate::{ShaderTarget, TranslatedShader, translate_wgsl};

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Test helpers shared by the user-placed image nodes (`solid`, `colorize`,
//! `alpha`).

use ravel_core::eval::EvalContext;
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, GpuFrameBuffer, TexturePool};
use std::sync::{Arc, Mutex};

/// GPU tests need an adapter; skip where there is none.
pub(crate) fn gpu_or_skip() -> Option<GpuContext> {
    GpuContext::new_blocking().ok()
}

pub(crate) fn pool(gpu: &GpuContext) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)))
}

pub(crate) fn ctx(resolution: (u32, u32)) -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), resolution)
}

pub(crate) fn readback(out: &dyn NodeData) -> FrameBuffer {
    out.downcast_ref::<GpuFrameBuffer>()
        .expect("the node stays GPU-resident")
        .to_frame_buffer()
        .expect("readback")
}

/// RGBA pixels from a flat list, row-major.
pub(crate) fn frame(width: u32, height: u32, pixels: &[[f32; 4]]) -> FrameBuffer {
    assert_eq!(pixels.len(), (width * height) as usize);
    FrameBuffer::from_f32(width, height, pixels.concat())
}

/// Varied colour and alpha, with dyadic values so arithmetic on them is exact.
pub(crate) fn ramp(width: u32, height: u32) -> FrameBuffer {
    let n = width * height;
    let pixels: Vec<[f32; 4]> = (0..n)
        .map(|i| {
            let k = (i % 8) as f32 / 8.0;
            [k, 1.0 - k, 0.25 + 0.5 * k, (i % 9) as f32 / 8.0 * 0.875]
        })
        .collect();
    frame(width, height, &pixels)
}

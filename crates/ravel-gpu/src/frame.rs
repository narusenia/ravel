// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU-resident frame buffer handle (Phase 2 of
//! `docs/implementation/eval-render-performance-plan.md`).
//!
//! [`GpuFrameBuffer`] is the GPU counterpart of
//! [`ravel_core::types::FrameBuffer`]: an RGBA f32 image that stays in VRAM
//! while it flows between node processors. It shares
//! [`DataTypeId::FRAME_BUFFER`] with the CPU type, so port typing and edge
//! validation are unchanged; processors negotiate the representation at
//! their boundaries (upload on CPU input, read back only at true CPU
//! boundaries such as the viewer or persistence).
//!
//! The handle carries its own [`GpuContext`] clone, so any holder can read
//! it back without extra plumbing, and a [`Weak`] reference to the shared
//! [`TexturePool`] so the texture returns to the pool when the last clone
//! is dropped.

use std::sync::{Arc, Mutex, Weak};

use ravel_core::id::DataTypeId;
use ravel_core::types::{BufferData, FrameBuffer, NodeData, PixelFormat};

use crate::device::GpuContext;
use crate::error::GpuResult;
use crate::texture_pool::{PooledTexture, TexturePool};

/// Inner handle: returns the texture to its pool exactly once, when the
/// last [`GpuFrameBuffer`] clone is dropped.
struct PooledHandle {
    pool: Weak<Mutex<TexturePool>>,
    texture: PooledTexture,
}

impl Drop for PooledHandle {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade()
            && let Ok(mut pool) = pool.lock()
        {
            pool.release(self.texture.clone());
        }
    }
}

/// An RGBA f32 frame resident in GPU memory.
#[derive(Clone)]
pub struct GpuFrameBuffer {
    ctx: GpuContext,
    inner: Arc<PooledHandle>,
    width: u32,
    height: u32,
}

impl GpuFrameBuffer {
    /// Wrap a pool-acquired texture. `pool` is the shared pool the texture
    /// came from; it is held weakly so dropping the pool itself is safe.
    pub fn new(
        ctx: GpuContext,
        pool: &Arc<Mutex<TexturePool>>,
        texture: PooledTexture,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            ctx,
            inner: Arc::new(PooledHandle {
                pool: Arc::downgrade(pool),
                texture,
            }),
            width,
            height,
        }
    }

    /// Upload a CPU frame into a pool texture and wrap it as a resident
    /// frame (the inverse of [`GpuFrameBuffer::to_frame_buffer`]).
    pub fn from_frame_buffer(
        ctx: GpuContext,
        pool: &Arc<Mutex<TexturePool>>,
        fb: &FrameBuffer,
    ) -> Self {
        let key = crate::texture_pool::TextureKey::new(
            fb.width,
            fb.height,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        );
        let texture = pool.lock().expect("texture pool poisoned").acquire(key);
        crate::transfer::upload_texture(
            &ctx,
            &texture.texture,
            key,
            bytemuck::cast_slice(&fb.data),
        );
        Self::new(ctx, pool, texture, fb.width, fb.height)
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The underlying texture.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.inner.texture.texture
    }

    /// The context this frame's GPU work is submitted through.
    pub fn context(&self) -> &GpuContext {
        &self.ctx
    }

    /// Read the frame back into a CPU [`FrameBuffer`]. Blocks until the GPU
    /// copy completes — call only at true CPU boundaries (viewer display,
    /// export, CPU-only nodes).
    pub fn to_frame_buffer(&self) -> GpuResult<FrameBuffer> {
        let raw = crate::transfer::read_texture(
            &self.ctx,
            &self.inner.texture.texture,
            self.inner.texture.key,
        )?;
        let floats: Vec<f32> = bytemuck::cast_slice(&raw).to_vec();
        Ok(FrameBuffer {
            width: self.width,
            height: self.height,
            data: Arc::from(floats),
        })
    }
}

impl NodeData for GpuFrameBuffer {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FRAME_BUFFER
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_gpu_resident(&self) -> bool {
        true
    }
}

impl BufferData for GpuFrameBuffer {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn pixel_format(&self) -> PixelFormat {
        PixelFormat::RgbaF32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_pool::TextureKey;

    fn rw_key(width: u32, height: u32) -> TextureKey {
        TextureKey::new(
            width,
            height,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        )
    }

    #[test]
    fn drop_returns_texture_to_pool_once() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let pool = Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 64 * 1024 * 1024)));
        let texture = pool.lock().unwrap().acquire(rw_key(8, 8));

        let frame = GpuFrameBuffer::new(ctx, &pool, texture, 8, 8);
        let clone = frame.clone();
        drop(frame);
        assert_eq!(pool.lock().unwrap().idle_count(), 0, "clone still alive");
        drop(clone);
        assert_eq!(
            pool.lock().unwrap().idle_count(),
            1,
            "released on last drop"
        );
    }

    #[test]
    fn roundtrip_upload_readback_preserves_pixels() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let pool = Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 64 * 1024 * 1024)));
        let key = rw_key(4, 4);
        let texture = pool.lock().unwrap().acquire(key);

        let pixels: Vec<f32> = (0..4 * 4 * 4).map(|i| i as f32 * 0.25).collect();
        crate::transfer::upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

        let frame = GpuFrameBuffer::new(ctx, &pool, texture, 4, 4);
        let fb = frame.to_frame_buffer().unwrap();
        assert_eq!(fb.width, 4);
        assert_eq!(&fb.data[..], &pixels[..]);
    }

    #[test]
    fn is_gpu_resident_marker() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let pool = Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 1024 * 1024)));
        let texture = pool.lock().unwrap().acquire(rw_key(2, 2));
        let frame = GpuFrameBuffer::new(ctx, &pool, texture, 2, 2);
        let dyn_data: &dyn NodeData = &frame;
        assert!(dyn_data.is_gpu_resident());
        assert_eq!(dyn_data.data_type_id(), DataTypeId::FRAME_BUFFER);
    }
}

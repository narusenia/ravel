// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU-resident frame buffer handle (Phase 2 of
//! `docs/implementation/done/eval-render-performance-plan.md`).
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
use crate::error::{GpuError, GpuResult};
use crate::texture_desc::{TextureFormat, TextureUsage};
use crate::texture_pool::{PooledTexture, TexturePool};

/// Inner handle: returns the texture to its pool exactly once, when the
/// last [`GpuFrameBuffer`] clone is dropped.
struct PooledHandle {
    pool: Weak<Mutex<TexturePool>>,
    /// `Some` until dropped; `take()`n exactly once so the lease moves back
    /// into the pool without cloning.
    texture: Option<PooledTexture>,
}

impl PooledHandle {
    fn texture(&self) -> &PooledTexture {
        self.texture.as_ref().expect("present until drop")
    }
}

impl Drop for PooledHandle {
    fn drop(&mut self) {
        if let Some(texture) = self.texture.take()
            && let Some(pool) = self.pool.upgrade()
            && let Ok(mut pool) = pool.lock()
        {
            pool.release(texture);
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

impl std::fmt::Debug for GpuFrameBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuFrameBuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
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
                texture: Some(texture),
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
    ) -> GpuResult<Self> {
        let key = crate::texture_pool::TextureKey::new(
            fb.width,
            fb.height,
            TextureFormat::Rgba32Float,
            TextureUsage::TEXTURE_BINDING
                | TextureUsage::STORAGE_BINDING
                | TextureUsage::COPY_SRC
                | TextureUsage::COPY_DST,
        );
        // The texture is `Rgba32Float`, so the upload needs four f32 channels
        // per pixel whatever the buffer stores. `as_f32()` borrows for
        // `RgbaF32` (the only format produced today), so this stays a
        // zero-copy upload, and a reduced buffer is widened instead of being
        // reinterpreted as garbage. A single-channel buffer has no meaning as
        // a colour texture and is refused rather than uploaded short.
        let pixels = fb
            .as_rgba_f32()
            .map_err(|e| GpuError::FrameLayout(e.to_string()))?;
        let texture = pool.lock().expect("texture pool poisoned").acquire(key);
        crate::transfer::upload_texture(&ctx, &texture, bytemuck::cast_slice(pixels.as_ref()));
        Ok(Self::new(ctx, pool, texture, fb.width, fb.height))
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
    ///
    /// Crate-internal: the frame is the handle callers pass around, and
    /// everything it is for — binding it to a dispatch, reading it back,
    /// naming it to the OFX host through [`interop`](crate::interop) — is
    /// reachable without the backend's texture type (`GPUBK-4`).
    pub(crate) fn texture(&self) -> &wgpu::Texture {
        self.inner.texture().raw()
    }

    /// A bindable view of the frame's texture for
    /// [`GpuContext::dispatch_compute`](crate::GpuContext::dispatch_compute).
    pub fn binding(&self) -> crate::dispatch::TextureBinding {
        self.inner.texture().binding()
    }

    /// The context this frame's GPU work is submitted through.
    pub fn context(&self) -> &GpuContext {
        &self.ctx
    }

    /// Read the frame back into a CPU [`FrameBuffer`]. Blocks until this
    /// frame's GPU copy completes — call only at true CPU boundaries (viewer
    /// display, export, CPU-only nodes).
    ///
    /// The readback lands **directly** in the buffer the returned frame keeps:
    /// `FrameBuffer` stores its pixels as `Arc<[u8]>`, and the transfer layer
    /// fills that shared allocation from the mapped range. The intermediate
    /// `Vec<f32>` this used to build — a second full-frame copy, ~32 MB per
    /// 1080p RGBA32F frame — is gone (`issues/high/HIGH-04`).
    pub fn to_frame_buffer(&self) -> GpuResult<FrameBuffer> {
        let lease = self.inner.texture();
        let format = cpu_pixel_format(lease.key.format);
        let data = crate::transfer::read_texture_shared(&self.ctx, lease)?;
        let expected = self.width as usize
            * self.height as usize
            * lease.key.format.bytes_per_pixel() as usize;
        if data.len() != expected {
            return Err(GpuError::FrameLayout(format!(
                "readback produced {} bytes for a {}x{} {:?} frame, expected {expected}",
                data.len(),
                self.width,
                self.height,
                lease.key.format,
            )));
        }
        Ok(FrameBuffer {
            width: self.width,
            height: self.height,
            format,
            data,
        })
    }

    /// Submit this frame's readback without waiting for it.
    ///
    /// The caller decides when to take the bytes (see
    /// [`PendingReadback`](crate::PendingReadback)); [`Self::to_frame_buffer`]
    /// is this followed immediately by the wait. Exposed so the cost of the
    /// readback can be split into "GPU copy latency" and "CPU copy", which is
    /// what deciding whether to overlap it with the next frame's evaluation
    /// depends on.
    pub fn begin_readback(&self) -> GpuResult<crate::transfer::PendingReadback> {
        crate::transfer::begin_read_texture(&self.ctx, self.inner.texture())
    }
}

/// The CPU-side pixel format a texture of `format` reads back as.
///
/// The resident format is the texture's, not the declared
/// [`BufferData::pixel_format`] — labelling f16 bytes as `RgbaF32` would make
/// every downstream reader misinterpret them, which is what the old
/// `cast_slice` route did silently. A readable frame needs a `FrameBuffer`
/// counterpart, and every format this crate describes has one.
fn cpu_pixel_format(format: TextureFormat) -> PixelFormat {
    match format {
        TextureFormat::Rgba32Float => PixelFormat::RgbaF32,
        TextureFormat::Rgba16Float => PixelFormat::RgbaF16,
        TextureFormat::Rgba8Unorm => PixelFormat::Rgba8,
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

    fn byte_size(&self) -> u64 {
        // VRAM, not RAM. Measured from the texture's *actual* key: the
        // declared `pixel_format()` is the CPU-facing story (`RgbaF32`) and
        // may not be what the texture was allocated as, so reading the key is
        // the only way the accounting stays right when the resident format
        // narrows to `Rgba16Float`.
        size_of::<Self>() as u64 + self.inner.texture().key.byte_size()
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
            TextureFormat::Rgba32Float,
            TextureUsage::TEXTURE_BINDING
                | TextureUsage::STORAGE_BINDING
                | TextureUsage::COPY_SRC
                | TextureUsage::COPY_DST,
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
        crate::transfer::upload_texture(&ctx, &texture, bytemuck::cast_slice(&pixels));

        let frame = GpuFrameBuffer::new(ctx, &pool, texture, 4, 4);
        let fb = frame.to_frame_buffer().unwrap();
        assert_eq!(fb.width, 4);
        assert_eq!(&fb.as_f32()[..], &pixels[..]);
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

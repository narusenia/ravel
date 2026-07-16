// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared GPU helpers for built-in node processors.
//!
//! GPU processors keep their intermediate results resident in VRAM
//! ([`GpuFrameBuffer`]) and only touch CPU memory at true boundaries.
//! [`ensure_gpu`] adapts either frame representation into a texture (an
//! upload happens only for CPU inputs); [`ensure_cpu`] is the inverse for
//! CPU-only processors (a readback happens only for GPU inputs).

use anyhow::Context as _;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{GpuContext, GpuFrameBuffer, PooledTexture, TextureKey, TexturePool};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

pub const WORKGROUP_SIZE: [u32; 2] = [8, 8];

/// A frame adapted to GPU representation for one dispatch.
pub enum GpuImage<'a> {
    /// Input was already GPU-resident; borrow its texture.
    Resident(&'a GpuFrameBuffer),
    /// Input was a CPU frame uploaded into a pool texture for this call.
    Uploaded {
        texture: PooledTexture,
        width: u32,
        height: u32,
    },
}

impl GpuImage<'_> {
    pub fn texture(&self) -> &wgpu::Texture {
        match self {
            GpuImage::Resident(frame) => frame.texture(),
            GpuImage::Uploaded { texture, .. } => &texture.texture,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            GpuImage::Resident(frame) => (frame.width(), frame.height()),
            GpuImage::Uploaded { width, height, .. } => (*width, *height),
        }
    }

    /// Return an uploaded temporary to the pool (no-op for resident inputs,
    /// whose textures are owned by their `GpuFrameBuffer`). Safe to call
    /// right after submitting the dispatch: reuse re-submits on the same
    /// queue, so ordering keeps the queued reads valid.
    pub fn release(self, pool: &Arc<Mutex<TexturePool>>) {
        if let GpuImage::Uploaded { texture, .. } = self {
            pool.lock().unwrap().release(texture);
        }
    }
}

/// Adapt a frame input (CPU or GPU representation) into a bindable texture.
pub fn ensure_gpu<'a>(
    ctx: &GpuContext,
    pool: &Arc<Mutex<TexturePool>>,
    input: &'a dyn NodeData,
) -> anyhow::Result<GpuImage<'a>> {
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Ok(GpuImage::Resident(frame));
    }
    let fb = input
        .downcast_ref::<FrameBuffer>()
        .context("expected FrameBuffer input")?;
    let key = tex_key_rw(fb.width, fb.height);
    let pooled = pool.lock().unwrap().acquire(key);
    ravel_gpu::upload_texture(ctx, &pooled.texture, key, bytemuck::cast_slice(&fb.data));
    Ok(GpuImage::Uploaded {
        texture: pooled,
        width: fb.width,
        height: fb.height,
    })
}

/// Adapt a frame input into CPU memory. Reads back (blocking) only when the
/// input is GPU-resident.
pub fn ensure_cpu(input: &dyn NodeData) -> anyhow::Result<Cow<'_, FrameBuffer>> {
    if let Some(fb) = input.downcast_ref::<FrameBuffer>() {
        return Ok(Cow::Borrowed(fb));
    }
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Ok(Cow::Owned(frame.to_frame_buffer()?));
    }
    anyhow::bail!("expected FrameBuffer input")
}

/// Clone a frame value in either representation (for pass-through
/// processors). Cloning a `GpuFrameBuffer` shares the texture handle.
pub fn clone_frame_value(input: &dyn NodeData) -> Option<Box<dyn NodeData>> {
    if let Some(fb) = input.downcast_ref::<FrameBuffer>() {
        return Some(Box::new(fb.clone()));
    }
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Some(Box::new(frame.clone()));
    }
    None
}

pub fn tex_key_rw(width: u32, height: u32) -> TextureKey {
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

pub fn input_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub fn output_storage_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

pub fn uniform_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared GPU helpers for built-in node processors.

use ravel_core::types::FrameBuffer;
use ravel_gpu::{GpuContext, TextureKey, TexturePool};
use std::sync::Arc;

pub const WORKGROUP_SIZE: [u32; 2] = [8, 8];

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

pub fn upload_frame_buffer(
    ctx: &GpuContext,
    pool: &mut TexturePool,
    fb: &FrameBuffer,
) -> ravel_gpu::PooledTexture {
    let key = tex_key_rw(fb.width, fb.height);
    let pooled = pool.acquire(key);
    let bytes: &[u8] = bytemuck::cast_slice(&fb.data);
    ravel_gpu::upload_texture(ctx, &pooled.texture, key, bytes);
    pooled
}

pub fn readback_frame_buffer(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> anyhow::Result<FrameBuffer> {
    let key = tex_key_rw(width, height);
    let raw_bytes = ravel_gpu::read_texture(ctx, texture, key)?;
    let floats: Vec<f32> = bytemuck::cast_slice(&raw_bytes).to_vec();
    Ok(FrameBuffer {
        width,
        height,
        data: Arc::from(floats),
    })
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

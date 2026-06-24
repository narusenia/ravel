// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end compute pipeline test: upload an RGBA32F image, run the built-in
//! `invert` compute shader on the GPU, read it back, and verify the result.
//!
//! Skips gracefully when no GPU adapter is available (e.g. headless CI without
//! a GPU), so it builds everywhere but only asserts where a device exists.

use ravel_gpu::compute::ComputePipeline;
use ravel_gpu::{GpuContext, ShaderManager, TextureKey, TexturePool, read_texture, upload_texture};

fn try_context() -> Option<GpuContext> {
    GpuContext::new_blocking().ok()
}

#[test]
fn invert_shader_runs_on_gpu() {
    let Some(ctx) = try_context() else {
        eprintln!("skipping invert_shader_runs_on_gpu: no GPU adapter available");
        return;
    };

    let width = 4u32;
    let height = 4u32;
    let format = wgpu::TextureFormat::Rgba32Float;

    // Compile the built-in invert shader.
    let mut shaders = ShaderManager::new(ctx.clone());
    let compiled = shaders.compile("invert").expect("compile invert");

    // Bind group layout: input sampled texture + output storage texture.
    let bgl_entries = [
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        },
    ];

    let pipeline = ComputePipeline::new(&ctx, &compiled, "main", &bgl_entries, [8, 8]);

    // Allocate textures from the pool.
    let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
    let in_key = TextureKey::new(
        width,
        height,
        format,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let out_key = TextureKey::new(
        width,
        height,
        format,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let input = pool.acquire(in_key);
    let output = pool.acquire(out_key);

    // Fill input with a known gradient.
    let pixel_count = (width * height) as usize;
    let mut data = Vec::<f32>::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let v = i as f32 / pixel_count as f32;
        data.extend_from_slice(&[v, 0.25, 0.5, 1.0]);
    }
    let bytes: &[u8] = bytemuck::cast_slice(&data);
    upload_texture(&ctx, &input.texture, in_key, bytes);

    // Bind and dispatch.
    let input_view = input.create_view();
    let output_view = output.create_view();
    let bind_group = ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("invert"),
        layout: pipeline.bind_group_layout(),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&input_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&output_view),
            },
        ],
    });

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("invert dispatch"),
        });
    pipeline.dispatch(&mut encoder, &bind_group, width, height);
    ctx.queue().submit(Some(encoder.finish()));

    // Read back and verify inversion: out.rgb == 1 - in.rgb, alpha preserved.
    let raw = read_texture(&ctx, &output.texture, out_key).expect("readback");
    let result: &[f32] = bytemuck::cast_slice(&raw);
    assert_eq!(result.len(), data.len());

    for i in 0..pixel_count {
        let base = i * 4;
        let eps = 1e-5;
        assert!((result[base] - (1.0 - data[base])).abs() < eps, "r at {i}");
        assert!((result[base + 1] - (1.0 - data[base + 1])).abs() < eps, "g at {i}");
        assert!((result[base + 2] - (1.0 - data[base + 2])).abs() < eps, "b at {i}");
        assert!((result[base + 3] - data[base + 3]).abs() < eps, "a at {i}");
    }
}

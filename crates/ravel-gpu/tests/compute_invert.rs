// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end compute pipeline test: upload an RGBA32F image, run the built-in
//! `invert` compute shader on the GPU, read it back, and verify the result.
//!
//! An integration test is an out-of-crate consumer, which makes this file the
//! standing proof that `ravel-gpu`'s abstraction is sufficient to describe a
//! complete dispatch: it names no `wgpu` type and builds no encoder, bind
//! group or submission of its own. Needing a raw handle here would mean the
//! façade has a gap (`GPUBK-4`), not that the test deserves an exception.
//!
//! Skips gracefully when no GPU adapter is available (e.g. headless CI without
//! a GPU), so it builds everywhere but only asserts where a device exists.

use std::sync::Arc;

use ravel_gpu::compute::ComputePipeline;
use ravel_gpu::{
    BindingDesc, BindingKind, ComputeDispatch, GpuContext, ShaderManager, ShaderVisibility,
    TextureFormat, TextureKey, TexturePool, TextureUsage, read_texture, upload_texture,
};

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
    let format = TextureFormat::Rgba32Float;

    // Compile the built-in invert shader.
    let mut shaders = ShaderManager::new(ctx.clone());
    let compiled = shaders.compile("invert").expect("compile invert");

    // Bind group layout: input sampled texture + output storage texture. The
    // shader takes no parameters, so there is no uniform slot.
    let bgl_entries = [
        BindingDesc::new(0, BindingKind::InputTexture, ShaderVisibility::COMPUTE),
        BindingDesc::new(
            1,
            BindingKind::OutputStorageTexture,
            ShaderVisibility::COMPUTE,
        ),
    ];

    // `Arc` because a dispatch names a *shared* pipeline: the batcher keys its
    // bind-group cache on the allocation's identity and holds a clone for as
    // long as an entry can be handed out.
    let pipeline = Arc::new(ComputePipeline::new(
        &ctx,
        &compiled,
        "main",
        &bgl_entries,
        [8, 8],
    ));

    // Allocate textures from the pool.
    let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
    let in_key = TextureKey::new(
        width,
        height,
        format,
        TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
    );
    let out_key = TextureKey::new(
        width,
        height,
        format,
        TextureUsage::STORAGE_BINDING | TextureUsage::COPY_SRC,
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

    // Describe the dispatch and hand it to the batcher: no encoder, no bind
    // group, no submit. The readback below is the flush point.
    ctx.dispatch_compute(&ComputeDispatch {
        label: "invert",
        pipeline: &pipeline,
        inputs: &[input.binding()],
        output: &output.binding(),
        uniform: &[],
        width,
        height,
    });

    // Read back and verify inversion: out.rgb == 1 - in.rgb, alpha preserved.
    let raw = read_texture(&ctx, &output.texture, out_key).expect("readback");
    let result: &[f32] = bytemuck::cast_slice(&raw);
    assert_eq!(result.len(), data.len());

    for i in 0..pixel_count {
        let base = i * 4;
        let eps = 1e-5;
        assert!((result[base] - (1.0 - data[base])).abs() < eps, "r at {i}");
        assert!(
            (result[base + 1] - (1.0 - data[base + 1])).abs() < eps,
            "g at {i}"
        );
        assert!(
            (result[base + 2] - (1.0 - data[base + 2])).abs() < eps,
            "b at {i}"
        );
        assert!((result[base + 3] - data[base + 3]).abs() < eps, "a at {i}");
    }
}

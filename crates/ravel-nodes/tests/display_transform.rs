// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `CM-7`: the viewer's display transform runs on the GPU.
//!
//! The claims under test are the unit's completion criteria — linear light is
//! gamma corrected on the way to the screen, the GPU and CPU roads out of the
//! working space agree, a user's `.cube` reaches the display and leaving it out
//! restores the default, and none of it depends on how many pixels were
//! evaluated.
//!
//! Requires a GPU adapter; every test skips gracefully without one.

use ravel_core::color::{CubeLut, quantize_u8, to_display_rgba8};
use ravel_core::eval::{EvalContext, Quality};
use ravel_core::runtime::EvalWorkerHooks as _;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::GpuContext;
use ravel_nodes::{DisplayFrame, GpuEvalHooks};
use std::sync::Arc;

/// Hooks wired the way the interactive viewer wires them.
fn viewer_hooks(gpu: GpuContext) -> GpuEvalHooks {
    GpuEvalHooks::new(gpu).with_display_transform()
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
}

fn solid(width: u32, height: u32, pixel: [f32; 4]) -> FrameBuffer {
    let count = (width as usize) * (height as usize);
    FrameBuffer::from_f32(
        width,
        height,
        pixel.iter().copied().cycle().take(count * 4).collect(),
    )
}

/// Put a frame through the hooks and take the display bytes back out.
fn display(hooks: &mut GpuEvalHooks, fb: &FrameBuffer) -> Vec<u8> {
    let value: Arc<dyn ravel_core::types::NodeData> = Arc::new(fb.clone());
    let out = hooks
        .finalize(&value, &ctx())
        .expect("a viewer frame must finalize");
    let frame = out
        .downcast_ref::<DisplayFrame>()
        .expect("a viewer hook yields display bytes");
    assert_eq!((frame.width(), frame.height()), (fb.width, fb.height));
    assert_eq!(
        frame.bgra().len(),
        (fb.width as usize) * (fb.height as usize) * 4,
        "the readback is four bytes per pixel, not sixteen",
    );
    frame.bgra().to_vec()
}

/// The first pixel, back in RGBA order.
fn first_rgba(bgra: &[u8]) -> [u8; 4] {
    [bgra[2], bgra[1], bgra[0], bgra[3]]
}

/// The linear buffer holds light; the screen wants the display encoding. 0.5
/// linear is sRGB 188, and a path that skipped the transform would show 128.
#[test]
fn linear_light_is_gamma_corrected_on_the_gpu() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = viewer_hooks(gpu);

    let bytes = display(&mut hooks, &solid(4, 4, [0.5, 0.5, 0.5, 1.0]));
    assert_eq!(first_rgba(&bytes), [188, 188, 188, 255]);

    // Black, white and the endpoints of the alpha range are exact on both
    // roads — no transfer function is involved at 0 and 1.
    let bytes = display(&mut hooks, &solid(2, 2, [0.0, 1.0, 0.0, 0.0]));
    assert_eq!(first_rgba(&bytes), [0, 255, 0, 0]);
}

/// The unit's central criterion: the GPU road and the CPU road out of the
/// working space land on the same picture.
///
/// **Within one 8-bit code per channel, not bit for bit.**
/// `to_display_rgba8` evaluates the transfer function in `f64`; WGSL has only
/// `f32`, and its `pow` is specified to a tolerance rather than exactly. A
/// value sitting within that tolerance of a code boundary may round either
/// way, and one code out of 256 is below the threshold of a display. Anything
/// wider means the shader is computing a different transform.
#[test]
fn the_gpu_and_cpu_roads_agree_within_one_code() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = viewer_hooks(gpu);

    // A sweep dense enough to straddle code boundaries, plus the out-of-range
    // values a float compositor really produces.
    let mut pixels: Vec<f32> = Vec::new();
    for step in 0..512u32 {
        let v = step as f32 / 511.0;
        pixels.extend_from_slice(&[v, 1.0 - v, v * 0.5 + 0.25, 1.0 - v * 0.5]);
    }
    for extra in [
        [0.0f32, 1.0, 0.5, 0.25],
        [-1.0, 2.0, 0.003, 1.5],
        [0.5 / 255.0, 1.5 / 255.0, 254.5 / 255.0, 1.0 - 1.0 / 512.0],
        [0.0031308, 0.0031309, 0.0031307, 1.0],
    ] {
        pixels.extend_from_slice(&extra);
    }
    let fb = FrameBuffer::from_f32(pixels.len() as u32 / 4, 1, pixels);

    let bytes = display(&mut hooks, &fb);
    let source = fb.as_f32();
    let mut worst = 0i32;
    for (index, (out, pixel)) in bytes
        .chunks_exact(4)
        .zip(source.chunks_exact(4))
        .enumerate()
    {
        let cpu = to_display_rgba8([pixel[0], pixel[1], pixel[2], pixel[3]]);
        let gpu = [out[2], out[1], out[0], out[3]];
        for channel in 0..4 {
            let delta = i32::from(gpu[channel]) - i32::from(cpu[channel]);
            assert!(
                delta.abs() <= 1,
                "pixel {index} channel {channel}: gpu {gpu:?} vs cpu {cpu:?} for {pixel:?}",
            );
            worst = worst.max(delta.abs());
        }
    }
    eprintln!("largest gpu/cpu difference: {worst} code(s)");
}

/// A `.cube` the user supplies replaces the display transform, and taking it
/// away restores the built-in one. The table is the definition on both sides:
/// the GPU interpolates the same cube `CubeLut::sample` does.
#[test]
fn a_cube_lut_reaches_the_display_and_removing_it_restores_the_default() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = viewer_hooks(gpu);

    // A size-3 table that halves every channel — visibly not the transfer
    // function, and not an identity the default could be mistaken for.
    let mut text = String::from("LUT_3D_SIZE 3\n");
    for b in 0..3 {
        for g in 0..3 {
            for r in 0..3 {
                let half = |v: usize| v as f32 / 4.0;
                text.push_str(&format!("{} {} {}\n", half(r), half(g), half(b)));
            }
        }
    }
    let lut = CubeLut::parse(&text).unwrap();

    let probe = [1.0f32, 0.5, 0.0, 1.0];
    let frame = solid(4, 4, probe);

    let default_bytes = first_rgba(&display(&mut hooks, &frame));
    assert_eq!(default_bytes, to_display_rgba8(probe));

    hooks.set_display_lut(Some(lut.clone()));
    let graded = first_rgba(&display(&mut hooks, &frame));
    let expected = lut.sample([probe[0], probe[1], probe[2]]);
    for channel in 0..3 {
        let want = i32::from(quantize_u8(expected[channel]));
        assert!(
            (i32::from(graded[channel]) - want).abs() <= 1,
            "graded {graded:?} does not match the table's {expected:?}",
        );
    }
    assert_ne!(
        graded, default_bytes,
        "a LUT that halves every channel has to change the picture"
    );
    // Alpha is coverage: the grade must not touch it.
    assert_eq!(graded[3], 255);

    hooks.set_display_lut(None);
    assert_eq!(
        first_rgba(&display(&mut hooks, &frame)),
        default_bytes,
        "removing the LUT has to restore the built-in transform"
    );
}

/// The interpolation itself, which the test above cannot see: its table is
/// linear per channel and its probe sits on a grid point, so a wrong corner
/// weight would still produce the right answer.
///
/// This one is deliberately hostile to that: a **non-linear** table (each
/// channel squared, and the channels crossed so a swapped axis shows), a
/// **shifted domain** (`DOMAIN_MIN` / `DOMAIN_MAX` other than 0..1), and
/// probes at **cell midpoints and off-grid fractions**. The reference is
/// `CubeLut::sample` — the same trilinear the shader re-implements by hand.
///
/// `CM-1`'s end-of-domain bug survived because the only LUT under test was
/// size 2, where every probe is a grid point. This is the test that would
/// have caught it.
#[test]
fn the_gpu_interpolates_the_cube_the_way_the_cpu_does() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = viewer_hooks(gpu);

    // Size 5 over the domain 0.2..0.9. Each output channel is a different
    // non-linear function of a different input axis, so a swapped axis, a
    // mirrored fraction or a dropped corner all move the answer.
    const SIZE: usize = 5;
    let mut text = String::from("LUT_3D_SIZE 5\nDOMAIN_MIN 0.2 0.2 0.2\nDOMAIN_MAX 0.9 0.9 0.9\n");
    for b in 0..SIZE {
        for g in 0..SIZE {
            for r in 0..SIZE {
                let t = |v: usize| v as f32 / (SIZE - 1) as f32;
                // r' = r^2, g' = 1 - g^3, b' = (r + b) / 2 — the last one is
                // what a swapped red/blue axis breaks.
                text.push_str(&format!(
                    "{} {} {}\n",
                    t(r) * t(r),
                    1.0 - t(g) * t(g) * t(g),
                    (t(r) + t(b)) * 0.5,
                ));
            }
        }
    }
    let lut = CubeLut::parse(&text).unwrap();
    hooks.set_display_lut(Some(lut.clone()));

    // Cell midpoints, off-grid fractions, both ends of the domain and outside
    // it (which clamps to the cube's edge).
    let probes = [
        [0.2f32, 0.2, 0.2, 1.0],
        [0.9, 0.9, 0.9, 1.0],
        [0.2875, 0.2875, 0.2875, 1.0],
        [0.55, 0.55, 0.55, 1.0],
        [0.31, 0.77, 0.48, 0.5],
        [0.83, 0.24, 0.61, 0.25],
        [0.4137, 0.6829, 0.2011, 1.0],
        [0.0, 1.0, 0.5, 1.0],
    ];
    let mut pixels = Vec::new();
    for probe in probes {
        pixels.extend_from_slice(&probe);
    }
    let fb = FrameBuffer::from_f32(probes.len() as u32, 1, pixels);

    let bytes = display(&mut hooks, &fb);
    let mut moved = 0;
    for (index, (out, probe)) in bytes.chunks_exact(4).zip(probes).enumerate() {
        let expected = lut.sample([probe[0], probe[1], probe[2]]);
        let gpu = [out[2], out[1], out[0]];
        for channel in 0..3 {
            let want = i32::from(quantize_u8(expected[channel]));
            assert!(
                (i32::from(gpu[channel]) - want).abs() <= 1,
                "probe {index} {probe:?} channel {channel}: gpu {gpu:?} vs cpu \
                 {expected:?} (code {want})",
            );
        }
        // Alpha never goes through the table.
        assert_eq!(out[3], quantize_u8(probe[3]), "probe {index} alpha");
        if gpu
            != [
                quantize_u8(probe[0]),
                quantize_u8(probe[1]),
                quantize_u8(probe[2]),
            ]
        {
            moved += 1;
        }
    }
    assert!(
        moved >= probes.len() - 1,
        "the table has to actually move the colours, or this proves nothing"
    );
}

/// `quality` and the viewer resolution decide *which* pixels are evaluated;
/// the display transform decides what a pixel value means, and that cannot
/// depend on how many of them there are.
#[test]
fn the_display_transform_ignores_quality_and_buffer_size() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = viewer_hooks(gpu);

    let pixel = [0.5f32, 0.18, 0.9, 0.75];
    let sizes = [(64u32, 36u32), (32, 18), (1, 1)];
    let qualities = [Quality::Preview, Quality::Final];

    let mut seen: Option<[u8; 4]> = None;
    for (width, height) in sizes {
        for quality in qualities {
            let fb = solid(width, height, pixel);
            let value: Arc<dyn ravel_core::types::NodeData> = Arc::new(fb.clone());
            let out = hooks
                .finalize(
                    &value,
                    &EvalContext::new(0, FrameRate::new(30, 1), (width, height))
                        .with_quality(quality),
                )
                .expect("a viewer frame must finalize");
            let frame = out.downcast_ref::<DisplayFrame>().expect("display bytes");
            assert_eq!((frame.width(), frame.height()), (width, height));
            let first = first_rgba(frame.bgra());
            match seen {
                None => seen = Some(first),
                Some(previous) => assert_eq!(
                    first, previous,
                    "{width}x{height} at {quality:?} produced a different colour",
                ),
            }
        }
    }
}

/// A `PooledTexture` does not return itself on drop — the transform hands
/// every lease back by hand, on every path. A lease that escaped would show
/// here as a pool that creates a fresh texture per frame instead of reusing
/// the one it already has.
///
/// The failure paths a test cannot reach (a lost device during readback) are
/// covered by structure rather than by this test: the pipeline is built and
/// the frame's size checked before anything is acquired, the input lease is
/// released by the caller whatever the body returns, and the output lease is
/// released before the readback's result is judged.
#[test]
fn the_pool_takes_its_textures_back_every_frame() {
    use ravel_gpu::ShaderManager;
    use ravel_nodes::DisplayTransform;

    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = ravel_nodes::shared_texture_pool(&gpu);
    let mut display = DisplayTransform::new();

    // A degenerate frame is refused before anything is acquired.
    let empty = FrameBuffer::from_f32(0, 0, Vec::new());
    assert!(display.run(&gpu, &mut shaders, &pool, &empty).is_err());
    assert_eq!(
        pool.lock().unwrap().total_created(),
        0,
        "a refused frame must not have acquired a texture"
    );

    let fb = solid(64, 64, [0.25, 0.5, 0.75, 1.0]);
    display.run(&gpu, &mut shaders, &pool, &fb).expect("first");
    let after_first = pool.lock().unwrap().total_created();
    assert!(after_first > 0);

    for _ in 0..8 {
        display.run(&gpu, &mut shaders, &pool, &fb).expect("frame");
    }
    assert_eq!(
        pool.lock().unwrap().total_created(),
        after_first,
        "every frame must reuse the textures the previous one gave back"
    );
}

/// The export worker and `ravel-cli` build hooks without the display
/// transform, and must keep receiving the linear frame their own encode step
/// expects. A display-encoded frame reaching `to_output_space` would apply the
/// transfer function twice.
#[test]
fn hooks_without_the_display_transform_still_yield_a_linear_frame() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut hooks = GpuEvalHooks::new(gpu);

    let fb: Arc<dyn ravel_core::types::NodeData> = Arc::new(solid(2, 2, [0.5, 0.5, 0.5, 1.0]));
    let out = hooks.finalize(&fb, &ctx()).expect("readback");
    let linear = out
        .downcast_ref::<FrameBuffer>()
        .expect("an export hook yields a linear frame");
    assert!(out.downcast_ref::<DisplayFrame>().is_none());
    assert!((linear.as_f32()[0] - 0.5).abs() < 1e-6);
}

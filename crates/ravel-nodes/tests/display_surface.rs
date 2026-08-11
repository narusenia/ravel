// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ZC-3`: the viewer's display transform can hand its texture straight to
//! GPUI instead of reading it back.
//!
//! The claims under test are the unit's completion criteria. The zero-copy
//! flag decides which representation `DisplayTransform` produces, so the
//! criteria are checkable here, on the worker side, without a window:
//!
//! 1. **The readback is gone** when the surface path is on — and still there
//!    when it is off, which is what proves the fallback still works. Zero
//!    against zero would pass even if the fallback had silently died.
//! 2. **The picture is unchanged.** Both roads leave the same display
//!    encoding in the texture, so reading the GPU one back in the test must
//!    reproduce the CPU one byte for byte. Reading it back *here* is fine;
//!    the criterion is that the *viewer* does not.
//! 3. **Neither road depends on `ViewerResolution` or `quality`**, which
//!    decide how many pixels are evaluated, not what one pixel means.
//!
//! `display_transform.rs` covers the transform's colour behaviour; this file
//! only covers the choice of representation. Requires a GPU adapter; every
//! test skips gracefully without one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ravel_core::eval::{EvalContext, Quality};
use ravel_core::runtime::EvalWorkerHooks as _;
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::GpuContext;
use ravel_nodes::{DisplayFrame, GpuEvalHooks};

/// Hooks wired the way the interactive viewer wires them, with the host
/// capability flag the application owns.
fn viewer_hooks(gpu: GpuContext, zero_copy: &Arc<AtomicBool>) -> GpuEvalHooks {
    GpuEvalHooks::new(gpu).with_display_surface_mode(Arc::clone(zero_copy))
}

fn solid(width: u32, height: u32, pixel: [f32; 4]) -> FrameBuffer {
    let count = (width as usize) * (height as usize);
    FrameBuffer::from_f32(
        width,
        height,
        pixel.iter().copied().cycle().take(count * 4).collect(),
    )
}

fn eval_ctx(width: u32, height: u32) -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (width, height))
}

/// Put a frame through the hooks and return the display frame.
fn finalize(hooks: &mut GpuEvalHooks, fb: &FrameBuffer, ctx: &EvalContext) -> DisplayFrame {
    let value: Arc<dyn NodeData> = Arc::new(fb.clone());
    let out = hooks
        .finalize(&value, ctx)
        .expect("a viewer frame must finalize");
    out.downcast_ref::<DisplayFrame>()
        .expect("a viewer hook yields a display frame")
        .clone()
}

/// The display bytes, whichever representation the frame carries. The GPU one
/// is read back **by the test**; that is the comparison, not the viewer path.
fn display_bytes(frame: &DisplayFrame) -> Vec<u8> {
    match frame.gpu_frame() {
        Some(gpu) => {
            let fb = gpu
                .to_frame_buffer()
                .expect("the display texture reads back");
            assert_eq!(
                fb.format,
                ravel_core::types::PixelFormat::Rgba8,
                "the display texture is the 8-bit encoding, not the linear buffer",
            );
            fb.data.to_vec()
        }
        None => frame
            .bgra()
            .expect("a frame is one representation or the other")
            .to_vec(),
    }
}

/// The whole point of the unit: with the surface path on, finalizing a viewer
/// frame costs no readback. With it off, it still costs exactly one — the
/// fallback the multi-GPU and non-macOS hosts depend on.
#[test]
fn the_surface_path_removes_the_readback_and_the_fallback_keeps_it() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let fb = solid(16, 16, [0.5, 0.18, 0.9, 1.0]);

    let zero_copy = Arc::new(AtomicBool::new(true));
    let mut hooks = viewer_hooks(gpu.clone(), &zero_copy);

    // Warm the pipeline and the texture pool so the measured window contains
    // the frame's own transfers and nothing else.
    let warm = finalize(&mut hooks, &fb, &eval_ctx(16, 16));
    assert!(
        warm.gpu_frame().is_some(),
        "the surface path must publish a texture, not CPU bytes",
    );
    drop(warm);

    let before = gpu.transfer_stats();
    let frame = finalize(&mut hooks, &fb, &eval_ctx(16, 16));
    let delta = before.delta(&gpu.transfer_stats());
    assert_eq!(
        delta.readbacks, 0,
        "the zero-copy viewer frame must not read back: {delta:?}",
    );
    assert!(frame.gpu_frame().is_some());
    drop(frame);

    // The same hooks, told the host cannot share a device.
    zero_copy.store(false, Ordering::Release);
    let warm = finalize(&mut hooks, &fb, &eval_ctx(16, 16));
    assert!(
        warm.gpu_frame().is_none(),
        "the fallback must publish CPU bytes",
    );
    drop(warm);

    let before = gpu.transfer_stats();
    let frame = finalize(&mut hooks, &fb, &eval_ctx(16, 16));
    let delta = before.delta(&gpu.transfer_stats());
    assert_eq!(
        delta.readbacks, 1,
        "the fallback still reads the frame back exactly once: {delta:?}",
    );
    assert!(frame.gpu_frame().is_none());
}

/// Consecutive worker results must keep their own contents when the output
/// texture is borrowed by the surface path. Distinct solid colours make a
/// stale-frame or frame-crossing reuse visible in the first pixel.
#[test]
fn consecutive_surface_frames_keep_their_sequence() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let zero_copy = Arc::new(AtomicBool::new(true));
    let mut hooks = viewer_hooks(gpu, &zero_copy);
    let pixels = [
        [0.05, 0.2, 0.8, 1.0],
        [0.7, 0.1, 0.25, 1.0],
        [0.15, 0.85, 0.35, 1.0],
        [0.95, 0.45, 0.05, 1.0],
    ];

    for (index, pixel) in pixels.into_iter().enumerate() {
        let frame = finalize(&mut hooks, &solid(8, 8, pixel), &eval_ctx(8, 8));
        assert!(frame.gpu_frame().is_some());
        let bytes = display_bytes(&frame);
        let expected = ravel_core::color::to_display_rgba8(pixel);
        assert_eq!(
            &bytes[..4],
            &[expected[2], expected[1], expected[0], expected[3]],
            "surface frame {index} did not retain its own pixels",
        );
    }
}

/// Removing the round trip must not change a pixel. Both roads run the same
/// dispatch into the same `Rgba8Unorm` texture; only what happens afterwards
/// differs, so the bytes must be identical rather than merely close.
#[test]
fn both_roads_produce_the_same_display_bytes() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };

    // A gradient rather than a flat fill: a swizzle or a stride error is
    // invisible in a solid colour.
    let (width, height) = (8u32, 8u32);
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.25, 1.0]);
        }
    }
    let fb = FrameBuffer::from_f32(width, height, data);

    let zero_copy = Arc::new(AtomicBool::new(false));
    let mut hooks = viewer_hooks(gpu, &zero_copy);

    let cpu = display_bytes(&finalize(&mut hooks, &fb, &eval_ctx(width, height)));

    zero_copy.store(true, Ordering::Release);
    let gpu_bytes = display_bytes(&finalize(&mut hooks, &fb, &eval_ctx(width, height)));

    assert_eq!(
        cpu.len(),
        (width as usize) * (height as usize) * 4,
        "four bytes per pixel on the CPU road",
    );
    // `CM-7` allows ±1 code between roads that differ. These two do not
    // differ — same shader, same texture — so hold them to equality and let
    // the stricter bound catch a regression the looser one would admit.
    assert_eq!(
        gpu_bytes, cpu,
        "the surface texture must hold exactly the bytes the readback road produced",
    );
}

/// `quality` and the viewer resolution decide *which* pixels are evaluated.
/// Which representation the frame takes must not follow them, and neither
/// must the colour.
#[test]
fn the_surface_path_ignores_quality_and_buffer_size() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let zero_copy = Arc::new(AtomicBool::new(true));
    let mut hooks = viewer_hooks(gpu, &zero_copy);

    let pixel = [0.5f32, 0.18, 0.9, 0.75];
    let mut seen: Option<[u8; 4]> = None;
    for (width, height) in [(64u32, 36u32), (32, 18), (1, 1)] {
        for quality in [Quality::Preview, Quality::Final] {
            let frame = finalize(
                &mut hooks,
                &solid(width, height, pixel),
                &eval_ctx(width, height).with_quality(quality),
            );
            assert!(
                frame.gpu_frame().is_some(),
                "{width}x{height} at {quality:?} fell off the surface path",
            );
            assert_eq!((frame.width(), frame.height()), (width, height));

            let bytes = display_bytes(&frame);
            let first = [bytes[2], bytes[1], bytes[0], bytes[3]];
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

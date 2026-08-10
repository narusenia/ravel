// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measurement harness for the viewer round-trip breakdown, the decision gate
//! of `docs/implementation/zero-copy-viewer-plan.md` (ZC-1). It measures
//! rather than asserts, so it stays out of the normal test run:
//!
//! ```text
//! cargo test -p ravel-app --release --test viewer_roundtrip \
//!     measure_viewer_roundtrip_breakdown -- --ignored --nocapture
//! ```
//!
//! **What it breaks down.** After `CM-7`, a finished GPU-resident frame
//! reaches the screen through four stages:
//!
//! 1. the display transform (`DisplayTransform::run`'s single dispatch),
//! 2. the readback (4 bytes a pixel, GPU -> CPU),
//! 3. the CPU-side wrap (`ViewerImage::from_display_frame`: one copy out of
//!    the shared readback buffer into the `Vec` a `RenderImage` owns),
//! 4. GPUI's upload and atlas churn.
//!
//! Zero-copy display removes 2, 3, and 4. Stage 4 lives inside gpui-ce and
//! cannot be timed without a window — no UI-thread harness exists in this
//! repository — so the harness reports 2 and 3 separately as an **estimate**
//! of what zero-copy saves, and stage 1's number (the `CM-7` measurement
//! already covers it) falls out of the stage 1+2 total.
//!
//! **Why an estimate and not a lower bound.** Stage 2 is timed on a texture of
//! the transform's output key, not on the one `run` reads back, and it runs
//! after `run` has already blocked on its own readback — so it carries none of
//! the dispatch wait `run`'s readback carries. It is a proxy, not a slice of
//! the measured path. The cross-check that makes the proxy trustworthy is
//! printed beside it: stage 1+2 minus stage 2 has to land within one dispatch
//! of zero. Read the numbers as an estimate whose error is a fraction of a
//! dispatch, not as a bound.
//!
//! **Why it lives in `ravel-app`.** Stage 3's entity is
//! [`ViewerImage::from_display_frame`], and the `RenderImage` it builds is a
//! gpui type `ravel-nodes` cannot name. Measuring 2 and 3 in one process also
//! keeps the interleaved rounds honest: machine load falls on every stage
//! alike, which is the only way the numbers mean anything on a machine whose
//! load average never settles.
//!
//! Each cell is one `ViewerResolution` factor applied to one composition
//! resolution; the harness alternates the three measurements within each of
//! 20 rounds and is meant to be run three times, like the `CM-7` harness in
//! `crates/ravel-nodes/tests/display_transform.rs`. The readback **count** is
//! recorded beside the times — load moves times, never counts.

use ravel_app::panels::ViewerImage;
use ravel_core::types::FrameBuffer;
use ravel_gpu::{
    GpuContext, GpuFrameBuffer, ShaderManager, TextureFormat, TextureKey, TextureUsage,
};
use ravel_nodes::DisplayTransform;
use ravel_ui::panels::viewer::ViewerResolution;
use std::time::Instant;

#[test]
#[ignore = "measurement harness; run with --ignored --nocapture"]
fn measure_viewer_roundtrip_breakdown() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = ravel_nodes::shared_texture_pool(&gpu);
    let mut display = DisplayTransform::new();

    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .or_else(|| {
            std::process::Command::new("uptime")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_default();
    eprintln!("load at start: {load}");

    for comp in [(1920u32, 1080u32), (3840, 2160)] {
        for factor in ViewerResolution::ALL {
            let (width, height) = factor.apply(comp);
            let count = (width as usize) * (height as usize) * 4;
            let pixels: Vec<f32> = (0..count).map(|i| (i % 511) as f32 / 510.0).collect();
            let cpu_frame = FrameBuffer::from_f32(width, height, pixels);
            // The viewer's normal case is a GPU-resident frame, so the input
            // is uploaded once in setup and never inside a round.
            let resident =
                GpuFrameBuffer::from_frame_buffer(gpu.clone(), &pool, &cpu_frame).expect("upload");
            // A texture of the same key the transform produces, for timing
            // the readback alone. Its contents are irrelevant: readback cost
            // is a function of size and format, not of what is in the bytes.
            let readback_tex = pool.lock().unwrap().acquire(TextureKey::new(
                width,
                height,
                TextureFormat::Rgba8Unorm,
                TextureUsage::STORAGE_BINDING | TextureUsage::COPY_SRC,
            ));

            // The first round of a cell pays the pipeline's first compilation
            // and this size's staging buffer allocation; both are what the
            // caches exist to amortize, so they are warm-up rather than part
            // of the per-frame number (the same treatment
            // `crates/ravel-nodes/examples/perf_baseline.rs` gives them).
            {
                let frame = display
                    .run(&gpu, &mut shaders, &pool, &resident)
                    .expect("display transform");
                let bytes = ravel_gpu::read_texture_shared(&gpu, &readback_tex).expect("readback");
                let image = ViewerImage::from_display_frame(&frame).expect("a display frame wraps");
                std::hint::black_box((&bytes, &image));
            }

            let rounds = 20;
            let (mut total_ns, mut readback_ns, mut wrap_ns) = (0u128, 0u128, 0u128);
            let stats_before = gpu.transfer_stats();
            for _ in 0..rounds {
                // Stages 1+2: the real viewer path up to the display bytes.
                // `run` blocks on its own readback, so the queue is drained
                // before the next measurement starts.
                let start = Instant::now();
                let frame = display
                    .run(&gpu, &mut shaders, &pool, &resident)
                    .expect("display transform");
                total_ns += start.elapsed().as_nanos();

                // Stage 2 alone: the 4-bytes-a-pixel GPU -> CPU readback.
                let start = Instant::now();
                let bytes = ravel_gpu::read_texture_shared(&gpu, &readback_tex).expect("readback");
                readback_ns += start.elapsed().as_nanos();
                std::hint::black_box(&bytes);

                // Stage 3 alone: the CPU-side wrap into the toolkit image.
                let start = Instant::now();
                let image = ViewerImage::from_display_frame(&frame).expect("a display frame wraps");
                wrap_ns += start.elapsed().as_nanos();
                std::hint::black_box(&image);
            }
            let delta = stats_before.delta(&gpu.transfer_stats());
            pool.lock().unwrap().release(readback_tex);

            let ms = |ns: u128| ns as f64 / rounds as f64 / 1e6;
            let (readback, wrap) = (ms(readback_ns), ms(wrap_ns));
            // The estimate is summed before rounding, so it need not equal the
            // sum of the two printed values.
            eprintln!(
                "{comp:?} {factor:?} ({width}x{height}): \
                 transform+readback {:.3} ms | readback {:.3} ms | wrap {:.3} ms | \
                 estimate (readback+wrap) {:.3} ms | dispatch (total-readback) {:.3} ms",
                ms(total_ns),
                readback,
                wrap,
                readback + wrap,
                ms(total_ns) - readback,
            );
            // The viewer path itself is exactly one readback per frame (the
            // one inside `run`); the other `rounds` readbacks are the
            // standalone stage-2 timing. Uploads stay at zero: the frame is
            // GPU-resident before the rounds begin.
            eprintln!(
                "  transfers over {rounds} rounds: {} readbacks ({} bytes), {} uploads",
                delta.readbacks, delta.readback_bytes, delta.uploads,
            );
        }
    }
}

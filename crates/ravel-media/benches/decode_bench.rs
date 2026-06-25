// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Decode benchmarks comparing HW vs SW decode performance.
//!
//! Run with: `cargo bench -p ravel-media --features ffmpeg`

use criterion::{Criterion, criterion_group, criterion_main};
use ravel_core::media::MediaReader;
use ravel_media::decoder::FfmpegDecoder;
use std::path::PathBuf;
use std::process::Command;

fn generate_video(dir: &std::path::Path, name: &str, resolution: &str) -> PathBuf {
    let path = dir.join(name);
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=duration=2:size={resolution}:rate=30"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "ultrafast",
            path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("ffmpeg CLI not found");
    assert!(status.success());
    path
}

fn bench_decode(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path_720 = generate_video(dir.path(), "bench_720p.mp4", "1280x720");

    let mut group = c.benchmark_group("decode");
    group.sample_size(20);

    group.bench_function("720p_h264_10frames", |b| {
        b.iter(|| {
            let mut decoder = FfmpegDecoder::open(&path_720).expect("open");
            let stream_idx = decoder.info().first_video().unwrap().stream_index;
            for frame in 0..10 {
                let _ = decoder
                    .decode_video_frame(stream_idx, frame)
                    .expect("decode");
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);

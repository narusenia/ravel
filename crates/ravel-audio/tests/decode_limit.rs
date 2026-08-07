// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The memory cap on full-length audio decode, against a real file.
//!
//! Decision 8 of `docs/implementation/audio-plan.md` decodes a whole asset
//! into memory and caps it at [`MAX_DECODE_BYTES`]. The cap only means
//! something if crossing it is *noticed*: an asset that quietly stopped at
//! 128 MiB would render as sound that cuts out partway, which is worse than
//! no sound at all because nothing says so.
//!
//! This is therefore the one test that builds an over-limit fixture rather
//! than reasoning about the arithmetic — the boundary is where the mistake
//! would be. It needs a decoder, so it lives behind the `ffmpeg` feature and
//! runs only under `cargo test --workspace --features ffmpeg`.
//!
//! [`MAX_DECODE_BYTES`]: ravel_audio::mixdown::MAX_DECODE_BYTES

#![cfg(feature = "ffmpeg")]

use ravel_audio::mixdown::{MAX_DECODE_BYTES, decode_full_audio};
use ravel_media::encode::WavWriter;

/// Write a mono 48 kHz WAV holding `frames` sample frames of silence.
///
/// Silence rather than a signal: 128 MiB of anything takes the same time to
/// decode, and zeroes compress to nothing in the writer's own buffering.
fn silent_wav(path: &std::path::Path, frames: usize) {
    let mut writer = WavWriter::create(path, 48_000, 1).expect("fixture WAV");
    // In blocks, so the fixture never holds a second copy of itself.
    let block = vec![0.0_f32; 1 << 16];
    let mut written = 0;
    while written < frames {
        let take = block.len().min(frames - written);
        writer
            .write_samples(&block[..take])
            .expect("fixture samples");
        written += take;
    }
    writer.finish().expect("fixture finishes");
}

/// The cap in sample frames for a mono `f32` stream — the same arithmetic
/// `decode_full_audio` does.
const CAP_FRAMES: usize = MAX_DECODE_BYTES / 4;

/// One frame under the cap decodes, and the whole stream comes back.
#[test]
fn an_asset_at_the_limit_decodes_whole() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("at_the_limit.wav");
    silent_wav(&path, CAP_FRAMES - 1);

    let audio = decode_full_audio(&path, 0).expect("one frame under the cap is fine");
    assert_eq!(audio.frame_count(), CAP_FRAMES - 1);
}

/// One frame over it is refused, by name, rather than decoded to a truncated
/// buffer that would render as sound cutting out mid-shot.
#[test]
fn an_asset_over_the_limit_is_refused_rather_than_truncated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("over_the_limit.wav");
    silent_wav(&path, CAP_FRAMES + 1);

    let error = decode_full_audio(&path, 0).expect_err("past the cap");
    let message = format!("{error:#}");
    assert!(
        message.contains("128 MiB"),
        "the refusal has to name the limit so the reader knows what to change: {message}"
    );
}

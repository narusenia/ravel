// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end coverage for audio sample format normalization.
//!
//! `crates/ravel-media/src/audio_sample.rs` pins the arithmetic with pure unit
//! tests; these tests pin the other half — that the decoder hands that module
//! the right encoding, the right plane geometry, and the right channel count
//! for real codecs. They need the `ffmpeg` feature and the `ffmpeg` CLI (used
//! to synthesize the fixtures), so they do not run under the default
//! `cargo test --workspace`.
//!
//! Every fixture is a constant two-channel signal with **different values per
//! channel** — left `0.5`, right `0.25`. That asymmetry is the point: a
//! regression that reads only the first plane, swaps the channels, or shifts
//! the interleave changes these numbers, whereas an L == R fixture would hide
//! all three.

#[cfg(feature = "ffmpeg")]
mod ffmpeg_tests {
    use ravel_core::media::{MediaReader, StreamInfo};
    use ravel_media::decoder::FfmpegDecoder;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Nominal left/right amplitude of every fixture.
    const LEFT: f32 = 0.5;
    const RIGHT: f32 = 0.25;

    /// Write a one-second stereo fixture holding a constant `0.5` on the left
    /// and `0.25` on the right, encoded with `codec_args`.
    fn generate(dir: &Path, name: &str, codec_args: &[&str]) -> PathBuf {
        let path = dir.join(name);
        let mut args = vec![
            "-y",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs=0.5|0.25:d=1:s=44100",
        ];
        args.extend_from_slice(codec_args);
        let output = Command::new("ffmpeg")
            .args(&args)
            .arg(&path)
            .output()
            .expect("ffmpeg CLI not found");
        assert!(
            output.status.success(),
            "ffmpeg failed to generate {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        path
    }

    /// Decode the first audio stream of `path` and return the mean absolute
    /// amplitude of each channel, skipping the head of the buffer so codec
    /// priming (AAC pads the start with silence) does not skew the average.
    fn channel_levels(path: &Path) -> Vec<f32> {
        let mut decoder = FfmpegDecoder::open(path).expect("open fixture");
        let stream_index = decoder
            .info()
            .streams
            .iter()
            .find_map(|stream| match stream {
                StreamInfo::Audio(audio) => Some(audio.stream_index),
                _ => None,
            })
            .expect("fixture has an audio stream");

        let buffer = decoder
            .decode_audio_chunk(stream_index, 0, 40_000)
            .expect("decode fixture");
        assert_eq!(buffer.channels, 2, "fixture is stereo");
        assert_eq!(buffer.sample_rate, 44_100);

        let channels = buffer.channels as usize;
        let frames = buffer.data.len() / channels;
        assert!(frames > 20_000, "expected a second of audio, got {frames}");

        let skip = 8_192; // past any encoder priming
        let mut sums = vec![0.0_f64; channels];
        for frame in skip..frames {
            for (channel, sum) in sums.iter_mut().enumerate() {
                let sample = buffer.data[frame * channels + channel];
                assert!(sample.is_finite(), "decoded a non-finite sample");
                assert!(
                    sample == 0.0 || sample.abs() >= f32::MIN_POSITIVE,
                    "decoded a denormal sample: {sample:e}"
                );
                *sum += f64::from(sample.abs());
            }
        }
        let counted = (frames - skip) as f64;
        sums.iter().map(|sum| (sum / counted) as f32).collect()
    }

    /// Assert both channels survived decoding with their own amplitude.
    fn assert_stereo_levels(path: &Path, tolerance: f32, label: &str) {
        let levels = channel_levels(path);
        assert!(
            (levels[0] - LEFT).abs() < tolerance,
            "{label}: left channel is {} not {LEFT}",
            levels[0]
        );
        assert!(
            (levels[1] - RIGHT).abs() < tolerance,
            "{label}: right channel is {} not {RIGHT}",
            levels[1]
        );
    }

    /// Packed integer and float PCM: the formats a bare WAV file carries.
    ///
    /// Before sample formats were honoured, every integer variant here was
    /// reinterpreted as `f32` — producing huge values and denormals that pegged
    /// a CPU core in the mixer — while `pcm_f32le` happened to work.
    #[test]
    fn packed_pcm_formats_decode_to_their_nominal_levels() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, codec, tolerance) in [
            ("s16.wav", "pcm_s16le", 1e-4),
            ("s32.wav", "pcm_s32le", 1e-4),
            ("f32.wav", "pcm_f32le", 1e-6),
            ("f64.wav", "pcm_f64le", 1e-6),
            // 8-bit quantization is coarse: one step is 1/128.
            ("u8.wav", "pcm_u8", 1.0 / 128.0),
        ] {
            let path = generate(dir.path(), name, &["-c:a", codec]);
            assert_stereo_levels(&path, tolerance, codec);
        }
    }

    /// Planar formats — the case that used to lose every channel but the
    /// first, because FFmpeg leaves `AVFrame::linesize[i > 0]` at zero for
    /// audio and the old code sized each plane from it.
    ///
    /// AAC (planar `f32`) is what the audio track of an ordinary video file
    /// decodes to, which is why video audio played on one side only.
    #[test]
    fn planar_formats_keep_every_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Lossy: AAC only has to land near the original amplitudes.
        let aac = generate(dir.path(), "aac.m4a", &["-c:a", "aac"]);
        assert_stereo_levels(&aac, 0.02, "aac (fltp)");

        // Lossless planar integers.
        let flac = generate(dir.path(), "planar.flac", &["-c:a", "flac"]);
        assert_stereo_levels(&flac, 1e-4, "flac");

        // Force planar 16-bit through a codec that always uses s16p.
        let mp2 = generate(dir.path(), "s16p.mp2", &["-c:a", "mp2", "-b:a", "384k"]);
        assert_stereo_levels(&mp2, 0.05, "mp2 (s16p)");
    }

    /// Mono sources stay mono: the decoder reports one channel and one sample
    /// per frame, leaving the up-mix to the mixer.
    #[test]
    fn mono_pcm_keeps_a_single_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mono.wav");
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "aevalsrc=exprs=0.5:d=1:s=44100",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg CLI not found");
        assert!(
            output.status.success(),
            "ffmpeg failed to generate mono.wav"
        );

        let mut decoder = FfmpegDecoder::open(&path).expect("open fixture");
        let stream_index = decoder
            .info()
            .streams
            .iter()
            .find_map(|stream| match stream {
                StreamInfo::Audio(audio) => Some(audio.stream_index),
                _ => None,
            })
            .expect("fixture has an audio stream");
        let buffer = decoder
            .decode_audio_chunk(stream_index, 0, 1_024)
            .expect("decode fixture");
        assert_eq!(buffer.channels, 1);
        assert_eq!(buffer.data.len(), 1_024);
        for sample in buffer.data.iter() {
            assert!((sample - LEFT).abs() < 1e-4, "mono sample {sample}");
        }
    }
}

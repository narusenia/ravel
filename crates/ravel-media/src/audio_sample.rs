// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Normalization of decoded audio samples to packed `f32`.
//!
//! Every audio decoder emits whatever sample format its codec uses natively:
//! WAV PCM arrives as packed 16-bit integers, FLAC as 32-bit integers, AAC as
//! planar `f32`, and older material as unsigned 8-bit. Everything downstream
//! of the decoder — [`AudioBuffer`](ravel_core::types::AudioBuffer), the
//! mixer, the CPAL callback — works exclusively on **packed `f32`** in the
//! nominal `-1.0..=1.0` range. This module is the one place that bridges the
//! two, so no other code has to know that `AVSampleFormat` exists.
//!
//! It is deliberately free of FFmpeg types. The decoder maps the frame's
//! `AVSampleFormat` onto [`SampleEncoding`] and hands over the raw plane
//! bytes; the arithmetic lives here, where it can be pinned by unit tests
//! that need neither the `ffmpeg` feature nor a media fixture.
//!
//! # Why the sample rate is left alone
//!
//! Only the sample *format* and the channel interleaving are normalized here.
//! The sample rate is passed through untouched, because rate conversion is
//! already owned by `ravel_audio::resampler`, which converts each track once
//! against the audio device's rate. Converting here as well would resample
//! twice — costing quality and time — and would make the decoded frame count
//! stop matching the container's own sample timestamps, which the seek
//! arithmetic in [`crate::decoder`] relies on.

/// Numeric encoding of a single decoded audio sample.
///
/// The byte order is the host's, matching FFmpeg: `AVSampleFormat` describes
/// native-endian data, never a fixed endianness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding {
    /// Unsigned 8-bit PCM. Silence is 128, not 0.
    U8,
    /// Signed 16-bit PCM — the usual WAV and MP3 output.
    S16,
    /// Signed 32-bit PCM.
    S32,
    /// Signed 64-bit PCM.
    S64,
    /// 32-bit float, already in the target representation.
    F32,
    /// 64-bit float, narrowed to `f32`.
    F64,
}

impl SampleEncoding {
    /// Width of one sample in bytes.
    pub const fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::S16 => 2,
            Self::S32 => 4,
            Self::S64 => 8,
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

/// Layout and encoding of one decoded frame's raw plane bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSpec {
    /// Numeric encoding of every sample in the planes.
    pub encoding: SampleEncoding,
    /// `true` when each channel occupies its own plane, `false` when all
    /// channels are interleaved into plane 0.
    pub planar: bool,
    /// Channel count the planes are laid out for.
    pub channels: usize,
    /// Sample frames present in the planes.
    pub samples: usize,
}

/// Convert raw decoded plane bytes into packed (interleaved) `f32`.
///
/// `planes` holds one entry per plane: `spec.channels` entries for planar
/// input, a single entry for packed input. The result always has
/// `spec.samples * out_channels` samples in interleaved channel order
/// (`L, R, L, R, …` for stereo), and every value has passed through
/// [`sanitize`].
///
/// The conversion is deliberately defensive: a decoded frame is bug- and
/// input-reachable data, so a plane that is shorter than the declared
/// geometry, a plane the caller failed to collect, and a channel the frame
/// does not carry all become silence rather than a panic or a read of
/// unrelated memory.
///
/// `out_channels` may differ from `spec.channels`. Mapping onto the caller's
/// stride — dropping surplus source channels, leaving missing ones silent —
/// keeps the interleave aligned when a frame disagrees with its stream
/// header. Writing the frame's own channel count into a buffer declared with
/// a different one would shift every following frame and smear the channels
/// into each other.
pub fn to_packed_f32(planes: &[&[u8]], spec: FrameSpec, out_channels: usize) -> Vec<f32> {
    let mut out = vec![0.0_f32; spec.samples * out_channels];
    if out.is_empty() || spec.channels == 0 {
        return out;
    }

    let width = spec.encoding.bytes();
    for channel in 0..spec.channels.min(out_channels) {
        // Planar input has one plane per channel and a stride of one sample;
        // packed input keeps every channel in plane 0, `channels` apart.
        let (plane, first, stride) = if spec.planar {
            (planes.get(channel), 0, 1)
        } else {
            (planes.first(), channel, spec.channels)
        };
        let Some(plane) = plane else { continue };

        for sample in 0..spec.samples {
            let start = (first + sample * stride) * width;
            let Some(bytes) = plane.get(start..start + width) else {
                // Truncated plane: this channel stays silent from here on.
                break;
            };
            out[sample * out_channels + channel] = sanitize(decode_sample(bytes, spec.encoding));
        }
    }

    out
}

/// Replace values the mixer must never see with silence.
///
/// NaN and infinities survive every later gain multiply and turn the whole
/// mix — not just the offending track — into noise. Denormals are worse in
/// practice: multiplying them is 10–100× slower than normal arithmetic on
/// common FPUs, so a buffer full of them starves the audio prep thread, and
/// the playhead visibly stutters while the output turns to crackle. Both are
/// far more expensive than the single branch that removes them here, once,
/// on the decode path.
///
/// Correct decoding of a well-formed file never produces either. This guards
/// against damaged media and against a future bug in the conversion above
/// degrading into an unresponsive application instead of quiet audio.
#[inline]
pub fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() && sample.abs() >= f32::MIN_POSITIVE {
        sample
    } else {
        0.0
    }
}

/// Decode one sample from exactly `encoding.bytes()` bytes.
///
/// Integer formats are scaled by the magnitude of their most negative value,
/// which is how FFmpeg's own `swresample` normalizes them: full-scale
/// positive stops just short of `1.0`, and `i16::MIN` maps to exactly `-1.0`.
#[inline]
fn decode_sample(bytes: &[u8], encoding: SampleEncoding) -> f32 {
    debug_assert_eq!(bytes.len(), encoding.bytes());
    match encoding {
        SampleEncoding::U8 => (f32::from(bytes[0]) - 128.0) / 128.0,
        SampleEncoding::S16 => f32::from(i16::from_ne_bytes([bytes[0], bytes[1]])) / 32_768.0,
        SampleEncoding::S32 => {
            let raw = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            raw as f32 / 2_147_483_648.0
        }
        SampleEncoding::S64 => {
            let raw = i64::from_ne_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            (raw as f64 / 9_223_372_036_854_775_808.0) as f32
        }
        SampleEncoding::F32 => f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        SampleEncoding::F64 => f64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]) as f32,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

// The expected `f32` values below are computed by hand from the documented
// scaling, never by re-running the implementation. Inputs are built with
// `to_ne_bytes` so the byte order matches what FFmpeg hands us on any host;
// `s16_packed_byte_order_is_pinned` additionally nails the concrete little-
// endian layout of every target Ravel ships on.
#[cfg(test)]
mod tests {
    use super::*;

    /// Build one packed plane from interleaved integer sample values.
    fn packed<const N: usize>(values: [i16; N]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn spec(encoding: SampleEncoding, planar: bool, channels: usize, samples: usize) -> FrameSpec {
        FrameSpec {
            encoding,
            planar,
            channels,
            samples,
        }
    }

    #[test]
    fn s16_packed_keeps_left_right_order() {
        // 16 384 = half of 32 768 → 0.5; -32 768 is full-scale negative.
        let plane = packed([16_384, -8_192, -32_768, 32_767]);
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 2, 2), 2);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 0.5); // frame 0 L
        assert_eq!(out[1], -0.25); // frame 0 R
        assert_eq!(out[2], -1.0); // frame 1 L
        assert!((out[3] - 0.999_969_5).abs() < 1e-6); // frame 1 R: 32767/32768
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn s16_packed_byte_order_is_pinned() {
        // 0x4000 = 16 384 → 0.5, 0xE000 = -8 192 → -0.25, little-endian.
        let plane = [0x00, 0x40, 0x00, 0xE0];
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 2, 1), 2);
        assert_eq!(out, vec![0.5, -0.25]);
    }

    #[test]
    fn s16_planar_interleaves_plane_order_into_channel_order() {
        // Plane 0 is the left channel, plane 1 the right. Before the fix the
        // right plane was read as a zero-length slice and every non-first
        // channel decoded to silence — a stereo file played on one side.
        let left = packed([16_384, 8_192]);
        let right = packed([-16_384, -8_192]);
        let out = to_packed_f32(&[&left, &right], spec(SampleEncoding::S16, true, 2, 2), 2);
        assert_eq!(out, vec![0.5, -0.5, 0.25, -0.25]);
    }

    #[test]
    fn s32_packed_scales_by_the_signed_range() {
        let plane: Vec<u8> = [1_073_741_824_i32, -2_147_483_648, 0]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S32, false, 3, 1), 3);
        assert_eq!(out, vec![0.5, -1.0, 0.0]);
    }

    #[test]
    fn s64_packed_scales_by_the_signed_range() {
        let plane: Vec<u8> = [4_611_686_018_427_387_904_i64, -9_223_372_036_854_775_808]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S64, false, 2, 1), 2);
        assert_eq!(out, vec![0.5, -1.0]);
    }

    #[test]
    fn u8_is_centred_on_128() {
        // 128 is silence, 255 is +127/128, 0 is -1.0, 64 is -0.5.
        let plane = [128_u8, 255, 0, 64];
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::U8, false, 1, 4), 1);
        assert_eq!(out, vec![0.0, 127.0 / 128.0, -1.0, -0.5]);
    }

    #[test]
    fn u8_planar_keeps_channel_order() {
        let left = [255_u8, 128];
        let right = [0_u8, 192];
        let out = to_packed_f32(&[&left, &right], spec(SampleEncoding::U8, true, 2, 2), 2);
        assert_eq!(out, vec![127.0 / 128.0, -1.0, 0.0, 0.5]);
    }

    #[test]
    fn f32_packed_passes_values_through() {
        let plane: Vec<u8> = [0.75_f32, -0.125]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::F32, false, 2, 1), 2);
        assert_eq!(out, vec![0.75, -0.125]);
    }

    #[test]
    fn f32_planar_reads_every_plane() {
        // The AAC / video-audio case: planar f32, one plane per channel.
        let left: Vec<u8> = [0.5_f32, 0.25]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let right: Vec<u8> = [-0.5_f32, -0.25]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let out = to_packed_f32(&[&left, &right], spec(SampleEncoding::F32, true, 2, 2), 2);
        assert_eq!(out, vec![0.5, -0.5, 0.25, -0.25]);
    }

    #[test]
    fn f64_is_narrowed_to_f32() {
        let plane: Vec<u8> = [0.25_f64, -1.0]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::F64, false, 2, 1), 2);
        assert_eq!(out, vec![0.25, -1.0]);
    }

    #[test]
    fn mono_stays_mono() {
        let plane = packed([16_384, -16_384]);
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 1, 2), 1);
        assert_eq!(out, vec![0.5, -0.5]);
    }

    #[test]
    fn zero_samples_produces_no_output() {
        let plane = packed([16_384]);
        assert!(to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 2, 0), 2).is_empty());
    }

    #[test]
    fn zero_channels_produces_no_output() {
        let plane = packed([16_384]);
        assert!(to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 0, 4), 0).is_empty());
        // A frame that reports no channels cannot fill a stereo stride.
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 0, 2), 2);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn truncated_packed_plane_is_padded_with_silence() {
        // Two frames declared, one and a half present.
        let plane = packed([16_384, 8_192, -16_384]);
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 2, 2), 2);
        assert_eq!(out, vec![0.5, 0.25, -0.5, 0.0]);
    }

    #[test]
    fn truncated_planar_plane_only_silences_that_channel() {
        let left = packed([16_384, 8_192]);
        let right = packed([-16_384]); // one frame short
        let out = to_packed_f32(&[&left, &right], spec(SampleEncoding::S16, true, 2, 2), 2);
        assert_eq!(out, vec![0.5, -0.5, 0.25, 0.0]);
    }

    #[test]
    fn missing_planar_plane_is_silent_not_a_panic() {
        let left = packed([16_384, 8_192]);
        let out = to_packed_f32(&[&left], spec(SampleEncoding::S16, true, 2, 2), 2);
        assert_eq!(out, vec![0.5, 0.0, 0.25, 0.0]);
    }

    #[test]
    fn surplus_source_channels_are_dropped_without_shifting_the_interleave() {
        // A 3-channel frame written into a stereo buffer keeps L and R in
        // place; the extra channel is discarded rather than pushing R into
        // the next frame's L.
        let plane = packed([16_384, 8_192, 4_096, -16_384, -8_192, -4_096]);
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 3, 2), 2);
        assert_eq!(out, vec![0.5, 0.25, -0.5, -0.25]);
    }

    #[test]
    fn missing_source_channels_stay_silent() {
        // A mono frame written into a stereo buffer fills the left channel
        // only; the mixer, not the decoder, decides how to up-mix.
        let plane = packed([16_384, 8_192]);
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::S16, false, 1, 2), 2);
        assert_eq!(out, vec![0.5, 0.0, 0.25, 0.0]);
    }

    #[test]
    fn non_finite_and_denormal_input_becomes_silence() {
        let denormal = f32::from_bits(1); // ~1.4e-45, the smallest denormal
        assert!(denormal != 0.0 && !denormal.is_normal());
        let plane: Vec<u8> = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            denormal,
            -denormal,
            0.5,
        ]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
        let out = to_packed_f32(&[&plane], spec(SampleEncoding::F32, false, 1, 6), 1);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn sanitize_keeps_normal_values_and_the_smallest_normal() {
        assert_eq!(sanitize(0.5), 0.5);
        assert_eq!(sanitize(-1.0), -1.0);
        assert_eq!(sanitize(f32::MIN_POSITIVE), f32::MIN_POSITIVE);
        assert_eq!(sanitize(-f32::MIN_POSITIVE), -f32::MIN_POSITIVE);
        assert_eq!(sanitize(0.0), 0.0);
        assert_eq!(sanitize(f32::NAN), 0.0);
        assert_eq!(sanitize(f32::INFINITY), 0.0);
        assert_eq!(sanitize(f32::from_bits(1)), 0.0);
    }

    #[test]
    fn sample_widths_match_their_encodings() {
        assert_eq!(SampleEncoding::U8.bytes(), 1);
        assert_eq!(SampleEncoding::S16.bytes(), 2);
        assert_eq!(SampleEncoding::S32.bytes(), 4);
        assert_eq!(SampleEncoding::S64.bytes(), 8);
        assert_eq!(SampleEncoding::F32.bytes(), 4);
        assert_eq!(SampleEncoding::F64.bytes(), 8);
    }
}

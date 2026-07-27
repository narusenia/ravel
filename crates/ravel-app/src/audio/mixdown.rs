// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document → mixer track mapping for audio playback
//! (`docs/implementation/audio-plan.md`, unit 3).
//!
//! [`AudioMixdown`] turns the audio-carrying layers of the active
//! composition into [`TrackSpec`]s (desired mixer state) and, once the
//! source audio has been decoded and cached, into concrete
//! [`ravel_audio::Track`]s for `AudioCommand::SetTrack`.
//!
//! Unit contract (enforced here, relied on by the mixer):
//!
//! - `Track::start_frame`, `TrackGain::Curve` indices, and fade lengths are
//!   **sample frames at the engine's output rate**. Layer timing is in
//!   composition frames, so every placement value is converted through
//!   `frame / comp_fps × output_rate`. Sampling the gain automation at the
//!   source rate instead would finish fades early on every track whose
//!   media rate differs from the device rate (see the [`TrackGain`] docs).
//! - `Track::samples` keep the source sample rate; `SetTrack` resamples
//!   them in the engine. The gain curve is automation, not audio, and is
//!   never resampled — it is evaluated straight onto output-rate frames.
//!
//! The pure functions in this module are GPUI-free so the mapping is
//! testable headlessly; [`crate::audio::AudioService`] owns the cache, the
//! diffing, and the decode scheduling.

use ravel_audio::{Track, TrackGain};
use ravel_core::animation::AnimationChannel;
use ravel_core::animation::channel::ChannelSource;
use ravel_core::composition::Composition;
use ravel_core::eval::EvalContext;
use ravel_core::id::LayerId;
use ravel_core::types::FrameRate;
use std::sync::Arc;

/// Cap on decoded audio held in memory per asset (decision 8 of the plan:
/// full-length decode, warn-and-skip past the limit). 128 MiB of `f32`
/// samples covers about 5 minutes of 48 kHz stereo (~115 MiB) with
/// headroom.
pub const MAX_DECODE_BYTES: usize = 128 * 1024 * 1024;

/// Cache key for decoded audio: the document asset plus the stream inside
/// its container. The resolved path is recorded in [`DecodedAudio`] for
/// diagnostics; identity stays the asset id so a relink does not silently
/// swap content under a playing track (the spec diff re-sends on change).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Key into `Document::media_assets`.
    pub asset_id: String,
    /// Audio stream number inside the container.
    pub stream_index: usize,
}

/// One fully decoded audio stream, shared between the cache and every
/// track built from it.
#[derive(Clone, Debug)]
pub struct DecodedAudio {
    /// Interleaved samples at `sample_rate`.
    pub samples: Arc<[f32]>,
    /// Sample rate of `samples` in Hz.
    pub sample_rate: u32,
    /// Channel count of `samples`.
    pub channels: u32,
}

impl DecodedAudio {
    /// Number of per-channel sample frames.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

/// Desired mixer state for one audio-carrying layer, in output-rate units.
///
/// Equality drives the diff in [`crate::audio::AudioService`]: a spec that
/// compares equal to the last sent one produces no `AudioCommand`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSpec {
    /// Owning layer; also the `TrackId` (`layer_id.raw()`).
    pub layer_id: LayerId,
    /// Key into `Document::media_assets`.
    pub asset_id: String,
    /// Audio stream number inside the container.
    pub stream_index: usize,
    /// Output-timeline sample frame where the track starts playing.
    pub start_frame: u64,
    /// First layer-local source frame (composition frames) that is heard:
    /// the layer's `in_frame` plus whatever a negative `start_frame` trims
    /// off the head.
    pub source_in_frames: u64,
    /// Layer-local source end (composition frames, half-open).
    /// `u64::MAX` when the layer does not trim the tail.
    pub source_out_frames: u64,
    /// Volume automation, evaluated in layer-local composition frames.
    pub gain: AnimationChannel,
    /// Fade-in length in output-rate sample frames.
    pub fade_in_frames: u64,
    /// Fade-out length in output-rate sample frames.
    pub fade_out_frames: u64,
    /// Effective mute: the layer mute, the audio-only mute, or silenced by
    /// another layer's solo (same rule as the compositor's `active_layers`).
    pub muted: bool,
    /// Layer solo flag (the mixer applies the "any solo ⇒ only solos" rule).
    pub solo: bool,
}

impl TrackSpec {
    /// Cache key for this spec's source audio.
    pub fn cache_key(&self) -> CacheKey {
        CacheKey {
            asset_id: self.asset_id.clone(),
            stream_index: self.stream_index,
        }
    }

    /// The parts of the spec that decide the expensive build products (the
    /// sample slice and the gain curve). Placement/mute/fade changes reuse
    /// the previously built track; a build-key change rebuilds it.
    fn build_key(&self) -> (&str, usize, u64, u64, &AnimationChannel) {
        (
            self.asset_id.as_str(),
            self.stream_index,
            self.source_in_frames,
            self.source_out_frames,
            &self.gain,
        )
    }

    /// Whether `other` can reuse this spec's built track, patching only the
    /// cheap fields (timeline placement, fades, mute/solo).
    pub fn shares_build_with(&self, other: &TrackSpec) -> bool {
        self.layer_id == other.layer_id && self.build_key() == other.build_key()
    }
}

/// Stateless document → track mapper (see the module docs).
pub struct AudioMixdown;

impl AudioMixdown {
    /// Collect the desired mixer state of every audio-carrying layer in
    /// `comp`, in output-rate units.
    ///
    /// Solo/mute follows the compositor's `active_layers` rule: a muted
    /// layer is silent, and when any layer (audio or not) is soloed, every
    /// non-solo layer is silent. Parenting deliberately has no effect on
    /// audio — the parent's transform means nothing to sound.
    pub fn desired_tracks(comp: &Composition, output_rate: u32) -> Vec<TrackSpec> {
        let fps = comp.frame_rate;
        let any_solo = comp.layers.iter().any(|layer| layer.solo);
        comp.layers
            .iter()
            .filter_map(|layer| {
                let audio = layer.audio.as_ref()?;
                // A negative start trims the head of the source instead of
                // moving the track before the timeline origin.
                let head_skip = layer.start_frame.min(0).unsigned_abs();
                let timeline_start = layer.start_frame.max(0) as u64;
                Some(TrackSpec {
                    layer_id: layer.id,
                    asset_id: audio.asset_id.clone(),
                    stream_index: audio.stream_index,
                    start_frame: comp_frames_to_rate(timeline_start, fps, output_rate),
                    source_in_frames: layer.in_frame + head_skip,
                    source_out_frames: if layer.out_frame > layer.in_frame {
                        layer.out_frame
                    } else {
                        u64::MAX
                    },
                    gain: audio.gain.clone(),
                    fade_in_frames: comp_frames_to_rate(audio.fade_in_frames, fps, output_rate),
                    fade_out_frames: comp_frames_to_rate(audio.fade_out_frames, fps, output_rate),
                    muted: layer.muted || audio.audio_muted || (any_solo && !layer.solo),
                    solo: layer.solo,
                })
            })
            .collect()
    }

    /// Build a concrete mixer [`Track`] from a spec and its decoded source.
    ///
    /// Returns the track plus the sample rate of `Track::samples` (the
    /// engine resamples on `SetTrack`). `None` when the trimmed source
    /// range is empty — the layer has nothing audible to contribute.
    pub fn build_track(
        spec: &TrackSpec,
        decoded: &DecodedAudio,
        comp_fps: FrameRate,
        output_rate: u32,
    ) -> Option<(Track, u32)> {
        let channels = decoded.channels;
        let source_rate = decoded.sample_rate;
        if channels == 0 || source_rate == 0 {
            return None;
        }
        let total_frames = decoded.frame_count() as u64;
        let in_frame =
            comp_frames_to_rate(spec.source_in_frames, comp_fps, source_rate).min(total_frames);
        let out_frame = if spec.source_out_frames == u64::MAX {
            total_frames
        } else {
            comp_frames_to_rate(spec.source_out_frames, comp_fps, source_rate).min(total_frames)
        };
        if out_frame <= in_frame {
            return None;
        }

        let ch = channels as usize;
        let samples: Arc<[f32]> = if in_frame == 0 && out_frame == total_frames {
            // Untrimmed: share the cached buffer with every user of the asset.
            decoded.samples.clone()
        } else {
            let start = in_frame as usize * ch;
            let end = out_frame as usize * ch;
            Arc::from(&decoded.samples[start..end])
        };

        let source_frames = out_frame - in_frame;
        let output_frames = source_frames
            .saturating_mul(output_rate as u64)
            .div_ceil(source_rate as u64);

        let gain = match &spec.gain.source {
            // No Vec for the common case of a static volume.
            ChannelSource::Constant(value) => TrackGain::Constant(*value),
            _ => TrackGain::Curve(sample_gain_curve(
                spec,
                comp_fps,
                output_rate,
                output_frames,
            )),
        };

        let mut track = Track::new(spec.layer_id.raw(), samples, channels);
        track.start_frame = spec.start_frame as usize;
        track.gain = gain;
        track.muted = spec.muted;
        track.solo = spec.solo;
        track.fade_in_frames = spec.fade_in_frames as usize;
        track.fade_out_frames = spec.fade_out_frames as usize;
        Some((track, source_rate))
    }
}

/// Evaluate the gain channel onto one value per output-rate sample frame.
///
/// The channel lives in layer-local composition frames, so output frame `f`
/// maps to `source_in + f × comp_fps / output_rate`. Sources that cannot be
/// resolved outside graph evaluation (node outputs, expressions) evaluate
/// to the channel's documented default value.
fn sample_gain_curve(
    spec: &TrackSpec,
    comp_fps: FrameRate,
    output_rate: u32,
    output_frames: u64,
) -> Arc<[f32]> {
    // Resolution is irrelevant to scalar channel evaluation.
    let ctx = EvalContext::new(0, comp_fps, (1, 1));
    let num = comp_fps.num.max(1) as u64;
    let den = comp_fps.den.max(1) as u64;
    (0..output_frames)
        .map(|frame| {
            let local = spec.source_in_frames + frame * num / (den * output_rate as u64);
            spec.gain.evaluate(local as f64, &ctx)
        })
        .collect::<Vec<_>>()
        .into()
}

/// Convert `frames` at `fps` into sample frames at `rate` (truncating, like
/// the playback clock's own frame arithmetic).
fn comp_frames_to_rate(frames: u64, fps: FrameRate, rate: u32) -> u64 {
    let num = fps.num.max(1) as u128;
    let value = frames as u128 * rate as u128 * fps.den.max(1) as u128 / num;
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Decode a whole audio stream into memory (decision 8 of the plan).
///
/// Assets past [`MAX_DECODE_BYTES`] are rejected with an error the caller
/// surfaces as a warning; everything else returns interleaved `f32` samples
/// at the container's own rate and channel count.
#[cfg(feature = "ffmpeg")]
pub fn decode_full_audio(
    path: &std::path::Path,
    stream_index: usize,
) -> anyhow::Result<DecodedAudio> {
    use ravel_core::media::MediaReader;

    let mut decoder = ravel_media::decoder::FfmpegDecoder::open(path)
        .map_err(|err| anyhow::anyhow!("open {}: {err}", path.display()))?;
    // Learn the stream's rate/channels with a one-frame probe, then decode
    // up to the memory cap in a single pass.
    let probe = decoder
        .decode_audio_chunk(stream_index, 0, 1)
        .map_err(|err| anyhow::anyhow!("decode probe: {err}"))?;
    let channels = probe.channels.max(1);
    let cap_frames = (MAX_DECODE_BYTES / size_of::<f32>() / channels as usize).max(1);
    let buffer = decoder
        .decode_audio_chunk(stream_index, 0, cap_frames)
        .map_err(|err| anyhow::anyhow!("decode: {err}"))?;
    if buffer.data.len() / channels as usize == cap_frames {
        // Exactly at the cap: the stream may continue past it.
        let extra = decoder.decode_audio_chunk(stream_index, cap_frames as u64, 1)?;
        if !extra.data.is_empty() {
            anyhow::bail!(
                "audio stream exceeds the {} MiB in-memory decode limit",
                MAX_DECODE_BYTES / 1024 / 1024
            );
        }
    }
    Ok(DecodedAudio {
        samples: buffer.data,
        sample_rate: buffer.sample_rate,
        channels: buffer.channels,
    })
}

/// Stub for builds without FFmpeg: audio decode is unavailable, so every
/// asset is reported as undecodable (the caller warns and skips the track,
/// exactly like an over-limit asset).
#[cfg(not(feature = "ffmpeg"))]
pub fn decode_full_audio(
    path: &std::path::Path,
    _stream_index: usize,
) -> anyhow::Result<DecodedAudio> {
    let _ = path;
    anyhow::bail!("the `ffmpeg` feature of ravel-app is disabled")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::composition::{AudioSource, Layer};
    use ravel_core::graph::Graph;
    use ravel_core::id::LayerId;

    const FPS_30: FrameRate = FrameRate { num: 30, den: 1 };
    const OUTPUT_RATE: u32 = 48_000;

    fn audio_layer(id: u64, start: i64, audio: AudioSource) -> Layer {
        let mut layer = Layer::new(LayerId::new(id), format!("layer {id}"), Graph::new())
            .with_time(start, 0, 300);
        layer.audio = Some(audio);
        layer
    }

    fn comp(layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(
            ravel_core::id::CompId::new(1),
            "comp",
            (1920, 1080),
            FPS_30,
            300,
        );
        comp.layers = layers.into_iter().collect();
        comp
    }

    fn decoded(frames: usize, channels: u32, sample_rate: u32) -> DecodedAudio {
        DecodedAudio {
            samples: vec![0.5; frames * channels as usize].into(),
            sample_rate,
            channels,
        }
    }

    #[test]
    fn start_frame_is_converted_to_output_rate() {
        // Layer starts at comp frame 30 = 1s at 30fps → 48 000 output frames.
        let spec = AudioMixdown::desired_tracks(
            &comp(vec![audio_layer(1, 30, AudioSource::new("a", 0))]),
            OUTPUT_RATE,
        );
        assert_eq!(spec.len(), 1);
        assert_eq!(spec[0].start_frame, 48_000);
        assert_eq!(spec[0].source_in_frames, 0);
    }

    #[test]
    fn negative_start_trims_the_source_head() {
        // Layer starts 15 comp frames before the origin: the track starts at
        // output frame 0 and the first 15 source frames are skipped.
        let spec = AudioMixdown::desired_tracks(
            &comp(vec![audio_layer(1, -15, AudioSource::new("a", 0))]),
            OUTPUT_RATE,
        );
        assert_eq!(spec[0].start_frame, 0);
        assert_eq!(spec[0].source_in_frames, 15);
    }

    #[test]
    fn solo_mute_matches_the_compositor_rule() {
        let mut quiet = audio_layer(1, 0, AudioSource::new("a", 0));
        quiet.muted = true;
        let mut soloed_video = Layer::new(LayerId::new(2), "video", Graph::new());
        soloed_video.solo = true; // no audio: still silences every other layer
        let audible = audio_layer(3, 0, AudioSource::new("b", 0));
        let mut audio_muted = audio_layer(4, 0, AudioSource::new("c", 0));
        audio_muted.audio.as_mut().unwrap().audio_muted = true;

        let specs = AudioMixdown::desired_tracks(
            &comp(vec![quiet, soloed_video, audible, audio_muted]),
            OUTPUT_RATE,
        );
        assert_eq!(specs.len(), 3);
        assert!(specs[0].muted, "layer mute");
        assert!(specs[1].muted, "silenced by another layer's solo");
        assert!(specs[2].muted, "audio-only mute");
        assert!(specs.iter().all(|s| !s.solo));
    }

    #[test]
    fn layers_without_audio_produce_no_specs() {
        let silent = Layer::new(LayerId::new(1), "solid", Graph::new());
        assert!(AudioMixdown::desired_tracks(&comp(vec![silent]), OUTPUT_RATE).is_empty());
    }

    #[test]
    fn fades_are_converted_to_output_rate() {
        let mut audio = AudioSource::new("a", 0);
        audio.fade_in_frames = 15; // 0.5s at 30fps
        audio.fade_out_frames = 30; // 1s
        let spec = AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        assert_eq!(spec[0].fade_in_frames, 24_000);
        assert_eq!(spec[0].fade_out_frames, 48_000);
    }

    #[test]
    fn constant_gain_stays_constant() {
        let mut audio = AudioSource::new("a", 0);
        audio.gain = AnimationChannel::constant(0.25);
        let specs =
            AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        let source = decoded(100, 2, 48_000);
        let (track, rate) = AudioMixdown::build_track(&specs[0], &source, FPS_30, OUTPUT_RATE)
            .expect("audible range");
        assert_eq!(rate, 48_000);
        assert!(matches!(track.gain, TrackGain::Constant(g) if g == 0.25));
        // Untrimmed: the cached buffer is shared, not copied.
        assert!(Arc::ptr_eq(&track.samples, &source.samples));
    }

    #[test]
    fn keyframed_gain_is_sampled_at_the_output_rate() {
        // 0 → 1 over 30 comp frames (1s). At 48 kHz output the ramp must
        // span 48 000 curve entries, not 30 and not the source-rate count.
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(30, 1.0, Interpolation::Linear);
        let mut audio = AudioSource::new("a", 0);
        audio.gain = AnimationChannel::keyframes(curve);

        let specs =
            AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        // 2s of 44.1 kHz source: the curve length follows the output rate.
        let (track, rate) = AudioMixdown::build_track(
            &specs[0],
            &decoded(2 * 44_100, 2, 44_100),
            FPS_30,
            OUTPUT_RATE,
        )
        .expect("audible range");
        assert_eq!(rate, 44_100);
        let TrackGain::Curve(gain) = track.gain else {
            panic!("keyframed gain must become a curve");
        };
        assert_eq!(gain.len(), 2 * OUTPUT_RATE as usize);
        assert!(gain[0].abs() < 1e-4, "ramp starts at 0");
        // One output second in ≈ one comp second in ≈ gain 1.0 at the end
        // of the keyframe span.
        let one_second = OUTPUT_RATE as usize;
        assert!((gain[one_second] - 1.0).abs() < 0.05, "gain at 1s ≈ 1.0");
        // The ramp finishes at 1s (gain 1.0) and holds afterwards; at the
        // source-rate position (44 100) it is still mid-ramp (~0.9), so the
        // curve was clearly not sampled against the source rate.
        assert!((gain[44_100] - 0.9).abs() < 0.05, "no source-rate aliasing");
    }

    #[test]
    fn trimmed_source_is_sliced() {
        let mut layer = Layer::new(LayerId::new(1), "a", Graph::new()).with_time(0, 30, 60);
        layer.audio = Some(AudioSource::new("a", 0));
        let specs = AudioMixdown::desired_tracks(&comp(vec![layer]), OUTPUT_RATE);
        assert_eq!(specs[0].source_in_frames, 30);
        assert_eq!(specs[0].source_out_frames, 60);
        // 30..60 comp frames at 30fps = 1s..2s of a 3s 48kHz source.
        let (track, _) = AudioMixdown::build_track(
            &specs[0],
            &decoded(3 * 48_000, 2, 48_000),
            FPS_30,
            OUTPUT_RATE,
        )
        .expect("audible range");
        assert_eq!(track.frame_count(), 48_000);
    }

    #[test]
    fn empty_trim_range_builds_nothing() {
        let mut spec = AudioMixdown::desired_tracks(
            &comp(vec![audio_layer(1, 0, AudioSource::new("a", 0))]),
            OUTPUT_RATE,
        )[0]
        .clone();
        spec.source_in_frames = 300;
        spec.source_out_frames = 300;
        assert!(
            AudioMixdown::build_track(&spec, &decoded(100, 2, 48_000), FPS_30, OUTPUT_RATE)
                .is_none()
        );
    }

    #[test]
    fn build_key_sharing_ignores_placement_and_mute() {
        let base = AudioMixdown::desired_tracks(
            &comp(vec![audio_layer(1, 0, AudioSource::new("a", 0))]),
            OUTPUT_RATE,
        )[0]
        .clone();
        let mut moved = base.clone();
        moved.start_frame = 48_000;
        moved.muted = true;
        moved.fade_in_frames = 100;
        assert!(base.shares_build_with(&moved));

        let mut trimmed = base.clone();
        trimmed.source_in_frames = 10;
        assert!(!base.shares_build_with(&trimmed));

        let mut regained = base.clone();
        regained.gain = AnimationChannel::constant(0.5);
        assert!(!base.shares_build_with(&regained));
    }
}

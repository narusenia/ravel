// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document → mixer track mapping
//! (`docs/implementation/audio-plan.md`, unit 3).
//!
//! [`AudioMixdown`] turns the audio-carrying layers of a composition into
//! [`TrackSpec`]s (desired mixer state) and, once the source audio has been
//! decoded and cached, into concrete [`Track`]s for the
//! [`Mixer`](crate::Mixer) — whether that mixer feeds an output device
//! (`AudioCommand::SetTrack`) or an offline render.
//!
//! Unit contract (enforced here, relied on by the mixer):
//!
//! - `Track::start_frame`, `TrackGain::Curve` indices, and fade lengths are
//!   **sample frames at the engine's output rate**. Layer timing is in
//!   composition frames, so every placement value is converted through
//!   `frame / comp_fps × output_rate`. Sampling the gain automation at the
//!   source rate instead would finish fades early on every track whose
//!   media rate differs from the device rate (see the [`TrackGain`] docs).
//! - Cached audio and `Track::samples` are already at the engine output
//!   rate. The gain curve is automation, not audio, and is evaluated
//!   straight onto the same output-rate frames.
//!
//! The functions here are pure and GUI-free, so the mapping is testable
//! headlessly and is shared by both consumers: `ravel-app`'s `AudioService`
//! owns the cache, the diffing, and the decode scheduling for playback,
//! while the offline render path builds the same tracks in one pass.

use crate::{Track, TrackGain};
use ravel_core::animation::AnimationChannel;
use ravel_core::animation::channel::ChannelSource;
use ravel_core::composition::Composition;
use ravel_core::eval::EvalContext;
use ravel_core::id::{AssetId, LayerId};
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
///
/// The id, not the asset's display name: renaming an asset must not evict the
/// buffer decoded from the file it still points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Identity of the asset in `Document::media_assets`.
    pub asset_id: AssetId,
    /// Audio stream number inside the container.
    pub stream_index: usize,
}

/// One fully decoded and output-rate-prepared audio stream, shared between
/// the cache and every track built from it.
#[derive(Clone, Debug)]
pub struct DecodedAudio {
    /// Interleaved samples at `sample_rate` (the engine output rate once
    /// inserted into `AudioService`'s cache).
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

/// Convert a decoded asset to the engine output rate before caching it.
/// Same-rate assets retain their existing `Arc`, while every other asset is
/// resampled exactly once on the caller's background task.
pub fn prepare_audio_at_rate(
    audio: DecodedAudio,
    output_rate: u32,
) -> anyhow::Result<DecodedAudio> {
    let channels = audio.channels as usize;
    if audio.sample_rate == 0 || output_rate == 0 {
        anyhow::bail!("audio sample rates must be non-zero");
    }
    if channels == 0 {
        anyhow::bail!("audio channel count must be non-zero");
    }
    if !audio.samples.len().is_multiple_of(channels) {
        anyhow::bail!("interleaved audio is not aligned to its channel count");
    }

    // The decoder applies the cap at the media rate. Upsampling can make the
    // prepared buffer larger, so reject it before the resampler allocates.
    let input_frames = audio.samples.len() / channels;
    let output_frames = (input_frames as u128 * output_rate as u128
        + u128::from(audio.sample_rate) / 2)
        / u128::from(audio.sample_rate);
    let output_bytes = output_frames
        .saturating_mul(channels as u128)
        .saturating_mul(size_of::<f32>() as u128);
    if output_bytes > MAX_DECODE_BYTES as u128 {
        anyhow::bail!(
            "prepared audio exceeds the {} MiB in-memory limit",
            MAX_DECODE_BYTES / 1024 / 1024
        );
    }

    if audio.sample_rate == output_rate {
        return Ok(audio);
    }
    let samples = crate::resampler::resample_buffer(
        &audio.samples,
        audio.sample_rate,
        output_rate,
        audio.channels as usize,
    )?;
    Ok(DecodedAudio {
        samples: samples.into(),
        sample_rate: output_rate,
        channels: audio.channels,
    })
}

/// Desired mixer state for one audio-carrying layer, in output-rate units.
///
/// Equality drives the diff in [`crate::audio::AudioService`]: a spec that
/// compares equal to the last sent one produces no `AudioCommand`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSpec {
    /// Owning layer; also the `TrackId` (`layer_id.raw()`).
    pub layer_id: LayerId,
    /// Identity of the asset in `Document::media_assets`.
    ///
    /// [`AssetId::UNSET`] when the layer's `AudioSource` names nothing this
    /// build can read — an empty reference, or one an older format left as a
    /// display name. The consumer resolves it against the document and
    /// reports the miss, exactly as it does for an asset that is simply gone.
    pub asset_id: AssetId,
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
            asset_id: self.asset_id,
            stream_index: self.stream_index,
        }
    }

    /// The parts of the spec that decide the expensive build products (the
    /// sample slice and the gain curve). Placement/mute/fade changes reuse
    /// the previously built track; a build-key change rebuilds it.
    fn build_key(&self) -> (AssetId, usize, u64, u64, &AnimationChannel) {
        (
            self.asset_id,
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
                    asset_id: audio.asset_id,
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
    /// `decoded` must already be prepared at `output_rate`. Returns `None`
    /// when the trimmed source range is empty — the layer has nothing audible
    /// to contribute.
    pub fn build_track(
        spec: &TrackSpec,
        decoded: &DecodedAudio,
        comp_fps: FrameRate,
        output_rate: u32,
    ) -> Option<Track> {
        let channels = decoded.channels;
        let source_rate = decoded.sample_rate;
        if channels == 0 || source_rate == 0 || source_rate != output_rate {
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

        let output_frames = out_frame - in_frame;

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
        Some(track)
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
///
/// Public because an offline render has to ask the same question about the
/// **boundaries** of its frame range. Converting the two ends separately and
/// subtracting is not the same as converting the length: at 30 fps and
/// 48 kHz they agree, but at 29.97 they do not, and it is the boundary form
/// that makes two half-renders join seamlessly — frame *n* lands on the same
/// sample whichever range contains it.
pub fn comp_frames_to_rate(frames: u64, fps: FrameRate, rate: u32) -> u64 {
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
    anyhow::bail!("the `ffmpeg` feature of ravel-audio is disabled")
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

    /// An `AudioSource` naming `asset`, spelled the way a document spells it:
    /// the id's decimal form, which is what `desired_tracks` parses.
    fn source(asset: AssetId) -> AudioSource {
        AudioSource::new(asset, 0)
    }

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
    fn same_rate_preparation_reuses_the_decoded_buffer() {
        let source = decoded(128, 2, OUTPUT_RATE);
        let samples = source.samples.clone();

        let prepared = prepare_audio_at_rate(source, OUTPUT_RATE).unwrap();

        assert!(Arc::ptr_eq(&prepared.samples, &samples));
        assert_eq!(prepared.sample_rate, OUTPUT_RATE);
    }

    #[test]
    fn preparation_converts_an_asset_to_the_output_rate_once() {
        let prepared = prepare_audio_at_rate(decoded(4_410, 2, 44_100), OUTPUT_RATE).unwrap();

        assert_eq!(prepared.sample_rate, OUTPUT_RATE);
        assert_eq!(prepared.channels, 2);
        assert_eq!(prepared.frame_count(), 4_800);
    }

    #[test]
    fn preparation_rejects_upsampling_past_the_memory_cap_before_allocating() {
        let error = prepare_audio_at_rate(decoded(1, 2, 1), u32::MAX).unwrap_err();

        assert!(error.to_string().contains("128 MiB"));
    }

    #[test]
    fn start_frame_is_converted_to_output_rate() {
        // Layer starts at comp frame 30 = 1s at 30fps → 48 000 output frames.
        let spec = AudioMixdown::desired_tracks(
            &comp(vec![audio_layer(1, 30, source(AssetId::next()))]),
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
            &comp(vec![audio_layer(1, -15, source(AssetId::next()))]),
            OUTPUT_RATE,
        );
        assert_eq!(spec[0].start_frame, 0);
        assert_eq!(spec[0].source_in_frames, 15);
    }

    #[test]
    fn solo_mute_matches_the_compositor_rule() {
        let mut quiet = audio_layer(1, 0, source(AssetId::next()));
        quiet.muted = true;
        let mut soloed_video = Layer::new(LayerId::new(2), "video", Graph::new());
        soloed_video.solo = true; // no audio: still silences every other layer
        let audible = audio_layer(3, 0, source(AssetId::next()));
        let mut audio_muted = audio_layer(4, 0, source(AssetId::next()));
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
        let mut audio = source(AssetId::next());
        audio.fade_in_frames = 15; // 0.5s at 30fps
        audio.fade_out_frames = 30; // 1s
        let spec = AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        assert_eq!(spec[0].fade_in_frames, 24_000);
        assert_eq!(spec[0].fade_out_frames, 48_000);
    }

    #[test]
    fn constant_gain_stays_constant() {
        let mut audio = source(AssetId::next());
        audio.gain = AnimationChannel::constant(0.25);
        let specs =
            AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        let source = decoded(100, 2, 48_000);
        let track = AudioMixdown::build_track(&specs[0], &source, FPS_30, OUTPUT_RATE)
            .expect("audible range");
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
        let mut audio = source(AssetId::next());
        audio.gain = AnimationChannel::keyframes(curve);

        let specs =
            AudioMixdown::desired_tracks(&comp(vec![audio_layer(1, 0, audio)]), OUTPUT_RATE);
        // 2s of prepared source: the curve length follows the output rate.
        let track = AudioMixdown::build_track(
            &specs[0],
            &decoded(2 * 48_000, 2, 48_000),
            FPS_30,
            OUTPUT_RATE,
        )
        .expect("audible range");
        let TrackGain::Curve(gain) = track.gain else {
            panic!("keyframed gain must become a curve");
        };
        assert_eq!(gain.len(), 2 * OUTPUT_RATE as usize);
        assert!(gain[0].abs() < 1e-4, "ramp starts at 0");
        // One output second in ≈ one comp second in ≈ gain 1.0 at the end
        // of the keyframe span.
        let one_second = OUTPUT_RATE as usize;
        assert!((gain[one_second] - 1.0).abs() < 0.05, "gain at 1s ≈ 1.0");
        // The ramp finishes at 1s (gain 1.0) and holds afterwards; the former
        // source-rate position (44 100) is still mid-ramp (~0.9), proving the
        // curve was sampled against the output rate.
        assert!((gain[44_100] - 0.9).abs() < 0.05, "no source-rate aliasing");
    }

    #[test]
    fn trimmed_source_is_sliced() {
        let mut layer = Layer::new(LayerId::new(1), "a", Graph::new()).with_time(0, 30, 60);
        layer.audio = Some(source(AssetId::next()));
        let specs = AudioMixdown::desired_tracks(&comp(vec![layer]), OUTPUT_RATE);
        assert_eq!(specs[0].source_in_frames, 30);
        assert_eq!(specs[0].source_out_frames, 60);
        // 30..60 comp frames at 30fps = 1s..2s of a 3s 48kHz source.
        let track = AudioMixdown::build_track(
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
            &comp(vec![audio_layer(1, 0, source(AssetId::next()))]),
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
            &comp(vec![audio_layer(1, 0, source(AssetId::next()))]),
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

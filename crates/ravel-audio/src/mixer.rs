// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Multi-track audio mixer using dasp sample operations.
//!
//! Mixes an arbitrary number of audio tracks into a single interleaved
//! output buffer. Each track can have independent gain, mute, and solo
//! states. Per-track sample-rate conversion is applied automatically when
//! the track's rate differs from the mixer's output rate.

use crate::effects::{apply_fade_in, apply_fade_out, apply_gain};
use dasp_sample::Sample;
use std::sync::Arc;

/// Unique identifier for a mixer track.
pub type TrackId = u64;

/// Pre-sampled per-frame gain automation for a track.
///
/// Curves are indexed by track-local sample frame. If playback extends past
/// the sampled curve, the final value is held so callers can sample only the
/// animated range. An empty curve uses unity gain.
#[derive(Clone, Debug)]
pub enum TrackGain {
    /// One multiplier for the entire track.
    Constant(f32),
    /// One multiplier per track-local sample frame.
    Curve(Arc<[f32]>),
}

impl TrackGain {
    /// Return the gain at a track-local sample frame.
    pub fn at_frame(&self, frame: usize) -> f32 {
        match self {
            Self::Constant(gain) => *gain,
            Self::Curve(curve) => curve
                .get(frame)
                .or_else(|| curve.last())
                .copied()
                .unwrap_or(1.0),
        }
    }

    /// Apply the gain to an interleaved track buffer.
    fn apply(&self, samples: &mut [f32], channels: usize, frame_offset: usize) {
        if channels == 0 {
            return;
        }
        for (local_frame, frame_samples) in samples.chunks_exact_mut(channels).enumerate() {
            let gain = self.at_frame(frame_offset + local_frame);
            for sample in frame_samples {
                *sample *= gain;
            }
        }
    }
}

/// A single audio track in the mixer.
#[derive(Clone, Debug)]
pub struct Track {
    /// Unique identifier.
    pub id: TrackId,
    /// Interleaved `f32` sample data (already at the mixer's output rate
    /// after resampling).
    pub samples: Arc<[f32]>,
    /// Number of channels in `samples`.
    pub channels: u32,
    /// Frame on the output timeline where this track starts playing.
    ///
    /// This is measured in sample frames, not interleaved samples. A value of
    /// zero preserves the historical behavior where every track began at the
    /// start of the output timeline.
    pub start_frame: usize,
    /// Track-local volume automation.
    pub gain: TrackGain,
    /// Whether this track is muted.
    pub muted: bool,
    /// Whether this track is soloed.
    pub solo: bool,
    /// Fade-in length in frames.
    pub fade_in_frames: usize,
    /// Fade-out length in frames.
    pub fade_out_frames: usize,
}

impl Track {
    /// Create a new track with default settings (gain = 1.0, unmuted, not
    /// soloed, no fades).
    pub fn new(id: TrackId, samples: Arc<[f32]>, channels: u32) -> Self {
        Self {
            id,
            samples,
            channels,
            start_frame: 0,
            gain: TrackGain::Constant(1.0),
            muted: false,
            solo: false,
            fade_in_frames: 0,
            fade_out_frames: 0,
        }
    }

    /// Total number of frames in this track.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / self.channels as usize
    }
}

/// Configuration for the mixer.
#[derive(Clone, Debug)]
pub struct MixerConfig {
    /// Output sample rate in Hz.
    pub output_sample_rate: u32,
    /// Number of output channels.
    pub output_channels: u32,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            output_sample_rate: 48_000,
            output_channels: 2,
        }
    }
}

/// Multi-track audio mixer.
///
/// Supports:
/// - Arbitrary number of tracks
/// - Per-track gain, mute, and solo
/// - Per-track fade-in / fade-out
/// - Mono-to-stereo up-mix (duplicate mono to both channels)
///
/// Track sample data is expected to already be at the mixer's output rate.
/// Use [`crate::resampler::Resampler`] to pre-convert tracks with different
/// source rates.
pub struct Mixer {
    config: MixerConfig,
    tracks: Vec<Track>,
    /// Master output gain.
    master_gain: f32,
}

impl Mixer {
    /// Create a new mixer with the given configuration.
    pub fn new(config: MixerConfig) -> Self {
        Self {
            config,
            tracks: Vec::new(),
            master_gain: 1.0,
        }
    }

    /// Add a track to the mixer.
    pub fn add_track(&mut self, track: Track) {
        self.tracks.push(track);
    }

    /// Remove a track by its [`TrackId`].
    ///
    /// Returns `true` if the track was found and removed.
    pub fn remove_track(&mut self, id: TrackId) -> bool {
        if let Some(pos) = self.tracks.iter().position(|t| t.id == id) {
            self.tracks.remove(pos);
            true
        } else {
            false
        }
    }

    /// Get a mutable reference to a track by its [`TrackId`].
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Get a reference to a track by its [`TrackId`].
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Number of tracks currently in the mixer.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Set the master output gain.
    pub fn set_master_gain(&mut self, gain: f32) {
        self.master_gain = gain;
    }

    /// Current master gain.
    pub fn master_gain(&self) -> f32 {
        self.master_gain
    }

    /// Reference to the mixer configuration.
    pub fn config(&self) -> &MixerConfig {
        &self.config
    }

    /// Mix `frame_count` frames starting at `frame_offset` into a new
    /// interleaved output buffer.
    ///
    /// The returned buffer has `frame_count * output_channels` samples.
    /// Solo logic: if any track is soloed, only soloed tracks contribute;
    /// otherwise all non-muted tracks contribute.
    ///
    /// Processing order is track-local gain, track-local fades, summing, then
    /// master gain. Keeping automation and fades before summing ensures each
    /// track is shaped independently while the master scales the final mix.
    pub fn mix(&self, frame_offset: usize, frame_count: usize) -> Vec<f32> {
        let out_ch = self.config.output_channels as usize;
        let total_samples = frame_count * out_ch;

        // Start with silence (dasp_sample equilibrium).
        let mut output = vec![f32::EQUILIBRIUM; total_samples];

        let any_solo = self.tracks.iter().any(|t| t.solo);

        for track in &self.tracks {
            // Solo/mute logic.
            if any_solo && !track.solo {
                continue;
            }
            if track.muted {
                continue;
            }
            if track.channels == 0 {
                continue;
            }

            let t_ch = track.channels as usize;
            let t_frames = track.frame_count();
            let output_end = frame_offset.saturating_add(frame_count);
            let track_end = track.start_frame.saturating_add(t_frames);
            let overlap_start = frame_offset.max(track.start_frame);
            let overlap_end = output_end.min(track_end);
            if overlap_start >= overlap_end {
                continue;
            }

            // Work only on the intersection of the requested output window
            // and the track. Source positions and fades use track-local
            // frames; destination positions use output-timeline frames.
            let track_frame_offset = overlap_start - track.start_frame;
            let output_frame_offset = overlap_start - frame_offset;
            let overlap_frames = overlap_end - overlap_start;
            let sample_start = track_frame_offset * t_ch;
            let sample_end = sample_start + overlap_frames * t_ch;
            let mut track_buf = track.samples[sample_start..sample_end].to_vec();

            // Apply per-track gain.
            track.gain.apply(&mut track_buf, t_ch, track_frame_offset);

            // Apply fades.
            if track.fade_in_frames > 0 {
                apply_fade_in(
                    &mut track_buf,
                    track.channels,
                    track.fade_in_frames,
                    track_frame_offset,
                );
            }
            if track.fade_out_frames > 0 {
                apply_fade_out(
                    &mut track_buf,
                    track.channels,
                    track.fade_out_frames,
                    t_frames,
                    track_frame_offset,
                );
            }

            // Mix into output with channel mapping.
            let output_sample_offset = output_frame_offset * out_ch;
            mix_into(
                &mut output[output_sample_offset..],
                &track_buf,
                out_ch,
                t_ch,
                overlap_frames,
            );
        }

        // Apply master gain.
        if (self.master_gain - 1.0).abs() > f32::EPSILON {
            apply_gain(&mut output, self.master_gain);
        }

        output
    }
}

/// Mix `src` (interleaved, `src_ch` channels) into `dst` (interleaved,
/// `dst_ch` channels) by summing.
///
/// Channel mapping rules:
/// - Mono source → duplicated to all output channels.
/// - Matching channel counts → 1:1 mapping.
/// - More source channels than output → extra channels discarded.
/// - Fewer source channels than output (non-mono) → extra output channels
///   get silence (no contribution from this source).
fn mix_into(dst: &mut [f32], src: &[f32], dst_ch: usize, src_ch: usize, frame_count: usize) {
    for f in 0..frame_count {
        for dc in 0..dst_ch {
            let sc = if src_ch == 1 {
                // Mono → duplicate to all output channels.
                0
            } else if dc < src_ch {
                dc
            } else {
                continue; // No source for this output channel.
            };
            let src_idx = f * src_ch + sc;
            let dst_idx = f * dst_ch + dc;
            if src_idx < src.len() && dst_idx < dst.len() {
                dst[dst_idx] += src[src_idx];
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_mixer() -> Mixer {
        Mixer::new(MixerConfig {
            output_sample_rate: 48_000,
            output_channels: 2,
        })
    }

    #[test]
    fn empty_mixer_outputs_silence() {
        let m = stereo_mixer();
        let out = m.mix(0, 4);
        assert_eq!(out.len(), 8); // 4 frames × 2 channels
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn single_stereo_track() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![0.5, -0.5, 0.3, -0.3].into();
        m.add_track(Track::new(1, samples, 2));
        let out = m.mix(0, 2);
        assert!((out[0] - 0.5).abs() < f32::EPSILON); // L0
        assert!((out[1] + 0.5).abs() < f32::EPSILON); // R0
        assert!((out[2] - 0.3).abs() < f32::EPSILON); // L1
        assert!((out[3] + 0.3).abs() < f32::EPSILON); // R1
    }

    #[test]
    fn mono_to_stereo_upmix() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![0.5, 0.8].into();
        m.add_track(Track::new(1, samples, 1));
        let out = m.mix(0, 2);
        // Mono duplicated to both channels.
        assert!((out[0] - 0.5).abs() < f32::EPSILON); // L0
        assert!((out[1] - 0.5).abs() < f32::EPSILON); // R0
        assert!((out[2] - 0.8).abs() < f32::EPSILON); // L1
        assert!((out[3] - 0.8).abs() < f32::EPSILON); // R1
    }

    #[test]
    fn two_tracks_summed() {
        let mut m = stereo_mixer();
        let a: Arc<[f32]> = vec![0.3, 0.3, 0.3, 0.3].into();
        let b: Arc<[f32]> = vec![0.2, -0.2, 0.2, -0.2].into();
        m.add_track(Track::new(1, a, 2));
        m.add_track(Track::new(2, b, 2));
        let out = m.mix(0, 2);
        assert!((out[0] - 0.5).abs() < f32::EPSILON); // 0.3 + 0.2
        assert!((out[1] - 0.1).abs() < f32::EPSILON); // 0.3 + (-0.2)
    }

    #[test]
    fn track_gain() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0, 1.0].into();
        let mut track = Track::new(1, samples, 2);
        track.gain = TrackGain::Constant(0.5);
        m.add_track(track);
        let out = m.mix(0, 1);
        assert!((out[0] - 0.5).abs() < f32::EPSILON);
        assert!((out[1] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn muted_track_silent() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0, 1.0].into();
        let mut track = Track::new(1, samples, 2);
        track.muted = true;
        m.add_track(track);
        let out = m.mix(0, 1);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn solo_excludes_non_soloed() {
        let mut m = stereo_mixer();
        let a: Arc<[f32]> = vec![0.5, 0.5].into();
        let b: Arc<[f32]> = vec![0.3, 0.3].into();

        let mut track_a = Track::new(1, a, 2);
        track_a.solo = true;
        m.add_track(track_a);
        m.add_track(Track::new(2, b, 2));

        let out = m.mix(0, 1);
        // Only track A should be heard.
        assert!((out[0] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn master_gain() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0, 1.0].into();
        m.add_track(Track::new(1, samples, 2));
        m.set_master_gain(0.25);
        let out = m.mix(0, 1);
        assert!((out[0] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn offset_beyond_track_length() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0, 1.0, 0.5, 0.5].into();
        m.add_track(Track::new(1, samples, 2));
        // Track has 2 frames. Reading at offset 5 should yield silence.
        let out = m.mix(5, 2);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn partial_overlap() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![0.8, 0.8, 0.6, 0.6].into();
        m.add_track(Track::new(1, samples, 2));
        // Read 3 frames starting at offset 1. Track has 2 frames total.
        // Frame 1 → data, Frame 2+ → silence.
        let out = m.mix(1, 3);
        assert!((out[0] - 0.6).abs() < f32::EPSILON); // frame 1: [0.6, 0.6]
        assert!((out[1] - 0.6).abs() < f32::EPSILON);
        assert!((out[2] - 0.0).abs() < f32::EPSILON); // frame 2: beyond → silence
        assert!((out[4] - 0.0).abs() < f32::EPSILON); // frame 3: beyond → silence
    }

    #[test]
    fn remove_track() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0, 1.0].into();
        m.add_track(Track::new(42, samples, 2));
        assert_eq!(m.track_count(), 1);
        assert!(m.remove_track(42));
        assert_eq!(m.track_count(), 0);
        assert!(!m.remove_track(42)); // already removed
    }

    #[test]
    fn track_with_fade() {
        let mut m = stereo_mixer();
        let samples: Arc<[f32]> = vec![1.0; 8].into(); // 4 stereo frames, all 1.0
        let mut track = Track::new(1, samples, 2);
        track.fade_in_frames = 2;
        m.add_track(track);
        let out = m.mix(0, 4);
        // Frame 0: fade t=0/2=0.0 → 0.0
        assert!((out[0] - 0.0).abs() < f32::EPSILON);
        // Frame 1: fade t=1/2=0.5 → 0.5
        assert!((out[2] - 0.5).abs() < f32::EPSILON);
        // Frame 2: past fade → 1.0
        assert!((out[4] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn tracks_with_start_frames_mix_on_the_output_timeline() {
        let mut m = stereo_mixer();
        let mut track_a = Track::new(1, Arc::from(vec![0.25; 160 * 2]), 2);
        track_a.start_frame = 0;
        let mut track_b = Track::new(2, Arc::from(vec![0.5; 80 * 2]), 2);
        track_b.start_frame = 100;
        m.add_track(track_a);
        m.add_track(track_b);

        let out = m.mix(0, 120);
        for frame in 0..120 {
            let expected = if frame < 100 { 0.25 } else { 0.25 + 0.5 };
            for channel in 0..2 {
                assert!((out[frame * 2 + channel] - expected).abs() < f32::EPSILON);
            }
        }

        // This request begins inside A and crosses B's output start frame.
        let crossing = m.mix(90, 30);
        for local_frame in 0..30 {
            let output_frame = 90 + local_frame;
            let expected = if output_frame < 100 { 0.25 } else { 0.75 };
            assert!((crossing[local_frame * 2] - expected).abs() < f32::EPSILON);
            assert!((crossing[local_frame * 2 + 1] - expected).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn track_outside_output_window_contributes_nothing() {
        let mut m = stereo_mixer();
        let mut future = Track::new(1, Arc::from(vec![1.0; 8]), 2);
        future.start_frame = 10;
        m.add_track(future);

        assert!(m.mix(0, 4).iter().all(|sample| *sample == 0.0));
        assert!(m.mix(14, 4).iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn fades_use_track_local_frames_after_timeline_offset() {
        let mut m = stereo_mixer();
        let mut track = Track::new(1, Arc::from(vec![1.0; 8]), 2);
        track.start_frame = 10;
        track.fade_in_frames = 2;
        track.fade_out_frames = 2;
        m.add_track(track);

        let out = m.mix(9, 6);
        let expected = [0.0, 0.0, 0.5, 1.0, 0.5, 0.0];
        for (frame, expected_sample) in expected.into_iter().enumerate() {
            assert!((out[frame * 2] - expected_sample).abs() < f32::EPSILON);
            assert!((out[frame * 2 + 1] - expected_sample).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn zero_frame_request_returns_empty_output() {
        let mut m = stereo_mixer();
        m.add_track(Track::new(1, Arc::from(vec![1.0; 8]), 2));
        assert!(m.mix(2, 0).is_empty());
    }

    #[test]
    fn gain_curve_uses_track_local_frames_and_composes_with_master() {
        let mut m = stereo_mixer();
        m.set_master_gain(0.5);
        let input = [0.8, -0.4];
        let curve = [0.0, 0.25, 0.5, 0.75, 1.0];
        let samples: Arc<[f32]> = (0..curve.len())
            .flat_map(|_| input)
            .collect::<Vec<_>>()
            .into();
        let mut track = Track::new(1, samples, 2);
        track.start_frame = 10;
        track.gain = TrackGain::Curve(Arc::from(curve));
        m.add_track(track);

        let out = m.mix(11, 3);
        for local_frame in 0..3 {
            let track_frame = local_frame + 1;
            for channel in 0..2 {
                let expected = input[channel] * curve[track_frame] * 0.5;
                assert!((out[local_frame * 2 + channel] - expected).abs() < f32::EPSILON);
            }
        }
    }

    #[test]
    fn short_gain_curve_holds_its_last_value() {
        let mut m = stereo_mixer();
        let curve = [0.25, 0.5];
        let mut track = Track::new(1, Arc::from(vec![1.0; 5]), 1);
        track.gain = TrackGain::Curve(Arc::from(curve));
        m.add_track(track);

        let out = m.mix(0, 5);
        let expected = [0.25, 0.5, 0.5, 0.5, 0.5];
        for (frame, expected_sample) in expected.into_iter().enumerate() {
            assert!((out[frame * 2] - expected_sample).abs() < f32::EPSILON);
            assert!((out[frame * 2 + 1] - expected_sample).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn gain_curve_longer_than_track_ignores_unused_values() {
        let mut m = stereo_mixer();
        let curve = [0.2, 0.4, 0.6, 20.0, 30.0];
        let mut track = Track::new(1, Arc::from(vec![2.0; 3]), 1);
        track.gain = TrackGain::Curve(Arc::from(curve));
        m.add_track(track);

        let out = m.mix(0, 5);
        let expected = [0.4, 0.8, 1.2, 0.0, 0.0];
        for (frame, expected_sample) in expected.into_iter().enumerate() {
            assert!((out[frame * 2] - expected_sample).abs() < f32::EPSILON);
            assert!((out[frame * 2 + 1] - expected_sample).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn empty_gain_curve_is_unity() {
        let mut m = stereo_mixer();
        let mut track = Track::new(1, Arc::from(vec![0.75]), 1);
        track.gain = TrackGain::Curve(Arc::from(Vec::<f32>::new()));
        m.add_track(track);

        assert_eq!(m.mix(0, 1), vec![0.75, 0.75]);
    }
}

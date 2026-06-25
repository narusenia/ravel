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
    /// Track volume multiplier (1.0 = unity).
    pub gain: f32,
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
            gain: 1.0,
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

            // Extract the region of this track that overlaps the request.
            let mut track_buf = Vec::with_capacity(frame_count * t_ch);
            for f in 0..frame_count {
                let src_frame = frame_offset + f;
                for c in 0..t_ch {
                    if src_frame < t_frames {
                        track_buf.push(track.samples[src_frame * t_ch + c]);
                    } else {
                        track_buf.push(0.0);
                    }
                }
            }

            // Apply per-track gain.
            apply_gain(&mut track_buf, track.gain);

            // Apply fades.
            if track.fade_in_frames > 0 {
                apply_fade_in(
                    &mut track_buf,
                    track.channels,
                    track.fade_in_frames,
                    frame_offset,
                );
            }
            if track.fade_out_frames > 0 {
                apply_fade_out(
                    &mut track_buf,
                    track.channels,
                    track.fade_out_frames,
                    t_frames,
                    frame_offset,
                );
            }

            // Mix into output with channel mapping.
            mix_into(&mut output, &track_buf, out_ch, t_ch, frame_count);
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
        track.gain = 0.5;
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
}

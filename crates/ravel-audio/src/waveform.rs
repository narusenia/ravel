// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Waveform data generation for UI display.
//!
//! Produces downsampled peak/RMS envelopes from PCM audio buffers so the
//! timeline editor can render a waveform overview without reading every
//! sample per draw.

/// Downsampled waveform representation for one channel.
#[derive(Clone, Debug)]
pub struct WaveformChannel {
    /// `(min, max)` peak pairs per segment.
    pub peaks: Vec<(f32, f32)>,
    /// RMS (root-mean-square) value per segment.
    pub rms: Vec<f32>,
}

/// Waveform data computed from an audio buffer.
///
/// Contains one [`WaveformChannel`] per source channel plus metadata about
/// the sampling parameters.
#[derive(Clone, Debug)]
pub struct WaveformData {
    /// Per-channel waveform envelopes.
    pub channels: Vec<WaveformChannel>,
    /// Source sample rate in Hz.
    pub sample_rate: u32,
    /// Number of source channels.
    pub channel_count: u32,
    /// How many source frames map to one waveform segment.
    pub frames_per_segment: usize,
    /// Total number of segments.
    pub segment_count: usize,
}

impl WaveformData {
    /// Generate waveform data from interleaved `f32` samples.
    ///
    /// # Arguments
    ///
    /// * `samples` — interleaved PCM data (`[L0, R0, L1, R1, …]`).
    /// * `sample_rate` — source sample rate in Hz.
    /// * `channel_count` — number of interleaved channels.
    /// * `frames_per_segment` — how many source frames to compress into a
    ///   single waveform segment.  Larger values produce a coarser (faster to
    ///   render) overview.
    ///
    /// # Panics
    ///
    /// Panics if `channel_count` is zero or `frames_per_segment` is zero.
    pub fn generate(
        samples: &[f32],
        sample_rate: u32,
        channel_count: u32,
        frames_per_segment: usize,
    ) -> Self {
        assert!(channel_count > 0, "channel_count must be > 0");
        assert!(frames_per_segment > 0, "frames_per_segment must be > 0");

        let ch = channel_count as usize;
        let total_frames = samples.len() / ch;
        let segment_count = total_frames.div_ceil(frames_per_segment);

        let mut channels: Vec<WaveformChannel> = (0..ch)
            .map(|_| WaveformChannel {
                peaks: Vec::with_capacity(segment_count),
                rms: Vec::with_capacity(segment_count),
            })
            .collect();

        for seg in 0..segment_count {
            let start_frame = seg * frames_per_segment;
            let end_frame = (start_frame + frames_per_segment).min(total_frames);
            let frame_count = end_frame - start_frame;

            for c in 0..ch {
                let mut min_val = f32::MAX;
                let mut max_val = f32::MIN;
                let mut sum_sq = 0.0f64;

                for f in start_frame..end_frame {
                    let s = samples[f * ch + c];
                    if s < min_val {
                        min_val = s;
                    }
                    if s > max_val {
                        max_val = s;
                    }
                    sum_sq += (s as f64) * (s as f64);
                }

                let rms = if frame_count > 0 {
                    (sum_sq / frame_count as f64).sqrt() as f32
                } else {
                    0.0
                };

                channels[c].peaks.push((min_val, max_val));
                channels[c].rms.push(rms);
            }
        }

        Self {
            channels,
            sample_rate,
            channel_count,
            frames_per_segment,
            segment_count,
        }
    }

    /// Number of waveform segments.
    pub fn segment_count(&self) -> usize {
        self.segment_count
    }

    /// Duration represented by this waveform in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        (self.segment_count * self.frames_per_segment) as f64 / self.sample_rate as f64
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "channel_count must be > 0")]
    fn zero_channels_panics() {
        WaveformData::generate(&[1.0, 2.0], 48_000, 0, 1);
    }

    #[test]
    #[should_panic(expected = "frames_per_segment must be > 0")]
    fn zero_frames_per_segment_panics() {
        WaveformData::generate(&[1.0, 2.0], 48_000, 1, 0);
    }

    #[test]
    fn mono_single_segment() {
        // 4 mono frames, 1 segment of 4.
        let samples = vec![0.5, -0.5, 0.3, -0.3];
        let wf = WaveformData::generate(&samples, 44_100, 1, 4);
        assert_eq!(wf.segment_count(), 1);
        assert_eq!(wf.channels.len(), 1);
        let (min, max) = wf.channels[0].peaks[0];
        assert!((min + 0.5).abs() < f32::EPSILON);
        assert!((max - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn mono_multiple_segments() {
        // 8 mono frames, 4 frames per segment → 2 segments.
        let samples = vec![1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0];
        let wf = WaveformData::generate(&samples, 48_000, 1, 4);
        assert_eq!(wf.segment_count(), 2);
        // Segment 0: all 1.0
        assert!((wf.channels[0].peaks[0].0 - 1.0).abs() < f32::EPSILON);
        assert!((wf.channels[0].peaks[0].1 - 1.0).abs() < f32::EPSILON);
        // Segment 1: all -1.0
        assert!((wf.channels[0].peaks[1].0 + 1.0).abs() < f32::EPSILON);
        assert!((wf.channels[0].peaks[1].1 + 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn stereo_waveform() {
        // 2 stereo frames: [L0, R0, L1, R1]
        let samples = vec![0.5, -0.5, 0.8, -0.8];
        let wf = WaveformData::generate(&samples, 48_000, 2, 2);
        assert_eq!(wf.segment_count(), 1);
        assert_eq!(wf.channels.len(), 2);
        // Left channel: min=0.5, max=0.8
        assert!((wf.channels[0].peaks[0].0 - 0.5).abs() < f32::EPSILON);
        assert!((wf.channels[0].peaks[0].1 - 0.8).abs() < f32::EPSILON);
        // Right channel: min=-0.8, max=-0.5
        assert!((wf.channels[1].peaks[0].0 + 0.8).abs() < f32::EPSILON);
        assert!((wf.channels[1].peaks[0].1 + 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn rms_calculation() {
        // 4 mono frames of 0.5: RMS = sqrt(4 × 0.25 / 4) = 0.5
        let samples = vec![0.5, 0.5, 0.5, 0.5];
        let wf = WaveformData::generate(&samples, 48_000, 1, 4);
        assert!((wf.channels[0].rms[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rms_sine_approximation() {
        // A half-cycle of a unit sine: RMS ≈ 1/√2 ≈ 0.707
        let n = 1000;
        let samples: Vec<f32> = (0..n)
            .map(|i| (i as f32 * std::f32::consts::PI / n as f32).sin())
            .collect();
        let wf = WaveformData::generate(&samples, 48_000, 1, n);
        // Half-sine RMS = 1/√2 ≈ 0.707
        assert!((wf.channels[0].rms[0] - 1.0 / 2.0f32.sqrt()).abs() < 0.01);
    }

    #[test]
    fn partial_last_segment() {
        // 5 mono frames, 4 per segment → 2 segments (last has 1 frame).
        let samples = vec![1.0, 1.0, 1.0, 1.0, 0.5];
        let wf = WaveformData::generate(&samples, 48_000, 1, 4);
        assert_eq!(wf.segment_count(), 2);
        assert!((wf.channels[0].peaks[1].0 - 0.5).abs() < f32::EPSILON);
        assert!((wf.channels[0].peaks[1].1 - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_input() {
        let wf = WaveformData::generate(&[], 48_000, 1, 256);
        assert_eq!(wf.segment_count(), 0);
        assert!(wf.channels[0].peaks.is_empty());
    }
}

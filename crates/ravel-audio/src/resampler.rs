// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sample rate conversion via rubato.
//!
//! Wraps the rubato sinc resampler to convert audio between different
//! sample rates (e.g.  44 100 Hz source → 48 000 Hz output).  The wrapper
//! handles the interleaved ↔ planar conversion that rubato requires.

use crate::error::AudioError;
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// High-quality sinc-based sample rate converter.
///
/// Create one per source sample-rate / output sample-rate / channel
/// combination.  Feed it interleaved `f32` chunks via [`Resampler::process`];
/// it returns interleaved `f32` output at the target rate.
pub struct Resampler {
    inner: SincFixedIn<f32>,
    channels: usize,
    chunk_size: usize,
    input_rate: u32,
    output_rate: u32,
}

impl Resampler {
    /// Create a new sinc resampler.
    ///
    /// * `input_rate` / `output_rate` — source and target sample rates in Hz.
    /// * `channels` — number of audio channels.
    /// * `chunk_size` — number of **input frames** to process per call.
    ///   Larger values amortise the sinc filter overhead but increase latency.
    pub fn new(
        input_rate: u32,
        output_rate: u32,
        channels: usize,
        chunk_size: usize,
    ) -> Result<Self, AudioError> {
        if input_rate == 0 || output_rate == 0 {
            return Err(AudioError::Resampler(
                "sample rates must be > 0".to_string(),
            ));
        }
        if channels == 0 {
            return Err(AudioError::Resampler("channels must be > 0".to_string()));
        }
        if chunk_size == 0 {
            return Err(AudioError::Resampler("chunk_size must be > 0".to_string()));
        }

        let ratio = output_rate as f64 / input_rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        let inner = SincFixedIn::new(ratio, 2.0, params, chunk_size, channels)
            .map_err(|e| AudioError::Resampler(e.to_string()))?;

        Ok(Self {
            inner,
            channels,
            chunk_size,
            input_rate,
            output_rate,
        })
    }

    /// Number of input frames expected per [`Self::process`] call.
    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    /// Process a chunk of interleaved `f32` samples and return the resampled
    /// output (also interleaved).
    ///
    /// The caller should supply exactly [`Self::input_frames_next`] frames
    /// (i.e. `input_frames_next() * channels` samples).  If fewer frames are
    /// provided, the remainder is zero-padded.
    pub fn process(&mut self, interleaved_input: &[f32]) -> Result<Vec<f32>, AudioError> {
        let needed = self.input_frames_next();
        // De-interleave into per-channel vectors.
        let mut planar: Vec<Vec<f32>> = (0..self.channels)
            .map(|_| Vec::with_capacity(needed))
            .collect();

        let frames_available = interleaved_input.len() / self.channels;
        for f in 0..needed {
            for c in 0..self.channels {
                let sample = if f < frames_available {
                    interleaved_input[f * self.channels + c]
                } else {
                    0.0
                };
                planar[c].push(sample);
            }
        }

        let output_planar = self
            .inner
            .process(&planar, None)
            .map_err(|e| AudioError::Resampler(e.to_string()))?;

        // Re-interleave.
        let output_frames = if output_planar.is_empty() {
            0
        } else {
            output_planar[0].len()
        };
        let mut interleaved = Vec::with_capacity(output_frames * self.channels);
        for f in 0..output_frames {
            for ch in &output_planar {
                interleaved.push(ch[f]);
            }
        }

        Ok(interleaved)
    }

    /// Input sample rate.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Output sample rate.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Resample ratio (`output_rate / input_rate`).
    pub fn ratio(&self) -> f64 {
        self.output_rate as f64 / self.input_rate as f64
    }

    /// Number of channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Chunk size (input frames per process call).
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Returns `true` if the input and output rates are equal (no conversion
    /// needed).
    pub fn is_passthrough(&self) -> bool {
        self.input_rate == self.output_rate
    }
}

/// Resample an entire interleaved buffer in one shot.
///
/// This is a convenience function for offline / non-streaming use. For
/// real-time streaming, create a [`Resampler`] and call [`Resampler::process`]
/// in chunks.
pub fn resample_buffer(
    input: &[f32],
    input_rate: u32,
    output_rate: u32,
    channels: usize,
) -> Result<Vec<f32>, AudioError> {
    if input_rate == output_rate {
        return Ok(input.to_vec());
    }
    if channels == 0 || input.is_empty() {
        return Ok(Vec::new());
    }

    let total_input_frames = input.len() / channels;
    // Use a reasonable chunk size.
    let chunk_size = 1024.min(total_input_frames).max(1);

    let mut resampler = Resampler::new(input_rate, output_rate, channels, chunk_size)?;
    let mut output = Vec::new();
    let mut offset = 0;

    while offset < input.len() {
        let needed = resampler.input_frames_next();
        let end = (offset + needed * channels).min(input.len());
        let chunk = &input[offset..end];
        let resampled = resampler.process(chunk)?;
        output.extend_from_slice(&resampled);
        offset += needed * channels;
    }

    Ok(output)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_detection() {
        let r = Resampler::new(48_000, 48_000, 2, 1024).unwrap();
        assert!(r.is_passthrough());

        let r2 = Resampler::new(44_100, 48_000, 2, 1024).unwrap();
        assert!(!r2.is_passthrough());
    }

    #[test]
    fn ratio_calculation() {
        let r = Resampler::new(44_100, 48_000, 1, 1024).unwrap();
        let expected = 48_000.0 / 44_100.0;
        assert!((r.ratio() - expected).abs() < 1e-9);
    }

    #[test]
    fn invalid_params() {
        assert!(Resampler::new(0, 48_000, 1, 1024).is_err());
        assert!(Resampler::new(44_100, 0, 1, 1024).is_err());
        assert!(Resampler::new(44_100, 48_000, 0, 1024).is_err());
        assert!(Resampler::new(44_100, 48_000, 1, 0).is_err());
    }

    #[test]
    fn resample_mono_upsample() {
        // Upsample 44100→48000, mono. Feed multiple chunks to let the
        // sinc filter fill its internal delay line.
        let mut r = Resampler::new(44_100, 48_000, 1, 1024).unwrap();
        let input = vec![0.5f32; 1024];
        let mut total_output = Vec::new();
        for _ in 0..4 {
            let output = r.process(&input).unwrap();
            total_output.extend_from_slice(&output);
        }
        // After several chunks the output should be significantly larger
        // than one input chunk (ratio ≈ 1.088).
        assert!(
            total_output.len() > 1024,
            "total output {} should exceed one input chunk",
            total_output.len()
        );
        // Skip the initial transient (sinc filter warm-up), then check DC.
        let skip = 512;
        for &s in &total_output[skip..] {
            assert!(
                (s - 0.5).abs() < 0.05,
                "DC signal should be preserved, got {s}"
            );
        }
    }

    #[test]
    fn resample_mono_downsample() {
        // Downsample 48000→44100, mono. Feed multiple chunks.
        let mut r = Resampler::new(48_000, 44_100, 1, 1024).unwrap();
        let input = vec![0.5f32; 1024];
        let mut total_output = Vec::new();
        for _ in 0..4 {
            let output = r.process(&input).unwrap();
            total_output.extend_from_slice(&output);
        }
        // Total output should be less than total input (ratio ≈ 0.919).
        let total_input = 4 * 1024;
        assert!(
            total_output.len() < total_input,
            "total output {} should be less than total input {total_input}",
            total_output.len()
        );
        // Skip transient, check DC.
        let skip = 512;
        for &s in &total_output[skip..] {
            assert!(
                (s - 0.5).abs() < 0.05,
                "DC signal should be preserved, got {s}"
            );
        }
    }

    #[test]
    fn resample_buffer_same_rate_passthrough() {
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let output = resample_buffer(&input, 48_000, 48_000, 2).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn resample_buffer_empty() {
        let output = resample_buffer(&[], 44_100, 48_000, 1).unwrap();
        assert!(output.is_empty());
    }
}

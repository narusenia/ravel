// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Basic audio effects: gain and fade.
//!
//! Effects operate in-place on interleaved `f32` sample buffers.
//! All processing is 32-bit float (REQ-CORE-009).

/// Apply a constant gain (volume multiplier) to every sample in `samples`.
///
/// A gain of `1.0` is unity (no change), `0.0` is silence, `2.0` doubles
/// amplitude, etc.
pub fn apply_gain(samples: &mut [f32], gain: f32) {
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// Apply a linear fade-in to an interleaved sample buffer.
///
/// - `channels`: number of interleaved channels (1 = mono, 2 = stereo, …).
/// - `fade_length`: length of the fade in **frames** (not samples).
/// - `frame_offset`: the starting frame index within the overall clip — used
///   to calculate how far into the fade this buffer sits.
///
/// Frames in `[0, fade_length)` are scaled from `0.0` to `1.0` linearly.
/// Frames at or beyond `fade_length` are untouched.
pub fn apply_fade_in(samples: &mut [f32], channels: u32, fade_length: usize, frame_offset: usize) {
    if fade_length == 0 || channels == 0 {
        return;
    }
    let ch = channels as usize;
    let frame_count = samples.len() / ch;
    for i in 0..frame_count {
        let abs_frame = frame_offset + i;
        if abs_frame >= fade_length {
            break;
        }
        let t = abs_frame as f32 / fade_length as f32;
        for c in 0..ch {
            samples[i * ch + c] *= t;
        }
    }
}

/// Apply a linear fade-out to an interleaved sample buffer.
///
/// - `channels`: number of interleaved channels.
/// - `fade_length`: length of the fade in **frames**.
/// - `total_frames`: total number of frames in the full clip.
/// - `frame_offset`: the starting frame index within the overall clip.
///
/// Frames in `[total_frames - fade_length, total_frames)` are scaled from
/// `1.0` to `0.0` linearly.
pub fn apply_fade_out(
    samples: &mut [f32],
    channels: u32,
    fade_length: usize,
    total_frames: usize,
    frame_offset: usize,
) {
    if fade_length == 0 || channels == 0 || total_frames == 0 {
        return;
    }
    let ch = channels as usize;
    let frame_count = samples.len() / ch;
    let fade_start = total_frames.saturating_sub(fade_length);
    for i in 0..frame_count {
        let abs_frame = frame_offset + i;
        if abs_frame < fade_start {
            continue;
        }
        if abs_frame >= total_frames {
            // Beyond the clip — silence.
            for c in 0..ch {
                samples[i * ch + c] = 0.0;
            }
        } else {
            let frames_into_fade = abs_frame - fade_start;
            let t = 1.0 - (frames_into_fade as f32 / fade_length as f32);
            for c in 0..ch {
                samples[i * ch + c] *= t;
            }
        }
    }
}

/// Trait for audio effects that process sample buffers in-place.
pub trait AudioEffect: Send + Sync {
    /// Process `samples` (interleaved, `channels`-wide) in-place.
    ///
    /// `frame_offset` is the absolute frame position within the clip,
    /// allowing effects to know their temporal position.
    fn process(&self, samples: &mut [f32], channels: u32, frame_offset: usize);
}

/// Constant-gain effect.
pub struct GainEffect {
    pub gain: f32,
}

impl AudioEffect for GainEffect {
    fn process(&self, samples: &mut [f32], _channels: u32, _frame_offset: usize) {
        apply_gain(samples, self.gain);
    }
}

/// Linear fade-in / fade-out effect.
pub struct FadeEffect {
    /// Fade-in length in frames.
    pub fade_in_frames: usize,
    /// Fade-out length in frames.
    pub fade_out_frames: usize,
    /// Total clip length in frames.
    pub total_frames: usize,
}

impl AudioEffect for FadeEffect {
    fn process(&self, samples: &mut [f32], channels: u32, frame_offset: usize) {
        apply_fade_in(samples, channels, self.fade_in_frames, frame_offset);
        apply_fade_out(
            samples,
            channels,
            self.fade_out_frames,
            self.total_frames,
            frame_offset,
        );
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_unity_no_change() {
        let mut buf = vec![0.5, -0.5, 1.0, -1.0];
        let original = buf.clone();
        apply_gain(&mut buf, 1.0);
        assert_eq!(buf, original);
    }

    #[test]
    fn gain_zero_silences() {
        let mut buf = vec![0.5, -0.5, 1.0, -1.0];
        apply_gain(&mut buf, 0.0);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn gain_doubles_amplitude() {
        let mut buf = vec![0.25, -0.25];
        apply_gain(&mut buf, 2.0);
        assert!((buf[0] - 0.5).abs() < f32::EPSILON);
        assert!((buf[1] + 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_in_mono() {
        // 4 frames, mono, fade over 4 frames starting at offset 0.
        let mut buf = vec![1.0, 1.0, 1.0, 1.0];
        apply_fade_in(&mut buf, 1, 4, 0);
        assert!((buf[0] - 0.0).abs() < f32::EPSILON); // frame 0: 0/4 = 0.0
        assert!((buf[1] - 0.25).abs() < f32::EPSILON); // frame 1: 1/4 = 0.25
        assert!((buf[2] - 0.5).abs() < f32::EPSILON); // frame 2: 2/4 = 0.5
        assert!((buf[3] - 0.75).abs() < f32::EPSILON); // frame 3: 3/4 = 0.75
    }

    #[test]
    fn fade_in_stereo() {
        // 2 frames, stereo, fade over 4 frames starting at offset 0.
        let mut buf = vec![1.0, 1.0, 1.0, 1.0];
        apply_fade_in(&mut buf, 2, 4, 0);
        // Frame 0: t=0.0
        assert!((buf[0] - 0.0).abs() < f32::EPSILON);
        assert!((buf[1] - 0.0).abs() < f32::EPSILON);
        // Frame 1: t=0.25
        assert!((buf[2] - 0.25).abs() < f32::EPSILON);
        assert!((buf[3] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_in_with_offset() {
        // 2 frames, mono, fade over 4 frames, starting at frame 2.
        let mut buf = vec![1.0, 1.0];
        apply_fade_in(&mut buf, 1, 4, 2);
        assert!((buf[0] - 0.5).abs() < f32::EPSILON); // frame 2: 2/4
        assert!((buf[1] - 0.75).abs() < f32::EPSILON); // frame 3: 3/4
    }

    #[test]
    fn fade_in_past_length_no_change() {
        let mut buf = vec![1.0, 1.0];
        apply_fade_in(&mut buf, 1, 4, 10);
        assert!((buf[0] - 1.0).abs() < f32::EPSILON);
        assert!((buf[1] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_out_mono() {
        // 4 frames mono, total_frames=4, fade_length=4, offset=0.
        // fade_start = 4 - 4 = 0
        let mut buf = vec![1.0, 1.0, 1.0, 1.0];
        apply_fade_out(&mut buf, 1, 4, 4, 0);
        assert!((buf[0] - 1.0).abs() < f32::EPSILON); // frame 0: 1 - 0/4 = 1.0
        assert!((buf[1] - 0.75).abs() < f32::EPSILON); // frame 1: 1 - 1/4 = 0.75
        assert!((buf[2] - 0.5).abs() < f32::EPSILON); // frame 2: 1 - 2/4 = 0.5
        assert!((buf[3] - 0.25).abs() < f32::EPSILON); // frame 3: 1 - 3/4 = 0.25
    }

    #[test]
    fn fade_out_with_offset() {
        // 2 frames, mono, total_frames=8, fade_length=4, offset=6.
        // fade_start = 8 - 4 = 4
        let mut buf = vec![1.0, 1.0];
        apply_fade_out(&mut buf, 1, 4, 8, 6);
        // Frame 6: frames_into_fade=2, t = 1 - 2/4 = 0.5
        assert!((buf[0] - 0.5).abs() < f32::EPSILON);
        // Frame 7: frames_into_fade=3, t = 1 - 3/4 = 0.25
        assert!((buf[1] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_zero_length_no_change() {
        let mut buf = vec![1.0, 1.0];
        apply_fade_in(&mut buf, 1, 0, 0);
        assert_eq!(buf, vec![1.0, 1.0]);

        apply_fade_out(&mut buf, 1, 0, 10, 0);
        assert_eq!(buf, vec![1.0, 1.0]);
    }

    #[test]
    fn gain_effect_trait() {
        let effect = GainEffect { gain: 0.5 };
        let mut buf = vec![1.0, -1.0, 0.5, -0.5];
        effect.process(&mut buf, 2, 0);
        assert!((buf[0] - 0.5).abs() < f32::EPSILON);
        assert!((buf[1] + 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_effect_trait() {
        let effect = FadeEffect {
            fade_in_frames: 2,
            fade_out_frames: 2,
            total_frames: 4,
        };
        // 4 frames mono.
        let mut buf = vec![1.0, 1.0, 1.0, 1.0];
        effect.process(&mut buf, 1, 0);
        // Frame 0: fade_in t=0.0 → 0.0
        assert!((buf[0] - 0.0).abs() < f32::EPSILON);
        // Frame 1: fade_in t=0.5 → 0.5
        assert!((buf[1] - 0.5).abs() < f32::EPSILON);
        // Frame 2: fade_out, fade_start=2, frames_into=0, t=1.0 → 1.0 (but also past fade_in)
        assert!((buf[2] - 1.0).abs() < f32::EPSILON);
        // Frame 3: fade_out, frames_into=1, t=1-1/2=0.5 → 0.5
        assert!((buf[3] - 0.5).abs() < f32::EPSILON);
    }
}

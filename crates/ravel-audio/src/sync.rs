// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Video-audio synchronization clock.
//!
//! The [`SyncClock`] is the single source of truth for playback position.
//! The audio callback advances it as samples are written to the output
//! device; the video renderer reads it to determine which frame to display.
//!
//! All state is maintained via atomics so neither the audio callback (which
//! must never block) nor the UI thread needs a lock.

use ravel_core::types::FrameRate;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Shared playback clock synchronising audio output with video rendering.
///
/// Create via [`SyncClock::new`] and share the returned `Arc` between the
/// audio engine and the UI / video renderer.
pub struct SyncClock {
    /// Current playback position in audio samples (monotonically increasing
    /// while playing).
    sample_position: AtomicU64,
    /// Output sample rate in Hz (e.g. 48 000).
    sample_rate: AtomicU32,
    /// Video frame rate (numerator / denominator).
    fps_num: AtomicU32,
    fps_den: AtomicU32,
    /// `true` while the transport is playing.
    playing: AtomicBool,
}

impl SyncClock {
    /// Create a new sync clock.
    ///
    /// The clock starts in the *paused* state at position `0`.
    pub fn new(sample_rate: u32, fps: FrameRate) -> Arc<Self> {
        Arc::new(Self {
            sample_position: AtomicU64::new(0),
            sample_rate: AtomicU32::new(sample_rate),
            fps_num: AtomicU32::new(fps.num),
            fps_den: AtomicU32::new(fps.den),
            playing: AtomicBool::new(false),
        })
    }

    // ----- position ----------------------------------------------------------

    /// Advance the sample position by `samples`.
    ///
    /// Called by the audio callback after writing `samples` to the output.
    pub fn advance(&self, samples: u64) {
        self.sample_position.fetch_add(samples, Ordering::Release);
    }

    /// Current playback time in seconds.
    pub fn current_time_secs(&self) -> f64 {
        let pos = self.sample_position.load(Ordering::Acquire);
        let rate = self.sample_rate.load(Ordering::Acquire);
        if rate == 0 {
            return 0.0;
        }
        pos as f64 / rate as f64
    }

    /// Current video frame index derived from the audio position and the
    /// configured frame rate.
    pub fn current_video_frame(&self) -> u64 {
        let time = self.current_time_secs();
        let num = self.fps_num.load(Ordering::Acquire) as f64;
        let den = self.fps_den.load(Ordering::Acquire) as f64;
        if den == 0.0 {
            return 0;
        }
        (time * num / den) as u64
    }

    /// Current sample position.
    pub fn sample_position(&self) -> u64 {
        self.sample_position.load(Ordering::Acquire)
    }

    // ----- seeking -----------------------------------------------------------

    /// Jump to an absolute time position (in seconds).
    ///
    /// Both audio and video should react to this: the audio prep thread will
    /// refill its buffer from the new position, and the video renderer will
    /// display the corresponding frame.
    pub fn seek(&self, time_secs: f64) {
        let rate = self.sample_rate.load(Ordering::Acquire);
        let sample = (time_secs * rate as f64) as u64;
        self.sample_position.store(sample, Ordering::Release);
    }

    /// Jump to an absolute sample position.
    pub fn seek_to_sample(&self, sample: u64) {
        self.sample_position.store(sample, Ordering::Release);
    }

    // ----- transport state ---------------------------------------------------

    /// Whether the transport is currently playing.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    /// Set the transport play/pause state.
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Release);
    }

    // ----- configuration -----------------------------------------------------

    /// Current output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Acquire)
    }

    /// Update the output sample rate (e.g. when the audio device changes).
    pub fn set_sample_rate(&self, rate: u32) {
        self.sample_rate.store(rate, Ordering::Release);
    }

    /// Current video frame rate.
    pub fn fps(&self) -> FrameRate {
        let num = self.fps_num.load(Ordering::Acquire);
        let den = self.fps_den.load(Ordering::Acquire);
        FrameRate::new(num, den)
    }

    /// Update the video frame rate.
    pub fn set_fps(&self, fps: FrameRate) {
        self.fps_num.store(fps.num, Ordering::Release);
        self.fps_den.store(fps.den, Ordering::Release);
    }

    /// Convert a time in seconds to a sample position at the current rate.
    pub fn time_to_samples(&self, time_secs: f64) -> u64 {
        let rate = self.sample_rate.load(Ordering::Acquire);
        (time_secs * rate as f64) as u64
    }

    /// Convert a sample position to time in seconds at the current rate.
    pub fn samples_to_time(&self, samples: u64) -> f64 {
        let rate = self.sample_rate.load(Ordering::Acquire);
        if rate == 0 {
            return 0.0;
        }
        samples as f64 / rate as f64
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fps_30() -> FrameRate {
        FrameRate::new(30, 1)
    }

    #[test]
    fn initial_state() {
        let clock = SyncClock::new(48_000, fps_30());
        assert_eq!(clock.sample_position(), 0);
        assert!((clock.current_time_secs() - 0.0).abs() < f64::EPSILON);
        assert_eq!(clock.current_video_frame(), 0);
        assert!(!clock.is_playing());
    }

    #[test]
    fn advance_updates_time() {
        let clock = SyncClock::new(48_000, fps_30());
        // Advance by exactly 1 second worth of samples.
        clock.advance(48_000);
        assert!((clock.current_time_secs() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn video_frame_from_audio_position() {
        let clock = SyncClock::new(48_000, fps_30());
        // 1 second at 30fps = frame 30.
        clock.advance(48_000);
        assert_eq!(clock.current_video_frame(), 30);
    }

    #[test]
    fn seek_resets_position() {
        let clock = SyncClock::new(48_000, fps_30());
        clock.advance(48_000 * 10); // 10 seconds
        clock.seek(2.5);
        assert!((clock.current_time_secs() - 2.5).abs() < 1e-6);
    }

    #[test]
    fn play_pause_toggle() {
        let clock = SyncClock::new(48_000, fps_30());
        assert!(!clock.is_playing());
        clock.set_playing(true);
        assert!(clock.is_playing());
        clock.set_playing(false);
        assert!(!clock.is_playing());
    }

    #[test]
    fn time_sample_conversion_roundtrip() {
        let clock = SyncClock::new(44_100, fps_30());
        let samples = clock.time_to_samples(3.0);
        assert_eq!(samples, 132_300); // 44100 * 3
        let time = clock.samples_to_time(132_300);
        assert!((time - 3.0).abs() < 1e-9);
    }

    #[test]
    fn fractional_fps() {
        // 29.97fps = 30000/1001
        let clock = SyncClock::new(48_000, FrameRate::new(30_000, 1001));
        // 1 second worth of samples.
        clock.advance(48_000);
        // 1s × (30000/1001) ≈ 29.97 → frame 29 (truncated).
        assert_eq!(clock.current_video_frame(), 29);
    }

    #[test]
    fn update_sample_rate() {
        let clock = SyncClock::new(48_000, fps_30());
        clock.advance(48_000); // 1 second at 48kHz
        assert!((clock.current_time_secs() - 1.0).abs() < 1e-9);

        clock.set_sample_rate(96_000);
        // Same sample position, but now at 96kHz → 0.5 seconds.
        assert!((clock.current_time_secs() - 0.5).abs() < 1e-9);
    }
}

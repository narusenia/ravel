// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Frame-accurate playback clock
//! (`docs/implementation/playback-foundation-plan.md`, TASK-013 step 1).
//!
//! [`PlaybackClock`] maps a monotonic time source to a frame index without
//! accumulating error: the current frame is always computed as
//! `base_frame + (now - base_instant) × fps`, never by incrementing per
//! tick, so timer jitter can drop frames but can never drift the clock.
//!
//! The time source is injected as an argument on every call. Today the
//! caller passes `Instant::now()` (wall-clock master); when audio playback
//! integration lands (TASK-013 step 2, currently out of scope), the same
//! interface accepts a clock derived from the audio device's sample
//! position instead.

use crate::types::FrameRate;
use std::time::Instant;

/// Playback transport state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// A frame-accurate transport clock over `[0, duration_frames)`.
#[derive(Clone, Debug)]
pub struct PlaybackClock {
    fps: FrameRate,
    duration_frames: u64,
    state: PlaybackState,
    /// Frame position when `base_instant` was captured (or while not
    /// playing, the current position).
    base_frame: u64,
    /// Time origin of the current play segment; `Some` only while playing.
    base_instant: Option<Instant>,
}

impl PlaybackClock {
    /// Create a stopped clock at frame 0.
    pub fn new(fps: FrameRate, duration_frames: u64) -> Self {
        Self {
            fps,
            duration_frames,
            state: PlaybackState::Stopped,
            base_frame: 0,
            base_instant: None,
        }
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn fps(&self) -> FrameRate {
        self.fps
    }

    pub fn duration_frames(&self) -> u64 {
        self.duration_frames
    }

    /// Last frame index that is inside the timeline (`duration - 1`),
    /// or 0 for an empty timeline.
    fn last_frame(&self) -> u64 {
        self.duration_frames.saturating_sub(1)
    }

    /// The frame under the playhead at time `now`.
    ///
    /// While playing this derives the frame from the elapsed time since the
    /// play origin; reaching the end pauses on the last frame (no looping
    /// yet). While paused/stopped it returns the held position.
    pub fn current_frame(&mut self, now: Instant) -> u64 {
        if let Some(base) = self.base_instant {
            let elapsed = now.saturating_duration_since(base);
            let advanced = (elapsed.as_secs_f64() * self.fps.as_f64()) as u64;
            let frame = self.base_frame.saturating_add(advanced);
            if frame >= self.last_frame() {
                // End of timeline: hold the last frame and pause.
                self.base_frame = self.last_frame();
                self.base_instant = None;
                self.state = PlaybackState::Paused;
                return self.base_frame;
            }
            frame
        } else {
            self.base_frame
        }
    }

    /// Begin (or resume) playback at time `now` from the current position.
    /// Playing from the end restarts at frame 0.
    pub fn play(&mut self, now: Instant) {
        if self.state == PlaybackState::Playing {
            return;
        }
        if self.base_frame >= self.last_frame() {
            self.base_frame = 0;
        }
        self.base_instant = Some(now);
        self.state = PlaybackState::Playing;
    }

    /// Freeze the playhead at its position as of `now`.
    pub fn pause(&mut self, now: Instant) {
        if self.state != PlaybackState::Playing {
            return;
        }
        self.base_frame = self.current_frame(now);
        self.base_instant = None;
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        }
    }

    /// Toggle between playing and paused, returning the new state.
    pub fn toggle(&mut self, now: Instant) -> PlaybackState {
        match self.state {
            PlaybackState::Playing => self.pause(now),
            _ => self.play(now),
        }
        self.state
    }

    /// Stop playback and rewind to frame 0.
    pub fn stop(&mut self) {
        self.base_frame = 0;
        self.base_instant = None;
        self.state = PlaybackState::Stopped;
    }

    /// Move the playhead to `frame` (clamped to the timeline). Playing
    /// clocks keep playing from the new position.
    pub fn seek(&mut self, frame: u64, now: Instant) {
        let frame = frame.min(self.last_frame());
        self.base_frame = frame;
        if self.state == PlaybackState::Playing {
            self.base_instant = Some(now);
        }
    }

    /// Step by whole frames (e.g. ±1) from the position at `now`, pausing
    /// playback: stepping is a precision operation.
    pub fn step(&mut self, delta: i64, now: Instant) -> u64 {
        let current = self.current_frame(now);
        self.pause(now);
        let target = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.unsigned_abs())
        };
        let target = target.min(self.last_frame());
        self.base_frame = target;
        if self.state == PlaybackState::Stopped {
            self.state = PlaybackState::Paused;
        }
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn clock() -> (PlaybackClock, Instant) {
        (PlaybackClock::new(FPS, 300), Instant::now())
    }

    fn at(t0: Instant, millis: u64) -> Instant {
        t0 + Duration::from_millis(millis)
    }

    #[test]
    fn play_advances_frames_at_fps() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        assert_eq!(clock.current_frame(t0), 0);
        assert_eq!(clock.current_frame(at(t0, 1000)), 30);
        // Mid-frame time truncates to the frame under the playhead.
        assert_eq!(clock.current_frame(at(t0, 1017)), 30);
        assert_eq!(clock.current_frame(at(t0, 1034)), 31);
    }

    #[test]
    fn frame_derivation_does_not_drift_across_many_ticks() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        // Simulate 200 jittery ticks; the reported frame must always equal
        // the closed-form value for the tick's absolute time.
        for i in 0..200u64 {
            let jitter = (i * 7) % 13;
            let millis = i * 33 + jitter;
            let expected = (millis as f64 / 1000.0 * 30.0) as u64;
            if expected >= 299 {
                break;
            }
            assert_eq!(clock.current_frame(at(t0, millis)), expected);
        }
    }

    #[test]
    fn pause_holds_and_resume_continues_without_jump() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        clock.pause(at(t0, 1000)); // frame 30
        assert_eq!(clock.state(), PlaybackState::Paused);
        // Time passing while paused does not move the playhead.
        assert_eq!(clock.current_frame(at(t0, 5000)), 30);

        clock.play(at(t0, 5000));
        assert_eq!(clock.current_frame(at(t0, 6000)), 60);
    }

    #[test]
    fn reaching_the_end_pauses_on_the_last_frame() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        // 300 frames at 30 fps = 10 s; far beyond that:
        assert_eq!(clock.current_frame(at(t0, 60_000)), 299);
        assert_eq!(clock.state(), PlaybackState::Paused);
        // Playing again from the end restarts at 0.
        clock.play(at(t0, 61_000));
        assert_eq!(clock.current_frame(at(t0, 61_000)), 0);
    }

    #[test]
    fn seek_clamps_and_keeps_playing() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        clock.seek(100, at(t0, 1000));
        assert_eq!(clock.current_frame(at(t0, 1000)), 100);
        assert_eq!(clock.state(), PlaybackState::Playing);
        assert_eq!(clock.current_frame(at(t0, 2000)), 130);

        clock.seek(9999, at(t0, 2000));
        assert_eq!(clock.current_frame(at(t0, 2000)), 299);
    }

    #[test]
    fn step_moves_one_frame_and_pauses() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        let frame = clock.step(1, at(t0, 1000)); // playing at frame 30 → 31
        assert_eq!(frame, 31);
        assert_eq!(clock.state(), PlaybackState::Paused);
        assert_eq!(clock.step(-1, at(t0, 2000)), 30);
        assert_eq!(clock.step(-1, at(t0, 2000)), 29);

        // Stepping never leaves the timeline.
        clock.stop();
        assert_eq!(clock.step(-1, at(t0, 3000)), 0);
        assert_eq!(clock.state(), PlaybackState::Paused);
    }

    #[test]
    fn stop_rewinds_to_zero() {
        let (mut clock, t0) = clock();
        clock.play(t0);
        clock.current_frame(at(t0, 1000));
        clock.stop();
        assert_eq!(clock.state(), PlaybackState::Stopped);
        assert_eq!(clock.current_frame(at(t0, 2000)), 0);
    }

    #[test]
    fn empty_timeline_stays_at_zero() {
        let mut clock = PlaybackClock::new(FPS, 0);
        let t0 = Instant::now();
        clock.play(t0);
        assert_eq!(clock.current_frame(at(t0, 1000)), 0);
    }

    #[test]
    fn fractional_frame_rates_are_frame_accurate() {
        // 24000/1001 ≈ 23.976 fps.
        let mut clock = PlaybackClock::new(FrameRate::new(24000, 1001), 240);
        let t0 = Instant::now();
        clock.play(t0);
        // After exactly 1001/24000 × 48 seconds, 48 frames have elapsed.
        let seconds = 1001.0 / 24000.0 * 48.0;
        let now = t0 + Duration::from_secs_f64(seconds + 1e-6);
        assert_eq!(clock.current_frame(now), 48);
    }
}

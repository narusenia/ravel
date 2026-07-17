// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Playback transport controller
//! (`docs/implementation/playback-foundation-plan.md`, units 2 and 3).
//!
//! [`PlaybackController`] is the GPUI host for the frame-accurate
//! [`PlaybackClock`]: transport commands (toggle/stop/step) mutate the clock,
//! and while playing a spawned task wakes once per frame interval, moving the
//! Timeline playhead and posting one background evaluation request whenever
//! the clock's current frame changed. Timer jitter therefore drops frames but
//! never drifts the clock, and evaluation stays off the UI thread
//! (latest-wins coalescing in [`EvalService`]).
//!
//! The pure transport state lives in [`Transport`] so the frame/drop
//! bookkeeping is testable without GPUI; the controller only adds the
//! timeline/eval glue. `eval.request` is deliberately confined to
//! [`PlaybackController::publish_position`] — the evaluator becomes
//! Document-aware in `layer-network-model-plan.md` Phase 1 and this is the
//! one playback call site that its Phase 3 will rewrite (today it evaluates
//! the NodeEditor's selected node; root-composition output evaluation is
//! that plan's scope).

use gpui::{App, Context, Entity};
use ravel_core::eval::EvalContext;
use ravel_core::runtime::InvalidationHint;
use ravel_core::runtime::playback::{PlaybackClock, PlaybackState};
use ravel_core::types::FrameRate;
use ravel_ui::command::CommandId;
use std::time::{Duration, Instant};

use crate::panels;

/// Evaluation resolution for playback requests; matches the selection path
/// in `NodeEditorPanel::evaluate_for_viewer`.
const EVAL_RESOLUTION: (u32, u32) = (512, 512);

/// A transport state change that hosts must reflect in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportUpdate {
    /// The frame now under the playhead.
    pub frame: u64,
    /// Whether the clock is still running after this change.
    pub playing: bool,
}

/// Pure transport bookkeeping over a [`PlaybackClock`]: the last published
/// frame and the count of frames skipped by tick jitter or slow ticks.
/// Headless — the time source is always an argument.
#[derive(Clone, Debug)]
pub struct Transport {
    clock: PlaybackClock,
    last_frame: u64,
    dropped_frames: u64,
}

impl Transport {
    pub fn new(fps: FrameRate, duration_frames: u64) -> Self {
        Self {
            clock: PlaybackClock::new(fps, duration_frames),
            last_frame: 0,
            dropped_frames: 0,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.clock.state() == PlaybackState::Playing
    }

    pub fn state(&self) -> PlaybackState {
        self.clock.state()
    }

    /// The frame most recently published to the UI.
    pub fn current_frame(&self) -> u64 {
        self.last_frame
    }

    /// Frames skipped by ticks since playback last started.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub fn fps(&self) -> FrameRate {
        self.clock.fps()
    }

    /// Wall-clock interval of one frame (floored at 1 ms so a degenerate
    /// frame rate cannot busy-spin the tick loop).
    pub fn frame_interval(&self) -> Duration {
        let fps = self.clock.fps();
        let nanos = 1_000_000_000u64 * fps.den as u64 / fps.num.max(1) as u64;
        Duration::from_nanos(nanos).max(Duration::from_millis(1))
    }

    /// Adopt the timeline's frame rate / duration. A change rebuilds the
    /// clock at the (clamped) current position, preserving the transport
    /// state — a playing clock keeps playing from that position. Returns
    /// whether anything changed, so a playing caller can restart its tick
    /// loop with the new frame interval.
    pub fn sync_params(&mut self, fps: FrameRate, duration_frames: u64, now: Instant) -> bool {
        if self.clock.fps() == fps && self.clock.duration_frames() == duration_frames {
            return false;
        }
        let state = self.clock.state();
        let frame = self.last_frame.min(duration_frames.saturating_sub(1));
        self.clock = PlaybackClock::new(fps, duration_frames);
        self.clock.seek(frame, now);
        match state {
            PlaybackState::Playing => self.clock.play(now),
            // step(0) parks a non-empty stopped clock in Paused in place.
            PlaybackState::Paused => {
                self.clock.step(0, now);
            }
            PlaybackState::Stopped => {}
        }
        self.last_frame = self.clock.current_frame(now);
        true
    }

    pub fn toggle(&mut self, now: Instant) -> TransportUpdate {
        let was_playing = self.is_playing();
        self.clock.toggle(now);
        if !was_playing && self.is_playing() {
            self.dropped_frames = 0;
        }
        self.last_frame = self.clock.current_frame(now);
        TransportUpdate {
            frame: self.last_frame,
            playing: self.is_playing(),
        }
    }

    pub fn stop(&mut self) -> TransportUpdate {
        self.clock.stop();
        self.last_frame = 0;
        TransportUpdate {
            frame: 0,
            playing: false,
        }
    }

    pub fn step(&mut self, delta: i64, now: Instant) -> TransportUpdate {
        self.last_frame = self.clock.step(delta, now);
        TransportUpdate {
            frame: self.last_frame,
            playing: false,
        }
    }

    /// Move the playhead to `frame` (clamped to the timeline). A playing
    /// clock keeps playing from the new position.
    pub fn seek(&mut self, frame: u64, now: Instant) -> TransportUpdate {
        self.clock.seek(frame, now);
        self.last_frame = self.clock.current_frame(now);
        TransportUpdate {
            frame: self.last_frame,
            playing: self.is_playing(),
        }
    }

    /// One playback tick: returns the update to publish when the clock's
    /// frame moved since the previous publication, `None` otherwise. Frames
    /// skipped between ticks are counted as dropped.
    pub fn tick(&mut self, now: Instant) -> Option<TransportUpdate> {
        if !self.is_playing() {
            return None;
        }
        let frame = self.clock.current_frame(now); // may auto-pause at the end
        if frame == self.last_frame {
            return None;
        }
        if frame > self.last_frame + 1 {
            self.dropped_frames += frame - self.last_frame - 1;
        }
        self.last_frame = frame;
        Some(TransportUpdate {
            frame,
            playing: self.is_playing(),
        })
    }
}

/// Durable registry of the app's single [`PlaybackController`], so the
/// Timeline panel can route playhead scrubs into the clock.
pub struct PlaybackControllerHandle(pub gpui::WeakEntity<PlaybackController>);

impl gpui::Global for PlaybackControllerHandle {}

/// GPUI entity driving playback: owns the [`Transport`] and, while playing,
/// a tick task that wakes once per frame interval.
pub struct PlaybackController {
    transport: Transport,
    /// Generation of the running tick loop; bumping it makes any older loop
    /// exit on its next wake so play/pause churn never stacks loops.
    epoch: u64,
}

impl PlaybackController {
    pub fn new() -> Self {
        Self {
            // Mirrors the default composition (30 fps, 300 frames) until the
            // first command syncs from the live timeline; a zero-duration
            // placeholder would make every transport command a no-op when no
            // Timeline panel has been built yet.
            transport: Transport::new(FrameRate::new(30, 1), 300),
            epoch: 0,
        }
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Handles a delegated transport command. Returns `false` for commands
    /// the controller does not own.
    pub fn handle_command(&mut self, cmd: CommandId, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        self.sync_from_timeline(now, cx);
        let update = match cmd {
            CommandId::PlaybackToggle => self.transport.toggle(now),
            CommandId::PlaybackStop => {
                let dropped = self.transport.dropped_frames();
                if dropped > 0 {
                    tracing::debug!(dropped, "playback stopped with dropped frames");
                }
                self.transport.stop()
            }
            CommandId::FrameStepForward => self.transport.step(1, now),
            CommandId::FrameStepBackward => self.transport.step(-1, now),
            _ => return false,
        };
        self.publish(update, cx);
        if update.playing {
            self.spawn_tick_loop(cx);
        }
        true
    }

    /// Seeks the clock to a playhead position the Timeline panel already
    /// displays (ruler click/drag). The caller is the panel itself, still on
    /// the entity update stack, so this must neither read nor write the
    /// timeline entity — the panel passes its composition parameters instead
    /// and has already set its own playhead.
    pub fn seek_from_timeline(
        &mut self,
        frame: u64,
        fps: FrameRate,
        duration_frames: u64,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        let params_changed = self.transport.sync_params(fps, duration_frames, now);
        let update = self.transport.seek(frame, now);
        self.publish_position(update, cx);
        // A frame-rate change invalidates the running tick loop's interval;
        // restarting bumps the epoch so the old loop exits on its next wake.
        if params_changed && update.playing {
            self.spawn_tick_loop(cx);
        }
    }

    /// Adopt the live timeline's frame rate and duration, so the clock always
    /// matches what the Timeline panel displays.
    fn sync_from_timeline(&mut self, now: Instant, cx: &App) {
        if let Some(timeline) = Self::timeline(cx) {
            let (fps, duration) = timeline.read(cx).composition_params();
            self.transport.sync_params(fps, duration, now);
        }
    }

    fn timeline(cx: &App) -> Option<Entity<panels::timeline::TimelineGpuiPanel>> {
        cx.try_global::<panels::TimelinePanelHandle>()
            .and_then(|handle| handle.0.upgrade())
    }

    /// Publishes one transport position: moves the Timeline playhead, then
    /// shares the position (evaluation follows in unit 3 of the plan).
    fn publish(&mut self, update: TransportUpdate, cx: &mut Context<Self>) {
        if let Some(timeline) = Self::timeline(cx) {
            timeline.update(cx, |timeline, cx| {
                timeline.set_playhead(update.frame);
                cx.notify();
            });
        }
        self.publish_position(update, cx);
    }

    /// Timeline-independent half of a position change: records the shared
    /// [`panels::PlaybackPosition`] and posts one background evaluation
    /// request for the frame. Playback's only `eval.request` call site (see
    /// the module docs). Slow evaluation never blocks here — the worker
    /// coalesces queued requests latest-wins, which is what turns an
    /// overloaded graph into dropped viewer frames instead of UI stalls.
    fn publish_position(&mut self, update: TransportUpdate, cx: &mut Context<Self>) {
        cx.set_global(panels::PlaybackPosition {
            frame: update.frame,
            fps: self.transport.fps(),
        });
        let editor = cx
            .try_global::<panels::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade());
        if let Some(editor) = editor {
            let ctx = EvalContext::new(update.frame, self.transport.fps(), EVAL_RESOLUTION);
            editor.update(cx, |editor, _| {
                if let Some((graph, node, eval)) = editor.playback_eval_parts() {
                    eval.request(graph, node, ctx, InvalidationHint::None);
                }
            });
        }
        cx.notify();
    }

    /// Spawns the per-frame tick task for the current play segment.
    fn spawn_tick_loop(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        let epoch = self.epoch;
        let interval = self.transport.frame_interval();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let finished = this.update(cx, |this, cx| {
                    if this.epoch != epoch || !this.transport.is_playing() {
                        return true;
                    }
                    if let Some(update) = this.transport.tick(Instant::now()) {
                        this.publish(update, cx);
                        if !update.playing {
                            // Reached the end of the timeline.
                            tracing::debug!(
                                dropped = this.transport.dropped_frames(),
                                "playback finished"
                            );
                        }
                    }
                    !this.transport.is_playing()
                });
                match finished {
                    Ok(true) | Err(_) => break,
                    Ok(false) => {}
                }
            }
        })
        .detach();
    }
}

impl Default for PlaybackController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn transport() -> (Transport, Instant) {
        (Transport::new(FPS, 300), Instant::now())
    }

    fn at(t0: Instant, millis: u64) -> Instant {
        t0 + Duration::from_millis(millis)
    }

    #[test]
    fn toggle_starts_and_pauses() {
        let (mut t, t0) = transport();
        let update = t.toggle(t0);
        assert_eq!(
            update,
            TransportUpdate {
                frame: 0,
                playing: true
            }
        );
        let update = t.toggle(at(t0, 1000));
        assert_eq!(
            update,
            TransportUpdate {
                frame: 30,
                playing: false
            }
        );
        assert_eq!(t.state(), PlaybackState::Paused);
    }

    #[test]
    fn tick_publishes_only_frame_changes_and_counts_drops() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        // Within frame 0's interval: nothing to publish.
        assert_eq!(t.tick(at(t0, 10)), None);
        // Normal cadence: one frame forward, no drops.
        assert_eq!(
            t.tick(at(t0, 34)),
            Some(TransportUpdate {
                frame: 1,
                playing: true
            })
        );
        assert_eq!(t.dropped_frames(), 0);
        // A late tick skips frames 2..=4: three shown as one, two dropped.
        assert_eq!(
            t.tick(at(t0, 167)),
            Some(TransportUpdate {
                frame: 5,
                playing: true
            })
        );
        assert_eq!(t.dropped_frames(), 3);
    }

    #[test]
    fn tick_reports_auto_pause_at_the_end() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        let update = t
            .tick(at(t0, 60_000))
            .expect("past the end moves the frame");
        assert_eq!(update.frame, 299);
        assert!(!update.playing);
        assert_eq!(t.tick(at(t0, 61_000)), None);
    }

    #[test]
    fn step_moves_one_frame_and_pauses() {
        let (mut t, t0) = transport();
        assert_eq!(t.step(1, t0).frame, 1);
        assert_eq!(t.step(1, t0).frame, 2);
        assert_eq!(t.step(-1, t0).frame, 1);
        assert_eq!(t.state(), PlaybackState::Paused);
        // Never leaves the timeline.
        assert_eq!(t.step(-5, t0).frame, 0);
    }

    #[test]
    fn seek_clamps_and_keeps_the_play_state() {
        let (mut t, t0) = transport();
        assert_eq!(
            t.seek(9999, t0),
            TransportUpdate {
                frame: 299,
                playing: false
            }
        );
        t.toggle(at(t0, 100));
        let update = t.seek(50, at(t0, 200));
        assert_eq!(
            update,
            TransportUpdate {
                frame: 50,
                playing: true
            }
        );
    }

    #[test]
    fn stop_rewinds_to_frame_zero() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        t.tick(at(t0, 1000));
        let update = t.stop();
        assert_eq!(
            update,
            TransportUpdate {
                frame: 0,
                playing: false
            }
        );
        assert_eq!(t.state(), PlaybackState::Stopped);
    }

    #[test]
    fn drop_counter_resets_when_playback_restarts() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        t.tick(at(t0, 167)); // frames 1..=4 skipped behind frame 5
        assert!(t.dropped_frames() > 0);
        t.toggle(at(t0, 200)); // pause
        t.toggle(at(t0, 300)); // play again
        assert_eq!(t.dropped_frames(), 0);
    }

    #[test]
    fn sync_params_preserves_position_and_state() {
        let (mut t, t0) = transport();
        t.step(10, t0);
        assert!(t.sync_params(FrameRate::new(24, 1), 120, at(t0, 100)));
        assert_eq!(t.current_frame(), 10);
        assert_eq!(t.state(), PlaybackState::Paused);
        assert_eq!(t.fps(), FrameRate::new(24, 1));
        // Unchanged parameters are a no-op.
        assert!(!t.sync_params(FrameRate::new(24, 1), 120, at(t0, 100)));
        // Shrinking below the position clamps to the new last frame.
        assert!(t.sync_params(FrameRate::new(24, 1), 5, at(t0, 100)));
        assert_eq!(t.current_frame(), 4);
    }

    #[test]
    fn sync_params_keeps_a_playing_clock_playing() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        t.tick(at(t0, 1000)); // frame 30
        assert!(t.sync_params(FrameRate::new(60, 1), 600, at(t0, 1000)));
        assert_eq!(t.state(), PlaybackState::Playing);
        assert_eq!(t.current_frame(), 30);
        // Still advancing, now at the new rate from the resync origin.
        assert_eq!(t.tick(at(t0, 2000)).unwrap().frame, 90);
    }

    #[test]
    fn frame_interval_matches_fps() {
        let t = Transport::new(FrameRate::new(24000, 1001), 240);
        let interval = t.frame_interval();
        assert!((interval.as_secs_f64() - 1001.0 / 24000.0).abs() < 1e-6);
    }
}

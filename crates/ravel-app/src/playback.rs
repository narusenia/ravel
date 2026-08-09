// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Playback transport controller
//! (`docs/implementation/done/playback-foundation-plan.md`, units 2 and 3).
//!
//! [`PlaybackController`] is the GPUI host for the frame-accurate
//! [`PlaybackClock`]: transport commands (toggle/stop/step) mutate the clock,
//! and while playing a spawned task wakes once per frame interval, moving the
//! Timeline playhead and posting one background evaluation request whenever
//! the clock's current frame changed. Timer jitter therefore drops frames but
//! never drifts the clock, and evaluation stays off the UI thread
//! (latest-wins coalescing in [`EvalService`]).
//!
//! The time source behind a tick is a [`ClockSource`] (audio-plan unit 3):
//! the audio device's [`SyncClock`] while the active composition has audio
//! tracks and an engine runs (decision point: [`crate::audio::playback_clock`]),
//! the wall clock everywhere else. Transport commands are mirrored into the
//! audio engine so the two clocks never diverge across a switch.
//!
//! The pure transport state lives in [`Transport`] so the frame/drop
//! bookkeeping is testable without GPUI; the controller only adds the
//! timeline/eval glue. Playback's one evaluation entry point is
//! [`PlaybackController::publish_position`], which asks the
//! [`crate::project_state::ProjectState`] to re-evaluate the current viewer
//! target (active composition output by default, REQ-LAYER-007) at the new
//! frame.

use gpui::{App, Context, Entity};
use ravel_audio::SyncClock;
use ravel_core::runtime::InvalidationHint;
use ravel_core::runtime::playback::{PlaybackClock, PlaybackState};
use ravel_core::types::FrameRate;
use ravel_ui::command::CommandId;
use std::time::{Duration, Instant};

use crate::panels;

/// Time source a [`Transport`] tick reads its frame from (decision 4 of
/// `docs/implementation/audio-plan.md`).
///
/// - `Wall`: the historical wall-clock master — always the fallback when
///   there is no audio to stay in sync with (no audio tracks, no output
///   device, headless tests). Every pre-audio test drives this variant.
/// - `Audio`: the engine's [`SyncClock`], advanced by the CPAL callback as
///   samples reach the device. Used while the active composition has audio
///   tracks and an engine is running, so audio never drifts against the
///   playhead. The single decision point between the two is
///   [`crate::audio::playback_clock`].
#[derive(Clone, Copy)]
pub enum ClockSource<'a> {
    /// Wall-clock master at this instant.
    Wall(Instant),
    /// Audio-device master: sample position → frames.
    Audio(&'a SyncClock),
}

/// Frame at the audio clock's sample position for `fps` (unclamped).
fn audio_frame(sync: &SyncClock, fps: FrameRate) -> u64 {
    let rate = sync.sample_rate().max(1) as u128;
    let frame =
        sync.sample_position() as u128 * fps.num.max(1) as u128 / (fps.den.max(1) as u128 * rate);
    u64::try_from(frame).unwrap_or(u64::MAX)
}

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
    /// Frame the current (or most recent) play segment started from, which is
    /// where [`Transport::stop`] returns to while
    /// `playback.stop_returns_to_play_start` is on. `None` until playback has
    /// run once — nothing else records a "start position", so the transport
    /// keeps it itself.
    play_origin: Option<u64>,
}

impl Transport {
    pub fn new(fps: FrameRate, duration_frames: u64) -> Self {
        Self {
            clock: PlaybackClock::new(fps, duration_frames),
            last_frame: 0,
            dropped_frames: 0,
            play_origin: None,
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
        self.toggle_with(&ClockSource::Wall(now), now)
    }

    /// Toggle under an explicit clock source. Pausing on the audio clock
    /// re-anchors the wall clock at the audio position first, so the
    /// freeze lands on the frame the listener actually reached (and a
    /// later fall back to `Wall` continues from there).
    pub fn toggle_with(&mut self, clock: &ClockSource, now: Instant) -> TransportUpdate {
        let was = self.state();
        let was_playing = self.is_playing();
        if was_playing && let ClockSource::Audio(sync) = clock {
            let frame = audio_frame(sync, self.clock.fps());
            self.clock.seek(frame, now);
        }
        self.clock.toggle(now);
        if !was_playing && self.is_playing() {
            self.dropped_frames = 0;
        }
        let frame = self.clock.current_frame(now);
        // Pausing publishes the frame under the playhead; anything the tick
        // loop never published in between counts as dropped, same as a
        // late tick would.
        if was_playing && frame > self.last_frame + 1 {
            self.dropped_frames += frame - self.last_frame - 1;
        }
        self.last_frame = frame;
        // A *fresh* play starts a segment; resuming from a pause continues
        // the one already running, so it must not move the origin — pausing
        // halfway and carrying on is still the same viewing pass, and
        // "return to where playback started" means where it started, not
        // where it was last unpaused.
        //
        // Read after the toggle: playing from the end restarts at frame 0
        // (`PlaybackClock::play`), so the position before it is not where this
        // segment actually starts.
        if was == PlaybackState::Stopped && self.is_playing() {
            self.play_origin = Some(frame);
        }
        TransportUpdate {
            frame: self.last_frame,
            playing: self.is_playing(),
        }
    }

    /// Stop playback. `return_to_play_start` is the resolved
    /// `playback.stop_returns_to_play_start` setting: off (the default) rewinds
    /// to frame 0 as Ravel has always done, on returns to the frame the last
    /// play segment started from.
    ///
    /// Stopping with the setting on but no play segment on record leaves the
    /// playhead where it is: the setting exists so that stopping does not throw
    /// away a position, and with nothing to return to, rewinding to 0 would be
    /// exactly the discard the user turned off.
    pub fn stop(&mut self, now: Instant, return_to_play_start: bool) -> TransportUpdate {
        let held = self.last_frame;
        let target = if return_to_play_start {
            self.play_origin.unwrap_or(held)
        } else {
            0
        };
        self.clock.stop();
        // Stopping parks the clock at frame 0; the seek moves it back and
        // leaves the state Stopped (it re-anchors playing clocks only).
        self.clock.seek(target, now);
        self.last_frame = self.clock.current_frame(now);
        TransportUpdate {
            frame: self.last_frame,
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
    /// frame or playback state changed, `None` otherwise. Frames skipped
    /// between ticks are counted as dropped.
    pub fn tick(&mut self, now: Instant) -> Option<TransportUpdate> {
        self.tick_with(&ClockSource::Wall(now))
    }

    /// [`Self::tick`] under an explicit clock source. On `ClockSource::Audio`
    /// the frame comes from the device's sample position; reaching the end
    /// of the timeline pauses at the last frame, mirroring the wall clock's
    /// own end behavior.
    pub fn tick_with(&mut self, clock: &ClockSource) -> Option<TransportUpdate> {
        let was_playing = self.is_playing();
        if !was_playing {
            return None;
        }
        let frame = self.frame_from(clock);
        let playing = self.is_playing();
        if frame == self.last_frame && playing == was_playing {
            return None;
        }
        if frame > self.last_frame + 1 {
            self.dropped_frames += frame - self.last_frame - 1;
        }
        self.last_frame = frame;
        Some(TransportUpdate { frame, playing })
    }

    /// The frame under the playhead for the given clock source.
    fn frame_from(&mut self, clock: &ClockSource) -> u64 {
        match clock {
            ClockSource::Wall(now) => self.clock.current_frame(*now),
            ClockSource::Audio(sync) => {
                if self.clock.state() != PlaybackState::Playing {
                    return self.clock.current_frame(Instant::now());
                }
                let frame = audio_frame(sync, self.clock.fps());
                if frame >= self.clock.duration_frames() {
                    // Past the end of the timeline: hold the last frame and
                    // pause, like `PlaybackClock::current_frame` does.
                    let now = Instant::now();
                    self.clock.seek(u64::MAX, now); // clamps to the last frame
                    self.clock.pause(now);
                    self.clock.current_frame(now)
                } else {
                    frame
                }
            }
        }
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
            // first command syncs from the active composition; a
            // zero-duration placeholder would make every transport command a
            // no-op before the first sync.
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
        self.sync_from_active_composition(now, cx);
        let update = match cmd {
            CommandId::PlaybackToggle => {
                let audio_clock = crate::audio::playback_clock(cx);
                match audio_clock {
                    Some(sync) => self.transport.toggle_with(&ClockSource::Audio(&sync), now),
                    None => self.transport.toggle_with(&ClockSource::Wall(now), now),
                }
            }
            CommandId::PlaybackStop => {
                let dropped = self.transport.dropped_frames();
                if dropped > 0 {
                    tracing::debug!(dropped, "playback stopped with dropped frames");
                }
                let return_to_play_start =
                    crate::app_settings::resolved(cx).stop_returns_to_play_start;
                self.transport.stop(now, return_to_play_start)
            }
            CommandId::FrameStepForward => self.transport.step(1, now),
            CommandId::FrameStepBackward => self.transport.step(-1, now),
            _ => return false,
        };
        // Mirror the transport into the audio engine (no-op without one).
        // Play from the timeline end restarts at frame 0, so a play command
        // re-seeks the engine clock to the published frame; pauses and
        // steps freeze it in place. Stop seeks to the frame it landed on,
        // which is 0 unless `stop_returns_to_play_start` moved it.
        let seek_secs = match cmd {
            CommandId::PlaybackToggle if update.playing => Some(self.secs_at_frame(update.frame)),
            CommandId::PlaybackStop
            | CommandId::FrameStepForward
            | CommandId::FrameStepBackward => Some(self.secs_at_frame(update.frame)),
            _ => None,
        };
        crate::audio::forward_transport(update.playing, seek_secs, cx);
        self.publish(update, cx);
        if update.playing {
            self.spawn_tick_loop(cx);
        }
        true
    }

    /// Seconds at `frame` under the transport's frame rate.
    fn secs_at_frame(&self, frame: u64) -> f64 {
        let fps = self.transport.fps();
        frame as f64 * fps.den.max(1) as f64 / fps.num.max(1) as f64
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
        crate::audio::forward_transport(update.playing, Some(self.secs_at_frame(update.frame)), cx);
        self.publish_position(update, cx);
        // A frame-rate change invalidates the running tick loop's interval;
        // restarting bumps the epoch so the old loop exits on its next wake.
        if params_changed && update.playing {
            self.spawn_tick_loop(cx);
        }
    }

    /// Adopt the active composition's frame rate and duration, so the clock
    /// always matches what the Timeline displays (REQ-UI-013). Resolving
    /// from the document rather than the Timeline panel keeps the transport
    /// correct while no Timeline panel exists.
    ///
    /// With no active composition (composition 0) the clock adopts a
    /// zero-length range at the current frame rate, which makes every
    /// transport command a no-op — playback must not run over a composition
    /// that is not there.
    fn sync_from_active_composition(&mut self, now: Instant, cx: &App) {
        let params = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade())
            .and_then(|project| project.read(cx).playback_params(cx));
        let (fps, duration) = params.unwrap_or((self.transport.fps(), 0));
        self.transport.sync_params(fps, duration, now);
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
                timeline.set_playhead(update.frame, cx);
                cx.notify();
            });
        }
        self.publish_position(update, cx);
    }

    /// Timeline-independent half of a position change: records the shared
    /// [`panels::PlaybackPosition`] and asks the project state to re-evaluate
    /// the current viewer target (active composition output by default) at the
    /// new frame. Slow evaluation never blocks here — the worker coalesces
    /// queued requests latest-wins, which is what turns an overloaded graph
    /// into dropped viewer frames instead of UI stalls.
    fn publish_position(&mut self, update: TransportUpdate, cx: &mut Context<Self>) {
        // Correlates with the eval service's per-request `frame`/`generation`
        // logs: a frozen viewer whose playhead advances shows a steady stream
        // of these publishes; the eval logs then classify the cause.
        tracing::debug!(
            frame = update.frame,
            playing = update.playing,
            "playback position published; requesting viewer re-evaluation"
        );
        cx.set_global(panels::PlaybackPosition {
            frame: update.frame,
            fps: self.transport.fps(),
        });
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        if let Some(project) = project {
            project.update(cx, |project, cx| {
                project.request_viewer_eval(InvalidationHint::None, cx);
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
                    // Audio tracks + running engine ⇒ the device clock is
                    // the master; anything else stays on the wall clock.
                    // The switch is decided in exactly one place:
                    // `crate::audio::playback_clock`.
                    let audio_clock = crate::audio::playback_clock(cx);
                    let update = match audio_clock {
                        Some(sync) => this.transport.tick_with(&ClockSource::Audio(&sync)),
                        None => this.transport.tick_with(&ClockSource::Wall(Instant::now())),
                    };
                    if let Some(update) = update {
                        this.publish(update, cx);
                        if !update.playing {
                            // Reached the end of the timeline.
                            tracing::debug!(
                                dropped = this.transport.dropped_frames(),
                                "playback finished"
                            );
                            crate::audio::forward_transport(false, None, cx);
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
    fn wall_tick_reports_auto_pause_after_the_last_frame_was_published() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        let last = t.tick(at(t0, 9_967)).expect("last frame is published");
        assert_eq!(last.frame, 299);
        assert!(last.playing);

        let paused = t
            .tick(at(t0, 10_000))
            .expect("the state change must be published");
        assert_eq!(paused.frame, 299);
        assert!(!paused.playing);
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

    /// The default (`stop_returns_to_play_start` off) is what Ravel has always
    /// done, including when playback started somewhere other than frame 0.
    #[test]
    fn stop_rewinds_to_frame_zero() {
        let (mut t, t0) = transport();
        t.seek(40, t0);
        t.toggle(t0);
        t.tick(at(t0, 1000));
        let update = t.stop(at(t0, 1000), false);
        assert_eq!(
            update,
            TransportUpdate {
                frame: 0,
                playing: false
            }
        );
        assert_eq!(t.state(), PlaybackState::Stopped);
    }

    /// With the setting on, Stop returns to the frame the play segment started
    /// from — and a later play resumes from there rather than from 0.
    #[test]
    fn stop_returns_to_the_frame_playback_started_from() {
        let (mut t, t0) = transport();
        t.seek(40, t0);
        t.toggle(t0);
        t.tick(at(t0, 1000));
        assert_eq!(t.current_frame(), 70);

        let update = t.stop(at(t0, 1000), true);
        assert_eq!(
            update,
            TransportUpdate {
                frame: 40,
                playing: false
            }
        );
        assert_eq!(t.state(), PlaybackState::Stopped);

        t.toggle(at(t0, 1100));
        assert_eq!(t.current_frame(), 40);
    }

    /// Playing from the end restarts at frame 0, so that — not the frame the
    /// playhead sat on — is the segment's start.
    #[test]
    fn stop_returns_to_zero_when_playback_restarted_from_the_end() {
        let (mut t, t0) = transport();
        t.seek(299, t0);
        t.toggle(t0);
        t.tick(at(t0, 100));
        assert_eq!(t.stop(at(t0, 100), true).frame, 0);
    }

    /// Pausing does not start a new segment: stopping after a pause and a
    /// resume returns to where playback originally started, not to where it
    /// was unpaused.
    #[test]
    fn a_pause_and_resume_keeps_the_original_play_start() {
        let (mut t, t0) = transport();
        t.seek(40, t0);
        t.toggle(t0);
        t.tick(at(t0, 1000));
        assert_eq!(t.current_frame(), 70);

        t.toggle(at(t0, 1000));
        assert_eq!(t.state(), PlaybackState::Paused);
        t.toggle(at(t0, 1100));
        assert!(t.is_playing());

        assert_eq!(t.stop(at(t0, 1100), true).frame, 40);
    }

    /// Nothing has played, so there is no start position to return to: the
    /// playhead stays put rather than losing the position the user scrubbed to.
    #[test]
    fn stop_without_a_play_segment_leaves_the_playhead_alone() {
        let (mut t, t0) = transport();
        t.seek(120, t0);
        let update = t.stop(t0, true);
        assert_eq!(
            update,
            TransportUpdate {
                frame: 120,
                playing: false
            }
        );
        assert_eq!(t.state(), PlaybackState::Stopped);
        // The same stop with the setting off is the historical rewind.
        assert_eq!(t.stop(t0, false).frame, 0);
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
    fn pausing_counts_frames_the_ticks_never_published() {
        let (mut t, t0) = transport();
        t.toggle(t0);
        t.tick(at(t0, 34)); // frame 1 published
        // Pause lands on frame 5; frames 2..=4 were never published.
        let update = t.toggle(at(t0, 167));
        assert_eq!(update.frame, 5);
        assert_eq!(t.dropped_frames(), 3);
    }

    #[test]
    fn frame_interval_matches_fps() {
        let t = Transport::new(FrameRate::new(24000, 1001), 240);
        let interval = t.frame_interval();
        assert!((interval.as_secs_f64() - 1001.0 / 24000.0).abs() < 1e-6);
    }
}

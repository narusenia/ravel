// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Top-level audio engine tying together device output, mixer, resampler,
//! effects, and sync clock.
//!
//! # Architecture
//!
//! ```text
//! UI / Eval Pool
//!     │  AudioCommand (crossbeam)
//!     ▼
//! Resample Worker     ← prepares source-rate tracks away from playback
//!     │  prepared Track
//!     ▼
//! Audio Prep Thread   ← runs the mixer and effects
//!     │  Arc<[f32]> chunks (crossbeam bounded)
//!     ▼
//! CPAL Callback       ← high-priority OS thread, reads chunks, advances clock
//! ```
//!
//! The CPAL callback never allocates, never blocks, and never locks — it
//! only reads from a bounded crossbeam channel and copies samples to the
//! output buffer.  All heavyweight work (mixing, resampling, effects)
//! happens on the prep thread.

use crate::device::{self, AudioChunk, OutputConfig};
use crate::error::AudioError;
use crate::mixer::{Mixer, MixerConfig, Track, TrackGain, TrackId};
use crate::resampler;
use crate::sync::{SyncClock, TransportSync};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use ravel_core::types::FrameRate;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// Commands sent from the UI / application layer to the audio prep thread.
#[derive(Debug)]
pub enum AudioCommand {
    /// Start playback.
    Play,
    /// Pause playback.
    Pause,
    /// Seek to an absolute time position (seconds).
    Seek(f64),
    /// Add or replace a track.
    SetTrack {
        /// Complete desired track state. Its samples may be at a different
        /// sample rate; timeline placement and automation use output frames.
        track: Track,
        /// Sample rate of `track.samples`.
        sample_rate: u32,
    },
    /// Remove a track.
    RemoveTrack(TrackId),
    /// Set the constant or pre-sampled gain automation of a track.
    SetTrackGain { id: TrackId, gain: TrackGain },
    /// Set the mute state of a track.
    SetTrackMute { id: TrackId, muted: bool },
    /// Set the solo state of a track.
    SetTrackSolo { id: TrackId, solo: bool },
    /// Set fade-in length for a track (in frames at output rate).
    SetTrackFadeIn { id: TrackId, frames: usize },
    /// Set fade-out length for a track (in frames at output rate).
    SetTrackFadeOut { id: TrackId, frames: usize },
    /// Set the master output gain.
    SetMasterGain(f32),
    /// Shut down the audio engine.
    Shutdown,
}

/// Configuration for the [`AudioEngine`].
#[derive(Clone, Debug)]
pub struct AudioEngineConfig {
    /// Explicit audio output configuration. `None` adopts the default
    /// device's supported sample rate, channels, format, and buffer size.
    pub output: Option<OutputConfig>,
    /// Video frame rate for the sync clock.
    pub fps: FrameRate,
    /// Number of audio chunks to queue ahead of playback.
    /// Higher values increase latency but reduce underrun risk.
    pub queue_depth: usize,
    /// Number of frames per mixer chunk.
    pub chunk_frames: usize,
}

impl Default for AudioEngineConfig {
    fn default() -> Self {
        Self {
            output: None,
            fps: FrameRate::new(30, 1),
            queue_depth: 8,
            chunk_frames: 1024,
        }
    }
}

/// The audio engine orchestrator.
///
/// Owns the CPAL stream, the sync clock, and a handle to the prep thread.
/// Communicate with it by sending [`AudioCommand`]s.
pub struct AudioEngine {
    command_tx: Sender<AudioCommand>,
    sync_clock: Arc<SyncClock>,
    output_config: OutputConfig,
    /// Keep the CPAL stream alive. Dropping this stops playback.
    _stream: cpal::Stream,
    /// Handle to the prep thread (joined on shutdown).
    prep_handle: Option<thread::JoinHandle<()>>,
    /// Handle to the offline track-preparation worker.
    resample_handle: Option<thread::JoinHandle<()>>,
    resample_queue: Arc<ResampleQueue>,
}

struct ResampleJob {
    generation: u64,
    track: Track,
    input_rate: u32,
    output_rate: u32,
}

struct PreparedTrack {
    generation: u64,
    track_id: TrackId,
    result: Result<Track, AudioError>,
}

struct ResampleQueue {
    pending: Mutex<HashMap<TrackId, ResampleJob>>,
    latest_generations: Mutex<HashMap<TrackId, u64>>,
    wake_tx: Sender<()>,
    shutdown: AtomicBool,
}

impl ResampleQueue {
    fn new(wake_tx: Sender<()>) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            latest_generations: Mutex::new(HashMap::new()),
            wake_tx,
            shutdown: AtomicBool::new(false),
        })
    }

    fn submit(&self, job: ResampleJob) -> Result<(), AudioError> {
        self.latest_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(job.track.id, job.generation);
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(job.track.id, job);
        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err(AudioError::NotRunning),
        }
    }

    fn supersede(&self, track_id: TrackId, generation: u64) {
        self.latest_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(track_id, generation);
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&track_id);
    }

    fn take_pending(&self) -> Option<ResampleJob> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let track_id = *pending.keys().next()?;
        pending.remove(&track_id)
    }

    fn is_current(&self, track_id: TrackId, generation: u64) -> bool {
        !self.shutdown.load(Ordering::Acquire)
            && self
                .latest_generations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&track_id)
                .copied()
                == Some(generation)
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.wake_tx.try_send(());
    }
}

struct PrepState {
    mixer: Mixer,
    sync_clock: Arc<SyncClock>,
    transport: Arc<TransportSync>,
    resample_queue: Arc<ResampleQueue>,
    track_generations: HashMap<TrackId, u64>,
    output_rate: u32,
    read_position: usize,
}

impl AudioEngine {
    /// Create and start a new audio engine.
    ///
    /// This opens the default audio output device, spawns the prep thread,
    /// and begins sending silence to the output.  Call
    /// [`AudioEngine::send`]`(AudioCommand::Play)` to start playback.
    pub fn new(config: AudioEngineConfig) -> Result<Self, AudioError> {
        let device = device::default_output_device()?;
        let output_config = match config.output {
            Some(output) => output,
            None => device::default_device_config(&device)?,
        };
        let sync_clock = SyncClock::new(output_config.sample_rate, config.fps);
        let transport = TransportSync::new();

        // Channel: prep thread → CPAL callback (audio chunks).
        let (chunk_tx, chunk_rx) = bounded::<AudioChunk>(config.queue_depth);

        // Channel: UI → prep thread (commands).
        let (command_tx, command_rx) = bounded::<AudioCommand>(64);

        // Channels: prep thread → resample worker → prep thread.
        let (resample_wake_tx, resample_wake_rx) = bounded::<()>(1);
        let resample_queue = ResampleQueue::new(resample_wake_tx);
        let (prepared_tx, prepared_rx) = unbounded::<PreparedTrack>();

        // Build CPAL stream.
        let stream = device::build_output_stream(
            &device,
            &output_config,
            chunk_rx,
            sync_clock.clone(),
            transport.clone(),
        )?;

        // Spawn the audio prep thread.
        let prep_clock = sync_clock.clone();
        let output_rate = output_config.sample_rate;
        let output_channels = output_config.channels as u32;
        let chunk_frames = config.chunk_frames;

        let worker_queue = resample_queue.clone();
        let resample_handle = thread::Builder::new()
            .name("ravel-audio-resample".into())
            .spawn(move || resample_worker_main(worker_queue, resample_wake_rx, prepared_tx))
            .map_err(|e| AudioError::Other(format!("failed to spawn resample worker: {e}")))?;

        let prep_resample_queue = resample_queue.clone();
        let prep_handle = match thread::Builder::new()
            .name("ravel-audio-prep".into())
            .spawn(move || {
                prep_thread_main(
                    command_rx,
                    chunk_tx,
                    prepared_rx,
                    chunk_frames,
                    PrepState {
                        mixer: Mixer::new(MixerConfig {
                            output_sample_rate: output_rate,
                            output_channels,
                        }),
                        sync_clock: prep_clock,
                        transport,
                        resample_queue: prep_resample_queue,
                        track_generations: HashMap::new(),
                        output_rate,
                        read_position: 0,
                    },
                );
            }) {
            Ok(handle) => handle,
            Err(error) => {
                resample_queue.request_shutdown();
                let _ = resample_handle.join();
                return Err(AudioError::Other(format!(
                    "failed to spawn prep thread: {error}"
                )));
            }
        };

        tracing::info!(
            sample_rate = output_rate,
            channels = output_config.channels,
            sample_format = %output_config.sample_format,
            "audio engine started"
        );

        Ok(Self {
            command_tx,
            sync_clock,
            output_config,
            _stream: stream,
            prep_handle: Some(prep_handle),
            resample_handle: Some(resample_handle),
            resample_queue,
        })
    }

    /// Send a command to the audio engine.
    pub fn send(&self, cmd: AudioCommand) -> Result<(), AudioError> {
        self.command_tx
            .send(cmd)
            .map_err(|_| AudioError::NotRunning)
    }

    /// Start playback.
    pub fn play(&self) -> Result<(), AudioError> {
        self.send(AudioCommand::Play)
    }

    /// Pause playback.
    pub fn pause(&self) -> Result<(), AudioError> {
        self.send(AudioCommand::Pause)
    }

    /// Seek to an absolute position (seconds).
    pub fn seek(&self, time_secs: f64) -> Result<(), AudioError> {
        self.send(AudioCommand::Seek(time_secs))
    }

    /// Get a reference to the shared sync clock.
    pub fn sync_clock(&self) -> &Arc<SyncClock> {
        &self.sync_clock
    }

    /// The device configuration used consistently by the stream, mixer, and clock.
    pub fn output_config(&self) -> &OutputConfig {
        &self.output_config
    }

    /// Shut down the audio engine, stopping playback and joining the prep
    /// thread.
    pub fn shutdown(mut self) {
        self.resample_queue.request_shutdown();
        let _ = self.command_tx.send(AudioCommand::Shutdown);
        if let Some(handle) = self.prep_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.resample_handle.take() {
            let _ = handle.join();
        }
        tracing::info!("audio engine shut down");
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Best-effort shutdown if not already done.
        self.resample_queue.request_shutdown();
        let _ = self.command_tx.send(AudioCommand::Shutdown);
        if let Some(handle) = self.prep_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.resample_handle.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Prep thread
// ---------------------------------------------------------------------------

/// Main loop of the audio preparation thread.
///
/// This thread runs the mixer to produce chunks of audio and pushes them
/// into the bounded channel consumed by the CPAL callback.  It also
/// processes incoming [`AudioCommand`]s.
fn prep_thread_main(
    command_rx: Receiver<AudioCommand>,
    chunk_tx: Sender<AudioChunk>,
    prepared_rx: Receiver<PreparedTrack>,
    chunk_frames: usize,
    mut state: PrepState,
) {
    loop {
        while let Ok(prepared) = prepared_rx.try_recv() {
            apply_prepared_track(prepared, &mut state);
        }

        // Drain all pending commands before mixing. This is the only point
        // where playback-time track state changes, so every update becomes
        // visible at the next complete prepared-block boundary.
        loop {
            match command_rx.try_recv() {
                Ok(cmd) => {
                    if !handle_command(&cmd, &mut state) {
                        return; // Shutdown
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        }

        if !state.sync_clock.is_playing() {
            // When paused, wait for a command instead of busy-spinning.
            crossbeam_channel::select_biased! {
                recv(command_rx) -> message => match message {
                    Ok(cmd) => {
                    if !handle_command(&cmd, &mut state) {
                        return;
                    }
                    }
                    Err(_) => return,
                },
                recv(prepared_rx) -> message => match message {
                    Ok(prepared) => apply_prepared_track(prepared, &mut state),
                    Err(_) => return,
                }
            }
            continue;
        }

        // Mix the next chunk.
        let chunk = AudioChunk {
            epoch: state.transport.epoch(),
            samples: state.mixer.mix(state.read_position, chunk_frames).into(),
        };

        // A full queue must not delay Pause or Seek. Only advance the mix
        // position after the block is accepted by the callback queue.
        crossbeam_channel::select_biased! {
            recv(command_rx) -> message => match message {
                Ok(cmd) => {
                    if !handle_command(&cmd, &mut state) {
                        return;
                    }
                }
                Err(_) => return,
            },
            recv(prepared_rx) -> message => match message {
                Ok(prepared) => apply_prepared_track(prepared, &mut state),
                Err(_) => return,
            },
            send(chunk_tx, chunk) -> result => {
                if result.is_err() {
                    tracing::warn!("audio chunk channel disconnected");
                    return;
                }
                state.read_position += chunk_frames;
            }
        }
    }
}

/// Process a single command. Returns `false` on [`AudioCommand::Shutdown`].
fn handle_command(cmd: &AudioCommand, state: &mut PrepState) -> bool {
    match cmd {
        AudioCommand::Play => {
            state.sync_clock.set_playing(true);
            tracing::debug!("playback started");
        }
        AudioCommand::Pause => {
            state
                .transport
                .update(|| state.sync_clock.set_playing(false));
            state.read_position = state.sync_clock.sample_position() as usize;
            tracing::debug!("playback paused");
        }
        AudioCommand::Seek(time_secs) => {
            state.transport.update(|| state.sync_clock.seek(*time_secs));
            let sample_pos = (*time_secs * state.output_rate as f64) as usize;
            let frame_pos = sample_pos; // output_rate is in frames/sec
            state.read_position = frame_pos;
            tracing::debug!(time = time_secs, frame = frame_pos, "seek");
        }
        AudioCommand::SetTrack { track, sample_rate } => {
            let generation = next_track_generation(&mut state.track_generations, track.id);
            if *sample_rate != state.output_rate {
                if state
                    .resample_queue
                    .submit(ResampleJob {
                        generation,
                        track: track.clone(),
                        input_rate: *sample_rate,
                        output_rate: state.output_rate,
                    })
                    .is_err()
                {
                    tracing::error!(track = track.id, "resample worker is unavailable");
                }
            } else {
                state.resample_queue.supersede(track.id, generation);
                state.mixer.set_track(track.clone());
                invalidate_prepared_audio(state);
                tracing::debug!(track = track.id, "track set");
            }
        }
        AudioCommand::RemoveTrack(id) => {
            let generation = next_track_generation(&mut state.track_generations, *id);
            state.resample_queue.supersede(*id, generation);
            state.mixer.remove_track(*id);
            invalidate_prepared_audio(state);
            tracing::debug!(track = id, "track removed");
        }
        AudioCommand::SetTrackGain { id, gain } => {
            if let Some(t) = state.mixer.track_mut(*id) {
                t.gain = gain.clone();
                invalidate_prepared_audio(state);
            }
        }
        AudioCommand::SetTrackMute { id, muted } => {
            if let Some(t) = state.mixer.track_mut(*id) {
                t.muted = *muted;
                invalidate_prepared_audio(state);
            }
        }
        AudioCommand::SetTrackSolo { id, solo } => {
            if let Some(t) = state.mixer.track_mut(*id) {
                t.solo = *solo;
                invalidate_prepared_audio(state);
            }
        }
        AudioCommand::SetTrackFadeIn { id, frames } => {
            if let Some(t) = state.mixer.track_mut(*id) {
                t.fade_in_frames = *frames;
                invalidate_prepared_audio(state);
            }
        }
        AudioCommand::SetTrackFadeOut { id, frames } => {
            if let Some(t) = state.mixer.track_mut(*id) {
                t.fade_out_frames = *frames;
                invalidate_prepared_audio(state);
            }
        }
        AudioCommand::SetMasterGain(gain) => {
            state.mixer.set_master_gain(*gain);
            invalidate_prepared_audio(state);
        }
        AudioCommand::Shutdown => {
            tracing::debug!("shutdown requested");
            return false;
        }
    }
    true
}

fn next_track_generation(generations: &mut HashMap<TrackId, u64>, id: TrackId) -> u64 {
    let generation = generations.entry(id).or_default();
    *generation = generation.wrapping_add(1);
    *generation
}

fn invalidate_prepared_audio(state: &mut PrepState) {
    state.transport.update(|| {});
    state.read_position = state.sync_clock.sample_position() as usize;
}

fn resample_worker_main(
    queue: Arc<ResampleQueue>,
    wake_rx: Receiver<()>,
    prepared_tx: Sender<PreparedTrack>,
) {
    while wake_rx.recv().is_ok() {
        if queue.shutdown.load(Ordering::Acquire) {
            return;
        }
        while let Some(job) = queue.take_pending() {
            let track_id = job.track.id;
            let generation = job.generation;
            let result = resampler::resample_buffer_cancellable(
                &job.track.samples,
                job.input_rate,
                job.output_rate,
                job.track.channels as usize,
                || !queue.is_current(track_id, generation),
            );
            let Ok(Some(samples)) = result else {
                if let Err(error) = result
                    && queue.is_current(track_id, generation)
                {
                    let _ = prepared_tx.send(PreparedTrack {
                        generation,
                        track_id,
                        result: Err(error),
                    });
                }
                continue;
            };
            if !queue.is_current(track_id, generation) {
                continue;
            }
            let mut track = job.track;
            track.samples = Arc::from(samples);
            if prepared_tx
                .send(PreparedTrack {
                    generation,
                    track_id,
                    result: Ok(track),
                })
                .is_err()
            {
                return;
            }
        }
    }
}

fn apply_prepared_track(prepared: PreparedTrack, state: &mut PrepState) {
    if state.track_generations.get(&prepared.track_id).copied() != Some(prepared.generation) {
        return;
    }
    match prepared.result {
        Ok(track) => {
            state.mixer.set_track(track);
            invalidate_prepared_audio(state);
            tracing::debug!(track = prepared.track_id, "resampled track set");
        }
        Err(error) => {
            tracing::error!(track = prepared.track_id, "resampling failed: {error}");
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_prep_state() -> PrepState {
        let (wake_tx, _wake_rx) = bounded(1);
        PrepState {
            mixer: Mixer::new(MixerConfig {
                output_sample_rate: 48_000,
                output_channels: 2,
            }),
            sync_clock: SyncClock::new(48_000, FrameRate::new(30, 1)),
            transport: TransportSync::new(),
            resample_queue: ResampleQueue::new(wake_tx),
            track_generations: HashMap::new(),
            output_rate: 48_000,
            read_position: 0,
        }
    }

    #[test]
    fn set_and_remove_track_commands_are_idempotent() {
        let mut state = test_prep_state();

        for sample in [0.25, 0.75] {
            let mut track = Track::new(5, Arc::from(vec![sample, sample]), 2);
            track.start_frame = 12;
            assert!(handle_command(
                &AudioCommand::SetTrack {
                    track,
                    sample_rate: 48_000,
                },
                &mut state,
            ));
        }

        assert_eq!(state.mixer.track_count(), 1);
        assert_eq!(
            state
                .mixer
                .track(5)
                .expect("track should exist")
                .start_frame,
            12
        );
        assert_eq!(state.mixer.mix(12, 1), vec![0.75, 0.75]);

        for _ in 0..2 {
            assert!(handle_command(&AudioCommand::RemoveTrack(5), &mut state));
        }
        assert_eq!(state.mixer.track_count(), 0);
    }

    #[test]
    fn pause_and_seek_invalidate_prepared_audio_and_reset_mix_position() {
        let mut state = test_prep_state();
        for _ in 0..9 {
            state.transport.update(|| {});
        }
        state.read_position = 8_192;
        state.sync_clock.seek_to_sample(2_048);
        state.sync_clock.set_playing(true);

        assert!(handle_command(&AudioCommand::Pause, &mut state));
        assert_eq!(state.transport.epoch(), 10);
        assert_eq!(state.read_position, 2_048);
        assert!(!state.sync_clock.is_playing());

        assert!(handle_command(&AudioCommand::Seek(0.25), &mut state));
        assert_eq!(state.transport.epoch(), 11);
        assert_eq!(state.sync_clock.sample_position(), 12_000);
        assert_eq!(state.read_position, 12_000);
    }

    #[test]
    fn stale_resample_result_cannot_replace_a_newer_track() {
        let mut state = test_prep_state();
        state.track_generations.insert(7, 2);

        let stale = PreparedTrack {
            generation: 1,
            track_id: 7,
            result: Ok(Track::new(7, Arc::from([0.25]), 1)),
        };
        apply_prepared_track(stale, &mut state);
        assert!(state.mixer.track(7).is_none());

        let current = PreparedTrack {
            generation: 2,
            track_id: 7,
            result: Ok(Track::new(7, Arc::from([0.75]), 1)),
        };
        apply_prepared_track(current, &mut state);
        assert_eq!(&*state.mixer.track(7).unwrap().samples, &[0.75]);
        assert_eq!(state.transport.epoch(), 1);
    }

    #[test]
    fn mixer_parameter_changes_invalidate_queued_audio() {
        let mut state = test_prep_state();
        state
            .mixer
            .set_track(Track::new(7, Arc::from([0.25, 0.25]), 2));

        let commands = [
            AudioCommand::SetTrackGain {
                id: 7,
                gain: TrackGain::Constant(0.5),
            },
            AudioCommand::SetTrackMute { id: 7, muted: true },
            AudioCommand::SetTrackSolo { id: 7, solo: true },
            AudioCommand::SetTrackFadeIn { id: 7, frames: 4 },
            AudioCommand::SetTrackFadeOut { id: 7, frames: 4 },
            AudioCommand::SetMasterGain(0.75),
        ];

        for (expected_epoch, command) in (1_u64..).zip(commands) {
            assert!(handle_command(&command, &mut state));
            assert_eq!(state.transport.epoch(), expected_epoch);
        }
    }

    #[test]
    fn different_rate_track_is_queued_without_blocking_prep() {
        let mut state = test_prep_state();
        let track = Track::new(11, Arc::from(vec![0.5; 44_100]), 1);

        assert!(handle_command(
            &AudioCommand::SetTrack {
                track,
                sample_rate: 44_100,
            },
            &mut state,
        ));

        assert!(state.mixer.track(11).is_none());
        let job = state
            .resample_queue
            .take_pending()
            .expect("resample job should be queued");
        assert_eq!(job.track.id, 11);
        assert_eq!(job.input_rate, 44_100);
        assert_eq!(job.output_rate, 48_000);
    }

    #[test]
    fn resample_queue_coalesces_each_track_and_supersedes_active_work() {
        let (wake_tx, _wake_rx) = bounded(1);
        let queue = ResampleQueue::new(wake_tx);
        for generation in 1..=3 {
            queue
                .submit(ResampleJob {
                    generation,
                    track: Track::new(11, Arc::from([generation as f32]), 1),
                    input_rate: 44_100,
                    output_rate: 48_000,
                })
                .unwrap();
        }

        assert!(!queue.is_current(11, 1));
        assert!(queue.is_current(11, 3));
        let job = queue.take_pending().unwrap();
        assert_eq!(job.generation, 3);
        assert!(queue.take_pending().is_none());
    }

    #[test]
    fn default_engine_config_defers_output_selection_to_the_device() {
        assert!(AudioEngineConfig::default().output.is_none());
    }
}

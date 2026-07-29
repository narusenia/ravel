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
//! Audio Prep Thread   ← runs the mixer, resampler, and effects
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
use crate::sync::SyncClock;
use crossbeam_channel::{Receiver, Sender, bounded};
use ravel_core::types::FrameRate;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Audio output configuration (sample rate, channels, buffer size).
    pub output: OutputConfig,
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
            output: OutputConfig::default(),
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
    /// Keep the CPAL stream alive. Dropping this stops playback.
    _stream: cpal::Stream,
    /// Handle to the prep thread (joined on shutdown).
    prep_handle: Option<thread::JoinHandle<()>>,
}

impl AudioEngine {
    /// Create and start a new audio engine.
    ///
    /// This opens the default audio output device, spawns the prep thread,
    /// and begins sending silence to the output.  Call
    /// [`AudioEngine::send`]`(AudioCommand::Play)` to start playback.
    pub fn new(config: AudioEngineConfig) -> Result<Self, AudioError> {
        let device = device::default_output_device()?;
        let sync_clock = SyncClock::new(config.output.sample_rate, config.fps);
        let transport_epoch = Arc::new(AtomicU64::new(0));

        // Channel: prep thread → CPAL callback (audio chunks).
        let (chunk_tx, chunk_rx) = bounded::<AudioChunk>(config.queue_depth);

        // Channel: UI → prep thread (commands).
        let (command_tx, command_rx) = bounded::<AudioCommand>(64);

        // Build CPAL stream.
        let stream = device::build_output_stream(
            &device,
            &config.output,
            chunk_rx,
            sync_clock.clone(),
            transport_epoch.clone(),
        )?;

        // Spawn the audio prep thread.
        let prep_clock = sync_clock.clone();
        let output_rate = config.output.sample_rate;
        let output_channels = config.output.channels as u32;
        let chunk_frames = config.chunk_frames;

        let prep_handle = thread::Builder::new()
            .name("ravel-audio-prep".into())
            .spawn(move || {
                prep_thread_main(
                    command_rx,
                    chunk_tx,
                    prep_clock,
                    transport_epoch,
                    output_rate,
                    output_channels,
                    chunk_frames,
                );
            })
            .map_err(|e| AudioError::Other(format!("failed to spawn prep thread: {e}")))?;

        tracing::info!(
            sample_rate = output_rate,
            channels = config.output.channels,
            "audio engine started"
        );

        Ok(Self {
            command_tx,
            sync_clock,
            _stream: stream,
            prep_handle: Some(prep_handle),
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

    /// Shut down the audio engine, stopping playback and joining the prep
    /// thread.
    pub fn shutdown(mut self) {
        let _ = self.command_tx.send(AudioCommand::Shutdown);
        if let Some(handle) = self.prep_handle.take() {
            let _ = handle.join();
        }
        tracing::info!("audio engine shut down");
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Best-effort shutdown if not already done.
        let _ = self.command_tx.send(AudioCommand::Shutdown);
        if let Some(handle) = self.prep_handle.take() {
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
    sync_clock: Arc<SyncClock>,
    transport_epoch: Arc<AtomicU64>,
    output_rate: u32,
    output_channels: u32,
    chunk_frames: usize,
) {
    let mut mixer = Mixer::new(MixerConfig {
        output_sample_rate: output_rate,
        output_channels,
    });

    // Track the read position independently (reset on seek).
    let mut read_position: usize = 0;

    loop {
        // Drain all pending commands before mixing. This is the only point
        // where playback-time track state changes, so every update becomes
        // visible at the next complete prepared-block boundary.
        loop {
            match command_rx.try_recv() {
                Ok(cmd) => {
                    if !handle_command(
                        &cmd,
                        &mut mixer,
                        &sync_clock,
                        &transport_epoch,
                        output_rate,
                        output_channels,
                        &mut read_position,
                    ) {
                        return; // Shutdown
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        }

        if !sync_clock.is_playing() {
            // When paused, wait for a command instead of busy-spinning.
            match command_rx.recv() {
                Ok(cmd) => {
                    if !handle_command(
                        &cmd,
                        &mut mixer,
                        &sync_clock,
                        &transport_epoch,
                        output_rate,
                        output_channels,
                        &mut read_position,
                    ) {
                        return;
                    }
                }
                Err(_) => return,
            }
            continue;
        }

        // Mix the next chunk.
        let chunk = AudioChunk {
            epoch: transport_epoch.load(Ordering::Acquire),
            samples: mixer.mix(read_position, chunk_frames).into(),
        };

        // A full queue must not delay Pause or Seek. Only advance the mix
        // position after the block is accepted by the callback queue.
        crossbeam_channel::select_biased! {
            recv(command_rx) -> message => match message {
                Ok(cmd) => {
                    if !handle_command(
                        &cmd,
                        &mut mixer,
                        &sync_clock,
                        &transport_epoch,
                        output_rate,
                        output_channels,
                        &mut read_position,
                    ) {
                        return;
                    }
                }
                Err(_) => return,
            },
            send(chunk_tx, chunk) -> result => {
                if result.is_err() {
                    tracing::warn!("audio chunk channel disconnected");
                    return;
                }
                read_position += chunk_frames;
            }
        }
    }
}

/// Process a single command. Returns `false` on [`AudioCommand::Shutdown`].
fn handle_command(
    cmd: &AudioCommand,
    mixer: &mut Mixer,
    sync_clock: &SyncClock,
    transport_epoch: &AtomicU64,
    output_rate: u32,
    output_channels: u32,
    read_position: &mut usize,
) -> bool {
    match cmd {
        AudioCommand::Play => {
            sync_clock.set_playing(true);
            tracing::debug!("playback started");
        }
        AudioCommand::Pause => {
            transport_epoch.fetch_add(1, Ordering::AcqRel);
            sync_clock.set_playing(false);
            *read_position = sync_clock.sample_position() as usize;
            tracing::debug!("playback paused");
        }
        AudioCommand::Seek(time_secs) => {
            transport_epoch.fetch_add(1, Ordering::AcqRel);
            sync_clock.seek(*time_secs);
            let sample_pos = (*time_secs * output_rate as f64) as usize;
            let frame_pos = sample_pos; // output_rate is in frames/sec
            *read_position = frame_pos;
            tracing::debug!(time = time_secs, frame = frame_pos, "seek");
        }
        AudioCommand::SetTrack { track, sample_rate } => {
            // Resample if needed.
            let resampled = if *sample_rate != output_rate {
                match resampler::resample_buffer(
                    &track.samples,
                    *sample_rate,
                    output_rate,
                    track.channels as usize,
                ) {
                    Ok(data) => Arc::from(data),
                    Err(e) => {
                        tracing::error!(track = track.id, "resampling failed: {e}");
                        return true;
                    }
                }
            } else {
                track.samples.clone()
            };

            // Channel conversion if needed (mono→stereo for matching output).
            let final_channels = if track.channels == 1 && output_channels > 1 {
                // Keep as mono — the mixer handles mono-to-stereo upmix.
                1
            } else {
                track.channels
            };

            let mut prepared = track.clone();
            prepared.samples = resampled;
            prepared.channels = final_channels;
            mixer.set_track(prepared);
            tracing::debug!(track = track.id, "track set");
        }
        AudioCommand::RemoveTrack(id) => {
            mixer.remove_track(*id);
            tracing::debug!(track = id, "track removed");
        }
        AudioCommand::SetTrackGain { id, gain } => {
            if let Some(t) = mixer.track_mut(*id) {
                t.gain = gain.clone();
            }
        }
        AudioCommand::SetTrackMute { id, muted } => {
            if let Some(t) = mixer.track_mut(*id) {
                t.muted = *muted;
            }
        }
        AudioCommand::SetTrackSolo { id, solo } => {
            if let Some(t) = mixer.track_mut(*id) {
                t.solo = *solo;
            }
        }
        AudioCommand::SetTrackFadeIn { id, frames } => {
            if let Some(t) = mixer.track_mut(*id) {
                t.fade_in_frames = *frames;
            }
        }
        AudioCommand::SetTrackFadeOut { id, frames } => {
            if let Some(t) = mixer.track_mut(*id) {
                t.fade_out_frames = *frames;
            }
        }
        AudioCommand::SetMasterGain(gain) => {
            mixer.set_master_gain(*gain);
        }
        AudioCommand::Shutdown => {
            tracing::debug!("shutdown requested");
            return false;
        }
    }
    true
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_clock() -> Arc<SyncClock> {
        SyncClock::new(48_000, FrameRate::new(30, 1))
    }

    #[test]
    fn set_and_remove_track_commands_are_idempotent() {
        let mut mixer = Mixer::new(MixerConfig {
            output_sample_rate: 48_000,
            output_channels: 2,
        });
        let clock = test_clock();
        let mut read_position = 0;

        for sample in [0.25, 0.75] {
            let mut track = Track::new(5, Arc::from(vec![sample, sample]), 2);
            track.start_frame = 12;
            assert!(handle_command(
                &AudioCommand::SetTrack {
                    track,
                    sample_rate: 48_000,
                },
                &mut mixer,
                &clock,
                &AtomicU64::new(0),
                48_000,
                2,
                &mut read_position,
            ));
        }

        assert_eq!(mixer.track_count(), 1);
        assert_eq!(mixer.track(5).expect("track should exist").start_frame, 12);
        assert_eq!(mixer.mix(12, 1), vec![0.75, 0.75]);

        for _ in 0..2 {
            assert!(handle_command(
                &AudioCommand::RemoveTrack(5),
                &mut mixer,
                &clock,
                &AtomicU64::new(0),
                48_000,
                2,
                &mut read_position,
            ));
        }
        assert_eq!(mixer.track_count(), 0);
    }

    #[test]
    fn pause_and_seek_invalidate_prepared_audio_and_reset_mix_position() {
        let mut mixer = Mixer::new(MixerConfig {
            output_sample_rate: 48_000,
            output_channels: 2,
        });
        let clock = test_clock();
        let epoch = AtomicU64::new(9);
        let mut read_position = 8_192;
        clock.seek_to_sample(2_048);
        clock.set_playing(true);

        assert!(handle_command(
            &AudioCommand::Pause,
            &mut mixer,
            &clock,
            &epoch,
            48_000,
            2,
            &mut read_position,
        ));
        assert_eq!(epoch.load(Ordering::Acquire), 10);
        assert_eq!(read_position, 2_048);
        assert!(!clock.is_playing());

        assert!(handle_command(
            &AudioCommand::Seek(0.25),
            &mut mixer,
            &clock,
            &epoch,
            48_000,
            2,
            &mut read_position,
        ));
        assert_eq!(epoch.load(Ordering::Acquire), 11);
        assert_eq!(clock.sample_position(), 12_000);
        assert_eq!(read_position, 12_000);
    }
}

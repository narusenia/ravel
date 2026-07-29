// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CPAL audio device management.
//!
//! Handles device discovery and output stream creation.  The CPAL callback
//! runs on a high-priority audio thread managed by the platform's audio
//! subsystem — we do **not** create this thread ourselves.
//!
//! The callback reads pre-mixed audio from a crossbeam channel written by
//! the audio prep thread (see [`crate::engine`]).

use crate::error::AudioError;
use crate::sync::SyncClock;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleRate, Stream, StreamConfig};
use crossbeam_channel::Receiver;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A prepared interleaved block tagged with the transport epoch that produced it.
pub(crate) struct AudioChunk {
    pub(crate) epoch: u64,
    pub(crate) samples: Arc<[f32]>,
}

struct CallbackState {
    current_chunk: Option<AudioChunk>,
    chunk_pos: usize,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            current_chunk: None,
            chunk_pos: 0,
        }
    }

    /// Fill one device buffer and return the number of samples sourced from
    /// current-epoch chunks. Silence inserted for pause or underrun is not counted.
    fn fill(
        &mut self,
        data: &mut [f32],
        chunk_rx: &Receiver<AudioChunk>,
        active_epoch: u64,
        playing: bool,
    ) -> usize {
        data.fill(0.0);
        if !playing {
            return 0;
        }

        let mut written = 0;
        while written < data.len() {
            if let Some(chunk) = self.current_chunk.as_ref() {
                if chunk.epoch < active_epoch {
                    self.current_chunk = None;
                    self.chunk_pos = 0;
                    continue;
                }
                if chunk.epoch > active_epoch {
                    break;
                }

                let remaining = chunk.samples.len() - self.chunk_pos;
                let to_copy = remaining.min(data.len() - written);
                data[written..written + to_copy]
                    .copy_from_slice(&chunk.samples[self.chunk_pos..self.chunk_pos + to_copy]);
                written += to_copy;
                self.chunk_pos += to_copy;

                if self.chunk_pos >= chunk.samples.len() {
                    self.current_chunk = None;
                    self.chunk_pos = 0;
                }
                continue;
            }

            match chunk_rx.try_recv() {
                Ok(chunk) => {
                    self.current_chunk = Some(chunk);
                    self.chunk_pos = 0;
                }
                Err(_) => break,
            }
        }

        written
    }
}

/// Configuration for the audio output stream.
#[derive(Clone, Debug)]
pub struct OutputConfig {
    /// Desired sample rate in Hz (e.g. 48 000).
    pub sample_rate: u32,
    /// Number of output channels (typically 2 for stereo).
    pub channels: u16,
    /// Buffer size hint in frames. `None` lets CPAL choose.
    pub buffer_size: Option<u32>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            buffer_size: None,
        }
    }
}

/// Discover the default audio output device.
pub fn default_output_device() -> Result<Device, AudioError> {
    let host = cpal::default_host();
    host.default_output_device()
        .ok_or(AudioError::DeviceNotFound)
}

/// Query the device's default output configuration.
pub fn default_device_config(device: &Device) -> Result<StreamConfig, AudioError> {
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::DefaultConfig(e.to_string()))?;
    Ok(supported.into())
}

/// Build and start a CPAL output stream.
///
/// The callback reads mixed audio chunks from `chunk_rx`.  Each received
/// `Arc<[f32]>` is a block of interleaved samples at the stream's sample
/// rate and channel count.
///
/// When no data is available the callback writes silence (zero-fill) —
/// this is an *underrun* but keeps the stream alive.
///
/// The returned [`Stream`] must be kept alive; dropping it stops playback.
pub(crate) fn build_output_stream(
    device: &Device,
    config: &OutputConfig,
    chunk_rx: Receiver<AudioChunk>,
    sync_clock: Arc<SyncClock>,
    transport_epoch: Arc<AtomicU64>,
) -> Result<Stream, AudioError> {
    let stream_config = StreamConfig {
        channels: config.channels,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: match config.buffer_size {
            Some(size) => cpal::BufferSize::Fixed(size),
            None => cpal::BufferSize::Default,
        },
    };

    let channels = config.channels as usize;

    // State carried across callback invocations.
    let mut callback_state = CallbackState::new();

    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                let epoch = transport_epoch.load(Ordering::Acquire);
                let sourced_samples =
                    callback_state.fill(data, &chunk_rx, epoch, sync_clock.is_playing());

                // A seek or pause may race this callback. Never emit or account
                // samples prepared before that boundary.
                if transport_epoch.load(Ordering::Acquire) != epoch || !sync_clock.is_playing() {
                    data.fill(0.0);
                } else {
                    let frames = sourced_samples / channels.max(1);
                    sync_clock.advance(frames as u64);
                }
            },
            |err| {
                tracing::error!("audio stream error: {err}");
            },
            None,
        )
        .map_err(|e| AudioError::StreamBuild(e.to_string()))?;

    stream
        .play()
        .map_err(|e| AudioError::StreamPlay(e.to_string()))?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    fn chunk(epoch: u64, samples: &[f32]) -> AudioChunk {
        AudioChunk {
            epoch,
            samples: Arc::from(samples),
        }
    }

    #[test]
    fn pause_outputs_silence_without_consuming_current_chunk() {
        let (tx, rx) = unbounded();
        tx.send(chunk(0, &[1.0, 2.0, 3.0, 4.0])).unwrap();
        let mut state = CallbackState::new();
        let mut first = [0.0; 2];
        assert_eq!(state.fill(&mut first, &rx, 0, true), 2);
        assert_eq!(first, [1.0, 2.0]);

        let mut paused = [9.0; 2];
        assert_eq!(state.fill(&mut paused, &rx, 0, false), 0);
        assert_eq!(paused, [0.0, 0.0]);

        let mut resumed = [0.0; 2];
        assert_eq!(state.fill(&mut resumed, &rx, 0, true), 2);
        assert_eq!(resumed, [3.0, 4.0]);
    }

    #[test]
    fn epoch_change_discards_current_and_queued_old_audio() {
        let (tx, rx) = unbounded();
        tx.send(chunk(4, &[1.0, 2.0, 3.0, 4.0])).unwrap();
        tx.send(chunk(4, &[5.0, 6.0])).unwrap();
        tx.send(chunk(5, &[7.0, 8.0])).unwrap();
        let mut state = CallbackState::new();

        let mut before_seek = [0.0; 2];
        state.fill(&mut before_seek, &rx, 4, true);
        assert_eq!(before_seek, [1.0, 2.0]);

        let mut after_seek = [0.0; 4];
        assert_eq!(state.fill(&mut after_seek, &rx, 5, true), 2);
        assert_eq!(after_seek, [7.0, 8.0, 0.0, 0.0]);
    }

    #[test]
    fn underrun_reports_only_samples_copied_from_chunks() {
        let (tx, rx) = unbounded();
        tx.send(chunk(0, &[0.25, -0.25])).unwrap();
        let mut state = CallbackState::new();
        let mut output = [1.0; 6];

        assert_eq!(state.fill(&mut output, &rx, 0, true), 2);
        assert_eq!(output, [0.25, -0.25, 0.0, 0.0, 0.0, 0.0]);
    }
}

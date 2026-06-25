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
pub fn build_output_stream(
    device: &Device,
    config: &OutputConfig,
    chunk_rx: Receiver<Arc<[f32]>>,
    sync_clock: Arc<SyncClock>,
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
    let mut current_chunk: Option<Arc<[f32]>> = None;
    let mut chunk_pos: usize = 0;

    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                let mut written = 0;

                while written < data.len() {
                    // Try to drain the current chunk first.
                    if let Some(ref chunk) = current_chunk {
                        let remaining = chunk.len() - chunk_pos;
                        let to_copy = remaining.min(data.len() - written);
                        data[written..written + to_copy]
                            .copy_from_slice(&chunk[chunk_pos..chunk_pos + to_copy]);
                        written += to_copy;
                        chunk_pos += to_copy;

                        if chunk_pos >= chunk.len() {
                            current_chunk = None;
                            chunk_pos = 0;
                        }
                    } else {
                        // Fetch the next chunk (non-blocking).
                        match chunk_rx.try_recv() {
                            Ok(chunk) => {
                                current_chunk = Some(chunk);
                                chunk_pos = 0;
                            }
                            Err(_) => {
                                // Underrun: fill remainder with silence.
                                for s in &mut data[written..] {
                                    *s = 0.0;
                                }
                                written = data.len();
                            }
                        }
                    }
                }

                // Advance the sync clock by the number of frames written.
                let frames = data.len() / channels.max(1);
                sync_clock.advance(frames as u64);
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

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
use crate::sync::{SyncClock, TransportSync};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, FromSample, Sample, SampleFormat, SampleRate, SizedSample, Stream, StreamConfig,
};
use crossbeam_channel::Receiver;
use std::sync::Arc;

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
    fn fill<T>(
        &mut self,
        data: &mut [T],
        chunk_rx: &Receiver<AudioChunk>,
        active_epoch: u64,
        playing: bool,
    ) -> usize
    where
        T: Sample + FromSample<f32>,
    {
        data.fill(T::from_sample(0.0));
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
                for (destination, source) in data[written..written + to_copy]
                    .iter_mut()
                    .zip(&chunk.samples[self.chunk_pos..self.chunk_pos + to_copy])
                {
                    *destination = T::from_sample(*source);
                }
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
    /// Device sample representation used by the CPAL callback.
    pub sample_format: SampleFormat,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            buffer_size: None,
            sample_format: SampleFormat::F32,
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
pub fn default_device_config(device: &Device) -> Result<OutputConfig, AudioError> {
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::DefaultConfig(e.to_string()))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    Ok(OutputConfig {
        sample_rate: config.sample_rate.0,
        channels: config.channels,
        buffer_size: match config.buffer_size {
            cpal::BufferSize::Fixed(size) => Some(size),
            cpal::BufferSize::Default => None,
        },
        sample_format,
    })
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
    transport: Arc<TransportSync>,
) -> Result<Stream, AudioError> {
    let stream_config = StreamConfig {
        channels: config.channels,
        sample_rate: SampleRate(config.sample_rate),
        buffer_size: match config.buffer_size {
            Some(size) => cpal::BufferSize::Fixed(size),
            None => cpal::BufferSize::Default,
        },
    };

    match config.sample_format {
        SampleFormat::I8 => build_output_stream_for::<i8>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::I16 => build_output_stream_for::<i16>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::I32 => build_output_stream_for::<i32>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::I64 => build_output_stream_for::<i64>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::U8 => build_output_stream_for::<u8>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::U16 => build_output_stream_for::<u16>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::U32 => build_output_stream_for::<u32>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::U64 => build_output_stream_for::<u64>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::F32 => build_output_stream_for::<f32>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        SampleFormat::F64 => build_output_stream_for::<f64>(
            device,
            &stream_config,
            config.channels,
            chunk_rx,
            sync_clock,
            transport,
        ),
        format => Err(AudioError::StreamBuild(format!(
            "unsupported device sample format {format}"
        ))),
    }
}

fn build_output_stream_for<T>(
    device: &Device,
    stream_config: &StreamConfig,
    channel_count: u16,
    chunk_rx: Receiver<AudioChunk>,
    sync_clock: Arc<SyncClock>,
    transport: Arc<TransportSync>,
) -> Result<Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = channel_count as usize;
    let mut callback_state = CallbackState::new();
    let stream = device
        .build_output_stream(
            stream_config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                let epoch = transport.epoch();
                let sourced_samples =
                    callback_state.fill(data, &chunk_rx, epoch, sync_clock.is_playing());

                // A seek or pause may race this callback. Never emit or account
                // samples prepared before that boundary.
                let frames = sourced_samples / channels.max(1);
                if !transport.try_commit_frames(&sync_clock, epoch, frames as u64) {
                    data.fill(T::from_sample(0.0));
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

    #[test]
    fn callback_converts_float_chunks_for_integer_devices() {
        let (tx, rx) = unbounded();
        tx.send(chunk(0, &[-1.0, 0.0, 1.0])).unwrap();
        let mut state = CallbackState::new();
        let mut output = [0_i16; 3];

        assert_eq!(state.fill(&mut output, &rx, 0, true), 3);
        assert_eq!(output, [i16::MIN, 0, i16::MAX]);
    }
}

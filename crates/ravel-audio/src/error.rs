// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for the audio engine.

use thiserror::Error;

/// Errors that can occur in the audio engine.
#[derive(Debug, Error)]
pub enum AudioError {
    /// No audio output device was found on the system.
    #[error("no audio output device found")]
    DeviceNotFound,

    /// Failed to query the default output configuration.
    #[error("failed to get default output config: {0}")]
    DefaultConfig(String),

    /// Failed to build the audio output stream.
    #[error("failed to build audio stream: {0}")]
    StreamBuild(String),

    /// Failed to start playback on the audio stream.
    #[error("failed to play audio stream: {0}")]
    StreamPlay(String),

    /// Failed to pause the audio stream.
    #[error("failed to pause audio stream: {0}")]
    StreamPause(String),

    /// An error occurred during sample-rate conversion.
    #[error("resampler error: {0}")]
    Resampler(String),

    /// The audio engine is not running.
    #[error("audio engine not running")]
    NotRunning,

    /// A track with the given ID was not found.
    #[error("track {0} not found")]
    TrackNotFound(u64),

    /// Generic error.
    #[error("{0}")]
    Other(String),
}

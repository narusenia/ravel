// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for the media crate.

use ravel_core::media::MediaError;
use thiserror::Error;

/// Internal error type for `ravel-media` that can wrap backend-specific
/// errors before converting to the core [`MediaError`].
#[derive(Debug, Error)]
pub enum InternalMediaError {
    #[error(transparent)]
    Media(#[from] MediaError),

    #[cfg(feature = "ffmpeg")]
    #[error("FFmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_the_third::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl From<InternalMediaError> for MediaError {
    fn from(e: InternalMediaError) -> Self {
        match e {
            InternalMediaError::Media(m) => m,
            #[cfg(feature = "ffmpeg")]
            InternalMediaError::Ffmpeg(e) => MediaError::Other(e.to_string()),
            InternalMediaError::Io(e) => MediaError::Io(e),
            InternalMediaError::Other(s) => MediaError::Other(s),
        }
    }
}

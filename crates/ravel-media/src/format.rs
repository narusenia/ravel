// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Format auto-detection combining extension heuristics and FFmpeg probing.
//!
//! For quick checks, use [`ravel_core::media::detect_format`] (pure Rust,
//! extension-only).  For robust detection with codec identification, use
//! [`probe`] which delegates to FFmpeg's `avformat_open_input` +
//! `avformat_find_stream_info`.

use std::path::Path;

use ravel_core::media::{DetectedFormat, MediaInfo, MediaResult};

/// Probe a media file with FFmpeg and return full [`MediaInfo`].
///
/// This is the most reliable way to detect format, codec, resolution,
/// and other metadata.  Falls back to the heuristic extension check if
/// FFmpeg cannot open the file.
#[cfg(feature = "ffmpeg")]
pub fn probe(path: &Path) -> MediaResult<MediaInfo> {
    crate::decoder::FfmpegDecoder::probe(path)
}

/// Quick format detection from file extension only (no I/O).
///
/// Delegates to [`ravel_core::media::detect_format`].
pub fn detect(path: &Path) -> DetectedFormat {
    ravel_core::media::detect_format(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::media::ContainerFormat;

    #[test]
    fn detect_from_extension() {
        assert_eq!(
            detect(Path::new("video.mp4")),
            DetectedFormat::Container(ContainerFormat::Mp4),
        );
    }

    #[test]
    fn detect_unknown_extension() {
        assert_eq!(detect(Path::new("file.xyz")), DetectedFormat::Unknown);
    }
}

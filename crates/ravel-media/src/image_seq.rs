// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Image sequence detection and reading.
//!
//! Detects numbered image sequences on disk (e.g. `render_0001.exr` …
//! `render_0120.exr`) and provides frame-by-frame access via FFmpeg's
//! single-image decoders (EXR, PNG, TIFF, DPX).

use std::collections::BTreeSet;
use std::path::Path;

use ravel_core::media::{ImageFormat, ImageSequenceInfo, MediaError, MediaResult};

/// Detect an image sequence from a single file path.
///
/// Given a path like `/renders/shot_0042.exr`, this function:
/// 1. Identifies the numeric portion of the filename
/// 2. Scans the directory for files matching the same pattern
/// 3. Returns an [`ImageSequenceInfo`] describing the sequence
///
/// Returns `Err` if the file has no numeric portion or the directory
/// cannot be read.
pub fn detect_sequence(path: &Path) -> MediaResult<ImageSequenceInfo> {
    let dir = path
        .parent()
        .ok_or_else(|| MediaError::Other("path has no parent directory".into()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| MediaError::Other("path has no file stem".into()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| MediaError::Other("path has no extension".into()))?;

    let format = ImageFormat::from_extension(ext)
        .ok_or_else(|| MediaError::Other(format!("unsupported image extension: {ext}")))?;

    // Find the numeric portion: scan from the end of the stem.
    let (prefix, number_str, suffix) = split_numeric(stem).ok_or_else(|| {
        MediaError::Other(format!("no numeric portion found in filename: {stem}"))
    })?;

    let padding = number_str.len();

    // Scan directory for matching files.
    let frames = scan_directory(dir, prefix, suffix, ext)?;

    if frames.is_empty() {
        return Err(MediaError::Other(
            "no matching frames found in directory".into(),
        ));
    }

    let start_frame = *frames.first().unwrap();
    let end_frame = *frames.last().unwrap();

    Ok(ImageSequenceInfo {
        directory: dir.to_path_buf(),
        prefix: prefix.to_string(),
        suffix: suffix.to_string(),
        format,
        start_frame,
        end_frame,
        padding,
    })
}

/// Read a single image frame from disk via FFmpeg.
///
/// Returns an RGBA f32 [`FrameBuffer`].
#[cfg(feature = "ffmpeg")]
pub fn read_image_frame(path: &Path) -> MediaResult<ravel_core::types::FrameBuffer> {
    use crate::decoder::FfmpegDecoder;
    use ravel_core::media::MediaReader;

    // Open the single image as a "video" with one frame.
    let mut reader = FfmpegDecoder::open(path)?;
    let video_info = reader
        .info()
        .first_video()
        .ok_or(MediaError::NoStreamFound)?;
    let stream_index = video_info.stream_index;
    reader.decode_video_frame(stream_index, 0)
}

// ===========================================================================
// Internal helpers
// ===========================================================================

/// Split a filename stem into `(prefix, number, suffix)`.
///
/// Finds the last contiguous run of ASCII digits in the stem and returns
/// byte-correct slices.  Safe for multi-byte UTF-8 filenames because we
/// use `char_indices()` which yields byte positions.
///
/// Example: `"shot_0042_final"` → `("shot_", "0042", "_final")`.
fn split_numeric(stem: &str) -> Option<(&str, &str, &str)> {
    // Collect (byte_offset, char) pairs.
    let indices: Vec<(usize, char)> = stem.char_indices().collect();

    // Scan backwards to find the last contiguous digit run.
    let mut end_byte: Option<usize> = None; // exclusive byte end
    let mut start_byte: Option<usize> = None;

    for &(byte_pos, ch) in indices.iter().rev() {
        if ch.is_ascii_digit() {
            if end_byte.is_none() {
                // `byte_pos` is the start of this char; end is after it.
                end_byte = Some(byte_pos + ch.len_utf8());
            }
            start_byte = Some(byte_pos);
        } else if end_byte.is_some() {
            break;
        }
    }

    let start = start_byte?;
    let end = end_byte?;

    let prefix = &stem[..start];
    let number = &stem[start..end];
    let suffix = &stem[end..];

    Some((prefix, number, suffix))
}

/// Scan a directory for files matching the pattern `{prefix}{digits}{suffix}.{ext}`.
fn scan_directory(dir: &Path, prefix: &str, suffix: &str, ext: &str) -> MediaResult<BTreeSet<u64>> {
    let mut frames = BTreeSet::new();

    let entries = std::fs::read_dir(dir).map_err(MediaError::Io)?;

    for entry in entries {
        let entry = entry.map_err(MediaError::Io)?;
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(n) => n,
            None => continue,
        };

        // Check extension.
        let expected_ext = format!(".{ext}");
        if !name.ends_with(&expected_ext) {
            continue;
        }
        let name_no_ext = &name[..name.len() - expected_ext.len()];

        // Check prefix and suffix.
        if !name_no_ext.starts_with(prefix) {
            continue;
        }
        let after_prefix = &name_no_ext[prefix.len()..];

        if !after_prefix.ends_with(suffix) {
            continue;
        }
        let number_part = &after_prefix[..after_prefix.len() - suffix.len()];

        // Parse frame number.
        if let Ok(n) = number_part.parse::<u64>() {
            frames.insert(n);
        }
    }

    Ok(frames)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_numeric_basic() {
        let (prefix, num, suffix) = split_numeric("render_0042").unwrap();
        assert_eq!(prefix, "render_");
        assert_eq!(num, "0042");
        assert_eq!(suffix, "");
    }

    #[test]
    fn split_numeric_with_suffix() {
        let (prefix, num, suffix) = split_numeric("shot_0042_final").unwrap();
        assert_eq!(prefix, "shot_");
        assert_eq!(num, "0042");
        assert_eq!(suffix, "_final");
    }

    #[test]
    fn split_numeric_no_prefix() {
        let (prefix, num, suffix) = split_numeric("0001").unwrap();
        assert_eq!(prefix, "");
        assert_eq!(num, "0001");
        assert_eq!(suffix, "");
    }

    #[test]
    fn split_numeric_no_digits() {
        assert!(split_numeric("nodigs").is_none());
    }

    #[test]
    fn split_numeric_multibyte_prefix() {
        // CJK prefix — char indices differ from byte indices.
        let (prefix, num, suffix) = split_numeric("素材_0042").unwrap();
        assert_eq!(prefix, "素材_");
        assert_eq!(num, "0042");
        assert_eq!(suffix, "");
    }

    #[test]
    fn detect_sequence_from_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Create numbered files.
        for i in 1..=10 {
            let name = format!("frame_{:04}.png", i);
            std::fs::write(dir.path().join(&name), b"fake").unwrap();
        }

        let sample = dir.path().join("frame_0005.png");
        let info = detect_sequence(&sample).unwrap();
        assert_eq!(info.prefix, "frame_");
        assert_eq!(info.suffix, "");
        assert_eq!(info.format, ImageFormat::Png);
        assert_eq!(info.start_frame, 1);
        assert_eq!(info.end_frame, 10);
        assert_eq!(info.padding, 4);
        assert_eq!(info.frame_count(), 10);
    }

    #[test]
    fn detect_sequence_with_gaps() {
        let dir = tempfile::tempdir().unwrap();
        for i in [1, 3, 5, 7] {
            let name = format!("img{:03}.exr", i);
            std::fs::write(dir.path().join(&name), b"fake").unwrap();
        }

        let sample = dir.path().join("img003.exr");
        let info = detect_sequence(&sample).unwrap();
        assert_eq!(info.start_frame, 1);
        assert_eq!(info.end_frame, 7);
        assert_eq!(info.format, ImageFormat::Exr);
    }

    #[test]
    fn detect_sequence_no_digits_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("frame.png"), b"fake").unwrap();
        assert!(detect_sequence(&dir.path().join("frame.png")).is_err());
    }
}

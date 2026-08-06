// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PNG and EXR image-sequence writing, without FFmpeg.
//!
//! This is the render output that must work in every build and on every host,
//! so it deliberately depends on nothing but the `image` crate.
//!
//! # What the two formats are for
//!
//! - **EXR** carries the evaluator's buffer through unchanged: 32-bit float,
//!   linear, values outside `0.0..=1.0` intact. It is the lossless
//!   intermediate.
//! - **PNG** is the interchange format: 8-bit RGBA, so values are clamped to
//!   `0.0..=1.0` and quantised. Round-tripping a PNG returns the quantised
//!   values, not the originals.
//!
//! # Alpha and colour
//!
//! `FrameBuffer` holds **straight (unpremultiplied) alpha**
//! (`docs/specifications/data-model.md`), which is also what PNG specifies
//! and what Ravel writes into EXR, so neither format needs an alpha
//! conversion and both round-trip exactly in that respect.
//!
//! **No transfer function is applied.** Channel values are written as they
//! come out of the evaluator, matching what the FFmpeg encoder path already
//! does. A linear buffer therefore lands in a PNG that ordinary viewers will
//! interpret as sRGB and show dark. Choosing and applying an output transform
//! is colour management's job and belongs to the OCIO work, not here; until
//! then the caller owns the decision.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::openexr::OpenExrEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use ravel_core::media::encode::{Encoder, ImageSequenceOutput, remove_partial_output};
use ravel_core::media::{ImageFormat, MediaError, MediaResult};
use ravel_core::types::FrameBuffer;

/// Where the encoder is in its lifecycle.
///
/// The state exists to make cancellation safe: only `Active` owns files that
/// have to disappear, and only `Active` may be terminated implicitly by a
/// drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Constructed, `begin` not yet called.
    Ready,
    /// Between `begin` and a terminator; owns partial output.
    Active,
    /// `finish` ran; the files on disk are the deliverable.
    Finished,
    /// `abort` ran; nothing remains to clean up.
    Aborted,
}

/// Writes one numbered PNG or EXR sequence.
///
/// Every frame is written to a hidden temporary file in the destination
/// directory and renamed into place, so a frame that exists under its final
/// name is a complete frame — a crash mid-encode cannot leave a truncated
/// image that later reads as a valid short file.
///
/// Cancellation removes the whole sequence, not just the frame in flight:
/// half a render is not a useful artifact, and leaving one behind makes the
/// next run's output ambiguous. Dropping an encoder that was begun and never
/// terminated does the same thing, so a panicking render worker cleans up
/// too.
pub struct ImageSequenceEncoder {
    output: ImageSequenceOutput,
    state: State,
    /// Frames renamed into place, in write order.
    written: Vec<PathBuf>,
    /// Temporary file for the frame currently being written, if any.
    in_flight: Option<PathBuf>,
    /// Directories this encoder created, deepest first.
    created_dirs: Vec<PathBuf>,
}

impl ImageSequenceEncoder {
    /// Create an encoder for `output`.
    ///
    /// Rejects formats this writer cannot produce up front, so a bad export
    /// setting fails when the job is configured rather than on its first
    /// frame. Nothing touches the filesystem until [`Encoder::begin`].
    pub fn new(output: ImageSequenceOutput) -> MediaResult<Self> {
        match output.format {
            ImageFormat::Png | ImageFormat::Exr => {}
            other => {
                return Err(MediaError::UnsupportedCodec(format!(
                    "image sequence encoder writes png and exr, not {other}"
                )));
            }
        }
        Ok(Self {
            output,
            state: State::Ready,
            written: Vec::new(),
            in_flight: None,
            created_dirs: Vec::new(),
        })
    }

    /// The frames written so far, in order.
    pub fn written_frames(&self) -> &[PathBuf] {
        &self.written
    }

    /// Create the destination directory, remembering what we made so that a
    /// cancelled job does not leave an empty tree behind.
    fn create_destination(&mut self) -> MediaResult<()> {
        let mut missing = Vec::new();
        let mut cursor: Option<&Path> = Some(&self.output.directory);
        while let Some(dir) = cursor {
            if dir.exists() {
                break;
            }
            missing.push(dir.to_path_buf());
            cursor = dir.parent();
        }
        std::fs::create_dir_all(&self.output.directory)?;
        // Deepest first: that is also the order they must be removed in.
        self.created_dirs = missing;
        Ok(())
    }

    /// Encode `frame` into the bytes of a complete image file.
    fn encode(&self, frame: &FrameBuffer) -> MediaResult<Vec<u8>> {
        if frame.width == 0 || frame.height == 0 {
            return Err(MediaError::EncodeError(format!(
                "cannot encode a {}x{} frame",
                frame.width, frame.height
            )));
        }
        // Reads through `as_rgba_f32` rather than indexing `data`, so an
        // `Rgba8` or `RgbaF16` buffer encodes correctly instead of being
        // reinterpreted as float bytes.
        let pixels = frame
            .as_rgba_f32()
            .map_err(|e| MediaError::EncodeError(e.to_string()))?;
        match self.output.format {
            ImageFormat::Png => encode_png(&pixels, frame.width, frame.height),
            ImageFormat::Exr => encode_exr(&pixels, frame.width, frame.height),
            // `new` refuses every other format.
            other => Err(MediaError::UnsupportedCodec(other.to_string())),
        }
    }

    /// Remove partial output. Idempotent, and safe to call from `Drop`.
    fn cleanup(&mut self) -> MediaResult<()> {
        let mut paths: Vec<PathBuf> = self.in_flight.take().into_iter().collect();
        paths.append(&mut self.written);
        let file_result = remove_partial_output(&paths);

        // Only directories this encoder created, and only while empty: a
        // render written into the user's existing output folder must not take
        // the folder with it.
        for dir in std::mem::take(&mut self.created_dirs) {
            if std::fs::remove_dir(&dir).is_err() {
                break;
            }
        }
        file_result
    }

    fn expect_active(&self, operation: &str) -> MediaResult<()> {
        if self.state == State::Active {
            return Ok(());
        }
        Err(MediaError::EncodeError(format!(
            "cannot {operation}: image sequence encoder is {:?}, not active",
            self.state
        )))
    }
}

impl Encoder for ImageSequenceEncoder {
    fn begin(&mut self) -> MediaResult<()> {
        if self.state != State::Ready {
            return Err(MediaError::EncodeError(format!(
                "image sequence encoder already begun (state {:?})",
                self.state
            )));
        }
        self.create_destination()?;
        self.state = State::Active;
        Ok(())
    }

    fn write_frame(&mut self, frame: &FrameBuffer, index: u64) -> MediaResult<()> {
        self.expect_active("write a frame")?;

        let final_path = self.output.frame_path(index);
        let file_name = final_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                MediaError::EncodeError(format!(
                    "frame path is not nameable: {}",
                    final_path.display()
                ))
            })?;
        // Same directory as the destination, or the rename below would stop
        // being atomic across a filesystem boundary.
        let temp_path = self
            .output
            .directory
            .join(format!(".{file_name}.ravel-partial"));

        let bytes = self.encode(frame)?;

        // Recorded before the write so a failure mid-write still has the
        // temporary file on the cleanup list.
        self.in_flight = Some(temp_path.clone());
        std::fs::write(&temp_path, &bytes)?;
        std::fs::rename(&temp_path, &final_path)?;
        self.in_flight = None;
        self.written.push(final_path);
        Ok(())
    }

    fn finish(&mut self) -> MediaResult<()> {
        self.expect_active("finish")?;
        self.state = State::Finished;
        // The files are the deliverable now; forget them so no later drop
        // can reclaim them.
        self.written.clear();
        self.created_dirs.clear();
        Ok(())
    }

    fn abort(&mut self) -> MediaResult<()> {
        if self.state == State::Finished {
            return Err(MediaError::EncodeError(
                "cannot abort an image sequence that was already finished".into(),
            ));
        }
        self.state = State::Aborted;
        self.cleanup()
    }
}

impl Drop for ImageSequenceEncoder {
    fn drop(&mut self) {
        if self.state != State::Active {
            return;
        }
        // A render worker that panics or returns early still must not leave a
        // half-written sequence on disk.
        self.state = State::Aborted;
        if let Err(e) = self.cleanup() {
            tracing::warn!(
                directory = %self.output.directory.display(),
                error = %e,
                "failed to remove partial image sequence output on drop",
            );
        }
    }
}

/// Encode straight-alpha RGBA into an 8-bit PNG.
///
/// Values are clamped and rounded, so a channel that came from an 8-bit
/// source survives the round trip exactly.
fn encode_png(pixels: &[f32], width: u32, height: u32) -> MediaResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(pixels.len());
    for value in pixels {
        bytes.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&bytes, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| MediaError::EncodeError(format!("encode png: {e}")))?;
    Ok(out)
}

/// Encode straight-alpha RGBA into a 32-bit float EXR.
///
/// No clamping and no conversion: the linear scene-referred values, including
/// negatives and anything above 1.0, are what land in the file.
fn encode_exr(pixels: &[f32], width: u32, height: u32) -> MediaResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for value in pixels {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    let mut out = Cursor::new(Vec::new());
    OpenExrEncoder::new(&mut out)
        .write_image(&bytes, width, height, ExtendedColorType::Rgba32F)
        .map_err(|e| MediaError::EncodeError(format!("encode exr: {e}")))?;
    Ok(out.into_inner())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::types::PixelFormat;
    use tempfile::TempDir;

    fn output(dir: &Path, format: ImageFormat) -> ImageSequenceOutput {
        ImageSequenceOutput {
            directory: dir.to_path_buf(),
            prefix: "frame_".into(),
            suffix: String::new(),
            format,
            padding: 4,
        }
    }

    /// A gradient with a distinct value in every channel of every pixel.
    fn frame(width: u32, height: u32, values: impl Fn(usize) -> f32) -> FrameBuffer {
        let count = (width as usize) * (height as usize) * 4;
        FrameBuffer::from_f32(width, height, (0..count).map(values).collect())
    }

    fn read_rgba_f32(path: &Path, format: image::ImageFormat) -> (u32, u32, Vec<f32>) {
        let bytes = std::fs::read(path).expect("output frame is readable");
        let image = image::load_from_memory_with_format(&bytes, format)
            .expect("output frame decodes")
            .to_rgba32f();
        (image.width(), image.height(), image.into_raw())
    }

    // ---- round trips -------------------------------------------------------

    #[test]
    fn png_sequence_reads_back_with_the_values_written() {
        let dir = TempDir::new().unwrap();
        // Multiples of 1/255 survive the 8-bit quantisation exactly.
        let source = frame(4, 3, |i| (i % 256) as f32 / 255.0);

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 0).unwrap();
        encoder.finish().unwrap();

        let path = dir.path().join("frame_0000.png");
        let (width, height, pixels) = read_rgba_f32(&path, image::ImageFormat::Png);
        assert_eq!((width, height), (4, 3));
        let expected = source.as_rgba_f32().unwrap();
        for (index, (got, want)) in pixels.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - want).abs() < 1e-6,
                "channel {index}: wrote {want}, read {got}",
            );
        }
    }

    #[test]
    fn png_preserves_alpha_including_fully_transparent_pixels() {
        let dir = TempDir::new().unwrap();
        // Opaque red, half-transparent green, fully transparent blue, opaque
        // white — the alpha values a compositing hand-off actually depends on.
        let source = FrameBuffer::from_f32(
            2,
            2,
            vec![
                1.0,
                0.0,
                0.0,
                1.0, //
                0.0,
                1.0,
                0.0,
                128.0 / 255.0, //
                0.0,
                0.0,
                1.0,
                0.0, //
                1.0,
                1.0,
                1.0,
                1.0,
            ],
        );

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 7).unwrap();
        encoder.finish().unwrap();

        let (_, _, pixels) =
            read_rgba_f32(&dir.path().join("frame_0007.png"), image::ImageFormat::Png);
        let alphas: Vec<f32> = pixels.iter().skip(3).step_by(4).copied().collect();
        assert!((alphas[0] - 1.0).abs() < 1e-6);
        assert!((alphas[1] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(alphas[2], 0.0);
        assert!((alphas[3] - 1.0).abs() < 1e-6);

        // A transparent pixel must keep its colour: PNG is straight alpha, so
        // the blue channel is not allowed to be multiplied away.
        assert!(
            (pixels[2 * 4 + 2] - 1.0).abs() < 1e-6,
            "colour under zero alpha was not preserved: {:?}",
            &pixels[8..12],
        );
    }

    #[test]
    fn exr_preserves_linear_32bit_float_exactly() {
        let dir = TempDir::new().unwrap();
        // Deliberately outside the 0..1 display range: a highlight at 12.5, a
        // near-zero value that 8- or 16-bit storage would flatten, and a
        // negative that clamping would destroy.
        let source = FrameBuffer::from_f32(
            2,
            1,
            vec![
                12.5,
                0.000_123_45,
                -0.75,
                1.0, //
                65_504.0,
                1.0 / 3.0,
                3.402_823_5e38,
                0.25,
            ],
        );

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Exr)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 1).unwrap();
        encoder.finish().unwrap();

        let (width, height, pixels) = read_rgba_f32(
            &dir.path().join("frame_0001.exr"),
            image::ImageFormat::OpenExr,
        );
        assert_eq!((width, height), (2, 1));
        let expected = source.as_rgba_f32().unwrap();
        assert_eq!(
            pixels,
            expected.to_vec(),
            "EXR must return bit-identical f32 values",
        );
    }

    #[test]
    fn non_f32_source_buffers_are_converted_rather_than_reinterpreted() {
        let dir = TempDir::new().unwrap();
        let mut source = FrameBuffer::with_format(1, 1, PixelFormat::Rgba8);
        source.data = vec![255u8, 0, 128, 255].into();

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 0).unwrap();
        encoder.finish().unwrap();

        let (_, _, pixels) =
            read_rgba_f32(&dir.path().join("frame_0000.png"), image::ImageFormat::Png);
        assert!((pixels[0] - 1.0).abs() < 1e-6);
        assert_eq!(pixels[1], 0.0);
        assert!((pixels[2] - 128.0 / 255.0).abs() < 1e-6);
    }

    // ---- naming ------------------------------------------------------------

    #[test]
    fn frames_are_named_by_absolute_index() {
        let dir = TempDir::new().unwrap();
        let source = frame(1, 1, |_| 0.5);

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        // A `--range 100-101` job hands over absolute numbers, not 0 and 1.
        encoder.write_frame(&source, 100).unwrap();
        encoder.write_frame(&source, 101).unwrap();
        encoder.finish().unwrap();

        assert!(dir.path().join("frame_0100.png").exists());
        assert!(dir.path().join("frame_0101.png").exists());
        assert!(!dir.path().join("frame_0000.png").exists());
    }

    // ---- cancellation ------------------------------------------------------

    fn frame_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    #[test]
    fn abort_leaves_no_partial_output() {
        let dir = TempDir::new().unwrap();
        let out_dir = dir.path().join("renders").join("shot_010");
        let source = frame(2, 2, |i| i as f32 / 16.0);

        let mut encoder = ImageSequenceEncoder::new(output(&out_dir, ImageFormat::Exr)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 0).unwrap();
        encoder.write_frame(&source, 1).unwrap();
        assert_eq!(frame_count(&out_dir), 2);

        encoder.abort().unwrap();

        assert!(
            !out_dir.exists(),
            "aborting must also remove the directory the encoder created",
        );
        assert!(
            !dir.path().join("renders").exists(),
            "every directory level the encoder created must go",
        );
    }

    #[test]
    fn abort_keeps_a_pre_existing_output_directory() {
        let dir = TempDir::new().unwrap();
        let source = frame(1, 1, |_| 1.0);
        let unrelated = dir.path().join("notes.txt");
        std::fs::write(&unrelated, b"keep me").unwrap();

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 0).unwrap();
        encoder.abort().unwrap();

        assert!(dir.path().exists(), "a directory we did not create stays");
        assert!(unrelated.exists(), "unrelated files are untouched");
        assert!(!dir.path().join("frame_0000.png").exists());
    }

    #[test]
    fn dropping_an_unfinished_encoder_removes_what_it_wrote() {
        let dir = TempDir::new().unwrap();
        let source = frame(1, 1, |_| 0.25);
        {
            let mut encoder =
                ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
            encoder.begin().unwrap();
            encoder.write_frame(&source, 3).unwrap();
            assert!(dir.path().join("frame_0003.png").exists());
            // Falls out of scope without finish or abort — the panicking or
            // early-returning render worker.
        }
        assert!(
            !dir.path().join("frame_0003.png").exists(),
            "a dropped, unfinished encoder must not leave frames behind",
        );
    }

    #[test]
    fn finished_output_survives_the_drop() {
        let dir = TempDir::new().unwrap();
        let source = frame(1, 1, |_| 0.25);
        {
            let mut encoder =
                ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
            encoder.begin().unwrap();
            encoder.write_frame(&source, 3).unwrap();
            encoder.finish().unwrap();
        }
        assert!(dir.path().join("frame_0003.png").exists());
    }

    #[test]
    fn no_temporary_files_remain_after_a_successful_run() {
        let dir = TempDir::new().unwrap();
        let source = frame(2, 2, |i| i as f32 / 16.0);

        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Exr)).unwrap();
        encoder.begin().unwrap();
        for index in 0..3 {
            encoder.write_frame(&source, index).unwrap();
        }
        encoder.finish().unwrap();

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "unexpected leftovers: {names:?}");
        assert!(
            names.iter().all(|n| n.ends_with(".exr")),
            "a partial file survived: {names:?}",
        );
    }

    // ---- lifecycle ---------------------------------------------------------

    #[test]
    fn writing_before_begin_is_refused() {
        let dir = TempDir::new().unwrap();
        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        let err = encoder
            .write_frame(&frame(1, 1, |_| 0.0), 0)
            .expect_err("frames before begin are a caller bug, not a silent no-op");
        assert!(matches!(err, MediaError::EncodeError(_)), "{err}");
        assert_eq!(frame_count(dir.path()), 0);
    }

    #[test]
    fn writing_after_finish_is_refused() {
        let dir = TempDir::new().unwrap();
        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.finish().unwrap();
        assert!(encoder.write_frame(&frame(1, 1, |_| 0.0), 0).is_err());
    }

    #[test]
    fn aborting_a_finished_sequence_is_refused() {
        let dir = TempDir::new().unwrap();
        let source = frame(1, 1, |_| 0.5);
        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        encoder.write_frame(&source, 0).unwrap();
        encoder.finish().unwrap();

        assert!(
            encoder.abort().is_err(),
            "abort must not be a way to delete a delivered render",
        );
        assert!(dir.path().join("frame_0000.png").exists());
    }

    #[test]
    fn unsupported_formats_are_refused_at_construction() {
        let dir = TempDir::new().unwrap();
        for format in [ImageFormat::Tiff, ImageFormat::Dpx] {
            let err = ImageSequenceEncoder::new(output(dir.path(), format))
                .err()
                .unwrap_or_else(|| panic!("{format} must not be accepted"));
            assert!(matches!(err, MediaError::UnsupportedCodec(_)), "{err}");
        }
    }

    #[test]
    fn empty_frames_are_refused_without_writing_anything() {
        let dir = TempDir::new().unwrap();
        let mut encoder = ImageSequenceEncoder::new(output(dir.path(), ImageFormat::Png)).unwrap();
        encoder.begin().unwrap();
        assert!(
            encoder
                .write_frame(&FrameBuffer::new_zeroed(0, 0), 0)
                .is_err()
        );
        assert_eq!(frame_count(dir.path()), 0);
        encoder.abort().unwrap();
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WAV writing, without FFmpeg.
//!
//! An image sequence carries no sound, so a render that writes one puts its
//! soundtrack in a WAV file beside the frames
//! (`docs/implementation/render-export-plan.md`, unit 4, "動画コンテナなら
//! mux、連番なら同じ範囲の WAV を併置する"). That companion has to exist
//! wherever the sequence does, and the sequence is the output path that works
//! in **every** build — so this module sits outside the `ffmpeg` feature,
//! exactly like [`sequence`](super::sequence), and writes the container by
//! hand rather than through a codec library.
//!
//! # 32-bit float, not 16-bit PCM
//!
//! The samples arrive from [`Mixer::mix`] as `f32`, so writing
//! `WAVE_FORMAT_IEEE_FLOAT` (format tag 3) is both the conversion-free choice
//! and the lossless one: no quantisation, no clipping of a mix that momentarily
//! exceeds ±1.0, and a round trip that returns the values that went in. It is
//! the same reasoning the plan applies to EXR — what the render output
//! guarantees is precision, and a narrowing conversion belongs to whoever
//! delivers, not to whoever renders.
//!
//! # The size limit is the format's
//!
//! RIFF sizes are unsigned 32-bit, so a WAV cannot describe more than 4 GiB
//! and this writer refuses past that instead of writing a header whose length
//! fields have wrapped — a file that looks valid and decodes as garbage. At
//! 48 kHz stereo `f32` the ceiling is about 3.7 hours.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use ravel_core::media::{MediaError, MediaResult};

/// `WAVE_FORMAT_IEEE_FLOAT`, the format tag for 32-bit float samples.
const FORMAT_IEEE_FLOAT: u16 = 3;

/// Bits per sample. Fixed by the format tag above.
const BITS_PER_SAMPLE: u16 = 32;

/// Byte size of the `fmt ` chunk body for a non-PCM format: the 16 bytes
/// every format has plus the two-byte `cbSize`, which is zero for
/// `WAVE_FORMAT_IEEE_FLOAT` (there are no extra parameters, but a non-PCM
/// format is required to carry the field).
const FMT_CHUNK_BYTES: u32 = 18;

/// Byte size of the `fact` chunk body: one `u32`, the number of sample frames.
/// Required for every non-PCM format.
const FACT_CHUNK_BYTES: u32 = 4;

/// Bytes of header before the sample data: `RIFF` + size + `WAVE`, then the
/// `fmt ` and `fact` chunks with their own headers, then the `data` header.
const HEADER_BYTES: u32 = 12 + (8 + FMT_CHUNK_BYTES) + (8 + FACT_CHUNK_BYTES) + 8;

/// The largest `data` chunk whose enclosing `RIFF` size still fits a `u32`.
pub const MAX_DATA_BYTES: u64 = u32::MAX as u64 - HEADER_BYTES as u64;

/// An open WAV file being written.
///
/// Samples stream out as they are handed over, so a long render never holds
/// the whole soundtrack twice; the two length fields are patched in
/// [`finish`](Self::finish), which is why that call is not optional.
///
/// **A writer dropped without [`finish`](Self::finish) removes its file.**
/// A WAV whose header still claims zero bytes is not a shorter soundtrack,
/// it is a broken one, and leaving it next to a cancelled render's frames
/// would contradict the guarantee that an abandoned job leaves nothing
/// behind.
pub struct WavWriter {
    path: PathBuf,
    file: Option<BufWriter<File>>,
    channels: u16,
    /// Interleaved samples written so far, in bytes.
    data_bytes: u64,
}

impl WavWriter {
    /// Create `path` and write the header for `sample_rate` Hz, `channels`
    /// interleaved 32-bit float channels.
    ///
    /// The parent directory must exist; the caller creating the sequence
    /// directory is what puts the WAV beside the frames rather than a
    /// directory this had to invent.
    pub fn create(path: impl Into<PathBuf>, sample_rate: u32, channels: u32) -> MediaResult<Self> {
        let path = path.into();
        if channels == 0 || sample_rate == 0 {
            return Err(MediaError::EncodeError(format!(
                "a WAV needs a non-zero rate and channel count, got {sample_rate} Hz × {channels}"
            )));
        }
        let channels = u16::try_from(channels).map_err(|_| {
            MediaError::EncodeError(format!("a WAV cannot describe {channels} channels"))
        })?;
        // Refused here rather than wrapped into a plausible-looking value
        // later: `block_align` says why.
        block_align(frame_bytes(channels))?;

        let file = File::create(&path)?;
        let mut writer = Self {
            path,
            file: Some(BufWriter::new(file)),
            channels,
            data_bytes: 0,
        };
        // A failed header leaves a truncated file, which `Drop` removes.
        writer.write_header(sample_rate)?;
        Ok(writer)
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append interleaved samples, in the channel order given to
    /// [`create`](Self::create).
    ///
    /// A slice whose length is not a whole number of sample frames is
    /// refused: it would silently rotate every later frame's channels.
    pub fn write_samples(&mut self, samples: &[f32]) -> MediaResult<()> {
        if !samples.len().is_multiple_of(self.channels as usize) {
            return Err(MediaError::EncodeError(format!(
                "{} samples is not a whole number of {}-channel frames",
                samples.len(),
                self.channels
            )));
        }
        let added = size_of_val(samples) as u64;
        if self.data_bytes + added > MAX_DATA_BYTES {
            return Err(MediaError::EncodeError(format!(
                "a WAV cannot hold more than {MAX_DATA_BYTES} bytes of samples"
            )));
        }

        let file = self.file.as_mut().ok_or_else(finished)?;
        // A fixed scratch buffer rather than one sized to `samples`: the
        // caller hands over a whole render's mix in one call, and a buffer
        // that matched it would double the peak memory of the very path
        // `MAX_DECODE_BYTES` exists to bound. Still a buffer rather than a
        // sample at a time, because `BufWriter` would otherwise pay a bounds
        // check and a copy for every four bytes.
        const CHUNK_SAMPLES: usize = 8 * 1024;
        let mut bytes = Vec::with_capacity(CHUNK_SAMPLES * size_of::<f32>());
        for chunk in samples.chunks(CHUNK_SAMPLES) {
            bytes.clear();
            for sample in chunk {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            file.write_all(&bytes)?;
        }
        self.data_bytes += added;
        Ok(())
    }

    /// Patch the two length fields and close the file.
    ///
    /// After this the WAV is final and the writer no longer owns it, so a
    /// later drop removes nothing.
    ///
    /// **The file is released last**, once every patch has landed. A seek or a
    /// write that fails partway — a full disk is the ordinary way — has to
    /// leave the writer still owning a file whose length fields are still
    /// zero, so [`Drop`] removes it; letting go first would leave a WAV that
    /// claims to be finished and decodes as empty, under the name a completed
    /// render would have used.
    pub fn finish(&mut self) -> MediaResult<()> {
        let data_bytes = u32::try_from(self.data_bytes).map_err(|_| {
            MediaError::EncodeError(format!(
                "a WAV cannot hold more than {MAX_DATA_BYTES} bytes of samples"
            ))
        })?;
        let frame_bytes = self.frame_bytes();
        let file = self.file.as_mut().ok_or_else(finished)?;

        // `RIFF` size: everything after that field itself.
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(HEADER_BYTES - 8 + data_bytes).to_le_bytes())?;
        // `fact` sample-frame count.
        file.seek(SeekFrom::Start(u64::from(HEADER_BYTES) - 12))?;
        file.write_all(&(data_bytes / frame_bytes).to_le_bytes())?;
        // `data` size.
        file.seek(SeekFrom::Start(u64::from(HEADER_BYTES) - 4))?;
        file.write_all(&data_bytes.to_le_bytes())?;
        file.flush()?;
        // Every field is on disk: only now may the file be let go, so that the
        // drop which follows leaves the finished WAV alone.
        self.file.take();
        Ok(())
    }

    /// Bytes per sample frame across all channels.
    fn frame_bytes(&self) -> u32 {
        frame_bytes(self.channels)
    }

    /// Write the header with both length fields zeroed; `finish` patches them.
    fn write_header(&mut self, sample_rate: u32) -> MediaResult<()> {
        let frame_bytes = self.frame_bytes();
        // Both header fields derived by multiplication, both checked: a
        // wrapped byte rate is not a smaller number, it is a header that every
        // reader believes and no reader decodes.
        let byte_rate = sample_rate.checked_mul(frame_bytes).ok_or_else(|| {
            MediaError::EncodeError(format!(
                "a WAV cannot describe {sample_rate} Hz at {frame_bytes} bytes per sample frame: \
                 the byte rate does not fit a u32"
            ))
        })?;
        let block_align = block_align(frame_bytes)?;
        let file = self.file.as_mut().ok_or_else(finished)?;
        file.write_all(b"RIFF")?;
        file.write_all(&0u32.to_le_bytes())?; // patched by `finish`
        file.write_all(b"WAVE")?;

        file.write_all(b"fmt ")?;
        file.write_all(&FMT_CHUNK_BYTES.to_le_bytes())?;
        file.write_all(&FORMAT_IEEE_FLOAT.to_le_bytes())?;
        file.write_all(&self.channels.to_le_bytes())?;
        file.write_all(&sample_rate.to_le_bytes())?;
        file.write_all(&byte_rate.to_le_bytes())?;
        file.write_all(&block_align.to_le_bytes())?;
        file.write_all(&BITS_PER_SAMPLE.to_le_bytes())?;
        file.write_all(&0u16.to_le_bytes())?; // cbSize

        file.write_all(b"fact")?;
        file.write_all(&FACT_CHUNK_BYTES.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?; // patched by `finish`

        file.write_all(b"data")?;
        file.write_all(&0u32.to_le_bytes())?; // patched by `finish`
        Ok(())
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            // Unfinished: the header's lengths are still zero. Removing it is
            // the same promise `ImageSequenceEncoder::abort` makes about the
            // frames — an abandoned render leaves nothing behind.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn finished() -> MediaError {
    MediaError::EncodeError("the WAV has already been finished".to_string())
}

/// Bytes per sample frame across all channels, in the width the arithmetic
/// needs rather than the width the header field has: `channels × 4` leaves
/// `u16` at 16 384 channels, and wrapping there would write a block align of
/// zero into a file that looks valid.
fn frame_bytes(channels: u16) -> u32 {
    u32::from(channels) * u32::from(BITS_PER_SAMPLE / 8)
}

/// The `fmt ` chunk's block align, or a refusal for a sample frame the field
/// cannot hold.
///
/// [`WavWriter::create`] asks first, so a constructed writer never sees the
/// error — which is the point: the channel count is refused where the caller
/// can still do something about it, not silently truncated where the header
/// is written.
fn block_align(frame_bytes: u32) -> MediaResult<u16> {
    u16::try_from(frame_bytes).map_err(|_| {
        MediaError::EncodeError(format!(
            "a sample frame of {frame_bytes} bytes does not fit a WAV's block align field"
        ))
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).expect("the WAV is readable")
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("two bytes"))
    }

    /// Every field a reader looks at, at the offset it looks for it. Spelled
    /// out because there is no library here to be right on our behalf.
    #[test]
    fn the_header_describes_ieee_float_at_the_offsets_a_reader_expects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mix.wav");
        let mut writer = WavWriter::create(&path, 48_000, 2).expect("creates");
        writer.write_samples(&[0.25, -0.25, 1.5, -1.5]).expect("w");
        writer.finish().expect("finishes");

        let bytes = read(&path);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8, "RIFF size");
        assert_eq!(&bytes[8..12], b"WAVE");

        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32_at(&bytes, 16), FMT_CHUNK_BYTES);
        assert_eq!(u16_at(&bytes, 20), FORMAT_IEEE_FLOAT, "format tag");
        assert_eq!(u16_at(&bytes, 22), 2, "channels");
        assert_eq!(u32_at(&bytes, 24), 48_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 48_000 * 8, "byte rate");
        assert_eq!(u16_at(&bytes, 32), 8, "block align");
        assert_eq!(u16_at(&bytes, 34), 32, "bits per sample");
        assert_eq!(u16_at(&bytes, 36), 0, "cbSize, required for non-PCM");

        assert_eq!(&bytes[38..42], b"fact");
        assert_eq!(u32_at(&bytes, 42), FACT_CHUNK_BYTES);
        assert_eq!(u32_at(&bytes, 46), 2, "sample frames");

        assert_eq!(&bytes[50..54], b"data");
        assert_eq!(u32_at(&bytes, 54), 16, "data size");
        assert_eq!(bytes.len() as u32, HEADER_BYTES + 16);
    }

    /// The reason for the format tag: what goes in comes back out, including
    /// the values a 16-bit conversion would have clipped.
    #[test]
    fn samples_round_trip_bit_exactly_including_ones_outside_unity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mix.wav");
        let written = [0.0_f32, 1.0, -1.0, 2.5, -3.75, f32::MIN_POSITIVE];
        let mut writer = WavWriter::create(&path, 44_100, 1).expect("creates");
        writer.write_samples(&written).expect("writes");
        writer.finish().expect("finishes");

        let bytes = read(&path);
        let read_back: Vec<f32> = bytes[HEADER_BYTES as usize..]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("four bytes")))
            .collect();
        assert_eq!(read_back, written);
    }

    /// Several calls have to produce the same file as one, or a render that
    /// streams its mix out block by block would not match one that did not.
    #[test]
    fn streaming_in_blocks_matches_writing_it_all_at_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let all_at_once = dir.path().join("one.wav");
        let mut writer = WavWriter::create(&all_at_once, 48_000, 2).expect("creates");
        writer.write_samples(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]).ok();
        writer.finish().expect("finishes");

        let in_blocks = dir.path().join("many.wav");
        let mut writer = WavWriter::create(&in_blocks, 48_000, 2).expect("creates");
        writer.write_samples(&[0.1, 0.2]).expect("first block");
        writer.write_samples(&[0.3, 0.4, 0.5, 0.6]).expect("rest");
        writer.finish().expect("finishes");

        assert_eq!(read(&all_at_once), read(&in_blocks));
    }

    /// A partial frame would rotate every later frame's channels, so it is
    /// refused rather than padded.
    #[test]
    fn a_partial_sample_frame_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut writer = WavWriter::create(dir.path().join("mix.wav"), 48_000, 2).expect("creates");
        assert!(writer.write_samples(&[0.1, 0.2, 0.3]).is_err());
    }

    /// The byte rate is `rate × frame bytes`, and unchecked that product
    /// panics in a debug build and wraps in a release one — a header every
    /// reader believes and no reader decodes. A rate this absurd is not the
    /// point; the point is that the failure is a refusal rather than a file.
    #[test]
    fn a_byte_rate_that_does_not_fit_the_header_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mix.wav");
        assert!(WavWriter::create(&path, u32::MAX, 2).is_err());
        assert!(!path.exists(), "and the refusal leaves nothing behind");
        // The same rate is fine for a mono frame, so the check is the
        // arithmetic and not a rate ceiling invented here.
        assert!(WavWriter::create(dir.path().join("mono.wav"), u32::MAX / 4, 1).is_ok());
    }

    /// Block align is a `u16`, so the channel count it cannot describe has to
    /// be refused rather than wrapped — `16_384 × 4` is `0` in `u16`.
    #[test]
    fn a_channel_count_whose_sample_frame_does_not_fit_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(WavWriter::create(dir.path().join("a.wav"), 48_000, 16_384).is_err());
        assert!(
            WavWriter::create(dir.path().join("b.wav"), 48_000, 16_383).is_ok(),
            "the refusal is the format's limit, not a smaller one invented here"
        );
    }

    #[test]
    fn a_zero_rate_or_channel_count_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(WavWriter::create(dir.path().join("a.wav"), 0, 2).is_err());
        assert!(WavWriter::create(dir.path().join("b.wav"), 48_000, 0).is_err());
    }

    /// The same promise the image sequence makes: an abandoned render leaves
    /// nothing behind, and a header whose lengths are still zero is not a
    /// shorter soundtrack.
    #[test]
    fn dropping_without_finishing_removes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mix.wav");
        {
            let mut writer = WavWriter::create(&path, 48_000, 2).expect("creates");
            writer.write_samples(&[0.1, 0.2]).expect("writes");
            assert!(path.exists(), "the file exists while it is being written");
        }
        assert!(!path.exists(), "an unfinished WAV is removed");

        {
            let mut writer = WavWriter::create(&path, 48_000, 2).expect("creates");
            writer.write_samples(&[0.1, 0.2]).expect("writes");
            writer.finish().expect("finishes");
        }
        assert!(path.exists(), "a finished WAV survives the drop");
    }

    #[test]
    fn writing_or_finishing_twice_is_an_error_rather_than_a_corrupt_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mix.wav");
        let mut writer = WavWriter::create(&path, 48_000, 1).expect("creates");
        writer.finish().expect("finishes");
        assert!(writer.finish().is_err());
        assert!(writer.write_samples(&[0.1]).is_err());
        assert!(path.exists(), "the finished file is still there");
    }
}

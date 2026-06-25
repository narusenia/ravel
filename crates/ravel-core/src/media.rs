// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Media I/O traits and supporting types.
//!
//! This module defines the [`MediaReader`] and [`MediaWriter`] traits along
//! with codec, container, and stream metadata types used across the media
//! pipeline.  The traits live in `ravel-core` so that both `ravel-media`
//! (FFmpeg backend) and future alternative backends can implement them
//! without circular dependencies.

use crate::types::{AudioBuffer, FrameBuffer, FrameRate};
use std::path::Path;
use thiserror::Error;

// ===========================================================================
// Error
// ===========================================================================

/// Errors originating from media I/O operations.
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),

    #[error("unsupported container format: {0}")]
    UnsupportedContainer(String),

    #[error("no stream of requested type found")]
    NoStreamFound,

    #[error("seek to frame {0} failed")]
    SeekFailed(u64),

    #[error("decode error: {0}")]
    DecodeError(String),

    #[error("encode error: {0}")]
    EncodeError(String),

    #[error("format detection failed for path: {}", .0.display())]
    DetectionFailed(std::path::PathBuf),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// Convenience alias.
pub type MediaResult<T> = Result<T, MediaError>;

// ===========================================================================
// Codec / container enums
// ===========================================================================

/// Supported video codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    ProRes,
    DnxHr,
    Vp8,
    Vp9,
}

impl VideoCodec {
    /// FFmpeg codec name for this variant.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
            Self::Av1 => "av1",
            Self::ProRes => "prores",
            Self::DnxHr => "dnxhd",
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
        }
    }

    /// Attempt to parse an FFmpeg codec name.
    pub fn from_ffmpeg_name(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "hevc" | "h265" => Some(Self::H265),
            "av1" => Some(Self::Av1),
            "prores" => Some(Self::ProRes),
            "dnxhd" => Some(Self::DnxHr),
            "vp8" => Some(Self::Vp8),
            "vp9" => Some(Self::Vp9),
            _ => None,
        }
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ffmpeg_name())
    }
}

/// Supported audio codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioCodec {
    Aac,
    Pcm,
    Flac,
    Opus,
    Mp3,
    Vorbis,
}

impl AudioCodec {
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Pcm => "pcm_f32le",
            Self::Flac => "flac",
            Self::Opus => "opus",
            Self::Mp3 => "mp3",
            Self::Vorbis => "vorbis",
        }
    }

    pub fn from_ffmpeg_name(name: &str) -> Option<Self> {
        match name {
            "aac" => Some(Self::Aac),
            s if s.starts_with("pcm_") => Some(Self::Pcm),
            "flac" => Some(Self::Flac),
            "opus" => Some(Self::Opus),
            "mp3" | "mp3float" => Some(Self::Mp3),
            "vorbis" => Some(Self::Vorbis),
            _ => None,
        }
    }
}

impl std::fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ffmpeg_name())
    }
}

/// Supported container (mux) formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContainerFormat {
    Mp4,
    Mov,
    Mkv,
    WebM,
}

impl ContainerFormat {
    /// FFmpeg short name for this container.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "matroska",
            Self::WebM => "webm",
        }
    }

    /// Detect container from a file extension (case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => Some(Self::Mp4),
            "mov" => Some(Self::Mov),
            "mkv" => Some(Self::Mkv),
            "webm" => Some(Self::WebM),
            _ => None,
        }
    }
}

impl std::fmt::Display for ContainerFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ffmpeg_name())
    }
}

/// Supported image formats for still-frame / image-sequence I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Exr,
    Png,
    Tiff,
    Dpx,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Exr => "exr",
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Dpx => "dpx",
        }
    }

    /// Detect image format from a file extension (case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "exr" => Some(Self::Exr),
            "png" => Some(Self::Png),
            "tif" | "tiff" => Some(Self::Tiff),
            "dpx" => Some(Self::Dpx),
            _ => None,
        }
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.extension())
    }
}

// ===========================================================================
// Stream / media info
// ===========================================================================

/// Metadata about a single stream inside a media container.
#[derive(Clone, Debug)]
pub enum StreamInfo {
    Video(VideoStreamInfo),
    Audio(AudioStreamInfo),
}

/// Metadata for a video stream.
#[derive(Clone, Debug)]
pub struct VideoStreamInfo {
    /// Zero-based stream index inside the container.
    pub stream_index: usize,
    pub codec: Option<VideoCodec>,
    pub codec_name: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    /// Total number of frames, if known.
    pub frame_count: Option<u64>,
    pub duration_secs: Option<f64>,
    pub pixel_format: String,
}

/// Metadata for an audio stream.
#[derive(Clone, Debug)]
pub struct AudioStreamInfo {
    pub stream_index: usize,
    pub codec: Option<AudioCodec>,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    /// Total number of samples per channel, if known.
    pub sample_count: Option<u64>,
    pub duration_secs: Option<f64>,
}

/// Aggregated metadata about a media file.
#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub container: Option<ContainerFormat>,
    pub container_name: String,
    pub streams: Vec<StreamInfo>,
    pub duration_secs: Option<f64>,
}

impl MediaInfo {
    /// Return the first video stream, if any.
    pub fn first_video(&self) -> Option<&VideoStreamInfo> {
        self.streams.iter().find_map(|s| match s {
            StreamInfo::Video(v) => Some(v),
            _ => None,
        })
    }

    /// Return the first audio stream, if any.
    pub fn first_audio(&self) -> Option<&AudioStreamInfo> {
        self.streams.iter().find_map(|s| match s {
            StreamInfo::Audio(a) => Some(a),
            _ => None,
        })
    }
}

// ===========================================================================
// Encoder configuration
// ===========================================================================

/// Configuration for an encode/mux operation.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub container: ContainerFormat,
    pub video: Option<VideoEncoderConfig>,
    pub audio: Option<AudioEncoderConfig>,
}

/// Per-stream video encoder parameters.
#[derive(Clone, Debug)]
pub struct VideoEncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    /// Constant-rate factor (lower = better quality). Codec-dependent range.
    pub crf: Option<u32>,
    /// Target bitrate in bits/sec. Ignored when `crf` is set.
    pub bitrate: Option<u64>,
}

/// Per-stream audio encoder parameters.
#[derive(Clone, Debug)]
pub struct AudioEncoderConfig {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u32,
    /// Target bitrate in bits/sec.
    pub bitrate: Option<u64>,
}

// ===========================================================================
// Traits
// ===========================================================================

/// Read (decode) media from a file or image sequence.
///
/// Implementations live in `ravel-media` (FFmpeg backend).
pub trait MediaReader: Send {
    /// Open a media file at `path` and probe its streams.
    fn open(path: &Path) -> MediaResult<Self>
    where
        Self: Sized;

    /// Return metadata about the opened media.
    fn info(&self) -> &MediaInfo;

    /// Decode a single video frame at the given frame number from the
    /// specified stream index.  The returned [`FrameBuffer`] contains
    /// RGBA f32 pixel data.
    fn decode_video_frame(
        &mut self,
        stream_index: usize,
        frame_number: u64,
    ) -> MediaResult<FrameBuffer>;

    /// Decode a chunk of audio samples starting at `start_sample` (per-channel
    /// offset) from the specified stream.
    fn decode_audio_chunk(
        &mut self,
        stream_index: usize,
        start_sample: u64,
        sample_count: usize,
    ) -> MediaResult<AudioBuffer>;
}

/// Write (encode + mux) media to a file.
pub trait MediaWriter: Send {
    /// Create a new output file at `path` with the given encoder config.
    fn create(path: &Path, config: &EncoderConfig) -> MediaResult<Self>
    where
        Self: Sized;

    /// Write a single video frame.  Frames are expected in sequential order.
    fn write_video_frame(&mut self, frame: &FrameBuffer) -> MediaResult<()>;

    /// Write a chunk of interleaved audio samples.
    fn write_audio_chunk(&mut self, chunk: &AudioBuffer) -> MediaResult<()>;

    /// Flush remaining packets and finalize the file.
    fn finalize(&mut self) -> MediaResult<()>;
}

// ===========================================================================
// Format detection (pure, no FFmpeg dependency)
// ===========================================================================

/// Detected media type for a given file path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectedFormat {
    /// A standard A/V container (MP4, MOV, MKV, WebM).
    Container(ContainerFormat),
    /// A single image that may be part of a numbered sequence.
    ImageSequence(ImageFormat),
    /// Unknown — could not determine from extension.
    Unknown,
}

/// Detect the media format from a file path's extension.
///
/// This is a fast, heuristic-only check.  For robust probing, use
/// the FFmpeg-based prober in `ravel-media`.
pub fn detect_format(path: &Path) -> DetectedFormat {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return DetectedFormat::Unknown,
    };

    if let Some(c) = ContainerFormat::from_extension(ext) {
        return DetectedFormat::Container(c);
    }
    if let Some(i) = ImageFormat::from_extension(ext) {
        return DetectedFormat::ImageSequence(i);
    }
    DetectedFormat::Unknown
}

// ===========================================================================
// Image sequence helpers
// ===========================================================================

/// Describes a numbered image sequence on disk.
///
/// Image sequences follow the pattern `prefix####suffix.ext`, e.g.
/// `render_0001.exr` … `render_0120.exr`.
#[derive(Clone, Debug)]
pub struct ImageSequenceInfo {
    pub directory: std::path::PathBuf,
    pub prefix: String,
    pub suffix: String,
    pub format: ImageFormat,
    pub start_frame: u64,
    pub end_frame: u64,
    pub padding: usize,
}

impl ImageSequenceInfo {
    /// Build the path for a specific frame number.
    pub fn frame_path(&self, frame: u64) -> std::path::PathBuf {
        let name = format!(
            "{}{:0>width$}{}.{}",
            self.prefix,
            frame,
            self.suffix,
            self.format.extension(),
            width = self.padding,
        );
        self.directory.join(name)
    }

    /// Total number of frames in the sequence.
    pub fn frame_count(&self) -> u64 {
        self.end_frame.saturating_sub(self.start_frame) + 1
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- codec / container enums ------------------------------------------

    #[test]
    fn video_codec_roundtrip() {
        let codecs = [
            VideoCodec::H264,
            VideoCodec::H265,
            VideoCodec::Av1,
            VideoCodec::ProRes,
            VideoCodec::DnxHr,
            VideoCodec::Vp8,
            VideoCodec::Vp9,
        ];
        for c in codecs {
            let name = c.ffmpeg_name();
            let parsed = VideoCodec::from_ffmpeg_name(name);
            assert_eq!(parsed, Some(c), "roundtrip failed for {c}");
        }
    }

    #[test]
    fn audio_codec_roundtrip() {
        for c in [AudioCodec::Aac, AudioCodec::Flac, AudioCodec::Opus] {
            assert_eq!(AudioCodec::from_ffmpeg_name(c.ffmpeg_name()), Some(c));
        }
        // PCM family
        assert_eq!(
            AudioCodec::from_ffmpeg_name("pcm_f32le"),
            Some(AudioCodec::Pcm)
        );
        assert_eq!(
            AudioCodec::from_ffmpeg_name("pcm_s16le"),
            Some(AudioCodec::Pcm)
        );
    }

    #[test]
    fn container_from_extension() {
        assert_eq!(
            ContainerFormat::from_extension("mp4"),
            Some(ContainerFormat::Mp4)
        );
        assert_eq!(
            ContainerFormat::from_extension("MOV"),
            Some(ContainerFormat::Mov)
        );
        assert_eq!(
            ContainerFormat::from_extension("mkv"),
            Some(ContainerFormat::Mkv)
        );
        assert_eq!(
            ContainerFormat::from_extension("webm"),
            Some(ContainerFormat::WebM)
        );
        assert_eq!(ContainerFormat::from_extension("avi"), None);
    }

    #[test]
    fn image_format_from_extension() {
        assert_eq!(ImageFormat::from_extension("exr"), Some(ImageFormat::Exr));
        assert_eq!(ImageFormat::from_extension("PNG"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_extension("tif"), Some(ImageFormat::Tiff));
        assert_eq!(ImageFormat::from_extension("TIFF"), Some(ImageFormat::Tiff));
        assert_eq!(ImageFormat::from_extension("dpx"), Some(ImageFormat::Dpx));
        assert_eq!(ImageFormat::from_extension("jpg"), None);
    }

    // ---- format detection -------------------------------------------------

    #[test]
    fn detect_container_format() {
        assert_eq!(
            detect_format(Path::new("video.mp4")),
            DetectedFormat::Container(ContainerFormat::Mp4),
        );
        assert_eq!(
            detect_format(Path::new("/some/path/clip.MOV")),
            DetectedFormat::Container(ContainerFormat::Mov),
        );
    }

    #[test]
    fn detect_image_sequence() {
        assert_eq!(
            detect_format(Path::new("render_0001.exr")),
            DetectedFormat::ImageSequence(ImageFormat::Exr),
        );
        assert_eq!(
            detect_format(Path::new("frame.png")),
            DetectedFormat::ImageSequence(ImageFormat::Png),
        );
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(
            detect_format(Path::new("README.md")),
            DetectedFormat::Unknown
        );
        assert_eq!(detect_format(Path::new("noext")), DetectedFormat::Unknown);
    }

    // ---- image sequence info ----------------------------------------------

    #[test]
    fn image_sequence_frame_path() {
        let seq = ImageSequenceInfo {
            directory: PathBuf::from("/renders"),
            prefix: "frame_".to_string(),
            suffix: String::new(),
            format: ImageFormat::Exr,
            start_frame: 1,
            end_frame: 100,
            padding: 4,
        };
        assert_eq!(seq.frame_path(1), PathBuf::from("/renders/frame_0001.exr"));
        assert_eq!(seq.frame_path(42), PathBuf::from("/renders/frame_0042.exr"));
        assert_eq!(seq.frame_count(), 100);
    }

    #[test]
    fn image_sequence_with_suffix() {
        let seq = ImageSequenceInfo {
            directory: PathBuf::from("/out"),
            prefix: "shot_".to_string(),
            suffix: "_final".to_string(),
            format: ImageFormat::Png,
            start_frame: 0,
            end_frame: 9,
            padding: 3,
        };
        assert_eq!(seq.frame_path(5), PathBuf::from("/out/shot_005_final.png"),);
    }

    // ---- media info helpers -----------------------------------------------

    #[test]
    fn media_info_first_video_audio() {
        let info = MediaInfo {
            container: Some(ContainerFormat::Mp4),
            container_name: "mp4".into(),
            duration_secs: Some(10.0),
            streams: vec![
                StreamInfo::Audio(AudioStreamInfo {
                    stream_index: 0,
                    codec: Some(AudioCodec::Aac),
                    codec_name: "aac".into(),
                    sample_rate: 48000,
                    channels: 2,
                    sample_count: None,
                    duration_secs: Some(10.0),
                }),
                StreamInfo::Video(VideoStreamInfo {
                    stream_index: 1,
                    codec: Some(VideoCodec::H264),
                    codec_name: "h264".into(),
                    width: 1920,
                    height: 1080,
                    frame_rate: FrameRate::new(30, 1),
                    frame_count: Some(300),
                    duration_secs: Some(10.0),
                    pixel_format: "yuv420p".into(),
                }),
            ],
        };
        let v = info.first_video().expect("should have video");
        assert_eq!(v.width, 1920);
        let a = info.first_audio().expect("should have audio");
        assert_eq!(a.sample_rate, 48000);
    }

    #[test]
    fn media_info_no_video() {
        let info = MediaInfo {
            container: None,
            container_name: String::new(),
            duration_secs: None,
            streams: vec![],
        };
        assert!(info.first_video().is_none());
        assert!(info.first_audio().is_none());
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg-based media decoder implementing [`MediaReader`].
//!
//! Opens a media file via FFmpeg's `avformat` layer, probes stream metadata,
//! and decodes video frames (to RGBA f32) and audio chunks (to interleaved
//! f32 PCM).  All FFmpeg access is dynamic-linked (LGPL compliant).

use std::path::Path;
use std::sync::Arc;

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::format::context::Input;
use ffmpeg_the_third::media::Type as MediaType;
use ffmpeg_the_third::software::scaling as sws;
use ffmpeg_the_third::util::format::pixel::Pixel as PixelFormat;
use ffmpeg_the_third::util::frame;

use ravel_core::media::{
    AudioCodec, AudioStreamInfo, ContainerFormat, MediaError, MediaInfo, MediaReader, MediaResult,
    StreamInfo, VideoCodec, VideoStreamInfo,
};
use ravel_core::types::{AudioBuffer, FrameBuffer, FrameRate};

/// Ensure FFmpeg is initialized (safe to call multiple times).
pub(crate) fn init_ffmpeg() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("FFmpeg initialization failed");
    });
}

/// Cached video decoder context, persisted across `decode_video_frame` calls.
struct CachedVideoDecoder {
    decoder: ffmpeg::codec::decoder::Video,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    frame_rate: ffmpeg::Rational,
}

/// Cached audio decoder context, persisted across `decode_audio_chunk` calls.
struct CachedAudioDecoder {
    decoder: ffmpeg::codec::decoder::Audio,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    sample_rate: u32,
    channels: u32,
}

/// FFmpeg-based decoder for video and audio files.
///
/// Supports H.264, H.265, AV1, ProRes, DNxHR video codecs and
/// AAC, PCM, FLAC, Opus audio codecs in MP4, MOV, MKV, WebM containers.
pub struct FfmpegDecoder {
    input_ctx: Input,
    info: MediaInfo,
    /// Index of the best video stream, if any.
    video_stream_index: Option<usize>,
    /// Index of the best audio stream, if any.
    audio_stream_index: Option<usize>,
    /// Cached video decoder, created on first decode call.
    video_decoder: Option<CachedVideoDecoder>,
    /// Cached audio decoder, created on first decode call.
    audio_decoder: Option<CachedAudioDecoder>,
}

/// Create a video decoder for the given stream in the input context.
fn create_video_decoder(
    input_ctx: &Input,
    stream_index: usize,
) -> MediaResult<CachedVideoDecoder> {
    let stream_ref = input_ctx
        .stream(stream_index)
        .ok_or(MediaError::NoStreamFound)?;
    let time_base = stream_ref.time_base();
    let frame_rate = stream_ref.rate();
    let codec_params = stream_ref.parameters();

    let decoder_ctx = ffmpeg::codec::Context::from_parameters(codec_params)
        .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;
    let decoder = decoder_ctx
        .decoder()
        .video()
        .map_err(|e| MediaError::DecodeError(format!("open video decoder: {e}")))?;

    Ok(CachedVideoDecoder {
        decoder,
        stream_index,
        time_base,
        frame_rate,
    })
}

/// Create an audio decoder for the given stream in the input context.
fn create_audio_decoder(
    input_ctx: &Input,
    stream_index: usize,
) -> MediaResult<CachedAudioDecoder> {
    let stream = input_ctx
        .stream(stream_index)
        .ok_or(MediaError::NoStreamFound)?;
    let time_base = stream.time_base();
    let codec_params = stream.parameters();

    let decoder_ctx = ffmpeg::codec::Context::from_parameters(codec_params)
        .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;
    let decoder = decoder_ctx
        .decoder()
        .audio()
        .map_err(|e| MediaError::DecodeError(format!("open audio decoder: {e}")))?;

    let sample_rate = decoder.rate();
    let channels = decoder.ch_layout().channels();

    Ok(CachedAudioDecoder {
        decoder,
        stream_index,
        time_base,
        sample_rate,
        channels,
    })
}

impl FfmpegDecoder {
    /// Probe a media file and build [`MediaInfo`] without fully opening
    /// a decoder context.  Useful for asset metadata collection.
    pub fn probe(path: &Path) -> MediaResult<MediaInfo> {
        init_ffmpeg();
        let ctx = ffmpeg::format::input(path)
            .map_err(|e| MediaError::Other(format!("cannot open {}: {e}", path.display())))?;
        Ok(build_media_info(&ctx))
    }

    /// Ensure a video decoder is cached for the given stream index.
    /// After this call, `self.video_decoder` is guaranteed to be `Some`.
    fn ensure_video_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create = match &self.video_decoder {
            Some(cached) if cached.stream_index == stream_index => false,
            _ => true,
        };
        if needs_create {
            let cached = create_video_decoder(&self.input_ctx, stream_index)?;
            self.video_decoder = Some(cached);
        }
        Ok(())
    }

    /// Ensure an audio decoder is cached for the given stream index.
    /// After this call, `self.audio_decoder` is guaranteed to be `Some`.
    fn ensure_audio_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create = match &self.audio_decoder {
            Some(cached) if cached.stream_index == stream_index => false,
            _ => true,
        };
        if needs_create {
            let cached = create_audio_decoder(&self.input_ctx, stream_index)?;
            self.audio_decoder = Some(cached);
        }
        Ok(())
    }
}

impl MediaReader for FfmpegDecoder {
    fn open(path: &Path) -> MediaResult<Self> {
        init_ffmpeg();

        let input_ctx = ffmpeg::format::input(path)
            .map_err(|e| MediaError::Other(format!("cannot open {}: {e}", path.display())))?;

        let info = build_media_info(&input_ctx);

        let video_stream_index = input_ctx
            .streams()
            .best(MediaType::Video)
            .map(|s| s.index());

        let audio_stream_index = input_ctx
            .streams()
            .best(MediaType::Audio)
            .map(|s| s.index());

        Ok(Self {
            input_ctx,
            info,
            video_stream_index,
            audio_stream_index,
            video_decoder: None,
            audio_decoder: None,
        })
    }

    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn decode_video_frame(
        &mut self,
        stream_index: usize,
        frame_number: u64,
    ) -> MediaResult<FrameBuffer> {
        self.ensure_video_decoder(stream_index)?;
        let cached = self.video_decoder.as_mut().unwrap();

        let time_base = cached.time_base;
        let frame_rate = cached.frame_rate;

        // Compute the PTS that corresponds to the target frame.
        let target_pts = if frame_rate.numerator() > 0 && frame_rate.denominator() > 0 {
            let sec_per_frame = frame_rate.denominator() as f64 / frame_rate.numerator() as f64;
            let target_sec = frame_number as f64 * sec_per_frame;
            (target_sec * time_base.denominator() as f64 / time_base.numerator() as f64) as i64
        } else {
            frame_number as i64
        };

        // Flush the decoder to discard buffered frames from any previous
        // decode position.
        cached.decoder.flush();

        // Seek to the nearest keyframe before the target.
        if frame_number == 0 {
            let _ = self.input_ctx.seek(0, ..=0);
        } else {
            self.input_ctx
                .seek(target_pts, ..=target_pts)
                .map_err(|_| MediaError::SeekFailed(frame_number))?;
        }

        let mut decoded_frame = frame::Video::empty();
        let mut best_frame: Option<frame::Video> = None;

        for result in self.input_ctx.packets() {
            let (stream, packet) =
                result.map_err(|e| MediaError::DecodeError(format!("read packet: {e}")))?;

            if stream.index() != stream_index {
                continue;
            }

            let decoder = &mut self.video_decoder.as_mut().unwrap().decoder;

            decoder
                .send_packet(&packet)
                .map_err(|e| MediaError::DecodeError(format!("send packet: {e}")))?;

            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let pts = decoded_frame.pts().unwrap_or(0);

                if pts >= target_pts {
                    return convert_video_frame_to_rgba(&decoded_frame);
                }

                let mut stash = frame::Video::empty();
                std::mem::swap(&mut stash, &mut decoded_frame);
                best_frame = Some(stash);
            }
        }

        // Flush decoder.
        let decoder = &mut self.video_decoder.as_mut().unwrap().decoder;
        decoder
            .send_eof()
            .map_err(|e| MediaError::DecodeError(format!("flush: {e}")))?;
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let pts = decoded_frame.pts().unwrap_or(0);
            if pts >= target_pts {
                return convert_video_frame_to_rgba(&decoded_frame);
            }
            let mut stash = frame::Video::empty();
            std::mem::swap(&mut stash, &mut decoded_frame);
            best_frame = Some(stash);
        }

        if let Some(ref frame) = best_frame {
            return convert_video_frame_to_rgba(frame);
        }

        Err(MediaError::SeekFailed(frame_number))
    }

    fn decode_audio_chunk(
        &mut self,
        stream_index: usize,
        start_sample: u64,
        sample_count: usize,
    ) -> MediaResult<AudioBuffer> {
        self.ensure_audio_decoder(stream_index)?;
        let cached = self.audio_decoder.as_ref().unwrap();

        let sample_rate = cached.sample_rate;
        let channels = cached.channels;
        let time_base = cached.time_base;

        // Flush the decoder before seeking.
        self.audio_decoder.as_mut().unwrap().decoder.flush();

        // Seek to the appropriate position.
        let target_sec = start_sample as f64 / sample_rate as f64;
        let target_ts =
            (target_sec * time_base.denominator() as f64 / time_base.numerator() as f64) as i64;

        if start_sample == 0 {
            let _ = self.input_ctx.seek(0, ..=0);
        } else {
            self.input_ctx
                .seek(target_ts, ..=target_ts)
                .map_err(|_| MediaError::SeekFailed(start_sample))?;
        }

        let mut collected: Vec<f32> = Vec::with_capacity(sample_count * channels as usize);
        let mut decoded_frame = frame::Audio::empty();

        for result in self.input_ctx.packets() {
            let (stream, packet) =
                result.map_err(|e| MediaError::DecodeError(format!("read packet: {e}")))?;

            if stream.index() != stream_index {
                continue;
            }

            let decoder = &mut self.audio_decoder.as_mut().unwrap().decoder;

            decoder
                .send_packet(&packet)
                .map_err(|e| MediaError::DecodeError(format!("send packet: {e}")))?;

            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let samples = extract_audio_samples(&decoded_frame, channels);
                collected.extend_from_slice(&samples);

                if collected.len() >= sample_count * channels as usize {
                    collected.truncate(sample_count * channels as usize);
                    return Ok(AudioBuffer::new(sample_rate, channels, collected));
                }
            }
        }

        // Flush decoder.
        let decoder = &mut self.audio_decoder.as_mut().unwrap().decoder;
        decoder
            .send_eof()
            .map_err(|e| MediaError::DecodeError(format!("flush: {e}")))?;
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let samples = extract_audio_samples(&decoded_frame, channels);
            collected.extend_from_slice(&samples);
        }

        collected.truncate(sample_count * channels as usize);
        Ok(AudioBuffer::new(sample_rate, channels, collected))
    }
}

// ===========================================================================
// Internal helpers
// ===========================================================================

/// Build [`MediaInfo`] from an opened FFmpeg input context.
fn build_media_info(ctx: &Input) -> MediaInfo {
    let format_name = ctx.format().name().to_string();
    // Try to detect container from the file URL extension first (more
    // precise for WebM vs MKV), then fall back to the format name.
    //
    // SAFETY: `ctx.as_ptr()` returns a valid `*const AVFormatContext`
    // that is alive for the duration of `ctx`.  The `url` field is a
    // NUL-terminated C string set by `avformat_open_input` and remains
    // valid while the context is open.
    let url = unsafe { std::ffi::CStr::from_ptr((*ctx.as_ptr()).url) }
        .to_str()
        .unwrap_or("");
    let container = detect_container_from_url(url).or_else(|| detect_container(&format_name));

    let duration_secs = if ctx.duration() >= 0 {
        Some(ctx.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE))
    } else {
        None
    };

    let streams = ctx
        .streams()
        .filter_map(|stream| {
            let codec_params = stream.parameters();
            match codec_params.medium() {
                MediaType::Video => {
                    let codec_name = codec_params.id().name().to_string();
                    let codec = VideoCodec::from_ffmpeg_name(&codec_name);
                    let rate = stream.rate();
                    let time_base = stream.time_base();
                    let frame_rate = if rate.numerator() > 0 && rate.denominator() > 0 {
                        FrameRate::new(rate.numerator() as u32, rate.denominator() as u32)
                    } else {
                        FrameRate::new(30, 1)
                    };

                    let frame_count = if stream.frames() > 0 {
                        Some(stream.frames() as u64)
                    } else {
                        None
                    };

                    let duration_secs = if stream.duration() > 0 && time_base.numerator() > 0 {
                        Some(
                            stream.duration() as f64 * time_base.numerator() as f64
                                / time_base.denominator() as f64,
                        )
                    } else {
                        None
                    };

                    let (width, height) = extract_video_dimensions(&codec_params);

                    Some(StreamInfo::Video(VideoStreamInfo {
                        stream_index: stream.index(),
                        codec,
                        codec_name,
                        width,
                        height,
                        frame_rate,
                        frame_count,
                        duration_secs,
                        pixel_format: String::new(),
                    }))
                }
                MediaType::Audio => {
                    let codec_name = codec_params.id().name().to_string();
                    let codec = AudioCodec::from_ffmpeg_name(&codec_name);
                    let time_base = stream.time_base();

                    let (sample_rate, channels) = extract_audio_params(&codec_params);

                    let duration_secs = if stream.duration() > 0 && time_base.numerator() > 0 {
                        Some(
                            stream.duration() as f64 * time_base.numerator() as f64
                                / time_base.denominator() as f64,
                        )
                    } else {
                        None
                    };

                    Some(StreamInfo::Audio(AudioStreamInfo {
                        stream_index: stream.index(),
                        codec,
                        codec_name,
                        sample_rate,
                        channels,
                        sample_count: None,
                        duration_secs,
                    }))
                }
                _ => None,
            }
        })
        .collect();

    MediaInfo {
        container,
        container_name: format_name,
        streams,
        duration_secs,
    }
}

/// Detect container from the file URL/path extension.
fn detect_container_from_url(url: &str) -> Option<ContainerFormat> {
    let path = std::path::Path::new(url);
    let ext = path.extension()?.to_str()?;
    ContainerFormat::from_extension(ext)
}

/// Map FFmpeg format name to our [`ContainerFormat`].
fn detect_container(name: &str) -> Option<ContainerFormat> {
    for part in name.split(',') {
        match part.trim() {
            "mp4" | "m4a" | "m4v" => return Some(ContainerFormat::Mp4),
            "mov" => return Some(ContainerFormat::Mov),
            "matroska" | "mkv" => return Some(ContainerFormat::Mkv),
            "webm" => return Some(ContainerFormat::WebM),
            _ => {}
        }
    }
    None
}

/// Extract video width and height from codec parameters.
fn extract_video_dimensions(params: &ffmpeg::codec::ParametersRef<'_>) -> (u32, u32) {
    unsafe {
        let ptr = params.as_ptr();
        ((*ptr).width as u32, (*ptr).height as u32)
    }
}

/// Extract audio sample rate and channel count from codec parameters.
fn extract_audio_params(params: &ffmpeg::codec::ParametersRef<'_>) -> (u32, u32) {
    unsafe {
        let ptr = params.as_ptr();
        let sample_rate = (*ptr).sample_rate as u32;
        let channels = (*ptr).ch_layout.nb_channels as u32;
        (sample_rate, channels)
    }
}

/// Convert an FFmpeg video frame to RGBA f32 [`FrameBuffer`].
fn convert_video_frame_to_rgba(frame: &frame::Video) -> MediaResult<FrameBuffer> {
    let width = frame.width();
    let height = frame.height();

    if width == 0 || height == 0 {
        return Err(MediaError::DecodeError(
            "decoded frame has zero dimensions".into(),
        ));
    }

    // Create a scaling context to convert to RGBA.
    let mut scaler = sws::Context::get(
        frame.format(),
        width,
        height,
        PixelFormat::RGBA,
        width,
        height,
        sws::Flags::BILINEAR,
    )
    .map_err(|e| MediaError::DecodeError(format!("create scaler: {e}")))?;

    let mut rgba_frame = frame::Video::empty();
    scaler
        .run(frame, &mut rgba_frame)
        .map_err(|e| MediaError::DecodeError(format!("scale frame: {e}")))?;

    // Convert u8 RGBA to f32 RGBA.
    let stride = rgba_frame.stride(0);
    let data = rgba_frame.data(0);
    let pixel_count = (width * height) as usize;
    let mut f32_data = Vec::with_capacity(pixel_count * 4);

    for y in 0..height as usize {
        let row_start = y * stride;
        for x in 0..width as usize {
            let offset = row_start + x * 4;
            f32_data.push(data[offset] as f32 / 255.0);
            f32_data.push(data[offset + 1] as f32 / 255.0);
            f32_data.push(data[offset + 2] as f32 / 255.0);
            f32_data.push(data[offset + 3] as f32 / 255.0);
        }
    }

    Ok(FrameBuffer {
        width,
        height,
        data: Arc::from(f32_data),
    })
}

/// Extract interleaved f32 samples from an FFmpeg audio frame.
fn extract_audio_samples(frame: &frame::Audio, channels: u32) -> Vec<f32> {
    let sample_count = frame.samples();
    let ch = channels as usize;
    let mut out = Vec::with_capacity(sample_count * ch);

    let is_planar = frame.is_planar();

    if is_planar {
        for s in 0..sample_count {
            for c in 0..ch {
                let plane = frame.data(c);
                if plane.len() >= (s + 1) * 4 {
                    let bytes = &plane[s * 4..(s + 1) * 4];
                    out.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                } else {
                    out.push(0.0);
                }
            }
        }
    } else {
        let plane = frame.data(0);
        let total_samples = sample_count * ch;
        for i in 0..total_samples {
            if plane.len() >= (i + 1) * 4 {
                let bytes = &plane[i * 4..(i + 1) * 4];
                out.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
            } else {
                out.push(0.0);
            }
        }
    }

    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_container_from_format_name() {
        assert_eq!(
            detect_container("mov,mp4,m4a,3gp,3g2,mj2"),
            Some(ContainerFormat::Mov)
        );
        assert_eq!(
            detect_container("matroska,webm"),
            Some(ContainerFormat::Mkv)
        );
        assert_eq!(detect_container("webm"), Some(ContainerFormat::WebM));
        assert_eq!(detect_container("mp4"), Some(ContainerFormat::Mp4));
        assert_eq!(detect_container("avi"), None);
    }

    #[test]
    fn init_ffmpeg_is_idempotent() {
        init_ffmpeg();
        init_ffmpeg();
        // Should not panic.
    }
}

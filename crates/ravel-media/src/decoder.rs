// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg-based media decoder implementing [`MediaReader`].
//!
//! Opens a media file via FFmpeg's `avformat` layer, probes stream metadata,
//! and decodes video frames (to RGBA f32) and audio chunks (to interleaved
//! f32 PCM).  All FFmpeg access is dynamic-linked (LGPL compliant).
//!
//! When available, hardware-accelerated decoding is used via VideoToolbox
//! (macOS) or NVDEC/D3D11VA (Windows), falling back to software decode
//! transparently.

use std::path::Path;
use std::sync::Arc;

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::ffi;
use ffmpeg_the_third::ffi::AV_TIME_BASE;
use ffmpeg_the_third::format::context::Input;
use ffmpeg_the_third::media::Type as MediaType;
use ffmpeg_the_third::software::scaling as sws;
use ffmpeg_the_third::util::format::pixel::Pixel as PixelFormat;
use ffmpeg_the_third::util::frame;
use tracing::{debug, warn};

use crate::hwaccel::HwAccelConfig;
use crate::hwaccel::device::HwDeviceContext;
use crate::hwaccel::transfer::ensure_sw_frame;
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
    /// Whether this decoder is using hardware acceleration.
    hw_active: bool,
    /// Presentation timestamp of the frame most recently returned, in
    /// `time_base` ticks. Lets a forward request continue from where the
    /// last one stopped instead of seeking again (see
    /// [`FfmpegDecoder::decode_video_frame`]).
    last_returned_pts: Option<i64>,
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
///
/// Hardware-accelerated decoding is attempted automatically via
/// VideoToolbox (macOS) or NVDEC/D3D11VA (Windows).
pub struct FfmpegDecoder {
    input_ctx: Input,
    info: MediaInfo,
    /// Index of the best video stream, if any.
    #[allow(dead_code)]
    video_stream_index: Option<usize>,
    /// Index of the best audio stream, if any.
    #[allow(dead_code)]
    audio_stream_index: Option<usize>,
    /// Cached video decoder, created on first decode call.
    video_decoder: Option<CachedVideoDecoder>,
    /// Cached audio decoder, created on first decode call.
    audio_decoder: Option<CachedAudioDecoder>,
    /// Hardware device context, shared across all video decoders.
    hw_device_ctx: Option<HwDeviceContext>,
}

/// C-callable `get_format` callback for FFmpeg codec context.
///
/// Selects the hardware pixel format matching the target stored in `opaque`.
/// Falls back to the first offered software format if the target is not
/// in the list.
unsafe extern "C" fn hw_get_format(
    ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let target_raw = unsafe { (*ctx).opaque as i32 };
    let target = ffi::AVPixelFormat(target_raw);

    let mut p = pix_fmts;
    unsafe {
        while *p != ffi::AVPixelFormat::NONE {
            if *p == target {
                return *p;
            }
            p = p.add(1);
        }
    }

    // HW format not offered — return first SW format.
    unsafe { *pix_fmts }
}

/// Try to find a matching HW config for the codec that is compatible
/// with our `HwDeviceContext`.
///
/// Returns the hardware pixel format if a match is found.
fn find_hw_config(codec: &ffmpeg::Codec, hw_ctx: &HwDeviceContext) -> Option<ffi::AVPixelFormat> {
    let target_device_type = hw_ctx.backend().to_av_device_type();
    let codec_ptr = codec.as_ptr();

    for i in 0.. {
        let config = unsafe { ffi::avcodec_get_hw_config(codec_ptr, i) };
        if config.is_null() {
            break;
        }

        let config = unsafe { &*config };
        let has_device_method =
            (config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX.0 as i32) != 0;

        if has_device_method && config.device_type == target_device_type {
            return Some(config.pix_fmt);
        }
    }

    None
}

/// A decode target expressed in the two units FFmpeg needs at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeekTarget {
    /// The stream's own time base — what packet timestamps carry.
    pts: i64,
    /// `AV_TIME_BASE` units (microseconds) — what `avformat_seek_file` reads
    /// when it is given `stream_index = -1`.
    micros: i64,
}

/// Ticks of `time_base` in one second, at least 1.
fn ticks_per_second(time_base: ffmpeg::Rational) -> i64 {
    (i64::from(time_base.denominator()) / i64::from(time_base.numerator()).max(1)).max(1)
}

/// The position of `frame_number` in both units.
///
/// Mixing these up is easy and expensive: seeking with stream-time-base ticks
/// makes FFmpeg read them as microseconds, which for a typical `1/12800` base
/// lands two orders of magnitude early — near the start of the file for any
/// frame. The decoder then walks forward to the target, so the cost of
/// decoding one frame grows with its index.
fn seek_target(
    frame_number: u64,
    frame_rate: ffmpeg::Rational,
    time_base: ffmpeg::Rational,
) -> SeekTarget {
    if frame_rate.numerator() <= 0 || frame_rate.denominator() <= 0 {
        // No usable rate: fall back to treating the index as a raw timestamp,
        // which at least keeps ordering monotonic.
        return SeekTarget {
            pts: frame_number as i64,
            micros: 0,
        };
    }

    let sec_per_frame = f64::from(frame_rate.denominator()) / f64::from(frame_rate.numerator());
    let target_sec = frame_number as f64 * sec_per_frame;
    SeekTarget {
        pts: (target_sec * f64::from(time_base.denominator()) / f64::from(time_base.numerator()))
            as i64,
        micros: (target_sec * f64::from(AV_TIME_BASE)) as i64,
    }
}

/// Create a video decoder for the given stream, optionally with HW accel.
fn create_video_decoder(
    input_ctx: &Input,
    stream_index: usize,
    hw_device_ctx: &Option<HwDeviceContext>,
) -> MediaResult<CachedVideoDecoder> {
    let stream_ref = input_ctx
        .stream(stream_index)
        .ok_or(MediaError::NoStreamFound)?;
    let time_base = stream_ref.time_base();
    let frame_rate = stream_ref.rate();
    let codec_params = stream_ref.parameters();

    let mut decoder_ctx = ffmpeg::codec::Context::from_parameters(codec_params)
        .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;

    // Try to configure hardware acceleration.
    let mut hw_active = false;
    if let Some(hw_ctx) = hw_device_ctx
        && let Some(codec) = decoder_ctx.codec()
        && let Some(hw_pix_fmt) = find_hw_config(&codec, hw_ctx)
    {
        let buf_ref = unsafe { hw_ctx.new_ref() };
        if !buf_ref.is_null() {
            unsafe {
                let raw = decoder_ctx.as_mut_ptr();
                (*raw).hw_device_ctx = buf_ref;
                (*raw).get_format = Some(hw_get_format);
                (*raw).opaque = hw_pix_fmt.0 as *mut std::ffi::c_void;
            }
            hw_active = true;
            debug!(
                backend = hw_ctx.backend().name(),
                "configured HW accel for stream {stream_index}"
            );
        } else {
            warn!("av_buffer_ref failed, skipping HW accel");
        }
    }

    let decoder_result = decoder_ctx.decoder().video();

    match decoder_result {
        Ok(decoder) => Ok(CachedVideoDecoder {
            decoder,
            stream_index,
            time_base,
            frame_rate,
            hw_active,
            last_returned_pts: None,
        }),
        Err(e) if hw_active => {
            // HW accel failed to open — retry without it.
            warn!("HW decoder open failed ({e}), falling back to software");
            let fallback_stream = input_ctx
                .stream(stream_index)
                .ok_or(MediaError::NoStreamFound)?;
            let fallback_params = fallback_stream.parameters();
            let decoder_ctx = ffmpeg::codec::Context::from_parameters(fallback_params)
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
                hw_active: false,
                last_returned_pts: None,
            })
        }
        Err(e) => Err(MediaError::DecodeError(format!("open video decoder: {e}"))),
    }
}

/// Create an audio decoder for the given stream.
fn create_audio_decoder(input_ctx: &Input, stream_index: usize) -> MediaResult<CachedAudioDecoder> {
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

    /// Whether hardware-accelerated decoding is active for video.
    pub fn hw_accel_active(&self) -> bool {
        self.video_decoder.as_ref().is_some_and(|d| d.hw_active)
    }

    /// The name of the active HW backend, if any.
    pub fn hw_backend_name(&self) -> Option<&'static str> {
        self.hw_device_ctx.as_ref().map(|ctx| ctx.backend().name())
    }

    /// Ensure a video decoder is cached for the given stream index.
    fn ensure_video_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create =
            !matches!(&self.video_decoder, Some(cached) if cached.stream_index == stream_index);
        if needs_create {
            let cached = create_video_decoder(&self.input_ctx, stream_index, &self.hw_device_ctx)?;
            self.video_decoder = Some(cached);
        }
        Ok(())
    }

    /// Ensure an audio decoder is cached for the given stream index.
    fn ensure_audio_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create =
            !matches!(&self.audio_decoder, Some(cached) if cached.stream_index == stream_index);
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

        // Try to initialize hardware acceleration.
        let config = HwAccelConfig::default();
        let hw_device_ctx = HwDeviceContext::try_create(&config).unwrap_or_else(|e| {
            warn!("HW device context creation failed: {e}");
            None
        });

        Ok(Self {
            input_ctx,
            info,
            video_stream_index,
            audio_stream_index,
            video_decoder: None,
            audio_decoder: None,
            hw_device_ctx,
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

        // The target position, in two different units.
        //
        // `target_pts` is in the stream's own time base and is what packet
        // timestamps are compared against below. `target_micros` is the same
        // instant in `AV_TIME_BASE` units, which is what seeking needs:
        // `Input::seek` calls `avformat_seek_file` with `stream_index = -1`,
        // and FFmpeg documents that timestamps are then in `AV_TIME_BASE`
        // (microseconds) rather than any stream's time base.
        //
        // Passing stream-time-base ticks here used to make every seek land
        // near the start of the file — for a typical `1/12800` time base,
        // frame 100 at 24 fps asks for tick 53333, which reads back as
        // 53 ms. The decode loop then walked forward from there to the
        // target, so the cost of one frame grew with its index.
        let SeekTarget {
            pts: target_pts,
            micros: target_micros,
        } = seek_target(frame_number, frame_rate, time_base);

        // How far ahead the sequential path may read before it gives up and
        // seeks, expressed in `time_base` ticks. One second is a couple of
        // GOPs for typical footage, so a scrub of more than that is faster
        // served by a keyframe seek.
        let forward_scan_limit_pts = ticks_per_second(time_base);

        // Playback asks for frames in order, so the common case is "the next
        // frame after the one just returned". Seeking for that would throw
        // away the decoder state and re-decode from the preceding keyframe
        // every time — on a 60-frame GOP, ~30 wasted frames per displayed
        // frame. Continue reading instead whenever the target lies ahead of
        // the last frame returned; the loop below already stops at the first
        // frame whose pts reaches the target.
        //
        // The window is capped so a long forward jump still seeks: walking
        // forward only wins while it stays cheaper than landing on a
        // keyframe.
        let can_continue = cached
            .last_returned_pts
            .is_some_and(|last| target_pts > last && target_pts - last <= forward_scan_limit_pts);

        if !can_continue {
            // Flush the decoder to discard buffered frames from the previous
            // decode position.
            cached.decoder.flush();

            // Seek to the nearest keyframe at or before the target. The range
            // is open at the start so FFmpeg may rewind as far as it needs to
            // reach one; capping it at the target keeps it from overshooting.
            if frame_number == 0 {
                let _ = self.input_ctx.seek(0, ..=0);
            } else {
                self.input_ctx
                    .seek(target_micros, ..=target_micros)
                    .map_err(|_| MediaError::SeekFailed(frame_number))?;
            }
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
                    // Remember where playback stopped so the next forward
                    // request can continue instead of seeking.
                    self.video_decoder.as_mut().unwrap().last_returned_pts = Some(pts);
                    let sw_frame = ensure_sw_frame(&decoded_frame)?;
                    return convert_video_frame_to_rgba(
                        sw_frame.as_ref().unwrap_or(&decoded_frame),
                    );
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
                // Drained at EOF: the decoder holds no more packets, so the
                // next request has to seek regardless.
                self.video_decoder.as_mut().unwrap().last_returned_pts = None;
                let sw_frame = ensure_sw_frame(&decoded_frame)?;
                return convert_video_frame_to_rgba(sw_frame.as_ref().unwrap_or(&decoded_frame));
            }
            let mut stash = frame::Video::empty();
            std::mem::swap(&mut stash, &mut decoded_frame);
            best_frame = Some(stash);
        }

        if let Some(ref frame) = best_frame {
            self.video_decoder.as_mut().unwrap().last_returned_pts = None;
            let sw_frame = ensure_sw_frame(frame)?;
            return convert_video_frame_to_rgba(sw_frame.as_ref().unwrap_or(frame));
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
    // SAFETY: `ctx.as_ptr()` returns a valid `*const AVFormatContext`
    // that is alive for the duration of `ctx`.
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

/// Borrow an audio frame's raw sample bytes, one slice per plane.
///
/// This deliberately bypasses `frame::Audio::data()`, which sizes plane `i`
/// from `AVFrame::linesize[i]`. FFmpeg fills only `linesize[0]` for audio and
/// leaves the rest at zero, so `data()` reports every planar channel past the
/// first as an empty plane. That made planar sources — AAC, and therefore the
/// audio track of most video files — decode with signal on channel 0 alone
/// and silence everywhere else, so a stereo file played on one side only.
///
/// The plane sizes come from the frame's own geometry instead: `nb_samples`
/// samples per planar plane, `nb_samples × channels` for a packed one.
fn audio_planes(frame: &frame::Audio, planar: bool, channels: usize) -> Vec<&[u8]> {
    let width = frame.format().bytes();
    let (plane_count, plane_len) = if planar {
        (channels, frame.samples() * width)
    } else {
        (1, frame.samples() * channels * width)
    };

    let mut planes = Vec::with_capacity(plane_count);
    // SAFETY: `extended_data` points to at least `plane_count` plane pointers
    // for the lifetime of the frame; it aliases `data` for the first eight
    // planes and is separately allocated beyond that, so `data` is only read
    // for indices inside its fixed-size array. Each plane FFmpeg allocated
    // holds at least `plane_len` bytes — `av_samples_get_buffer_size` rounds
    // the sample count up to a multiple of 32, never down. The returned
    // slices borrow `frame`, so they cannot outlive the buffer.
    unsafe {
        let raw = frame.as_ptr();
        let extended = (*raw).extended_data;
        for index in 0..plane_count {
            let data = if !extended.is_null() {
                *extended.add(index)
            } else if index < (*raw).data.len() {
                (*raw).data[index]
            } else {
                break;
            };
            if data.is_null() {
                break;
            }
            planes.push(std::slice::from_raw_parts(data, plane_len));
        }
    }
    planes
}

/// Extract interleaved f32 samples from an FFmpeg audio frame.
fn extract_audio_samples(frame: &frame::Audio, channels: u32) -> Vec<f32> {
    let sample_count = frame.samples();
    let ch = channels as usize;
    let mut out = Vec::with_capacity(sample_count * ch);

    let is_planar = frame.is_planar();
    let planes = audio_planes(frame, is_planar, ch);

    if is_planar {
        for s in 0..sample_count {
            for c in 0..ch {
                let plane = planes.get(c).copied().unwrap_or_default();
                if plane.len() >= (s + 1) * 4 {
                    let bytes = &plane[s * 4..(s + 1) * 4];
                    out.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                } else {
                    out.push(0.0);
                }
            }
        }
    } else {
        let plane = planes.first().copied().unwrap_or_default();
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

    /// The seek timestamp must be in `AV_TIME_BASE` units, not the stream's.
    /// Conflating them made every seek land near the start of the file, so
    /// decoding frame N cost O(N) (audio-plan/media perf regression).
    #[test]
    fn seek_target_reports_microseconds_independently_of_the_time_base() {
        let rate = ffmpeg::Rational::new(24, 1);
        // A 1/12800 time base is what a typical H.264 mp4 carries.
        let fine = ffmpeg::Rational::new(1, 12800);
        let target = seek_target(240, rate, fine);

        // 240 frames at 24 fps is exactly 10 s.
        assert_eq!(target.micros, 10_000_000);
        assert_eq!(target.pts, 128_000);

        // The same instant, described by a coarser stream clock: the pts
        // changes, the microsecond position must not.
        let coarse = ffmpeg::Rational::new(1, 600);
        let target = seek_target(240, rate, coarse);
        assert_eq!(target.micros, 10_000_000);
        assert_eq!(target.pts, 6_000);
    }

    #[test]
    fn seek_target_handles_frame_zero_and_unusable_rates() {
        let fine = ffmpeg::Rational::new(1, 12800);
        let zero = seek_target(0, ffmpeg::Rational::new(24, 1), fine);
        assert_eq!(zero.pts, 0);
        assert_eq!(zero.micros, 0);

        // A stream with no declared rate must not divide by zero.
        let unusable = seek_target(7, ffmpeg::Rational::new(0, 1), fine);
        assert_eq!(unusable.pts, 7);
        assert_eq!(unusable.micros, 0);
    }

    #[test]
    fn forward_scan_limit_is_one_second_of_ticks() {
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(1, 12800)), 12_800);
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(1, 600)), 600);
        // Degenerate numerators must not panic or yield a zero-width window.
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(0, 600)), 600);
        assert!(ticks_per_second(ffmpeg::Rational::new(1, 0)) >= 1);
    }

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
    }
}

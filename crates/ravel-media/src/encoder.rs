// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg-based media encoder implementing [`MediaWriter`].
//!
//! Creates an output container, encodes video frames from RGBA f32
//! [`FrameBuffer`] data, and muxes audio from interleaved f32 PCM
//! [`AudioBuffer`] data.

use std::path::Path;

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::software::scaling as sws;
use ffmpeg_the_third::util::format::pixel::Pixel as PixelFmt;
use ffmpeg_the_third::util::frame;
use ffmpeg_the_third::{ChannelLayout, ChannelLayoutMask, Rational};

use ravel_core::media::{
    AudioEncoderConfig, EncoderConfig, MediaError, MediaResult, MediaWriter, VideoCodec,
    VideoEncoderConfig,
};
use ravel_core::types::{AudioBuffer, FrameBuffer};

/// FFmpeg-based encoder / muxer.
///
/// **Not `Send`** due to internal FFmpeg pointers; the [`MediaWriter`] trait
/// requires `Send`, so we add an explicit unsafe impl below — this is safe
/// because we never share the encoder across threads concurrently (single
/// owner writes sequentially).
pub struct FfmpegEncoder {
    output_ctx: ffmpeg::format::context::Output,
    video_stream_index: Option<usize>,
    audio_stream_index: Option<usize>,
    video_encoder: Option<ffmpeg::codec::encoder::video::Encoder>,
    audio_encoder: Option<ffmpeg::codec::encoder::audio::Encoder>,
    video_scaler: Option<sws::Context>,
    /// Saved video time base for packet rescaling.
    video_time_base: Option<Rational>,
    /// Saved audio time base for packet rescaling.
    audio_time_base: Option<Rational>,
    video_pts: i64,
    audio_pts: i64,
}

// SAFETY: The raw pointers inside `sws::Context` and `Output` have no
// thread affinity (no TLS dependency in FFmpeg's libswscale or
// libavformat).  Ownership can be safely transferred to another thread,
// which is what `Send` requires.  The struct is used as a single-owner
// sequential writer, so no concurrent access occurs.
unsafe impl Send for FfmpegEncoder {}

impl MediaWriter for FfmpegEncoder {
    fn create(path: &Path, config: &EncoderConfig) -> MediaResult<Self> {
        crate::decoder::init_ffmpeg();

        let mut output_ctx = ffmpeg::format::output(path)
            .map_err(|e| MediaError::Other(format!("cannot create {}: {e}", path.display())))?;

        let mut video_stream_index = None;
        let mut audio_stream_index = None;
        let mut video_encoder = None;
        let mut audio_encoder = None;
        let mut video_scaler = None;
        let mut video_time_base = None;
        let mut audio_time_base = None;

        // Set up video stream if configured.
        if let Some(ref vcfg) = config.video {
            let result = create_video_stream(&mut output_ctx, vcfg)?;
            video_stream_index = Some(result.stream_index);
            video_time_base = Some(result.time_base);
            video_encoder = Some(result.encoder);
            video_scaler = Some(result.scaler);
        }

        // Set up audio stream if configured.
        if let Some(ref acfg) = config.audio {
            let result = create_audio_stream(&mut output_ctx, acfg)?;
            audio_stream_index = Some(result.stream_index);
            audio_time_base = Some(result.time_base);
            audio_encoder = Some(result.encoder);
        }

        // Write header.
        output_ctx
            .write_header()
            .map_err(|e| MediaError::EncodeError(format!("write header: {e}")))?;

        Ok(Self {
            output_ctx,
            video_stream_index,
            audio_stream_index,
            video_encoder,
            audio_encoder,
            video_scaler,
            video_time_base,
            audio_time_base,
            video_pts: 0,
            audio_pts: 0,
        })
    }

    fn write_video_frame(&mut self, frame_buf: &FrameBuffer) -> MediaResult<()> {
        let encoder = self
            .video_encoder
            .as_mut()
            .ok_or_else(|| MediaError::EncodeError("no video encoder configured".into()))?;
        let scaler = self
            .video_scaler
            .as_mut()
            .ok_or_else(|| MediaError::EncodeError("no video scaler configured".into()))?;
        let stream_index = self.video_stream_index.unwrap();

        let width = frame_buf.width;
        let height = frame_buf.height;

        // Build RGBA u8 frame from f32 data.
        let mut rgba_frame = frame::Video::new(PixelFmt::RGBA, width, height);
        let stride = rgba_frame.stride(0);
        {
            let data = rgba_frame.data_mut(0);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let src_idx = (y * width as usize + x) * 4;
                    let dst_idx = y * stride + x * 4;
                    data[dst_idx] = (frame_buf.data[src_idx].clamp(0.0, 1.0) * 255.0) as u8;
                    data[dst_idx + 1] = (frame_buf.data[src_idx + 1].clamp(0.0, 1.0) * 255.0) as u8;
                    data[dst_idx + 2] = (frame_buf.data[src_idx + 2].clamp(0.0, 1.0) * 255.0) as u8;
                    data[dst_idx + 3] = (frame_buf.data[src_idx + 3].clamp(0.0, 1.0) * 255.0) as u8;
                }
            }
        }
        rgba_frame.set_pts(Some(self.video_pts));
        self.video_pts += 1;

        // Scale RGBA -> encoder's pixel format (typically YUV420P).
        let mut yuv_frame = frame::Video::empty();
        scaler
            .run(&rgba_frame, &mut yuv_frame)
            .map_err(|e| MediaError::EncodeError(format!("scale frame: {e}")))?;
        yuv_frame.set_pts(rgba_frame.pts());

        // Encode.
        encoder
            .send_frame(&yuv_frame)
            .map_err(|e| MediaError::EncodeError(format!("send frame: {e}")))?;

        let enc_tb = self.video_time_base.unwrap();
        receive_video_packets(encoder, stream_index, enc_tb, &mut self.output_ctx)?;
        Ok(())
    }

    fn write_audio_chunk(&mut self, chunk: &AudioBuffer) -> MediaResult<()> {
        let encoder = self
            .audio_encoder
            .as_mut()
            .ok_or_else(|| MediaError::EncodeError("no audio encoder configured".into()))?;
        let stream_index = self.audio_stream_index.unwrap();

        let frame_size = encoder.frame_size() as usize;
        // Use at least 1024 as frame size if the encoder reports 0
        // (some codecs like FLAC have variable frame sizes).
        let chunk_size = if frame_size > 0 { frame_size } else { 1024 };
        let channels = chunk.channels as usize;
        let total_samples = match chunk.data.len().checked_div(channels) {
            Some(n) => n,
            None => return Ok(()),
        };

        // Choose the right channel layout mask.
        let layout_mask = match channels {
            1 => ChannelLayoutMask::MONO,
            2 => ChannelLayoutMask::STEREO,
            _ => ChannelLayoutMask::STEREO, // Fallback for now.
        };

        let mut offset = 0;
        while offset < total_samples {
            let remaining = total_samples - offset;
            let samples_this_chunk = remaining.min(chunk_size);

            let mut audio_frame = frame::Audio::new(
                ffmpeg_the_third::util::format::sample::Sample::F32(
                    ffmpeg_the_third::util::format::sample::Type::Packed,
                ),
                samples_this_chunk,
                layout_mask,
            );
            audio_frame.set_pts(Some(self.audio_pts));
            self.audio_pts += samples_this_chunk as i64;

            // Copy interleaved f32 samples into the frame.
            let plane = audio_frame.data_mut(0);
            let src_start = offset * channels;
            let src_end = (offset + samples_this_chunk) * channels;
            let src_slice = &chunk.data[src_start..src_end];
            let byte_count = src_slice.len() * 4;
            if plane.len() < byte_count {
                return Err(MediaError::EncodeError(format!(
                    "audio frame plane too small: need {byte_count} bytes, have {}",
                    plane.len()
                )));
            }
            // SAFETY: f32 and u8 have compatible layouts, and we
            // checked that the destination is large enough above.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src_slice.as_ptr() as *const u8,
                    plane.as_mut_ptr(),
                    byte_count,
                );
            }

            encoder
                .send_frame(&audio_frame)
                .map_err(|e| MediaError::EncodeError(format!("send audio frame: {e}")))?;

            let enc_tb = self.audio_time_base.unwrap();
            receive_audio_packets(encoder, stream_index, enc_tb, &mut self.output_ctx)?;
            offset += samples_this_chunk;
        }

        Ok(())
    }

    fn finalize(&mut self) -> MediaResult<()> {
        // Flush video encoder.
        if let Some(ref mut enc) = self.video_encoder {
            enc.send_eof()
                .map_err(|e| MediaError::EncodeError(format!("video flush: {e}")))?;
            let idx = self.video_stream_index.unwrap();
            let tb = self.video_time_base.unwrap();
            receive_video_packets(enc, idx, tb, &mut self.output_ctx)?;
        }

        // Flush audio encoder.
        if let Some(ref mut enc) = self.audio_encoder {
            enc.send_eof()
                .map_err(|e| MediaError::EncodeError(format!("audio flush: {e}")))?;
            let idx = self.audio_stream_index.unwrap();
            let tb = self.audio_time_base.unwrap();
            receive_audio_packets(enc, idx, tb, &mut self.output_ctx)?;
        }

        // Write trailer.
        self.output_ctx
            .write_trailer()
            .map_err(|e| MediaError::EncodeError(format!("write trailer: {e}")))?;

        Ok(())
    }
}

// ===========================================================================
// Packet drain helper
// ===========================================================================

/// Receive and write video packets.
fn receive_video_packets(
    encoder: &mut ffmpeg::codec::encoder::video::Encoder,
    stream_index: usize,
    encoder_time_base: Rational,
    output_ctx: &mut ffmpeg::format::context::Output,
) -> MediaResult<()> {
    let mut packet = ffmpeg::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream_index);
        let out_tb = output_ctx.stream(stream_index).unwrap().time_base();
        packet.rescale_ts(encoder_time_base, out_tb);
        packet
            .write_interleaved(output_ctx)
            .map_err(|e| MediaError::EncodeError(format!("write video packet: {e}")))?;
    }
    Ok(())
}

/// Receive and write audio packets.
fn receive_audio_packets(
    encoder: &mut ffmpeg::codec::encoder::audio::Encoder,
    stream_index: usize,
    encoder_time_base: Rational,
    output_ctx: &mut ffmpeg::format::context::Output,
) -> MediaResult<()> {
    let mut packet = ffmpeg::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream_index);
        let out_tb = output_ctx.stream(stream_index).unwrap().time_base();
        packet.rescale_ts(encoder_time_base, out_tb);
        packet
            .write_interleaved(output_ctx)
            .map_err(|e| MediaError::EncodeError(format!("write audio packet: {e}")))?;
    }
    Ok(())
}

// ===========================================================================
// Stream creation helpers
// ===========================================================================

/// Map our [`VideoCodec`] enum to an FFmpeg encoder name.
fn video_encoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "libx264",
        VideoCodec::H265 => "libx265",
        VideoCodec::Av1 => "libsvtav1",
        VideoCodec::ProRes => "prores_ks",
        VideoCodec::DnxHr => "dnxhd",
        VideoCodec::Vp8 => "libvpx",
        VideoCodec::Vp9 => "libvpx-vp9",
    }
}

/// Result of creating a video stream.
struct VideoStreamResult {
    encoder: ffmpeg::codec::encoder::video::Encoder,
    stream_index: usize,
    time_base: Rational,
    scaler: sws::Context,
}

/// Result of creating an audio stream.
struct AudioStreamResult {
    encoder: ffmpeg::codec::encoder::audio::Encoder,
    stream_index: usize,
    time_base: Rational,
}

/// Create a video output stream and encoder.
fn create_video_stream(
    output_ctx: &mut ffmpeg::format::context::Output,
    cfg: &VideoEncoderConfig,
) -> MediaResult<VideoStreamResult> {
    let encoder_name = video_encoder_name(cfg.codec);
    let codec = ffmpeg::encoder::find_by_name(encoder_name)
        .ok_or_else(|| MediaError::UnsupportedCodec(format!("encoder {encoder_name} not found")))?;

    let mut stream = output_ctx
        .add_stream(codec)
        .map_err(|e| MediaError::EncodeError(format!("add video stream: {e}")))?;
    let stream_index = stream.index();

    let encoder_ctx = ffmpeg::codec::Context::new_with_codec(codec);
    let mut video = encoder_ctx
        .encoder()
        .video()
        .map_err(|e| MediaError::EncodeError(format!("create video encoder: {e}")))?;

    let time_base = Rational::new(cfg.frame_rate.den as i32, cfg.frame_rate.num as i32);
    video.set_width(cfg.width);
    video.set_height(cfg.height);
    video.set_time_base(time_base);
    video.set_frame_rate(Some(Rational::new(
        cfg.frame_rate.num as i32,
        cfg.frame_rate.den as i32,
    )));

    // Choose pixel format based on codec.
    let pix_fmt = match cfg.codec {
        VideoCodec::ProRes => PixelFmt::YUV422P10LE,
        _ => PixelFmt::YUV420P,
    };
    video.set_format(pix_fmt);

    // Set quality / bitrate.
    if let Some(bitrate) = cfg.bitrate {
        video.set_bit_rate(bitrate as usize);
    }

    // Open encoder (with or without CRF option).
    let opened = if let Some(crf) = cfg.crf {
        let mut opts = ffmpeg::Dictionary::new();
        opts.set("crf", crf.to_string());
        video.open_with(opts)
    } else {
        video.open()
    }
    .map_err(|e| MediaError::EncodeError(format!("open video encoder: {e}")))?;

    // Copy encoder parameters to the stream.
    stream.copy_parameters_from_context(&opened);

    // Create scaler RGBA -> encoder pixel format.
    let scaler = sws::Context::get(
        PixelFmt::RGBA,
        cfg.width,
        cfg.height,
        pix_fmt,
        cfg.width,
        cfg.height,
        sws::Flags::BILINEAR,
    )
    .map_err(|e| MediaError::EncodeError(format!("create scaler: {e}")))?;

    Ok(VideoStreamResult {
        encoder: opened,
        stream_index,
        time_base,
        scaler,
    })
}

/// Create an audio output stream and encoder.
fn create_audio_stream(
    output_ctx: &mut ffmpeg::format::context::Output,
    cfg: &AudioEncoderConfig,
) -> MediaResult<AudioStreamResult> {
    let encoder_name = cfg.codec.ffmpeg_name();
    let codec = ffmpeg::encoder::find_by_name(encoder_name)
        .ok_or_else(|| MediaError::UnsupportedCodec(format!("encoder {encoder_name} not found")))?;

    let mut stream = output_ctx
        .add_stream(codec)
        .map_err(|e| MediaError::EncodeError(format!("add audio stream: {e}")))?;
    let stream_index = stream.index();

    let encoder_ctx = ffmpeg::codec::Context::new_with_codec(codec);
    let mut audio = encoder_ctx
        .encoder()
        .audio()
        .map_err(|e| MediaError::EncodeError(format!("create audio encoder: {e}")))?;

    let time_base = Rational::new(1, cfg.sample_rate as i32);
    audio.set_rate(cfg.sample_rate as i32);
    audio.set_ch_layout(ChannelLayout::default_for_channels(cfg.channels));
    audio.set_format(ffmpeg_the_third::util::format::sample::Sample::F32(
        ffmpeg_the_third::util::format::sample::Type::Packed,
    ));
    audio.set_time_base(time_base);

    if let Some(bitrate) = cfg.bitrate {
        audio.set_bit_rate(bitrate as usize);
    }

    let opened = audio
        .open()
        .map_err(|e| MediaError::EncodeError(format!("open audio encoder: {e}")))?;

    // Copy encoder parameters to the stream.
    stream.copy_parameters_from_context(&opened);

    Ok(AudioStreamResult {
        encoder: opened,
        stream_index,
        time_base,
    })
}

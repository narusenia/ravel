// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg integration, hardware decode, audio engine, and OCIO for Ravel.
//!
//! # Feature flags
//!
//! - `ffmpeg` (**off by default**; `ravel-app` and `ravel-nodes` re-export it)
//!   — enables the FFmpeg-based [`decoder::FfmpegDecoder`] and
//!   [`encoder::FfmpegEncoder`].  Requires FFmpeg shared libraries at link
//!   time (LGPL dynamic linking).
//!
//! [`audio_sample`] is deliberately outside that flag: it holds the sample
//! format arithmetic the decoder feeds, so the conversion stays testable
//! without FFmpeg.  [`encode`] is outside it too, because PNG and EXR
//! sequences are the render output that must work in every build, and so is
//! [`frame_cache`], which holds decoded frames whatever produced them — a
//! build without FFmpeg still caches what an injected reader decodes.

#[cfg(feature = "ffmpeg")]
pub mod decoder;
#[cfg(feature = "ffmpeg")]
pub mod encoder;
#[cfg(feature = "ffmpeg")]
pub mod hwaccel;

pub mod audio_sample;
pub mod encode;
pub mod error;
pub mod format;
pub mod frame_cache;
pub mod image_seq;

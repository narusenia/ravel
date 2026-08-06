// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-output encoders and the runtime encoder inventory.
//!
//! The [`Encoder`](ravel_core::media::encode::Encoder) contract itself lives
//! in `ravel-core`, so a render worker can name it without depending on this
//! crate. What lives here is the implementation:
//!
//! - [`ImageSequenceEncoder`] writes PNG and EXR sequences **without FFmpeg**,
//!   which is why this module is not behind the `ffmpeg` feature. Image
//!   sequences are the output path that has to exist in every build.

pub mod sequence;

pub use sequence::ImageSequenceEncoder;

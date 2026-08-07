// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-output encoders and the runtime encoder inventory.
//!
//! The [`Encoder`](ravel_core::media::encode::Encoder) contract itself lives
//! in `ravel-core`, so a render worker can name it without depending on this
//! crate. What lives here is the implementations and the environment probe:
//!
//! - [`ImageSequenceEncoder`] writes PNG and EXR sequences **without FFmpeg**,
//!   which is why this module is not behind the `ffmpeg` feature. Image
//!   sequences are the output path that has to exist in every build.
//! - [`WavWriter`] is the sound half of that same path: an image sequence
//!   carries none, so a render puts its soundtrack in a WAV beside the
//!   frames. FFmpeg-free for the same reason.
//! - [`available_encoders`] answers "what can this binary, on this machine,
//!   actually write?" by asking the linked FFmpeg and the host platform.

pub mod probe;
pub mod sequence;
pub mod wav;

pub use probe::{RuntimeProbe, available_encoders};
pub use sequence::ImageSequenceEncoder;
pub use wav::WavWriter;

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified animation channel system (REQ-CORE-007).
//!
//! Every animatable parameter draws its value from an [`AnimationChannel`],
//! whose [`ChannelSource`] may be a constant, a keyframe curve, a Lua
//! expression, another node's output, an audio-reactive analysis, or a blend of
//! two sources. Keyframe curves support Bézier, linear, and step interpolation
//! and full CRUD over their keyframes.
//!
//! ```text
//! AnimationChannel
//! └── ChannelSource
//!     ├── Constant(f32)
//!     ├── Keyframes(KeyframeCurve)
//!     ├── Expression(placeholder)
//!     ├── NodeOutput(NodeId, OutputPortIndex)
//!     ├── AudioReactive(placeholder)
//!     └── Blend(left, right, BlendMode, factor)
//! ```

pub mod blend;
pub mod channel;
pub mod curve;
pub mod interpolation;

pub use blend::BlendMode;
pub use channel::{
    AnimationChannel, AudioReactivePlaceholder, ChannelSource, ExpressionPlaceholder,
};
pub use curve::{Keyframe, KeyframeCurve};
pub use interpolation::Interpolation;

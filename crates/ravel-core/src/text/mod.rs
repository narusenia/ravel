// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text for the `text.*` nodes (REQ-MOGRAPH-004).
//!
//! Split by the question each half answers, because only the first one has
//! anything to do with the host machine:
//!
//! - [`font`] — *which face, and where are its bytes*. Face indexing,
//!   selection, and the caches that make a resolved [`FontRef`] shared
//!   (typography-plan unit 1).
//! - [`layout`] — *where does each character go*. Shaping, line breaking,
//!   glyph outlines, and the per-character instance geometry the rasterizer
//!   stamps (typography-plan unit 2).
//!
//! Everything here is pure Rust and touches no platform API, which is what
//! lets `ravel-cli` render text without linking a font or window library
//! (`AGENTS.md`, the two shipped binaries).

mod font;
mod layout;

pub use font::{
    DEFAULT_FAMILY, FONT_STYLES, FONT_WEIGHTS, FontLibrary, FontQuery, FontRef, shared,
    style_is_italic, weight_from_name,
};
pub use layout::{
    Align, DEFAULT_SIZE, LayoutParams, LayoutTiming, TEXT_ALIGNS, TEXT_ANCHORS, TextError,
    VerticalAnchor, layout_text, layout_text_timed,
};

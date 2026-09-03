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
//!
//! Everything here is pure Rust and touches no platform API, which is what
//! lets `ravel-cli` render text without linking a font or window library
//! (`AGENTS.md`, the two shipped binaries).

mod font;

pub use font::{
    DEFAULT_FAMILY, FONT_STYLES, FONT_WEIGHTS, FontLibrary, FontQuery, FontRef, shared,
    style_is_italic, weight_from_name,
};

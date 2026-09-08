// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The per-node load readout's two loud steps.
//!
//! **A scale, not palette steps** (UX invariant 12's named exception). The
//! readout answers "which node is costing me the frame", and the answer is read
//! by *rank*: muted, then yellow, then red. `muted_foreground` supplies the
//! quiet end, because a node inside budget is chrome. The other two have to be
//! the two the eye already sorts, and the theme models neither — `danger` is
//! Ravel's "this failed" colour, which is a different statement from "this is
//! slow", and there is no amber at all.
//!
//! They used to come from a literal beside their painter; they are here so the
//! exception is one line in `scripts/lint-patterns.allow`.

use gpui::{Hsla, hsla};

/// Over the warn threshold: yellow, the middle rank.
pub const WARN_COLOR: Hsla = hsla(0.13, 0.90, 0.60, 1.0);

/// Over the critical threshold: red, the top rank.
pub const CRITICAL_COLOR: Hsla = hsla(0.0, 0.85, 0.60, 1.0);

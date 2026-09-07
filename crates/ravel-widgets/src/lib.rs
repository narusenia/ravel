// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's own widget layer.
//!
//! Today the crate holds only [`tokens`] — the single source for the colors,
//! spacing, row heights, typography, motion and radii the UI is built from. The
//! widgets themselves move here in later units
//! (`docs/implementation/ui-component-layer-plan.md`, `UIX-2` and `UIX-4`).
//!
//! **This crate must not depend on `gpui-component`.** Ravel's theme schema is
//! authoritative and gpui-component's `ThemeConfig` is derived from it by the
//! application host, so the dependency edge only ever points that way. A
//! borrowed component (an `Input`, the window `Root`) is wired up in
//! `ravel-app`, never here.

pub mod tokens;

pub use tokens::{
    Colors, Motion, Radii, RavelTheme, Rows, Spacing, ThemeFile, ThemeMode, ThemeSpec, Typography,
    hex_color_string, parse_hex_color,
};

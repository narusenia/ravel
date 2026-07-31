// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared multi-window types.
//!
//! The workspace is a set of windows, each hosting one layout tree; the
//! model for that lives in [`crate::layout`] ([`crate::layout::WorkspaceLayout`]
//! owns window lifecycle — detach, close, absorb). This module holds the
//! cross-cutting types both the model and the host need: logical window
//! identifiers and on-desktop placement records.

use serde::{Deserialize, Serialize};

/// Opaque identifier for a window, unique within a
/// [`crate::layout::WorkspaceLayout`]. The host keeps the mapping to real OS
/// window handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub u64);

/// On-desktop placement of a window, in logical pixels.
///
/// Recorded so multi-monitor arrangements can be restored on next launch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// X position of the top-left corner.
    pub x: f32,
    /// Y position of the top-left corner.
    pub y: f32,
    /// Window width.
    pub width: f32,
    /// Window height.
    pub height: f32,
}

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

impl WindowPlacement {
    /// Smallest edge length a restored window is opened at. A record below it
    /// is treated as unusable rather than opening a window the user cannot
    /// find or grab.
    pub const MIN_SIZE: f32 = 240.0;

    /// Whether this record can be turned into real window bounds.
    ///
    /// A persisted placement is plain text on disk: it can be hand-edited,
    /// truncated, or written by a build with a different notion of the
    /// screen. Restoring is therefore gated on the record describing a window
    /// that can actually be opened — a caller that gets `false` falls back to
    /// its default size instead of failing the launch.
    pub fn is_usable(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= Self::MIN_SIZE
            && self.height >= Self::MIN_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(x: f32, y: f32, width: f32, height: f32) -> WindowPlacement {
        WindowPlacement {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn an_ordinary_placement_is_usable() {
        assert!(placement(120.0, 64.0, 1280.0, 800.0).is_usable());
        // Negative origins are legitimate on a multi-monitor desktop.
        assert!(placement(-1920.0, -200.0, 640.0, 480.0).is_usable());
    }

    #[test]
    fn degenerate_and_non_finite_placements_are_rejected() {
        assert!(!placement(0.0, 0.0, 0.0, 0.0).is_usable());
        assert!(!placement(0.0, 0.0, 1280.0, -800.0).is_usable());
        assert!(!placement(0.0, 0.0, 100.0, 800.0).is_usable());
        assert!(!placement(f32::NAN, 0.0, 1280.0, 800.0).is_usable());
        assert!(!placement(0.0, f32::INFINITY, 1280.0, 800.0).is_usable());
        assert!(!placement(0.0, 0.0, f32::NAN, 800.0).is_usable());
    }
}

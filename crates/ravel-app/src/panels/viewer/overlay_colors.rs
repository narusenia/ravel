// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Viewer overlays' colours, in one place.
//!
//! **These are functional colour, not palette steps** (UX invariant 12's named
//! exception). Every value here exists to make one overlay distinguishable from
//! the others that are routinely on screen with it: the selection blue from the
//! geometry warm, a snap guide's magenta from a user guide's cyan, an anchor
//! from a scale handle. A theme step cannot do that job — the whole point is
//! that two marks drawn over the same composition stay telling apart — and
//! deriving them from `primary` / `accent` would collapse the distinctions the
//! doc comments below name.
//!
//! They live in one module rather than beside their painters so the exception
//! is one line in `scripts/lint-patterns.allow` instead of six, and so the
//! "reads as neither X nor Y" claims can be checked by reading one screen.

use gpui::{Hsla, hsla};

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Accent used by both selection bboxes.
pub(super) const SELECTION_COLOR: Hsla = hsla(0.58, 0.7, 0.6, 0.9);

/// Inner fill of a two-square handle mark.
pub(super) const HANDLE_FILL: Hsla = hsla(0.0, 0.0, 1.0, 1.0);

/// The rotation ring: the selection accent held back so the ring reads as a
/// zone around the corner rather than as another grip.
pub(super) const ROTATE_RING_COLOR: Hsla = hsla(0.58, 0.7, 0.6, 0.4);

/// Anchor marker colour: warm, so it never reads as one of the blue scale
/// handles.
pub(super) const ANCHOR_COLOR: Hsla = hsla(0.09, 0.9, 0.6, 0.95);

/// The line from a child's anchor to its parent's.
pub(super) const PARENT_LINK_COLOR: Hsla = hsla(0.09, 0.5, 0.6, 0.55);

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Point and path marks: a warmer accent than the bbox, so a dense point cloud
/// stays distinguishable from the outline around it.
pub(super) const GEOMETRY_MARK_COLOR: Hsla = hsla(0.12, 0.85, 0.62, 0.9);

/// Attribute arrows: cool where the point marks are warm, so an arrow reads as
/// a separate thing from the element it leaves.
pub(super) const ARROW_COLOR: Hsla = hsla(0.45, 0.85, 0.62, 0.95);

/// The colour of the group named `name`.
///
/// Derived from the name, not from the group's position in a list, so a group
/// keeps its colour when another one appears beside it or when the same group
/// exists on both drawn domains, and no table has to be maintained.
///
/// **Three axes, not one.** Telling groups apart is the whole point of colouring
/// them, so two names sharing a colour defeats the feature — and hue alone has
/// only 360 buckets. Distinct colours produced, measured rather than assumed:
///
/// | group names | hue only | hue + saturation + lightness |
/// |---|---|---|
/// | 26 (single letters) | 26 | 26 |
/// | 100 (`g0`…`g99`) | 92 | 100 |
/// | 500 (`g0`…`g499`) | 258 | 500 |
///
/// A splitmix finalizer over the hash was measured too and bought nothing at any
/// plausible group count (it only separates 994 of 1000 names against 984), so it
/// is not carried.
pub(super) fn group_color(name: &str) -> Hsla {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in name.as_bytes() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(1_099_511_628_211);
    }
    hsla(
        (hash % 720) as f32 / 720.0,
        // Kept inside a legible band: every combination has to read as a mark
        // over both the composition and the point cloud around it.
        0.55 + ((hash >> 32) & 0x7) as f32 * 0.05,
        0.45 + ((hash >> 40) & 0x7) as f32 * 0.04,
        0.9,
    )
}

// ---------------------------------------------------------------------------
// Guides
// ---------------------------------------------------------------------------

/// The user guide colour: cyan, distinct from the snap guide's magenta. A snap
/// guide reports a correction that is happening now; a user guide is a standing
/// mark, and the two are routinely on screen together.
pub(super) const USER_GUIDE_COLOR: Hsla = hsla(0.5, 0.85, 0.6, 0.85);

/// The snap guide colour: magenta, so it reads as neither the selection blue,
/// the geometry warm, nor the safe-area grey it is drawn over.
pub(super) const SNAP_GUIDE_COLOR: Hsla = hsla(0.85, 0.9, 0.65, 0.9);

// ---------------------------------------------------------------------------
// Motion path
// ---------------------------------------------------------------------------

/// The trajectory: dimmer than the selection accent, because it is context for
/// the layer rather than a thing being pointed at.
pub(super) const MOTION_PATH_COLOR: Hsla = hsla(0.58, 0.45, 0.75, 0.7);

/// The key marks, in the selection accent: these are grabbable.
pub(super) const MOTION_KEY_COLOR: Hsla = hsla(0.58, 0.7, 0.6, 0.95);

// ---------------------------------------------------------------------------
// Field visualisation
// ---------------------------------------------------------------------------

/// The heat ramp: blue at 0, red at 1.
///
/// A ramp rather than a colour, because what it encodes is a *magnitude* — the
/// reader has to be able to compare two samples by eye, which no pair of theme
/// steps supports. `value` is already normalised to `0..=1`.
pub(super) fn field_heat(value: f32, alpha: f32) -> Hsla {
    // 0.66 (blue) down to 0.0 (red).
    hsla((1.0 - value) * 0.66, 0.85, 0.5, alpha)
}

/// The grayscale ramp: the sample's magnitude as lightness, for a field read
/// against a coloured composition where a hue ramp would fight it.
pub(super) fn field_grayscale(value: f32, alpha: f32) -> Hsla {
    hsla(0.0, 0.0, value, alpha)
}

// ---------------------------------------------------------------------------
// Frame marks
// ---------------------------------------------------------------------------

/// The safe-area and centre lines: white held well back, so one line stays
/// readable over both the black frame and bright content without ever competing
/// with the picture it measures.
pub(super) const SAFE_AREA_LINE_COLOR: Hsla = hsla(0.0, 0.0, 1.0, 0.3);

// ---------------------------------------------------------------------------
// Transparency
// ---------------------------------------------------------------------------

/// The two cells of the transparency checkerboard.
///
/// **Neutral by requirement, not by taste.** The checkerboard is what the user
/// judges the composition's own colours against, so a theme-tinted board would
/// make every semi-transparent pixel read wrong — which is why every compositor
/// draws the same two mid greys whatever its chrome does.
pub(super) const CHECKER_CELLS: [Hsla; 2] =
    [hsla(0.0, 0.0, 0.290, 1.0), hsla(0.0, 0.0, 0.439, 1.0)];

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure pixel math for split rendering, splitter drags, and tab drop zones.
//!
//! The ratio stored in a [`ravel_ui::layout::LayoutNode::Split`] is the
//! fraction of the split container's axis length given to the first child —
//! the same semantics as GPUI's `relative(ratio)` length used to render the
//! children, so drag math and rendering agree exactly. The separator is
//! carved out of the remainder.

use ravel_ui::layout::Orientation;

/// Hit width (or height) of the draggable separator between two panes, in
/// logical pixels.
pub const SPLITTER_PX: f32 = 5.0;

/// Thickness of the visible separator line centered inside the hit area.
pub const SEPARATOR_PX: f32 = 1.0;

/// Smallest ratio a drag can produce. Keeps both panes reachable and the
/// model's `(0.0, 1.0)` ratio invariant satisfied.
pub const MIN_RATIO: f32 = 0.05;

/// Fraction of an area's width (or height) that each edge reserves as a split
/// drop zone. A quarter per edge leaves the middle half of both axes as the
/// merge zone.
pub const DROP_EDGE_FRACTION: f32 = 0.25;

/// Ratio given to a split created by a tab drop or an area menu action. Both
/// panes start at the same size; the user resizes from there.
pub const DEFAULT_SPLIT_RATIO: f32 = 0.5;

/// Where a dragged tab would land inside the area under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropZone {
    /// The middle of the area: the tab joins the area's tab strip.
    Center,
    /// The left quarter: the tab becomes a new area left of this one.
    Left,
    /// The right quarter: the tab becomes a new area right of this one.
    Right,
    /// The top quarter: the tab becomes a new area above this one.
    Top,
    /// The bottom quarter: the tab becomes a new area below this one.
    Bottom,
}

impl DropZone {
    /// The orientation of the split this zone creates, or `None` for
    /// [`DropZone::Center`], which merges instead of splitting.
    pub fn orientation(self) -> Option<Orientation> {
        match self {
            DropZone::Center => None,
            DropZone::Left | DropZone::Right => Some(Orientation::Horizontal),
            DropZone::Top | DropZone::Bottom => Some(Orientation::Vertical),
        }
    }

    /// `true` when the dropped tab becomes the leading (left or top) child of
    /// the split this zone creates.
    pub fn leads(self) -> bool {
        matches!(self, DropZone::Left | DropZone::Top)
    }
}

/// Resolves the drop zone for a pointer at `x`, `y` local to an area of
/// `width` × `height` logical pixels.
///
/// Each edge owns the outer [`DROP_EDGE_FRACTION`] of its axis, measured as a
/// half-open band: a pointer exactly on the `0.25` line already belongs to the
/// center. Overlapping corners go to the nearer edge, ties resolving in
/// left, right, top, bottom order. A degenerate area is all center.
pub fn drop_zone(width: f32, height: f32, x: f32, y: f32) -> DropZone {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return DropZone::Center;
    }
    if !x.is_finite() || !y.is_finite() {
        return DropZone::Center;
    }
    let left = (x / width).clamp(0.0, 1.0);
    let right = 1.0 - left;
    let top = (y / height).clamp(0.0, 1.0);
    let bottom = 1.0 - top;
    let nearest = left.min(right).min(top).min(bottom);
    if nearest >= DROP_EDGE_FRACTION {
        DropZone::Center
    } else if nearest == left {
        DropZone::Left
    } else if nearest == right {
        DropZone::Right
    } else if nearest == top {
        DropZone::Top
    } else {
        DropZone::Bottom
    }
}

/// The highlight rectangle to draw for `zone`, as `(left, top, width, height)`
/// fractions of the area. Fractions feed GPUI's `relative()` length directly,
/// so the highlight matches [`drop_zone`] without a second pixel calculation.
pub fn drop_highlight(zone: DropZone) -> (f32, f32, f32, f32) {
    let edge = DROP_EDGE_FRACTION;
    match zone {
        DropZone::Center => (0.0, 0.0, 1.0, 1.0),
        DropZone::Left => (0.0, 0.0, edge, 1.0),
        DropZone::Right => (1.0 - edge, 0.0, edge, 1.0),
        DropZone::Top => (0.0, 0.0, 1.0, edge),
        DropZone::Bottom => (0.0, 1.0 - edge, 1.0, edge),
    }
}

/// The separator thickness to use in a container spanning `total` px along the
/// split axis.
///
/// [`SPLITTER_PX`] is the comfortable hit size, but a container narrower than
/// twice that would be mostly separator, so the thickness is capped at half
/// the container. Rendering and [`ratio_from_position`] are fed the same
/// value, so a clamped separator still drags exactly.
pub fn splitter_thickness(total: f32) -> f32 {
    if !total.is_finite() || total <= 0.0 {
        return 0.0;
    }
    SPLITTER_PX.min(total * 0.5)
}

/// Splits `total` px of container axis length between the two children of a
/// split node. The first child gets `ratio * total`; the separator takes
/// `splitter` px; the second child gets what remains.
pub fn split_sizes(total: f32, splitter: f32, ratio: f32) -> (f32, f32) {
    let total = total.max(0.0);
    let splitter = splitter.clamp(0.0, total);
    let first = total * ratio.clamp(0.0, 1.0);
    let second = (total - splitter - first).max(0.0);
    (first, second)
}

/// The ratio a splitter drag produces when the pointer sits at `pointer` on
/// the split axis (same coordinate space as `origin`). `origin` and `len` are
/// the split container's axis origin and length. The result is clamped to
/// `[MIN_RATIO, 1.0 - MIN_RATIO]`; a zero-length container yields `0.5`.
///
/// This is the exact inverse of [`split_sizes`] for the separator center:
/// dragging the separator center to `pointer` yields the ratio that renders
/// the separator center at `pointer`.
pub fn ratio_from_position(origin: f32, len: f32, splitter: f32, pointer: f32) -> f32 {
    if !len.is_finite() || len <= 0.0 {
        return 0.5;
    }
    let center_offset = splitter / 2.0;
    ((pointer - origin - center_offset) / len).clamp(MIN_RATIO, 1.0 - MIN_RATIO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sizes_give_first_child_the_ratio_fraction() {
        let (first, second) = split_sizes(1000.0, SPLITTER_PX, 0.5);
        assert_eq!(first, 500.0);
        assert_eq!(second, 495.0);
        assert!((first + SPLITTER_PX + second - 1000.0).abs() < f32::EPSILON);
    }

    #[test]
    fn split_sizes_clamp_degenerate_inputs() {
        assert_eq!(split_sizes(0.0, SPLITTER_PX, 0.5), (0.0, 0.0));
        assert_eq!(split_sizes(-100.0, SPLITTER_PX, 0.5), (0.0, 0.0));
        // The separator can never make the second child negative.
        assert_eq!(split_sizes(4.0, SPLITTER_PX, 1.0), (4.0, 0.0));
        let (first, second) = split_sizes(100.0, SPLITTER_PX, 1.5);
        assert_eq!((first, second), (100.0, 0.0), "ratio clamps to 1.0");
    }

    #[test]
    fn ratio_from_position_inverts_split_sizes_at_separator_center() {
        let (origin, len) = (120.0, 800.0);
        for ratio in [0.1, 0.25, 0.5, 0.75, 0.9] {
            let (first, _) = split_sizes(len, SPLITTER_PX, ratio);
            let center = origin + first + SPLITTER_PX / 2.0;
            let got = ratio_from_position(origin, len, SPLITTER_PX, center);
            assert!(
                (got - ratio).abs() < 1e-5,
                "ratio {ratio} round-tripped to {got}"
            );
        }
    }

    #[test]
    fn ratio_from_position_clamps_to_drag_bounds() {
        assert_eq!(
            ratio_from_position(0.0, 1000.0, SPLITTER_PX, -500.0),
            MIN_RATIO
        );
        assert_eq!(
            ratio_from_position(0.0, 1000.0, SPLITTER_PX, 5000.0),
            1.0 - MIN_RATIO
        );
    }

    #[test]
    fn ratio_from_position_handles_zero_length_container() {
        assert_eq!(ratio_from_position(0.0, 0.0, SPLITTER_PX, 10.0), 0.5);
    }

    /// The area used for the drop-zone boundary cases: 400 × 200 px, so the
    /// edge bands are 100 px wide and 50 px tall.
    const W: f32 = 400.0;
    const H: f32 = 200.0;

    #[test]
    fn drop_zone_center_covers_the_middle_half_of_both_axes() {
        assert_eq!(drop_zone(W, H, 200.0, 100.0), DropZone::Center);
        // Exactly on the boundary lines: the band is half-open, so these are
        // already center.
        assert_eq!(drop_zone(W, H, 100.0, 100.0), DropZone::Center);
        assert_eq!(drop_zone(W, H, 300.0, 100.0), DropZone::Center);
        assert_eq!(drop_zone(W, H, 200.0, 50.0), DropZone::Center);
        assert_eq!(drop_zone(W, H, 200.0, 150.0), DropZone::Center);
    }

    #[test]
    fn drop_zone_edges_start_just_inside_the_boundary() {
        assert_eq!(drop_zone(W, H, 99.0, 100.0), DropZone::Left);
        assert_eq!(drop_zone(W, H, 301.0, 100.0), DropZone::Right);
        assert_eq!(drop_zone(W, H, 200.0, 49.0), DropZone::Top);
        assert_eq!(drop_zone(W, H, 200.0, 151.0), DropZone::Bottom);
    }

    #[test]
    fn drop_zone_corners_pick_the_nearer_edge() {
        // Top-left corner of a wide area: 10 px from the top, 40 px from the
        // left, so the top edge is nearer in fraction terms (0.05 vs 0.1).
        assert_eq!(drop_zone(W, H, 40.0, 10.0), DropZone::Top);
        // Same corner, but now the left edge is the nearer one.
        assert_eq!(drop_zone(W, H, 8.0, 20.0), DropZone::Left);
        // Bottom-right corner.
        assert_eq!(drop_zone(W, H, 392.0, 180.0), DropZone::Right);
        assert_eq!(drop_zone(W, H, 360.0, 190.0), DropZone::Bottom);
        // Exact corner: the documented tie order puts left first.
        assert_eq!(drop_zone(W, H, 0.0, 0.0), DropZone::Left);
    }

    #[test]
    fn drop_zone_handles_degenerate_areas_and_positions() {
        assert_eq!(drop_zone(0.0, 0.0, 0.0, 0.0), DropZone::Center);
        assert_eq!(drop_zone(-10.0, H, 5.0, 5.0), DropZone::Center);
        assert_eq!(drop_zone(f32::NAN, H, 5.0, 5.0), DropZone::Center);
        assert_eq!(drop_zone(W, H, f32::NAN, 5.0), DropZone::Center);
        // Positions outside the area clamp to the nearest edge band.
        assert_eq!(drop_zone(W, H, -50.0, 100.0), DropZone::Left);
        assert_eq!(drop_zone(W, H, 500.0, 100.0), DropZone::Right);
    }

    #[test]
    fn drop_zone_orientation_and_lead_match_the_edge() {
        use Orientation::{Horizontal, Vertical};
        assert_eq!(DropZone::Center.orientation(), None);
        assert_eq!(DropZone::Left.orientation(), Some(Horizontal));
        assert_eq!(DropZone::Right.orientation(), Some(Horizontal));
        assert_eq!(DropZone::Top.orientation(), Some(Vertical));
        assert_eq!(DropZone::Bottom.orientation(), Some(Vertical));
        assert!(DropZone::Left.leads());
        assert!(DropZone::Top.leads());
        assert!(!DropZone::Right.leads());
        assert!(!DropZone::Bottom.leads());
        assert!(!DropZone::Center.leads());
    }

    #[test]
    fn drop_highlight_rectangles_match_the_zone_bands() {
        assert_eq!(drop_highlight(DropZone::Center), (0.0, 0.0, 1.0, 1.0));
        assert_eq!(drop_highlight(DropZone::Left), (0.0, 0.0, 0.25, 1.0));
        assert_eq!(drop_highlight(DropZone::Right), (0.75, 0.0, 0.25, 1.0));
        assert_eq!(drop_highlight(DropZone::Top), (0.0, 0.0, 1.0, 0.25));
        assert_eq!(drop_highlight(DropZone::Bottom), (0.0, 0.75, 1.0, 0.25));
        // Every rectangle stays inside the area.
        for zone in [
            DropZone::Center,
            DropZone::Left,
            DropZone::Right,
            DropZone::Top,
            DropZone::Bottom,
        ] {
            let (x, y, w, h) = drop_highlight(zone);
            assert!(x >= 0.0 && y >= 0.0 && x + w <= 1.0 && y + h <= 1.0);
        }
    }

    #[test]
    fn splitter_thickness_never_exceeds_half_the_container() {
        assert_eq!(splitter_thickness(1000.0), SPLITTER_PX);
        assert_eq!(splitter_thickness(10.0), SPLITTER_PX);
        assert_eq!(splitter_thickness(8.0), 4.0);
        assert_eq!(splitter_thickness(1.0), 0.5);
        assert_eq!(splitter_thickness(0.0), 0.0);
        assert_eq!(splitter_thickness(-5.0), 0.0);
        assert_eq!(splitter_thickness(f32::NAN), 0.0);
    }

    #[test]
    fn clamped_splitter_still_inverts_split_sizes() {
        let total = 8.0;
        let thickness = splitter_thickness(total);
        let (first, _) = split_sizes(total, thickness, 0.5);
        let center = first + thickness / 2.0;
        let got = ratio_from_position(0.0, total, thickness, center);
        assert!((got - 0.5).abs() < 1e-5, "round-tripped to {got}");
    }

    #[test]
    fn drag_ratios_stay_model_valid() {
        for pointer in [-1000.0, 0.0, 250.0, 500.0, 1000.0, 2000.0] {
            let ratio = ratio_from_position(0.0, 1000.0, SPLITTER_PX, pointer);
            assert!(
                ratio > 0.0 && ratio < 1.0,
                "ratio {ratio} must be in (0, 1)"
            );
        }
    }
}

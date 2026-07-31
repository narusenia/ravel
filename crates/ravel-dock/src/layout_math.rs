// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure pixel math for split rendering and splitter drags.
//!
//! The ratio stored in a [`ravel_ui::layout::LayoutNode::Split`] is the
//! fraction of the split container's axis length given to the first child —
//! the same semantics as GPUI's `relative(ratio)` length used to render the
//! children, so drag math and rendering agree exactly. The separator is
//! carved out of the remainder.

/// Hit width (or height) of the draggable separator between two panes, in
/// logical pixels.
pub const SPLITTER_PX: f32 = 5.0;

/// Thickness of the visible separator line centered inside the hit area.
pub const SEPARATOR_PX: f32 = 1.0;

/// Smallest ratio a drag can produce. Keeps both panes reachable and the
/// model's `(0.0, 1.0)` ratio invariant satisfied.
pub const MIN_RATIO: f32 = 0.05;

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

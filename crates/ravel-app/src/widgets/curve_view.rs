// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! View state shared by every curve editor: the visible value range and the
//! grid ticks drawn for it.
//!
//! Both curve editors — the Timeline's keyframe graph
//! ([`super::curve_editor`]) and the Properties inline editor for curve
//! parameters ([`super::param_curve_editor`]) — need the same three things:
//! an automatic range fitted to the data, an optional range the user pinned
//! instead, and "nice" tick values for whatever range ends up visible. Keeping
//! one implementation is what makes zooming, fitting, and the grid behave
//! identically in both places.
//!
//! The range is **view state**. It never reaches the Document, so it records
//! no undo step and undo never changes it.

/// Fraction of the data span left as empty margin when a range is fitted.
pub const VALUE_MARGIN_RATIO: f64 = 0.08;
/// Half-height of the range a completely flat curve is fitted into.
pub const DEGENERATE_MARGIN: f64 = 0.5;
/// Pixels a grid line aims to be apart.
pub const GRID_TARGET_PX: f64 = 48.0;
/// Narrowest and widest span a pinned range may hold, so a zoom gesture
/// cannot collapse the axis or run off into infinity.
const MIN_SPAN: f64 = 1.0e-6;
const MAX_SPAN: f64 = 1.0e9;

/// `min..max` widened by the standard margin.
///
/// A span of zero (a flat curve, a single control point) has no extent to take
/// a proportional margin from, so it falls back to [`DEGENERATE_MARGIN`],
/// scaled up for values far from the origin.
pub fn padded_bounds(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    let (min, max) = if min <= max { (min, max) } else { (max, min) };
    let span = max - min;
    let margin = if span <= f64::EPSILON {
        DEGENERATE_MARGIN.max(min.abs() * VALUE_MARGIN_RATIO)
    } else {
        span * VALUE_MARGIN_RATIO
    };
    (min - margin, max + margin)
}

/// Tick values inside `min..=max` at a round step, aiming for one line every
/// [`GRID_TARGET_PX`] of `extent`.
pub fn value_grid_values(min: f64, max: f64, extent: f64) -> Vec<f64> {
    grid_values(min, max, extent, GRID_TARGET_PX)
}

/// [`value_grid_values`] with an explicit target spacing, for axes that need
/// to thin out more (a narrow inline editor's horizontal axis carries wider
/// labels than the vertical one).
pub fn grid_values(min: f64, max: f64, extent: f64, target_px: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || max <= min || extent <= 0.0 {
        return Vec::new();
    }
    let target_lines = (extent / target_px.max(1.0)).max(1.0);
    let step = nice_value_step((max - min) / target_lines);
    if !step.is_finite() || step <= 0.0 {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut value = (min / step).ceil() * step;
    while value <= max && values.len() < 128 {
        values.push(if value.abs() < step * 1.0e-9 {
            0.0
        } else {
            value
        });
        value += step;
    }
    values
}

/// `raw` rounded up to the next 1 / 2 / 5 × 10ⁿ step.
pub fn nice_value_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10.0_f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// Axis label text: exponent form for the extremes, otherwise a fixed number
/// of decimals chosen by magnitude.
pub fn format_value_label(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000.0 || (abs > 0.0 && abs < 0.01) {
        format!("{value:.1e}")
    } else if abs >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// One axis of a curve editor's visible range.
///
/// Either it follows the data (`auto`, the default) or it holds a range the
/// user pinned by zooming or by typing bounds. "Fit" is exactly "go back to
/// following the data", which is why fitting can never lose a control point:
/// the automatic range is derived from all of them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurveValueRange {
    pinned: Option<(f64, f64)>,
}

impl CurveValueRange {
    /// A range that follows the data.
    pub fn auto() -> Self {
        Self::default()
    }

    /// A range pinned to `min..max`.
    pub fn pinned(min: f64, max: f64) -> Self {
        let mut range = Self::auto();
        range.set(min, max);
        range
    }

    /// Whether the range still follows the data.
    pub fn is_auto(self) -> bool {
        self.pinned.is_none()
    }

    /// The pinned bounds, if any.
    pub fn bounds(self) -> Option<(f64, f64)> {
        self.pinned
    }

    /// The range to draw: the pinned one, or `auto` while following the data.
    pub fn resolved(self, auto: (f64, f64)) -> (f64, f64) {
        self.pinned.unwrap_or(auto)
    }

    /// Follow the data again. This is the Fit operation: a point dragged out
    /// of view is always reachable again through it.
    pub fn fit(&mut self) {
        self.pinned = None;
    }

    /// Pin `min..max`, ordering the bounds and refusing a degenerate or
    /// non-finite span (which would divide by zero in the transform).
    pub fn set(&mut self, min: f64, max: f64) -> bool {
        if !min.is_finite() || !max.is_finite() {
            return false;
        }
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        let span = max - min;
        if !(MIN_SPAN..=MAX_SPAN).contains(&span) {
            return false;
        }
        self.pinned = Some((min, max));
        true
    }

    /// Scale the visible span by `factor` about `focus`, a 0..1 position
    /// across the current range (0 = the `max` end, matching a widget's top
    /// edge). Zooming pins the range, so the axis stops following the data
    /// until it is fitted again.
    pub fn zoom(&mut self, auto: (f64, f64), factor: f64, focus: f64) -> bool {
        if !factor.is_finite() || factor <= 0.0 {
            return false;
        }
        let (min, max) = self.resolved(auto);
        let span = max - min;
        if !span.is_finite() || span <= 0.0 {
            return false;
        }
        let focus = focus.clamp(0.0, 1.0);
        let anchor = max - span * focus;
        let next = (span * factor).clamp(MIN_SPAN, MAX_SPAN);
        self.set(anchor - next * (1.0 - focus), anchor + next * focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_range_follows_the_data() {
        let range = CurveValueRange::auto();
        assert!(range.is_auto());
        assert_eq!(range.resolved((-1.0, 1.0)), (-1.0, 1.0));
        assert_eq!(range.resolved((3.0, 9.0)), (3.0, 9.0), "and keeps doing so");
    }

    #[test]
    fn pinning_overrides_the_data_until_it_is_fitted() {
        let mut range = CurveValueRange::auto();
        assert!(range.set(0.0, 2.0));
        assert!(!range.is_auto());
        assert_eq!(range.resolved((-100.0, 100.0)), (0.0, 2.0));
        range.fit();
        assert_eq!(range.resolved((-100.0, 100.0)), (-100.0, 100.0));
    }

    #[test]
    fn a_reversed_range_is_ordered_and_a_degenerate_one_refused() {
        let mut range = CurveValueRange::auto();
        assert!(range.set(5.0, 1.0));
        assert_eq!(range.bounds(), Some((1.0, 5.0)));
        assert!(!range.set(2.0, 2.0), "zero span would divide by zero");
        assert!(!range.set(f64::NAN, 1.0));
        assert_eq!(range.bounds(), Some((1.0, 5.0)), "a refused set is inert");
    }

    /// Zooming keeps the value under the focus point where it was — that is
    /// what makes a wheel zoom feel anchored to the pointer.
    #[test]
    fn zooming_holds_the_focused_value_in_place() {
        let mut range = CurveValueRange::pinned(0.0, 10.0);
        // focus 0.25 from the top => the value 7.5.
        assert!(range.zoom((0.0, 10.0), 0.5, 0.25));
        let (min, max) = range.bounds().expect("pinned");
        assert!((max - min - 5.0).abs() < 1e-9, "span halved: {min}..{max}");
        assert!((max - 0.25 * (max - min) - 7.5).abs() < 1e-9);
    }

    #[test]
    fn zooming_an_auto_range_starts_from_the_data_and_pins_it() {
        let mut range = CurveValueRange::auto();
        assert!(range.zoom((0.0, 4.0), 0.5, 0.5));
        assert!(!range.is_auto());
        let (min, max) = range.bounds().expect("pinned");
        assert!(
            (min - 1.0).abs() < 1e-9 && (max - 3.0).abs() < 1e-9,
            "{min}..{max}"
        );
    }

    #[test]
    fn zooming_cannot_collapse_or_explode_the_axis() {
        let mut range = CurveValueRange::pinned(0.0, 1.0);
        for _ in 0..200 {
            range.zoom((0.0, 1.0), 0.5, 0.5);
        }
        let (min, max) = range.bounds().expect("pinned");
        assert!(max - min >= MIN_SPAN * 0.5, "{min}..{max}");
        for _ in 0..200 {
            range.zoom((0.0, 1.0), 2.0, 0.5);
        }
        let (min, max) = range.bounds().expect("pinned");
        assert!(max - min <= MAX_SPAN, "{min}..{max}");
    }

    #[test]
    fn padding_widens_a_span_and_rescues_a_flat_one() {
        let (min, max) = padded_bounds(0.0, 10.0);
        assert!((min - -0.8).abs() < 1e-9 && (max - 10.8).abs() < 1e-9);
        let (min, max) = padded_bounds(2.0, 2.0);
        assert!(max - min >= DEGENERATE_MARGIN * 2.0);
        assert_eq!(padded_bounds(1.0, 0.0), padded_bounds(0.0, 1.0));
        assert_eq!(padded_bounds(f64::NAN, 1.0), (0.0, 1.0));
    }

    #[test]
    fn grid_ticks_use_round_steps_and_thin_out_with_the_target() {
        assert_eq!(value_grid_values(-1.0, 1.0, 96.0), vec![-1.0, 0.0, 1.0]);
        assert_eq!(nice_value_step(0.24), 0.5);
        assert_eq!(nice_value_step(24.0), 50.0);
        assert!(value_grid_values(1.0, 1.0, 100.0).is_empty());
        // The same extent with a wider target yields no more lines.
        let dense = grid_values(0.0, 1.0, 200.0, 20.0).len();
        let sparse = grid_values(0.0, 1.0, 200.0, 80.0).len();
        assert!(sparse <= dense, "{sparse} vs {dense}");
    }

    #[test]
    fn labels_switch_to_exponent_form_at_the_extremes() {
        assert_eq!(format_value_label(0.5), "0.50");
        assert_eq!(format_value_label(12.0), "12.0");
        assert!(format_value_label(5_000.0).contains('e'));
        assert!(format_value_label(0.0001).contains('e'));
    }
}

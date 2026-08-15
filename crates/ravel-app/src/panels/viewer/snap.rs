// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Drag snapping in the Viewer, and the guide lines that report it.
//!
//! Three properties this carries:
//!
//! - **The rectangle snaps, not the pointer.** A gesture is handed the
//!   composition-space rectangle it is moving and [`snap_delta`] corrects its
//!   delta so an *edge or centre* of that rectangle lands on a candidate.
//!   Snapping the pointer instead would make "align this layer's left edge with
//!   that one's" impossible to express, because the pointer is nowhere near the
//!   edge being aligned.
//! - **The threshold is a screen distance.** A composition-space threshold
//!   would grab from half the canvas away when zoomed out and be unreachable
//!   when zoomed in. Callers convert once through [`comp_threshold`], with the
//!   panel's composition-pixels-per-screen-pixel; the Viewer's zoom is
//!   isotropic, so one factor covers both axes.
//! - **The guide comes from the same result as the delta.** [`SnapResult`]
//!   carries both, so a drawn guide always names the line the delta actually
//!   landed on.
//!
//! The platform's primary modifier — Cmd on macOS, Ctrl elsewhere — suppresses
//! the pull ([`snap_delta`] returns the delta untouched), which is the plan's
//! decision: no toggle and no setting until one is asked for. Not Alt, which
//! already means "draw from the centre" for the shape tools and "scale about
//! the anchor" for the shell grips.

use gpui::Hsla;
use ravel_core::id::{CompId, LayerId};

use super::CompRect;
use super::overlay::{
    DragModifiers, OverlayContext, OverlayId, OverlayPainter, SAFE_AREA_FRACTIONS, ViewerOverlay,
    priority,
};

/// Screen-pixel distance within which a gesture is pulled onto a candidate.
pub const SNAP_THRESHOLD_PX: f32 = 8.0;

/// [`SNAP_THRESHOLD_PX`] in composition units. `comp_per_px` is the panel's
/// composition pixels per screen pixel — the inverse of the zoom — so the pull
/// covers the same distance on screen at every zoom level.
pub fn comp_threshold(comp_per_px: f32) -> f32 {
    SNAP_THRESHOLD_PX * comp_per_px
}

/// The lines a gesture can be pulled onto, in composition space.
///
/// Order is the tie-break: [`snap_delta`] keeps the first line of a group of
/// equally distant ones, so the composition frame wins over the safe areas,
/// which win over the layers, which are read in composition order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapLines {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
}

impl SnapLines {
    /// The candidates a gesture in `comp` sees: the composition frame, the safe
    /// areas while they are drawn, and the bounding box of every layer except
    /// the ones the gesture is moving.
    ///
    /// The moving layers are excluded because their own edges travel with the
    /// gesture: a rectangle is always within zero of itself, so leaving them in
    /// would pin every drag to its start.
    pub fn collect(ctx: &OverlayContext, comp: Option<CompId>, moving: &[LayerId]) -> Self {
        let mut lines = Self::default();
        if let Some((width, height)) = ctx.resolution {
            let (width, height) = (width as f32, height as f32);
            lines.x.extend([0.0, width * 0.5, width]);
            lines.y.extend([0.0, height * 0.5, height]);
            // Only while the rectangles are on screen. A pull towards a line
            // nobody can see is exactly the "it moved on its own" reading the
            // guide line exists to prevent.
            if ctx.show_safe_areas {
                for fraction in SAFE_AREA_FRACTIONS {
                    let (inset_x, inset_y) = (
                        width * (1.0 - fraction) * 0.5,
                        height * (1.0 - fraction) * 0.5,
                    );
                    lines.x.extend([inset_x, width - inset_x]);
                    lines.y.extend([inset_y, height - inset_y]);
                }
            }
        }
        if let (Some(comp), Some(document)) = (comp, ctx.document.as_ref())
            && let Some(composition) = document.get_composition(comp)
        {
            for layer in &composition.layers {
                if moving.contains(&layer.id) {
                    continue;
                }
                // `None` for a layer with nothing measurable — no evaluated
                // geometry, or none at this frame — which is the same "there is
                // no edge here" as a layer that does not exist.
                let Some(rect) = super::layer_comp_rect(ctx, document, comp, layer.id) else {
                    continue;
                };
                lines.push_rect(rect);
            }
        }
        lines
    }

    /// The three lines a rectangle offers per axis: both edges and the centre.
    fn push_rect(&mut self, rect: CompRect) {
        self.x
            .extend([rect.x, rect.x + rect.w * 0.5, rect.x + rect.w]);
        self.y
            .extend([rect.y, rect.y + rect.h * 0.5, rect.y + rect.h]);
    }

    pub fn is_empty(&self) -> bool {
        self.x.is_empty() && self.y.is_empty()
    }
}

/// The lines a snapped gesture landed on this frame, one per axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapGuides {
    /// Composition x of the vertical guide.
    pub x: Option<f32>,
    /// Composition y of the horizontal guide.
    pub y: Option<f32>,
}

impl SnapGuides {
    pub fn is_empty(self) -> bool {
        self.x.is_none() && self.y.is_none()
    }
}

/// A corrected gesture delta and the guides that explain it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapResult {
    pub delta: (f32, f32),
    pub guides: SnapGuides,
}

impl SnapResult {
    /// The delta as the caller gave it, corrected on neither axis.
    pub fn unsnapped(delta: (f32, f32)) -> Self {
        Self {
            delta,
            guides: SnapGuides::default(),
        }
    }

    /// Keep the correction only on the axes the gesture actually writes,
    /// restoring `raw` on the others.
    ///
    /// A gesture whose edit discards an axis — a shell edge grip drives one
    /// only — must not be pulled along it: the guide would name an alignment
    /// the edit cannot make, and a delta corrected on a discarded axis reads as
    /// movement, which is how a gesture that changed nothing ends up committing
    /// an undo step.
    pub fn restrict(mut self, raw: (f32, f32), axes: (bool, bool)) -> Self {
        if !axes.0 {
            self.delta.0 = raw.0;
            self.guides.x = None;
        }
        if !axes.1 {
            self.delta.1 = raw.1;
            self.guides.y = None;
        }
        self
    }
}

/// Pull `rect + delta` onto the nearest candidate within `threshold`, per axis.
///
/// `rect` is the moving element's rectangle as it stood when the gesture
/// pressed; a gesture that moves a single point (a scale grip, a drawing
/// pointer) passes a zero-sized rectangle at that point. The returned delta is
/// the caller's own with the correction folded in, so the gesture stays
/// absolute — repeated calls during one drag never compound.
pub fn snap_delta(
    rect: CompRect,
    delta: (f32, f32),
    lines: &SnapLines,
    threshold: f32,
    modifiers: DragModifiers,
) -> SnapResult {
    // A non-finite delta or threshold has no nearest anything: comparisons
    // against NaN are all false, so the answer would be "no candidate" reached
    // by accident rather than on purpose.
    if modifiers.primary
        || !threshold.is_finite()
        || threshold < 0.0
        || !delta.0.is_finite()
        || !delta.1.is_finite()
    {
        return SnapResult::unsnapped(delta);
    }
    let x = snap_axis(rect.x + delta.0, rect.w, &lines.x, threshold);
    let y = snap_axis(rect.y + delta.1, rect.h, &lines.y, threshold);
    SnapResult {
        delta: (
            delta.0 + x.map_or(0.0, |(adjust, _)| adjust),
            delta.1 + y.map_or(0.0, |(adjust, _)| adjust),
        ),
        guides: SnapGuides {
            x: x.map(|(_, line)| line),
            y: y.map(|(_, line)| line),
        },
    }
}

/// The correction and the line for one axis: the nearest candidate to any of
/// the moving rectangle's low edge, centre and high edge.
///
/// Ties keep the earlier candidate, and within one candidate the earlier edge,
/// which is what makes a drag between two equally distant lines land on the
/// same one every time instead of flickering between them.
fn snap_axis(origin: f32, size: f32, lines: &[f32], threshold: f32) -> Option<(f32, f32)> {
    let edges = [origin, origin + size * 0.5, origin + size];
    let mut best: Option<(f32, f32, f32)> = None;
    for &line in lines {
        if !line.is_finite() {
            continue;
        }
        for edge in edges {
            if !edge.is_finite() {
                continue;
            }
            let distance = (line - edge).abs();
            if distance > threshold {
                continue;
            }
            if best.is_none_or(|(nearest, _, _)| distance < nearest) {
                best = Some((distance, line - edge, line));
            }
        }
    }
    best.map(|(_, adjust, line)| (adjust, line))
}

/// The guide colour: magenta, so it reads as neither the selection blue, the
/// geometry warm, nor the safe-area grey it is drawn over.
const GUIDE_COLOR: Hsla = Hsla {
    h: 0.85,
    s: 0.9,
    l: 0.65,
    a: 0.9,
};

/// The lines the drag in flight is snapped to, for as long as it is snapped.
///
/// It owns no state: the gesture that computed the pull publishes it through
/// [`OverlayContext::snap_guides`], and this draws whatever is there. A gesture
/// that is not snapped this frame publishes nothing and the overlay stands
/// down, which is what makes the guide a report of the correction rather than a
/// decoration that outlives it.
pub struct SnapGuideOverlay;

impl SnapGuideOverlay {
    pub const ID: OverlayId = OverlayId("viewer.snap_guides");
}

impl ViewerOverlay for SnapGuideOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::SNAP_GUIDES
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        !ctx.snap_guides.is_empty() && ctx.resolution.is_some()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        if let Some(x) = ctx.snap_guides.x {
            painter.comp_vrule(x, 1.0, GUIDE_COLOR);
        }
        if let Some(y) = ctx.snap_guides.y {
            painter.comp_hrule(y, 1.0, GUIDE_COLOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> CompRect {
        CompRect { x, y, w, h }
    }

    fn lines(x: Vec<f32>, y: Vec<f32>) -> SnapLines {
        SnapLines { x, y }
    }

    fn plain() -> DragModifiers {
        DragModifiers::default()
    }

    /// The centre of the moving rectangle is pulled onto a candidate 6 units
    /// away, and left alone when the same candidate is 12 away.
    #[test]
    fn a_candidate_inside_the_threshold_pulls_and_one_outside_does_not() {
        let moving = rect(0.0, 0.0, 100.0, 100.0);
        let candidates = lines(vec![1000.0], vec![]);
        let inside = snap_delta(moving, (944.0, 0.0), &candidates, 8.0, plain());
        assert_eq!(inside.delta, (950.0, 0.0), "the centre landed on 1000");
        assert_eq!(inside.guides.x, Some(1000.0));

        let outside = snap_delta(moving, (938.0, 0.0), &candidates, 8.0, plain());
        assert_eq!(outside.delta, (938.0, 0.0), "12 units away is out of reach");
        assert_eq!(outside.guides, SnapGuides::default());
    }

    /// Both edges snap, not only the centre: this is what "align this layer's
    /// left edge with that one's right edge" is made of.
    #[test]
    fn an_edge_snaps_as_well_as_the_centre() {
        let moving = rect(0.0, 0.0, 100.0, 40.0);
        // 300 is 5 from the moving rectangle's right edge (0 + 100 + 195).
        let result = snap_delta(
            moving,
            (195.0, 0.0),
            &lines(vec![300.0], vec![]),
            8.0,
            plain(),
        );
        assert_eq!(result.delta, (200.0, 0.0));
        assert_eq!(result.guides.x, Some(300.0));
    }

    /// The threshold is handed over in composition units, so the same screen
    /// distance snaps at every zoom: at 4x a 2-unit gap is 8px and pulls, while
    /// at 0.25x the same 2-unit gap is half a pixel of a 32-unit reach.
    #[test]
    fn the_threshold_is_a_constant_screen_distance() {
        let moving = rect(0.0, 0.0, 0.0, 0.0);
        let candidates = lines(vec![100.0], vec![]);
        // 4x zoom: one screen pixel is a quarter of a composition unit.
        let zoomed_in = comp_threshold(0.25);
        assert_eq!(zoomed_in, 2.0);
        // Six composition units is 24px at this zoom: out of reach.
        assert_eq!(
            snap_delta(moving, (94.0, 0.0), &candidates, zoomed_in, plain()).delta,
            (94.0, 0.0)
        );
        // 0.25x zoom: one screen pixel is four composition units.
        let zoomed_out = comp_threshold(4.0);
        assert_eq!(zoomed_out, 32.0);
        // The same six units is now a pixel and a half: well inside.
        assert_eq!(
            snap_delta(moving, (94.0, 0.0), &candidates, zoomed_out, plain()).delta,
            (100.0, 0.0)
        );
        // And a gap of exactly 8px snaps at both zooms, which is the invariant
        // itself rather than a consequence of the numbers above.
        for (comp_per_px, gap) in [(0.25f32, 2.0f32), (4.0, 32.0)] {
            let result = snap_delta(
                moving,
                (100.0 - gap, 0.0),
                &candidates,
                comp_threshold(comp_per_px),
                plain(),
            );
            assert_eq!(result.delta, (100.0, 0.0), "8px pulls at {comp_per_px}");
        }
    }

    /// Two candidates at the same distance: the earlier one in the list wins,
    /// every time, whichever side of the rectangle it sits on.
    #[test]
    fn equally_distant_candidates_resolve_to_the_first_one() {
        let moving = rect(0.0, 0.0, 100.0, 0.0);
        // 95 is 5 below the moving left edge, 205 is 5 above the right edge.
        let first = lines(vec![95.0, 205.0], vec![]);
        let second = lines(vec![205.0, 95.0], vec![]);
        let a = snap_delta(moving, (100.0, 0.0), &first, 8.0, plain());
        let b = snap_delta(moving, (100.0, 0.0), &second, 8.0, plain());
        assert_eq!((a.delta, a.guides.x), ((95.0, 0.0), Some(95.0)));
        assert_eq!((b.delta, b.guides.x), ((105.0, 0.0), Some(205.0)));
        // Repeated evaluation of the same input never changes its mind.
        assert_eq!(snap_delta(moving, (100.0, 0.0), &first, 8.0, plain()), a);
    }

    /// A nearer candidate beats an earlier one — the tie-break is only for
    /// distances that are actually equal.
    #[test]
    fn the_nearest_candidate_wins_over_the_earlier_one() {
        let moving = rect(0.0, 0.0, 0.0, 0.0);
        let candidates = lines(vec![106.0, 101.0], vec![]);
        let result = snap_delta(moving, (100.0, 0.0), &candidates, 8.0, plain());
        assert_eq!(result.guides.x, Some(101.0));
    }

    /// The platform's primary modifier suppresses the pull entirely, including
    /// the guide: the drag reports nothing because nothing was corrected.
    ///
    /// Alt does not, and that is the point of the split — a shape drawn from
    /// its centre and a shell scaled about its anchor both hold Alt, and both
    /// keep snapping.
    #[test]
    fn the_primary_modifier_suppresses_the_pull_and_alt_does_not() {
        let moving = rect(0.0, 0.0, 100.0, 100.0);
        let candidates = lines(vec![1000.0], vec![1000.0]);
        let held = DragModifiers {
            primary: true,
            ..DragModifiers::default()
        };
        let result = snap_delta(moving, (944.0, 944.0), &candidates, 8.0, held);
        assert_eq!(result.delta, (944.0, 944.0));
        assert!(result.guides.is_empty());

        // The two constrain / reference-point modifiers leave snapping on.
        // Shift is gated per gesture by the caller, not here.
        for other in [
            DragModifiers {
                alt: true,
                ..DragModifiers::default()
            },
            DragModifiers {
                shift: true,
                ..DragModifiers::default()
            },
        ] {
            assert_eq!(
                snap_delta(moving, (944.0, 944.0), &candidates, 8.0, other).delta,
                (950.0, 950.0),
                "{other:?} is not the suppression key"
            );
        }
    }

    /// The two axes are independent: one can snap while the other does not.
    #[test]
    fn each_axis_snaps_on_its_own() {
        let moving = rect(0.0, 0.0, 0.0, 0.0);
        let candidates = lines(vec![100.0], vec![500.0]);
        let result = snap_delta(moving, (103.0, 200.0), &candidates, 8.0, plain());
        assert_eq!(result.delta, (100.0, 200.0));
        assert_eq!(
            result.guides,
            SnapGuides {
                x: Some(100.0),
                y: None
            }
        );
    }

    /// Non-finite input is left alone rather than compared into a silent
    /// "nothing is near", and a non-finite candidate is skipped without
    /// swallowing the finite ones beside it.
    #[test]
    fn non_finite_values_do_not_snap() {
        /// Bit-for-bit equality, so a NaN that came back unchanged still counts
        /// as unchanged — `==` calls every NaN different from itself.
        fn unchanged(delta: (f32, f32), from: (f32, f32)) -> bool {
            delta.0.to_bits() == from.0.to_bits() && delta.1.to_bits() == from.1.to_bits()
        }

        let moving = rect(0.0, 0.0, 100.0, 100.0);
        let candidates = lines(vec![1000.0], vec![1000.0]);
        for delta in [(f32::NAN, 0.0), (0.0, f32::INFINITY)] {
            let result = snap_delta(moving, delta, &candidates, 8.0, plain());
            assert!(result.guides.is_empty(), "{delta:?} produced a guide");
            assert!(
                unchanged(result.delta, delta),
                "{delta:?} was corrected to {:?}",
                result.delta
            );
        }
        let unusable = snap_delta(moving, (944.0, 0.0), &candidates, f32::NAN, plain());
        assert!(unusable.guides.is_empty());
        assert_eq!(unusable.delta, (944.0, 0.0), "a NaN reach corrects nothing");

        let poisoned = lines(vec![f32::NAN, 1000.0], vec![]);
        let skipped = snap_delta(moving, (944.0, 0.0), &poisoned, 8.0, plain());
        assert_eq!(
            skipped.guides.x,
            Some(1000.0),
            "a NaN candidate is skipped, not treated as nearest"
        );
        assert_eq!(
            skipped.delta,
            (950.0, 0.0),
            "and the finite candidate beside it still pulls"
        );

        let infinite = rect(f32::INFINITY, 0.0, 100.0, 100.0);
        let unmeasurable = snap_delta(infinite, (1.0, 2.0), &candidates, 8.0, plain());
        assert!(
            unmeasurable.guides.x.is_none(),
            "an infinite edge has no distance"
        );
        assert_eq!(
            unmeasurable.delta.0, 1.0,
            "and its axis keeps the delta it was given"
        );
    }

    /// A zero-sized rectangle is the single-point gesture: all three edges are
    /// the same point, so it snaps that point and nothing else.
    #[test]
    fn a_zero_sized_rect_snaps_its_single_point() {
        let point = rect(10.0, 10.0, 0.0, 0.0);
        let result = snap_delta(
            point,
            (0.0, 0.0),
            &lines(vec![15.0], vec![4.0]),
            8.0,
            plain(),
        );
        assert_eq!(result.delta, (5.0, -6.0));
    }

    /// The overlay stands down with nothing to report, and draws a rule per
    /// snapped axis at the composition coordinate the result named.
    #[test]
    fn the_guide_overlay_draws_one_rule_per_snapped_axis() {
        use crate::panels::viewer::overlay::OverlayPrimitive;
        use gpui::{Bounds, point, px, size};

        let idle = OverlayContext {
            resolution: Some((1920, 1080)),
            ..OverlayContext::default()
        };
        assert!(
            !SnapGuideOverlay.is_active(&idle),
            "no correction, no guide"
        );

        let snapped = OverlayContext {
            snap_guides: SnapGuides {
                x: Some(960.0),
                y: None,
            },
            ..idle.clone()
        };
        assert!(SnapGuideOverlay.is_active(&snapped));
        // Half-scale frame at the origin: composition 960 is screen 480.
        let mut painter = OverlayPainter::new(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(960.0), px(540.0)),
            },
            (1920, 1080),
        );
        SnapGuideOverlay.paint(&snapped, &mut painter);
        let primitives = painter.finish();
        let [OverlayPrimitive::Quad { bounds, .. }] = primitives.as_slice() else {
            panic!("one vertical rule, got {primitives:?}");
        };
        assert_eq!(f32::from(bounds.origin.x), 480.0);
        assert_eq!(f32::from(bounds.size.height), 540.0, "full frame height");

        let both = OverlayContext {
            snap_guides: SnapGuides {
                x: Some(0.0),
                y: Some(540.0),
            },
            ..idle
        };
        let mut painter = OverlayPainter::new(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(960.0), px(540.0)),
            },
            (1920, 1080),
        );
        SnapGuideOverlay.paint(&both, &mut painter);
        assert_eq!(painter.finish().len(), 2, "one rule per snapped axis");
    }

    /// The guide reaches the screen through the registry, not through a
    /// painting path of its own.
    #[test]
    fn the_guide_is_registered_with_the_builtin_overlays() {
        use crate::panels::viewer::overlay::OverlayRegistry;
        use gpui::{Bounds, point, px, size};

        let registry = OverlayRegistry::builtin();
        assert!(registry.overlay(SnapGuideOverlay::ID).is_some());

        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            snap_guides: SnapGuides {
                x: Some(960.0),
                y: None,
            },
            ..OverlayContext::default()
        };
        let mut painter = OverlayPainter::new(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(960.0), px(540.0)),
            },
            (1920, 1080),
        );
        registry.paint(&ctx, &mut painter);
        assert_eq!(
            painter.finish().len(),
            1,
            "the registry painted the guide, and nothing else was active"
        );
    }

    #[test]
    fn no_candidates_means_no_correction() {
        let moving = rect(0.0, 0.0, 100.0, 100.0);
        let result = snap_delta(moving, (7.0, 7.0), &SnapLines::default(), 8.0, plain());
        assert_eq!(result.delta, (7.0, 7.0));
        assert!(result.guides.is_empty());
        assert!(SnapLines::default().is_empty());
    }
}

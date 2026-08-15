// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rulers and user-placed guides.
//!
//! Three properties this carries:
//!
//! - **The ruler is panel chrome, the guide is composition content.** A guide
//!   lives at a composition coordinate and is drawn by [`GuideOverlay`] like
//!   every other mark; the ruler is pinned to the panel's edges, so it cannot
//!   be an overlay — [`OverlayPainter`] only knows the composition rectangle,
//!   which leaves the panel entirely when the view is zoomed in. It is painted
//!   beside the checkerboard, from the same `(panel, frame)` pair.
//! - **The drawn tick and the grabbed strip come from one place.** Both the
//!   marks and [`ruler_axis`] measure from [`RULER_PX`], so the strip a press
//!   lands in is the strip the user can see.
//! - **The guide is the composition's, not the panel's.** Positions live in
//!   `Composition::guides` and ride the document's undo and persistence; only
//!   the *view* of them — rulers shown, guides shown, guides locked — is panel
//!   state, the same class as the grid and safe-area toggles.

use gpui::{Bounds, Hsla, Pixels, point, px, size};
use ravel_core::composition::{Guide, GuideAxis};

use super::overlay::{
    OverlayContext, OverlayId, OverlayPainter, OverlayPrimitive, ViewerOverlay, priority,
};

/// Thickness of each ruler strip, in screen pixels.
pub const RULER_PX: f32 = 16.0;

/// Screen distance within which a press grabs a guide.
pub const GUIDE_HIT_PX: f32 = 6.0;

/// Smallest screen distance between two ticks. Below this the ruler is a solid
/// bar rather than a scale.
const MIN_TICK_PX: f32 = 6.0;

/// Every fifth tick is drawn full depth. A ruler of identical marks says how
/// dense the scale is but not where one reading ends and the next begins.
const MAJOR_EVERY: i64 = 5;

/// Ticks drawn per ruler, at most. `tick_step` already keeps the count near
/// `panel_length / MIN_TICK_PX`; this bounds the loop when a caller hands over a
/// step that did not come from it.
const MAX_TICKS: i64 = 4096;

/// The composition-space distance between ruler ticks at this zoom.
///
/// The 1-2-5 decade series, taking the smallest step whose on-screen spacing is
/// at least [`MIN_TICK_PX`]. `comp_per_px` is the panel's composition pixels per
/// screen pixel — the inverse of the zoom — so the ruler's readings change but
/// its density does not: the spacing this produces always lands in
/// `[MIN_TICK_PX, 2.5 * MIN_TICK_PX)`, the widest ratio the 1-2-5 series has
/// (a requirement just past `2 * decade` is served by `5 * decade`).
pub fn tick_step(comp_per_px: f32) -> f32 {
    if !comp_per_px.is_finite() || comp_per_px <= 0.0 {
        return 1.0;
    }
    let min_comp = MIN_TICK_PX * comp_per_px;
    let decade = 10f32.powf(min_comp.log10().floor());
    for factor in [1.0, 2.0, 5.0] {
        if decade * factor >= min_comp {
            return decade * factor;
        }
    }
    decade * 10.0
}

/// Which ruler strip a panel-local point is in, and therefore which guide a
/// drag out of it creates.
///
/// The top strip runs along composition x and drags out a *horizontal* guide;
/// the left strip drags out a vertical one. The corner belongs to the top
/// strip, which is arbitrary but stable.
pub fn ruler_axis(local: (f32, f32), panel: (f32, f32)) -> Option<GuideAxis> {
    if local.0 < 0.0 || local.1 < 0.0 || local.0 > panel.0 || local.1 > panel.1 {
        return None;
    }
    if local.1 <= RULER_PX {
        Some(GuideAxis::Horizontal)
    } else if local.0 <= RULER_PX {
        Some(GuideAxis::Vertical)
    } else {
        None
    }
}

/// The index of the guide `pointer` grabs, within `threshold` composition
/// units.
///
/// `resolution` is the composition's, in composition units: [`GuideOverlay`]
/// draws each line across that rectangle and no further, so a press in the
/// letterbox around the picture — level with a guide but nowhere near a drawn
/// line — grabs nothing. The reach extends by `threshold` past the frame for
/// the same reason it does across the line: the end of a line is as grabbable
/// as its middle.
///
/// The nearest one wins, and ties keep the earlier guide, so a press between two
/// coincident guides picks the same one every time instead of alternating.
pub fn guide_at(
    guides: &[Guide],
    pointer: (f32, f32),
    threshold: f32,
    resolution: (f32, f32),
) -> Option<usize> {
    if !threshold.is_finite() || threshold < 0.0 {
        return None;
    }
    let mut best: Option<(f32, usize)> = None;
    for (index, guide) in guides.iter().enumerate() {
        // `across` is measured against the guide's position; `along` runs down
        // the drawn line and is bounded by how far the line is drawn.
        let (across, along, extent) = match guide.axis {
            GuideAxis::Vertical => (pointer.0, pointer.1, resolution.1),
            GuideAxis::Horizontal => (pointer.1, pointer.0, resolution.0),
        };
        if !(-threshold..=extent + threshold).contains(&along) {
            continue;
        }
        let distance = (guide.position - across).abs();
        if !distance.is_finite() || distance > threshold {
            continue;
        }
        if best.is_none_or(|(nearest, _)| distance < nearest) {
            best = Some((distance, index));
        }
    }
    best.map(|(_, index)| index)
}

/// The ruler strips and their ticks, in screen space.
///
/// `panel` is the canvas area and `frame` the composition rectangle inside it —
/// the same pair the checkerboard is drawn from. Ticks are placed through the
/// composition-to-screen mapping the pointer is resolved with, so a mark and the
/// coordinate it names cannot drift apart.
pub fn ruler_primitives(
    panel: Bounds<Pixels>,
    frame: Bounds<Pixels>,
    resolution: (u32, u32),
    background: Hsla,
    tick: Hsla,
) -> Vec<OverlayPrimitive> {
    let zoom = f32::from(frame.size.width) / resolution.0.max(1) as f32;
    if !zoom.is_finite() || zoom <= 0.0 {
        return Vec::new();
    }
    let (panel_x, panel_y) = (f32::from(panel.origin.x), f32::from(panel.origin.y));
    let (panel_w, panel_h) = (f32::from(panel.size.width), f32::from(panel.size.height));
    let mut primitives = vec![
        OverlayPrimitive::Quad {
            bounds: Bounds {
                origin: panel.origin,
                size: size(panel.size.width, px(RULER_PX)),
            },
            color: background,
        },
        OverlayPrimitive::Quad {
            bounds: Bounds {
                origin: panel.origin,
                size: size(px(RULER_PX), panel.size.height),
            },
            color: background,
        },
    ];

    let step = tick_step(1.0 / zoom);
    for axis in [GuideAxis::Vertical, GuideAxis::Horizontal] {
        let (origin, length) = match axis {
            GuideAxis::Vertical => (f32::from(frame.origin.x) - panel_x, panel_w),
            GuideAxis::Horizontal => (f32::from(frame.origin.y) - panel_y, panel_h),
        };
        // The composition coordinates the strip spans, as tick indices.
        let first = (-origin / (step * zoom)).ceil();
        let last = ((length - origin) / (step * zoom)).floor();
        if !first.is_finite() || !last.is_finite() {
            continue;
        }
        let (first, last) = (first as i64, last as i64);
        for index in first..=last.min(first.saturating_add(MAX_TICKS)) {
            let offset = origin + index as f32 * step * zoom;
            let depth = if index.rem_euclid(MAJOR_EVERY) == 0 {
                RULER_PX
            } else {
                RULER_PX * 0.5
            };
            let bounds = match axis {
                GuideAxis::Vertical => Bounds {
                    origin: point(px(panel_x + offset), px(panel_y + RULER_PX - depth)),
                    size: size(px(1.0), px(depth)),
                },
                GuideAxis::Horizontal => Bounds {
                    origin: point(px(panel_x + RULER_PX - depth), px(panel_y + offset)),
                    size: size(px(depth), px(1.0)),
                },
            };
            primitives.push(OverlayPrimitive::Quad {
                bounds,
                color: tick,
            });
        }
    }
    primitives
}

/// The guide colour: cyan, distinct from the snap guide's magenta. A snap guide
/// reports a correction that is happening now; a user guide is a standing mark,
/// and the two are routinely on screen together.
const GUIDE_COLOR: Hsla = Hsla {
    h: 0.5,
    s: 0.85,
    l: 0.6,
    a: 0.85,
};

/// The composition's user guides, while they are shown.
///
/// It owns nothing: the positions live in the document, so undo, redo and
/// reload move the drawn lines with them.
pub struct GuideOverlay;

impl GuideOverlay {
    pub const ID: OverlayId = OverlayId("viewer.guides");

    /// The guides of the composition on screen, or an empty slice.
    pub fn guides(ctx: &OverlayContext) -> &[Guide] {
        let Some((comp, document)) = ctx.comp.zip(ctx.document.as_ref()) else {
            return &[];
        };
        document
            .get_composition(comp)
            .map_or(&[], |composition| composition.guides.as_slice())
    }
}

impl ViewerOverlay for GuideOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::GUIDES
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.show_guides && ctx.resolution.is_some() && !Self::guides(ctx).is_empty()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        for guide in Self::guides(ctx) {
            match guide.axis {
                GuideAxis::Vertical => painter.comp_vrule(guide.position, 1.0, GUIDE_COLOR),
                GuideAxis::Horizontal => painter.comp_hrule(guide.position, 1.0, GUIDE_COLOR),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{Composition, Document};
    use ravel_core::id::CompId;
    use ravel_core::types::FrameRate;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    /// The ruler's density follows the zoom: whatever the scale, consecutive
    /// ticks are between one and two minimum spacings apart on screen, and the
    /// step itself always comes from the 1-2-5 series.
    #[test]
    fn the_tick_step_keeps_a_constant_screen_density_at_every_zoom() {
        let mut zoom = 0.01f32;
        while zoom <= 64.0 {
            let step = tick_step(1.0 / zoom);
            let spacing = step * zoom;
            assert!(
                (MIN_TICK_PX..MIN_TICK_PX * 2.5).contains(&spacing),
                "zoom {zoom}: {step} units is {spacing}px apart"
            );
            let mantissa = step / 10f32.powf(step.log10().floor());
            assert!(
                [1.0, 2.0, 5.0].iter().any(|m| (mantissa - m).abs() < 1e-3),
                "zoom {zoom}: {step} is not a 1-2-5 step (mantissa {mantissa})"
            );
            zoom *= 1.3;
        }
    }

    /// Zooming in never coarsens the ruler and zooming out never refines it.
    #[test]
    fn the_tick_step_is_monotonic_in_the_zoom() {
        let mut previous = f32::INFINITY;
        let mut zoom = 0.05f32;
        while zoom <= 32.0 {
            let step = tick_step(1.0 / zoom);
            assert!(step <= previous, "zoom {zoom} coarsened the ruler");
            previous = step;
            zoom *= 1.2;
        }
    }

    /// A degenerate zoom has no scale to report, and answering with a NaN step
    /// would drive the tick loop out of its bounds instead of drawing nothing.
    #[test]
    fn a_degenerate_zoom_falls_back_to_unit_ticks() {
        for comp_per_px in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(tick_step(comp_per_px), 1.0, "{comp_per_px}");
        }
    }

    /// The strip a press lands in is the one the ruler draws, and the guide it
    /// produces runs across the strip rather than along it.
    #[test]
    fn the_ruler_strips_map_to_the_guide_they_drag_out() {
        let panel = (800.0, 600.0);
        assert_eq!(
            ruler_axis((400.0, RULER_PX - 1.0), panel),
            Some(GuideAxis::Horizontal),
            "the top strip drags out a horizontal guide"
        );
        assert_eq!(
            ruler_axis((RULER_PX - 1.0, 400.0), panel),
            Some(GuideAxis::Vertical),
            "the left strip drags out a vertical one"
        );
        assert_eq!(
            ruler_axis((1.0, 1.0), panel),
            Some(GuideAxis::Horizontal),
            "the corner is stable, not ambiguous"
        );
        assert_eq!(ruler_axis((400.0, RULER_PX + 1.0), panel), None);
        assert_eq!(ruler_axis((RULER_PX + 1.0, 400.0), panel), None);
        assert_eq!(ruler_axis((-1.0, -1.0), panel), None, "outside the panel");
        assert_eq!(ruler_axis((900.0, 300.0), panel), None);
    }

    /// A press picks the nearest guide inside the threshold, on the axis the
    /// guide actually runs across, and nothing outside it.
    #[test]
    fn a_press_grabs_the_nearest_guide_within_the_threshold() {
        let guides = [
            Guide::vertical(100.0),
            Guide::horizontal(50.0),
            Guide::vertical(104.0),
        ];
        let frame = (1920.0, 1080.0);
        assert_eq!(guide_at(&guides, (103.0, 300.0), 8.0, frame), Some(2));
        assert_eq!(guide_at(&guides, (101.0, 300.0), 8.0, frame), Some(0));
        assert_eq!(
            guide_at(&guides, (300.0, 52.0), 8.0, frame),
            Some(1),
            "a horizontal guide is measured along y"
        );
        assert_eq!(
            guide_at(&guides, (100.0, 300.0), 0.0, frame),
            Some(0),
            "a zero threshold still grabs an exact hit"
        );
        assert_eq!(guide_at(&guides, (120.0, 300.0), 8.0, frame), None);
        assert_eq!(guide_at(&[], (0.0, 0.0), 8.0, frame), None);
        assert_eq!(
            guide_at(&guides, (f32::NAN, 0.0), 8.0, frame),
            None,
            "a non-finite pointer is nowhere, not everywhere"
        );
    }

    /// A guide is drawn across the composition rectangle and no further, so it
    /// is grabbable there and no further: the letterbox holds no line.
    #[test]
    fn a_guide_is_grabbable_only_where_it_is_drawn() {
        let frame = (1920.0, 1080.0);
        let vertical = [Guide::vertical(100.0)];
        assert_eq!(guide_at(&vertical, (100.0, 540.0), 8.0, frame), Some(0));
        assert_eq!(
            guide_at(&vertical, (100.0, 1085.0), 8.0, frame),
            Some(0),
            "the end of a line is as grabbable as its middle, within the reach"
        );
        assert_eq!(
            guide_at(&vertical, (100.0, 1200.0), 8.0, frame),
            None,
            "below the frame there is nothing drawn to grab"
        );
        assert_eq!(guide_at(&vertical, (100.0, -200.0), 8.0, frame), None);
        assert_eq!(
            guide_at(&vertical, (100.0, f32::NAN), 8.0, frame),
            None,
            "a pointer that is nowhere along the line grabs nothing"
        );

        // The horizontal guide is bounded the other way round.
        let horizontal = [Guide::horizontal(100.0)];
        assert_eq!(guide_at(&horizontal, (960.0, 100.0), 8.0, frame), Some(0));
        assert_eq!(guide_at(&horizontal, (2100.0, 100.0), 8.0, frame), None);
        assert_eq!(guide_at(&horizontal, (-30.0, 100.0), 8.0, frame), None);
    }

    /// Ties keep the earlier guide, so the choice is stable.
    #[test]
    fn coincident_guides_resolve_to_the_first_one() {
        let guides = [Guide::vertical(100.0), Guide::vertical(100.0)];
        assert_eq!(
            guide_at(&guides, (100.0, 0.0), 8.0, (1920.0, 1080.0)),
            Some(0)
        );
    }

    /// The ruler draws two strips and places its ticks at the composition
    /// coordinates the same viewport maps: at half scale with the frame at
    /// panel-local 100, composition 0 is 100px in and each step is
    /// `step * zoom` further.
    #[test]
    fn the_ruler_places_its_ticks_through_the_viewport() {
        let panel = bounds(0.0, 0.0, 400.0, 300.0);
        let frame = bounds(100.0, 50.0, 960.0, 540.0);
        let primitives = ruler_primitives(
            panel,
            frame,
            (1920, 1080),
            gpui::hsla(0.0, 0.0, 0.1, 1.0),
            gpui::hsla(0.0, 0.0, 1.0, 0.5),
        );
        let [
            OverlayPrimitive::Quad { bounds: top, .. },
            OverlayPrimitive::Quad { bounds: left, .. },
            ticks @ ..,
        ] = primitives.as_slice()
        else {
            panic!("two strips and their ticks, got {primitives:?}");
        };
        assert_eq!(f32::from(top.size.height), RULER_PX);
        assert_eq!(f32::from(top.size.width), 400.0);
        assert_eq!(f32::from(left.size.width), RULER_PX);
        assert_eq!(f32::from(left.size.height), 300.0);

        // Half scale: 0.5 screen pixels per composition unit, so the step is
        // the smallest 1-2-5 value at or past 12 composition units.
        let step = tick_step(2.0);
        assert_eq!(step, 20.0);
        // The first vertical tick at or past the panel's left edge is
        // composition -200 (screen 0), and the ticks advance by 10px.
        let vertical: Vec<f32> = ticks
            .iter()
            .filter_map(|primitive| match primitive {
                OverlayPrimitive::Quad { bounds, .. } if f32::from(bounds.size.width) == 1.0 => {
                    Some(f32::from(bounds.origin.x))
                }
                _ => None,
            })
            .collect();
        assert_eq!(vertical.first().copied(), Some(0.0));
        assert_eq!(vertical.get(1).copied(), Some(10.0));
        assert_eq!(
            vertical.last().copied(),
            Some(400.0),
            "the last tick is still inside the panel"
        );
        // The composition origin carries a major tick, drawn full depth.
        let origin_tick = ticks
            .iter()
            .find_map(|primitive| match primitive {
                OverlayPrimitive::Quad { bounds, .. }
                    if f32::from(bounds.origin.x) == 100.0
                        && f32::from(bounds.size.width) == 1.0 =>
                {
                    Some(f32::from(bounds.size.height))
                }
                _ => None,
            })
            .expect("a tick at composition 0");
        assert_eq!(origin_tick, RULER_PX, "composition 0 is a major tick");
    }

    /// Nothing to measure against: no strips, no ticks, and no division by a
    /// zero-width frame.
    #[test]
    fn a_collapsed_frame_draws_no_ruler() {
        let primitives = ruler_primitives(
            bounds(0.0, 0.0, 400.0, 300.0),
            bounds(0.0, 0.0, 0.0, 0.0),
            (1920, 1080),
            gpui::hsla(0.0, 0.0, 0.1, 1.0),
            gpui::hsla(0.0, 0.0, 1.0, 0.5),
        );
        assert!(primitives.is_empty());
    }

    fn context_with(guides: Vec<Guide>) -> OverlayContext {
        let comp = CompId::new(7);
        let mut composition =
            Composition::new(comp, "Comp", (1920, 1080), FrameRate::new(30, 1), 10);
        composition.guides = guides;
        OverlayContext {
            resolution: Some((1920, 1080)),
            comp: Some(comp),
            document: Some(Document::default().with_composition(composition)),
            show_guides: true,
            ..OverlayContext::default()
        }
    }

    /// One rule per guide, at the composition coordinate it names — and nothing
    /// at all while the guides are hidden or there are none.
    #[test]
    fn the_overlay_draws_one_rule_per_guide() {
        let ctx = context_with(vec![Guide::vertical(960.0), Guide::horizontal(270.0)]);
        assert!(GuideOverlay.is_active(&ctx));

        let mut painter = OverlayPainter::new(bounds(0.0, 0.0, 960.0, 540.0), (1920, 1080));
        GuideOverlay.paint(&ctx, &mut painter);
        let primitives = painter.finish();
        let [
            OverlayPrimitive::Quad {
                bounds: vertical, ..
            },
            OverlayPrimitive::Quad {
                bounds: horizontal, ..
            },
        ] = primitives.as_slice()
        else {
            panic!("one rule per guide, got {primitives:?}");
        };
        // Half scale: composition 960 is screen 480, composition 270 is 135.
        assert_eq!(f32::from(vertical.origin.x), 480.0);
        assert_eq!(f32::from(vertical.size.height), 540.0);
        assert_eq!(f32::from(horizontal.origin.y), 135.0);
        assert_eq!(f32::from(horizontal.size.width), 960.0);

        let hidden = OverlayContext {
            show_guides: false,
            ..ctx.clone()
        };
        assert!(
            !GuideOverlay.is_active(&hidden),
            "hidden guides draw nothing"
        );
        assert!(!GuideOverlay.is_active(&context_with(Vec::new())));
        assert!(
            !GuideOverlay.is_active(&OverlayContext {
                show_guides: true,
                ..OverlayContext::default()
            }),
            "no composition, no guides"
        );
    }

    /// The guides reach the screen through the registry, not through a painting
    /// path of their own.
    #[test]
    fn the_guide_overlay_is_registered_with_the_builtin_overlays() {
        use crate::panels::viewer::overlay::OverlayRegistry;

        let registry = OverlayRegistry::builtin();
        assert!(registry.overlay(GuideOverlay::ID).is_some());

        let ctx = context_with(vec![Guide::vertical(960.0)]);
        let mut painter = OverlayPainter::new(bounds(0.0, 0.0, 960.0, 540.0), (1920, 1080));
        registry.paint(&ctx, &mut painter);
        assert_eq!(
            painter.finish().len(),
            1,
            "the registry painted the guide, and nothing else was active"
        );
    }
}

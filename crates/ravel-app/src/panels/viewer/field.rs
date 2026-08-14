// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Viewer's field overlay.
//!
//! A [`FieldValue`] has no shape of its own — it is a function, and the only
//! way to see one is to sample it. This overlay samples the selected node's
//! `FIELD` output on a grid over the composition rectangle and draws the
//! result as a heatmap, as bands separated by isolines, or as arrows for a
//! vector field.
//!
//! Two properties the sampling has to keep:
//!
//! - **Bounded work.** The grid comes from the on-screen size, so a small
//!   viewer costs less than a large one, but it is clamped by
//!   [`MAX_FIELD_SAMPLES`]: zooming in must not make the sample count diverge.
//!   `GradientField` samples its source four times *per point* and chains
//!   multiply, so an uncapped grid is not a slow frame, it is a hung one.
//! - **Layer-local coordinates.** A field is defined in the coordinates of the
//!   network it lives in. The grid is laid out in composition space and each
//!   point is mapped back through the inverse of the layer's compositing
//!   transform before sampling, so the picture follows the layer.

use gpui::Hsla;
use ravel_core::composition::transform::Affine;
use ravel_core::eval::EvalContext;
use ravel_core::geometry::{AttributeArray, FieldSample, FieldValue};
use ravel_core::id::{DataTypeId, NodeId, OutputPortIndex};
use ravel_core::types::Vec2;
use ravel_ui::document::NetworkPath;

use super::CompRect;
use super::overlay::{
    OverlayContext, OverlayId, OverlayPainter, OverlayTarget, ViewerOverlay, priority,
};

/// Hard ceiling on grid points sampled per frame, whatever the zoom.
pub const MAX_FIELD_SAMPLES: usize = 4096;

/// Alpha the field is drawn at until the user changes it: enough to read the
/// field, transparent enough to keep the composition under it visible.
pub const DEFAULT_FIELD_OPACITY: f32 = 0.55;

/// The opacity steps the toolbar offers. A short list rather than a slider:
/// the value is a viewing preference with no need for fine control.
pub const FIELD_OPACITY_STEPS: [f32; 4] = [0.25, 0.55, 0.8, 1.0];

/// Target on-screen size of one grid cell, before the cap applies.
const FIELD_CELL_PX: f32 = 16.0;

/// Longest arrow drawn, in composition units per cell, as a fraction of the
/// cell size. Keeps a large vector from reaching across the whole picture.
const ARROW_CELL_FRACTION: f32 = 0.45;

/// What the field overlay draws, or that it draws nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldDisplay {
    #[default]
    Off,
    /// One filled cell per sample.
    Heatmap,
    /// Only the cells where the quantised value changes, which reads as the
    /// isolines between bands.
    ///
    /// Cell-resolution, not marching squares: the line is one cell wide and
    /// steps rather than interpolating. That is enough to see the shape of a
    /// field at a glance, which is what this overlay is for; a smooth contour
    /// would need edge interpolation and a segment list.
    Contours,
    /// One arrow per cell, for a field that samples to a vector.
    Arrows,
}

impl FieldDisplay {
    pub const ALL: [Self; 4] = [Self::Off, Self::Heatmap, Self::Contours, Self::Arrows];

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Off => "viewer.field_off",
            Self::Heatmap => "viewer.field_heatmap",
            Self::Contours => "viewer.field_contours",
            Self::Arrows => "viewer.field_arrows",
        }
    }
}

/// How a normalised scalar becomes a colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldColorMap {
    /// Cold to hot through the hue wheel: the default because it separates
    /// nearby values better than lightness alone.
    #[default]
    Heat,
    /// Lightness only, for reading a field over colourful footage.
    Grayscale,
}

impl FieldColorMap {
    pub const ALL: [Self; 2] = [Self::Heat, Self::Grayscale];

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Heat => "viewer.field_map_heat",
            Self::Grayscale => "viewer.field_map_gray",
        }
    }

    /// `value` is already normalised to `0..=1`.
    pub fn color(self, value: f32, alpha: f32) -> Hsla {
        let value = value.clamp(0.0, 1.0);
        match self {
            // 0.66 (blue) down to 0.0 (red).
            Self::Heat => Hsla {
                h: (1.0 - value) * 0.66,
                s: 0.85,
                l: 0.5,
                a: alpha,
            },
            Self::Grayscale => Hsla {
                h: 0.0,
                s: 0.0,
                l: value,
                a: alpha,
            },
        }
    }
}

/// The number of grid columns and rows for a frame of `width` x `height`
/// screen pixels, capped so the total never exceeds [`MAX_FIELD_SAMPLES`].
///
/// The cap scales both axes by the same factor, so a wide viewer keeps a wide
/// grid instead of being squared off.
pub fn grid_dimensions(width_px: f32, height_px: f32) -> (usize, usize) {
    let cols = ((width_px / FIELD_CELL_PX).floor() as usize).max(1);
    let rows = ((height_px / FIELD_CELL_PX).floor() as usize).max(1);
    let total = cols.saturating_mul(rows);
    if total <= MAX_FIELD_SAMPLES {
        return (cols, rows);
    }
    let shrink = (MAX_FIELD_SAMPLES as f32 / total as f32).sqrt();
    (
        ((cols as f32 * shrink).floor() as usize).max(1),
        ((rows as f32 * shrink).floor() as usize).max(1),
    )
}

/// One sampled grid: cell centres in composition space and the values there.
pub struct FieldGrid {
    pub cols: usize,
    pub rows: usize,
    /// Composition-space size of one cell.
    pub cell: (f32, f32),
    /// Cell centres in composition space, row-major.
    pub centers: Vec<(f32, f32)>,
    pub values: AttributeArray,
}

impl FieldGrid {
    /// Number of samples in the grid. Never zero — [`grid_dimensions`]
    /// floors at one cell per axis — so there is no `is_empty` to ask.
    pub fn sample_count(&self) -> usize {
        self.centers.len()
    }

    /// Scalar readings: the magnitude of a vector, the value of a scalar, the
    /// luminance-free lightness of a colour. `None` for a field that samples
    /// to something with no magnitude (a string, a bool).
    pub fn scalars(&self) -> Option<Vec<f32>> {
        match &self.values {
            AttributeArray::F32(values) => Some(values.clone()),
            AttributeArray::I32(values) => Some(values.iter().map(|v| *v as f32).collect()),
            AttributeArray::Vec2(values) => Some(
                values
                    .iter()
                    .map(|v| (v.0 * v.0 + v.1 * v.1).sqrt())
                    .collect(),
            ),
            AttributeArray::Vec3(values) => Some(
                values
                    .iter()
                    .map(|v| (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt())
                    .collect(),
            ),
            AttributeArray::Vec4(values) => Some(
                values
                    .iter()
                    .map(|v| (v.0 * v.0 + v.1 * v.1 + v.2 * v.2 + v.3 * v.3).sqrt())
                    .collect(),
            ),
            AttributeArray::Color(values) => {
                Some(values.iter().map(|c| (c.r + c.g + c.b) / 3.0).collect())
            }
            AttributeArray::Bool(_) | AttributeArray::Str(_) => None,
        }
    }

    /// Planar readings, for the arrow mode. `None` for a field that does not
    /// sample to a vector — an arrow needs a direction, and a scalar has none.
    pub fn vectors(&self) -> Option<Vec<(f32, f32)>> {
        match &self.values {
            AttributeArray::Vec2(values) => Some(values.iter().map(|v| (v.0, v.1)).collect()),
            AttributeArray::Vec3(values) => Some(values.iter().map(|v| (v.0, v.1)).collect()),
            AttributeArray::Vec4(values) => Some(values.iter().map(|v| (v.0, v.1)).collect()),
            _ => None,
        }
    }
}

/// Normalise `values` to `0..=1` over their own range.
///
/// Auto-ranged rather than fixed: a noise field lands in `-1..=1`, a falloff
/// in `0..=1`, an expression anywhere at all, and a fixed range would show
/// most of them as flat. A constant field normalises to the middle of the map
/// instead of dividing by zero.
pub fn normalize(values: &[f32]) -> Vec<f32> {
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for value in values.iter().filter(|v| v.is_finite()) {
        min = min.min(*value);
        max = max.max(*value);
    }
    let span = max - min;
    if !span.is_finite() || span <= f32::EPSILON {
        return vec![0.5; values.len()];
    }
    values
        .iter()
        .map(|value| ((value - min) / span).clamp(0.0, 1.0))
        .collect()
}

/// Sample `field` over `rect` in composition space.
///
/// `to_local` maps a composition point into the coordinates the field is
/// defined in — the inverse of the layer's compositing transform.
pub fn sample_grid(
    field: &FieldValue,
    rect: CompRect,
    cols: usize,
    rows: usize,
    to_local: &Affine,
    ctx: &EvalContext,
) -> FieldGrid {
    let cell = (rect.w / cols as f32, rect.h / rows as f32);
    let mut centers = Vec::with_capacity(cols * rows);
    let mut local = Vec::with_capacity(cols * rows);
    for row in 0..rows {
        for col in 0..cols {
            let center = (
                rect.x + (col as f32 + 0.5) * cell.0,
                rect.y + (row as f32 + 0.5) * cell.1,
            );
            centers.push(center);
            let (x, y) = to_local.apply(center.0, center.1);
            local.push(Vec2(x, y));
        }
    }
    // One call for the whole grid: `Field::sample` is column-shaped, and a
    // call per point would pay the dispatch and any per-call setup N times.
    let values = field.sample(&FieldSample::positions_only(&local, ctx));
    FieldGrid {
        cols,
        rows,
        cell,
        centers,
        values,
    }
}

/// The output port of `node` that carries a field, if it has one.
pub fn field_output_port(node: &ravel_core::graph::Node) -> Option<OutputPortIndex> {
    node.outputs
        .iter()
        .position(|port| port.data_type == DataTypeId::FIELD)
        .map(|index| OutputPortIndex(index as u32))
}

/// The single selected node with a `FIELD` output, and the network it lives
/// in.
///
/// **One** node: two heatmaps drawn over each other say nothing about either.
/// A selection that is not exactly one field node draws nothing.
pub fn selected_field_node(ctx: &OverlayContext) -> Option<(NetworkPath, NodeId, OutputPortIndex)> {
    let selection = ctx.selection.as_ref()?;
    let network = selection.path.clone()?;
    let [node_id] = selection.nodes.iter().copied().collect::<Vec<_>>()[..] else {
        return None;
    };
    let graph = ravel_ui::document::resolve_network(ctx.document.as_ref()?, &network)?;
    let port = field_output_port(graph.node(node_id)?)?;
    Some((network, node_id, port))
}

/// Draws the selected node's `FIELD` output over the composition.
pub struct FieldOverlay;

impl FieldOverlay {
    pub const ID: OverlayId = OverlayId("viewer.field");
}

impl ViewerOverlay for FieldOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::FIELD
    }

    /// Active on the *selection*, not on the toggle and not on the result.
    ///
    /// The panel's display mode is not visible while evaluation targets are
    /// collected (that context has no window and no panel state), and an
    /// inactive overlay is never asked for its targets — so gating here would
    /// stop the field from ever being requested. `paint` reads the mode.
    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.resolution.is_some() && selected_field_node(ctx).is_some()
    }

    fn eval_targets(&self, ctx: &OverlayContext) -> Vec<OverlayTarget> {
        selected_field_node(ctx)
            .map(|(network, node, output)| {
                vec![OverlayTarget {
                    network,
                    node,
                    output,
                }]
            })
            .unwrap_or_default()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        if ctx.field_display == FieldDisplay::Off {
            return;
        }
        let Some((network, node, output)) = selected_field_node(ctx) else {
            return;
        };
        let Some((document, resolution, _)) = ctx.resolved() else {
            return;
        };
        let Some(value) = ctx.eval_result(&OverlayTarget {
            network: network.clone(),
            node,
            output,
        }) else {
            // No result yet: draw nothing rather than a field of zeroes.
            return;
        };
        let Some(field) = value.downcast_ref::<FieldValue>() else {
            return;
        };
        let Some(shell) = super::layer_shell(ctx, document, network.comp, network.layer) else {
            return;
        };
        // A degenerate shell (a zero scale) has no inverse, so there is no
        // composition point that maps to a field coordinate.
        let Some(to_local) = shell.inverse() else {
            return;
        };
        let frame = painter.frame();
        let (cols, rows) =
            grid_dimensions(f32::from(frame.size.width), f32::from(frame.size.height));
        let rect = CompRect {
            x: 0.0,
            y: 0.0,
            w: resolution.0 as f32,
            h: resolution.1 as f32,
        };
        let Some(eval) = field_eval_context(ctx, document, &network, resolution) else {
            return;
        };
        let grid = sample_grid(field, rect, cols, rows, &to_local, &eval);
        paint_grid(
            &grid,
            ctx.field_display,
            ctx.field_map,
            ctx.field_opacity,
            painter,
        );
    }
}

/// The context the field is sampled at: the layer-local frame the field itself
/// was evaluated at, so a time-varying field is drawn at the frame on screen.
///
/// `None` when the layer is not on screen at all. `Layer::displayed_local_frame`
/// rather than `local_frame` on purpose: the clamped form reports `in_frame`
/// for every composition frame before the layer starts, and the two frames have
/// to agree with the interval check that decided to request this field
/// (`ProjectState::overlay_scoped_targets`) or the overlay samples one frame and
/// draws the value of another.
fn field_eval_context(
    ctx: &OverlayContext,
    document: &ravel_core::composition::Document,
    network: &NetworkPath,
    resolution: (u32, u32),
) -> Option<EvalContext> {
    let playback = ctx.playback?;
    let frame = document
        .get_composition(network.comp)?
        .get_layer(network.layer)?
        .displayed_local_frame(playback.frame)?;
    Some(EvalContext::new(frame, playback.fps, resolution).with_comp_resolution(resolution))
}

/// Draw a sampled grid. Split from `paint` so the drawing is testable without
/// a document, a selection, or an evaluator.
pub fn paint_grid(
    grid: &FieldGrid,
    display: FieldDisplay,
    map: FieldColorMap,
    opacity: f32,
    painter: &mut OverlayPainter,
) {
    match display {
        FieldDisplay::Off => {}
        FieldDisplay::Heatmap => {
            let Some(scalars) = grid.scalars() else {
                return;
            };
            for (index, value) in normalize(&scalars).into_iter().enumerate() {
                painter.fill_comp_rect(cell_rect(grid, index), map.color(value, opacity));
            }
        }
        FieldDisplay::Contours => {
            let Some(scalars) = grid.scalars() else {
                return;
            };
            let normalized = normalize(&scalars);
            // Eight bands: enough to read the shape, few enough that the lines
            // stay apart at overlay sizes.
            let band = |value: f32| (value * 8.0).min(7.0) as u8;
            for index in 0..grid.sample_count() {
                let (col, row) = (index % grid.cols, index / grid.cols);
                let here = band(normalized[index]);
                let right = (col + 1 < grid.cols).then(|| band(normalized[index + 1]));
                let below = (row + 1 < grid.rows).then(|| band(normalized[index + grid.cols]));
                if right.is_some_and(|other| other != here)
                    || below.is_some_and(|other| other != here)
                {
                    painter.fill_comp_rect(
                        cell_rect(grid, index),
                        map.color(normalized[index], opacity),
                    );
                }
            }
        }
        FieldDisplay::Arrows => {
            let Some(vectors) = grid.vectors() else {
                // A scalar field has no direction; nothing is drawn rather
                // than an arrow pointing at an arbitrary axis.
                return;
            };
            let longest = vectors
                .iter()
                .map(|v| (v.0 * v.0 + v.1 * v.1).sqrt())
                .fold(0.0f32, f32::max);
            if longest <= f32::EPSILON {
                return;
            }
            let reach = grid.cell.0.min(grid.cell.1) * ARROW_CELL_FRACTION;
            let scale = reach / longest;
            let color = map.color(1.0, opacity);
            for (index, vector) in vectors.iter().enumerate() {
                let from = grid.centers[index];
                let to = (from.0 + vector.0 * scale, from.1 + vector.1 * scale);
                painter.stroke_comp_polyline(&[from, to], false, 1.0, color);
            }
        }
    }
}

fn cell_rect(grid: &FieldGrid, index: usize) -> CompRect {
    let center = grid.centers[index];
    CompRect {
        x: center.0 - grid.cell.0 * 0.5,
        y: center.1 - grid.cell.1 * 0.5,
        w: grid.cell.0,
        h: grid.cell.1,
    }
}

#[cfg(test)]
mod tests {
    use super::super::overlay::OverlayResults;
    use super::*;

    /// A field whose value is the x coordinate, so a sampled grid has a value
    /// this test can predict at any point.
    struct RampField;

    impl ravel_core::geometry::Field for RampField {
        fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
            AttributeArray::F32(input.positions.iter().map(|p| p.0).collect())
        }

        fn byte_size(&self) -> u64 {
            0
        }
    }

    /// A constant planar field, for the arrow mode.
    struct EastField;

    impl ravel_core::geometry::Field for EastField {
        fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
            AttributeArray::Vec2(input.positions.iter().map(|_| Vec2(2.0, 0.0)).collect())
        }

        fn byte_size(&self) -> u64 {
            0
        }
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, ravel_core::types::FrameRate::new(30, 1), (100, 100))
    }

    fn ramp_grid(cols: usize, rows: usize) -> FieldGrid {
        sample_grid(
            &FieldValue::new(RampField),
            CompRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            cols,
            rows,
            &Affine::IDENTITY,
            &ctx(),
        )
    }

    /// Completion criterion: a known analytic field lands on the colour the
    /// map says it should at a known coordinate.
    #[test]
    fn a_scalar_field_becomes_the_heatmap_colour_its_value_maps_to() {
        let grid = ramp_grid(4, 1);
        // Cell centres at x = 12.5, 37.5, 62.5, 87.5 — a straight ramp, so the
        // normalised values are 0, 1/3, 2/3, 1.
        let scalars = grid.scalars().expect("a scalar field");
        assert_eq!(scalars, vec![12.5, 37.5, 62.5, 87.5]);
        let normalized = normalize(&scalars);
        assert!((normalized[0] - 0.0).abs() < 1e-6);
        assert!((normalized[3] - 1.0).abs() < 1e-6);

        // The lowest sample is the cold end of the map, the highest the hot
        // end — the property a reader relies on to read the picture.
        let cold = FieldColorMap::Heat.color(normalized[0], 1.0);
        let hot = FieldColorMap::Heat.color(normalized[3], 1.0);
        assert!((cold.h - 0.66).abs() < 1e-6, "{cold:?}");
        assert!((hot.h - 0.0).abs() < 1e-6, "{hot:?}");

        // And the drawing agrees: one filled cell per sample, the first in the
        // cold colour and the last in the hot one.
        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        paint_grid(
            &grid,
            FieldDisplay::Heatmap,
            FieldColorMap::Heat,
            0.5,
            &mut painter,
        );
        let quads: Vec<_> = painter
            .finish()
            .into_iter()
            .filter_map(|primitive| match primitive {
                super::super::overlay::OverlayPrimitive::Quad { bounds, color } => {
                    Some((bounds, color))
                }
                _ => None,
            })
            .collect();
        assert_eq!(quads.len(), 4);
        assert!((quads[0].1.h - 0.66).abs() < 1e-6);
        assert!((quads[3].1.h - 0.0).abs() < 1e-6);
        assert!((quads[0].1.a - 0.5).abs() < 1e-6, "opacity was ignored");
    }

    /// Completion criterion: the sample count is bounded however far the
    /// viewer is zoomed in.
    #[test]
    fn the_sample_count_never_exceeds_the_cap() {
        for (width, height) in [
            (320.0, 180.0),
            (1920.0, 1080.0),
            (19_200.0, 10_800.0),
            (1_000_000.0, 1_000_000.0),
        ] {
            let (cols, rows) = grid_dimensions(width, height);
            assert!(
                cols * rows <= MAX_FIELD_SAMPLES,
                "{width}x{height} asked for {cols}x{rows}"
            );
            assert!(cols >= 1 && rows >= 1);
            let grid = ramp_grid(cols, rows);
            assert_eq!(grid.sample_count(), cols * rows);
        }
        // A viewer small enough to stay under the cap keeps its natural grid,
        // so the cap is a ceiling rather than a fixed resolution.
        assert_eq!(grid_dimensions(160.0, 160.0), (10, 10));
    }

    /// A degenerate viewport still asks for a grid, and one cell is the
    /// smallest answer that is not a division by zero.
    #[test]
    fn a_collapsed_viewport_still_samples_one_cell() {
        assert_eq!(grid_dimensions(0.0, 0.0), (1, 1));
    }

    /// Contours draw only where the band changes, so they are strictly fewer
    /// marks than the heatmap of the same grid.
    #[test]
    fn contours_draw_only_the_cells_where_the_band_changes() {
        let grid = ramp_grid(32, 1);
        let count = |display: FieldDisplay| {
            let mut painter = OverlayPainter::new(
                gpui::Bounds {
                    origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                    size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
                },
                (100, 100),
            );
            paint_grid(&grid, display, FieldColorMap::Heat, 1.0, &mut painter);
            painter.finish().len()
        };
        let contours = count(FieldDisplay::Contours);
        assert!(contours > 0, "a ramp crosses every band");
        assert!(
            contours < count(FieldDisplay::Heatmap),
            "contours drew as much as the heatmap"
        );
    }

    /// Arrows need a direction: a vector field draws one segment per cell, a
    /// scalar field draws nothing rather than an arbitrary heading.
    #[test]
    fn arrows_need_a_vector_field() {
        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        paint_grid(
            &ramp_grid(4, 4),
            FieldDisplay::Arrows,
            FieldColorMap::Heat,
            1.0,
            &mut painter,
        );
        assert!(painter.finish().is_empty(), "a scalar field grew arrows");

        let vectors = sample_grid(
            &FieldValue::new(EastField),
            CompRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            4,
            4,
            &Affine::IDENTITY,
            &ctx(),
        );
        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        paint_grid(
            &vectors,
            FieldDisplay::Arrows,
            FieldColorMap::Heat,
            1.0,
            &mut painter,
        );
        assert_eq!(painter.finish().len(), 16, "one arrow per cell");
    }

    /// A one-layer document whose network holds `nodes`, with `selected`
    /// selected inside it.
    fn context(nodes: Vec<ravel_core::graph::Node>, selected: Vec<NodeId>) -> OverlayContext {
        context_starting_at(nodes, selected, 0)
    }

    /// `start` places the layer on the composition timeline, so a test can put
    /// the playhead before the layer begins.
    fn context_starting_at(
        nodes: Vec<ravel_core::graph::Node>,
        selected: Vec<NodeId>,
        start: i64,
    ) -> OverlayContext {
        use ravel_core::composition::{Composition, Document, Layer};
        use ravel_core::graph::Graph;
        use ravel_core::id::{CompId, LayerId};
        use ravel_core::types::FrameRate;

        let mut graph = Graph::new();
        for node in nodes {
            graph = graph.add_node(node).unwrap();
        }
        let (comp_id, layer_id) = (CompId::next(), LayerId::next());
        let comp = Composition::new(comp_id, "Comp", (100, 100), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(layer_id, "Layer", graph).with_time(start, 0, 300));
        OverlayContext {
            resolution: Some((100, 100)),
            playback: Some(crate::panels::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            document: Some(Document::default().with_composition(comp)),
            selection: Some(crate::panels::CanvasSelection {
                path: Some(NetworkPath::layer(comp_id, layer_id)),
                nodes: selected.into_iter().collect(),
            }),
            field_display: FieldDisplay::Heatmap,
            field_opacity: DEFAULT_FIELD_OPACITY,
            ..OverlayContext::default()
        }
    }

    fn node_with_output(type_key: &str, data_type: DataTypeId) -> ravel_core::graph::Node {
        ravel_core::graph::Node::new(NodeId::next(), type_key).with_output("out", data_type)
    }

    /// Completion criterion: selecting a node with no `FIELD` output leaves
    /// the overlay inactive, so it neither requests an evaluation nor draws.
    #[test]
    fn a_node_without_a_field_output_activates_nothing() {
        let node = node_with_output("shape.rect", DataTypeId::GEOMETRY);
        let ctx = context(vec![node.clone()], vec![node.id]);

        assert!(selected_field_node(&ctx).is_none());
        assert!(!FieldOverlay.is_active(&ctx));
        assert!(FieldOverlay.eval_targets(&ctx).is_empty());

        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        FieldOverlay.paint(&ctx, &mut painter);
        assert!(painter.finish().is_empty());
    }

    /// A field node **does** activate it, and the target it declares names the
    /// port the field comes out of.
    #[test]
    fn a_field_node_declares_its_own_output_as_the_target() {
        let node = node_with_output("field.noise", DataTypeId::FIELD);
        let ctx = context(vec![node.clone()], vec![node.id]);

        assert!(FieldOverlay.is_active(&ctx));
        let targets = FieldOverlay.eval_targets(&ctx);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].node, node.id);
        assert_eq!(targets[0].output, OutputPortIndex(0));

        // The result has not arrived, so nothing is drawn — no field of
        // zeroes standing in for one that has not been evaluated.
        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        FieldOverlay.paint(&ctx, &mut painter);
        assert!(painter.finish().is_empty());
    }

    /// Two selected field nodes have no single picture to draw: two heatmaps
    /// over each other say nothing about either.
    #[test]
    fn two_selected_field_nodes_draw_nothing() {
        let a = node_with_output("field.noise", DataTypeId::FIELD);
        let b = node_with_output("field.radial", DataTypeId::FIELD);
        let ctx = context(vec![a.clone(), b.clone()], vec![a.id, b.id]);

        assert!(selected_field_node(&ctx).is_none());
        assert!(!FieldOverlay.is_active(&ctx));
    }

    /// A layer that has not started composites as transparent, so the field
    /// overlay must not draw over it — even handed a result, which is a state
    /// only a stale snapshot could produce, because
    /// `ProjectState::overlay_scoped_targets` refuses to request one.
    ///
    /// Defence in depth on purpose: the sampling frame and the interval check
    /// that decided to request the field have to agree, and the clamped
    /// `Layer::local_frame` disagrees with it for exactly these frames.
    #[test]
    fn a_layer_that_has_not_started_draws_no_field() {
        let node = node_with_output("field.noise", DataTypeId::FIELD);
        // Playhead at composition frame 0, layer starting at 5.
        let mut ctx = context_starting_at(vec![node.clone()], vec![node.id], 5);
        let network = ctx.selection.as_ref().unwrap().path.clone().unwrap();
        ctx.results = OverlayResults::new(std::collections::HashMap::from([(
            (network.segments(), node.id),
            std::sync::Arc::new(FieldValue::new(RampField))
                as std::sync::Arc<dyn ravel_core::types::NodeData>,
        )]));

        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        FieldOverlay.paint(&ctx, &mut painter);
        assert!(
            painter.finish().is_empty(),
            "the field was drawn over a layer that is not on screen"
        );

        // The same snapshot on the first frame the layer *is* on screen draws.
        let mut shown = ctx.clone();
        shown.playback = Some(crate::panels::PlaybackPosition {
            frame: 5,
            fps: ravel_core::types::FrameRate::new(30, 1),
        });
        let mut painter = OverlayPainter::new(
            gpui::Bounds {
                origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                size: gpui::size(gpui::px(100.0), gpui::px(100.0)),
            },
            (100, 100),
        );
        FieldOverlay.paint(&shown, &mut painter);
        assert!(
            !painter.finish().is_empty(),
            "this test needs the drawing path it is written against"
        );
    }

    /// The grid is laid out in composition space but sampled in the field's
    /// own: a translated layer moves the picture, it does not resample the
    /// same coordinates.
    #[test]
    fn the_grid_is_sampled_in_the_networks_own_coordinates() {
        let shell = Affine([1.0, 0.0, 40.0, 0.0, 1.0, 0.0]);
        let grid = sample_grid(
            &FieldValue::new(RampField),
            CompRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            4,
            1,
            &shell.inverse().expect("a translation inverts"),
            &ctx(),
        );
        // Cell centre 12.5 in composition space is 12.5 - 40 in the layer's.
        assert_eq!(grid.scalars().unwrap()[0], -27.5);
    }
}

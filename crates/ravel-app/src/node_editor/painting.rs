// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, NodeId};
use ravel_widgets::tokens::Colors;

use super::load_colors;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ravel_core::registry::NodeCategory;

use super::bezier::{BezierPath, horizontal_bezier, vertical_bezier};
use super::port_colors::{PortShape, category_color, port_color, port_shape};
use super::viewport::Viewport;
use crate::assets::RavelIcon;
use ravel_ui::node_editor::FlowDirection;

/// Display value for an animated channel without an evaluation context:
/// the constant value, the curve's frame-0 sample, or 0 for
/// not-yet-resolvable sources.
/// The number a channel shows in a node body, or `Err` with the text to show
/// instead. An expression has no value without a context, and `0.00` would be
/// a number the parameter does not hold; its source is what the node stores,
/// and the row elides it like any other over-wide value.
fn channel_sample(ch: &ravel_core::animation::channel::AnimationChannel) -> Result<f32, &str> {
    use ravel_core::animation::channel::ChannelSource;
    Ok(match &ch.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(0.0),
        ChannelSource::Expression(expression) => return Err(expression.source()),
        _ => 0.0,
    })
}

fn channel_display(ch: &ravel_core::animation::channel::AnimationChannel) -> String {
    match channel_sample(ch) {
        Ok(v) => format!("{v:.2}"),
        Err(source) => source.to_string(),
    }
}

/// The same for an animatable int: the row shows the integer the processor
/// reads, not the f32 curve underneath it.
fn channel_int_display(ch: &ravel_core::animation::channel::AnimationChannel) -> String {
    match channel_sample(ch) {
        Ok(v) => (v.round() as i32).to_string(),
        Err(source) => source.to_string(),
    }
}

fn channels_display(chs: &[ravel_core::animation::channel::AnimationChannel]) -> String {
    let parts: Vec<String> = chs.iter().map(channel_display).collect();
    format!("[{}]", parts.join(", "))
}

const BASE_NODE_WIDTH: f32 = 160.0;
/// A top-down node is a small box (about 3:1, Houdini's proportions) holding
/// only the category glyph; the name and parameter values sit *outside* it, to
/// its right. These are the box's height and its narrowest width at zoom 1.0.
const BASE_VERTICAL_BOX_H: f32 = 24.0;
const BASE_VERTICAL_BOX_MIN_W: f32 = 72.0;
/// Least width each port on a top/bottom edge gets, at zoom 1.0 — the hit
/// diameter, so two neighbouring dots never share a hit target; the box
/// widens past [`BASE_VERTICAL_BOX_MIN_W`] before they would.
const BASE_VERTICAL_PORT_SPACING: f32 = 2.0 * PORT_HIT_RADIUS;
/// Gap between the box and the label beside it, and the widest the label
/// column may get (names and values elide past it), both at zoom 1.0. The
/// column is as wide as the text actually is ([`LabelWidths`]) up to this cap.
const BASE_VERTICAL_LABEL_GAP: f32 = 6.0;
const BASE_VERTICAL_LABEL_W: f32 = 104.0;
/// Slack added to a measured text width, at zoom 1.0: the width is measured at
/// zoom 1 and scaled, and shaping is not exactly linear in the font size, so
/// without it paint could elide a name the size just made room for.
const LABEL_MEASURE_SLACK: f32 = 2.0;
/// Height of the name line in the label column.
const BASE_VERTICAL_NAME_H: f32 = 18.0;
/// The category glyph centred in the box.
const BASE_VERTICAL_ICON: f32 = 16.0;
/// Font sizes of the label column at zoom 1.0: the name and the value lines.
/// Measuring (panel) and painting both read these.
pub const VERTICAL_NAME_FONT: f32 = 12.0;
pub const VERTICAL_VALUE_FONT: f32 = 9.0;
const BASE_HEADER_H: f32 = 24.0;
const BASE_PORT_ROW_H: f32 = 18.0;
const BASE_PARAM_ROW_H: f32 = 16.0;
const BASE_NODE_PAD: f32 = 8.0;
const BASE_PORT_GAP: f32 = 4.0;
const BASE_PORT_DOT_R: f32 = 4.0;
const BASE_CORNER_R: f32 = 6.0;
const PORT_HIT_RADIUS: f32 = 10.0;
const SNAP_RADIUS: f32 = 20.0;
/// Port-name tag text size and padding, in screen pixels: the tag is an
/// overlay read at any zoom, so it does not scale with the canvas.
const PORT_LABEL_FONT: f32 = 10.0;
const PORT_LABEL_PAD: f32 = 4.0;
/// Alpha multiplier applied to every part of a bypassed node's painting.
const BYPASSED_OPACITY: f32 = 0.45;

/// Minimum gap between a parameter key and its right-aligned value, at zoom
/// 1.0. Values wider than what is left over are elided.
const BASE_PARAM_KEY_VALUE_GAP: f32 = 6.0;

/// Header glyph size at zoom 1.0, before quantization.
const BASE_HEADER_ICON: f32 = 12.0;
/// Gap between the header glyph and the label, at zoom 1.0.
const BASE_HEADER_ICON_GAP: f32 = 4.0;
/// Screen sizes (px) the header glyph may rasterize at. `paint_svg` keeps
/// one sprite-atlas entry per (path, size), so the continuous zoom range is
/// snapped to this ladder instead of allocating an atlas tile per zoom step.
const HEADER_ICON_SIZES: [f32; 5] = [8.0, 12.0, 16.0, 24.0, 32.0];
/// Below this many screen pixels the glyph is illegible and not drawn at
/// all — the same reasoning as the background grid dots, skipped under 5px.
const HEADER_ICON_MIN_PX: f32 = 6.0;

/// Atlas-friendly size for the header glyph at `zoom`, or `None` when the
/// glyph would be too small to read. Snapping to [`HEADER_ICON_SIZES`]
/// bounds the number of sprite-atlas entries a zoom session creates.
pub fn quantized_header_icon_size(zoom: f32) -> Option<f32> {
    quantize_icon_px(BASE_HEADER_ICON * zoom)
}

/// [`quantized_header_icon_size`] for an arbitrary raw pixel size.
fn quantize_icon_px(raw: f32) -> Option<f32> {
    if raw < HEADER_ICON_MIN_PX {
        return None;
    }
    HEADER_ICON_SIZES
        .iter()
        .copied()
        .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()))
}

/// The node body's screen size at `zoom`.
///
/// `show_param_values` must be the same value the paint pass is given
/// (`paint_nodes`): the parameter rows are part of the height, and measuring
/// them while not drawing them (or the reverse) leaves the node box and the
/// picture disagreeing about where the body ends. Every caller therefore
/// takes the flag from `panels::show_node_param_values`.
/// How many parameter rows a node body shows — `0` when the values are
/// hidden, whatever the node holds.
///
/// [`compute_node_size`] and [`paint_single_node`] both go through this, which
/// is the whole point: a body painted with rows the height did not reserve
/// spills over the node below it, and a height that reserved rows nobody
/// painted moves every port's hit target down by rows that are not there
/// (`MED-APP-13` was that disagreement). One function means the two cannot
/// drift apart — a change to the rule is a change to both.
pub fn painted_param_rows(node: &Node, show_param_values: bool) -> usize {
    if show_param_values {
        node.parameters.len()
    } else {
        0
    }
}

/// [`compute_node_size_labeled`] with the widest label column — the size of a
/// node nobody has measured text for (a fallback, and horizontal flow, which
/// ignores it).
pub fn compute_node_size(
    node: &Node,
    zoom: f32,
    show_param_values: bool,
    flow: FlowDirection,
) -> (f32, f32) {
    compute_node_size_labeled(node, zoom, show_param_values, flow, BASE_VERTICAL_LABEL_W)
}

/// What the top-down label column needs, measured at zoom 1.0: the name's
/// width and the widest `key: value` line. Measured by the panel where it
/// refreshes its sizes (the size function has no text system), cached by text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LabelWidths {
    pub name: f32,
    pub values: f32,
}

impl LabelWidths {
    /// The label column's width at zoom 1.0: the widest line that is drawn,
    /// capped. **The one source** for both the size ([`compute_node_size_labeled`])
    /// and the elision width paint uses (the extent minus the box).
    pub fn column(&self, show_param_values: bool) -> f32 {
        let widest = if show_param_values {
            self.name.max(self.values)
        } else {
            self.name
        };
        (widest + LABEL_MEASURE_SLACK).clamp(0.0, BASE_VERTICAL_LABEL_W)
    }
}

/// Measure a top-down node's label column. `measure(text, mono)` returns the
/// width of `text` at zoom 1.0 in the UI font (`mono = false`, the name) or
/// the monospace font (`mono = true`, the value lines).
pub fn measure_label_widths(
    node: &Node,
    label: &str,
    measure: &mut dyn FnMut(&str, bool) -> f32,
) -> LabelWidths {
    LabelWidths {
        name: measure(label, false),
        values: node
            .parameters
            .iter()
            .map(|p| measure(&param_line(p), true))
            .fold(0.0, f32::max),
    }
}

/// One value line in the label column.
fn param_line(param: &ravel_core::graph::Parameter) -> String {
    format!("{}: {}", param.key, param_value_text(&param.value))
}

/// [`compute_node_size`] with a measured label column: `label_w` is its width
/// at zoom 1.0 ([`LabelWidths::column`]).
pub fn compute_node_size_labeled(
    node: &Node,
    zoom: f32,
    show_param_values: bool,
    flow: FlowDirection,
    label_w: f32,
) -> (f32, f32) {
    let z = zoom;
    if flow == FlowDirection::Vertical {
        // The full extent: box plus the label column beside it. Clicking the
        // label selects the node, a marquee that touches it takes the node,
        // and layout / fit measure all of it. Ports are on the box only
        // ([`vertical_box_size`]).
        let (box_w, box_h) = vertical_box_size(node, z);
        let rows = painted_param_rows(node, show_param_values);
        let label_h = (BASE_VERTICAL_NAME_H + rows as f32 * BASE_PARAM_ROW_H) * z;
        return (
            box_w + (BASE_VERTICAL_LABEL_GAP + label_w.clamp(0.0, BASE_VERTICAL_LABEL_W)) * z,
            box_h.max(label_h),
        );
    }
    let port_rows = node.inputs.len().max(node.outputs.len());
    let param_rows = painted_param_rows(node, show_param_values);
    let sep = if param_rows > 0 { 6.0 * z } else { 0.0 };
    let h = BASE_NODE_PAD * z
        + BASE_HEADER_H * z
        + BASE_PORT_GAP * z
        + port_rows as f32 * BASE_PORT_ROW_H * z
        + sep
        + param_rows as f32 * BASE_PARAM_ROW_H * z
        + BASE_NODE_PAD * z;
    (BASE_NODE_WIDTH * z, h)
}

/// The top-down node's box (the part that carries the ports), at `zoom`.
/// A pure function of the node: it does not depend on the label or on the
/// parameter-values flag, so a port stays put when either changes.
pub fn vertical_box_size(node: &Node, zoom: f32) -> (f32, f32) {
    let ports = node.inputs.len().max(node.outputs.len()) as f32;
    (
        BASE_VERTICAL_BOX_MIN_W.max(ports * BASE_VERTICAL_PORT_SPACING) * zoom,
        BASE_VERTICAL_BOX_H * zoom,
    )
}

/// A node's drawn size, falling back to what [`compute_node_size`] gives
/// without parameter rows when the cache has no entry yet.
pub fn node_size_or_default(
    sizes: &HashMap<NodeId, (f32, f32)>,
    node: &Node,
    zoom: f32,
    flow: FlowDirection,
) -> (f32, f32) {
    sizes
        .get(&node.id)
        .copied()
        .unwrap_or_else(|| compute_node_size(node, zoom, false, flow))
}

/// Screen centre of port `index` on `node`: the **one** place the flow axis
/// enters port geometry. Painting, hit testing, snapping and edge endpoints
/// all call this, so they cannot disagree about where a dot is.
///
/// Horizontal puts inputs on the left edge and outputs on the right, one per
/// row; vertical spreads them evenly across the top (inputs) or bottom
/// (outputs) edge of the node's *box* ([`vertical_box_size`]) — never of the
/// label beside it.
pub fn port_screen_center(
    node: &Node,
    node_screen: (f32, f32),
    zoom: f32,
    flow: FlowDirection,
    is_output: bool,
    port_index: usize,
) -> (f32, f32) {
    match flow {
        FlowDirection::Horizontal => {
            if is_output {
                output_port_screen_center(node_screen, port_index, zoom)
            } else {
                input_port_screen_center(node_screen, port_index, zoom)
            }
        }
        FlowDirection::Vertical => {
            let count = if is_output {
                node.outputs.len()
            } else {
                node.inputs.len()
            }
            .max(1);
            let (box_w, box_h) = vertical_box_size(node, zoom);
            let x = node_screen.0 + box_w * (port_index as f32 + 0.5) / count as f32;
            let y = if is_output {
                node_screen.1 + box_h
            } else {
                node_screen.1
            };
            (x, y)
        }
    }
}

pub fn input_port_screen_center(
    node_screen: (f32, f32),
    port_index: usize,
    zoom: f32,
) -> (f32, f32) {
    let z = zoom;
    let y = node_screen.1
        + BASE_NODE_PAD * z
        + BASE_HEADER_H * z
        + BASE_PORT_GAP * z
        + (port_index as f32 + 0.5) * BASE_PORT_ROW_H * z;
    (node_screen.0, y)
}

pub fn output_port_screen_center(
    node_screen: (f32, f32),
    port_index: usize,
    zoom: f32,
) -> (f32, f32) {
    let z = zoom;
    let y = node_screen.1
        + BASE_NODE_PAD * z
        + BASE_HEADER_H * z
        + BASE_PORT_GAP * z
        + (port_index as f32 + 0.5) * BASE_PORT_ROW_H * z;
    (node_screen.0 + BASE_NODE_WIDTH * z, y)
}

pub fn paint_background(bounds: &Bounds<Pixels>, bg: Hsla, window: &mut Window) {
    window.paint_quad(fill(*bounds, bg));
}

pub fn paint_grid(
    bounds: &Bounds<Pixels>,
    viewport: &Viewport,
    colors: &Colors,
    window: &mut Window,
) {
    let spacing = 20.0 * viewport.zoom;
    if spacing < 5.0 {
        return;
    }

    let dot_color = Hsla {
        alpha: 0.3,
        ..colors.border
    };
    let dot_size = 1.5 * viewport.zoom.min(1.0);
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let bw: f32 = bounds.size.width.into();
    let bh: f32 = bounds.size.height.into();

    let start_x = viewport.x.rem_euclid(spacing);
    let start_y = viewport.y.rem_euclid(spacing);

    let mut x = start_x;
    while x < bw {
        let mut y = start_y;
        while y < bh {
            let dot = Bounds::new(
                Point::new(px(ox + x - dot_size / 2.0), px(oy + y - dot_size / 2.0)),
                Size {
                    width: px(dot_size),
                    height: px(dot_size),
                },
            );
            window.paint_quad(fill(dot, dot_color));
            y += spacing;
        }
        x += spacing;
    }
}

/// Screen positions of an edge's two ports, shared by painting and the edge
/// hit test.
fn edge_endpoints(
    edge: &ravel_core::graph::Edge,
    src_node: &Node,
    tgt_node: &Node,
    viewport: &Viewport,
    flow: FlowDirection,
) -> ((f32, f32), (f32, f32)) {
    let at = |node: &Node, is_output: bool, port: usize| {
        let screen = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);
        port_screen_center(node, screen, viewport.zoom, flow, is_output, port)
    };
    (
        at(src_node, true, edge.source_port.0 as usize),
        at(tgt_node, false, edge.target_port.0 as usize),
    )
}

/// The cubic an edge follows, leaving along the flow axis.
fn flow_bezier(flow: FlowDirection, sx: f32, sy: f32, tx: f32, ty: f32) -> BezierPath {
    match flow {
        FlowDirection::Horizontal => horizontal_bezier(sx, sy, tx, ty, 0.25),
        FlowDirection::Vertical => vertical_bezier(sx, sy, tx, ty, 0.25),
    }
}

/// The four corners of a `Step` edge: it runs along the flow axis, jogs across
/// halfway, and runs on into the target.
fn step_points(flow: FlowDirection, sx: f32, sy: f32, tx: f32, ty: f32) -> [(f32, f32); 4] {
    match flow {
        FlowDirection::Horizontal => {
            let mid_x = (sx + tx) / 2.0;
            [(sx, sy), (mid_x, sy), (mid_x, ty), (tx, ty)]
        }
        FlowDirection::Vertical => {
            let mid_y = (sy + ty) / 2.0;
            [(sx, sy), (sx, mid_y), (tx, mid_y), (tx, ty)]
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint_edges(
    graph: &Graph,
    viewport: &Viewport,
    bounds: &Bounds<Pixels>,
    selected_edges: &HashSet<EdgeId>,
    edge_style: super::EdgeStyle,
    flow: FlowDirection,
    colors: &Colors,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let normal_color: Hsla = Hsla {
        alpha: 0.6,
        ..colors.muted_foreground
    };

    for edge in graph.edges() {
        let src_node = match graph.node(edge.source) {
            Some(n) => n,
            None => continue,
        };
        let tgt_node = match graph.node(edge.target) {
            Some(n) => n,
            None => continue,
        };
        if src_node.metadata.synthetic || tgt_node.metadata.synthetic {
            continue;
        }

        let ((sx, sy), (tx, ty)) = edge_endpoints(edge, src_node, tgt_node, viewport, flow);

        let sx = sx + ox;
        let sy = sy + oy;
        let tx = tx + ox;
        let ty = ty + oy;

        let highlight = colors.primary;
        let is_selected = selected_edges.contains(&edge.id);
        let color = if is_selected { highlight } else { normal_color };
        let stroke_w = if is_selected { 3.0 } else { 2.0 };

        match edge_style {
            super::EdgeStyle::Bezier => {
                let path = flow_bezier(flow, sx, sy, tx, ty);
                let mut builder = PathBuilder::stroke(px(stroke_w));
                builder.move_to(Point::new(px(path.source.0), px(path.source.1)));
                builder.cubic_bezier_to(
                    Point::new(px(path.target.0), px(path.target.1)),
                    Point::new(px(path.source_control.0), px(path.source_control.1)),
                    Point::new(px(path.target_control.0), px(path.target_control.1)),
                );
                if let Ok(p) = builder.build() {
                    window.paint_path(p, color);
                }
                paint_arrowhead(
                    window,
                    tx,
                    ty,
                    path.target_control.0,
                    path.target_control.1,
                    color,
                );
            }
            super::EdgeStyle::Straight => {
                let mut builder = PathBuilder::stroke(px(stroke_w));
                builder.move_to(Point::new(px(sx), px(sy)));
                builder.line_to(Point::new(px(tx), px(ty)));
                if let Ok(p) = builder.build() {
                    window.paint_path(p, color);
                }
                paint_arrowhead(window, tx, ty, sx, sy, color);
            }
            super::EdgeStyle::Step => {
                let points = step_points(flow, sx, sy, tx, ty);
                let mut builder = PathBuilder::stroke(px(stroke_w));
                builder.move_to(Point::new(px(points[0].0), px(points[0].1)));
                for corner in &points[1..] {
                    builder.line_to(Point::new(px(corner.0), px(corner.1)));
                }
                if let Ok(p) = builder.build() {
                    window.paint_path(p, color);
                }
                paint_arrowhead(window, tx, ty, points[2].0, points[2].1, color);
            }
        }
    }
}

fn paint_arrowhead(
    window: &mut Window,
    tip_x: f32,
    tip_y: f32,
    from_x: f32,
    from_y: f32,
    color: Hsla,
) {
    let dx = tip_x - from_x;
    let dy = tip_y - from_y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.001 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let perpx = -uy;
    let perpy = ux;

    let arrow_len = 6.0;
    let half_w = 3.0;
    let bx = tip_x - ux * arrow_len;
    let by = tip_y - uy * arrow_len;

    let mut builder = PathBuilder::fill();
    builder.move_to(Point::new(px(tip_x), px(tip_y)));
    builder.line_to(Point::new(px(bx + perpx * half_w), px(by + perpy * half_w)));
    builder.line_to(Point::new(px(bx - perpx * half_w), px(by - perpy * half_w)));
    builder.line_to(Point::new(px(tip_x), px(tip_y)));
    if let Ok(p) = builder.build() {
        window.paint_path(p, color);
    }
}

/// Per-node load readout thresholds: within roughly a quarter frame budget
/// the readout stays muted, above it turns yellow, and past a full 30 fps
/// frame budget (33 ms) it turns red.
const TIMING_WARN: Duration = Duration::from_millis(8);
const TIMING_CRITICAL: Duration = Duration::from_millis(33);

/// Write the compact display of an evaluation duration into `out`, replacing
/// its contents (e.g. `0.4ms`, `12ms`, `1.2s`).
///
/// The single place the rounding is decided. Callers on the repaint gate
/// reuse one buffer across nodes, which is why this exists next to
/// [`format_eval_duration`] instead of behind it.
pub fn write_eval_duration(out: &mut String, duration: Duration) {
    use std::fmt::Write as _;
    out.clear();
    let ms = duration.as_secs_f64() * 1000.0;
    let written = if ms >= 1000.0 {
        write!(out, "{:.1}s", ms / 1000.0)
    } else if ms >= 10.0 {
        write!(out, "{:.0}ms", ms)
    } else {
        write!(out, "{:.1}ms", ms)
    };
    // Writing into a String is infallible; the Result only exists because
    // `write!` is generic over the sink.
    debug_assert!(written.is_ok());
}

/// Compact display of a node's evaluation duration (e.g. `0.4ms`, `12ms`,
/// `1.2s`).
pub fn format_eval_duration(duration: Duration) -> String {
    let mut out = String::new();
    write_eval_duration(&mut out, duration);
    out
}

/// Load band of a readout: the three states [`eval_duration_color`] paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingLevel {
    Normal,
    Warn,
    Critical,
}

impl TimingLevel {
    fn of(duration: Duration) -> Self {
        if duration >= TIMING_CRITICAL {
            Self::Critical
        } else if duration >= TIMING_WARN {
            Self::Warn
        } else {
            Self::Normal
        }
    }

    fn color(self, colors: &Colors) -> Hsla {
        match self {
            Self::Critical => load_colors::CRITICAL_COLOR,
            Self::Warn => load_colors::WARN_COLOR,
            Self::Normal => colors.muted_foreground,
        }
    }
}

/// Load color of the readout: muted → yellow → red as the node gets more
/// expensive.
pub fn eval_duration_color(duration: Duration, colors: &Colors) -> Hsla {
    TimingLevel::of(duration).color(colors)
}

/// Everything the load readout draws for one node, derived from the raw
/// measurement once.
///
/// This is the display grain of a timing: the panel stores these instead of
/// raw `Duration`s so a measurement that moves without moving the readout
/// (`12.3ms` → `12.4ms`, both drawn as `12ms`, both muted) costs no repaint.
/// Deriving the text and the color band together is what makes the grain
/// safe — a change that keeps the text but crosses `TIMING_WARN` or
/// `TIMING_CRITICAL` (`7.96ms` → `8.04ms`, both drawn as `8.0ms`) still
/// compares unequal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalReadout {
    text: SharedString,
    level: TimingLevel,
}

impl EvalReadout {
    /// The one place a raw duration is reduced to what the readout shows.
    pub fn of(duration: Duration) -> Self {
        Self {
            text: format_eval_duration(duration).into(),
            level: TimingLevel::of(duration),
        }
    }

    /// [`Self::of`] for the repaint gate, which rebuilds every node's readout
    /// on every evaluation result — during playback, once a frame.
    ///
    /// `scratch` is a caller-owned buffer reused across nodes, so no `String`
    /// is allocated per node; a readout is far below `SharedString`'s inline
    /// capacity, so building one from the buffer does not allocate either.
    /// [`Self::of`] is the allocating convenience for one-off callers.
    pub fn written(duration: Duration, scratch: &mut String) -> Self {
        write_eval_duration(scratch, duration);
        Self {
            text: SharedString::from(scratch.as_str()),
            level: TimingLevel::of(duration),
        }
    }
}

/// Visible (non-synthetic) nodes in paint order: ascending `metadata.z`,
/// ties keeping graph iteration order (stable sort). The last element
/// paints frontmost; hit tests walk the same order and keep the last hit
/// so painting and picking always agree.
pub fn z_ordered(graph: &Graph) -> Vec<&std::sync::Arc<Node>> {
    let mut nodes: Vec<_> = graph.nodes().filter(|n| !n.metadata.synthetic).collect();
    nodes.sort_by_key(|n| n.metadata.z);
    nodes
}

#[allow(clippy::too_many_arguments)]
pub fn paint_nodes(
    graph: &Graph,
    viewport: &Viewport,
    bounds: &Bounds<Pixels>,
    selected: &HashSet<NodeId>,
    node_sizes: &HashMap<NodeId, (f32, f32)>,
    timings: &HashMap<NodeId, EvalReadout>,
    categories: &HashMap<NodeId, NodeCategory>,
    labels: &HashMap<NodeId, String>,
    show_param_values: bool,
    flow: FlowDirection,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let bw: f32 = bounds.size.width.into();
    let bh: f32 = bounds.size.height.into();
    let z = viewport.zoom;

    for node in z_ordered(graph) {
        let (sw, sh) = node_sizes
            .get(&node.id)
            .copied()
            .unwrap_or_else(|| compute_node_size(node, z, show_param_values, flow));
        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        if sx + sw < -50.0 || sx > bw + 50.0 || sy + sh < -50.0 || sy > bh + 50.0 {
            continue;
        }

        let wx = ox + sx;
        let wy = oy + sy;
        let is_selected = selected.contains(&node.id);

        paint_single_node(
            node,
            labels
                .get(&node.id)
                .map(String::as_str)
                .unwrap_or(&node.type_key),
            wx,
            wy,
            sw,
            sh,
            is_selected,
            categories.get(&node.id).copied(),
            z,
            show_param_values,
            flow,
            colors,
            window,
            cx,
        );

        // Load readout below the node (evaluation wall-clock time). Hidden
        // while bypassed: the pass-through records no timings, so the
        // readout would show a stale pre-bypass measurement.
        if !node.metadata.bypassed
            && let Some(readout) = timings.get(&node.id)
        {
            paint_mono_text(
                &readout.text,
                Point::new(
                    px(wx + BASE_NODE_PAD * z),
                    // Below the output dots too in top-down flow, where they
                    // straddle the box's bottom edge.
                    px(wy
                        + sh
                        + if flow == FlowDirection::Vertical {
                            BASE_PORT_DOT_R * z
                        } else {
                            0.0
                        }
                        + 2.0 * z),
                ),
                9.0 * z,
                readout.level.color(colors),
                window,
                cx,
            );
        }
    }
}

/// Alpha of the category tint painted over the node header.
const HEADER_TINT_ALPHA: f32 = 0.18;

#[allow(clippy::too_many_arguments)]
fn paint_single_node(
    node: &Node,
    label: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    selected: bool,
    category: Option<NodeCategory>,
    z: f32,
    show_param_values: bool,
    flow: FlowDirection,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    let pad = BASE_NODE_PAD * z;
    let header_h = BASE_HEADER_H * z;
    let port_row_h = BASE_PORT_ROW_H * z;
    let port_gap = BASE_PORT_GAP * z;
    let dot_r = BASE_PORT_DOT_R * z;
    let corner_r = BASE_CORNER_R * z;
    let font_header = 12.0 * z;
    let font_port = 10.0 * z;
    let font_param = 9.0 * z;

    // Bypassed nodes paint semi-transparent: the node is inert (its input is
    // passed through), so it recedes like a muted element.
    let opacity = if node.metadata.bypassed {
        BYPASSED_OPACITY
    } else {
        1.0
    };
    let dim = |color: Hsla| Hsla {
        alpha: color.alpha * opacity,
        ..color
    };

    let node_bg = dim(Hsla {
        alpha: 0.95,
        ..colors.background
    });
    let highlight = colors.primary;
    let node_border = dim(if selected { highlight } else { colors.border });
    let border_w = if selected { 2.0 } else { 1.0 };

    // The outlined body: the whole node in horizontal flow, only the small
    // box in top-down flow (its label is drawn outside, beside it). Selection,
    // bypass and the outline all live on this rect.
    let vertical = flow == FlowDirection::Vertical;
    let (face_w, face_h) = if vertical {
        vertical_box_size(node, z)
    } else {
        (w, h)
    };
    let node_bounds = Bounds::new(
        Point::new(px(x), px(y)),
        Size {
            width: px(face_w),
            height: px(face_h),
        },
    );

    window.paint_quad(fill(node_bounds, node_bg).corner_radii(px(corner_r)));

    // Header tint: the category color (port-palette hues, see
    // `category_color`) at low alpha over the whole header strip. The
    // strip is taller than the corner radius, so its rounded top corners
    // coincide exactly with the node outline (a thinner bar would bulge
    // past it).
    if let Some(category) = category {
        let tint = Hsla {
            alpha: HEADER_TINT_ALPHA,
            ..category_color(category)
        };
        // Top-down: the box is all header, so the wash covers all of it.
        let header = Bounds::new(
            Point::new(px(x), px(y)),
            Size {
                width: px(face_w),
                height: if vertical {
                    px(face_h)
                } else {
                    px(pad + header_h)
                },
            },
        );
        let bottom_r = if vertical { corner_r } else { 0.0 };
        window.paint_quad(fill(header, dim(tint)).corner_radii(Corners {
            top_left: px(corner_r),
            top_right: px(corner_r),
            bottom_left: px(bottom_r),
            bottom_right: px(bottom_r),
        }));
    }

    window.paint_quad(
        outline(node_bounds, node_border, BorderStyle::default())
            .corner_radii(px(corner_r))
            .border_widths(px(border_w)),
    );

    let sep_y = y + pad + header_h;
    if !vertical {
        // Resolved by the host (`crate::node_locale::display_label`): a user
        // rename, else the locale entry for the type, else the type key.
        let mut label_x = x + pad;
        // Type glyph at the header's left edge, in the category color: the tint
        // is a low-alpha wash of the same hue, so the header reads as a pale
        // face with a saturated mark. Neither the header height nor the node
        // width changes — the label simply starts after the glyph. The glyph is
        // skipped at low zoom (see `quantized_header_icon_size`).
        if let Some(icon_size) = quantized_header_icon_size(z) {
            let icon = RavelIcon::for_node_type(&node.type_key, category);
            let icon_color = dim(category
                .map(category_color)
                .unwrap_or(colors.muted_foreground));
            // Vertically centered against the label's line box.
            let icon_y = y + pad + 2.0 * z + (font_header * 1.4 - icon_size) / 2.0;
            window
                .paint_svg(
                    Bounds::new(
                        Point::new(px(label_x), px(icon_y)),
                        Size {
                            width: px(icon_size),
                            height: px(icon_size),
                        },
                    ),
                    icon.icon_path(),
                    None,
                    TransformationMatrix::default(),
                    icon_color,
                    cx,
                )
                .ok();
            label_x += icon_size + BASE_HEADER_ICON_GAP * z;
        }
        // Elided against the node's right edge: a localized title
        // ("カーブリマップフィールド") is far wider than the fixed node box.
        let label_avail = (x + w - pad - label_x).max(0.0);
        shape_elided(
            label,
            font_header,
            &crate::fonts::ui_font(cx),
            dim(colors.foreground),
            label_avail,
            window,
        )
        .paint(
            Point::new(px(label_x), px(y + pad + 2.0 * z)),
            px(font_header * 1.4),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();

        let sep_bounds = Bounds::new(
            Point::new(px(x + 4.0 * z), px(sep_y)),
            Size {
                width: px(w - 8.0 * z),
                height: px(1.0),
            },
        );
        window.paint_quad(fill(
            sep_bounds,
            dim(Hsla {
                alpha: 0.2,
                ..colors.border
            }),
        ));
    }
    if vertical {
        paint_vertical_label(
            node,
            label,
            (x, y),
            (face_w, face_h),
            // The label column is whatever the extent leaves beside the box:
            // elision uses the very width the size was measured to.
            w - face_w - BASE_VERTICAL_LABEL_GAP * z,
            category,
            z,
            show_param_values,
            opacity,
            colors,
            window,
            cx,
        );
    }
    let port_base_y = sep_y + port_gap;

    for (i, input) in node.inputs.iter().enumerate() {
        let (pcx, py) = port_screen_center(node, (x, y), z, flow, false, i);
        // Parameter inputs keep the same center and hit target as ordinary
        // inputs, but render slightly smaller so their role is visible.
        let input_dot_r = if input.is_param { dot_r * 0.75 } else { dot_r };
        let dot_color = dim(input
            .accepted_types
            .first()
            .map(|t| port_color(*t))
            .unwrap_or(colors.muted_foreground));
        let shape = input
            .accepted_types
            .first()
            .map(|t| port_shape(*t))
            .unwrap_or(PortShape::Circle);

        paint_port_marker(window, (pcx, py), input_dot_r, shape, dot_color);

        // Top-down nodes draw no port-name rows; the name appears on hover
        // (`paint_port_label`).
        if vertical {
            continue;
        }
        paint_text(
            &input.name,
            Point::new(px(x + dot_r + 4.0 * z), px(py - 5.0 * z)),
            font_port,
            dim(colors.muted_foreground),
            window,
            cx,
        );
    }

    for (i, output) in node.outputs.iter().enumerate() {
        let (pcx, py) = port_screen_center(node, (x, y), z, flow, true, i);
        let dot_color = dim(port_color(output.data_type));

        paint_port_marker(
            window,
            (pcx, py),
            dot_r,
            port_shape(output.data_type),
            dot_color,
        );
        if vertical {
            continue;
        }

        let text: SharedString = output.name.as_str().into();
        let len = text.len();
        let shaped = window.text_system().shape_line(
            text,
            px(font_port),
            &[TextRun {
                len,
                font: crate::fonts::ui_font(cx),
                color: dim(colors.muted_foreground),
                background_color: None,
                underline: None,
                strikethrough: None,
                letter_spacing: None,
            }],
            None,
        );
        let tw: f32 = shaped.width.into();
        shaped
            .paint(
                Point::new(px(x + w - dot_r - 4.0 * z - tw), px(py - 5.0 * z)),
                px(font_port * 1.4),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
    }

    // Skipped wholesale when the rows are off, separator included: the
    // height `compute_node_size` returned for this node left no room for
    // either (the two read the same flag).
    // Top-down nodes list their values in the label column instead
    // ([`paint_vertical_label`]).
    let rows = painted_param_rows(node, show_param_values);
    if rows > 0 && !vertical {
        let param_row_h = BASE_PARAM_ROW_H * z;
        let params_base_y =
            port_base_y + node.inputs.len().max(node.outputs.len()) as f32 * port_row_h + 6.0 * z;

        let sep2 = Bounds::new(
            Point::new(px(x + 4.0 * z), px(params_base_y - 3.0 * z)),
            Size {
                width: px(w - 8.0 * z),
                height: px(1.0),
            },
        );
        window.paint_quad(fill(
            sep2,
            dim(Hsla {
                alpha: 0.2,
                ..colors.border
            }),
        ));

        for (i, param) in node.parameters.iter().enumerate().take(rows) {
            let py = params_base_y + i as f32 * param_row_h;
            let key_w = paint_mono_text_measured(
                &param.key,
                Point::new(px(x + pad), px(py)),
                font_param,
                dim(colors.muted_foreground),
                window,
                cx,
            );
            let val_str = param_value_text(&param.value);
            // The value is right-aligned against a fixed node width, so a long
            // one — a vec4, a long string — would otherwise run back over the
            // key on its left.
            let avail = (w - pad * 2.0 - key_w - BASE_PARAM_KEY_VALUE_GAP * z).max(0.0);
            let mono = crate::fonts::mono_font(cx);
            let shaped = shape_elided(
                &val_str,
                font_param,
                &mono,
                dim(colors.foreground),
                avail,
                window,
            );
            let tw: f32 = shaped.width.into();
            shaped
                .paint(
                    Point::new(px(x + w - pad - tw), px(py)),
                    px(font_param * 1.4),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
        }
    }
}

/// The text a node body shows for a parameter value.
fn param_value_text(value: &ParameterValue) -> String {
    match value {
        ParameterValue::Float(v) => format!("{v:.2}"),
        ParameterValue::Int(v) => v.to_string(),
        ParameterValue::Bool(v) => v.to_string(),
        ParameterValue::String(v) => v.clone(),
        // No evaluation context in a node body, so the frame-0 sample
        // stands in — the same stand-in `channel_sample` uses for a
        // keyframe curve.
        ParameterValue::StringSteps(steps) => steps.sample(0.0).clone(),
        ParameterValue::Channel(ch) => channel_display(ch),
        ParameterValue::IntChannel(ch) => channel_int_display(ch),
        ParameterValue::Channel2(chs) => channels_display(chs),
        ParameterValue::Channel3(chs) => channels_display(chs),
        ParameterValue::Channel4(chs) => channels_display(chs),
        ParameterValue::PathPoints(points) => format!("{} points", points.len()),
        ParameterValue::Curve(curve) => format!("{} points", curve.len()),
        ParameterValue::Ramp(ramp) => format!("{} stops", ramp.len()),
    }
}

/// The top-down node's face: the category glyph centred in the box, and the
/// name — with the parameter values small and muted under it — in a column to
/// the box's right. The column is part of the node's extent
/// ([`compute_node_size`]) so it is clickable, but it is not outlined: the box
/// carries the selection and bypass state.
#[allow(clippy::too_many_arguments)]
fn paint_vertical_label(
    node: &Node,
    label: &str,
    origin: (f32, f32),
    box_size: (f32, f32),
    column_w: f32,
    category: Option<NodeCategory>,
    z: f32,
    show_param_values: bool,
    opacity: f32,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    let (x, y) = origin;
    let dim = |color: Hsla| Hsla {
        alpha: color.alpha * opacity,
        ..color
    };
    if let Some(icon_size) = quantize_icon_px(BASE_VERTICAL_ICON * z) {
        let icon = RavelIcon::for_node_type(&node.type_key, category);
        let icon_color = dim(category
            .map(category_color)
            .unwrap_or(colors.muted_foreground));
        window
            .paint_svg(
                Bounds::new(
                    Point::new(
                        px(x + (box_size.0 - icon_size) / 2.0),
                        px(y + (box_size.1 - icon_size) / 2.0),
                    ),
                    Size {
                        width: px(icon_size),
                        height: px(icon_size),
                    },
                ),
                icon.icon_path(),
                None,
                TransformationMatrix::default(),
                icon_color,
                cx,
            )
            .ok();
    }

    let col_x = x + box_size.0 + BASE_VERTICAL_LABEL_GAP * z;
    let avail = column_w.max(0.0);
    let font_name = VERTICAL_NAME_FONT * z;
    shape_elided(
        label,
        font_name,
        &crate::fonts::ui_font(cx),
        dim(colors.foreground),
        avail,
        window,
    )
    .paint(
        Point::new(
            px(col_x),
            px(y + (BASE_VERTICAL_NAME_H * z - font_name * 1.4) / 2.0),
        ),
        px(font_name * 1.4),
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();

    let rows = painted_param_rows(node, show_param_values);
    let font_param = VERTICAL_VALUE_FONT * z;
    let mono = crate::fonts::mono_font(cx);
    for (i, param) in node.parameters.iter().enumerate().take(rows) {
        let text = param_line(param);
        shape_elided(
            &text,
            font_param,
            &mono,
            dim(colors.muted_foreground),
            avail,
            window,
        )
        .paint(
            Point::new(
                px(col_x),
                px(y + (BASE_VERTICAL_NAME_H + i as f32 * BASE_PARAM_ROW_H) * z),
            ),
            px(font_param * 1.4),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    }
}

/// Paint one port marker centered at `center`; `r` is half the bounding
/// box of the circle the marker replaces. Path-drawn shapes extend
/// slightly past `r` so their area reads like the circle's, but all stay
/// well inside the hit radius — port hit testing and edge anchors are
/// center-based and unaffected by the silhouette.
fn paint_port_marker(
    window: &mut Window,
    center: (f32, f32),
    r: f32,
    shape: PortShape,
    color: Hsla,
) {
    let (cx, cy) = center;
    match shape {
        PortShape::Circle | PortShape::RoundedSquare => {
            let bounds = Bounds::new(
                Point::new(px(cx - r), px(cy - r)),
                Size {
                    width: px(r * 2.0),
                    height: px(r * 2.0),
                },
            );
            let corner = if shape == PortShape::Circle {
                r
            } else {
                r * 0.35
            };
            window.paint_quad(fill(bounds, color).corner_radii(px(corner)));
        }
        PortShape::Diamond => {
            let e = r * 1.25;
            let mut builder = PathBuilder::fill();
            builder.move_to(Point::new(px(cx), px(cy - e)));
            builder.line_to(Point::new(px(cx + e), px(cy)));
            builder.line_to(Point::new(px(cx), px(cy + e)));
            builder.line_to(Point::new(px(cx - e), px(cy)));
            builder.line_to(Point::new(px(cx), px(cy - e)));
            if let Ok(p) = builder.build() {
                window.paint_path(p, color);
            }
        }
        PortShape::Triangle => {
            let e = r * 1.2;
            let mut builder = PathBuilder::fill();
            builder.move_to(Point::new(px(cx - e), px(cy - e)));
            builder.line_to(Point::new(px(cx + e), px(cy)));
            builder.line_to(Point::new(px(cx - e), px(cy + e)));
            builder.line_to(Point::new(px(cx - e), px(cy - e)));
            if let Ok(p) = builder.build() {
                window.paint_path(p, color);
            }
        }
        PortShape::Hexagon => {
            // Pointy-top regular hexagon: the silhouette of a volume seen
            // from a corner, which is what a scene is next to a geometry's
            // flat diamond.
            let e = r * 1.2;
            let mut builder = PathBuilder::fill();
            let vertex = |k: f32| {
                let angle = -std::f32::consts::FRAC_PI_2 + k * std::f32::consts::FRAC_PI_3;
                Point::new(px(cx + e * angle.cos()), px(cy + e * angle.sin()))
            };
            builder.move_to(vertex(0.0));
            for k in 1..6 {
                builder.line_to(vertex(k as f32));
            }
            builder.line_to(vertex(0.0));
            if let Ok(p) = builder.build() {
                window.paint_path(p, color);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn edge_at_local_pos(
    graph: &Graph,
    viewport: &Viewport,
    lx: f32,
    ly: f32,
    threshold: f32,
    edge_style: super::EdgeStyle,
    flow: FlowDirection,
) -> Option<EdgeId> {
    use super::bezier::point_to_bezier_distance;

    for edge in graph.edges() {
        let src_node = match graph.node(edge.source) {
            Some(n) => n,
            None => continue,
        };
        let tgt_node = match graph.node(edge.target) {
            Some(n) => n,
            None => continue,
        };
        if src_node.metadata.synthetic || tgt_node.metadata.synthetic {
            continue;
        }

        let ((sx, sy), (tx, ty)) = edge_endpoints(edge, src_node, tgt_node, viewport, flow);

        let dist = match edge_style {
            super::EdgeStyle::Bezier => {
                let path = flow_bezier(flow, sx, sy, tx, ty);
                point_to_bezier_distance(lx, ly, &path, 20)
            }
            super::EdgeStyle::Straight => point_to_segment_distance(lx, ly, sx, sy, tx, ty),
            super::EdgeStyle::Step => {
                let [a, b, c, d] = step_points(flow, sx, sy, tx, ty);
                let d1 = point_to_segment_distance(lx, ly, a.0, a.1, b.0, b.1);
                let d2 = point_to_segment_distance(lx, ly, b.0, b.1, c.0, c.1);
                let d3 = point_to_segment_distance(lx, ly, c.0, c.1, d.0, d.1);
                d1.min(d2).min(d3)
            }
        };
        if dist <= threshold {
            return Some(edge.id);
        }
    }
    None
}

fn point_to_segment_distance(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 0.001 {
        return ((px - x0).powi(2) + (py - y0).powi(2)).sqrt();
    }
    let t = ((px - x0) * dx + (py - y0) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let proj_x = x0 + t * dx;
    let proj_y = y0 + t * dy;
    ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt()
}

#[derive(Clone, Debug)]
pub struct PortHit {
    pub node_id: NodeId,
    pub is_output: bool,
    pub port_index: u32,
    pub center: (f32, f32),
}

pub fn port_at_local_pos(
    graph: &Graph,
    viewport: &Viewport,
    flow: FlowDirection,
    lx: f32,
    ly: f32,
) -> Option<PortHit> {
    // Nearest port within the radius: at low zoom neighbouring hit circles
    // overlap, and taking the last match would make some ports unreachable.
    let mut hit: Option<PortHit> = None;
    let mut best = PORT_HIT_RADIUS;
    for node in z_ordered(graph) {
        let screen = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        for (i, _input) in node.inputs.iter().enumerate() {
            let (cx, cy) = port_screen_center(node, screen, viewport.zoom, flow, false, i);
            let dist = ((lx - cx).powi(2) + (ly - cy).powi(2)).sqrt();
            if dist <= best {
                best = dist;
                hit = Some(PortHit {
                    node_id: node.id,
                    is_output: false,
                    port_index: i as u32,
                    center: (cx, cy),
                });
            }
        }

        for (i, _output) in node.outputs.iter().enumerate() {
            let (cx, cy) = port_screen_center(node, screen, viewport.zoom, flow, true, i);
            let dist = ((lx - cx).powi(2) + (ly - cy).powi(2)).sqrt();
            if dist <= best {
                best = dist;
                hit = Some(PortHit {
                    node_id: node.id,
                    is_output: true,
                    port_index: i as u32,
                    center: (cx, cy),
                });
            }
        }
    }
    hit
}

/// Name of the port a [`PortHit`] points at.
pub fn port_name<'a>(graph: &'a Graph, hit: &PortHit) -> Option<&'a str> {
    let node = graph.node(hit.node_id)?;
    let i = hit.port_index as usize;
    if hit.is_output {
        node.outputs.get(i).map(|p| p.name.as_str())
    } else {
        node.inputs.get(i).map(|p| p.name.as_str())
    }
}

/// Port-name tag drawn beside a top-down node's port dot while it is hovered
/// or is the snapped target of a wire being dragged. The row of names a
/// horizontal node prints is absent in that mode, so this is where a port's
/// name is read. `center` is in panel-local pixels; inputs get the tag above
/// the dot (outside the node) and outputs below it.
pub fn paint_port_label(
    name: &str,
    center: (f32, f32),
    is_output: bool,
    bounds: &Bounds<Pixels>,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    if name.is_empty() {
        return;
    }
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let font_size = PORT_LABEL_FONT;
    let text: SharedString = name.to_string().into();
    let len = text.len();
    let shaped = window.text_system().shape_line(
        text,
        px(font_size),
        &[TextRun {
            len,
            font: crate::fonts::ui_font(cx),
            color: colors.foreground,
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        }],
        None,
    );
    let tw: f32 = shaped.width.into();
    let tag_w = tw + 2.0 * PORT_LABEL_PAD;
    let tag_h = font_size * 1.4 + PORT_LABEL_PAD;
    let gap = BASE_PORT_DOT_R + PORT_LABEL_PAD;
    let x = ox + center.0 - tag_w / 2.0;
    let y = if is_output {
        oy + center.1 + gap
    } else {
        oy + center.1 - gap - tag_h
    };
    let tag = Bounds::new(
        Point::new(px(x), px(y)),
        Size {
            width: px(tag_w),
            height: px(tag_h),
        },
    );
    window.paint_quad(fill(tag, colors.background).corner_radii(px(BASE_CORNER_R / 2.0)));
    window.paint_quad(
        outline(tag, colors.border, BorderStyle::default())
            .corner_radii(px(BASE_CORNER_R / 2.0))
            .border_widths(px(1.0)),
    );
    shaped
        .paint(
            Point::new(px(x + PORT_LABEL_PAD), px(y + PORT_LABEL_PAD / 2.0)),
            px(font_size * 1.4),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
}

pub fn find_snap_target(
    graph: &Graph,
    viewport: &Viewport,
    flow: FlowDirection,
    from: &PortHit,
    mouse_lx: f32,
    mouse_ly: f32,
) -> Option<PortHit> {
    let mut best: Option<(f32, PortHit)> = None;

    for node in z_ordered(graph) {
        if node.id == from.node_id {
            continue;
        }

        let screen = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        let ports: Vec<(usize, bool)> = if from.is_output {
            node.inputs
                .iter()
                .enumerate()
                .map(|(i, _)| (i, false))
                .collect()
        } else {
            node.outputs
                .iter()
                .enumerate()
                .map(|(i, _)| (i, true))
                .collect()
        };

        for (i, is_out) in ports {
            if !is_port_compatible(graph, from, node, i, is_out) {
                continue;
            }

            let (cx, cy) = port_screen_center(node, screen, viewport.zoom, flow, is_out, i);

            let dist = ((mouse_lx - cx).powi(2) + (mouse_ly - cy).powi(2)).sqrt();
            // z_ordered walks back to front, so an equal-distance candidate
            // replaces the previous one and the frontmost port wins the tie.
            if dist <= SNAP_RADIUS && best.as_ref().is_none_or(|(d, _)| dist <= *d) {
                best = Some((
                    dist,
                    PortHit {
                        node_id: node.id,
                        is_output: is_out,
                        port_index: i as u32,
                        center: (cx, cy),
                    },
                ));
            }
        }
    }

    best.map(|(_, hit)| hit)
}

fn is_port_compatible(
    graph: &Graph,
    from: &PortHit,
    target_node: &Node,
    target_port_idx: usize,
    target_is_output: bool,
) -> bool {
    let from_node = match graph.node(from.node_id) {
        Some(n) => n,
        None => return false,
    };

    let (src_type, accepted) = if from.is_output && !target_is_output {
        let src = from_node
            .outputs
            .get(from.port_index as usize)
            .map(|p| p.data_type);
        let acc = target_node
            .inputs
            .get(target_port_idx)
            .map(|p| &p.accepted_types);
        (src, acc)
    } else if !from.is_output && target_is_output {
        let src = target_node
            .outputs
            .get(target_port_idx)
            .map(|p| p.data_type);
        let acc = from_node
            .inputs
            .get(from.port_index as usize)
            .map(|p| &p.accepted_types);
        (src, acc)
    } else {
        return false;
    };

    match (src_type, accepted) {
        (Some(dt), Some(types)) => types.is_empty() || types.contains(&dt),
        _ => false,
    }
}

pub fn paint_connection_draft(
    from: (f32, f32),
    to: (f32, f32),
    flow: FlowDirection,
    bounds: &Bounds<Pixels>,
    colors: &Colors,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();

    let sx = ox + from.0;
    let sy = oy + from.1;
    let tx = ox + to.0;
    let ty = oy + to.1;

    let draft_color = colors.primary;

    let path = flow_bezier(flow, sx, sy, tx, ty);
    let mut builder = PathBuilder::stroke(px(2.0));
    builder.move_to(Point::new(px(path.source.0), px(path.source.1)));
    builder.cubic_bezier_to(
        Point::new(px(path.target.0), px(path.target.1)),
        Point::new(px(path.source_control.0), px(path.source_control.1)),
        Point::new(px(path.target_control.0), px(path.target_control.1)),
    );
    if let Ok(p) = builder.build() {
        window.paint_path(p, draft_color);
    }
}

pub fn paint_selection_box(
    start: (f32, f32),
    current: (f32, f32),
    bounds: &Bounds<Pixels>,
    colors: &Colors,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let x = start.0.min(current.0) + ox;
    let y = start.1.min(current.1) + oy;
    let w = (start.0 - current.0).abs();
    let h = (start.1 - current.1).abs();
    if w < 1.0 || h < 1.0 {
        return;
    }

    let rect = Bounds::new(
        Point::new(px(x), px(y)),
        Size {
            width: px(w),
            height: px(h),
        },
    );

    let highlight = colors.primary;
    let fill_color = Hsla {
        alpha: 0.08,
        ..highlight
    };
    window.paint_quad(fill(rect, fill_color));
    window.paint_quad(outline(rect, highlight, BorderStyle::default()).border_widths(px(1.0)));
}

/// Shapes one line of node-body text.
fn shape_run(
    text: &str,
    font_size: f32,
    font: &Font,
    color: Hsla,
    window: &mut Window,
) -> ShapedLine {
    let text: SharedString = text.to_owned().into();
    let len = text.len();
    window.text_system().shape_line(
        text,
        px(font_size),
        &[TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        }],
        None,
    )
}

/// The first `keep` characters of `text` plus an ellipsis.
///
/// Cuts on characters, not bytes: node titles and string parameters are
/// user-supplied and routinely multi-byte.
fn elided_prefix(text: &str, keep: usize) -> String {
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// The longest prefix length whose elided line fits in `max_width`, or `None`
/// when not even the bare ellipsis does.
///
/// `measure(keep)` reports the width of `elided_prefix(text, keep)`. The
/// search treats those widths as non-decreasing in `keep`, which shaping does
/// **not** strictly guarantee — a ligature or a kern pair can make a longer
/// prefix narrower than a shorter one. That costs at most a character of
/// width on such a font: the search only ever accepts a length it has
/// measured as fitting, so a non-monotonic font can make the label shorter
/// than necessary but can never make it overflow.
///
/// Split out from [`shape_elided`] so the search is testable without a text
/// system.
fn longest_fitting_keep(
    chars: usize,
    max_width: f32,
    mut measure: impl FnMut(usize) -> f32,
) -> Option<usize> {
    // Empty text has nothing to elide; `Some(0)` would mean "a bare ellipsis
    // fits", which is not an answer the caller wants for no text at all.
    if chars == 0 {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = chars - 1;
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        if measure(mid) <= max_width {
            best = Some(mid);
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// Shapes `text` so the line fits in `max_width`, eliding it with a trailing
/// ellipsis when it does not.
///
/// Node boxes are a fixed width, so anything long — a localized node title, a
/// vec4 value — would otherwise spill past the box or collide with the label
/// beside it. [`longest_fitting_keep`] finds the cut in about `log2(len)`
/// shaping calls, and only for text that actually overflows. An
/// average-advance estimate would be exact for the monospace values but wrong
/// for proportional titles, which is where the overflow shows.
///
/// The probes repeat verbatim from frame to frame at a fixed zoom, so gpui's
/// line-layout cache absorbs them; a zoom gesture re-shapes because the width
/// budget moves with it.
fn shape_elided(
    text: &str,
    font_size: f32,
    font: &Font,
    color: Hsla,
    max_width: f32,
    window: &mut Window,
) -> ShapedLine {
    let full = shape_run(text, font_size, font, color, window);
    if f32::from(full.width) <= max_width {
        return full;
    }
    let chars = text.chars().count();
    if chars == 0 {
        return full;
    }

    let keep = longest_fitting_keep(chars, max_width, |keep| {
        f32::from(shape_run(&elided_prefix(text, keep), font_size, font, color, window).width)
    });
    match keep {
        Some(keep) => shape_run(&elided_prefix(text, keep), font_size, font, color, window),
        // Nothing fits, not even the bare ellipsis: draw it anyway rather than
        // dropping the row, so the node still shows that a value is there.
        None => shape_run("…", font_size, font, color, window),
    }
}

/// Draws a label in the UI font: node titles, port names, anything the user
/// named. Readouts go through [`paint_mono_text`] instead.
fn paint_text(
    text: &str,
    origin: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let font = crate::fonts::ui_font(cx);
    paint_text_in(text, origin, font_size, color, font, window, cx);
}

/// Like [`paint_mono_text`], but reports the width it drew so the caller can
/// lay out against it.
fn paint_mono_text_measured(
    text: &str,
    origin: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let font = crate::fonts::mono_font(cx);
    paint_text_in(text, origin, font_size, color, font, window, cx)
}

/// Draws a readout in the monospace font — parameter keys and values, and the
/// evaluation timing under the node.
///
/// Monospaced so a value that ticks while scrubbing keeps its digits in place
/// and the key column of a node stays aligned.
fn paint_mono_text(
    text: &str,
    origin: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let font = crate::fonts::mono_font(cx);
    paint_text_in(text, origin, font_size, color, font, window, cx);
}

/// Shapes and paints one line, returning the width it occupies.
#[allow(clippy::too_many_arguments)]
fn paint_text_in(
    text: &str,
    origin: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    font: Font,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let text: SharedString = text.into();
    let len = text.len();
    if len == 0 {
        return 0.0;
    }
    let shaped = window.text_system().shape_line(
        text,
        px(font_size),
        &[TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        }],
        None,
    );
    shaped
        .paint(
            origin,
            px(font_size * 1.4),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    shaped.width.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::DataTypeId;
    use std::sync::Arc;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one for these plain unit tests.
    use core::prelude::v1::test;

    #[test]
    fn elided_prefix_keeps_the_requested_characters() {
        assert_eq!(elided_prefix("[1.00, 1.00, 1.00, 1.00]", 9), "[1.00, 1.…");
        assert_eq!(elided_prefix("radius", 0), "…");
    }

    /// Node titles and string parameters are user-supplied and routinely
    /// multi-byte: the cut counts characters, never bytes.
    #[test]
    fn elided_prefix_cuts_on_character_boundaries() {
        assert_eq!(elided_prefix("カーブリマップフィールド", 5), "カーブリマ…");
    }

    /// A keep count past the end yields the whole string — the binary search
    /// probes `chars - 1` at most, but the helper must not panic if it does.
    #[test]
    fn elided_prefix_saturates_at_the_end() {
        assert_eq!(elided_prefix("fill", 99), "fill…");
    }

    /// Uniform 6px cells, 24 characters, a 60px budget: 10 cells fit, one of
    /// which the ellipsis takes, so 9 characters survive.
    #[test]
    fn longest_fitting_keep_takes_the_widest_prefix_that_fits() {
        let keep = longest_fitting_keep(24, 60.0, |keep| (keep + 1) as f32 * 6.0);
        assert_eq!(keep, Some(9));
    }

    /// One character: the only candidate is the bare ellipsis.
    #[test]
    fn longest_fitting_keep_handles_a_single_character() {
        assert_eq!(longest_fitting_keep(1, 60.0, |_| 6.0), Some(0));
        assert_eq!(longest_fitting_keep(1, 1.0, |_| 6.0), None);
    }

    /// Nothing fits, not even "…" — the caller draws the ellipsis unclipped
    /// rather than dropping the row.
    #[test]
    fn longest_fitting_keep_gives_up_when_even_the_ellipsis_overflows() {
        assert_eq!(
            longest_fitting_keep(12, 2.0, |keep| (keep + 1) as f32 * 6.0),
            None
        );
        assert_eq!(longest_fitting_keep(0, 60.0, |_| 6.0), None);
    }

    /// Shaping is not strictly monotonic — a ligature can make a longer
    /// prefix narrower. The search may then return a shorter prefix than
    /// ideal, but must never return one it measured as overflowing.
    #[test]
    fn longest_fitting_keep_never_accepts_an_overflowing_measurement() {
        // Prefix 3 collapses into a ligature and measures narrower than 2.
        let width = |keep: usize| match keep {
            0 => 6.0,
            1 => 12.0,
            2 => 40.0,
            3 => 20.0,
            _ => 100.0,
        };
        let keep = longest_fitting_keep(8, 30.0, width).expect("something fits");
        assert!(
            width(keep) <= 30.0,
            "accepted keep={keep} measuring {} against a 30px budget",
            width(keep)
        );
    }

    fn viewport() -> Viewport {
        Viewport {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }

    fn scalar_source(id: u64, synthetic: bool) -> Node {
        let mut node = Node::new(ravel_core::id::NodeId::new(id), "constant")
            .with_output("out", DataTypeId::SCALAR);
        node.metadata.synthetic = synthetic;
        node
    }

    /// Synthetic shell nodes are hidden from the editor (REQ-LAYER-011):
    /// their ports must not be hit-testable either.
    #[test]
    fn ports_of_synthetic_nodes_are_not_hit() {
        let vp = viewport();
        let (px, py) = output_port_screen_center((0.0, 0.0), 0, vp.zoom);

        let hidden = Graph::new().add_node(scalar_source(1, true)).unwrap();
        assert!(port_at_local_pos(&hidden, &vp, FlowDirection::Horizontal, px, py).is_none());

        let visible = Graph::new().add_node(scalar_source(1, false)).unwrap();
        let hit = port_at_local_pos(&visible, &vp, FlowDirection::Horizontal, px, py)
            .expect("visible port hits");
        assert!(hit.is_output);
    }

    /// Paint order is ascending z with synthetic nodes excluded; ties keep
    /// graph iteration order.
    #[test]
    fn z_ordered_sorts_ascending_and_skips_synthetic() {
        let mut low = scalar_source(1, false);
        low.metadata.z = 1;
        let mut high = scalar_source(2, false);
        high.metadata.z = 8;
        let hidden = scalar_source(3, true);
        let graph = Graph::new()
            .add_node(high)
            .unwrap()
            .add_node(low)
            .unwrap()
            .add_node(hidden)
            .unwrap();

        let order: Vec<u64> = z_ordered(&graph).iter().map(|n| n.metadata.z).collect();
        assert_eq!(order, vec![1, 8]);
    }

    #[test]
    fn overlapping_ports_prefer_the_frontmost_node() {
        let vp = viewport();
        let mut back = scalar_source(1, false);
        back.metadata.z = 1;
        let mut front = scalar_source(2, false);
        front.metadata.z = 9;
        let graph = Graph::new()
            .add_node(front)
            .unwrap()
            .add_node(back)
            .unwrap();
        let (x, y) = output_port_screen_center((0.0, 0.0), 0, vp.zoom);

        assert_eq!(
            port_at_local_pos(&graph, &vp, FlowDirection::Horizontal, x, y)
                .unwrap()
                .node_id,
            NodeId::new(2)
        );
    }

    #[test]
    fn equal_distance_snap_prefers_the_frontmost_node() {
        let vp = viewport();
        let source = scalar_source(1, false).with_position(-200.0, 0.0);
        let mut back = Node::new(NodeId::new(2), "sink").with_input("in", &[DataTypeId::SCALAR]);
        back.metadata.z = 1;
        let mut front = Node::new(NodeId::new(3), "sink").with_input("in", &[DataTypeId::SCALAR]);
        front.metadata.z = 9;
        let graph = Graph::new()
            .add_node(front)
            .unwrap()
            .add_node(source)
            .unwrap()
            .add_node(back)
            .unwrap();
        let source_pos = vp.flow_to_screen(-200.0, 0.0);
        let (sx, sy) = output_port_screen_center(source_pos, 0, vp.zoom);
        let from = port_at_local_pos(&graph, &vp, FlowDirection::Horizontal, sx, sy).unwrap();
        let (x, y) = input_port_screen_center((0.0, 0.0), 0, vp.zoom);

        assert_eq!(
            find_snap_target(&graph, &vp, FlowDirection::Horizontal, &from, x, y)
                .unwrap()
                .node_id,
            NodeId::new(3)
        );
    }

    #[test]
    fn eval_duration_formats_compactly() {
        assert_eq!(format_eval_duration(Duration::from_micros(400)), "0.4ms");
        assert_eq!(format_eval_duration(Duration::from_millis(12)), "12ms");
        assert_eq!(format_eval_duration(Duration::from_millis(1200)), "1.2s");
    }

    /// The readout grain: measurements that draw the same text in the same
    /// color compare equal, and everything the readout can show compares
    /// unequal — including a color-band crossing the rounded text hides.
    #[test]
    fn eval_readout_is_equal_exactly_when_the_readout_looks_the_same() {
        let readout = |micros| EvalReadout::of(Duration::from_micros(micros));

        assert_eq!(
            readout(12_300),
            readout(12_400),
            "both draw `12ms` in the same band"
        );
        assert_ne!(
            readout(12_300),
            readout(13_000),
            "`12ms` and `13ms` are different text"
        );

        // 7.96ms and 8.04ms both round to `8.0ms`, but TIMING_WARN (8ms)
        // sits between them: the text alone would swallow the color change.
        assert_eq!(
            format_eval_duration(Duration::from_micros(7_960)),
            format_eval_duration(Duration::from_micros(8_040))
        );
        assert_ne!(readout(7_960), readout(8_040), "muted → yellow must show");

        // Same at TIMING_CRITICAL (33ms), where both sides print `33ms`.
        assert_eq!(
            format_eval_duration(Duration::from_micros(32_600)),
            format_eval_duration(Duration::from_micros(33_000))
        );
        assert_ne!(readout(32_600), readout(33_000), "yellow → red must show");
    }

    /// The rounding lives in exactly one place: whatever
    /// `format_eval_duration` returns, `write_eval_duration` writes, at every
    /// boundary of the ladder. Two implementations drifting apart would make
    /// the repaint gate and the canvas disagree.
    #[test]
    fn writing_and_formatting_a_duration_cannot_drift() {
        let mut buffer = String::from("stale contents");
        for micros in [
            0, 1, 99, 400, 9_949, 9_950, 9_999, 10_000, 12_300, 32_600, 33_000, 999_499, 999_500,
            1_000_000, 1_200_000, 90_000_000,
        ] {
            let duration = Duration::from_micros(micros);
            write_eval_duration(&mut buffer, duration);
            assert_eq!(buffer, format_eval_duration(duration), "at {micros}µs");
        }
    }

    /// The buffered path the repaint gate uses and the allocating one agree,
    /// including across a buffer that still holds a longer previous readout.
    #[test]
    fn the_buffered_readout_matches_the_allocating_one() {
        let mut scratch = String::new();
        for micros in [12_300, 400, 1_200_000, 7_960, 8_040, 33_000] {
            let duration = Duration::from_micros(micros);
            assert_eq!(
                EvalReadout::written(duration, &mut scratch),
                EvalReadout::of(duration),
                "at {micros}µs"
            );
        }
        assert!(
            scratch.capacity() > 0,
            "the buffer is reused rather than reallocated per node"
        );
    }

    /// Across the whole zoom range the header glyph rasterizes at a handful
    /// of sizes only — the sprite atlas never sees a continuous size stream.
    #[test]
    fn header_icon_size_quantizes_to_a_finite_ladder() {
        let mut seen = std::collections::HashSet::new();
        let steps = 1000;
        for i in 0..=steps {
            let zoom = Viewport::MIN_ZOOM
                + (Viewport::MAX_ZOOM - Viewport::MIN_ZOOM) * i as f32 / steps as f32;
            if let Some(size) = quantized_header_icon_size(zoom) {
                assert!(
                    HEADER_ICON_SIZES.contains(&size),
                    "size {size} at zoom {zoom} is off the ladder"
                );
                seen.insert(size.to_bits());
            }
        }
        assert!(seen.len() <= HEADER_ICON_SIZES.len());
        assert!(
            seen.len() > 1,
            "the zoom range should reach several ladder rungs"
        );
    }

    /// At low zoom the glyph is too small to read and is omitted entirely
    /// (like the background grid dots).
    #[test]
    fn header_icon_is_omitted_at_low_zoom() {
        assert!(quantized_header_icon_size(Viewport::MIN_ZOOM).is_none());
        assert!(
            quantized_header_icon_size(0.4).is_none(),
            "4.8px is below the legibility floor"
        );
        assert_eq!(quantized_header_icon_size(1.0), Some(12.0));
        assert!(
            quantized_header_icon_size(Viewport::MAX_ZOOM).is_some(),
            "max zoom clamps to the top rung instead of dropping the glyph"
        );
    }

    /// The header glyph never changes the node geometry: a node whose type
    /// has an explicit icon and one that falls back to the category default
    /// (an unknown `type_key`) keep the same width and header height.
    #[test]
    fn header_icon_does_not_change_node_size() {
        let explicit =
            Node::new(NodeId::new(1), "blur").with_output("out", DataTypeId::FRAME_BUFFER);
        let fallback =
            Node::new(NodeId::new(2), "user.unknown").with_output("out", DataTypeId::FRAME_BUFFER);
        for zoom in [Viewport::MIN_ZOOM, 1.0, Viewport::MAX_ZOOM] {
            assert_eq!(
                compute_node_size(&explicit, zoom, true, FlowDirection::Horizontal),
                compute_node_size(&fallback, zoom, true, FlowDirection::Horizontal),
                "node size must not depend on the header glyph at zoom {zoom}"
            );
        }
    }

    /// PGRP-5: turning the parameter rows off takes their height — and the
    /// separator above them — out of the node, and leaves the width alone. A
    /// node with no parameters is the same size either way, so the toggle
    /// The row count is one rule, and both the height and the paint read it.
    /// A body that painted rows the height did not reserve would spill over
    /// the node below; a height that reserved rows nobody painted would move
    /// every port's hit target down (`MED-APP-13`).
    #[test]
    fn the_painted_row_count_is_zero_exactly_when_the_values_are_hidden() {
        let bare = Node::new(NodeId::new(1), "constant");
        let with_params = Node::new(NodeId::new(2), "blur")
            .with_param("radius", ParameterValue::Float(2.0))
            .with_param("quality", ParameterValue::Int(1));

        assert_eq!(painted_param_rows(&with_params, true), 2);
        assert_eq!(painted_param_rows(&with_params, false), 0);
        // A node with nothing to show is the same either way.
        assert_eq!(painted_param_rows(&bare, true), 0);
        assert_eq!(painted_param_rows(&bare, false), 0);
    }

    /// cannot move anything that was not showing rows.
    #[test]
    fn hiding_the_parameter_values_shrinks_the_node_by_its_parameter_rows() {
        let node = Node::new(NodeId::new(1), "test")
            .with_output("out", DataTypeId::FRAME_BUFFER)
            .with_param("a", ravel_core::graph::ParameterValue::Float(1.0))
            .with_param("b", ravel_core::graph::ParameterValue::Int(2))
            .with_param("c", ravel_core::graph::ParameterValue::Bool(true));
        let bare = Node::new(NodeId::new(2), "test").with_output("out", DataTypeId::FRAME_BUFFER);

        for zoom in [Viewport::MIN_ZOOM, 1.0, Viewport::MAX_ZOOM] {
            let (shown_w, shown_h) =
                compute_node_size(&node, zoom, true, FlowDirection::Horizontal);
            let (hidden_w, hidden_h) =
                compute_node_size(&node, zoom, false, FlowDirection::Horizontal);
            assert_eq!(
                shown_w, hidden_w,
                "the node width never depends on the rows"
            );
            assert!(
                (shown_h - hidden_h - (3.0 * BASE_PARAM_ROW_H + 6.0) * zoom).abs() < 1e-3,
                "at zoom {zoom} only the rows and their separator come off: {shown_h} -> {hidden_h}"
            );
            assert_eq!(
                compute_node_size(&bare, zoom, true, FlowDirection::Horizontal),
                compute_node_size(&bare, zoom, false, FlowDirection::Horizontal),
                "a node with no parameters is unaffected at zoom {zoom}"
            );
        }
    }

    /// The readout escalates muted → yellow → red with load.
    #[test]
    fn eval_duration_color_escalates_with_load() {
        let colors = Colors::dark();
        let ok = eval_duration_color(Duration::from_millis(2), &colors);
        assert_eq!(ok, colors.muted_foreground);
        let warn = eval_duration_color(Duration::from_millis(15), &colors);
        let critical = eval_duration_color(Duration::from_millis(100), &colors);
        assert_ne!(warn, ok);
        assert_ne!(critical, warn);
        assert_eq!(
            critical.color.hue.into_positive_degrees(),
            0.0,
            "critical is red"
        );
    }

    /// Connection-drag snapping must never target a synthetic node.
    #[test]
    fn snap_skips_synthetic_nodes() {
        let vp = viewport();
        let mut sink = Node::new(ravel_core::id::NodeId::new(2), "test")
            .with_input("in", &[DataTypeId::SCALAR]);
        sink.metadata.synthetic = true;
        let graph = Graph::new()
            .add_node(scalar_source(1, false))
            .unwrap()
            .add_node(sink)
            .unwrap();

        let (px, py) = output_port_screen_center((0.0, 0.0), 0, vp.zoom);
        let from = port_at_local_pos(&graph, &vp, FlowDirection::Horizontal, px, py).unwrap();
        let (ix, iy) = input_port_screen_center((0.0, 0.0), 0, vp.zoom);
        assert!(find_snap_target(&graph, &vp, FlowDirection::Horizontal, &from, ix, iy).is_none());
    }

    #[test]
    fn exposed_param_ports_use_existing_snap_type_filtering() {
        let vp = viewport();
        let source_id = NodeId::new(11);
        let sink_id = NodeId::new(12);
        let source = Node::new(source_id, "constant")
            .with_output("out", DataTypeId::SCALAR)
            .with_position(0.0, 100.0);
        let sink = Node::new(sink_id, "blur")
            .with_param("radius", ParameterValue::Float(8.0))
            .with_position(300.0, 0.0);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(sink)
            .unwrap()
            .expose_param_port(sink_id, "radius")
            .unwrap();

        let source_screen = vp.flow_to_screen(0.0, 100.0);
        let (source_x, source_y) = output_port_screen_center(source_screen, 0, vp.zoom);
        let from =
            port_at_local_pos(&graph, &vp, FlowDirection::Horizontal, source_x, source_y).unwrap();
        let sink_screen = vp.flow_to_screen(300.0, 0.0);
        let (target_x, target_y) = input_port_screen_center(sink_screen, 0, vp.zoom);
        let target = find_snap_target(
            &graph,
            &vp,
            FlowDirection::Horizontal,
            &from,
            target_x,
            target_y,
        )
        .expect("scalar snaps to exposed float parameter");
        assert_eq!(target.node_id, sink_id);
        assert_eq!(target.port_index, 0);

        let color_source = Node::new(source_id, "constant.color")
            .with_output("out", DataTypeId::COLOR)
            .with_position(0.0, 100.0);
        let graph = graph.replace_node(Arc::new(color_source));
        let from =
            port_at_local_pos(&graph, &vp, FlowDirection::Horizontal, source_x, source_y).unwrap();
        assert!(
            find_snap_target(
                &graph,
                &vp,
                FlowDirection::Horizontal,
                &from,
                target_x,
                target_y
            )
            .is_none(),
            "color does not snap to scalar parameter input"
        );
    }

    // ---- top-down flow ----------------------------------------------------

    const V: FlowDirection = FlowDirection::Vertical;

    fn ported(id: u64, inputs: usize, outputs: usize) -> Node {
        let mut node = Node::new(NodeId::new(id), "test");
        for i in 0..inputs {
            node = node.with_input(format!("in{i}"), &[DataTypeId::SCALAR]);
        }
        for i in 0..outputs {
            node = node.with_output(format!("out{i}"), DataTypeId::SCALAR);
        }
        node
    }

    /// Every port's drawn centre must be what the hit test finds, in both
    /// directions and across port counts and zooms — the centre is computed
    /// once (`port_screen_center`) and painting and picking both read it.
    /// Low zooms are included: neighbouring hit circles overlap there, and the
    /// nearest port must still win.
    #[test]
    fn port_centres_round_trip_through_the_hit_test_in_both_flows() {
        for (flow, zoom) in [FlowDirection::Horizontal, V]
            .into_iter()
            .flat_map(|flow| [1.5, 0.3, Viewport::MIN_ZOOM].map(|zoom| (flow, zoom)))
        {
            for (inputs, outputs) in [(1, 1), (2, 1), (3, 2), (8, 1), (12, 3)] {
                let node = ported(1, inputs, outputs).with_position(40.0, 30.0);
                let graph = Graph::new().add_node(node).unwrap();
                let vp = Viewport {
                    x: 7.0,
                    y: 5.0,
                    zoom,
                };
                let node = graph.node(NodeId::new(1)).unwrap();
                let screen = vp.flow_to_screen(40.0, 30.0);
                for (is_output, count) in [(false, inputs), (true, outputs)] {
                    for index in 0..count {
                        let (cx, cy) =
                            port_screen_center(node, screen, vp.zoom, flow, is_output, index);
                        let hit =
                            port_at_local_pos(&graph, &vp, flow, cx, cy).unwrap_or_else(|| {
                                panic!("{flow:?} z{zoom} {inputs}x{outputs} port {index} not hit")
                            });
                        assert_eq!(
                            (hit.is_output, hit.port_index),
                            (is_output, index as u32),
                            "{flow:?} z{zoom} {inputs}x{outputs}"
                        );
                        assert_eq!(hit.center, (cx, cy));
                    }
                }
            }
        }
    }

    /// Ports sit on the box's top and bottom edges — never on the label
    /// column beside it, whatever the extent is.
    #[test]
    fn vertical_ports_sit_on_the_box_edges_only() {
        let node = ported(1, 3, 2);
        let (box_w, box_h) = vertical_box_size(&node, 1.0);
        let (extent_w, _) = compute_node_size(&node, 1.0, true, V);
        let (ix, iy) = port_screen_center(&node, (10.0, 20.0), 1.0, V, false, 1);
        let (ox, oy) = port_screen_center(&node, (10.0, 20.0), 1.0, V, true, 0);
        assert_eq!(iy, 20.0, "inputs on the top edge");
        assert_eq!(oy, 20.0 + box_h, "outputs on the bottom edge");
        assert_eq!(ix, 10.0 + box_w / 2.0, "the middle of three is the centre");
        assert_eq!(ox, 10.0 + box_w / 4.0, "the first of two is a quarter in");
        for index in 0..3 {
            let (x, _) = port_screen_center(&node, (0.0, 0.0), 1.0, V, false, index);
            assert!(x < box_w && x < extent_w - BASE_VERTICAL_LABEL_W);
        }
    }

    /// The box is a small ~3:1 rectangle that does not depend on the label or
    /// the values flag; the extent adds the label column (so it is clickable)
    /// and grows with the value rows without growing the box.
    #[test]
    fn the_vertical_extent_is_the_box_plus_the_label_column() {
        let node = ported(1, 2, 1)
            .with_param("a", ParameterValue::Float(1.0))
            .with_param("b", ParameterValue::Float(2.0))
            .with_param("c", ParameterValue::Float(3.0));
        let (box_w, box_h) = vertical_box_size(&node, 1.0);
        assert_eq!(
            (box_w, box_h),
            (BASE_VERTICAL_BOX_MIN_W, BASE_VERTICAL_BOX_H)
        );
        assert!(box_w / box_h >= 2.5 && box_w / box_h <= 3.5, "about 3:1");

        let (hw, hh) = compute_node_size(&node, 1.0, false, FlowDirection::Horizontal);
        let (hidden_w, hidden_h) = compute_node_size(&node, 1.0, false, V);
        let (shown_w, shown_h) = compute_node_size(&node, 1.0, true, V);
        assert_eq!(
            hidden_w,
            box_w + BASE_VERTICAL_LABEL_GAP + BASE_VERTICAL_LABEL_W
        );
        assert_eq!(shown_w, hidden_w, "values do not widen the node");
        assert_eq!(hidden_h, box_h, "the name line fits beside the box");
        assert_eq!(
            shown_h,
            BASE_VERTICAL_NAME_H + 3.0 * BASE_PARAM_ROW_H,
            "the value rows extend the label column, not the box"
        );
        assert!(hidden_h < hh && hidden_w < hw + BASE_VERTICAL_LABEL_W);
        assert_eq!(vertical_box_size(&node, 2.0), (box_w * 2.0, box_h * 2.0));
        assert_eq!(
            compute_node_size(&node, 2.0, true, V),
            (shown_w * 2.0, shown_h * 2.0)
        );
    }

    /// Many inputs widen the box (not the label) so neighbouring dots keep
    /// their hit targets apart.
    #[test]
    fn many_inputs_widen_the_box_to_keep_the_dots_apart() {
        let few = ported(1, 2, 1);
        let many = ported(2, 12, 1);
        assert!(vertical_box_size(&many, 1.0).0 > vertical_box_size(&few, 1.0).0);
        let xs: Vec<f32> = (0..12)
            .map(|i| port_screen_center(&many, (0.0, 0.0), 1.0, V, false, i).0)
            .collect();
        for pair in xs.windows(2) {
            assert!(
                pair[1] - pair[0] >= 2.0 * PORT_HIT_RADIUS,
                "dots {pair:?} would share a hit target"
            );
        }
    }

    /// `Step` jogs across halfway along the flow axis: horizontal runs along x
    /// first, vertical along y first.
    #[test]
    fn step_edges_bend_along_the_flow_axis() {
        let h = step_points(FlowDirection::Horizontal, 0.0, 0.0, 100.0, 60.0);
        assert_eq!(h, [(0.0, 0.0), (50.0, 0.0), (50.0, 60.0), (100.0, 60.0)]);
        let v = step_points(V, 0.0, 0.0, 100.0, 60.0);
        assert_eq!(v, [(0.0, 0.0), (0.0, 30.0), (100.0, 30.0), (100.0, 60.0)]);
    }

    /// The edge hit test follows the same routes the painter draws, for all
    /// three styles in top-down flow.
    #[test]
    fn edges_are_hit_where_they_are_drawn_in_vertical_flow() {
        let source = ported(1, 0, 1).with_position(0.0, 0.0);
        let sink = ported(2, 1, 0).with_position(200.0, 200.0);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(sink)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                ravel_core::id::OutputPortIndex(0),
                NodeId::new(2),
                ravel_core::id::InputPortIndex(0),
            )
            .unwrap();
        let vp = viewport();
        let src = graph.node(NodeId::new(1)).unwrap();
        let tgt = graph.node(NodeId::new(2)).unwrap();
        let (sx, sy) = port_screen_center(src, (0.0, 0.0), 1.0, V, true, 0);
        let (tx, ty) = port_screen_center(tgt, (200.0, 200.0), 1.0, V, false, 0);
        let hit =
            |style, x: f32, y: f32| edge_at_local_pos(&graph, &vp, x, y, 3.0, style, V).is_some();
        use super::super::EdgeStyle::{Bezier, Step, Straight};

        // Step: down from the source, across at mid-height, down into the target.
        let mid_y = (sy + ty) / 2.0;
        assert!(hit(Step, sx, (sy + mid_y) / 2.0));
        assert!(hit(Step, (sx + tx) / 2.0, mid_y));
        assert!(hit(Step, tx, (mid_y + ty) / 2.0));
        // The horizontal route's first leg runs along y = sy; vertical does not.
        assert!(!hit(Step, (sx + tx) / 2.0, sy));

        assert!(hit(Straight, (sx + tx) / 2.0, mid_y));
        assert!(hit(Bezier, sx, sy));
        assert!(hit(Bezier, (sx + tx) / 2.0, mid_y));
        assert!(!hit(Bezier, sx + 60.0, sy));
    }

    /// A fake text system: six pixels a character, in either font.
    fn six_per_char(text: &str, _mono: bool) -> f32 {
        text.chars().count() as f32 * 6.0
    }

    fn extent_w(node: &Node, label: &str, show: bool) -> f32 {
        let widths = measure_label_widths(node, label, &mut six_per_char);
        compute_node_size_labeled(node, 1.0, show, V, widths.column(show)).0
    }

    /// The label column is as wide as the text: a short name gives a narrower
    /// extent than a long one, and a very long one stops at the cap.
    #[test]
    fn the_vertical_extent_follows_the_measured_name_up_to_the_cap() {
        let node = ported(1, 1, 1);
        let short = extent_w(&node, "net.out", false);
        let long = extent_w(&node, "a rather longer node name", false);
        let huge = extent_w(&node, &"x".repeat(200), false);
        let (box_w, _) = vertical_box_size(&node, 1.0);
        assert_eq!(
            short,
            box_w + BASE_VERTICAL_LABEL_GAP + 7.0 * 6.0 + LABEL_MEASURE_SLACK
        );
        assert!(short < long);
        assert_eq!(
            huge,
            box_w + BASE_VERTICAL_LABEL_GAP + BASE_VERTICAL_LABEL_W,
            "capped"
        );
        assert_eq!(huge, compute_node_size(&node, 1.0, false, V).0);
    }

    /// The elision width paint uses is the extent minus the box and the gap —
    /// exactly the column the size was built from, at any zoom.
    #[test]
    fn paint_elides_at_the_width_the_size_was_measured_to() {
        let node = ported(1, 1, 1);
        let widths = measure_label_widths(&node, &"y".repeat(60), &mut six_per_char);
        for zoom in [0.5, 1.0, 2.0] {
            let (w, _) = compute_node_size_labeled(&node, zoom, false, V, widths.column(false));
            let (box_w, _) = vertical_box_size(&node, zoom);
            let paint_avail = w - box_w - BASE_VERTICAL_LABEL_GAP * zoom;
            assert!(
                (paint_avail - BASE_VERTICAL_LABEL_W * zoom).abs() < 1e-3,
                "z{zoom}: {paint_avail}"
            );
        }
    }

    /// A value line wider than the name widens the extent only while the
    /// values flag is on.
    #[test]
    fn a_wide_value_line_widens_the_extent_only_when_values_are_shown() {
        let node =
            ported(1, 1, 1).with_param("amount", ParameterValue::String("0123456789".into()));
        let off = extent_w(&node, "Blur", false);
        let on = extent_w(&node, "Blur", true);
        assert!(on > off, "{on} vs {off}");
        let (_, shown_h) = compute_node_size_labeled(&node, 1.0, true, V, 50.0);
        let (_, hidden_h) = compute_node_size_labeled(&node, 1.0, false, V, 50.0);
        assert!(shown_h >= hidden_h);
    }
}

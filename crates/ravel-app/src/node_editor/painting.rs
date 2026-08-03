// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::IconNamed as _;
use gpui_component::theme::ThemeColor;
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, NodeId};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ravel_core::registry::NodeCategory;

use super::bezier::horizontal_bezier;
use super::port_colors::{PortShape, category_color, port_color, port_shape};
use super::viewport::Viewport;
use crate::assets::RavelIcon;

/// Display value for an animated channel without an evaluation context:
/// the constant value, the curve's frame-0 sample, or 0 for
/// not-yet-resolvable sources.
fn channel_display(ch: &ravel_core::animation::channel::AnimationChannel) -> String {
    use ravel_core::animation::channel::ChannelSource;
    let v = match &ch.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(0.0),
        _ => 0.0,
    };
    format!("{v:.2}")
}

fn channels_display(chs: &[ravel_core::animation::channel::AnimationChannel]) -> String {
    let parts: Vec<String> = chs.iter().map(channel_display).collect();
    format!("[{}]", parts.join(", "))
}

const BASE_NODE_WIDTH: f32 = 160.0;
const BASE_HEADER_H: f32 = 24.0;
const BASE_PORT_ROW_H: f32 = 18.0;
const BASE_PARAM_ROW_H: f32 = 16.0;
const BASE_NODE_PAD: f32 = 8.0;
const BASE_PORT_GAP: f32 = 4.0;
const BASE_PORT_DOT_R: f32 = 4.0;
const BASE_CORNER_R: f32 = 6.0;
const PORT_HIT_RADIUS: f32 = 10.0;
const SNAP_RADIUS: f32 = 20.0;
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
    let raw = BASE_HEADER_ICON * zoom;
    if raw < HEADER_ICON_MIN_PX {
        return None;
    }
    HEADER_ICON_SIZES
        .iter()
        .copied()
        .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()))
}

pub fn node_width(zoom: f32) -> f32 {
    BASE_NODE_WIDTH * zoom
}

pub fn compute_node_size(node: &Node, zoom: f32) -> (f32, f32) {
    let z = zoom;
    let port_rows = node.inputs.len().max(node.outputs.len());
    let param_rows = node.parameters.len();
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
    colors: &ThemeColor,
    window: &mut Window,
) {
    let spacing = 20.0 * viewport.zoom;
    if spacing < 5.0 {
        return;
    }

    let dot_color = Hsla {
        a: 0.3,
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

pub fn paint_edges(
    graph: &Graph,
    viewport: &Viewport,
    bounds: &Bounds<Pixels>,
    selected_edges: &HashSet<EdgeId>,
    edge_style: super::EdgeStyle,
    colors: &ThemeColor,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let normal_color: Hsla = Hsla {
        a: 0.6,
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

        let src_screen =
            viewport.flow_to_screen(src_node.metadata.position.0, src_node.metadata.position.1);
        let tgt_screen =
            viewport.flow_to_screen(tgt_node.metadata.position.0, tgt_node.metadata.position.1);

        let (sx, sy) =
            output_port_screen_center(src_screen, edge.source_port.0 as usize, viewport.zoom);
        let (tx, ty) =
            input_port_screen_center(tgt_screen, edge.target_port.0 as usize, viewport.zoom);

        let sx = sx + ox;
        let sy = sy + oy;
        let tx = tx + ox;
        let ty = ty + oy;

        let highlight = Hsla {
            h: 0.55,
            s: 0.7,
            l: 0.6,
            a: 1.0,
        };
        let is_selected = selected_edges.contains(&edge.id);
        let color = if is_selected { highlight } else { normal_color };
        let stroke_w = if is_selected { 3.0 } else { 2.0 };

        match edge_style {
            super::EdgeStyle::Bezier => {
                let path = horizontal_bezier(sx, sy, tx, ty, 0.25);
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
                let mid_x = (sx + tx) / 2.0;
                let mut builder = PathBuilder::stroke(px(stroke_w));
                builder.move_to(Point::new(px(sx), px(sy)));
                builder.line_to(Point::new(px(mid_x), px(sy)));
                builder.line_to(Point::new(px(mid_x), px(ty)));
                builder.line_to(Point::new(px(tx), px(ty)));
                if let Ok(p) = builder.build() {
                    window.paint_path(p, color);
                }
                paint_arrowhead(window, tx, ty, mid_x, ty, color);
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

    fn color(self, colors: &ThemeColor) -> Hsla {
        match self {
            Self::Critical => Hsla {
                h: 0.0,
                s: 0.85,
                l: 0.60,
                a: 1.0,
            },
            Self::Warn => Hsla {
                h: 0.13,
                s: 0.90,
                l: 0.60,
                a: 1.0,
            },
            Self::Normal => colors.muted_foreground,
        }
    }
}

/// Load color of the readout: muted → yellow → red as the node gets more
/// expensive.
pub fn eval_duration_color(duration: Duration, colors: &ThemeColor) -> Hsla {
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
    colors: &ThemeColor,
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
            .unwrap_or((BASE_NODE_WIDTH * z, 60.0 * z));
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
                Point::new(px(wx + BASE_NODE_PAD * z), px(wy + sh + 2.0 * z)),
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
    colors: &ThemeColor,
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
        a: color.a * opacity,
        ..color
    };

    let node_bg = dim(Hsla {
        a: 0.95,
        ..colors.background
    });
    let highlight = Hsla {
        h: 0.55,
        s: 0.7,
        l: 0.6,
        a: 1.0,
    };
    let node_border = dim(if selected { highlight } else { colors.border });
    let border_w = if selected { 2.0 } else { 1.0 };

    let node_bounds = Bounds::new(
        Point::new(px(x), px(y)),
        Size {
            width: px(w),
            height: px(h),
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
            a: HEADER_TINT_ALPHA,
            ..category_color(category)
        };
        let header = Bounds::new(
            Point::new(px(x), px(y)),
            Size {
                width: px(w),
                height: px(pad + header_h),
            },
        );
        window.paint_quad(fill(header, dim(tint)).corner_radii(Corners {
            top_left: px(corner_r),
            top_right: px(corner_r),
            bottom_left: px(0.0),
            bottom_right: px(0.0),
        }));
    }

    window.paint_quad(
        outline(node_bounds, node_border, BorderStyle::default())
            .corner_radii(px(corner_r))
            .border_widths(px(border_w)),
    );

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
                icon.path(),
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

    let sep_y = y + pad + header_h;
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
            a: 0.2,
            ..colors.border
        }),
    ));

    let port_base_y = sep_y + port_gap;

    for (i, input) in node.inputs.iter().enumerate() {
        let py = port_base_y + (i as f32 + 0.5) * port_row_h;
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

        paint_port_marker(window, (x, py), input_dot_r, shape, dot_color);

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
        let py = port_base_y + (i as f32 + 0.5) * port_row_h;
        let dot_color = dim(port_color(output.data_type));

        paint_port_marker(
            window,
            (x + w, py),
            dot_r,
            port_shape(output.data_type),
            dot_color,
        );

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

    if !node.parameters.is_empty() {
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
                a: 0.2,
                ..colors.border
            }),
        ));

        for (i, param) in node.parameters.iter().enumerate() {
            let py = params_base_y + i as f32 * param_row_h;
            let key_w = paint_mono_text_measured(
                &param.key,
                Point::new(px(x + pad), px(py)),
                font_param,
                dim(colors.muted_foreground),
                window,
                cx,
            );
            let val_str = match &param.value {
                ParameterValue::Float(v) => format!("{v:.2}"),
                ParameterValue::Int(v) => v.to_string(),
                ParameterValue::Bool(v) => v.to_string(),
                ParameterValue::String(v) => v.clone(),
                ParameterValue::Channel(ch) => channel_display(ch),
                ParameterValue::Channel2(chs) => channels_display(chs),
                ParameterValue::Channel3(chs) => channels_display(chs),
                ParameterValue::Channel4(chs) => channels_display(chs),
                ParameterValue::PathPoints(points) => format!("{} points", points.len()),
                ParameterValue::Curve(curve) => format!("{} points", curve.len()),
            };
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

pub fn edge_at_local_pos(
    graph: &Graph,
    viewport: &Viewport,
    lx: f32,
    ly: f32,
    threshold: f32,
    edge_style: super::EdgeStyle,
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

        let src_screen =
            viewport.flow_to_screen(src_node.metadata.position.0, src_node.metadata.position.1);
        let tgt_screen =
            viewport.flow_to_screen(tgt_node.metadata.position.0, tgt_node.metadata.position.1);

        let (sx, sy) =
            output_port_screen_center(src_screen, edge.source_port.0 as usize, viewport.zoom);
        let (tx, ty) =
            input_port_screen_center(tgt_screen, edge.target_port.0 as usize, viewport.zoom);

        let dist = match edge_style {
            super::EdgeStyle::Bezier => {
                let path = horizontal_bezier(sx, sy, tx, ty, 0.25);
                point_to_bezier_distance(lx, ly, &path, 20)
            }
            super::EdgeStyle::Straight => point_to_segment_distance(lx, ly, sx, sy, tx, ty),
            super::EdgeStyle::Step => {
                let mid_x = (sx + tx) / 2.0;
                let d1 = point_to_segment_distance(lx, ly, sx, sy, mid_x, sy);
                let d2 = point_to_segment_distance(lx, ly, mid_x, sy, mid_x, ty);
                let d3 = point_to_segment_distance(lx, ly, mid_x, ty, tx, ty);
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

pub fn port_at_local_pos(graph: &Graph, viewport: &Viewport, lx: f32, ly: f32) -> Option<PortHit> {
    let mut hit = None;
    for node in z_ordered(graph) {
        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        for (i, _input) in node.inputs.iter().enumerate() {
            let (cx, cy) = input_port_screen_center((sx, sy), i, viewport.zoom);
            let dist = ((lx - cx).powi(2) + (ly - cy).powi(2)).sqrt();
            if dist <= PORT_HIT_RADIUS {
                hit = Some(PortHit {
                    node_id: node.id,
                    is_output: false,
                    port_index: i as u32,
                    center: (cx, cy),
                });
            }
        }

        for (i, _output) in node.outputs.iter().enumerate() {
            let (cx, cy) = output_port_screen_center((sx, sy), i, viewport.zoom);
            let dist = ((lx - cx).powi(2) + (ly - cy).powi(2)).sqrt();
            if dist <= PORT_HIT_RADIUS {
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

pub fn find_snap_target(
    graph: &Graph,
    viewport: &Viewport,
    from: &PortHit,
    mouse_lx: f32,
    mouse_ly: f32,
) -> Option<PortHit> {
    let mut best: Option<(f32, PortHit)> = None;

    for node in z_ordered(graph) {
        if node.id == from.node_id {
            continue;
        }

        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

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

            let (cx, cy) = if is_out {
                output_port_screen_center((sx, sy), i, viewport.zoom)
            } else {
                input_port_screen_center((sx, sy), i, viewport.zoom)
            };

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
    bounds: &Bounds<Pixels>,
    _colors: &ThemeColor,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();

    let sx = ox + from.0;
    let sy = oy + from.1;
    let tx = ox + to.0;
    let ty = oy + to.1;

    let draft_color = Hsla {
        h: 0.55,
        s: 0.7,
        l: 0.6,
        a: 1.0,
    };

    let path = horizontal_bezier(sx, sy, tx, ty, 0.25);
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
    _colors: &ThemeColor,
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

    let highlight = Hsla {
        h: 0.55,
        s: 0.7,
        l: 0.6,
        a: 1.0,
    };
    let fill_color = Hsla {
        a: 0.08,
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
        assert!(port_at_local_pos(&hidden, &vp, px, py).is_none());

        let visible = Graph::new().add_node(scalar_source(1, false)).unwrap();
        let hit = port_at_local_pos(&visible, &vp, px, py).expect("visible port hits");
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
            port_at_local_pos(&graph, &vp, x, y).unwrap().node_id,
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
        let from = port_at_local_pos(&graph, &vp, sx, sy).unwrap();
        let (x, y) = input_port_screen_center((0.0, 0.0), 0, vp.zoom);

        assert_eq!(
            find_snap_target(&graph, &vp, &from, x, y).unwrap().node_id,
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
                compute_node_size(&explicit, zoom),
                compute_node_size(&fallback, zoom),
                "node size must not depend on the header glyph at zoom {zoom}"
            );
        }
    }

    /// The readout escalates muted → yellow → red with load.
    #[test]
    fn eval_duration_color_escalates_with_load() {
        let colors = ThemeColor::default();
        let ok = eval_duration_color(Duration::from_millis(2), &colors);
        assert_eq!(ok, colors.muted_foreground);
        let warn = eval_duration_color(Duration::from_millis(15), &colors);
        let critical = eval_duration_color(Duration::from_millis(100), &colors);
        assert_ne!(warn, ok);
        assert_ne!(critical, warn);
        assert_eq!(critical.h, 0.0, "critical is red");
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
        let from = port_at_local_pos(&graph, &vp, px, py).unwrap();
        let (ix, iy) = input_port_screen_center((0.0, 0.0), 0, vp.zoom);
        assert!(find_snap_target(&graph, &vp, &from, ix, iy).is_none());
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
        let from = port_at_local_pos(&graph, &vp, source_x, source_y).unwrap();
        let sink_screen = vp.flow_to_screen(300.0, 0.0);
        let (target_x, target_y) = input_port_screen_center(sink_screen, 0, vp.zoom);
        let target = find_snap_target(&graph, &vp, &from, target_x, target_y)
            .expect("scalar snaps to exposed float parameter");
        assert_eq!(target.node_id, sink_id);
        assert_eq!(target.port_index, 0);

        let color_source = Node::new(source_id, "constant.color")
            .with_output("out", DataTypeId::COLOR)
            .with_position(0.0, 100.0);
        let graph = graph.replace_node(Arc::new(color_source));
        let from = port_at_local_pos(&graph, &vp, source_x, source_y).unwrap();
        assert!(
            find_snap_target(&graph, &vp, &from, target_x, target_y).is_none(),
            "color does not snap to scalar parameter input"
        );
    }
}

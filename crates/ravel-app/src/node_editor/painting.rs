// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::theme::ThemeColor;
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, NodeId};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use super::bezier::horizontal_bezier;
use super::port_colors::port_color;
use super::viewport::Viewport;

/// Display value for an animated channel without an evaluation context:
/// the constant value, the curve's frame-0 sample, or 0 for
/// not-yet-resolvable sources.
fn channel_display(ch: &ravel_core::animation::channel::AnimationChannel) -> String {
    use ravel_core::animation::channel::ChannelSource;
    let v = match &ch.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(0),
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

/// Compact display of a node's evaluation duration (e.g. `0.4ms`, `12ms`,
/// `1.2s`).
pub fn format_eval_duration(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{:.0}ms", ms)
    } else {
        format!("{:.1}ms", ms)
    }
}

/// Load color of the readout: muted → yellow → red as the node gets more
/// expensive.
pub fn eval_duration_color(duration: Duration, colors: &ThemeColor) -> Hsla {
    if duration >= TIMING_CRITICAL {
        Hsla {
            h: 0.0,
            s: 0.85,
            l: 0.60,
            a: 1.0,
        }
    } else if duration >= TIMING_WARN {
        Hsla {
            h: 0.13,
            s: 0.90,
            l: 0.60,
            a: 1.0,
        }
    } else {
        colors.muted_foreground
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
    timings: &HashMap<NodeId, Duration>,
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

        paint_single_node(node, wx, wy, sw, sh, is_selected, z, colors, window, cx);

        // Load readout below the node (evaluation wall-clock time). Hidden
        // while bypassed: the pass-through records no timings, so the
        // readout would show a stale pre-bypass measurement.
        if !node.metadata.bypassed
            && let Some(duration) = timings.get(&node.id)
        {
            paint_text(
                &format_eval_duration(*duration),
                Point::new(px(wx + BASE_NODE_PAD * z), px(wy + sh + 2.0 * z)),
                9.0 * z,
                eval_duration_color(*duration, colors),
                window,
                cx,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_single_node(
    node: &Node,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    selected: bool,
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
    window.paint_quad(
        outline(node_bounds, node_border, BorderStyle::default())
            .corner_radii(px(corner_r))
            .border_widths(px(border_w)),
    );

    let label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
    paint_text(
        label,
        Point::new(px(x + pad), px(y + pad + 2.0 * z)),
        font_header,
        dim(colors.foreground),
        window,
        cx,
    );

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

        let dot = Bounds::new(
            Point::new(px(x - input_dot_r), px(py - input_dot_r)),
            Size {
                width: px(input_dot_r * 2.0),
                height: px(input_dot_r * 2.0),
            },
        );
        window.paint_quad(fill(dot, dot_color).corner_radii(px(input_dot_r)));

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

        let dot = Bounds::new(
            Point::new(px(x + w - dot_r), px(py - dot_r)),
            Size {
                width: px(dot_r * 2.0),
                height: px(dot_r * 2.0),
            },
        );
        window.paint_quad(fill(dot, dot_color).corner_radii(px(dot_r)));

        let text: SharedString = output.name.as_str().into();
        let len = text.len();
        let shaped = window.text_system().shape_line(
            text,
            px(font_port),
            &[TextRun {
                len,
                font: Font {
                    family: SharedString::from("sans-serif"),
                    ..Default::default()
                },
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
            paint_text(
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
            };
            let text: SharedString = val_str.into();
            let len = text.len();
            let shaped = window.text_system().shape_line(
                text,
                px(font_param),
                &[TextRun {
                    len,
                    font: Font {
                        family: SharedString::from("sans-serif"),
                        ..Default::default()
                    },
                    color: dim(colors.foreground),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
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
    for node in graph.nodes() {
        if node.metadata.synthetic {
            continue;
        }
        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        for (i, _input) in node.inputs.iter().enumerate() {
            let (cx, cy) = input_port_screen_center((sx, sy), i, viewport.zoom);
            let dist = ((lx - cx).powi(2) + (ly - cy).powi(2)).sqrt();
            if dist <= PORT_HIT_RADIUS {
                return Some(PortHit {
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
                return Some(PortHit {
                    node_id: node.id,
                    is_output: true,
                    port_index: i as u32,
                    center: (cx, cy),
                });
            }
        }
    }
    None
}

pub fn find_snap_target(
    graph: &Graph,
    viewport: &Viewport,
    from: &PortHit,
    mouse_lx: f32,
    mouse_ly: f32,
) -> Option<PortHit> {
    let mut best: Option<(f32, PortHit)> = None;

    for node in graph.nodes() {
        if node.id == from.node_id || node.metadata.synthetic {
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
            if dist <= SNAP_RADIUS && best.as_ref().is_none_or(|(d, _)| dist < *d) {
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

fn paint_text(
    text: &str,
    origin: Point<Pixels>,
    font_size: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = text.into();
    let len = text.len();
    if len == 0 {
        return;
    }
    let shaped = window.text_system().shape_line(
        text,
        px(font_size),
        &[TextRun {
            len,
            font: Font {
                family: SharedString::from("sans-serif"),
                ..Default::default()
            },
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::DataTypeId;
    use std::sync::Arc;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one for these plain unit tests.
    use core::prelude::v1::test;

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
    fn eval_duration_formats_compactly() {
        assert_eq!(format_eval_duration(Duration::from_micros(400)), "0.4ms");
        assert_eq!(format_eval_duration(Duration::from_millis(12)), "12ms");
        assert_eq!(format_eval_duration(Duration::from_millis(1200)), "1.2s");
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

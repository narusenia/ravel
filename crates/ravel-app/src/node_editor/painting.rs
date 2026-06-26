// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::theme::ThemeColor;
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::NodeId;
use std::collections::{HashMap, HashSet};

use super::bezier::horizontal_bezier;
use super::port_colors::port_color;
use super::viewport::Viewport;

pub const NODE_WIDTH: f32 = 160.0;
const HEADER_H: f32 = 24.0;
const PORT_ROW_H: f32 = 18.0;
const PARAM_ROW_H: f32 = 16.0;
const NODE_PAD: f32 = 8.0;
const PORT_GAP: f32 = 4.0;
const PORT_DOT_R: f32 = 4.0;
const CORNER_R: f32 = 6.0;
const PORT_HIT_RADIUS: f32 = 10.0;
const SNAP_RADIUS: f32 = 20.0;

pub fn compute_node_size(node: &Node) -> (f32, f32) {
    let port_rows = node.inputs.len().max(node.outputs.len());
    let param_rows = node.parameters.len();
    let sep = if param_rows > 0 { 6.0 } else { 0.0 };
    let h = NODE_PAD
        + HEADER_H
        + PORT_GAP
        + port_rows as f32 * PORT_ROW_H
        + sep
        + param_rows as f32 * PARAM_ROW_H
        + NODE_PAD;
    (NODE_WIDTH, h)
}

pub fn input_port_screen_center(node_screen: (f32, f32), port_index: usize) -> (f32, f32) {
    let y = node_screen.1 + NODE_PAD + HEADER_H + PORT_GAP + (port_index as f32 + 0.5) * PORT_ROW_H;
    (node_screen.0, y)
}

pub fn output_port_screen_center(node_screen: (f32, f32), port_index: usize) -> (f32, f32) {
    let y = node_screen.1 + NODE_PAD + HEADER_H + PORT_GAP + (port_index as f32 + 0.5) * PORT_ROW_H;
    (node_screen.0 + NODE_WIDTH, y)
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
    _node_sizes: &HashMap<NodeId, (f32, f32)>,
    colors: &ThemeColor,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let edge_color: Hsla = Hsla {
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

        let src_screen =
            viewport.flow_to_screen(src_node.metadata.position.0, src_node.metadata.position.1);
        let tgt_screen =
            viewport.flow_to_screen(tgt_node.metadata.position.0, tgt_node.metadata.position.1);

        let (sx, sy) = output_port_screen_center(src_screen, edge.source_port.0 as usize);
        let (tx, ty) = input_port_screen_center(tgt_screen, edge.target_port.0 as usize);

        let sx = sx + ox;
        let sy = sy + oy;
        let tx = tx + ox;
        let ty = ty + oy;

        let path = horizontal_bezier(sx, sy, tx, ty, 0.25);

        let mut builder = PathBuilder::stroke(px(2.0));
        builder.move_to(Point::new(px(path.source.0), px(path.source.1)));
        builder.cubic_bezier_to(
            Point::new(px(path.target.0), px(path.target.1)),
            Point::new(px(path.source_control.0), px(path.source_control.1)),
            Point::new(px(path.target_control.0), px(path.target_control.1)),
        );
        if let Ok(p) = builder.build() {
            window.paint_path(p, edge_color);
        }

        paint_arrowhead(
            window,
            tx,
            ty,
            path.target_control.0,
            path.target_control.1,
            edge_color,
        );
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

pub fn paint_nodes(
    graph: &Graph,
    viewport: &Viewport,
    bounds: &Bounds<Pixels>,
    selected: &HashSet<NodeId>,
    node_sizes: &HashMap<NodeId, (f32, f32)>,
    colors: &ThemeColor,
    window: &mut Window,
    cx: &mut App,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let bw: f32 = bounds.size.width.into();
    let bh: f32 = bounds.size.height.into();

    for node in graph.nodes() {
        let (sw, sh) = node_sizes
            .get(&node.id)
            .copied()
            .unwrap_or((NODE_WIDTH, 60.0));
        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        if sx + sw < -50.0 || sx > bw + 50.0 || sy + sh < -50.0 || sy > bh + 50.0 {
            continue;
        }

        let wx = ox + sx;
        let wy = oy + sy;
        let is_selected = selected.contains(&node.id);

        paint_single_node(node, wx, wy, sw, sh, is_selected, colors, window, cx);
    }
}

fn paint_single_node(
    node: &Node,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    selected: bool,
    colors: &ThemeColor,
    window: &mut Window,
    cx: &mut App,
) {
    let node_bg = Hsla {
        a: 0.95,
        ..colors.background
    };
    let node_border = if selected {
        colors.accent
    } else {
        colors.border
    };
    let border_w = if selected { 2.0 } else { 1.0 };

    let node_bounds = Bounds::new(
        Point::new(px(x), px(y)),
        Size {
            width: px(w),
            height: px(h),
        },
    );

    window.paint_quad(fill(node_bounds, node_bg).corner_radii(px(CORNER_R)));
    window.paint_quad(
        outline(node_bounds, node_border, BorderStyle::default())
            .corner_radii(px(CORNER_R))
            .border_widths(px(border_w)),
    );

    let label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
    paint_text(
        label,
        Point::new(px(x + NODE_PAD), px(y + NODE_PAD + 2.0)),
        12.0,
        colors.foreground,
        window,
        cx,
    );

    let sep_y = y + NODE_PAD + HEADER_H;
    let sep_bounds = Bounds::new(
        Point::new(px(x + 4.0), px(sep_y)),
        Size {
            width: px(w - 8.0),
            height: px(1.0),
        },
    );
    window.paint_quad(fill(
        sep_bounds,
        Hsla {
            a: 0.2,
            ..colors.border
        },
    ));

    let port_base_y = sep_y + PORT_GAP;

    for (i, input) in node.inputs.iter().enumerate() {
        let py = port_base_y + (i as f32 + 0.5) * PORT_ROW_H;
        let dot_color = input
            .accepted_types
            .first()
            .map(|t| port_color(*t))
            .unwrap_or(colors.muted_foreground);

        let dot = Bounds::new(
            Point::new(px(x - PORT_DOT_R), px(py - PORT_DOT_R)),
            Size {
                width: px(PORT_DOT_R * 2.0),
                height: px(PORT_DOT_R * 2.0),
            },
        );
        window.paint_quad(fill(dot, dot_color).corner_radii(px(PORT_DOT_R)));

        paint_text(
            &input.name,
            Point::new(px(x + PORT_DOT_R + 4.0), px(py - 5.0)),
            10.0,
            colors.muted_foreground,
            window,
            cx,
        );
    }

    for (i, output) in node.outputs.iter().enumerate() {
        let py = port_base_y + (i as f32 + 0.5) * PORT_ROW_H;
        let dot_color = port_color(output.data_type);

        let dot = Bounds::new(
            Point::new(px(x + w - PORT_DOT_R), px(py - PORT_DOT_R)),
            Size {
                width: px(PORT_DOT_R * 2.0),
                height: px(PORT_DOT_R * 2.0),
            },
        );
        window.paint_quad(fill(dot, dot_color).corner_radii(px(PORT_DOT_R)));

        let text: SharedString = output.name.as_str().into();
        let len = text.len();
        let shaped = window.text_system().shape_line(
            text,
            px(10.0),
            &[TextRun {
                len,
                font: Font {
                    family: SharedString::from("sans-serif"),
                    ..Default::default()
                },
                color: colors.muted_foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let tw: f32 = shaped.width.into();
        shaped
            .paint(
                Point::new(px(x + w - PORT_DOT_R - 4.0 - tw), px(py - 5.0)),
                px(14.0),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
    }

    if !node.parameters.is_empty() {
        let params_base_y =
            port_base_y + node.inputs.len().max(node.outputs.len()) as f32 * PORT_ROW_H + 6.0;

        let sep2 = Bounds::new(
            Point::new(px(x + 4.0), px(params_base_y - 3.0)),
            Size {
                width: px(w - 8.0),
                height: px(1.0),
            },
        );
        window.paint_quad(fill(
            sep2,
            Hsla {
                a: 0.2,
                ..colors.border
            },
        ));

        for (i, param) in node.parameters.iter().enumerate() {
            let py = params_base_y + i as f32 * PARAM_ROW_H;
            paint_text(
                &param.key,
                Point::new(px(x + NODE_PAD), px(py)),
                9.0,
                colors.muted_foreground,
                window,
                cx,
            );
            let val_str = match &param.value {
                ParameterValue::Float(v) => format!("{v:.2}"),
                ParameterValue::Int(v) => v.to_string(),
                ParameterValue::Bool(v) => v.to_string(),
                ParameterValue::String(v) => v.clone(),
            };
            let text: SharedString = val_str.into();
            let len = text.len();
            let shaped = window.text_system().shape_line(
                text,
                px(9.0),
                &[TextRun {
                    len,
                    font: Font {
                        family: SharedString::from("sans-serif"),
                        ..Default::default()
                    },
                    color: colors.foreground,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let tw: f32 = shaped.width.into();
            shaped
                .paint(
                    Point::new(px(x + w - NODE_PAD - tw), px(py)),
                    px(13.0),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
        }
    }
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
        let (sx, sy) = viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);

        for (i, _input) in node.inputs.iter().enumerate() {
            let (cx, cy) = input_port_screen_center((sx, sy), i);
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
            let (cx, cy) = output_port_screen_center((sx, sy), i);
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
            let (cx, cy) = if is_out {
                output_port_screen_center((sx, sy), i)
            } else {
                input_port_screen_center((sx, sy), i)
            };

            let dist = ((mouse_lx - cx).powi(2) + (mouse_ly - cy).powi(2)).sqrt();
            if dist <= SNAP_RADIUS {
                if best.as_ref().map_or(true, |(d, _)| dist < *d) {
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
    }

    best.map(|(_, hit)| hit)
}

pub fn paint_connection_draft(
    from: (f32, f32),
    to: (f32, f32),
    bounds: &Bounds<Pixels>,
    colors: &ThemeColor,
    window: &mut Window,
) {
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();

    let sx = ox + from.0;
    let sy = oy + from.1;
    let tx = ox + to.0;
    let ty = oy + to.1;

    let draft_color = Hsla {
        a: 0.5,
        ..colors.accent
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

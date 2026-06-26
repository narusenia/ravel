// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_core::graph::Graph;
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::undo::UndoStack;
use ravel_i18n::t;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::node_editor::painting::{self, NODE_WIDTH, PortHit, compute_node_size};
use crate::node_editor::viewport::Viewport;

#[derive(Clone)]
enum DragMode {
    None,
    Pan {
        start_mouse: (f32, f32),
        start_viewport: (f32, f32),
    },
    MoveNodes {
        origin_mouse: (f32, f32),
        node_origins: Vec<(NodeId, f32, f32)>,
    },
    Connect {
        from: PortHit,
        to_point: (f32, f32),
        snap: Option<PortHit>,
    },
}

pub struct NodeEditorPanel {
    graph: Graph,
    undo_stack: UndoStack,
    #[allow(dead_code)]
    registry: NodeRegistry,
    viewport: Viewport,
    selected_nodes: HashSet<NodeId>,
    node_sizes: HashMap<NodeId, (f32, f32)>,
    drag: DragMode,
    next_edge_id: u64,
    canvas_origin: Rc<Cell<(f32, f32)>>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl NodeEditorPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let graph = Self::build_demo_graph(&registry);
        let undo_stack = UndoStack::new(graph.clone()).with_max_history(200);
        let node_sizes = Self::compute_all_sizes(&graph);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });

        Self {
            graph,
            undo_stack,
            registry,
            viewport: Viewport {
                x: 50.0,
                y: 50.0,
                zoom: 1.0,
            },
            selected_nodes: HashSet::new(),
            node_sizes,
            drag: DragMode::None,
            next_edge_id: 100,
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            focus_handle: cx.focus_handle(),
            focused_sub,
        }
    }

    fn commit_graph(&mut self, graph: Graph) {
        self.node_sizes = Self::compute_all_sizes(&graph);
        self.graph = graph.clone();
        self.undo_stack.push(graph);
    }

    fn undo(&mut self) {
        if let Some(g) = self.undo_stack.undo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph);
        }
    }

    fn redo(&mut self) {
        if let Some(g) = self.undo_stack.redo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph);
        }
    }

    fn compute_all_sizes(graph: &Graph) -> HashMap<NodeId, (f32, f32)> {
        graph
            .nodes()
            .map(|n| (n.id, compute_node_size(n)))
            .collect()
    }

    fn node_at_local_pos(&self, lx: f32, ly: f32) -> Option<NodeId> {
        for node in self.graph.nodes() {
            let (sx, sy) = self
                .viewport
                .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
            let (w, h) = self
                .node_sizes
                .get(&node.id)
                .copied()
                .unwrap_or((NODE_WIDTH, 60.0));
            if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
                return Some(node.id);
            }
        }
        None
    }

    fn local_from_event(&self, pos: Point<Pixels>) -> (f32, f32) {
        let origin = self.canvas_origin.get();
        let mx: f32 = pos.x.into();
        let my: f32 = pos.y.into();
        (mx - origin.0, my - origin.1)
    }

    fn alloc_edge_id(&mut self) -> EdgeId {
        let id = EdgeId::new(self.next_edge_id);
        self.next_edge_id += 1;
        id
    }

    fn build_demo_graph(registry: &NodeRegistry) -> Graph {
        let blur_id = NodeId::new(1);
        let const_id = NodeId::new(2);
        let merge_id = NodeId::new(3);

        let mut blur = registry.create_node("blur", blur_id).unwrap();
        blur.metadata.position = (300.0, 100.0);

        let mut constant = registry.create_node("constant", const_id).unwrap();
        constant.metadata.position = (50.0, 100.0);

        let mut merge = registry.create_node("merge", merge_id).unwrap();
        merge.metadata.position = (550.0, 150.0);

        Graph::new()
            .add_node(constant)
            .unwrap()
            .add_node(blur)
            .unwrap()
            .add_node(merge)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                blur_id,
                OutputPortIndex(0),
                merge_id,
                InputPortIndex(0),
            )
            .unwrap()
    }
}

impl Panel for NodeEditorPanel {
    fn panel_name(&self) -> &'static str {
        "node_graph"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(ravel_ui::panel::PanelKind::NodeGraph, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        div()
            .text_xs()
            .text_color(color)
            .child(SharedString::from(t!("panel.node_graph")))
    }
}

impl EventEmitter<PanelEvent> for NodeEditorPanel {}

impl Focusable for NodeEditorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NodeEditorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let graph = self.graph.clone();
        let viewport = self.viewport;
        let selected = self.selected_nodes.clone();
        let node_sizes = self.node_sizes.clone();
        let canvas_origin = self.canvas_origin.clone();
        let colors = cx.theme().colors;
        let draft_line = match &self.drag {
            DragMode::Connect {
                from,
                to_point,
                snap,
            } => {
                let to = snap.as_ref().map(|s| s.center).unwrap_or(*to_point);
                Some((from.center, to))
            }
            _ => None,
        };

        div()
            .id("node-editor-panel")
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                if event.keystroke.modifiers.platform {
                    if key == "z" && !event.keystroke.modifiers.shift {
                        this.undo();
                        cx.notify();
                    } else if (key == "z" && event.keystroke.modifiers.shift) || key == "y" {
                        this.redo();
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    cx.set_global(super::FocusedPanelGlobal(Some(
                        ravel_ui::panel::PanelKind::NodeGraph,
                    )));

                    let (lx, ly) = this.local_from_event(event.position);

                    if event.modifiers.alt {
                        this.drag = DragMode::Pan {
                            start_mouse: (lx, ly),
                            start_viewport: (this.viewport.x, this.viewport.y),
                        };
                        cx.notify();
                        return;
                    }

                    if let Some(port_hit) =
                        painting::port_at_local_pos(&this.graph, &this.viewport, lx, ly)
                    {
                        this.drag = DragMode::Connect {
                            from: port_hit.clone(),
                            to_point: (lx, ly),
                            snap: None,
                        };
                        cx.notify();
                        return;
                    }

                    if let Some(node_id) = this.node_at_local_pos(lx, ly) {
                        if !event.modifiers.shift && !this.selected_nodes.contains(&node_id) {
                            this.selected_nodes.clear();
                        }
                        this.selected_nodes.insert(node_id);

                        let origins: Vec<_> = this
                            .selected_nodes
                            .iter()
                            .filter_map(|id| {
                                this.graph
                                    .node(*id)
                                    .map(|n| (*id, n.metadata.position.0, n.metadata.position.1))
                            })
                            .collect();

                        this.drag = DragMode::MoveNodes {
                            origin_mouse: (lx, ly),
                            node_origins: origins,
                        };
                    } else {
                        this.selected_nodes.clear();
                        this.drag = DragMode::Pan {
                            start_mouse: (lx, ly),
                            start_viewport: (this.viewport.x, this.viewport.y),
                        };
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    let (lx, ly) = this.local_from_event(event.position);
                    this.drag = DragMode::Pan {
                        start_mouse: (lx, ly),
                        start_viewport: (this.viewport.x, this.viewport.y),
                    };
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    match &this.drag {
                        DragMode::Connect {
                            from,
                            snap: Some(target),
                            ..
                        } => {
                            let (src_node, src_port, tgt_node, tgt_port) = if from.is_output {
                                (
                                    from.node_id,
                                    OutputPortIndex(from.port_index),
                                    target.node_id,
                                    InputPortIndex(target.port_index),
                                )
                            } else {
                                (
                                    target.node_id,
                                    OutputPortIndex(target.port_index),
                                    from.node_id,
                                    InputPortIndex(from.port_index),
                                )
                            };

                            let edge_id = this.alloc_edge_id();
                            if let Ok(new_graph) = this
                                .graph
                                .clone()
                                .add_edge(edge_id, src_node, src_port, tgt_node, tgt_port)
                            {
                                this.commit_graph(new_graph);
                            }
                        }
                        DragMode::MoveNodes { .. } => {
                            this.commit_graph(this.graph.clone());
                        }
                        _ => {}
                    }
                    this.drag = DragMode::None;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.drag = DragMode::None;
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let (lx, ly) = this.local_from_event(event.position);

                match &this.drag {
                    DragMode::Pan {
                        start_mouse,
                        start_viewport,
                    } => {
                        this.viewport.x = start_viewport.0 + (lx - start_mouse.0);
                        this.viewport.y = start_viewport.1 + (ly - start_mouse.1);
                        cx.notify();
                    }
                    DragMode::MoveNodes {
                        origin_mouse,
                        node_origins,
                    } => {
                        let dx = (lx - origin_mouse.0) / this.viewport.zoom;
                        let dy = (ly - origin_mouse.1) / this.viewport.zoom;

                        let mut graph = this.graph.clone();
                        for &(id, ox, oy) in node_origins {
                            if let Some(node) = graph.node(id) {
                                let mut updated = node.as_ref().clone();
                                updated.metadata.position = (ox + dx, oy + dy);
                                graph = graph.replace_node(Arc::new(updated));
                            }
                        }
                        this.graph = graph;
                        cx.notify();
                    }
                    DragMode::Connect { from, .. } => {
                        let snap =
                            painting::find_snap_target(&this.graph, &this.viewport, from, lx, ly);
                        this.drag = DragMode::Connect {
                            from: from.clone(),
                            to_point: (lx, ly),
                            snap,
                        };
                        cx.notify();
                    }
                    DragMode::None => {}
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                let (lx, ly) = this.local_from_event(event.position);

                if event.modifiers.platform || event.modifiers.control {
                    let zoom_delta = -<Pixels as Into<f32>>::into(delta.y) * 0.01;
                    this.viewport
                        .zoom_toward(this.viewport.zoom + zoom_delta, lx, ly);
                } else {
                    this.viewport.x += <Pixels as Into<f32>>::into(delta.x);
                    this.viewport.y += <Pixels as Into<f32>>::into(delta.y);
                }
                cx.notify();
            }))
            .child(
                canvas(
                    {
                        let co = canvas_origin.clone();
                        move |bounds: Bounds<Pixels>, _window, _cx| {
                            let ox: f32 = bounds.origin.x.into();
                            let oy: f32 = bounds.origin.y.into();
                            co.set((ox, oy));
                        }
                    },
                    move |bounds: Bounds<Pixels>, _, window, cx| {
                        painting::paint_background(&bounds, colors.background, window);
                        painting::paint_grid(&bounds, &viewport, &colors, window);
                        painting::paint_edges(
                            &graph,
                            &viewport,
                            &bounds,
                            &node_sizes,
                            &colors,
                            window,
                        );
                        painting::paint_nodes(
                            &graph,
                            &viewport,
                            &bounds,
                            &selected,
                            &node_sizes,
                            &colors,
                            window,
                            cx,
                        );
                        if let Some((from, to)) = draft_line {
                            painting::paint_connection_draft(from, to, &bounds, &colors, window);
                        }
                    },
                )
                .size_full(),
            )
    }
}

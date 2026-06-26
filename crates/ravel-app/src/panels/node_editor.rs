// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
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

use crate::node_editor::painting::{self, PortHit, compute_node_size, node_width};
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
    SelectBox {
        start: (f32, f32),
        current: (f32, f32),
    },
}

pub struct NodeEditorPanel {
    graph: Graph,
    undo_stack: UndoStack,
    #[allow(dead_code)]
    registry: NodeRegistry,
    viewport: Viewport,
    selected_nodes: HashSet<NodeId>,
    selected_edges: HashSet<EdgeId>,
    node_sizes: HashMap<NodeId, (f32, f32)>,
    drag: DragMode,
    next_node_id: u64,
    next_edge_id: u64,
    canvas_origin: Rc<Cell<(f32, f32)>>,
    last_right_click: Rc<Cell<(f32, f32)>>,
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
        let zoom = 1.0;
        let node_sizes = Self::compute_all_sizes(&graph, zoom);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });

        cx.observe_global::<super::PanelUndoRedo>(|this, cx| {
            if !super::is_panel_focused(ravel_ui::panel::PanelKind::NodeGraph, cx) {
                return;
            }
            let signal = cx.try_global::<super::PanelUndoRedo>().and_then(|g| g.0);
            match signal {
                Some(super::UndoRedoSignal::Undo) => {
                    this.undo();
                    cx.notify();
                }
                Some(super::UndoRedoSignal::Redo) => {
                    this.redo();
                    cx.notify();
                }
                None => {}
            }
        })
        .detach();

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
            selected_edges: HashSet::new(),
            node_sizes,
            drag: DragMode::None,
            next_node_id: 100,
            next_edge_id: 100,
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            last_right_click: Rc::new(Cell::new((0.0, 0.0))),
            focus_handle: cx.focus_handle(),
            focused_sub,
        }
    }

    fn commit_graph(&mut self, graph: Graph) {
        self.node_sizes = Self::compute_all_sizes(&graph, self.viewport.zoom);
        self.graph = graph.clone();
        self.undo_stack.push(graph);
    }

    fn undo(&mut self) {
        if let Some(g) = self.undo_stack.undo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
        }
    }

    fn redo(&mut self) {
        if let Some(g) = self.undo_stack.redo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
        }
    }

    fn refresh_node_sizes(&mut self) {
        self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
    }

    fn compute_all_sizes(graph: &Graph, zoom: f32) -> HashMap<NodeId, (f32, f32)> {
        graph
            .nodes()
            .map(|n| (n.id, compute_node_size(n, zoom)))
            .collect()
    }

    fn node_at_local_pos(&self, lx: f32, ly: f32) -> Option<NodeId> {
        let mut hit = None;
        for node in self.graph.nodes() {
            let (sx, sy) = self
                .viewport
                .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
            let (w, h) = self
                .node_sizes
                .get(&node.id)
                .copied()
                .unwrap_or((node_width(self.viewport.zoom), 60.0));
            if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
                hit = Some(node.id);
            }
        }
        hit
    }

    fn local_from_event(&self, pos: Point<Pixels>) -> (f32, f32) {
        let origin = self.canvas_origin.get();
        let mx: f32 = pos.x.into();
        let my: f32 = pos.y.into();
        (mx - origin.0, my - origin.1)
    }

    fn add_node_from_template(&mut self, type_key: &str) {
        let node_id = self.alloc_node_id();
        if let Some(mut node) = self.registry.create_node(type_key, node_id) {
            let (fx, fy) = self.viewport.screen_to_flow(200.0, 200.0);
            node.metadata.position = (fx, fy);
            if let Ok(new_graph) = self.graph.clone().add_node(node) {
                self.commit_graph(new_graph);
            }
        }
    }

    fn alloc_node_id(&mut self) -> NodeId {
        let id = NodeId::new(self.next_node_id);
        self.next_node_id += 1;
        id
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
        let selected_edges = self.selected_edges.clone();
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
        let selection_box = match &self.drag {
            DragMode::SelectBox { start, current } => Some((*start, *current)),
            _ => None,
        };

        let entity = cx.entity().downgrade();
        let template_keys: Vec<String> = self
            .registry
            .all_templates()
            .map(|t| t.type_key.clone())
            .collect();

        div()
            .id("node-editor-panel")
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                if (key == "delete" || key == "backspace")
                    && (!this.selected_nodes.is_empty() || !this.selected_edges.is_empty())
                {
                    let edges: Vec<_> = this.selected_edges.iter().copied().collect();
                    let nodes: Vec<_> = this.selected_nodes.iter().copied().collect();
                    let graph = edges.into_iter().fold(this.graph.clone(), |g, eid| {
                        g.clone().remove_edge(eid).unwrap_or(g)
                    });
                    let graph = nodes
                        .into_iter()
                        .fold(graph, |g, nid| g.clone().remove_node(nid).unwrap_or(g));
                    this.selected_nodes.clear();
                    this.selected_edges.clear();
                    this.commit_graph(graph);
                    cx.notify();
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

                    if let Some(edge_id) =
                        painting::edge_at_local_pos(&this.graph, &this.viewport, lx, ly, 5.0)
                    {
                        if !event.modifiers.shift {
                            this.selected_edges.clear();
                            this.selected_nodes.clear();
                        }
                        this.selected_edges.insert(edge_id);
                        cx.notify();
                        return;
                    }

                    if let Some(node_id) = this.node_at_local_pos(lx, ly) {
                        if !event.modifiers.shift && !this.selected_nodes.contains(&node_id) {
                            this.selected_nodes.clear();
                        }
                        this.selected_edges.clear();
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
                    } else if event.modifiers.shift {
                        this.drag = DragMode::SelectBox {
                            start: (lx, ly),
                            current: (lx, ly),
                        };
                    } else {
                        this.selected_nodes.clear();
                        this.selected_edges.clear();
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _window, _cx| {
                    let (lx, ly) = this.local_from_event(event.position);
                    this.last_right_click.set((lx, ly));
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

                        let snap_grid = 10.0;
                        let mut graph = this.graph.clone();
                        for &(id, ox, oy) in node_origins {
                            if let Some(node) = graph.node(id) {
                                let mut updated = node.as_ref().clone();
                                let new_x = ((ox + dx) / snap_grid).round() * snap_grid;
                                let new_y = ((oy + dy) / snap_grid).round() * snap_grid;
                                updated.metadata.position = (new_x, new_y);
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
                    DragMode::SelectBox { start, .. } => {
                        let start = *start;
                        this.drag = DragMode::SelectBox {
                            start,
                            current: (lx, ly),
                        };
                        let (sx, ex) = (start.0.min(lx), start.0.max(lx));
                        let (sy, ey) = (start.1.min(ly), start.1.max(ly));
                        this.selected_nodes.clear();
                        for node in this.graph.nodes() {
                            let (nx, ny) = this
                                .viewport
                                .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
                            let (nw, nh) = this
                                .node_sizes
                                .get(&node.id)
                                .copied()
                                .unwrap_or((node_width(this.viewport.zoom), 60.0));
                            if nx + nw > sx && nx < ex && ny + nh > sy && ny < ey {
                                this.selected_nodes.insert(node.id);
                            }
                        }
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
                    this.refresh_node_sizes();
                } else {
                    this.viewport.x += <Pixels as Into<f32>>::into(delta.x);
                    this.viewport.y += <Pixels as Into<f32>>::into(delta.y);
                }
                cx.notify();
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _window, cx| {
                let (lx, ly) = this.local_from_event(event.position);
                let new_zoom = this.viewport.zoom * (1.0 + event.delta);
                this.viewport.zoom_toward(new_zoom, lx, ly);
                this.refresh_node_sizes();
                cx.notify();
            }))
            .context_menu({
                let entity = entity.clone();
                let keys = template_keys.clone();
                let right_click = self.last_right_click.clone();
                let graph_snap = self.graph.clone();
                let vp_snap = self.viewport;
                move |menu, window, cx| {
                    let (lx, ly) = right_click.get();
                    let hit_edge = painting::edge_at_local_pos(&graph_snap, &vp_snap, lx, ly, 5.0);

                    let entity_add = entity.clone();
                    let keys = keys.clone();
                    let menu = menu.submenu(
                        t!("panel.node_graph_menu.add_node"),
                        window,
                        cx,
                        move |sub, _window, _cx| {
                            keys.iter().fold(sub, |sub, key| {
                                let entity = entity_add.clone();
                                let key = key.clone();
                                sub.item(
                                    PopupMenuItem::new(SharedString::from(key.clone())).on_click(
                                        move |_, _window, cx| {
                                            entity
                                                .update(cx, |this, cx| {
                                                    this.add_node_from_template(&key);
                                                    cx.notify();
                                                })
                                                .ok();
                                        },
                                    ),
                                )
                            })
                        },
                    );

                    if let Some(edge_id) = hit_edge {
                        let entity_del = entity.clone();
                        menu.separator().item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.delete_edge")).on_click(
                                move |_, _window, cx| {
                                    entity_del
                                        .update(cx, |this, cx| {
                                            if let Ok(g) = this.graph.clone().remove_edge(edge_id) {
                                                this.commit_graph(g);
                                            }
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            ),
                        )
                    } else {
                        menu
                    }
                }
            })
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
                            &selected_edges,
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
                        if let Some((start, current)) = selection_box {
                            painting::paint_selection_box(start, current, &bounds, &colors, window);
                        }
                    },
                )
                .size_full(),
            )
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor as _};
use ravel_core::graph::Graph;
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::FrameRate;
use ravel_core::undo::UndoStack;
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_i18n::t;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::node_editor::EdgeStyle;
use crate::node_editor::painting::{self, PortHit, compute_node_size, node_width};
use crate::node_editor::viewport::Viewport;
use crate::workspace::{
    EditCopy, EditDelete, EditDuplicate, EditPaste, EditRedo, EditUndo, ViewFit,
};
use ravel_ui::command::CommandId;

use ravel_core::graph::{Edge, Node};

/// GPUI key context used by shortcuts local to the node editor.
pub const KEY_CONTEXT: &str = "NodeEditor";

#[derive(Clone)]
struct ClipboardContent {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

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
    undo_stack: UndoStack<Graph>,
    evaluator: Evaluator,
    gpu_ctx: GpuContext,
    shader_manager: ShaderManager,
    #[allow(dead_code)]
    registry: NodeRegistry,
    viewport: Viewport,
    selected_nodes: HashSet<NodeId>,
    selected_edges: HashSet<EdgeId>,
    /// Node last evaluated for the Viewer (dedup for selection churn).
    last_viewer_node: Option<NodeId>,
    node_sizes: HashMap<NodeId, (f32, f32)>,
    edge_style: EdgeStyle,
    clipboard: Option<ClipboardContent>,
    drag: DragMode,
    next_node_id: u64,
    next_edge_id: u64,
    canvas_origin: Rc<Cell<(f32, f32)>>,
    canvas_size: Rc<Cell<(f32, f32)>>,
    last_right_click: Rc<Cell<(f32, f32)>>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl NodeEditorPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let graph = Self::build_demo_graph(&registry);
        let undo_stack = UndoStack::new(graph.clone()).with_max_history(200);
        let zoom = 1.0;
        let node_sizes = Self::compute_all_sizes(&graph, zoom);

        let gpu_ctx = GpuContext::new_blocking().expect("GPU context initialization failed");
        let mut shader_manager = ShaderManager::new(gpu_ctx.clone());
        let mut evaluator = Evaluator::new();
        ravel_nodes::register_all_processors(&mut evaluator, &graph, &gpu_ctx, &mut shader_manager);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(
            ravel_ui::panel::PanelKind::NodeGraph,
            &focus_handle,
            window,
            cx,
        );

        cx.observe_global::<super::PropertyChanged>(|this, cx| {
            let Some(changed) = cx.try_global::<super::PropertyChanged>().cloned() else {
                return;
            };
            this.apply_property_change(&changed, cx);
        })
        .detach();

        Self {
            graph,
            undo_stack,
            evaluator,
            gpu_ctx,
            shader_manager,
            registry,
            viewport: Viewport {
                x: 50.0,
                y: 50.0,
                zoom: 1.0,
            },
            selected_nodes: HashSet::new(),
            selected_edges: HashSet::new(),
            last_viewer_node: None,
            node_sizes,
            edge_style: EdgeStyle::default(),
            clipboard: None,
            drag: DragMode::None,
            next_node_id: 100,
            next_edge_id: 100,
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            canvas_size: Rc::new(Cell::new((800.0, 600.0))),
            last_right_click: Rc::new(Cell::new((0.0, 0.0))),
            focus_handle,
            focus_subscriptions,
            focused_sub,
        }
    }

    fn commit_graph(&mut self, graph: Graph, cx: &mut Context<Self>) {
        self.node_sizes = Self::compute_all_sizes(&graph, self.viewport.zoom);
        self.graph = graph.clone();
        self.undo_stack.push(graph);
        self.sync_processors();
        self.notify_properties_selection(cx);
        self.evaluate_for_viewer(true, cx);
    }

    fn sync_processors(&mut self) {
        self.evaluator = Evaluator::new();
        ravel_nodes::register_all_processors(
            &mut self.evaluator,
            &self.graph,
            &self.gpu_ctx,
            &mut self.shader_manager,
        );
    }

    /// Applies a property edit from the Properties panel.
    ///
    /// Numeric values are clamped to the parameter's hard range (registry
    /// metadata). Live edits (`commit == false`, e.g. mid-scrub) update the
    /// graph and re-evaluate but do not record undo; the gesture-ending
    /// `commit == true` event pushes one undo snapshot for the whole edit.
    fn apply_property_change(&mut self, changed: &super::PropertyChanged, cx: &mut Context<Self>) {
        use ravel_core::graph::ParameterValue;
        use ravel_ui::properties::PropertyValue;

        let mut graph = self.graph.clone();
        for node_id in &changed.node_ids {
            let Some(node) = graph.node(*node_id) else {
                continue;
            };
            let range = self.registry.param_range(&node.type_key, &changed.key);
            let param_value = match &changed.value {
                PropertyValue::Float(v) => ParameterValue::Float(range.map_or(*v, |r| r.clamp(*v))),
                PropertyValue::Int(v) => {
                    ParameterValue::Int(range.map_or(*v, |r| r.clamp(*v as f32).round() as i32))
                }
                PropertyValue::Bool(v) => ParameterValue::Bool(*v),
                PropertyValue::String(v) => ParameterValue::String(v.clone()),
                PropertyValue::Color { .. } => return,
            };
            let mut updated = (**node).clone();
            if let Some(param) = updated.parameters.iter_mut().find(|p| p.key == changed.key) {
                param.value = param_value;
            }
            graph = graph.replace_node(Arc::new(updated));
        }

        self.node_sizes = Self::compute_all_sizes(&graph, self.viewport.zoom);
        self.graph = graph.clone();
        if changed.commit {
            self.undo_stack.push(graph);
        }
        self.sync_processors();
        self.evaluate_for_viewer(true, cx);
        cx.notify();
    }

    fn copy_selected(&mut self) {
        if self.selected_nodes.is_empty() {
            return;
        }
        let nodes: Vec<Node> = self
            .selected_nodes
            .iter()
            .filter_map(|id| self.graph.node(*id).map(|n| (**n).clone()))
            .collect();
        let node_ids: HashSet<NodeId> = self.selected_nodes.clone();
        let edges: Vec<Edge> = self
            .graph
            .edges()
            .filter(|e| node_ids.contains(&e.source) && node_ids.contains(&e.target))
            .cloned()
            .collect();
        self.clipboard = Some(ClipboardContent { nodes, edges });
    }

    fn paste(&mut self, offset: (f32, f32), cx: &mut Context<Self>) {
        let content = match &self.clipboard {
            Some(c) => c.clone(),
            None => return,
        };

        let mut id_map: HashMap<NodeId, NodeId> = HashMap::new();
        let mut graph = self.graph.clone();

        for node in &content.nodes {
            let new_id = self.alloc_node_id();
            id_map.insert(node.id, new_id);
            let mut new_node = node.clone();
            new_node.id = new_id;
            new_node.metadata.position.0 += offset.0;
            new_node.metadata.position.1 += offset.1;
            if let Ok(g) = graph.clone().add_node(new_node) {
                graph = g;
            }
        }

        for edge in &content.edges {
            let Some(&new_src) = id_map.get(&edge.source) else {
                continue;
            };
            let Some(&new_tgt) = id_map.get(&edge.target) else {
                continue;
            };
            let new_edge_id = self.alloc_edge_id();
            if let Ok(g) = graph.clone().add_edge(
                new_edge_id,
                new_src,
                edge.source_port,
                new_tgt,
                edge.target_port,
            ) {
                graph = g;
            }
        }

        self.selected_nodes.clear();
        for new_id in id_map.values() {
            self.selected_nodes.insert(*new_id);
        }
        self.commit_graph(graph, cx);
    }

    fn duplicate_selected(&mut self, cx: &mut Context<Self>) {
        if self.selected_nodes.is_empty() {
            return;
        }
        self.copy_selected();
        self.paste((20.0, 20.0), cx);
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if self.selected_nodes.is_empty() && self.selected_edges.is_empty() {
            return;
        }

        let edges: Vec<_> = self.selected_edges.iter().copied().collect();
        let nodes: Vec<_> = self.selected_nodes.iter().copied().collect();
        let graph = edges
            .into_iter()
            .fold(self.graph.clone(), |graph, edge_id| {
                graph.clone().remove_edge(edge_id).unwrap_or(graph)
            });
        let graph = nodes.into_iter().fold(graph, |graph, node_id| {
            graph.clone().remove_node(node_id).unwrap_or(graph)
        });
        self.selected_nodes.clear();
        self.selected_edges.clear();
        self.commit_graph(graph, cx);
    }

    fn trace_action(cx: &mut App, command: CommandId, outcome: &str) {
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::PanelKeyDown,
                command: Some(command),
                focused_panel: crate::trace::focused_panel(cx),
                handler: "NodeEditorPanel::on_action",
                outcome: Some(outcome.to_string()),
            },
        );
    }

    fn on_copy(&mut self, _: &EditCopy, _window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected();
        Self::trace_action(cx, CommandId::EditCopy, "copy_selected");
    }

    fn on_undo(&mut self, _: &EditUndo, _window: &mut Window, cx: &mut Context<Self>) {
        self.undo();
        self.notify_properties_selection(cx);
        self.evaluate_for_viewer(true, cx);
        Self::trace_action(cx, CommandId::EditUndo, "undo");
        cx.notify();
    }

    fn on_redo(&mut self, _: &EditRedo, _window: &mut Window, cx: &mut Context<Self>) {
        self.redo();
        self.notify_properties_selection(cx);
        self.evaluate_for_viewer(true, cx);
        Self::trace_action(cx, CommandId::EditRedo, "redo");
        cx.notify();
    }

    fn on_paste(&mut self, _: &EditPaste, _window: &mut Window, cx: &mut Context<Self>) {
        self.paste((20.0, 20.0), cx);
        Self::trace_action(cx, CommandId::EditPaste, "paste");
        cx.notify();
    }

    fn on_duplicate(&mut self, _: &EditDuplicate, _window: &mut Window, cx: &mut Context<Self>) {
        self.duplicate_selected(cx);
        Self::trace_action(cx, CommandId::EditDuplicate, "duplicate_selected");
        cx.notify();
    }

    fn on_delete(&mut self, _: &EditDelete, _window: &mut Window, cx: &mut Context<Self>) {
        self.delete_selected(cx);
        Self::trace_action(cx, CommandId::EditDelete, "delete_selected");
        cx.notify();
    }

    fn on_fit_view(&mut self, _: &ViewFit, _window: &mut Window, cx: &mut Context<Self>) {
        self.fit_view();
        Self::trace_action(cx, CommandId::ViewFit, "fit_view");
        cx.notify();
    }

    fn fit_view(&mut self) {
        let rects: Vec<(f32, f32, f32, f32)> = self
            .graph
            .nodes()
            .map(|n| {
                let (w, h) = self.node_sizes.get(&n.id).copied().unwrap_or((160.0, 60.0));
                let unzoomed_w = w / self.viewport.zoom;
                let unzoomed_h = h / self.viewport.zoom;
                (
                    n.metadata.position.0,
                    n.metadata.position.1,
                    unzoomed_w,
                    unzoomed_h,
                )
            })
            .collect();
        let (cw, ch) = self.canvas_size.get();
        self.viewport.fit_to_content(&rects, cw, ch, 40.0);
        self.refresh_node_sizes();
    }

    fn bypass_node(&mut self, node_id: NodeId) {
        let incoming: Vec<_> = self
            .graph
            .edges()
            .filter(|e| e.target == node_id)
            .map(|e| (e.id, e.source, e.source_port))
            .collect();
        let outgoing: Vec<_> = self
            .graph
            .edges()
            .filter(|e| e.source == node_id)
            .map(|e| (e.id, e.target, e.target_port))
            .collect();

        if incoming.len() == 1 && outgoing.len() == 1 {
            let (_, src, src_port) = incoming[0];
            let (_, tgt, tgt_port) = outgoing[0];
            if let Ok(g) = self.graph.clone().remove_node(node_id) {
                let edge_id = self.alloc_edge_id();
                if let Ok(connected) = g.clone().add_edge(edge_id, src, src_port, tgt, tgt_port) {
                    self.graph = connected;
                } else {
                    self.graph = g;
                }
            }
        } else if let Ok(g) = self.graph.clone().remove_node(node_id) {
            self.graph = g;
        }
    }

    fn notify_properties_selection(&mut self, cx: &mut Context<Self>) {
        let target = if self.selected_nodes.is_empty() {
            super::PropertiesTarget::Empty
        } else {
            let ids: Vec<_> = self.selected_nodes.iter().copied().collect();
            let nodes: Vec<_> = ids
                .iter()
                .filter_map(|id| self.graph.node(*id).cloned())
                .collect();
            super::PropertiesTarget::Nodes { ids, nodes }
        };
        cx.set_global(super::SelectedPropertiesTarget(target));
        self.evaluate_for_viewer(false, cx);
    }

    /// Evaluates the selected node for the Viewer and publishes the frame.
    ///
    /// Skipped while a select-box drag is active (the drag-end mouse-up
    /// re-triggers it) and when the same node was already evaluated with an
    /// unchanged graph. `force` bypasses the dedup after graph mutations.
    fn evaluate_for_viewer(&mut self, force: bool, cx: &mut Context<Self>) {
        use ravel_core::geometry::Geometry;
        use ravel_core::types::{FrameBuffer, NodeData};

        if matches!(self.drag, DragMode::SelectBox { .. }) {
            return;
        }

        let node_id = match self.selected_nodes.iter().next() {
            Some(id) => *id,
            None => {
                self.last_viewer_node = None;
                cx.set_global(super::ViewerFrame(None));
                return;
            }
        };

        if !force && self.last_viewer_node == Some(node_id) {
            return;
        }
        self.last_viewer_node = Some(node_id);

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (512, 512));
        let result = self.evaluator.evaluate(&self.graph, node_id, &ctx);

        let frame = match result {
            Ok(data) => {
                if let Some(fb) = data.downcast_ref::<FrameBuffer>() {
                    Some(Arc::new(fb.clone()))
                } else if let Some(geo) = data.downcast_ref::<Geometry>() {
                    let rast_node =
                        ravel_core::graph::Node::new(NodeId::new(u64::MAX), "rasterize")
                            .with_param("fill", ravel_core::graph::ParameterValue::Bool(true))
                            .with_param(
                                "stroke_width",
                                ravel_core::graph::ParameterValue::Float(0.0),
                            );
                    let proc = ravel_nodes::rasterize::RasterizeProcessor::from_node(&rast_node);
                    let inputs: Vec<&dyn NodeData> = vec![geo];
                    proc.process(&ctx, &inputs).ok().and_then(|d| {
                        d.downcast_ref::<FrameBuffer>()
                            .map(|fb| Arc::new(fb.clone()))
                    })
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        cx.set_global(super::ViewerFrame(frame));
    }

    fn undo(&mut self) {
        if let Some(g) = self.undo_stack.undo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
            self.sync_processors();
        }
    }

    fn redo(&mut self) {
        if let Some(g) = self.undo_stack.redo() {
            self.graph = g.clone();
            self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
            self.sync_processors();
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

    fn add_node_from_template(&mut self, type_key: &str, cx: &mut Context<Self>) {
        let node_id = self.alloc_node_id();
        if let Some(mut node) = self.registry.create_node(type_key, node_id) {
            let (fx, fy) = self.viewport.screen_to_flow(200.0, 200.0);
            node.metadata.position = (fx, fy);
            if let Ok(new_graph) = self.graph.clone().add_node(node) {
                self.commit_graph(new_graph, cx);
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
        super::tab_title(
            Some(ravel_ui::panel::PanelKind::NodeGraph),
            SharedString::from(t!("panel.node_graph")),
            color,
        )
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
        let edge_style = self.edge_style;
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
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_duplicate))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_fit_view))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
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

                    if let Some(edge_id) = painting::edge_at_local_pos(
                        &this.graph,
                        &this.viewport,
                        lx,
                        ly,
                        5.0,
                        this.edge_style,
                    ) {
                        if !event.modifiers.shift {
                            this.selected_edges.clear();
                            this.selected_nodes.clear();
                        }
                        this.selected_edges.insert(edge_id);
                        this.notify_properties_selection(cx);
                        cx.notify();
                        return;
                    }

                    if let Some(node_id) = this.node_at_local_pos(lx, ly) {
                        if !event.modifiers.shift && !this.selected_nodes.contains(&node_id) {
                            this.selected_nodes.clear();
                        }
                        this.selected_edges.clear();
                        this.selected_nodes.insert(node_id);
                        this.notify_properties_selection(cx);

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
                        this.notify_properties_selection(cx);
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
                cx.listener(move |this, event: &MouseDownEvent, _window, _cx| {
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

                            let mut graph = this.graph.clone();
                            let existing: Vec<_> = graph
                                .edges()
                                .filter(|e| e.target == tgt_node && e.target_port == tgt_port)
                                .map(|e| e.id)
                                .collect();
                            for eid in existing {
                                graph = graph.clone().remove_edge(eid).unwrap_or(graph);
                            }
                            let edge_id = this.alloc_edge_id();
                            if let Ok(new_graph) =
                                graph.add_edge(edge_id, src_node, src_port, tgt_node, tgt_port)
                            {
                                this.commit_graph(new_graph, cx);
                            }
                        }
                        DragMode::MoveNodes { .. } => {
                            this.commit_graph(this.graph.clone(), cx);
                        }
                        _ => {}
                    }
                    let was_select_box = matches!(this.drag, DragMode::SelectBox { .. });
                    this.drag = DragMode::None;
                    if was_select_box {
                        this.evaluate_for_viewer(false, cx);
                    }
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
                        this.notify_properties_selection(cx);
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
                let sizes_snap = self.node_sizes.clone();
                let selected_snap = self.selected_nodes.clone();
                let es = self.edge_style;
                move |menu, window, cx| {
                    let (lx, ly) = right_click.get();
                    let hit_edge =
                        painting::edge_at_local_pos(&graph_snap, &vp_snap, lx, ly, 5.0, es);
                    let hit_node = {
                        let mut found = None;
                        for node in graph_snap.nodes() {
                            let (sx, sy) = vp_snap
                                .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
                            let (w, h) = sizes_snap
                                .get(&node.id)
                                .copied()
                                .unwrap_or((node_width(vp_snap.zoom), 60.0));
                            if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
                                found = Some(node.id);
                            }
                        }
                        found
                    };

                    let entity_add = entity.clone();
                    let keys = keys.clone();
                    let mut menu = menu.submenu(
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
                                                    this.add_node_from_template(&key, cx);
                                                    cx.notify();
                                                })
                                                .ok();
                                        },
                                    ),
                                )
                            })
                        },
                    );

                    if hit_node.is_some() || !selected_snap.is_empty() {
                        let entity_del = entity.clone();
                        let sel = selected_snap.clone();
                        let hit = hit_node;
                        menu = menu.separator().item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.delete_node")).on_click(
                                move |_, _window, cx| {
                                    entity_del
                                        .update(cx, |this, cx| {
                                            let targets: Vec<NodeId> = if sel.is_empty() {
                                                hit.into_iter().collect()
                                            } else {
                                                sel.iter().copied().collect()
                                            };
                                            let graph = targets
                                                .iter()
                                                .fold(this.graph.clone(), |g, nid| {
                                                    g.clone().remove_node(*nid).unwrap_or(g)
                                                });
                                            this.selected_nodes.clear();
                                            this.selected_edges.clear();
                                            this.commit_graph(graph, cx);
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            ),
                        );

                        let entity_bypass = entity.clone();
                        let sel_bypass = selected_snap.clone();
                        let hit_bypass = hit_node;
                        menu = menu.item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.bypass_node")).on_click(
                                move |_, _window, cx| {
                                    entity_bypass
                                        .update(cx, |this, cx| {
                                            let targets: Vec<NodeId> = if sel_bypass.is_empty() {
                                                hit_bypass.into_iter().collect()
                                            } else {
                                                sel_bypass.iter().copied().collect()
                                            };
                                            for nid in targets {
                                                this.bypass_node(nid);
                                            }
                                            this.selected_nodes.clear();
                                            this.selected_edges.clear();
                                            this.commit_graph(this.graph.clone(), cx);
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            ),
                        );
                    }

                    if let Some(edge_id) = hit_edge {
                        let entity_del = entity.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.delete_edge")).on_click(
                                move |_, _window, cx| {
                                    entity_del
                                        .update(cx, |this, cx| {
                                            if let Ok(g) = this.graph.clone().remove_edge(edge_id) {
                                                this.commit_graph(g, cx);
                                            }
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            ),
                        );
                    }

                    let entity_es = entity.clone();
                    menu.separator()
                        .submenu("Edge Style", window, cx, move |sub, _window, _cx| {
                            let e1 = entity_es.clone();
                            let e2 = entity_es.clone();
                            let e3 = entity_es.clone();
                            sub.item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_bezier"))
                                    .on_click(move |_, _window, cx| {
                                        e1.update(cx, |this, cx| {
                                            this.edge_style = EdgeStyle::Bezier;
                                            cx.notify();
                                        })
                                        .ok();
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_straight"))
                                    .on_click(move |_, _window, cx| {
                                        e2.update(cx, |this, cx| {
                                            this.edge_style = EdgeStyle::Straight;
                                            cx.notify();
                                        })
                                        .ok();
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_step"))
                                    .on_click(move |_, _window, cx| {
                                        e3.update(cx, |this, cx| {
                                            this.edge_style = EdgeStyle::Step;
                                            cx.notify();
                                        })
                                        .ok();
                                    }),
                            )
                        })
                }
            })
            .child(
                canvas(
                    {
                        let co = canvas_origin.clone();
                        let cs = self.canvas_size.clone();
                        move |bounds: Bounds<Pixels>, _window, _cx| {
                            let ox: f32 = bounds.origin.x.into();
                            let oy: f32 = bounds.origin.y.into();
                            co.set((ox, oy));
                            let w: f32 = bounds.size.width.into();
                            let h: f32 = bounds.size.height.into();
                            cs.set((w, h));
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
                            edge_style,
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

#[cfg(test)]
mod tests {
    use super::NodeEditorPanel;
    use gpui::TestAppContext;
    use ravel_core::graph::ParameterValue;
    use ravel_core::id::NodeId;
    use ravel_ui::properties::PropertyValue;

    fn blur_radius(panel: &NodeEditorPanel) -> f32 {
        let node = panel.graph.node(NodeId::new(1)).expect("blur node");
        match node
            .parameters
            .iter()
            .find(|p| p.key == "radius")
            .map(|p| &p.value)
        {
            Some(ParameterValue::Float(v)) => *v,
            other => panic!("unexpected radius parameter: {other:?}"),
        }
    }

    fn change(value: f32, commit: bool) -> crate::panels::PropertyChanged {
        crate::panels::PropertyChanged {
            node_ids: vec![NodeId::new(1)],
            key: "radius".into(),
            value: PropertyValue::Float(value),
            commit,
        }
    }

    #[gpui::test]
    fn scrub_gesture_records_a_single_undo_step(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(NodeEditorPanel::new);

        window
            .update(cx, |panel, _window, cx| {
                let original = blur_radius(panel);

                // Live scrub: many Change events, no undo snapshots.
                panel.apply_property_change(&change(10.0, false), cx);
                panel.apply_property_change(&change(20.0, false), cx);
                panel.apply_property_change(&change(30.0, false), cx);
                assert!((blur_radius(panel) - 30.0).abs() < f32::EPSILON);

                // Drag end: one commit, one undo snapshot.
                panel.apply_property_change(&change(42.0, true), cx);
                assert!((blur_radius(panel) - 42.0).abs() < f32::EPSILON);

                // A single undo returns to the pre-drag value.
                panel.undo();
                assert!(
                    (blur_radius(panel) - original).abs() < f32::EPSILON,
                    "one undo restores pre-gesture value, got {}",
                    blur_radius(panel)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn property_change_clamps_to_hard_range(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(NodeEditorPanel::new);

        window
            .update(cx, |panel, _window, cx| {
                // blur.radius hard range is 0..=500.
                panel.apply_property_change(&change(9999.0, true), cx);
                assert!((blur_radius(panel) - 500.0).abs() < f32::EPSILON);

                panel.apply_property_change(&change(-50.0, true), cx);
                assert!(blur_radius(panel).abs() < f32::EPSILON);
            })
            .unwrap();
    }
}

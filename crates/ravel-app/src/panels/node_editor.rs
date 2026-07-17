// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node editor panel: edits exactly one network of the document at a time
//! (REQ-LAYER-011).
//!
//! The edited network is identified by an ownership path
//! ([`NetworkPath`]: `CompId / LayerId / [SubnetNodeId ...]`). The timeline
//! opens a layer's network via [`NodeEditorPanel::open_network`]
//! (double-click / "open network"), double-clicking a subnet node dives one
//! level deeper, and the breadcrumb bar returns to any ancestor. Selecting a
//! layer never force-switches the context — only the explicit open does.
//!
//! Edits are committed to the app-wide [`ProjectState`]: the new network is
//! spliced into the document (structural sharing) and recorded as one
//! Document-level undo step (REQ-LAYER-009). Undo/redo are *not* handled
//! here — the edit actions bubble to the workspace, which routes them to the
//! document store, and this panel resyncs through its project observer.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::animation::curve::KeyframeCurve;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::graph::Graph;
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::builtin::register_builtins;
use ravel_core::registry::{NodeRegistry, ParamRange};
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::document::{NetworkPath, replace_network, resolve_network};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::node_editor::EdgeStyle;
use crate::node_editor::painting::{self, PortHit, compute_node_size, node_width};
use crate::node_editor::viewport::Viewport;
use crate::project_state::ProjectState;
use crate::workspace::{EditCopy, EditDelete, EditDuplicate, EditPaste, ViewFit};
use ravel_ui::command::CommandId;

use ravel_core::graph::{Edge, Node, ParameterValue};

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
        /// Whether any position actually changed; a plain click-release on a
        /// node must not record an undo step.
        moved: bool,
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

// ----- keyframe editing (REQ-LAYER-004) -------------------------------------

/// Whether the channel has a keyframe exactly at `frame`.
fn channel_has_key(channel: &AnimationChannel, frame: u64) -> bool {
    match &channel.source {
        ChannelSource::Keyframes(curve) => curve.keyframes().iter().any(|k| k.frame == frame),
        _ => false,
    }
}

/// Insert (or overwrite) a keyframe at `frame` holding the channel's current
/// value there; a constant channel converts to keyframes, keeping its value
/// as the curve default. Returns `false` for non-key-editable sources.
fn insert_channel_key(channel: &mut AnimationChannel, frame: u64) -> bool {
    match &mut channel.source {
        ChannelSource::Constant(v) => {
            let mut curve = KeyframeCurve::with_default(*v);
            curve.insert(frame, *v, Interpolation::Linear);
            channel.source = ChannelSource::Keyframes(curve);
            true
        }
        ChannelSource::Keyframes(curve) => {
            let value = curve.sample(frame);
            curve.insert(frame, value, Interpolation::Linear);
            true
        }
        _ => false,
    }
}

/// Remove the keyframe at `frame`; the last key reverts the channel to a
/// constant holding the removed key's value (mirroring
/// `ravel_ui::keyframes::remove_keyframe`).
fn remove_channel_key(channel: &mut AnimationChannel, frame: u64) -> bool {
    let ChannelSource::Keyframes(curve) = &mut channel.source else {
        return false;
    };
    let Some(removed) = curve.remove(frame) else {
        return false;
    };
    if curve.is_empty() {
        channel.source = ChannelSource::Constant(removed.value);
    }
    true
}

/// Toggle a keyframe at `frame` on every component channel: removes the key
/// from all when all components are keyed there, otherwise inserts the
/// current value into every component. Returns `false` when nothing changed.
fn toggle_components_key(channels: &mut [AnimationChannel], frame: u64) -> bool {
    let all_keyed = channels.iter().all(|ch| channel_has_key(ch, frame));
    let mut changed = false;
    for channel in channels {
        changed |= if all_keyed {
            remove_channel_key(channel, frame)
        } else {
            insert_channel_key(channel, frame)
        };
    }
    changed
}

/// The new parameter value for a Properties-panel numeric edit, keeping
/// animated channels animated (REQ-LAYER-004): a constant channel updates
/// its constant, a keyframed channel gets a key at `local_frame` (live
/// `Change`s overwrite the same key, so one scrub gesture still records one
/// undo step). Without a local frame the value falls back to a plain
/// constant — the legacy flattening behavior. Returns `None` when the edit
/// does not apply to the parameter (color values; multi-component channels,
/// which are read-only in the panel for now).
fn edited_param_value(
    existing: &ParameterValue,
    value: &ravel_ui::properties::PropertyValue,
    range: Option<&ParamRange>,
    local_frame: Option<u64>,
) -> Option<ParameterValue> {
    use ravel_ui::properties::PropertyValue;
    match value {
        PropertyValue::Float(v) => {
            let v = range.map_or(*v, |r| r.clamp(*v));
            match existing {
                ParameterValue::Channel(channel) => match &channel.source {
                    ChannelSource::Constant(_) => {
                        Some(ParameterValue::Channel(AnimationChannel::constant(v)))
                    }
                    ChannelSource::Keyframes(curve) => match local_frame {
                        Some(frame) => {
                            let mut curve = curve.clone();
                            curve.insert(frame, v, Interpolation::Linear);
                            Some(ParameterValue::Channel(AnimationChannel::keyframes(curve)))
                        }
                        None => Some(ParameterValue::Float(v)),
                    },
                    // Expressions / node outputs are not key-editable;
                    // flattening matches the legacy behavior.
                    _ => Some(ParameterValue::Float(v)),
                },
                ParameterValue::Channel2(_)
                | ParameterValue::Channel3(_)
                | ParameterValue::Channel4(_) => None,
                _ => Some(ParameterValue::Float(v)),
            }
        }
        PropertyValue::Int(v) => Some(ParameterValue::Int(
            range.map_or(*v, |r| r.clamp(*v as f32).round() as i32),
        )),
        PropertyValue::Bool(v) => Some(ParameterValue::Bool(*v)),
        PropertyValue::String(v) => Some(ParameterValue::String(v.clone())),
        PropertyValue::Color { .. } => None,
    }
}

pub struct NodeEditorPanel {
    /// The app-wide document state; `None` only when the panel outlives it.
    project: Option<Entity<ProjectState>>,
    /// Ownership path of the network being edited; `None` until a network
    /// is opened from the timeline (REQ-LAYER-011).
    context: Option<NetworkPath>,
    /// Display copy of the network at `context` (empty without a context).
    /// Mutated locally during drags; committed to the document on gesture
    /// end.
    graph: Graph,
    registry: NodeRegistry,
    viewport: Viewport,
    selected_nodes: HashSet<NodeId>,
    selected_edges: HashSet<EdgeId>,
    node_sizes: HashMap<NodeId, (f32, f32)>,
    edge_style: EdgeStyle,
    clipboard: Option<ClipboardContent>,
    drag: DragMode,
    canvas_origin: Rc<Cell<(f32, f32)>>,
    canvas_size: Rc<Cell<(f32, f32)>>,
    last_right_click: Rc<Cell<(f32, f32)>>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
}

impl NodeEditorPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, project, cx| {
                this.sync_from_project(&project, cx);
            })
        });

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

        cx.set_global(super::NodeEditorHandle(cx.entity().downgrade()));

        cx.observe_global::<super::PropertyChanged>(|this, cx| {
            let Some(changed) = cx.try_global::<super::PropertyChanged>().cloned() else {
                return;
            };
            this.apply_property_change(&changed, cx);
        })
        .detach();

        Self {
            project,
            context: None,
            graph: Graph::new(),
            registry,
            viewport: Viewport {
                x: 50.0,
                y: 50.0,
                zoom: 1.0,
            },
            selected_nodes: HashSet::new(),
            selected_edges: HashSet::new(),
            node_sizes: HashMap::new(),
            edge_style: EdgeStyle::default(),
            clipboard: None,
            drag: DragMode::None,
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            canvas_size: Rc::new(Cell::new((800.0, 600.0))),
            last_right_click: Rc::new(Cell::new((0.0, 0.0))),
            focus_handle,
            focus_subscriptions,
            focused_sub,
            project_sub,
        }
    }

    // ----- network context (REQ-LAYER-011) ----------------------------------

    /// The ownership path of the network currently being edited.
    pub fn context(&self) -> Option<&NetworkPath> {
        self.context.as_ref()
    }

    /// Open the network at `path` (timeline double-click / open-network
    /// command, subnet dive, breadcrumb jump).
    pub fn open_network(&mut self, path: NetworkPath, cx: &mut Context<Self>) {
        if self.context.as_ref() == Some(&path) {
            return;
        }
        self.context = Some(path);
        self.selected_nodes.clear();
        self.selected_edges.clear();
        self.refresh_from_document(cx);
        self.fit_view();
        self.notify_properties_selection(cx);
        cx.notify();
    }

    fn enter_subnet(&mut self, subnet: NodeId, cx: &mut Context<Self>) {
        if let Some(context) = &self.context {
            self.open_network(context.entered(subnet), cx);
        }
    }

    /// Re-resolve the display graph from the document. A context whose
    /// network vanished (deleted layer / subnet, undo) pops to the nearest
    /// surviving ancestor, or to no context at all.
    fn refresh_from_document(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let document = project.read(cx).document().clone();

        let resolved = loop {
            let Some(context) = &self.context else {
                break None;
            };
            if let Some(graph) = resolve_network(&document, context) {
                break Some(graph.clone());
            }
            if context.subnets.is_empty() {
                self.context = None;
                break None;
            }
            let depth = context.subnets.len() - 1;
            self.context = Some(context.truncated(depth));
        };

        let graph = resolved.unwrap_or_default();
        if self.graph != graph {
            self.graph = graph;
            self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
            let before = self.selected_nodes.len() + self.selected_edges.len();
            self.selected_nodes
                .retain(|id| self.graph.node(*id).is_some());
            let edge_ids: HashSet<EdgeId> = self.graph.edges().map(|e| e.id).collect();
            self.selected_edges.retain(|id| edge_ids.contains(id));
            if before != self.selected_nodes.len() + self.selected_edges.len() {
                self.notify_properties_selection(cx);
            }
        }
    }

    fn sync_from_project(&mut self, _project: &Entity<ProjectState>, cx: &mut Context<Self>) {
        self.refresh_from_document(cx);
        cx.notify();
    }

    /// Breadcrumb segments: `(label, Some(depth))` for clickable segments
    /// (depth = number of subnet segments to keep), `(label, None)` for the
    /// composition prefix.
    fn breadcrumbs(&self, cx: &App) -> Vec<(String, Option<usize>)> {
        let Some(context) = &self.context else {
            return Vec::new();
        };
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let document = project.read(cx).document();
        let Some(comp) = document.get_composition(context.comp) else {
            return Vec::new();
        };
        let Some(layer) = comp.get_layer(context.layer) else {
            return Vec::new();
        };

        let mut crumbs = vec![(comp.name.clone(), None), (layer.name.clone(), Some(0))];
        let mut graph = &layer.network;
        for (i, subnet) in context.subnets.iter().enumerate() {
            let label = graph
                .node(*subnet)
                .map(|n| {
                    n.metadata
                        .label
                        .clone()
                        .unwrap_or_else(|| n.type_key.clone())
                })
                .unwrap_or_else(|| "?".to_string());
            crumbs.push((label, Some(i + 1)));
            graph = match graph.node(*subnet).and_then(|n| n.subnet.as_deref()) {
                Some(inner) => inner,
                None => break,
            };
        }
        crumbs
    }

    // ----- document commits (REQ-LAYER-009) ----------------------------------

    /// Splice `graph` into the document at the current context and record
    /// one undo step.
    fn commit_graph(&mut self, graph: Graph, cx: &mut Context<Self>) {
        self.commit_to_document(graph, InvalidationHint::Structural, true, cx);
        self.notify_properties_selection(cx);
    }

    fn commit_to_document(
        &mut self,
        graph: Graph,
        hint: InvalidationHint,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        self.graph = graph.clone();
        self.node_sizes = Self::compute_all_sizes(&graph, self.viewport.zoom);
        let (Some(project), Some(context)) = (self.project.clone(), self.context.clone()) else {
            return;
        };
        project.update(cx, |project, cx| {
            let Some(doc) = replace_network(project.document(), &context, graph) else {
                return;
            };
            if commit {
                project.commit_document(doc, hint, cx);
            } else {
                project.apply_document(doc, hint, cx);
            }
        });
    }

    /// The layer-local frame at the playhead for the network being edited,
    /// resolved from the context's owning layer and the shared
    /// [`PlaybackPosition`](super::PlaybackPosition) (REQ-LAYER-006).
    /// `None` without a context or when the owning layer is gone.
    pub fn current_local_frame(&self, cx: &App) -> Option<u64> {
        let context = self.context.as_ref()?;
        let project = self.project.as_ref()?;
        let document = project.read(cx).document();
        let layer = document
            .get_composition(context.comp)?
            .get_layer(context.layer)?;
        let frame = cx
            .try_global::<super::PlaybackPosition>()
            .map(|position| position.frame)
            .unwrap_or_default();
        Some(ravel_ui::keyframes::layer_local_frame(layer, frame))
    }

    /// Toggle a keyframe at the current layer-local frame on the parameter
    /// `param_key` of `node_id` (REQ-LAYER-004): a constant `Float`
    /// parameter converts to a keyframed channel; keyed channels drop their
    /// key at the frame (the last key reverts to a constant). Multi-
    /// component channels key all components together. `Int` / `Bool` /
    /// `String` parameters are constant-only in v1. One Document undo step
    /// per call; a no-op without a network context.
    pub fn toggle_param_keyframe(
        &mut self,
        node_id: NodeId,
        param_key: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(local_frame) = self.current_local_frame(cx) else {
            return;
        };
        let Some(node) = self.graph.node(node_id) else {
            return;
        };
        let Some(param) = node.parameters.iter().find(|p| p.key == param_key) else {
            return;
        };
        let value = match &param.value {
            ParameterValue::Float(v) => {
                let mut channel = AnimationChannel::constant(*v);
                insert_channel_key(&mut channel, local_frame);
                ParameterValue::Channel(channel)
            }
            ParameterValue::Channel(channel) => {
                let mut channel = channel.clone();
                let toggled = if channel_has_key(&channel, local_frame) {
                    remove_channel_key(&mut channel, local_frame)
                } else {
                    insert_channel_key(&mut channel, local_frame)
                };
                if !toggled {
                    return;
                }
                ParameterValue::Channel(channel)
            }
            ParameterValue::Channel2(channels) => {
                let mut channels = channels.clone();
                if !toggle_components_key(&mut channels, local_frame) {
                    return;
                }
                ParameterValue::Channel2(channels)
            }
            ParameterValue::Channel3(channels) => {
                let mut channels = channels.clone();
                if !toggle_components_key(&mut channels, local_frame) {
                    return;
                }
                ParameterValue::Channel3(channels)
            }
            ParameterValue::Channel4(channels) => {
                let mut channels = channels.clone();
                if !toggle_components_key(&mut channels, local_frame) {
                    return;
                }
                ParameterValue::Channel4(channels)
            }
            // Int / Bool / String stay constant-only in v1 (REQ-LAYER-004).
            _ => return,
        };
        let mut updated = (**node).clone();
        updated
            .parameters
            .iter_mut()
            .find(|p| p.key == param_key)
            .expect("parameter checked above")
            .value = value;
        let graph = self.graph.clone().replace_node(Arc::new(updated));
        self.commit_to_document(graph, InvalidationHint::Params(vec![node_id]), true, cx);
        // Refresh the properties snapshot so the key-toggle state re-renders.
        self.notify_properties_selection(cx);
        cx.notify();
    }

    /// Applies a property edit from the Properties panel.
    ///
    /// Numeric values are clamped to the parameter's hard range (registry
    /// metadata). Channel-backed parameters keep their channel: a constant
    /// channel updates its constant, a keyframed channel gets a key at the
    /// current layer-local frame (REQ-LAYER-004). Live edits
    /// (`commit == false`, e.g. mid-scrub) update the document without
    /// recording undo; the gesture-ending `commit == true` event records
    /// one Document undo step for the whole edit.
    fn apply_property_change(&mut self, changed: &super::PropertyChanged, cx: &mut Context<Self>) {
        let local_frame = self.current_local_frame(cx);
        let mut graph = self.graph.clone();
        let mut touched = false;
        for node_id in &changed.node_ids {
            let Some(node) = graph.node(*node_id) else {
                continue;
            };
            let range = self.registry.param_range(&node.type_key, &changed.key);
            let param_value = {
                let Some(param) = node.parameters.iter().find(|p| p.key == changed.key) else {
                    continue;
                };
                let Some(value) =
                    edited_param_value(&param.value, &changed.value, range, local_frame)
                else {
                    continue;
                };
                value
            };
            let mut updated = (**node).clone();
            updated
                .parameters
                .iter_mut()
                .find(|p| p.key == changed.key)
                .expect("parameter checked above")
                .value = param_value;
            touched = true;
            graph = graph.replace_node(Arc::new(updated));
        }
        if !touched {
            return;
        }

        self.commit_to_document(
            graph,
            InvalidationHint::Params(changed.node_ids.clone()),
            changed.commit,
            cx,
        );
        cx.notify();
    }

    // ----- clipboard / editing ------------------------------------------------

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
        if self.context.is_none() {
            return;
        }
        let content = match &self.clipboard {
            Some(c) => c.clone(),
            None => return,
        };

        let mut id_map: HashMap<NodeId, NodeId> = HashMap::new();
        let mut graph = self.graph.clone();

        for node in &content.nodes {
            let new_id = NodeId::next();
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
            if let Ok(g) = graph.clone().add_edge(
                EdgeId::next(),
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
            .filter(|n| !n.metadata.synthetic)
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
                if let Ok(connected) =
                    g.clone()
                        .add_edge(EdgeId::next(), src, src_port, tgt, tgt_port)
                {
                    self.graph = connected;
                } else {
                    self.graph = g;
                }
            }
        } else if let Ok(g) = self.graph.clone().remove_node(node_id) {
            self.graph = g;
        }
    }

    /// Publish the current selection to the Properties panel. The Viewer is
    /// untouched: it always shows the root composition output
    /// (REQ-LAYER-007).
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
            if node.metadata.synthetic {
                continue;
            }
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
        if self.context.is_none() {
            return;
        }
        if let Some(mut node) = self.registry.create_node(type_key, NodeId::next()) {
            let (fx, fy) = self.viewport.screen_to_flow(200.0, 200.0);
            node.metadata.position = (fx, fy);
            if let Ok(new_graph) = self.graph.clone().add_node(node) {
                self.commit_graph(new_graph, cx);
            }
        }
    }

    fn build_breadcrumb_bar(&self, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let crumbs = self.breadcrumbs(cx);

        let mut bar = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .h(px(24.0))
            .flex_shrink_0()
            .bg(colors.tab_bar)
            .border_b_1()
            .border_color(colors.border)
            .text_xs();

        if crumbs.is_empty() {
            return bar.child(
                div()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("node_graph.no_network"))),
            );
        }

        let last = crumbs.len() - 1;
        for (i, (label, depth)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(
                    div()
                        .text_color(colors.muted_foreground)
                        .child(SharedString::from("/")),
                );
            }
            let color = if i == last {
                colors.foreground
            } else {
                colors.muted_foreground
            };
            let mut crumb = div()
                .id(SharedString::from(format!("crumb-{i}")))
                .text_color(color)
                .child(SharedString::from(label));
            if let Some(depth) = depth
                && i != last
            {
                crumb = crumb.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        if let Some(context) = &this.context {
                            this.open_network(context.truncated(depth), cx);
                        }
                    }),
                );
            }
            bar = bar.child(crumb);
        }
        bar
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
        // Per-node evaluation durations for the load readout under each node.
        let timings = cx
            .try_global::<crate::project_state::NodeEvalTimings>()
            .map(|t| t.0.clone())
            .unwrap_or_default();

        let breadcrumb = self.build_breadcrumb_bar(cx);

        let canvas_area = div()
            .id("node-editor-canvas")
            .flex_grow()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (lx, ly) = this.local_from_event(event.position);

                    // Double-click on a subnet node dives into it
                    // (REQ-LAYER-003/011).
                    if event.click_count == 2
                        && let Some(node_id) = this.node_at_local_pos(lx, ly)
                        && this.graph.node(node_id).is_some_and(|n| n.subnet.is_some())
                    {
                        this.drag = DragMode::None;
                        this.enter_subnet(node_id, cx);
                        return;
                    }

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
                            moved: false,
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
                            if let Ok(new_graph) = graph.add_edge(
                                EdgeId::next(),
                                src_node,
                                src_port,
                                tgt_node,
                                tgt_port,
                            ) {
                                this.commit_graph(new_graph, cx);
                            }
                        }
                        DragMode::MoveNodes { moved: true, .. } => {
                            this.commit_graph(this.graph.clone(), cx);
                        }
                        _ => {}
                    }
                    let was_select_box = matches!(this.drag, DragMode::SelectBox { .. });
                    this.drag = DragMode::None;
                    if was_select_box {
                        this.notify_properties_selection(cx);
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
                        ..
                    } => {
                        let origin_mouse = *origin_mouse;
                        let node_origins = node_origins.clone();
                        let dx = (lx - origin_mouse.0) / this.viewport.zoom;
                        let dy = (ly - origin_mouse.1) / this.viewport.zoom;

                        let snap_grid = 10.0;
                        let mut graph = this.graph.clone();
                        let mut moved = false;
                        for &(id, ox, oy) in &node_origins {
                            if let Some(node) = graph.node(id) {
                                let mut updated = node.as_ref().clone();
                                let new_x = ((ox + dx) / snap_grid).round() * snap_grid;
                                let new_y = ((oy + dy) / snap_grid).round() * snap_grid;
                                moved |= updated.metadata.position != (new_x, new_y);
                                updated.metadata.position = (new_x, new_y);
                                graph = graph.replace_node(Arc::new(updated));
                            }
                        }
                        this.graph = graph;
                        if moved {
                            this.drag = DragMode::MoveNodes {
                                origin_mouse,
                                node_origins,
                                moved: true,
                            };
                        }
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
                            if node.metadata.synthetic {
                                continue;
                            }
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
                            if node.metadata.synthetic {
                                continue;
                            }
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
                            &timings,
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
            );

        div()
            .id("node-editor-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_duplicate))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_fit_view))
            .child(breadcrumb)
            .child(canvas_area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use super::*` re-exports gpui's `test` attribute macro (via the
    // panel's `use gpui::*`); shadow it back to the built-in one so
    // `#[gpui::test]`'s generated `#[test]` resolves correctly.
    use core::prelude::v1::test;
    use gpui::TestAppContext;
    use ravel_core::composition::Layer;
    use ravel_core::graph::ParameterValue;
    use ravel_core::id::{DataTypeId, LayerId};
    use ravel_ui::properties::PropertyValue;

    /// Builds a ProjectState (eval disabled) whose root comp has one layer
    /// containing a blur node, registers the global handle, and returns the
    /// panel plus the layer's network path.
    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<NodeEditorPanel>,
        Entity<ProjectState>,
        NetworkPath,
        NodeId,
    ) {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ))
        });

        let blur_id = NodeId::next();
        let (path, comp_id, layer_id) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let blur = registry.create_node("blur", blur_id).expect("blur node");
            let network = Graph::new().add_node(blur).unwrap();
            let layer_id = LayerId::next();
            let layer = Layer::new(layer_id, "Blur Layer", network).with_time(0, 0, 300);
            let doc = ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (NetworkPath::layer(comp_id, layer_id), comp_id, layer_id)
        });
        let _ = (comp_id, layer_id);

        let window = cx.add_window(NodeEditorPanel::new);
        window
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
            })
            .unwrap();
        (window, project, path, blur_id)
    }

    fn blur_radius(
        project: &Entity<ProjectState>,
        path: &NetworkPath,
        node: NodeId,
        cx: &mut TestAppContext,
    ) -> f32 {
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), path).expect("network");
            let node = graph.node(node).expect("blur node");
            match node
                .parameters
                .iter()
                .find(|p| p.key == "radius")
                .map(|p| &p.value)
            {
                Some(ParameterValue::Float(v)) => *v,
                other => panic!("unexpected radius parameter: {other:?}"),
            }
        })
    }

    fn change(node: NodeId, value: f32, commit: bool) -> crate::panels::PropertyChanged {
        crate::panels::PropertyChanged {
            node_ids: vec![node],
            key: "radius".into(),
            value: PropertyValue::Float(value),
            commit,
        }
    }

    /// A scrub gesture (many live changes + one commit) lands in the
    /// document and records exactly one Document-level undo step
    /// (REQ-LAYER-009).
    #[gpui::test]
    fn scrub_gesture_records_a_single_document_undo_step(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        let original = blur_radius(&project, &path, blur, cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.apply_property_change(&change(blur, 10.0, false), cx);
                panel.apply_property_change(&change(blur, 20.0, false), cx);
                panel.apply_property_change(&change(blur, 42.0, true), cx);
            })
            .unwrap();
        assert!((blur_radius(&project, &path, blur, cx) - 42.0).abs() < f32::EPSILON);

        // One Document undo returns to the pre-gesture value.
        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!((blur_radius(&project, &path, blur, cx) - original).abs() < f32::EPSILON);
    }

    #[gpui::test]
    fn property_change_clamps_to_hard_range(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                // blur.radius hard range is 0..=500.
                panel.apply_property_change(&change(blur, 9999.0, true), cx);
            })
            .unwrap();
        assert!((blur_radius(&project, &path, blur, cx) - 500.0).abs() < f32::EPSILON);
    }

    /// The key toggle converts a constant Float parameter into a keyframed
    /// channel holding the current value (REQ-LAYER-004); one Document
    /// undo restores the constant.
    #[gpui::test]
    fn toggle_param_keyframe_keys_a_float_param_and_undoes(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        let original = blur_radius(&project, &path, blur, cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(blur, "radius", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            let node = graph.node(blur).expect("blur node");
            let param = node
                .parameters
                .iter()
                .find(|p| p.key == "radius")
                .expect("radius parameter");
            let ParameterValue::Channel(channel) = &param.value else {
                panic!("radius converted to a channel: {:?}", param.value);
            };
            let ChannelSource::Keyframes(curve) = &channel.source else {
                panic!("keyed at the current frame: {:?}", channel.source);
            };
            assert_eq!(curve.len(), 1);
            assert!((curve.sample(0) - original).abs() < f32::EPSILON);
        });

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!((blur_radius(&project, &path, blur, cx) - original).abs() < f32::EPSILON);
    }

    /// Scrubbing a keyframed channel inserts/updates a key at the current
    /// frame instead of flattening the channel to a constant
    /// (REQ-LAYER-004).
    #[gpui::test]
    fn property_change_keys_an_animated_channel_instead_of_flattening(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(blur, "radius", cx);
            })
            .unwrap();
        window
            .update(cx, |panel, _window, cx| {
                panel.apply_property_change(&change(blur, 10.0, false), cx);
                panel.apply_property_change(&change(blur, 42.0, true), cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            let node = graph.node(blur).expect("blur node");
            let param = node
                .parameters
                .iter()
                .find(|p| p.key == "radius")
                .expect("radius parameter");
            let ParameterValue::Channel(channel) = &param.value else {
                panic!("radius stays a channel: {:?}", param.value);
            };
            let ChannelSource::Keyframes(curve) = &channel.source else {
                panic!("radius stays keyframed: {:?}", channel.source);
            };
            assert_eq!(curve.len(), 1, "live changes overwrite the same key");
            assert!((curve.sample(0) - 42.0).abs() < f32::EPSILON);
        });
    }

    /// Structural edits (delete) go through the document, and undoing the
    /// document restores the editor's display graph via the observer.
    #[gpui::test]
    fn delete_and_document_undo_roundtrip(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.selected_nodes.insert(blur);
                panel.delete_selected(cx);
                assert!(panel.graph.node(blur).is_none());
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(graph.node(blur).is_none());
        });

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.graph.node(blur).is_some(),
                    "observer resyncs after undo"
                );
            })
            .unwrap();
    }

    /// Deleting the opened layer pops the editor back to no context instead
    /// of leaving a dangling path.
    #[gpui::test]
    fn context_pops_when_the_layer_disappears(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);

        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::remove_layer(project.document(), path.comp, path.layer)
                .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.context().is_none());
                assert_eq!(panel.graph.node_count(), 0);
            })
            .unwrap();
    }

    /// A synthetic node inside the displayed graph is not hit-testable
    /// (REQ-LAYER-011; painting skips are covered in `painting::tests`).
    #[gpui::test]
    fn synthetic_nodes_are_not_selectable(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);

        let synthetic_id = NodeId::next();
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path).unwrap().clone();
            let mut node = Node::new(synthetic_id, "comp.opacity")
                .with_output("output", DataTypeId::FRAME_BUFFER);
            node.metadata.position = (500.0, 500.0);
            node.metadata.synthetic = true;
            let graph = graph.add_node(node).unwrap();
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, _cx| {
                panel.viewport = Viewport {
                    x: 0.0,
                    y: 0.0,
                    zoom: 1.0,
                };
                let (sx, sy) = panel.viewport.flow_to_screen(500.0, 500.0);
                assert_eq!(panel.node_at_local_pos(sx + 10.0, sy + 10.0), None);
            })
            .unwrap();
    }
}

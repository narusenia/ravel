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
use ravel_core::registry::{NodeCategory, NodeRegistry, ParamRange};
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

const CUSTOM_PATH_TYPE_KEY: &str = "shape.custom_path";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AddNodeMenuItem {
    label: String,
    type_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AddNodeMenuGroup {
    category: NodeCategory,
    items: Vec<AddNodeMenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExposeParamMenuItem {
    key: String,
    checked: bool,
}

/// Menu state of the Bypass context-menu item: enabled when at least one
/// target can be bypassed (every output port has a type-matching input, see
/// [`Node::is_bypassable`]); checked when every bypassable target is
/// currently bypassed. Clicking applies `!checked` to all bypassable
/// targets. Network boundary nodes (`net.in` / `net.out`, REQ-LAYER-002) are
/// excluded before the state is computed, so a boundary-only selection
/// disables the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BypassMenuItem {
    enabled: bool,
    checked: bool,
}

fn bypass_menu_model(graph: &Graph, targets: &[NodeId]) -> BypassMenuItem {
    let bypassable: Vec<_> = NodeEditorPanel::editable_targets(graph, targets.iter().copied())
        .into_iter()
        .filter_map(|id| graph.node(id))
        .filter(|node| node.is_bypassable())
        .collect();
    BypassMenuItem {
        enabled: !bypassable.is_empty(),
        checked: !bypassable.is_empty() && bypassable.iter().all(|node| node.metadata.bypassed),
    }
}

fn node_category_order(category: NodeCategory) -> u8 {
    match category {
        NodeCategory::Generator => 0,
        NodeCategory::Compositor => 1,
        NodeCategory::Filter => 2,
        NodeCategory::Transform => 3,
        NodeCategory::Color => 4,
        NodeCategory::Time => 5,
        NodeCategory::Utility => 6,
    }
}

fn node_category_label(category: NodeCategory) -> String {
    match category {
        NodeCategory::Generator => t!("panel.node_graph_menu.category.generator"),
        NodeCategory::Compositor => t!("panel.node_graph_menu.category.compositor"),
        NodeCategory::Filter => t!("panel.node_graph_menu.category.filter"),
        NodeCategory::Transform => t!("panel.node_graph_menu.category.transform"),
        NodeCategory::Color => t!("panel.node_graph_menu.category.color"),
        NodeCategory::Time => t!("panel.node_graph_menu.category.time"),
        NodeCategory::Utility => t!("panel.node_graph_menu.category.utility"),
    }
}

fn add_node_menu_model(registry: &NodeRegistry) -> Vec<AddNodeMenuGroup> {
    let mut categories = registry.categories();
    categories.sort_by_key(|category| node_category_order(*category));

    categories
        .into_iter()
        .filter_map(|category| {
            let mut items: Vec<_> = registry
                .list_by_category(category)
                .into_iter()
                // This placeholder remains hidden until ParameterValue::PathPoints
                // is implemented by the pen-tool plan.
                .filter(|template| template.type_key != CUSTOM_PATH_TYPE_KEY)
                .map(|template| AddNodeMenuItem {
                    label: template.label.clone(),
                    type_key: template.type_key.clone(),
                })
                .collect();
            items.sort_by(|left, right| {
                left.label
                    .cmp(&right.label)
                    .then_with(|| left.type_key.cmp(&right.type_key))
            });

            (!items.is_empty()).then_some(AddNodeMenuGroup { category, items })
        })
        .collect()
}

/// Parameters of `node` currently driven by a connected parameter port,
/// with a display value when the source is statically known (constant /
/// constant.color). Live evaluated values for arbitrary sources are a
/// planned follow-up (param-input-ports-plan Phase 4). Shared with the
/// Properties panel, which re-derives driven state from the document.
pub(crate) fn driven_params(graph: &Graph, node: &Node) -> Vec<ravel_ui::properties::DrivenParam> {
    let mut driven = Vec::new();
    for (index, port) in node.inputs.iter().enumerate() {
        if !port.is_param {
            continue;
        }
        let Some(edge) = graph
            .edges()
            .find(|e| e.target == node.id && e.target_port.0 as usize == index)
        else {
            continue;
        };
        let Some(source) = graph.node(edge.source) else {
            continue;
        };
        let label = source
            .metadata
            .label
            .clone()
            .unwrap_or_else(|| source.type_key.clone());
        let value = match source.type_key.as_str() {
            "constant" => source
                .parameters
                .iter()
                .find(|p| p.key == "value")
                .and_then(|p| p.value.as_float())
                .map(|v| format!("{v:.3}")),
            "constant.color" => source
                .parameters
                .iter()
                .find(|p| p.key == "color")
                .and_then(|p| match &p.value {
                    ParameterValue::Channel4(chs) => {
                        let component = |ch: &AnimationChannel| match &ch.source {
                            ChannelSource::Constant(v) => Some(*v),
                            _ => None,
                        };
                        Some(format!(
                            "({:.2}, {:.2}, {:.2}, {:.2})",
                            component(&chs[0])?,
                            component(&chs[1])?,
                            component(&chs[2])?,
                            component(&chs[3])?,
                        ))
                    }
                    _ => None,
                }),
            _ => None,
        };
        driven.push(ravel_ui::properties::DrivenParam {
            key: port.name.clone(),
            source: label,
            value,
        });
    }
    driven
}

fn expose_param_menu_model(node: &Node) -> Vec<ExposeParamMenuItem> {
    if !node.supports_param_ports() {
        return Vec::new();
    }

    node.parameters
        .iter()
        .filter(|param| param.value.port_data_type().is_some())
        .map(|param| ExposeParamMenuItem {
            key: param.key.clone(),
            checked: node.param_port_index(&param.key).is_some(),
        })
        .collect()
}

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
/// as the curve default. An existing key keeps its interpolation mode and
/// tangents. Returns `false` for non-key-editable sources.
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
            ravel_ui::keyframes::set_curve_value(curve, frame, value);
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
/// current value into the components that lack one (existing keys keep
/// their interpolation and tangents). Returns `false` when nothing changed.
fn toggle_components_key(channels: &mut [AnimationChannel], frame: u64) -> bool {
    let all_keyed = channels.iter().all(|ch| channel_has_key(ch, frame));
    let mut changed = false;
    for channel in channels {
        changed |= if all_keyed {
            remove_channel_key(channel, frame)
        } else if channel_has_key(channel, frame) {
            false
        } else {
            insert_channel_key(channel, frame)
        };
    }
    changed
}

/// One component of a channel-backed edit (REQ-LAYER-004): a constant
/// channel updates its constant, a keyframed channel gets a key at
/// `local_frame` (or flattens to a constant without one); expression /
/// node-output sources are not editable and keep the existing channel.
fn edited_channel(
    channel: &AnimationChannel,
    v: f32,
    local_frame: Option<u64>,
) -> AnimationChannel {
    match &channel.source {
        ChannelSource::Constant(_) => AnimationChannel::constant(v),
        ChannelSource::Keyframes(curve) => match local_frame {
            Some(frame) => {
                let mut curve = curve.clone();
                ravel_ui::keyframes::set_curve_value(&mut curve, frame, v);
                AnimationChannel::keyframes(curve)
            }
            None => AnimationChannel::constant(v),
        },
        _ => channel.clone(),
    }
}

/// The new parameter value for a Properties-panel numeric edit, keeping
/// animated channels animated (REQ-LAYER-004): a constant channel updates
/// its constant, a keyframed channel gets a key at `local_frame` (live
/// `Change`s overwrite the same key, so one scrub gesture still records one
/// undo step). Without a local frame the value falls back to a plain
/// constant — the legacy flattening behavior. Vector and color edits write
/// every component of their `Channel2`/`Channel4` parameter. Returns `None`
/// when the edit does not apply to the parameter.
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
                            ravel_ui::keyframes::set_curve_value(&mut curve, frame, v);
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
        PropertyValue::Vector(components) => {
            let clamped: Vec<f32> = components
                .iter()
                .map(|v| range.map_or(*v, |r| r.clamp(*v)))
                .collect();
            match (existing, clamped.as_slice()) {
                (ParameterValue::Channel2(chs), [x, y]) => Some(ParameterValue::Channel2([
                    edited_channel(&chs[0], *x, local_frame),
                    edited_channel(&chs[1], *y, local_frame),
                ])),
                (ParameterValue::Channel3(chs), [x, y, z]) => Some(ParameterValue::Channel3([
                    edited_channel(&chs[0], *x, local_frame),
                    edited_channel(&chs[1], *y, local_frame),
                    edited_channel(&chs[2], *z, local_frame),
                ])),
                _ => None,
            }
        }
        PropertyValue::Color { r, g, b, a } => match existing {
            ParameterValue::Channel4(chs) => Some(ParameterValue::Channel4([
                edited_channel(&chs[0], *r, local_frame),
                edited_channel(&chs[1], *g, local_frame),
                edited_channel(&chs[2], *b, local_frame),
                edited_channel(&chs[3], *a, local_frame),
            ])),
            _ => None,
        },
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
            if before > 0 {
                // Any graph change can alter the selected nodes' values,
                // exposure, or driven state (undo/redo, external edits):
                // republish so the Properties panel never shows stale
                // driven info. Same-identity targets refresh in place, so
                // this cannot steal an unrelated selection.
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

    /// Toggle one parameter input port as one structural Document undo step.
    /// Removing a port also removes its connected edges atomically in
    /// [`Graph::remove_param_port`].
    pub fn toggle_param_port(&mut self, node_id: NodeId, key: &str, cx: &mut Context<Self>) {
        let Some(node) = self.graph.node(node_id) else {
            return;
        };
        let result = if node.param_port_index(key).is_some() {
            self.graph.clone().remove_param_port(node_id, key)
        } else {
            self.graph.clone().expose_param_port(node_id, key)
        };
        if let Ok(graph) = result {
            self.commit_graph(graph, cx);
        }
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

    /// `targets` minus the network boundary nodes. `net.in` / `net.out` are
    /// the fixed interface of a layer network (REQ-LAYER-002) — exactly one
    /// of each must exist — so copy / duplicate / delete / bypass never
    /// target them.
    fn editable_targets(graph: &Graph, targets: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
        targets
            .into_iter()
            .filter(|id| {
                graph.node(*id).is_some_and(|node| {
                    !ravel_core::network::is_in_node(node)
                        && !ravel_core::network::is_out_node(node)
                })
            })
            .collect()
    }

    fn copy_selected(&mut self) {
        let ids = Self::editable_targets(&self.graph, self.selected_nodes.iter().copied());
        if ids.is_empty() {
            return;
        }
        let nodes: Vec<Node> = ids
            .iter()
            .filter_map(|id| self.graph.node(*id).map(|n| (**n).clone()))
            .collect();
        let node_ids: HashSet<NodeId> = ids.into_iter().collect();
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

        for (z, node) in (Self::next_z(&graph)..).zip(content.nodes.iter()) {
            let new_id = NodeId::next();
            id_map.insert(node.id, new_id);
            let mut new_node = node.clone();
            new_node.id = new_id;
            new_node.metadata.position.0 += offset.0;
            new_node.metadata.position.1 += offset.1;
            new_node.metadata.z = z;
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
        // Boundary-only selections must not fall through to `paste` (it
        // would duplicate the stale clipboard instead of nothing).
        if Self::editable_targets(&self.graph, self.selected_nodes.iter().copied()).is_empty() {
            return;
        }
        self.copy_selected();
        self.paste((20.0, 20.0), cx);
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let nodes = Self::editable_targets(&self.graph, self.selected_nodes.iter().copied());
        if nodes.is_empty() && self.selected_edges.is_empty() {
            return;
        }

        let edges: Vec<_> = self.selected_edges.iter().copied().collect();
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

    /// Set the bypass flag of every bypassable target node to `bypass` as
    /// one structural Document undo step. Bypassed nodes keep their wiring
    /// and pass a type-matching input through to each output unchanged (see
    /// the bypass notes in `ravel_core::eval`); non-bypassable nodes (pure
    /// generators, partially matched multi-output nodes, see
    /// [`Node::is_bypassable`]) are left untouched. Network boundary nodes
    /// (`net.in` / `net.out`) are filtered out up front — they are the
    /// network's fixed interface (REQ-LAYER-002) and can never be bypassed.
    /// A call that changes nothing records no undo step.
    fn set_bypass(&mut self, targets: &[NodeId], bypass: bool, cx: &mut Context<Self>) {
        let mut changed = false;
        let graph = Self::editable_targets(&self.graph, targets.iter().copied())
            .into_iter()
            .fold(self.graph.clone(), |graph, id| {
                let Some(node) = graph.node(id) else {
                    return graph;
                };
                if !node.is_bypassable() || node.metadata.bypassed == bypass {
                    return graph;
                }
                let mut updated = (**node).clone();
                updated.metadata.bypassed = bypass;
                changed = true;
                graph.replace_node(Arc::new(updated))
            });
        if changed {
            self.commit_graph(graph, cx);
        }
    }

    /// Publish the current selection to the Properties panel. The target
    /// only identifies the network and node ids; the panel resolves live
    /// values (and driven state) from the document. The Viewer is
    /// untouched: it always shows the root composition output
    /// (REQ-LAYER-007).
    fn notify_properties_selection(&mut self, cx: &mut Context<Self>) {
        let target = match &self.context {
            Some(network) if !self.selected_nodes.is_empty() => {
                let ids: Vec<_> = self.selected_nodes.iter().copied().collect();
                super::PropertiesTarget::Nodes {
                    network: network.clone(),
                    ids,
                }
            }
            _ => super::PropertiesTarget::Empty,
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
        Self::node_hit_at(&self.graph, &self.viewport, &self.node_sizes, lx, ly)
    }

    /// The topmost (highest `z`) non-synthetic node whose body contains the
    /// local point — the same walk order the canvas paints, keeping the
    /// last hit.
    fn node_hit_at(
        graph: &Graph,
        viewport: &Viewport,
        node_sizes: &HashMap<NodeId, (f32, f32)>,
        lx: f32,
        ly: f32,
    ) -> Option<NodeId> {
        let mut hit = None;
        for node in painting::z_ordered(graph) {
            let (sx, sy) =
                viewport.flow_to_screen(node.metadata.position.0, node.metadata.position.1);
            let (w, h) = node_sizes
                .get(&node.id)
                .copied()
                .unwrap_or((node_width(viewport.zoom), 60.0));
            if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
                hit = Some(node.id);
            }
        }
        hit
    }

    /// The z value that places a new node above everything currently in
    /// the graph.
    fn next_z(graph: &Graph) -> u64 {
        graph
            .nodes()
            .filter(|n| !n.metadata.synthetic)
            .map(|n| n.metadata.z)
            .max()
            .map_or(0, |z| z + 1)
    }

    /// Reassign `ids` the top z slots — above every other node — keeping
    /// their relative stacking order. Returns the graph unchanged when the
    /// targets already occupy the top of the stack, so re-grabbing the
    /// frontmost node does not churn the document.
    fn raised_to_front(graph: &Graph, ids: &HashSet<NodeId>) -> Graph {
        let max_other = graph
            .nodes()
            .filter(|n| !n.metadata.synthetic && !ids.contains(&n.id))
            .map(|n| n.metadata.z)
            .max();
        let Some(max_other) = max_other else {
            // Nothing else in the graph to raise above.
            return graph.clone();
        };
        let mut targets: Vec<(NodeId, u64)> = graph
            .nodes()
            .filter(|n| !n.metadata.synthetic && ids.contains(&n.id))
            .map(|n| (n.id, n.metadata.z))
            .collect();
        targets.sort_by_key(|(_, z)| *z);
        if targets.first().is_none_or(|(_, z)| *z > max_other) {
            return graph.clone();
        }
        let mut result = graph.clone();
        for (i, (id, _)) in targets.into_iter().enumerate() {
            let Some(node) = result.node(id) else {
                continue;
            };
            let mut updated = (**node).clone();
            updated.metadata.z = max_other + 1 + i as u64;
            result = result.replace_node(Arc::new(updated));
        }
        result
    }

    fn local_from_event(&self, pos: Point<Pixels>) -> (f32, f32) {
        let origin = self.canvas_origin.get();
        let mx: f32 = pos.x.into();
        let my: f32 = pos.y.into();
        (mx - origin.0, my - origin.1)
    }

    /// Add a node from the registry template, placed at `local` —
    /// a canvas-relative screen position (the right-click point of the
    /// add-node menu) converted to flow coordinates.
    fn add_node_from_template(
        &mut self,
        type_key: &str,
        local: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if self.context.is_none() {
            return;
        }
        if let Some(mut node) = self.registry.create_node(type_key, NodeId::next()) {
            let (fx, fy) = self.viewport.screen_to_flow(local.0, local.1);
            node.metadata.position = (fx, fy);
            node.metadata.z = Self::next_z(&self.graph);
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
        let add_node_menu = add_node_menu_model(&self.registry);
        // Per-node evaluation durations for the load readout under each node.
        let timings = cx
            .try_global::<crate::project_state::NodeEvalTimings>()
            .map(|t| t.0.clone())
            .unwrap_or_default();
        // Template category per node for the header accent bar; nodes
        // without a registered template (or synthetic ones) paint none.
        let categories: HashMap<NodeId, NodeCategory> = self
            .graph
            .nodes()
            .filter_map(|n| {
                self.registry
                    .get(&n.type_key)
                    .map(|template| (n.id, template.category))
            })
            .collect();

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

                        // Grabbing raises the selection to the front
                        // immediately (paint order); the new z values are
                        // committed with the move gesture — a plain click
                        // records no undo step.
                        this.graph = Self::raised_to_front(&this.graph, &this.selected_nodes);

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
                let add_node_menu = add_node_menu.clone();
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
                    let hit_node =
                        NodeEditorPanel::node_hit_at(&graph_snap, &vp_snap, &sizes_snap, lx, ly);

                    let entity_add = entity.clone();
                    let groups = add_node_menu.clone();
                    let mut menu = menu.submenu(
                        t!("panel.node_graph_menu.add_node"),
                        window,
                        cx,
                        move |sub, window, cx| {
                            groups.iter().fold(sub, |sub, group| {
                                let items = group.items.clone();
                                let entity_add = entity_add.clone();
                                sub.submenu(
                                    node_category_label(group.category),
                                    window,
                                    cx,
                                    move |sub, _window, _cx| {
                                        items.iter().fold(sub, |sub, item| {
                                            let entity = entity_add.clone();
                                            let type_key = item.type_key.clone();
                                            sub.item(
                                                PopupMenuItem::new(SharedString::from(
                                                    item.label.clone(),
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    entity
                                                        .update(cx, |this, cx| {
                                                            this.add_node_from_template(
                                                                &type_key,
                                                                (lx, ly),
                                                                cx,
                                                            );
                                                            cx.notify();
                                                        })
                                                        .ok();
                                                }),
                                            )
                                        })
                                    },
                                )
                            })
                        },
                    );

                    if let Some(node_id) = hit_node
                        && let Some(node) = graph_snap.node(node_id)
                    {
                        let params = expose_param_menu_model(node);
                        if !params.is_empty() {
                            let entity_expose = entity.clone();
                            menu = menu.separator().submenu(
                                t!("panel.node_graph_menu.expose_parameter"),
                                window,
                                cx,
                                move |sub, _window, _cx| {
                                    params.iter().fold(sub, |sub, param| {
                                        let entity = entity_expose.clone();
                                        let key = param.key.clone();
                                        sub.item(
                                            PopupMenuItem::new(SharedString::from(
                                                param.key.clone(),
                                            ))
                                            .checked(param.checked)
                                            .on_click(move |_, _window, cx| {
                                                entity
                                                    .update(cx, |this, cx| {
                                                        this.toggle_param_port(node_id, &key, cx);
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }),
                                        )
                                    })
                                },
                            );
                        }
                    }

                    if hit_node.is_some() || !selected_snap.is_empty() {
                        // Boundary nodes (net.in / net.out) are excluded from
                        // deletion and bypass (REQ-LAYER-002).
                        let targets = NodeEditorPanel::editable_targets(
                            &graph_snap,
                            if selected_snap.is_empty() {
                                hit_node.into_iter().collect::<Vec<_>>()
                            } else {
                                selected_snap.iter().copied().collect()
                            },
                        );

                        let entity_del = entity.clone();
                        let del_targets = targets.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.delete_node"))
                                .disabled(del_targets.is_empty())
                                .on_click(move |_, _window, cx| {
                                    entity_del
                                        .update(cx, |this, cx| {
                                            let graph = del_targets
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
                                }),
                        );

                        let entity_bypass = entity.clone();
                        let bypass_targets = targets;
                        let bypass_model = bypass_menu_model(&graph_snap, &bypass_targets);
                        menu = menu.item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.bypass_node"))
                                .checked(bypass_model.checked)
                                .disabled(!bypass_model.enabled)
                                .on_click(move |_, _window, cx| {
                                    entity_bypass
                                        .update(cx, |this, cx| {
                                            this.set_bypass(
                                                &bypass_targets,
                                                !bypass_model.checked,
                                                cx,
                                            );
                                            cx.notify();
                                        })
                                        .ok();
                                }),
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
                            &categories,
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
    use ravel_core::registry::NodeTemplate;
    use ravel_ui::properties::PropertyValue;

    #[test]
    fn vector_edits_write_every_channel_component() {
        let existing = ParameterValue::Channel2([
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
        ]);
        let value = PropertyValue::Vector(vec![4.0, -2.0]);
        let Some(ParameterValue::Channel2(chs)) = edited_param_value(&existing, &value, None, None)
        else {
            panic!("expected Channel2");
        };
        assert!(matches!(chs[0].source, ChannelSource::Constant(v) if v == 4.0));
        assert!(matches!(chs[1].source, ChannelSource::Constant(v) if v == -2.0));

        // Component-count mismatches do not apply.
        let wrong = PropertyValue::Vector(vec![1.0, 2.0, 3.0]);
        assert!(edited_param_value(&existing, &wrong, None, None).is_none());
    }

    #[test]
    fn color_edits_keep_keyframed_components_animated() {
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let existing = ParameterValue::Channel4([
            AnimationChannel::keyframes(curve),
            AnimationChannel::constant(0.5),
            AnimationChannel::constant(0.5),
            AnimationChannel::constant(1.0),
        ]);
        let value = PropertyValue::Color {
            r: 0.25,
            g: 0.75,
            b: 0.75,
            a: 1.0,
        };
        let Some(ParameterValue::Channel4(chs)) =
            edited_param_value(&existing, &value, None, Some(5))
        else {
            panic!("expected Channel4");
        };
        // The keyframed component gets a key at the local frame.
        let ChannelSource::Keyframes(curve) = &chs[0].source else {
            panic!("component stays keyframed");
        };
        assert_eq!(curve.keyframes().len(), 3);
        // Constant components update their constants.
        assert!(matches!(chs[1].source, ChannelSource::Constant(v) if v == 0.75));
    }

    #[test]
    fn add_node_menu_model_groups_and_sorts_templates() {
        let mut registry = NodeRegistry::new();
        registry.register(NodeTemplate::new(
            "filter.zulu",
            "Zulu",
            NodeCategory::Filter,
        ));
        registry.register(NodeTemplate::new(
            CUSTOM_PATH_TYPE_KEY,
            "Custom Path",
            NodeCategory::Generator,
        ));
        registry.register(NodeTemplate::new(
            "generator.beta",
            "Beta",
            NodeCategory::Generator,
        ));
        registry.register(NodeTemplate::new(
            "filter.alpha",
            "Alpha",
            NodeCategory::Filter,
        ));

        assert_eq!(
            add_node_menu_model(&registry),
            vec![
                AddNodeMenuGroup {
                    category: NodeCategory::Generator,
                    items: vec![AddNodeMenuItem {
                        label: "Beta".into(),
                        type_key: "generator.beta".into(),
                    }],
                },
                AddNodeMenuGroup {
                    category: NodeCategory::Filter,
                    items: vec![
                        AddNodeMenuItem {
                            label: "Alpha".into(),
                            type_key: "filter.alpha".into(),
                        },
                        AddNodeMenuItem {
                            label: "Zulu".into(),
                            type_key: "filter.zulu".into(),
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn expose_param_menu_model_lists_supported_params_and_checked_state() {
        let node_id = NodeId::new(41);
        let node = Node::new(node_id, "test")
            .with_param("radius", ParameterValue::Float(12.0))
            .with_param("label", ParameterValue::String("hello".into()))
            .with_param(
                "position_3d",
                ParameterValue::Channel3([
                    AnimationChannel::constant(0.0),
                    AnimationChannel::constant(0.0),
                    AnimationChannel::constant(0.0),
                ]),
            )
            .with_param("enabled", ParameterValue::Bool(true));
        let graph = Graph::new()
            .add_node(node)
            .unwrap()
            .expose_param_port(node_id, "enabled")
            .unwrap();

        assert_eq!(
            expose_param_menu_model(graph.node(node_id).unwrap()),
            vec![
                ExposeParamMenuItem {
                    key: "radius".into(),
                    checked: false,
                },
                ExposeParamMenuItem {
                    key: "enabled".into(),
                    checked: true,
                },
            ]
        );

        let interface = Node::new(NodeId::new(42), ravel_core::network::NET_IN_TYPE_KEY)
            .with_param("value", ParameterValue::Float(0.0));
        assert!(expose_param_menu_model(&interface).is_empty());
    }

    #[test]
    fn bypass_menu_model_reflects_bypassability_and_flag_state() {
        let filter = Node::new(NodeId::new(1), "test")
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let generator =
            Node::new(NodeId::new(2), "constant").with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new()
            .add_node(filter)
            .unwrap()
            .add_node(generator)
            .unwrap();

        // A lone generator cannot be bypassed: the item is disabled.
        assert_eq!(
            bypass_menu_model(&graph, &[NodeId::new(2)]),
            BypassMenuItem {
                enabled: false,
                checked: false,
            }
        );
        // A bypassable node starts enabled and unchecked.
        assert_eq!(
            bypass_menu_model(&graph, &[NodeId::new(1)]),
            BypassMenuItem {
                enabled: true,
                checked: false,
            }
        );

        // Once every bypassable target is bypassed the item is checked;
        // non-bypassable targets in the selection do not affect the state.
        let mut bypassed = (**graph.node(NodeId::new(1)).unwrap()).clone();
        bypassed.metadata.bypassed = true;
        let graph = graph.replace_node(Arc::new(bypassed));
        assert_eq!(
            bypass_menu_model(&graph, &[NodeId::new(1), NodeId::new(2)]),
            BypassMenuItem {
                enabled: true,
                checked: true,
            }
        );
    }

    /// Boundary nodes never count as bypass targets (REQ-LAYER-002): a
    /// boundary-only selection disables the item even when the boundary
    /// nodes are bypassable, and a mixed selection ignores them.
    #[test]
    fn bypass_menu_model_excludes_boundary_nodes() {
        // Both boundary nodes are shaped bypassable (a type-matching input
        // for the output), so only the boundary exclusion can disable the
        // item.
        let in_node = Node::new(NodeId::new(1), ravel_core::network::NET_IN_TYPE_KEY)
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let out_node = Node::new(NodeId::new(2), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let filter = Node::new(NodeId::new(3), "test")
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let graph = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_node(filter)
            .unwrap();

        assert_eq!(
            bypass_menu_model(&graph, &[NodeId::new(1), NodeId::new(2)]),
            BypassMenuItem {
                enabled: false,
                checked: false,
            }
        );
        assert_eq!(
            bypass_menu_model(&graph, &[NodeId::new(1), NodeId::new(3)]),
            BypassMenuItem {
                enabled: true,
                checked: false,
            }
        );
    }

    #[test]
    fn driven_params_report_connected_ports_with_static_values() {
        let source = Node::new(NodeId::new(1), "constant")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(12.0));
        let noise = Node::new(NodeId::new(3), "field.noise").with_output("out", DataTypeId::SCALAR);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("radius", ParameterValue::Float(0.0))
            .with_param("amount", ParameterValue::Float(0.0))
            .with_param("spare", ParameterValue::Float(0.0));
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(noise)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "radius")
            .unwrap()
            .expose_param_port(NodeId::new(2), "amount")
            .unwrap()
            .expose_param_port(NodeId::new(2), "spare")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap();

        let driven = driven_params(&graph, graph.node(NodeId::new(2)).unwrap());
        assert_eq!(driven.len(), 2, "unconnected exposed port not reported");
        assert_eq!(driven[0].key, "radius");
        assert_eq!(driven[0].source, "constant");
        assert_eq!(driven[0].value.as_deref(), Some("12.000"));
        assert_eq!(driven[1].key, "amount");
        assert_eq!(driven[1].source, "field.noise");
        assert_eq!(driven[1].value, None, "non-constant sources show connected");
    }

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

    fn positioned_node(id: u64, z: u64) -> Node {
        let mut node = Node::new(NodeId::new(id), "test").with_position(0.0, 0.0);
        node.metadata.z = z;
        node
    }

    /// Raising assigns the targets the top z slots, above every other
    /// node, keeping their relative stacking order.
    #[test]
    fn raised_to_front_assigns_top_slots_preserving_relative_order() {
        let graph = Graph::new()
            .add_node(positioned_node(1, 5))
            .unwrap()
            .add_node(positioned_node(2, 1))
            .unwrap()
            .add_node(positioned_node(3, 3))
            .unwrap();
        let ids: HashSet<NodeId> = [NodeId::new(2), NodeId::new(3)].into();

        let raised = NodeEditorPanel::raised_to_front(&graph, &ids);
        let z = |id: u64| raised.node(NodeId::new(id)).unwrap().metadata.z;
        assert_eq!(z(1), 5, "non-target keeps its z");
        assert_eq!(z(2), 6, "lower target raised first");
        assert_eq!(z(3), 7, "higher target stays above the lower one");
    }

    /// Re-grabbing nodes that are already frontmost must not churn the
    /// graph (no spurious document commit on every drag).
    #[test]
    fn raised_to_front_keeps_graph_unchanged_when_already_front() {
        let graph = Graph::new()
            .add_node(positioned_node(1, 0))
            .unwrap()
            .add_node(positioned_node(2, 7))
            .unwrap();
        let ids: HashSet<NodeId> = [NodeId::new(2)].into();

        let raised = NodeEditorPanel::raised_to_front(&graph, &ids);
        assert_eq!(raised, graph);
    }

    /// Overlapping nodes hit-test in paint order: the higher-z node wins
    /// even when it iterates earlier in the graph.
    #[test]
    fn node_hit_prefers_higher_z_node() {
        let graph = Graph::new()
            .add_node(positioned_node(1, 9))
            .unwrap()
            .add_node(positioned_node(2, 2))
            .unwrap();
        let viewport = Viewport {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        };
        let sizes: HashMap<NodeId, (f32, f32)> = [
            (NodeId::new(1), (160.0, 60.0)),
            (NodeId::new(2), (160.0, 60.0)),
        ]
        .into();

        assert_eq!(
            NodeEditorPanel::node_hit_at(&graph, &viewport, &sizes, 10.0, 10.0),
            Some(NodeId::new(1))
        );
    }

    /// New nodes always land on top of the existing stack.
    #[test]
    fn next_z_places_new_nodes_above_everything() {
        let graph = Graph::new()
            .add_node(positioned_node(1, 4))
            .unwrap()
            .add_node(positioned_node(2, 11))
            .unwrap();
        assert_eq!(NodeEditorPanel::next_z(&graph), 12);
        assert_eq!(NodeEditorPanel::next_z(&Graph::new()), 0);
    }

    /// The add-node menu drops the new node at the clicked canvas position
    /// converted to flow coordinates, not at a fixed offset.
    #[gpui::test]
    fn add_node_from_template_places_node_at_click_position(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.viewport = Viewport {
                    x: 50.0,
                    y: 30.0,
                    zoom: 2.0,
                };
                panel.add_node_from_template("blur", (250.0, 130.0), cx);
            })
            .unwrap();

        // screen_to_flow(250, 130) with x=50, y=30, zoom=2 → (100, 50).
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert!(
                graph.nodes().any(|n| n.metadata.position == (100.0, 50.0)),
                "node placed at the flow position of the click"
            );
        });
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

    /// Expose and unexpose each commit exactly one structural Document
    /// snapshot. Undoing unexpose restores both the port and its edge.
    #[gpui::test]
    fn toggle_param_port_roundtrips_through_document_undo(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_port(blur, "radius", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(
                graph
                    .node(blur)
                    .unwrap()
                    .param_port_index("radius")
                    .is_some()
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(
                graph
                    .node(blur)
                    .unwrap()
                    .param_port_index("radius")
                    .is_none()
            );
        });
        project.update(cx, |project, cx| assert!(project.redo(cx)));

        let source_id = NodeId::next();
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path).unwrap().clone();
            let target_port = graph
                .node(blur)
                .unwrap()
                .param_port_index("radius")
                .unwrap();
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let source = registry
                .create_node("constant", source_id)
                .expect("constant node");
            let graph = graph
                .add_node(source)
                .unwrap()
                .add_edge(
                    EdgeId::next(),
                    source_id,
                    OutputPortIndex(0),
                    blur,
                    target_port,
                )
                .unwrap();
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_port(blur, "radius", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(
                graph
                    .node(blur)
                    .unwrap()
                    .param_port_index("radius")
                    .is_none()
            );
            assert_eq!(
                graph.edge_count(),
                0,
                "unexpose removes the edge atomically"
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(
                graph
                    .node(blur)
                    .unwrap()
                    .param_port_index("radius")
                    .is_some()
            );
            assert_eq!(graph.edge_count(), 1, "one undo restores port and edge");
        });
    }

    /// Bypass is a metadata flag toggle committed through the document: one
    /// undo step restores the un-bypassed node (and redo re-applies it).
    #[gpui::test]
    fn bypass_toggle_roundtrips_through_document_undo(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        let is_bypassed = |project: &Entity<ProjectState>, cx: &mut TestAppContext| {
            project.read_with(cx, |project, _| {
                resolve_network(project.document(), &path)
                    .expect("network")
                    .node(blur)
                    .expect("blur node")
                    .metadata
                    .bypassed
            })
        };

        assert!(!is_bypassed(&project, cx));
        window
            .update(cx, |panel, _window, cx| {
                panel.set_bypass(&[blur], true, cx);
            })
            .unwrap();
        assert!(is_bypassed(&project, cx));

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(!is_bypassed(&project, cx));
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!(is_bypassed(&project, cx));
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

    /// Boundary nodes (net.in / net.out) are the network's fixed interface
    /// (REQ-LAYER-002): copy, delete, duplicate, and bypass must never
    /// target them, so each network keeps exactly one In and one Out.
    #[gpui::test]
    fn boundary_nodes_survive_delete_duplicate_and_bypass(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        // Give the layer network its interface nodes.
        let in_id = NodeId::next();
        let out_id = NodeId::next();
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path).unwrap().clone();
            let graph = graph
                .add_node(
                    Node::new(in_id, ravel_core::network::NET_IN_TYPE_KEY)
                        .with_output("f", DataTypeId::SCALAR),
                )
                .unwrap()
                .add_node(
                    Node::new(out_id, ravel_core::network::NET_OUT_TYPE_KEY)
                        .with_input("frame", &[DataTypeId::FRAME_BUFFER]),
                )
                .unwrap();
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                let count = |panel: &NodeEditorPanel, pred: fn(&Node) -> bool| {
                    panel.graph.nodes().filter(|n| pred(n)).count()
                };
                let is_in = |n: &Node| ravel_core::network::is_in_node(n);
                let is_out = |n: &Node| ravel_core::network::is_out_node(n);

                // Copy of a mixed selection stores only editable nodes.
                panel.selected_nodes = [in_id, out_id, blur].into_iter().collect();
                panel.copy_selected();
                let clipboard = panel.clipboard.as_ref().expect("copy stored nodes");
                assert_eq!(clipboard.nodes.len(), 1);
                assert_eq!(clipboard.nodes[0].id, blur);

                // Delete removes the blur node but keeps both boundaries.
                panel.delete_selected(cx);
                assert!(panel.graph.node(blur).is_none());
                assert_eq!(count(panel, is_in), 1);
                assert_eq!(count(panel, is_out), 1);

                // Duplicate of a boundary-only selection is a no-op.
                panel.selected_nodes = [in_id, out_id].into_iter().collect();
                panel.duplicate_selected(cx);
                assert_eq!(count(panel, is_in), 1);
                assert_eq!(count(panel, is_out), 1);

                // Bypass of a boundary-only selection is a no-op: the flags
                // stay clear and the nodes stay put.
                panel.set_bypass(&[in_id, out_id], true, cx);
                assert_eq!(count(panel, is_in), 1);
                assert_eq!(count(panel, is_out), 1);
                assert!(!panel.graph.node(in_id).unwrap().metadata.bypassed);
                assert!(!panel.graph.node(out_id).unwrap().metadata.bypassed);
            })
            .unwrap();

        // The bypass call recorded no undo step: the single Document undo
        // reverts the blur deletion above, not a no-op bypass snapshot.
        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.graph.node(blur).is_some());
            })
            .unwrap();
    }
}

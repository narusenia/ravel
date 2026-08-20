// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node editor panel: edits exactly one network of the document at a time
//! (REQ-LAYER-011).
//!
//! The edited network is identified by an ownership path
//! ([`NetworkPath`]: `CompId / LayerId / [SubnetNodeId ...]`). The timeline
//! opens a layer's network via [`NodeEditorPanel::open_network`]
//! by selecting a Timeline layer; double-clicking a subnet node dives one
//! level deeper, and the breadcrumb bar returns to any ancestor. Clearing the
//! Timeline selection closes the network.
//!
//! Edits are committed to the app-wide [`ProjectState`]: the new network is
//! spliced into the document (structural sharing) and recorded as one
//! Document-level undo step (REQ-LAYER-009). Undo/redo are *not* handled
//! here — the edit actions bubble to the workspace, which routes them to the
//! document store, and this panel resyncs through its project observer.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::Sizable as _;
use gpui_component::input::{self, Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::animation::curve::KeyframeCurve;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::animation::step::StepCurve;
use ravel_core::eval::EvalContext;
use ravel_core::exposed::KeyRename;
use ravel_core::graph::{Graph, PortSide};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::network::{
    CustomPortType, NetworkContext, NetworkError, PinRename, PortEdit, is_fixed_port, is_in_node,
    is_out_node,
};
use ravel_core::registry::builtin::register_builtins;
use ravel_core::registry::{NodeCategory, NodeRegistry};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::document::{NetworkPath, replace_network_renaming_pin, resolve_network};
use ravel_ui::properties::expression;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::assets::RavelIcon;
use crate::node_editor::EdgeStyle;
use crate::node_editor::hover_popover::{
    HOVER_DWELL, HoverPopover, hover_info, hover_popover_element,
};
use crate::node_editor::layout::{LayoutAxis, auto_layout};
use crate::node_editor::painting::{self, EvalReadout, PortHit, compute_node_size, node_width};
use crate::node_editor::palette::{PaletteEvent, SearchPalette, retain_connectable};
use crate::node_editor::viewport::Viewport;
use crate::project_state::ProjectState;
use crate::workspace::{
    EditCopy, EditDelete, EditDuplicate, EditPaste, NodeAutoLayout, NodeCollapseToSubnet,
    NodeExtractSubnet, NodeSearchPalette, ViewFit,
};
use ravel_ui::command::CommandId;

use ravel_core::graph::{Edge, Node, ParameterValue};

use super::param_edit::edited_param_value;

/// GPUI key context used by shortcuts local to the node editor.
pub const KEY_CONTEXT: &str = "NodeEditor";

const CUSTOM_PATH_TYPE_KEY: &str = "shape.custom_path";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AddNodeMenuItem {
    pub(crate) label: String,
    pub(crate) type_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AddNodeMenuGroup {
    pub(crate) category: NodeCategory,
    pub(crate) items: Vec<AddNodeMenuItem>,
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

/// Menu state of the Collapse / Extract items for the nodes the menu is
/// acting on (REQ-LAYER-003).
///
/// Both items are always shown and only ever disabled, for the reason
/// [`PortMenuModel`] gives: an item that comes and goes never teaches that the
/// operation exists.
///
/// **Collapse** is the core's own answer
/// ([`ravel_core::network::can_collapse`]), so the menu offers exactly what
/// the transform accepts — something left to move once the boundary and
/// synthetic nodes are dropped, and no path that leaves the selection and
/// comes back.
///
/// **Extract** names the one subnet node it would open, so it is enabled only
/// for a single target that owns an inner graph. A selection of several nodes
/// gives no answer to "which one", and a `subnet` node without an inner graph
/// has nothing to give back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SubnetMenuModel {
    collapse: bool,
    extract: Option<NodeId>,
}

fn subnet_menu_model(graph: &Graph, targets: &[NodeId]) -> SubnetMenuModel {
    let extract = match targets {
        [id] => graph
            .node(*id)
            .filter(|node| ravel_core::network::is_subnet_node(node) && node.subnet.is_some())
            .map(|node| node.id),
        _ => None,
    };
    SubnetMenuModel {
        collapse: ravel_core::network::can_collapse(graph, targets.iter().copied()),
        extract,
    }
}

/// What the port context menu offers for the port under the cursor
/// (REQ-LAYER-002, REQ-LAYER-003).
///
/// Rename and Delete act on the custom ports of a network-interface node and
/// on nothing else, but the items are *shown* on every port: a menu whose
/// contents change with where the cursor landed never teaches that the
/// operation exists, so only `enabled` differs.
///
/// Disabled covers three cases, all of them "the network owns this port, the
/// user does not":
///
/// - a **fixed** port of an In / Out node. [`is_fixed_port`] is the authority,
///   which also keeps the legacy `f` exception: a `net.in` output named `f`
///   that carries its own parameter is a user-defined port and stays editable;
/// - any port of an ordinary node, which has no custom-port concept, and the
///   non-custom side of an interface node (In declares custom ports as
///   outputs, Out as inputs);
/// - a **Subnet** node's pins. Those are derived from the inner network's
///   In / Out nodes rather than declared on the subnet node, so the port to
///   edit is the inner one — `ravel_core::network::remove_custom_port` accepts
///   `net.in` / `net.out` only. Editing pins from the outside waits for the
///   pin synchronization of unit 5.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PortMenuModel {
    node_id: NodeId,
    side: PortSide,
    name: String,
    enabled: bool,
}

fn port_menu_model(graph: &Graph, hit: &PortHit) -> Option<PortMenuModel> {
    let node = graph.node(hit.node_id)?;
    let index = hit.port_index as usize;
    let (side, name) = if hit.is_output {
        (PortSide::Output, node.outputs.get(index)?.name.clone())
    } else {
        (PortSide::Input, node.inputs.get(index)?.name.clone())
    };
    let custom_side = if is_in_node(node) {
        Some(PortSide::Output)
    } else if is_out_node(node) {
        Some(PortSide::Input)
    } else {
        None
    };
    let enabled = custom_side == Some(side) && !is_fixed_port(node, side, &name);
    Some(PortMenuModel {
        node_id: hit.node_id,
        side,
        name,
        enabled,
    })
}

pub(crate) fn node_category_order(category: NodeCategory) -> u8 {
    match category {
        NodeCategory::Geometry => 0,
        NodeCategory::Scene => 1,
        NodeCategory::Field => 2,
        NodeCategory::Image => 3,
        NodeCategory::Color => 4,
        NodeCategory::Time => 5,
        NodeCategory::Utility => 6,
    }
}

/// Localized menu label of a category.
///
/// `pub` rather than `pub(crate)` for the same reason
/// [`crate::node_editor::hover_popover::data_type_name`] is: the lib unit
/// tests run with an empty i18n store, so the catalog coverage of these
/// labels can only be asserted from an integration test that loads the real
/// catalogs (`tests/node_hover_popover.rs`).
pub fn node_category_label(category: NodeCategory) -> String {
    match category {
        NodeCategory::Geometry => t!("panel.node_graph_menu.category.geometry"),
        NodeCategory::Scene => t!("panel.node_graph_menu.category.scene"),
        NodeCategory::Field => t!("panel.node_graph_menu.category.field"),
        NodeCategory::Image => t!("panel.node_graph_menu.category.image"),
        NodeCategory::Color => t!("panel.node_graph_menu.category.color"),
        NodeCategory::Time => t!("panel.node_graph_menu.category.time"),
        NodeCategory::Utility => t!("panel.node_graph_menu.category.utility"),
    }
}

pub(crate) fn add_node_menu_model(registry: &NodeRegistry) -> Vec<AddNodeMenuGroup> {
    let mut categories = registry.categories();
    categories.sort_by_key(|category| node_category_order(*category));

    categories
        .into_iter()
        .filter_map(|category| {
            let mut items: Vec<_> = registry
                .list_by_category(category)
                .into_iter()
                // Custom Path stays hidden: its `points` parameter is only
                // editable through the pen tool (tool-system plan unit 7);
                // adding it from the menu would create an uneditable node.
                .filter(|template| template.type_key != CUSTOM_PATH_TYPE_KEY)
                .map(|template| AddNodeMenuItem {
                    label: crate::node_locale::type_label(&template.type_key),
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

/// Returns the first port on `candidate` that can connect to `from`.
pub(crate) fn first_compatible_port(
    graph: &Graph,
    from: &PortHit,
    candidate: &Node,
) -> Option<u32> {
    let source = graph.node(from.node_id)?;
    if from.is_output {
        let data_type = source.outputs.get(from.port_index as usize)?.data_type;
        candidate
            .inputs
            .iter()
            .position(|port| {
                port.accepted_types.is_empty() || port.accepted_types.contains(&data_type)
            })
            .map(|index| index as u32)
    } else {
        let accepted_types = &source.inputs.get(from.port_index as usize)?.accepted_types;
        candidate
            .outputs
            .iter()
            .position(|port| accepted_types.is_empty() || accepted_types.contains(&port.data_type))
            .map(|index| index as u32)
    }
}

/// Restore the variadic-group invariant after one or more edges have been
/// removed from `node_id`: connected slots stay ordered and exactly one empty
/// slot remains at the end.
fn compact_empty_variadic_inputs(mut graph: Graph, node_id: NodeId) -> Graph {
    loop {
        let Some(node) = graph.node(node_id) else {
            return graph;
        };
        let Some((index, _)) = node.inputs.iter().enumerate().find(|(index, input)| {
            input.is_variadic
                && node.inputs[*index + 1..]
                    .first()
                    .is_some_and(|next| next.is_variadic)
                && !graph.edges().any(|edge| {
                    edge.target == node_id && edge.target_port == InputPortIndex(*index as u32)
                })
        }) else {
            return graph;
        };
        let Ok(compacted) = graph
            .clone()
            .compact_variadic_input_group(node_id, InputPortIndex(index as u32))
        else {
            return graph;
        };
        graph = compacted;
    }
}

/// Remove one edge and compact its target's variadic group in the same graph
/// snapshot.
fn remove_edge_and_compact(graph: Graph, edge_id: EdgeId) -> Option<Graph> {
    let target = graph.edge(edge_id)?.target;
    let removed = graph.remove_edge(edge_id).ok()?;
    Some(compact_empty_variadic_inputs(removed, target))
}

/// Remove one node and compact every surviving variadic target reached by its
/// outgoing edges.
fn remove_node_and_compact(graph: Graph, node_id: NodeId) -> Option<Graph> {
    let targets: HashSet<_> = graph
        .edges()
        .filter(|edge| edge.source == node_id)
        .map(|edge| edge.target)
        .collect();
    let mut graph = graph.remove_node(node_id).ok()?;
    for target in targets {
        graph = compact_empty_variadic_inputs(graph, target);
    }
    Some(graph)
}

/// Replace any edge occupying `target_port`, connect the new edge, and grow a
/// trailing variadic slot when the connection fills the group. Replacement of
/// a variadic slot compacts first, so the replacement is appended after the
/// surviving connected sources. Shared with the Viewer's shape drawing tools,
/// which call it only on free inputs (no replacement).
pub(super) fn connect_edge_and_update_variadic_inputs(
    mut graph: Graph,
    edge_id: EdgeId,
    source: NodeId,
    source_port: OutputPortIndex,
    target: NodeId,
    mut target_port: InputPortIndex,
) -> Option<Graph> {
    let target_is_variadic = graph
        .node(target)?
        .inputs
        .get(target_port.0 as usize)
        .is_some_and(|input| input.is_variadic);
    let existing: Vec<_> = graph
        .edges()
        .filter(|edge| edge.target == target && edge.target_port == target_port)
        .map(|edge| edge.id)
        .collect();
    for existing_edge in existing {
        graph = remove_edge_and_compact(graph, existing_edge)?;
    }

    if target_is_variadic {
        target_port = graph
            .node(target)?
            .inputs
            .iter()
            .enumerate()
            .find(|(index, input)| {
                input.is_variadic
                    && !graph.edges().any(|edge| {
                        edge.target == target && edge.target_port == InputPortIndex(*index as u32)
                    })
            })
            .map(|(index, _)| InputPortIndex(index as u32))?;
    }

    let graph = graph
        .add_edge(edge_id, source, source_port, target, target_port)
        .ok()?;
    Some(
        graph
            .clone()
            .grow_variadic_input_group(target)
            .unwrap_or(graph),
    )
}

/// Parameters of `node` currently driven by a connected parameter port,
/// with a display value when the source is statically known (constant /
/// constant.color). Live evaluated values for arbitrary sources are a
/// planned follow-up (param-input-ports-plan Phase 4). Shared with the
/// Properties panel, which re-derives driven state from the document.
pub(crate) fn driven_params(
    graph: &Graph,
    node: &Node,
    registry: &NodeRegistry,
) -> Vec<ravel_ui::properties::DrivenParam> {
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
        let label = crate::node_locale::display_label(source, registry);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PointerHint {
    #[default]
    Empty,
    Port,
    Node,
    Edge,
}

impl PointerHint {
    fn cursor(self) -> CursorStyle {
        match self {
            Self::Empty | Self::Port => CursorStyle::Crosshair,
            Self::Node => CursorStyle::OpenHand,
            Self::Edge => CursorStyle::PointingHand,
        }
    }
}

fn pointer_hint_transition(
    current: PointerHint,
    next: PointerHint,
    dragging: bool,
) -> Option<PointerHint> {
    (!dragging && current != next).then_some(next)
}

fn drag_cursor(drag: &DragMode) -> Option<CursorStyle> {
    match drag {
        DragMode::None => None,
        DragMode::Pan { .. } | DragMode::MoveNodes { .. } => Some(CursorStyle::ClosedHand),
        DragMode::Connect { snap: Some(_), .. } => Some(CursorStyle::DragLink),
        DragMode::Connect { snap: None, .. } | DragMode::SelectBox { .. } => {
            Some(CursorStyle::Crosshair)
        }
    }
}

/// An open node search palette (DISC-3). The placement context — where the
/// new node lands and, for a wire-drop invocation, which port it connects —
/// stays on the panel; the palette entity only reports the picked type.
struct PaletteOpen {
    palette: Entity<SearchPalette>,
    /// The dragged port when the palette was invoked from a wire drop.
    from: Option<PortHit>,
    /// Canvas-local position the new node is placed at.
    local: (f32, f32),
    /// Window-space anchor of the overlay.
    anchor: Point<Pixels>,
    #[allow(dead_code)]
    event_sub: Subscription,
}

/// An open custom-port rename editor.
///
/// The Outliner's layer rename is the shape this follows — an `InputState`
/// the panel owns, committed on Enter or on blur, cancelled with Escape —
/// floated over the canvas at the port instead of living in a row, because a
/// canvas port has no row to edit in place.
struct PortRename {
    node_id: NodeId,
    old_name: String,
    /// Canvas-local center of the port, turned into a window-space anchor at
    /// paint time so the popover follows the canvas origin.
    center: (f32, f32),
    input: Entity<InputState>,
    /// The name already sent for this port, if the graph refused it. Enter
    /// and the blur that follows it report the same text, and a refused
    /// rename stays open to be blurred later — resending would repeat a
    /// failure the user can already read.
    attempted: Option<String>,
    #[allow(dead_code)]
    sub: Subscription,
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
            let value = curve.sample(frame as f64);
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
    add_node_menu: Vec<AddNodeMenuGroup>,
    /// Load readouts of the displayed nodes, already reduced to what the
    /// canvas draws. Holding the drawn form (and not the raw `Duration`)
    /// is what keeps a repaint tied to a *visible* change; see
    /// [`EvalReadout`].
    displayed_timings: HashMap<NodeId, EvalReadout>,
    viewport: Viewport,
    selected_edges: HashSet<EdgeId>,
    node_sizes: HashMap<NodeId, (f32, f32)>,
    /// Header tint category per node, rebuilt with [`Self::node_sizes`].
    /// A function of the graph, so `render()` only clones it.
    node_categories: HashMap<NodeId, NodeCategory>,
    /// Localized display label per node, rebuilt with [`Self::node_sizes`].
    node_labels: HashMap<NodeId, String>,
    edge_style: EdgeStyle,
    clipboard: Option<ClipboardContent>,
    drag: DragMode,
    pointer_hint: PointerHint,
    /// Hover-dwell tracking for the node info popover (DISC-2); suppressed
    /// while a gesture is active.
    hover_popover: HoverPopover,
    palette: Option<PaletteOpen>,
    /// In-flight custom-port rename, `None` when no port is being renamed.
    port_rename: Option<PortRename>,
    /// The last refused custom-port edit, shown in the canvas corner until
    /// the next port edit or context menu.
    port_error: Option<SharedString>,
    /// Recently added node types, most recent first. Session memory only
    /// (not persisted); the search palette ranks these first.
    recent_types: Vec<String>,
    canvas_origin: Rc<Cell<(f32, f32)>>,
    canvas_size: Rc<Cell<(f32, f32)>>,
    last_right_click: Rc<Cell<(f32, f32)>>,
    /// Last pointer position **over the canvas**, in canvas-local pixels, so
    /// the keyboard-opened search palette lands where the hand last worked.
    ///
    /// `on_mouse_move` only fires while the hitbox is hovered, so this is
    /// never written with an outside position and it keeps its value after the
    /// pointer leaves — which is what the palette wants, since the last place
    /// the user was editing beats the canvas center. The bounds check at use
    /// is for the case the value goes stale another way: the canvas shrinking
    /// out from under a position that used to be inside it.
    last_pointer: Option<(f32, f32)>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    /// Keeps [`super::NodeEditorHandle`] pointing at this instance while it
    /// holds the focus.
    #[allow(dead_code)]
    handle_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    layer_selection_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    timings_sub: Subscription,
    /// Pays off the sync skipped while the panel was behind another tab
    /// (see [`super::on_became_visible`]).
    #[allow(dead_code)]
    visibility_sub: Subscription,
}

impl NodeEditorPanel {
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, move |this: &mut Self, project, cx| {
                // Behind another tab nobody sees the graph, so the resolve
                // waits for the panel to come back. Before the epoch gate, so
                // that what was skipped stays owed (`visibility_sub` below).
                if !super::is_instance_visible(instance, cx) {
                    return;
                }
                // Re-resolving the display graph clones the document and deep
                // compares the network; skip it when the document has not moved
                // since the last sync.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.sync_from_project(&project, cx);
            })
        });

        let selection_sub = cx.observe_global::<super::CanvasSelection>(|_this, cx| cx.notify());
        // The editor follows the shared layer selection instead of being
        // pushed at by whoever wrote it (REQ-UI-013): Timeline and Outliner
        // are both writers, and a composition switch resets the selection, so
        // observing it is the single path that covers all three.
        let layer_selection_sub =
            cx.observe_global::<super::LayerSelection>(move |this: &mut Self, cx| {
                if super::is_instance_visible(instance, cx) {
                    Self::follow_layer_selection(this, cx);
                    return;
                }
                // Hidden: opening the newly selected network (a document
                // resolve, the caches, the view fit) waits for the panel to
                // come back. Nothing has to be queued for that —
                // `LayerSelection` is durable shared state, so the catch-up
                // below reads whatever it holds *then*.
                //
                // What cannot wait is the selection this panel publishes.
                // `CanvasSelection` names nodes of the network still open
                // here, and the Viewer — never gated — draws its bbox and
                // resolves its gesture targets from it. Leaving it pointed
                // into a network the user has left would put a stale gizmo on
                // screen in a panel that is *not* hidden, so dropping it is
                // the one thing this branch still does.
                if !this.selection_names_open_network(cx) && !Self::selected_nodes(cx).is_empty() {
                    this.clear_selected_nodes(cx);
                }
            });
        // The per-node load readout is the one thing this panel draws from
        // evaluation output rather than from the document, so it follows the
        // timings global directly. `ProjectState` deliberately does not notify
        // on evaluation results (see `ProjectState::on_eval_update`), and this
        // repaints without rebuilding the graph model.
        let timings_sub =
            cx.observe_global::<crate::project_state::NodeEvalTimings>(|this: &mut Self, cx| {
                if this.context.is_none() {
                    return;
                }
                let timings = Self::collect_readouts(&this.graph, cx);
                if timings != this.displayed_timings {
                    this.displayed_timings = timings;
                    cx.notify();
                }
            });
        // Coming back into view pays off both observers above, and does it
        // with exactly one document resolve.
        //
        // `LayerSelection` is durable shared state, so the selection made
        // while this panel was hidden is simply the value it holds now — there
        // is no pending-selection queue to keep. `open_network` re-resolves
        // the graph itself, so the extra `refresh_from_document` is for the
        // other case only: the selection did not move and the document did.
        let visibility_sub = super::on_became_visible(instance, cx, |this, cx| {
            if let Some(project) = this.project.clone() {
                let epoch = project.read(cx).mirror_epoch();
                this.mirror_epoch.advanced(epoch);
            }
            let already_open = this.selection_names_open_network(cx);
            this.follow_layer_selection(cx);
            if already_open {
                this.refresh_from_document(cx);
                cx.notify();
            }
        });

        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);

        // Properties and the playback controller post into one node editor:
        // the instance that was built last, and from then on the focused one.
        cx.set_global(super::NodeEditorHandle(cx.entity().downgrade()));
        let handle_sub =
            super::track_focused_handle(&focus_handle, window, cx, super::NodeEditorHandle);

        Self {
            project,
            context: None,
            graph: Graph::new(),
            add_node_menu: add_node_menu_model(&registry),
            displayed_timings: HashMap::new(),
            registry,
            viewport: Viewport {
                x: 50.0,
                y: 50.0,
                zoom: 1.0,
            },
            selected_edges: HashSet::new(),
            node_sizes: HashMap::new(),
            node_categories: HashMap::new(),
            node_labels: HashMap::new(),
            edge_style: crate::app_settings::resolved(cx).node_editor_edge_style,
            clipboard: None,
            drag: DragMode::None,
            pointer_hint: PointerHint::default(),
            hover_popover: HoverPopover::default(),
            palette: None,
            port_rename: None,
            port_error: None,
            recent_types: Vec::new(),
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            last_pointer: None,
            canvas_size: Rc::new(Cell::new((800.0, 600.0))),
            last_right_click: Rc::new(Cell::new((0.0, 0.0))),
            focus_handle,
            focus_subscriptions,
            handle_sub,
            selection_sub,
            layer_selection_sub,
            project_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            timings_sub,
            visibility_sub,
        }
    }

    // ----- layer selection follow (REQ-UI-013) ------------------------------

    /// Open the network of the selected layer, or close the current one when
    /// the selection does not name exactly one layer.
    ///
    /// A single-layer editor has no meaningful view of a multi-layer selection,
    /// so nothing selected and several layers selected map to the same closed
    /// state (the message differs, hence the unconditional notify): closing also
    /// clears `CanvasSelection`, so no node — or Viewer bbox reading it — is
    /// left pointing into a network the user is no longer editing.
    ///
    /// A re-selection of the layer already open is left alone, subnet depth
    /// included: diving into a subnet must survive the Outliner and the
    /// Timeline re-publishing the same selection.
    fn follow_layer_selection(&mut self, cx: &mut Context<Self>) {
        let selection = super::layer_selection(cx);
        let single = match selection.layers() {
            [layer] => Some(*layer),
            _ => None,
        };
        let Some((comp, layer)) = selection.comp().zip(single) else {
            self.close_network(cx);
            cx.notify();
            return;
        };
        if self.selection_names_open_network(cx) {
            return;
        }
        self.open_network(NetworkPath::layer(comp, layer), cx);
    }

    /// Whether the shared layer selection names exactly the network this panel
    /// already has open.
    ///
    /// Both the "nothing to do" guard above and the visibility gate ask this,
    /// and they have to agree: the gate uses it to decide whether the pending
    /// selection is a real switch, and the catch-up uses it to avoid resolving
    /// the document twice (once through `open_network`, once on its own).
    fn selection_names_open_network(&self, cx: &App) -> bool {
        let selection = super::layer_selection(cx);
        let [layer] = selection.layers() else {
            return false;
        };
        selection.comp().is_some_and(|comp| {
            self.context
                .as_ref()
                .is_some_and(|open| open.comp == comp && open.layer == *layer)
        })
    }

    // ----- canvas selection (CanvasSelection Global) -------------------------

    fn selected_nodes(cx: &App) -> HashSet<NodeId> {
        cx.try_global::<super::CanvasSelection>()
            .map(|s| s.nodes.clone())
            .unwrap_or_default()
    }

    /// Whether the published [`CanvasSelection`](super::CanvasSelection)
    /// already names exactly `nodes` in the network this panel has open.
    fn selection_matches(&self, nodes: &HashSet<NodeId>, cx: &App) -> bool {
        cx.try_global::<super::CanvasSelection>()
            .is_some_and(|current| current.path == self.context && &current.nodes == nodes)
    }

    /// Publish `nodes` as the canvas selection.
    ///
    /// Republishing a selection that is already current is dropped here rather
    /// than at each caller: `CanvasSelection` is a durable global whose every
    /// write wakes the Viewer's gesture-target walk and the Outliner's
    /// repaint, and a caller that recomputes the same set on every mouse move
    /// (the rubber band) would pay that wave per move for no change at all.
    fn set_selected_nodes(&self, nodes: HashSet<NodeId>, cx: &mut App) {
        if self.selection_matches(&nodes, cx) {
            return;
        }
        cx.set_global(super::CanvasSelection {
            path: self.context.clone(),
            nodes,
        });
    }

    fn clear_selected_nodes(&self, cx: &mut App) {
        self.set_selected_nodes(HashSet::new(), cx);
    }

    /// Publish the set a rubber band currently encloses, and report whether
    /// anything was published.
    ///
    /// The band recomputes that set on every mouse move and most moves
    /// enclose exactly what the previous one did. Both publications are
    /// expensive on the other side — `CanvasSelection` wakes the Viewer and
    /// the Outliner, and `PropertiesTarget` makes the Properties panel
    /// re-resolve every section — so a band that has not changed what it
    /// holds publishes nothing.
    ///
    /// This is deliberately narrower than the guard inside
    /// [`Self::set_selected_nodes`]: [`Self::refresh_from_document`]
    /// republishes the *Properties* target on purpose when the selection
    /// stands still but its values, exposure or driven state moved.
    fn publish_band_selection(&self, nodes: HashSet<NodeId>, cx: &mut App) -> bool {
        if self.selection_matches(&nodes, cx) {
            return false;
        }
        self.set_selected_nodes(nodes, cx);
        self.notify_properties_selection(cx);
        true
    }

    // ----- network context (REQ-LAYER-011) ----------------------------------

    /// The ownership path of the network currently being edited.
    pub fn context(&self) -> Option<&NetworkPath> {
        self.context.as_ref()
    }

    /// The graph this panel currently displays (tests and the debug
    /// inspector). Resolved from the document by
    /// [`Self::refresh_from_document`], so it is what the canvas draws — not
    /// the live document.
    pub fn displayed_graph(&self) -> &Graph {
        &self.graph
    }

    /// Open the network at `path` (layer selection, subnet dive, breadcrumb
    /// jump, Outliner row).
    ///
    /// A node selection that already names `path` is kept: `CanvasSelection`
    /// carries the network it belongs to, so a writer that selects nodes of a
    /// not-yet-open network (the Outliner selecting a node row) stays valid
    /// through the switch. Any other selection belongs to the network being
    /// left and is dropped.
    pub fn open_network(&mut self, path: NetworkPath, cx: &mut Context<Self>) {
        if self.context.as_ref() == Some(&path) {
            return;
        }
        // The hover popover belongs to the network being left: another
        // network may reuse the same node ids, and a stale popover would
        // anchor to the wrong node (or resurrect without a dwell).
        self.hover_popover.cancel();
        let keep_selection = cx
            .try_global::<super::CanvasSelection>()
            .is_some_and(|selection| selection.path.as_ref() == Some(&path));
        self.context = Some(path);
        // The palette's placement context (local point, wire source) belongs
        // to the network being left, and so do an open port rename and the
        // last refused port edit.
        self.dismiss_palette(cx);
        self.cancel_port_rename(cx);
        self.port_error = None;
        if !keep_selection {
            self.clear_selected_nodes(cx);
        }
        self.selected_edges.clear();
        self.refresh_from_document(cx);
        // `refresh_from_document` above only rebuilds the caches when the
        // graph actually moved; opening a network whose graph happens to
        // equal the previous one still has to re-resolve the readouts.
        self.displayed_timings = Self::collect_readouts(&self.graph, cx);
        self.fit_view();
        self.notify_properties_selection(cx);
        cx.notify();
    }

    /// Open `path` if needed and pan the view onto `node`, keeping the current
    /// zoom (Outliner double-click, REQ-UI-013).
    pub fn center_on_node(&mut self, path: NetworkPath, node: NodeId, cx: &mut Context<Self>) {
        self.open_network(path, cx);
        let Some(target) = self.graph.node(node) else {
            return;
        };
        // Measured sizes are zoomed; the fallback matches `fit_view`'s.
        let (w, h) = self.node_sizes.get(&node).copied().unwrap_or((160.0, 60.0));
        let (canvas_w, canvas_h) = self.canvas_size.get();
        let rect = (
            target.metadata.position.0,
            target.metadata.position.1,
            w / self.viewport.zoom,
            h / self.viewport.zoom,
        );
        self.viewport.center_on(rect, canvas_w, canvas_h);
        cx.notify();
    }

    /// Open `path` and fit its whole network into view (Outliner layer-row
    /// double-click).
    pub fn open_and_fit(&mut self, path: NetworkPath, cx: &mut Context<Self>) {
        self.open_network(path, cx);
        self.fit_view();
        cx.notify();
    }

    /// Dive into the subnet owned by `node` of the network at `path`
    /// (Outliner subnet-row double-click, REQ-LAYER-003).
    pub fn enter_subnet_at(&mut self, path: NetworkPath, node: NodeId, cx: &mut Context<Self>) {
        self.open_network(path.entered(node), cx);
    }

    /// Close the current network and return to the empty state.
    pub fn close_network(&mut self, cx: &mut Context<Self>) {
        if self.context.is_none() {
            return;
        }
        self.context = None;
        self.graph = Graph::default();
        self.dismiss_palette(cx);
        self.cancel_port_rename(cx);
        self.port_error = None;
        self.node_sizes.clear();
        self.node_categories.clear();
        self.node_labels.clear();
        self.displayed_timings.clear();
        // Reopening the same network yields the same node ids: without this,
        // an open popover would resurrect over the reopened node with no
        // dwell.
        self.hover_popover.cancel();
        self.clear_selected_nodes(cx);
        self.selected_edges.clear();
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
        super::sync_probe::record(super::sync_probe::PanelSync::NodeEditorRefresh);
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
            // The graph the palette's placement context (the wire-drop
            // source port, the drop point) refers to is gone: an accept
            // would connect to a stale node or place at stale coordinates.
            self.dismiss_palette(cx);
            // The document moved under the panel (undo/redo, an edit from
            // another panel or window), so a port may have been added,
            // removed or reordered: the wire drag's port indices and the
            // rename editor's anchor can both be stale. Neither is worth
            // repairing against a graph the user did not edit here.
            self.invalidate_port_interactions(cx);
            // And the refusal notice described a graph state that is gone.
            self.port_error = None;
            self.refresh_graph_caches(cx);
            // The hovered node may be gone (delete, undo, context switch):
            // close the popover instead of anchoring it to a stale id.
            if self
                .hover_popover
                .target()
                .is_some_and(|id| self.graph.node(id).is_none())
            {
                self.hover_popover.cancel();
            }
            let mut sel = Self::selected_nodes(cx);
            let before = sel.len() + self.selected_edges.len();
            sel.retain(|id| self.graph.node(*id).is_some());
            // Only publish when the pruning (or a truncated context) actually
            // moved the selection. A parameter drag changes the graph on every
            // mouse move while the selection stays put, and re-publishing the
            // identical `CanvasSelection` would wake its own wave of global
            // observers — the Viewer walking the document for its gesture
            // targets, the Outliner repainting — for no change at all.
            if !self.selection_matches(&sel, cx) {
                self.set_selected_nodes(sel, cx);
            }
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
                .map(|n| crate::node_locale::display_label(n, &self.registry))
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
    ///
    /// `key_rename` is the parameter key the edit moved, if it moved one: it
    /// travels into the same commit so the declarations bound to that key
    /// follow it (see [`Self::edit_custom_ports`]). Every other edit passes
    /// `None` — nothing else in this panel can rename a parameter key.
    fn commit_graph(
        &mut self,
        graph: Graph,
        key_rename: Option<KeyRename>,
        cx: &mut Context<Self>,
    ) {
        self.commit_port_edit(graph, key_rename, None, cx);
    }

    /// [`Self::commit_graph`] for the one edit that also moves a pin of the
    /// subnet node enclosing the open network ([`PinRename`]).
    fn commit_port_edit(
        &mut self,
        graph: Graph,
        key_rename: Option<KeyRename>,
        pin_rename: Option<PinRename>,
        cx: &mut Context<Self>,
    ) {
        self.commit_to_document(
            graph,
            key_rename,
            pin_rename,
            InvalidationHint::Structural,
            true,
            cx,
        );
        self.notify_properties_selection(cx);
    }

    /// Connect ports and normalize the target variadic group as one Document
    /// undo step.
    fn connect_ports(
        &mut self,
        source: NodeId,
        source_port: OutputPortIndex,
        target: NodeId,
        target_port: InputPortIndex,
        cx: &mut Context<Self>,
    ) {
        if let Some(graph) = connect_edge_and_update_variadic_inputs(
            self.graph.clone(),
            EdgeId::next(),
            source,
            source_port,
            target,
            target_port,
        ) {
            self.commit_graph(graph, None, cx);
        }
    }

    /// Remove an edge and normalize its target variadic group as one Document
    /// undo step.
    fn remove_edge(&mut self, edge_id: EdgeId, cx: &mut Context<Self>) {
        if let Some(graph) = remove_edge_and_compact(self.graph.clone(), edge_id) {
            self.commit_graph(graph, None, cx);
        }
    }

    /// Toggle one parameter input port as one structural Document undo step.
    /// Removing a port also removes its connected edges atomically in
    /// [`Graph::remove_param_port`].
    pub fn toggle_param_port(&mut self, node_id: NodeId, key: &str, cx: &mut Context<Self>) {
        let Some(node) = self.graph.node(node_id) else {
            return;
        };
        let exposed = node.param_port_index(key).is_some();
        // An identifier parameter must not be *driven* either: a Scalar wire
        // into `layer.ref`'s `layer` makes the referenced id a function of the
        // frame (`param_port_overlay` converts it), which is the same hole the
        // keyframe toggle refuses — the id scan reads stored values, so a
        // wire-driven reference is invisible to it.
        //
        // Only the **exposing** half is refused, and only here, on the UI path.
        // Removing an existing port stays possible so a document that already
        // holds one is not stuck with it, and `Graph::expose_param_port` /
        // `Document::validate` are deliberately left alone: rejecting the port
        // down there would make an existing document fail to open, which trades
        // this bug for data loss (`HIGH-26`, "what saved must open"). Closing
        // the API path is tracked as a separate issue.
        if !exposed
            && ravel_core::composition::validate::is_identifier_parameter(&node.type_key, key)
        {
            return;
        }
        let result = if exposed {
            self.graph.clone().remove_param_port(node_id, key)
        } else {
            self.graph.clone().expose_param_port(node_id, key)
        };
        if let Ok(graph) = result {
            self.commit_graph(graph, None, cx);
        }
    }

    // ----- network interface ports (REQ-LAYER-002, REQ-LAYER-003) -----------

    /// Run one custom-port edit against the open network and commit it as a
    /// single structural Document undo step.
    ///
    /// Every operation here goes through [`Self::commit_graph`], so the whole
    /// edit — the port, the parameter that pairs with it, the edges the
    /// change costs, and the exposed parameter declarations that name the
    /// parameter key it moved ([`PortEdit::key_rename`]) — lands in one
    /// Document snapshot. The `Err` is handed back rather than logged: a
    /// rejected name or type is something the user typed, and the Properties
    /// panel that called in is the place to say so. Without an open network
    /// there is nothing to edit and nothing went wrong, so that is a silent
    /// no-op.
    fn edit_custom_ports(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(Graph, NetworkContext) -> Result<PortEdit, NetworkError>,
    ) -> Result<(), NetworkError> {
        let Some(context) = self.context.as_ref().map(NetworkPath::context) else {
            return Ok(());
        };
        let (graph, key_rename, pin_rename) = edit(self.graph.clone(), context)?.into_parts();
        // A port edit that changed nothing — the group already selected, a
        // move that ran into a fixed neighbour — hands back the graph it was
        // given. Committing it anyway would push an undo step with no
        // difference in it and mark the project dirty, which is the rule the
        // declaration list already follows (`Graph::ptr_eq` is O(1) and only
        // returns true for the shared root, so a real edit never trips it).
        if graph.ptr_eq(&self.graph) && key_rename.is_none() && pin_rename.is_none() {
            return Ok(());
        }
        self.commit_port_edit(graph, key_rename, pin_rename, cx);
        // The edit went straight into `self.graph`, so the document observer
        // will find nothing to re-sync and the teardown in
        // `refresh_from_document` never runs. Every caller of this funnel —
        // this panel's context menu and the Properties Ports section through
        // `NodeEditorHandle` — reaches it here instead.
        self.invalidate_port_interactions(cx);
        cx.notify();
        Ok(())
    }

    /// Append a custom port named `name` to the interface node `node_id`
    /// (an In node's output plus its parameter, an Out node's input).
    pub fn add_custom_port(
        &mut self,
        node_id: NodeId,
        name: &str,
        port_type: CustomPortType,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, context| {
            ravel_core::network::add_custom_port(graph, node_id, name, port_type, context)
                .map(PortEdit::from)
        })
    }

    /// Remove the custom port `name`, its parameter, and its edges.
    pub fn remove_custom_port(
        &mut self,
        node_id: NodeId,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, context| {
            ravel_core::network::remove_custom_port(graph, node_id, name, context)
                .map(PortEdit::from)
        })
    }

    /// Rename the custom port `old_name`, carrying its parameter with it.
    pub fn rename_custom_port(
        &mut self,
        node_id: NodeId,
        old_name: &str,
        new_name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, context| {
            ravel_core::network::rename_custom_port(graph, node_id, old_name, new_name, context)
        })
    }

    /// Give the custom port `name` a new type, dropping the edges the new
    /// type cannot carry.
    pub fn set_custom_port_type(
        &mut self,
        node_id: NodeId,
        name: &str,
        port_type: CustomPortType,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, context| {
            ravel_core::network::set_custom_port_type(graph, node_id, name, port_type, context)
                .map(PortEdit::from)
        })
    }

    /// Put the custom parameter `name` of the In node `node_id` into the
    /// display group `group` (empty takes it out of every group).
    pub fn set_custom_port_group(
        &mut self,
        node_id: NodeId,
        name: &str,
        group: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, _context| {
            ravel_core::network::set_custom_port_group(graph, node_id, name, group)
                .map(PortEdit::from)
        })
    }

    /// Move the custom port `name` one slot earlier (`offset < 0`) or later
    /// (`offset > 0`), never past a built-in port.
    pub fn move_custom_port(
        &mut self,
        node_id: NodeId,
        name: &str,
        offset: i32,
        cx: &mut Context<Self>,
    ) -> Result<(), NetworkError> {
        self.edit_custom_ports(cx, |graph, _context| {
            ravel_core::network::move_custom_port(graph, node_id, name, offset).map(PortEdit::from)
        })
    }

    // ----- collapse / extract (REQ-LAYER-003) -------------------------------

    /// Move `targets` into a new subnet node as one Document undo step, and
    /// leave the selection on the node that now owns them.
    ///
    /// The selection moves because the nodes the user was working on are no
    /// longer in this network: keeping their ids selected would name nodes a
    /// level down that nothing here can show, and clearing it outright would
    /// throw away the one thing the operation produced. The refused cases are
    /// the ones [`subnet_menu_model`] disables, so reaching one here means the
    /// graph moved under an open menu — logged, not shown, because nothing was
    /// destroyed and nothing is half-applied.
    fn collapse_to_subnet(&mut self, targets: &[NodeId], cx: &mut Context<Self>) {
        if self.context.is_none() {
            return;
        }
        match ravel_core::network::collapse_to_subnet(self.graph.clone(), targets.iter().copied()) {
            Ok((graph, subnet)) => {
                self.selected_edges.clear();
                self.set_selected_nodes(std::iter::once(subnet).collect(), cx);
                self.commit_graph(graph, None, cx);
                // The commit writes `self.graph` directly, so the document
                // observer finds nothing to re-sync; the port lists this edit
                // moved are stale in exactly the interactions that hold port
                // indices (see [`Self::invalidate_port_interactions`]).
                self.invalidate_port_interactions(cx);
            }
            Err(error) => tracing::warn!(%error, "collapse to subnet refused"),
        }
        cx.notify();
    }

    /// Move the contents of the subnet node `node_id` back into the open
    /// network as one Document undo step, selecting what came out.
    ///
    /// The new selection is read from the graph rather than from the inner
    /// node ids: a node whose id the parent already used is renumbered on the
    /// way out ([`ravel_core::network::extract_subnet`]), so "what is here now
    /// that was not before" is the only description that stays true.
    fn extract_subnet(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        if self.context.is_none() {
            return;
        }
        let before: HashSet<NodeId> = self.graph.node_ids().collect();
        match ravel_core::network::extract_subnet(self.graph.clone(), node_id) {
            Ok(graph) => {
                let extracted: HashSet<NodeId> =
                    graph.node_ids().filter(|id| !before.contains(id)).collect();
                self.selected_edges.clear();
                self.set_selected_nodes(extracted, cx);
                self.commit_graph(graph, None, cx);
                self.invalidate_port_interactions(cx);
            }
            Err(error) => tracing::warn!(%error, "subnet extraction refused"),
        }
        cx.notify();
    }

    // ----- port context menu (REQ-LAYER-002, REQ-LAYER-003) -----------------

    /// Whether the node still declares a `side` port named `name`.
    fn declares_port(&self, node_id: NodeId, side: PortSide, name: &str) -> bool {
        let Some(node) = self.graph.node(node_id) else {
            return false;
        };
        match side {
            PortSide::Input => node.inputs.iter().any(|p| p.name == name),
            PortSide::Output => node.outputs.iter().any(|p| p.name == name),
        }
    }

    /// Drop the interactions that a change to a port list invalidates.
    ///
    /// Both of them are positional. A [`PortHit`] — the source of a wire drag
    /// and its snap target — names a port by index, and removing or reordering
    /// a port shifts every port after it. `Graph::add_edge` validates neither
    /// the index nor the type, so dropping a wire through a stale `PortHit`
    /// does not fail: it writes an edge to a slot nothing reads, and the
    /// evaluator treats the input as unconnected. That silently dead edge is
    /// the exact failure this work exists to remove, so a live wire drag is
    /// cancelled rather than repaired. The rename editor is anchored to canvas
    /// coordinates that the same reindexing moves, so it closes for the same
    /// reason — reanchoring it would still leave it editing whatever port has
    /// since taken the name it was opened on.
    fn invalidate_port_interactions(&mut self, cx: &mut Context<Self>) {
        // Only `Connect` carries port indices; a pan, a box selection or a
        // node move survives an unrelated port edit untouched.
        if matches!(self.drag, DragMode::Connect { .. }) {
            self.drag = DragMode::None;
            cx.notify();
        }
        self.cancel_port_rename(cx);
    }

    /// Open the rename editor for the custom port under the cursor. The
    /// caller focuses the returned Input — a panel never grabs focus on its
    /// own (`.agents/rules/gpui.md`).
    fn begin_port_rename(
        &mut self,
        node_id: NodeId,
        old_name: String,
        center: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        // The menu was built from a snapshot; the node may be gone by now.
        self.graph.node(node_id)?;
        self.port_error = None;
        let input = cx.new(|cx| InputState::new(window, cx).default_value(old_name.clone()));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, state, event: &InputEvent, _window, cx| match event {
                // Enter and blur both commit: leaving the field is the same
                // intent as confirming it (the Outliner's rename rule).
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let name = state.read(cx).value().to_string();
                    this.commit_port_rename(name, cx);
                }
                _ => {}
            },
        );
        self.port_rename = Some(PortRename {
            node_id,
            old_name,
            center,
            input: input.clone(),
            attempted: None,
            sub,
        });
        cx.notify();
        Some(input)
    }

    /// Apply an edited port name as one Document undo step (the commit path
    /// is [`Self::rename_custom_port`]). A blank or unchanged name just
    /// closes the editor — neither is a failure. A refusal keeps the editor
    /// open with the reason beside it, so the name can be corrected without
    /// reopening the menu.
    fn commit_port_rename(&mut self, new_name: String, cx: &mut Context<Self>) {
        let Some(mut rename) = self.port_rename.take() else {
            return;
        };
        cx.notify();
        let new_name = new_name.trim().to_string();
        if new_name.is_empty()
            || new_name == rename.old_name
            || rename.attempted.as_deref() == Some(new_name.as_str())
        {
            self.release_rename_focus(&rename, cx);
            return;
        }
        self.port_error = None;
        let (node_id, old_name) = (rename.node_id, rename.old_name.clone());
        match self.rename_custom_port(node_id, &old_name, &new_name, cx) {
            Ok(()) => self.release_rename_focus(&rename, cx),
            Err(err) => {
                // The editor stays open and keeps the focus it has: the point
                // of leaving it up is that the name can be corrected in place.
                self.port_error = Some(super::port_error_message(&err));
                rename.attempted = Some(new_name);
                self.port_rename = Some(rename);
            }
        }
    }

    /// Abandon a port rename, keeping the port's current name.
    fn cancel_port_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = self.port_rename.take() {
            self.release_rename_focus(&rename, cx);
            cx.notify();
        }
    }

    /// Hand focus back to the canvas when the closing rename editor is what
    /// holds it.
    ///
    /// Same shape as [`Self::dismiss_palette`], and for the same reasons: the
    /// teardown contexts (document observers, network switches) have no
    /// `Window` at hand, so the check goes through the window list, and it is
    /// deferred because that window is often mid-update. The condition is what
    /// keeps this inside the focus-ownership rule — focus returns to the panel
    /// only when the Input being dropped still owns it, so an editor closed
    /// after the user has already clicked into another panel does not pull the
    /// focus back out of it.
    fn release_rename_focus(&self, rename: &PortRename, cx: &mut Context<Self>) {
        let input_focus = rename.input.focus_handle(cx);
        let panel_focus = self.focus_handle.clone();
        cx.defer(move |cx| {
            for handle in cx.windows() {
                let input_focus = input_focus.clone();
                let panel_focus = panel_focus.clone();
                handle
                    .update(cx, move |_, window, cx| {
                        if window.focused(cx).is_some_and(|f| f == input_focus) {
                            panel_focus.focus(window, cx);
                        }
                    })
                    .ok();
            }
        });
    }

    /// Delete the custom port the context menu named (the commit path is
    /// [`Self::remove_custom_port`], so the port, its parameter and its edges
    /// land in one undo step).
    ///
    /// The name comes from the paint-time snapshot the menu was built from,
    /// and the click that runs this can itself have committed an in-flight
    /// rename of that very port first (the menu click blurs the Input). So the
    /// port is looked up again here, **by name**: the user pointed at a name,
    /// not at a slot, and deleting whatever has since moved into that slot
    /// would be a destructive guess. A name that is gone is a no-op — nothing
    /// was destroyed and there is nothing the user needs to be told.
    ///
    /// A port that is still there but refused is a different matter: the menu
    /// disables the item for everything the core rejects, so a refusal means
    /// the graph moved under the open menu, and that is reported.
    fn delete_port_from_menu(&mut self, port: &PortMenuModel, cx: &mut Context<Self>) {
        if !self.declares_port(port.node_id, port.side, &port.name) {
            return;
        }
        self.port_error = None;
        if let Err(err) = self.remove_custom_port(port.node_id, &port.name, cx) {
            self.port_error = Some(super::port_error_message(&err));
        }
        cx.notify();
    }

    fn commit_to_document(
        &mut self,
        graph: Graph,
        key_rename: Option<KeyRename>,
        pin_rename: Option<PinRename>,
        hint: InvalidationHint,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        self.graph = graph.clone();
        self.refresh_graph_caches(cx);
        let (Some(project), Some(context)) = (self.project.clone(), self.context.clone()) else {
            return;
        };
        project.update(cx, |project, cx| {
            // `pin_rename` is what the enclosing subnet node's pin sync needs
            // to read the edit as a rename instead of a delete plus an add.
            let Some(doc) = replace_network_renaming_pin(
                project.document(),
                &context,
                graph,
                pin_rename.as_ref(),
            ) else {
                return;
            };
            // The declarations move with the parameter key in the same
            // snapshot as the graph that moved it: an undo step that carried
            // one without the other would leave the project's external
            // contract naming a parameter nothing has (REQ-PROJ-006).
            let doc = match &key_rename {
                Some(rename) => ravel_core::exposed::apply::follow_key_rename(doc, rename),
                None => doc,
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

    /// The context an expression-driven channel is *displayed* through.
    ///
    /// Only the frame rate and the resolutions are read from it, by the
    /// parameter-expression vocabulary (`fps`, `res.*`, `comp.*`). Nothing
    /// here asks the evaluator for anything, so building it is as cheap as
    /// reading the composition's settings.
    ///
    /// Falls back to a 30 fps, 1×1 context when the panel is not attached to a
    /// composition — which is also when there is no document value to show, so
    /// the fallback is never what a user reads.
    pub fn display_eval_context(&self, cx: &App) -> EvalContext {
        let resolved = (|| {
            let context = self.context.as_ref()?;
            let project = self.project.as_ref()?;
            let document = project.read(cx).document();
            let comp = document.get_composition(context.comp)?;
            let frame = self.current_local_frame(cx).unwrap_or(0);
            Some(EvalContext::new(frame, comp.frame_rate, comp.resolution))
        })();
        resolved.unwrap_or_else(|| EvalContext::new(0, FrameRate::new(30, 1), (1, 1)))
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
        // An identifier parameter is not animatable, and the Properties row
        // hides its toggle for that reason. Refusing here as well is what
        // makes the rule hold for every caller of this method, not just the one
        // that draws the button (`is_identifier_parameter` is the single place
        // that says which parameters those are).
        if ravel_core::composition::validate::is_identifier_parameter(&node.type_key, param_key) {
            return;
        }
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
            // Keying an `Int` re-types it to an `IntChannel` — the same
            // channel a `Float` gets, so the curve is editable the same way —
            // and keeps it one when the last key goes, exactly as a `Channel`
            // stays a `Channel` holding a constant.
            ParameterValue::Int(v) => {
                let mut channel = AnimationChannel::constant(*v as f32);
                insert_channel_key(&mut channel, local_frame);
                ParameterValue::IntChannel(channel)
            }
            ParameterValue::IntChannel(channel) => {
                let mut channel = channel.clone();
                let toggled = if channel_has_key(&channel, local_frame) {
                    remove_channel_key(&mut channel, local_frame)
                } else {
                    insert_channel_key(&mut channel, local_frame)
                };
                if !toggled {
                    return;
                }
                ParameterValue::IntChannel(channel)
            }
            // A string has no channel to hold a constant, so the round trip is
            // one variant wider: keying re-types to `StringSteps` and removing
            // the last key returns to a plain `String`. `StepCurve::keyed`
            // seeds the curve's **default** with the constant the parameter
            // had, and that default is what the return reads — not the key
            // that happened to be removed last. Reading the removed key
            // instead loses the original the moment a second key is edited to
            // something else: key A then key B, remove both, and the parameter
            // would come back holding B.
            ParameterValue::String(v) => {
                ParameterValue::StringSteps(StepCurve::keyed(local_frame, v.clone()))
            }
            ParameterValue::StringSteps(steps) => {
                let mut steps = steps.clone();
                if steps.contains_key(local_frame) {
                    steps.remove(local_frame).expect("key checked above");
                    if steps.is_empty() {
                        ParameterValue::String(steps.default_value().clone())
                    } else {
                        ParameterValue::StringSteps(steps)
                    }
                } else {
                    let held = steps.sample(local_frame as f64).clone();
                    steps.insert(local_frame, held);
                    ParameterValue::StringSteps(steps)
                }
            }
            // Bool stays constant-only in v1 (REQ-LAYER-004), and so do
            // PathPoints / Curve / Ramp.
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
        self.commit_to_document(
            graph,
            None,
            None,
            InvalidationHint::Params(vec![node_id]),
            true,
            cx,
        );
        // Refresh the properties snapshot so the key-toggle state re-renders.
        self.notify_properties_selection(cx);
        cx.notify();
    }

    /// Attach or detach an expression on every channel of `param_key`
    /// (REQ-CORE-014).
    ///
    /// Attaching seeds each component with the value it already shows, so
    /// reaching for an expression does not move the picture; detaching freezes
    /// the value that is on screen. One `commit_to_document`, so one undo step
    /// per click — the same contract as the keyframe stopwatch beside it.
    pub fn toggle_param_expression(
        &mut self,
        node_id: NodeId,
        param_key: &str,
        cx: &mut Context<Self>,
    ) {
        let frame = self.current_local_frame(cx).unwrap_or(0) as f64;
        let eval = self.display_eval_context(cx);
        let edited = {
            let Some(node) = self.graph.node(node_id) else {
                return;
            };
            let Some(param) = node.parameters.iter().find(|p| p.key == param_key) else {
                return;
            };
            if expression::has_expression(&param.value) {
                expression::detach(&param.value, frame, &eval)
            } else {
                // `None` when no component can take an expression without
                // destroying what drives it; the panel greys the badge for
                // exactly that case, so there is nothing to commit here.
                expression::attach(&param.value)
            }
        };
        let Some(edited) = edited else {
            return;
        };
        self.replace_param_value(node_id, param_key, edited, cx);
    }

    /// Store `source` on one component of `param_key`.
    ///
    /// **Written whether or not it compiles.** `ParameterExpression` keeps the
    /// text of a source it could not compile and persists only that text, so a
    /// half-typed expression survives the edit, a save and a reload; the error
    /// is what the row displays. Refusing the commit here would delete the
    /// author's work at the one moment they are most likely to be mid-word.
    pub fn set_param_expression(
        &mut self,
        node_id: NodeId,
        param_key: &str,
        component: usize,
        source: &str,
        cx: &mut Context<Self>,
    ) {
        let edited = {
            let Some(node) = self.graph.node(node_id) else {
                return;
            };
            let Some(param) = node.parameters.iter().find(|p| p.key == param_key) else {
                return;
            };
            if expression::component_expression(&param.value, component)
                .is_some_and(|existing| existing.source() == source)
            {
                return;
            }
            expression::set_source(&param.value, component, source)
        };
        let Some(edited) = edited else {
            return;
        };
        self.replace_param_value(node_id, param_key, edited, cx);
    }

    /// Write one parameter back into the graph and commit it as a single undo
    /// step.
    fn replace_param_value(
        &mut self,
        node_id: NodeId,
        param_key: &str,
        value: ParameterValue,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.graph.node(node_id) else {
            return;
        };
        let mut updated = (**node).clone();
        let Some(parameter) = updated.parameters.iter_mut().find(|p| p.key == param_key) else {
            return;
        };
        parameter.value = value;
        let graph = self.graph.clone().replace_node(Arc::new(updated));
        self.commit_to_document(
            graph,
            None,
            None,
            InvalidationHint::Params(vec![node_id]),
            true,
            cx,
        );
        self.notify_properties_selection(cx);
        cx.notify();
    }

    /// Applies a property edit called directly by the Properties panel.
    ///
    /// Numeric values are clamped to the parameter's hard range (registry
    /// metadata). Channel-backed parameters keep their channel: a constant
    /// channel updates its constant, a keyframed channel gets a key at the
    /// current layer-local frame (REQ-LAYER-004). Live edits
    /// (`commit == false`, e.g. mid-scrub) update the document without
    /// recording undo; the gesture-ending `commit == true` call records one
    /// Document undo step for the whole edit.
    pub(crate) fn apply_property_change(
        &mut self,
        node_ids: &[NodeId],
        key: &str,
        value: &ravel_ui::properties::PropertyValue,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let local_frame = self.current_local_frame(cx);
        let mut graph = self.graph.clone();
        let mut touched = false;
        for node_id in node_ids {
            let Some(node) = graph.node(*node_id) else {
                continue;
            };
            let range = self.registry.param_range(&node.type_key, key);
            let param_value = {
                let Some(param) = node.parameters.iter().find(|p| p.key == key) else {
                    continue;
                };
                let Some(value) = edited_param_value(&param.value, value, range, local_frame)
                else {
                    continue;
                };
                value
            };
            // One command writes the edit and everything it forces —
            // `attribute.set`'s `value` is reshaped when its `type` changes —
            // so the Document snapshot stays a single undo step and the node
            // is never committed half-converted.
            let changed = ravel_core::graph::Parameter {
                key: key.to_string(),
                value: param_value,
            };
            let mut updates =
                ravel_core::registry::builtin::dependent_param_updates(node, &changed);
            updates.insert(0, changed);
            let next = match graph.clone().set_params(*node_id, &updates) {
                Ok(next) => next,
                Err(err) => {
                    // Dropping the edit silently would look like the panel
                    // ignored the user; the surrounding loop still applies the
                    // other selected nodes' edits.
                    tracing::warn!(node = ?node_id, %key, %err, "parameter edit not applied");
                    continue;
                }
            };
            touched = true;
            graph = next;
        }
        if !touched {
            return;
        }

        self.commit_to_document(
            graph,
            None,
            None,
            InvalidationHint::Params(node_ids.to_vec()),
            commit,
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

    fn copy_selected(&mut self, cx: &App) {
        let sel = Self::selected_nodes(cx);
        let Some(content) = self.content_for_nodes(sel.iter().copied()) else {
            return;
        };
        self.clipboard = Some(content);
    }

    fn content_for_nodes(
        &self,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Option<ClipboardContent> {
        let ids = Self::editable_targets(&self.graph, nodes);
        if ids.is_empty() {
            return None;
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
        Some(ClipboardContent { nodes, edges })
    }

    fn paste(&mut self, offset: (f32, f32), cx: &mut Context<Self>) {
        if self.context.is_none() {
            return;
        }
        let content = match &self.clipboard {
            Some(c) => c.clone(),
            None => return,
        };
        self.paste_content(content, offset, cx);
    }

    fn paste_content(
        &mut self,
        content: ClipboardContent,
        offset: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if self.context.is_none() {
            return;
        }

        // Fresh ids for the whole hierarchy, not just the pasted nodes: a
        // subnet node clones its inner `Arc<Graph>`, and an inner node that
        // kept its id would share the evaluator's one processor entry (and its
        // cache path) with the node it was copied from.
        let (copies, id_map) = Graph::duplicate_nodes_with_fresh_ids(&content.nodes);
        let mut graph = self.graph.clone();

        for (z, mut new_node) in (Self::next_z(&graph)..).zip(copies) {
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

        // Only the pasted nodes themselves: `id_map` also carries the inner
        // nodes of every pasted subnet, which live in another graph and are
        // not selectable here.
        let new_sel: HashSet<NodeId> = content
            .nodes
            .iter()
            .filter_map(|node| id_map.get(&node.id).copied())
            .collect();
        self.set_selected_nodes(new_sel, cx);
        self.commit_graph(graph, None, cx);
    }

    fn duplicate_selected(&mut self, cx: &mut Context<Self>) {
        let sel = Self::selected_nodes(cx);
        let Some(content) = self.content_for_nodes(sel.iter().copied()) else {
            return;
        };
        self.paste_content(content, (20.0, 20.0), cx);
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let sel = Self::selected_nodes(cx);
        let nodes = Self::editable_targets(&self.graph, sel.iter().copied());
        if nodes.is_empty() && self.selected_edges.is_empty() {
            return;
        }

        let edges: Vec<_> = self.selected_edges.iter().copied().collect();
        let graph = edges
            .into_iter()
            .fold(self.graph.clone(), |graph, edge_id| {
                remove_edge_and_compact(graph.clone(), edge_id).unwrap_or(graph)
            });
        let graph = nodes.into_iter().fold(graph, |graph, node_id| {
            remove_node_and_compact(graph.clone(), node_id).unwrap_or(graph)
        });
        self.clear_selected_nodes(cx);
        self.selected_edges.clear();
        self.commit_graph(graph, None, cx);
    }

    fn trace_action(cx: &mut App, command: CommandId, outcome: &str) {
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::PanelKeyDown,
                command: Some(command),
                focused_instance: crate::trace::focused_instance(cx),
                handler: "NodeEditorPanel::on_action",
                outcome: Some(outcome.to_string()),
            },
        );
    }

    fn on_copy(&mut self, _: &EditCopy, _window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selected(cx);
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

    fn on_collapse_to_subnet(
        &mut self,
        _: &NodeCollapseToSubnet,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<NodeId> = Self::selected_nodes(cx).into_iter().collect();
        self.collapse_to_subnet(&targets, cx);
        Self::trace_action(cx, CommandId::NodeCollapseToSubnet, "collapse_to_subnet");
    }

    fn on_extract_subnet(
        &mut self,
        _: &NodeExtractSubnet,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<NodeId> = Self::selected_nodes(cx).into_iter().collect();
        let Some(node_id) = subnet_menu_model(&self.graph, &targets).extract else {
            return;
        };
        self.extract_subnet(node_id, cx);
        Self::trace_action(cx, CommandId::NodeExtractSubnet, "extract_subnet");
    }

    fn on_auto_layout(&mut self, _: &NodeAutoLayout, _window: &mut Window, cx: &mut Context<Self>) {
        self.auto_layout_nodes(cx);
        Self::trace_action(cx, CommandId::NodeAutoLayout, "auto_layout_nodes");
        cx.notify();
    }

    /// Lay the selected nodes out in layers — the whole network when nothing
    /// is selected — as one structural Document undo step.
    ///
    /// Nothing calls this but the command: `NodeMetadata::position` is saved
    /// data, and a collapse, an extract or a node insertion must not silently
    /// rewrite where the user put things (`node-graph-readability-plan.md`).
    /// A layout that moves nothing records no undo step, for the same reason
    /// [`Self::set_bypass`] does not.
    fn auto_layout_nodes(&mut self, cx: &mut Context<Self>) {
        // `node_sizes` is measured in screen pixels; positions live in network
        // coordinates, which is what `fit_view` un-zooms for the same reason.
        let zoom = self.viewport.zoom;
        let sizes = self
            .node_sizes
            .iter()
            .map(|(&id, &(w, h))| (id, (w / zoom, h / zoom)))
            .collect();
        let positions = auto_layout(
            &self.graph,
            &Self::selected_nodes(cx),
            &sizes,
            LayoutAxis::Horizontal,
        );

        let mut changed = false;
        let graph = positions
            .into_iter()
            .fold(self.graph.clone(), |graph, (id, position)| {
                let Some(node) = graph.node(id) else {
                    return graph;
                };
                if node.metadata.position == position {
                    return graph;
                }
                let mut moved = (**node).clone();
                moved.metadata.position = position;
                changed = true;
                graph.replace_node(Arc::new(moved))
            });
        if changed {
            self.commit_graph(graph, None, cx);
        }
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
            self.commit_graph(graph, None, cx);
        }
    }

    /// Publish the current selection to the Properties panel. The target
    /// only identifies the network and node ids; the panel resolves live
    /// values (and driven state) from the document. The Viewer is
    /// untouched: it always shows the root composition output
    /// (REQ-LAYER-007).
    ///
    /// With nothing selected the panel only withdraws its *own* target: a
    /// `Layer` target belongs to the layer-selection writers (see
    /// `panels::set_layer_selection`), and opening a network as a consequence
    /// of a layer being selected must not blank the Properties panel that same
    /// selection just filled.
    ///
    /// The ids are published in ascending id order, the order the Viewer's own
    /// selection publisher already uses. The canvas selection is a
    /// [`HashSet`], so its iteration order is an artifact of the hasher: the
    /// consumers that follow `ids.first()` — Properties for the keyframe
    /// target, the scoped evaluation target an inspection panel declares —
    /// would otherwise name a different node from run to run, and a different
    /// one again depending on which panel published the selection.
    fn notify_properties_selection(&self, cx: &mut App) {
        let sel = Self::selected_nodes(cx);
        let target = match &self.context {
            Some(network) if !sel.is_empty() => {
                let mut ids: Vec<_> = sel.into_iter().collect();
                ids.sort_by_key(|id| id.raw());
                super::PropertiesTarget::Nodes {
                    network: network.clone(),
                    ids,
                }
            }
            _ => {
                let owned = matches!(
                    cx.try_global::<super::SelectedPropertiesTarget>()
                        .map(|t| &t.0),
                    None | Some(super::PropertiesTarget::Nodes { .. })
                );
                if !owned {
                    return;
                }
                super::PropertiesTarget::Empty
            }
        };
        cx.set_global(super::SelectedPropertiesTarget(target));
    }

    /// Adopt `style` and remember it.
    ///
    /// The choice is a preference rather than a property of this panel, this
    /// window or this project, so it is written to the global settings layer
    /// and read back by whatever node editor is built next
    /// (`node-graph-readability-plan.md`, `NGR-3`). The field is kept in step
    /// so the current panel repaints without waiting to be rebuilt.
    fn set_edge_style(&mut self, style: EdgeStyle, cx: &mut Context<Self>) {
        self.edge_style = style;
        crate::app_settings::update(
            crate::app_settings::SettingsScope::Global,
            |layer| layer.node_editor.edge_style = Some(style),
            cx,
        );
        cx.notify();
    }

    fn refresh_node_sizes(&mut self) {
        self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
    }

    /// Rebuild everything derived from the displayed graph. Called from the
    /// two places `self.graph` is replaced with a non-empty graph
    /// ([`Self::refresh_from_document`] and [`Self::commit_to_document`]);
    /// [`Self::close_network`] clears the same four maps. `render()` never
    /// rebuilds them — it clones what is already there — so the per-node
    /// registry and locale lookups cost one pass per graph change instead of
    /// one per frame.
    ///
    /// [`Self::displayed_timings`] is rebuilt here too, which is what makes
    /// it a function of (graph, timings global) at all times: leaving the
    /// previous graph's readouts in place would show nothing for a node that
    /// just appeared and — since node ids are reused across networks and
    /// documents — another node's measurement for one that did not.
    ///
    /// Only [`Self::node_sizes`] also depends on the zoom, which is why
    /// [`Self::refresh_node_sizes`] stays separate for viewport changes.
    ///
    /// The labels are localized at build time. `ravel_i18n::set_locale` is
    /// called once at startup (`main.rs`) and there is no runtime language
    /// switch, so nothing invalidates them today; a future switch has to
    /// call this (see the note in `node_locale`).
    fn refresh_graph_caches(&mut self, cx: &App) {
        self.node_sizes = Self::compute_all_sizes(&self.graph, self.viewport.zoom);
        // Nodes without a registered template (and synthetic ones) get no
        // header tint, so they are simply absent from the map.
        self.node_categories = self
            .graph
            .nodes()
            .filter_map(|n| {
                self.registry
                    .get(&n.type_key)
                    .map(|template| (n.id, template.category))
            })
            .collect();
        // A user rename wins over the locale entry; the paint path only
        // looks the map up.
        self.node_labels = self
            .graph
            .nodes()
            .map(|n| (n.id, crate::node_locale::display_label(n, &self.registry)))
            .collect();
        self.displayed_timings = Self::collect_readouts(&self.graph, cx);
    }

    /// Load readouts for the nodes of `graph`, from the published timings
    /// global. Reducing to [`EvalReadout`] here is what lets the observer
    /// compare at the grain the canvas actually draws.
    ///
    /// Only the nodes that are actually drawn with a readout are collected:
    /// [`painting::paint_nodes`] skips synthetic nodes and hides the readout
    /// of a bypassed one (the pass-through records no timing), so including
    /// them would repaint for a change nobody can see.
    ///
    /// This runs on every published evaluation result — once a frame during
    /// playback — so the readout texts are written into one reused buffer
    /// instead of a `String` per node.
    fn collect_readouts(graph: &Graph, cx: &App) -> HashMap<NodeId, EvalReadout> {
        let Some(all) = cx.try_global::<crate::project_state::NodeEvalTimings>() else {
            return HashMap::new();
        };
        let mut scratch = String::new();
        graph
            .nodes()
            .filter(|node| !node.metadata.synthetic && !node.metadata.bypassed)
            .filter_map(|node| {
                let value = all.0.get(&node.id)?;
                Some((node.id, EvalReadout::written(*value, &mut scratch)))
            })
            .collect()
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

    /// The frontmost port at the point, unless a higher-painted node body
    /// occludes it. Ports still win over their own node body.
    fn port_at_local_pos(&self, lx: f32, ly: f32) -> Option<PortHit> {
        Self::port_hit_at(&self.graph, &self.viewport, &self.node_sizes, lx, ly)
    }

    fn pointer_hint_at(&self, lx: f32, ly: f32) -> PointerHint {
        Self::pointer_hint_at_in(
            &self.graph,
            &self.viewport,
            &self.node_sizes,
            self.edge_style,
            lx,
            ly,
        )
    }

    fn pointer_hint_at_in(
        graph: &Graph,
        viewport: &Viewport,
        node_sizes: &HashMap<NodeId, (f32, f32)>,
        edge_style: EdgeStyle,
        lx: f32,
        ly: f32,
    ) -> PointerHint {
        if Self::port_hit_at(graph, viewport, node_sizes, lx, ly).is_some() {
            PointerHint::Port
        } else if Self::node_hit_at(graph, viewport, node_sizes, lx, ly).is_some() {
            PointerHint::Node
        } else if painting::edge_at_local_pos(graph, viewport, lx, ly, 5.0, edge_style).is_some() {
            PointerHint::Edge
        } else {
            PointerHint::Empty
        }
    }

    fn update_pointer_hint(&mut self, next: PointerHint, cx: &mut Context<Self>) {
        if let Some(next) = pointer_hint_transition(
            self.pointer_hint,
            next,
            !matches!(self.drag, DragMode::None),
        ) {
            self.pointer_hint = next;
            cx.notify();
        }
    }

    /// Arm the hover-dwell timer for the current hover generation. A timer
    /// that fires after the hover moved on — or after a gesture started —
    /// reports a stale generation and does nothing.
    fn arm_hover_dwell(&mut self, cx: &mut Context<Self>) {
        let generation = self.hover_popover.generation();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DWELL).await;
            this.update(cx, |this, cx| {
                if matches!(this.drag, DragMode::None)
                    && this.hover_popover.dwell_elapsed(generation)
                {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn port_hit_at(
        graph: &Graph,
        viewport: &Viewport,
        node_sizes: &HashMap<NodeId, (f32, f32)>,
        lx: f32,
        ly: f32,
    ) -> Option<PortHit> {
        let port = painting::port_at_local_pos(graph, viewport, lx, ly)?;
        let Some(body) = Self::node_hit_at(graph, viewport, node_sizes, lx, ly) else {
            return Some(port);
        };
        if body == port.node_id {
            return Some(port);
        }

        let order: Vec<_> = painting::z_ordered(graph)
            .into_iter()
            .map(|node| node.id)
            .collect();
        let body_rank = order.iter().position(|id| *id == body)?;
        let port_rank = order.iter().position(|id| *id == port.node_id)?;
        (port_rank > body_rank).then_some(port)
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

    /// Where a keyboard-opened overlay belongs: the last pointer position over
    /// the canvas, falling back to the center when there is none or it no
    /// longer lands inside.
    fn pointer_or_canvas_center(&self) -> (f32, f32) {
        let (w, h) = self.canvas_size.get();
        self.last_pointer
            .filter(|&(x, y)| (0.0..=w).contains(&x) && (0.0..=h).contains(&y))
            .unwrap_or((w * 0.5, h * 0.5))
    }

    fn local_from_event(&self, pos: Point<Pixels>) -> (f32, f32) {
        let origin = self.canvas_origin.get();
        let mx: f32 = pos.x.into();
        let my: f32 = pos.y.into();
        (mx - origin.0, my - origin.1)
    }

    /// Center of the composition owning the edited network, in pixels.
    fn comp_center(&self, cx: &Context<Self>) -> Option<(f32, f32)> {
        let context = self.context.as_ref()?;
        let project = self.project.as_ref()?;
        let comp = project.read(cx).document().get_composition(context.comp)?;
        let (w, h) = comp.resolution;
        Some((w as f32 * 0.5, h as f32 * 0.5))
    }

    /// New shape generators start at the composition center instead of the
    /// registry default `(0, 0)`. Only freshly created nodes are affected;
    /// registry templates and existing documents keep their values.
    fn apply_default_shape_center(&self, node: &mut Node, cx: &Context<Self>) {
        let Some(center) = self.comp_center(cx) else {
            return;
        };
        ravel_ui::document::apply_shape_center_default(node, center);
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
            self.apply_default_shape_center(&mut node, cx);
            let (fx, fy) = self.viewport.screen_to_flow(local.0, local.1);
            node.metadata.position = (fx, fy);
            node.metadata.z = Self::next_z(&self.graph);
            if let Ok(new_graph) = self.graph.clone().add_node(node) {
                self.commit_graph(new_graph, None, cx);
                self.record_recent_type(type_key);
            }
        }
    }

    /// Remembers `type_key` as most recently used (driving the search
    /// palette's recency ranking); the list is capped and session-only.
    fn record_recent_type(&mut self, type_key: &str) {
        const MAX_RECENT_TYPES: usize = 10;
        self.recent_types.retain(|key| key != type_key);
        self.recent_types.insert(0, type_key.to_string());
        self.recent_types.truncate(MAX_RECENT_TYPES);
    }

    // ----- node search palette (DISC-3) --------------------------------------

    /// Opens the search palette. `from` is the dragged port when invoked
    /// from a wire drop — then only connectable types are offered. `local`
    /// is the canvas-local point the new node is placed at; `anchor` the
    /// window-space position of the overlay.
    fn open_search_palette(
        &mut self,
        from: Option<PortHit>,
        local: (f32, f32),
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context.is_none() {
            return;
        }
        let mut candidates = crate::node_editor::palette::search_candidates(&self.registry);
        if let Some(port) = &from {
            candidates = retain_connectable(candidates, &self.registry, &self.graph, port);
        }
        let palette =
            cx.new(|cx| SearchPalette::new(candidates, self.recent_types.clone(), window, cx));
        let event_sub = cx.subscribe_in(
            &palette,
            window,
            |this, _palette, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Accept(type_key) => {
                    let type_key = type_key.clone();
                    this.accept_palette(&type_key, window, cx);
                }
                PaletteEvent::Dismiss => this.dismiss_palette(cx),
            },
        );
        palette.update(cx, |palette, cx| palette.focus_input(window, cx));
        self.palette = Some(PaletteOpen {
            palette,
            from,
            local,
            anchor,
            event_sub,
        });
        cx.notify();
    }

    /// Accepts the palette's pick: the same document change as the
    /// equivalent context-menu or edge-drop-menu pick (one undo step).
    fn accept_palette(&mut self, type_key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.palette.take() else {
            return;
        };
        self.focus_handle.focus(window, cx);
        match open.from {
            Some(from) => self.add_node_from_edge_drop(type_key, from, open.local, cx),
            None => self.add_node_from_template(type_key, open.local, cx),
        }
        cx.notify();
    }

    /// Drops the open palette, if any, moving focus back to the canvas when
    /// the palette actually holds it — the check goes through the window
    /// list because the teardown contexts that need this (document
    /// observers, network switches) have no `Window` at hand, and a palette
    /// left open in another window must not steal focus. This is the single
    /// teardown path; `accept_palette` takes the placement context out of
    /// [`PaletteOpen`] first and restores focus with its own `Window`.
    /// Nothing of the palette survives: a fresh entity is built on the next
    /// open.
    fn dismiss_palette(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.palette.take() else {
            return;
        };
        let palette_focus = open.palette.read(cx).input_focus_handle(cx);
        let panel_focus = self.focus_handle.clone();
        // Refocusing needs a `Window`, and this teardown often runs while
        // that very window is mid-update (event handlers, observers), where
        // a nested window update would fail — so it is deferred to the end
        // of the current update.
        cx.defer(move |cx| {
            for handle in cx.windows() {
                let palette_focus = palette_focus.clone();
                let panel_focus = panel_focus.clone();
                handle
                    .update(cx, move |_, window, cx| {
                        // Only take focus back when the palette actually
                        // holds it; a palette left open in another window
                        // must not steal that window's focus.
                        if window.focused(cx).is_some_and(|f| f == palette_focus) {
                            panel_focus.focus(window, cx);
                        }
                    })
                    .ok();
            }
        });
        cx.notify();
    }

    /// Tab in the node editor toggles the palette under the pointer.
    ///
    /// The double-click path opens the same palette at the click, so a fixed
    /// canvas center made one palette appear in two places and put the node it
    /// places somewhere the hand was not (`MED-APP-27`). Before the pointer
    /// has ever been over the canvas there is no better answer than the
    /// center.
    fn on_search_palette(
        &mut self,
        _: &NodeSearchPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            self.dismiss_palette(cx);
            return;
        }
        let local = self.pointer_or_canvas_center();
        let origin = self.canvas_origin.get();
        self.open_search_palette(
            None,
            local,
            point(px(origin.0 + local.0), px(origin.1 + local.1)),
            window,
            cx,
        );
    }

    /// Places the selected template and connects its first compatible port.
    /// If the template has no compatible port, the node is still placed
    /// without a connection, matching edge-drop menu behavior.
    fn add_node_from_edge_drop(
        &mut self,
        type_key: &str,
        from: PortHit,
        local: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if self.context.is_none() {
            cx.notify();
            return;
        }
        let Some(mut node) = self.registry.create_node(type_key, NodeId::next()) else {
            cx.notify();
            return;
        };
        self.apply_default_shape_center(&mut node, cx);
        let (fx, fy) = self.viewport.screen_to_flow(local.0, local.1);
        node.metadata.position = (fx, fy);
        node.metadata.z = Self::next_z(&self.graph);
        let compatible_port = first_compatible_port(&self.graph, &from, &node);
        let new_node_id = node.id;
        let Ok(mut graph) = self.graph.clone().add_node(node) else {
            cx.notify();
            return;
        };

        if let Some(port_index) = compatible_port {
            let (source, source_port, target, target_port) = if from.is_output {
                (
                    from.node_id,
                    OutputPortIndex(from.port_index),
                    new_node_id,
                    InputPortIndex(port_index),
                )
            } else {
                (
                    new_node_id,
                    OutputPortIndex(port_index),
                    from.node_id,
                    InputPortIndex(from.port_index),
                )
            };
            if let Some(connected) = connect_edge_and_update_variadic_inputs(
                graph.clone(),
                EdgeId::next(),
                source,
                source_port,
                target,
                target_port,
            ) {
                graph = connected;
            }
        }

        self.commit_graph(graph, None, cx);
        self.record_recent_type(type_key);
        cx.notify();
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
            .overflow_hidden()
            .bg(colors.tab_bar)
            .border_b_1()
            .border_color(colors.border)
            .text_xs();

        if crumbs.is_empty() {
            return bar;
        }

        let last = crumbs.len() - 1;
        for (i, (label, depth)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(
                    div()
                        .flex_shrink_0()
                        .text_color(colors.muted_foreground)
                        .child(SharedString::from("/")),
                );
            }
            let color = if i == last {
                colors.foreground
            } else {
                colors.muted_foreground
            };
            // A crumb is a composition or node name: keep the trail on one
            // line and ellipsize the crumbs that no longer fit.
            let mut crumb = div()
                .id(SharedString::from(format!("crumb-{i}")))
                .min_w_0()
                .truncate()
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

impl Focusable for NodeEditorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NodeEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let graph = self.graph.clone();
        let viewport = self.viewport;
        let selected = Self::selected_nodes(cx);
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
        let canvas_cursor = self.pointer_hint.cursor();
        let active_drag_cursor = drag_cursor(&self.drag);

        let entity = cx.entity().downgrade();
        let add_node_menu = self.add_node_menu.clone();
        // Per-node load readouts, header tints and labels: all three are
        // functions of the displayed graph and are rebuilt when it changes
        // (`refresh_graph_caches`), so a repaint only clones them.
        let timings = self.displayed_timings.clone();
        let categories = self.node_categories.clone();
        let labels = self.node_labels.clone();

        let breadcrumb = self.build_breadcrumb_bar(cx);

        // Hover info popover (DISC-2): always rendered, open or closed, so
        // the keyed PopoverState survives across frames. The zero-size
        // trigger anchors the deferred content below the hovered node
        // without adding a canvas hit target, and the content is rebuilt
        // from the document on every repaint, so animated values follow the
        // displayed frame without any evaluation request.
        let hover_popover = match self
            .hover_popover
            .open_target()
            .and_then(|id| self.graph.node(id))
        {
            Some(node) => {
                let frame = self.current_local_frame(cx).unwrap_or(0);
                let eval = self.display_eval_context(cx);
                let info = hover_info(node, &self.registry, frame, &eval);
                let (sx, sy) = self
                    .viewport
                    .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
                let (_, h) = self
                    .node_sizes
                    .get(&node.id)
                    .copied()
                    .unwrap_or((node_width(self.viewport.zoom), 60.0));
                hover_popover_element(Some(&info), point(px(sx), px(sy + h)), true, cx)
            }
            None => hover_popover_element(None, point(px(0.0), px(0.0)), false, cx),
        };

        // With no network open, say *why*: a multi-layer selection is a closed
        // state the user asked for, not a missing selection (REQ-UI-013).
        let no_network = self.context.is_none().then(|| {
            let message = if super::layer_selection(cx).layers().len() > 1 {
                t!("node_graph.multiple_layers")
            } else {
                t!("node_graph.no_network")
            };
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(colors.muted_foreground)
                .child(SharedString::from(message))
        });

        // A refused custom-port edit says why, in the canvas corner. The node
        // editor has no section footer to put the reason under (the
        // Properties Ports list does), and the message belongs to the panel
        // rather than to any one node, so it sits over the canvas until the
        // next port edit or the next context menu replaces it.
        let port_notice = self.port_error.clone().map(|message| {
            div()
                .absolute()
                .left_2()
                .bottom_2()
                .px_2()
                .py_1()
                .bg(colors.popover)
                .border_1()
                .border_color(colors.border)
                .rounded_md()
                .shadow_sm()
                .text_xs()
                .text_color(colors.danger)
                .child(message)
        });

        let canvas_area = div()
            .id("node-editor-canvas")
            .relative()
            .flex_grow()
            .overflow_hidden()
            .cursor(canvas_cursor)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let (lx, ly) = this.local_from_event(event.position);
                    this.hover_popover.cancel();

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

                    // Double-click on empty canvas opens the node search
                    // palette (DISC-3); the first click of the pair may have
                    // started a pan, which is abandoned here.
                    if event.click_count == 2
                        && this.port_at_local_pos(lx, ly).is_none()
                        && this.node_at_local_pos(lx, ly).is_none()
                        && painting::edge_at_local_pos(
                            &this.graph,
                            &this.viewport,
                            lx,
                            ly,
                            5.0,
                            this.edge_style,
                        )
                        .is_none()
                    {
                        this.drag = DragMode::None;
                        this.open_search_palette(None, (lx, ly), event.position, window, cx);
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

                    if let Some(port_hit) = this.port_at_local_pos(lx, ly) {
                        this.drag = DragMode::Connect {
                            from: port_hit.clone(),
                            to_point: (lx, ly),
                            snap: None,
                        };
                        cx.notify();
                        return;
                    }

                    if let Some(node_id) = this.node_at_local_pos(lx, ly) {
                        let mut sel = Self::selected_nodes(cx);
                        if !event.modifiers.shift && !sel.contains(&node_id) {
                            sel.clear();
                        }
                        this.selected_edges.clear();
                        sel.insert(node_id);
                        this.set_selected_nodes(sel.clone(), cx);
                        this.notify_properties_selection(cx);

                        this.graph = Self::raised_to_front(&this.graph, &sel);

                        let origins: Vec<_> = sel
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
                    } else if let Some(edge_id) = painting::edge_at_local_pos(
                        &this.graph,
                        &this.viewport,
                        lx,
                        ly,
                        5.0,
                        this.edge_style,
                    ) {
                        if !event.modifiers.shift {
                            this.selected_edges.clear();
                            this.clear_selected_nodes(cx);
                        }
                        this.selected_edges.insert(edge_id);
                        this.notify_properties_selection(cx);
                    } else if event.modifiers.shift {
                        this.drag = DragMode::SelectBox {
                            start: (lx, ly),
                            current: (lx, ly),
                        };
                    } else {
                        this.clear_selected_nodes(cx);
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
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (lx, ly) = this.local_from_event(event.position);
                    this.hover_popover.cancel();
                    this.drag = DragMode::Pan {
                        start_mouse: (lx, ly),
                        start_viewport: (this.viewport.x, this.viewport.y),
                    };
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (lx, ly) = this.local_from_event(event.position);
                    if this.hover_popover.cancel() {
                        cx.notify();
                    }
                    // The refusal notice belongs to the edit that was just
                    // attempted; opening the menu again starts a new one.
                    if this.port_error.take().is_some() {
                        cx.notify();
                    }
                    this.last_right_click.set((lx, ly));
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    let drag = this.drag.clone();
                    match &drag {
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

                            this.connect_ports(src_node, src_port, tgt_node, tgt_port, cx);
                        }
                        DragMode::Connect {
                            from, snap: None, ..
                        } => {
                            let (lx, ly) = this.local_from_event(event.position);
                            let empty = this.port_at_local_pos(lx, ly).is_none()
                                && this.node_at_local_pos(lx, ly).is_none()
                                && painting::edge_at_local_pos(
                                    &this.graph,
                                    &this.viewport,
                                    lx,
                                    ly,
                                    5.0,
                                    this.edge_style,
                                )
                                .is_none();
                            if empty {
                                // A wire dropped on empty canvas opens the
                                // search palette offering only types that can
                                // connect to the dragged port (DISC-3).
                                this.open_search_palette(
                                    Some(from.clone()),
                                    (lx, ly),
                                    event.position,
                                    window,
                                    cx,
                                );
                            }
                        }
                        DragMode::MoveNodes { moved: true, .. } => {
                            this.commit_graph(this.graph.clone(), None, cx);
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
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.drag = DragMode::None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let (lx, ly) = this.local_from_event(event.position);
                this.last_pointer = Some((lx, ly));

                // Gestures suppress the hover popover (DISC-2); the drag
                // branches below repaint anyway.
                if !matches!(this.drag, DragMode::None) {
                    this.hover_popover.cancel();
                }

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
                        let mut sel = HashSet::new();
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
                                sel.insert(node.id);
                            }
                        }
                        this.publish_band_selection(sel, cx);
                        // The rectangle itself moved even when its contents
                        // did not.
                        cx.notify();
                    }
                    DragMode::None => {
                        let hint = this.pointer_hint_at(lx, ly);
                        this.update_pointer_hint(hint, cx);

                        // Idle hover tracking: re-arm the dwell when the
                        // hovered node changes; repaint when an open popover
                        // just closed.
                        let hovered = this.node_at_local_pos(lx, ly);
                        let (repaint, arm) = this.hover_popover.pointer_moved(hovered);
                        if arm {
                            this.arm_hover_dwell(cx);
                        }
                        if repaint {
                            cx.notify();
                        }
                    }
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                let (lx, ly) = this.local_from_event(event.position);
                // The view moves under the pointer: the hover anchor is stale.
                this.hover_popover.cancel();

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
                this.hover_popover.cancel();
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
                let selected_snap = Self::selected_nodes(cx);
                let es = self.edge_style;
                move |menu, window, cx| {
                    let (lx, ly) = right_click.get();
                    let hit_edge =
                        painting::edge_at_local_pos(&graph_snap, &vp_snap, lx, ly, 5.0, es);
                    let hit_node =
                        NodeEditorPanel::node_hit_at(&graph_snap, &vp_snap, &sizes_snap, lx, ly);
                    let hit_port =
                        NodeEditorPanel::port_hit_at(&graph_snap, &vp_snap, &sizes_snap, lx, ly);

                    let entity_add = entity.clone();
                    let groups = add_node_menu.clone();
                    let mut menu = menu.submenu(
                        t!("panel.node_graph_menu.add_node"),
                        window,
                        cx,
                        move |sub, window, cx| {
                            groups.iter().fold(sub, |sub, group| {
                                let items = group.items.clone();
                                let category = group.category;
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
                                                .icon(Icon::new(RavelIcon::for_node_type(
                                                    &item.type_key,
                                                    Some(category),
                                                )))
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

                    // Port operations sit above the node ones: the cursor is
                    // on a port, which is also on a node, and the narrower
                    // target reads first.
                    if let Some(hit) = hit_port
                        && let Some(port) = port_menu_model(&graph_snap, &hit)
                    {
                        let center = hit.center;
                        let entity_rename = entity.clone();
                        let rename_port = port.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.rename_port"))
                                .disabled(!rename_port.enabled)
                                .on_click(move |_, window, cx| {
                                    entity_rename
                                        .update(cx, |this, cx| {
                                            // Focus belongs to the click, not
                                            // to the panel's own construction.
                                            if let Some(input) = this.begin_port_rename(
                                                rename_port.node_id,
                                                rename_port.name.clone(),
                                                center,
                                                window,
                                                cx,
                                            ) {
                                                input.update(cx, |state, cx| {
                                                    state.focus(window, cx);
                                                });
                                            }
                                        })
                                        .ok();
                                }),
                        );

                        let entity_delete = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(t!("panel.node_graph_menu.delete_port"))
                                .disabled(!port.enabled)
                                .on_click(move |_, _window, cx| {
                                    entity_delete
                                        .update(cx, |this, cx| {
                                            this.delete_port_from_menu(&port, cx);
                                        })
                                        .ok();
                                }),
                        );
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
                                            let graph = del_targets.iter().fold(
                                                this.graph.clone(),
                                                |g, nid| {
                                                    remove_node_and_compact(g.clone(), *nid)
                                                        .unwrap_or(g)
                                                },
                                            );
                                            this.clear_selected_nodes(cx);
                                            this.selected_edges.clear();
                                            this.commit_graph(graph, None, cx);
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        );

                        let entity_bypass = entity.clone();
                        let bypass_targets = targets.clone();
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

                        // Collapse / Extract (REQ-LAYER-003). Both act on the
                        // same targets as Delete and Bypass above, and both
                        // land in one Document undo step.
                        let subnet_model = subnet_menu_model(&graph_snap, &targets);
                        let entity_collapse = entity.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t!(CommandId::NodeCollapseToSubnet.label_key()))
                                .disabled(!subnet_model.collapse)
                                .on_click(move |_, _window, cx| {
                                    entity_collapse
                                        .update(cx, |this, cx| {
                                            this.collapse_to_subnet(&targets, cx);
                                        })
                                        .ok();
                                }),
                        );

                        let entity_extract = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(t!(CommandId::NodeExtractSubnet.label_key()))
                                .disabled(subnet_model.extract.is_none())
                                .on_click(move |_, _window, cx| {
                                    let Some(node_id) = subnet_model.extract else {
                                        return;
                                    };
                                    entity_extract
                                        .update(cx, |this, cx| {
                                            this.extract_subnet(node_id, cx);
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
                                            this.remove_edge(edge_id, cx);
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            ),
                        );
                    }

                    let entity_es = entity.clone();
                    menu.separator().submenu(
                        t!("panel.node_graph_menu.edge_style"),
                        window,
                        cx,
                        move |sub, _window, _cx| {
                            let e1 = entity_es.clone();
                            let e2 = entity_es.clone();
                            let e3 = entity_es.clone();
                            sub.item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_bezier"))
                                    .on_click(move |_, _window, cx| {
                                        e1.update(cx, |this, cx| {
                                            this.set_edge_style(EdgeStyle::Bezier, cx);
                                        })
                                        .ok();
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_straight"))
                                    .on_click(move |_, _window, cx| {
                                        e2.update(cx, |this, cx| {
                                            this.set_edge_style(EdgeStyle::Straight, cx);
                                        })
                                        .ok();
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(t!("panel.node_graph_menu.edge_style_step"))
                                    .on_click(move |_, _window, cx| {
                                        e3.update(cx, |this, cx| {
                                            this.set_edge_style(EdgeStyle::Step, cx);
                                        })
                                        .ok();
                                    }),
                            )
                        },
                    )
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
                            &labels,
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
                        if let Some(cursor) = active_drag_cursor {
                            window.set_window_cursor_style(cursor);
                        }
                    },
                )
                .size_full(),
            )
            .child(hover_popover)
            .children(no_network)
            .children(port_notice);

        let palette_overlay = self.palette.as_ref().map(|open| {
            deferred(
                // Window-origin anchored so the window-sized scrim actually
                // covers the whole window: any click outside the palette
                // dismisses it, wherever the panel sits.
                anchored().position(point(px(0.0), px(0.0))).child(
                    div()
                        .w(window.bounds().size.width)
                        .h(window.bounds().size.height)
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        // Click outside the palette dismisses it.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _window, cx| this.dismiss_palette(cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _window, cx| this.dismiss_palette(cx)),
                        )
                        .child(
                            anchored()
                                .position(open.anchor)
                                .snap_to_window_with_margin(px(8.))
                                .child(open.palette.clone()),
                        ),
                ),
            )
            .with_priority(1)
        });

        // The port rename floats at its port: a canvas port has no row to
        // edit in place. Clicking anywhere else blurs the Input, which
        // commits and closes it, so the editor needs no scrim of its own.
        let rename_overlay = self.port_rename.as_ref().map(|rename| {
            let (ox, oy) = self.canvas_origin.get();
            let input = rename.input.clone();
            let commit_input = rename.input.clone();
            deferred(
                anchored()
                    .position(point(px(ox + rename.center.0), px(oy + rename.center.1)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .w(px(180.0))
                            .p_1()
                            .bg(colors.popover)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .shadow_lg()
                            // The capture phase fires before the focused
                            // Input's own handlers, the same way the search
                            // palette takes Enter and Escape: Enter confirms
                            // the name, Escape abandons the edit. `InputState`
                            // has no Escape event of its own, and taking Enter
                            // here keeps one commit path — blur still commits
                            // through the subscription.
                            .capture_action(cx.listener(
                                move |this, _: &input::Enter, _window, cx| {
                                    let name = commit_input.read(cx).value().to_string();
                                    this.commit_port_rename(name, cx);
                                    cx.stop_propagation();
                                },
                            ))
                            .capture_action(cx.listener(|this, _: &input::Escape, _window, cx| {
                                this.cancel_port_rename(cx);
                                cx.stop_propagation();
                            }))
                            .child(Input::new(&input).xsmall()),
                    ),
            )
            .with_priority(1)
        });

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
            .on_action(cx.listener(Self::on_search_palette))
            .on_action(cx.listener(Self::on_collapse_to_subnet))
            .on_action(cx.listener(Self::on_extract_subnet))
            .on_action(cx.listener(Self::on_auto_layout))
            .child(breadcrumb)
            .child(canvas_area)
            .children(palette_overlay)
            .children(rename_overlay)
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
    use ravel_ui::document::replace_network;
    use ravel_ui::properties::PropertyValue;
    #[test]
    fn add_node_menu_model_groups_and_sorts_templates() {
        let mut registry = NodeRegistry::new();
        registry.register(NodeTemplate::new("image.zulu", "Zulu", NodeCategory::Image));
        registry.register(NodeTemplate::new(
            CUSTOM_PATH_TYPE_KEY,
            "Custom Path",
            NodeCategory::Geometry,
        ));
        registry.register(NodeTemplate::new(
            "geometry.beta",
            "Beta",
            NodeCategory::Geometry,
        ));
        registry.register(NodeTemplate::new(
            "image.alpha",
            "Alpha",
            NodeCategory::Image,
        ));

        // These fake types have no locale entry, so their menu labels fall
        // back to the type key (a keyed type would show its localized label).
        assert_eq!(
            add_node_menu_model(&registry),
            vec![
                AddNodeMenuGroup {
                    category: NodeCategory::Geometry,
                    items: vec![AddNodeMenuItem {
                        label: "geometry.beta".into(),
                        type_key: "geometry.beta".into(),
                    }],
                },
                AddNodeMenuGroup {
                    category: NodeCategory::Image,
                    items: vec![
                        AddNodeMenuItem {
                            label: "image.alpha".into(),
                            type_key: "image.alpha".into(),
                        },
                        AddNodeMenuItem {
                            label: "image.zulu".into(),
                            type_key: "image.zulu".into(),
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn first_compatible_port_for_output_drag_skips_incompatible_inputs() {
        let source = Node::new(NodeId::new(1), "source").with_output("out", DataTypeId::SCALAR);
        let candidate = Node::new(NodeId::new(2), "candidate")
            .with_input("color", &[DataTypeId::COLOR])
            .with_input("any", &[]);
        let graph = Graph::new().add_node(source).unwrap();
        let from = PortHit {
            node_id: NodeId::new(1),
            is_output: true,
            port_index: 0,
            center: (0.0, 0.0),
        };

        assert_eq!(first_compatible_port(&graph, &from, &candidate), Some(1));
    }

    #[test]
    fn first_compatible_port_for_input_drag_picks_accepted_output() {
        let target = Node::new(NodeId::new(1), "target").with_input("in", &[DataTypeId::SCALAR]);
        let candidate = Node::new(NodeId::new(2), "candidate")
            .with_output("color", DataTypeId::COLOR)
            .with_output("scalar", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(target).unwrap();
        let from = PortHit {
            node_id: NodeId::new(1),
            is_output: false,
            port_index: 0,
            center: (0.0, 0.0),
        };

        assert_eq!(first_compatible_port(&graph, &from, &candidate), Some(1));
    }

    #[test]
    fn first_compatible_port_returns_none_without_a_match() {
        let source = Node::new(NodeId::new(1), "source").with_output("out", DataTypeId::SCALAR);
        let candidate =
            Node::new(NodeId::new(2), "candidate").with_input("color", &[DataTypeId::COLOR]);
        let graph = Graph::new().add_node(source).unwrap();
        let from = PortHit {
            node_id: NodeId::new(1),
            is_output: true,
            port_index: 0,
            center: (0.0, 0.0),
        };

        assert_eq!(first_compatible_port(&graph, &from, &candidate), None);
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

        // `position_3d` is listed: a `Channel3` exposes a VEC3 port. Only
        // `label` is skipped — a String has no driving node.
        assert_eq!(
            expose_param_menu_model(graph.node(node_id).unwrap()),
            vec![
                ExposeParamMenuItem {
                    key: "radius".into(),
                    checked: false,
                },
                ExposeParamMenuItem {
                    key: "position_3d".into(),
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

    /// The port menu names the port under the cursor whatever it is, and only
    /// enables Rename / Delete for a custom port of an interface node.
    #[test]
    fn port_menu_model_enables_only_interface_custom_ports() {
        use ravel_core::network as net;

        // The In node carries three fixed outputs and one custom port; `f`
        // without a parameter is the built-in frame index, so it is fixed.
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(1.0));
        let out_node = Node::new(NodeId::new(2), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER])
            .with_input("extra", &[DataTypeId::SCALAR]);
        let ordinary = Node::new(NodeId::new(3), "test")
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        // A subnet node's pins are derived from its inner network, not
        // declared here, so they are not editable from the outside.
        let subnet = Node::new(NodeId::new(4), "subnet")
            .with_input("value", &[DataTypeId::SCALAR])
            .with_subnet(Graph::new());
        let graph = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_node(ordinary)
            .unwrap()
            .add_node(subnet)
            .unwrap();

        let model = |node: u64, is_output: bool, port_index: u32| {
            port_menu_model(
                &graph,
                &PortHit {
                    node_id: NodeId::new(node),
                    is_output,
                    port_index,
                    center: (0.0, 0.0),
                },
            )
            .expect("the hit names a declared port")
        };
        let named = |model: PortMenuModel| (model.name, model.enabled);

        // In node: the three built-ins are protected, the custom port is not.
        assert_eq!(
            named(model(1, true, 0)),
            (net::PORT_BASE_GEOMETRY.to_string(), false)
        );
        assert_eq!(
            named(model(1, true, 1)),
            (net::PORT_TIME.to_string(), false)
        );
        assert_eq!(
            named(model(1, true, 2)),
            (net::PORT_FRAME_INDEX.to_string(), false)
        );
        assert_eq!(named(model(1, true, 3)), ("amount".to_string(), true));
        assert_eq!(model(1, true, 3).side, PortSide::Output);

        // Out node: `frame` is the shell's, `extra` is the user's.
        assert_eq!(
            named(model(2, false, 0)),
            (net::PORT_FRAME.to_string(), false)
        );
        assert_eq!(named(model(2, false, 1)), ("extra".to_string(), true));
        assert_eq!(model(2, false, 1).side, PortSide::Input);

        // An ordinary node has no custom-port concept on either side.
        assert!(!model(3, false, 0).enabled);
        assert!(!model(3, true, 0).enabled);

        // Neither does a subnet pin (unit 5 owns the inner-port derivation).
        assert!(!model(4, false, 0).enabled);

        // A hit that names a port the node does not declare yields no model.
        assert!(
            port_menu_model(
                &graph,
                &PortHit {
                    node_id: NodeId::new(1),
                    is_output: true,
                    port_index: 9,
                    center: (0.0, 0.0),
                },
            )
            .is_none()
        );
    }

    /// The legacy `net.in` `f`: with a same-named parameter beside it, it is a
    /// user-defined port that predates the built-in frame index, and
    /// `is_fixed_port` — the only authority the menu consults — keeps it
    /// editable.
    #[test]
    fn port_menu_model_follows_the_legacy_frame_index_exception() {
        use ravel_core::network as net;

        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
                    .with_param(net::PORT_FRAME_INDEX, ParameterValue::Float(0.0)),
            )
            .unwrap();

        let model = port_menu_model(
            &graph,
            &PortHit {
                node_id: NodeId::new(1),
                is_output: true,
                port_index: 0,
                center: (0.0, 0.0),
            },
        )
        .expect("the port is declared");
        assert!(model.enabled);
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

        let mut registry = NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let driven = driven_params(&graph, graph.node(NodeId::new(2)).unwrap(), &registry);
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
            ));
            cx.set_global(crate::panels::CanvasSelection::default());
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

        let window = cx.add_window(|window, cx| {
            NodeEditorPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
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

    fn change(
        panel: &mut NodeEditorPanel,
        node: NodeId,
        value: f32,
        commit: bool,
        cx: &mut Context<NodeEditorPanel>,
    ) {
        panel.apply_property_change(&[node], "radius", &PropertyValue::Float(value), commit, cx);
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

    #[test]
    fn front_node_body_occludes_a_rear_port() {
        let mut rear = positioned_node(1, 2).with_output("out", DataTypeId::SCALAR);
        rear.metadata.position = (0.0, 0.0);
        let mut front = positioned_node(2, 9);
        front.metadata.position = (150.0, 30.0);
        let graph = Graph::new()
            .add_node(front)
            .unwrap()
            .add_node(rear)
            .unwrap();
        let viewport = Viewport {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        };
        let sizes: HashMap<NodeId, (f32, f32)> = [
            (NodeId::new(1), (160.0, 80.0)),
            (NodeId::new(2), (160.0, 80.0)),
        ]
        .into();
        let (x, y) = painting::output_port_screen_center((0.0, 0.0), 0, 1.0);

        assert!(painting::port_at_local_pos(&graph, &viewport, x, y).is_some());
        assert!(NodeEditorPanel::port_hit_at(&graph, &viewport, &sizes, x, y).is_none());
        assert_eq!(
            NodeEditorPanel::pointer_hint_at_in(&graph, &viewport, &sizes, EdgeStyle::Bezier, x, y,),
            PointerHint::Node,
            "hover follows the same frontmost target as mouse-down"
        );
    }

    #[test]
    fn pointer_hint_notifies_only_on_change_and_drag_cursor_tracks_snap() {
        assert_eq!(
            pointer_hint_transition(PointerHint::Empty, PointerHint::Port, false),
            Some(PointerHint::Port)
        );
        assert_eq!(
            pointer_hint_transition(PointerHint::Port, PointerHint::Port, false),
            None
        );
        assert_eq!(
            pointer_hint_transition(PointerHint::Port, PointerHint::Node, true),
            None
        );

        let from = PortHit {
            node_id: NodeId::new(1),
            port_index: 0,
            is_output: true,
            center: (0.0, 0.0),
        };
        let target = PortHit {
            node_id: NodeId::new(2),
            port_index: 0,
            is_output: false,
            center: (10.0, 10.0),
        };
        assert_eq!(
            drag_cursor(&DragMode::Connect {
                from: from.clone(),
                to_point: (5.0, 5.0),
                snap: None,
            }),
            Some(CursorStyle::Crosshair)
        );
        assert_eq!(
            drag_cursor(&DragMode::Connect {
                from,
                to_point: (10.0, 10.0),
                snap: Some(target),
            }),
            Some(CursorStyle::DragLink)
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
    /// The header tint and the display label of each node are functions of
    /// the graph, so they are built when the graph moves and never during a
    /// repaint — a pan, a hover or a playback frame must not pay for a
    /// registry lookup and a locale lookup per node (issue HIGH-21).
    ///
    /// Poisoned entries make the "never during a repaint" half observable:
    /// a `render()` that rebuilt the maps would wipe them.
    #[gpui::test]
    fn graph_derived_caches_are_built_on_graph_change_not_on_render(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.node_labels.contains_key(&blur),
                    "opening a network fills the caches"
                );
                panel.node_labels.insert(blur, "POISON".to_string());
                panel.node_categories.remove(&blur);
            })
            .unwrap();

        for _ in 0..5 {
            window
                .update(cx, |panel, window, cx| {
                    let _ = panel.render(window, cx);
                })
                .unwrap();
        }

        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.node_labels.get(&blur).map(String::as_str),
                    Some("POISON"),
                    "render must not rebuild the label cache"
                );
                assert!(
                    !panel.node_categories.contains_key(&blur),
                    "render must not rebuild the category cache"
                );
            })
            .unwrap();

        // A graph change is what rebuilds them, so no stale label survives it.
        window
            .update(cx, |panel, _window, cx| {
                panel.add_node_from_template("blur", (0.0, 0.0), cx);
            })
            .unwrap();

        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.node_labels.len(), panel.graph.nodes().count());
                for node in panel.graph.nodes() {
                    assert_eq!(
                        panel.node_labels.get(&node.id).map(String::as_str),
                        Some(crate::node_locale::display_label(node, &panel.registry).as_str()),
                        "every cached label matches the graph"
                    );
                }
                assert!(panel.node_categories.contains_key(&blur));
            })
            .unwrap();
    }

    /// The load readouts are a function of (displayed graph, timings global)
    /// and must be re-resolved whenever the graph is replaced. Carrying the
    /// previous graph's map over would show nothing under a node that just
    /// appeared and — since node ids are reused across networks and
    /// documents — another node's measurement under one that did not.
    #[gpui::test]
    fn a_graph_change_re_resolves_the_load_readouts(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, _cx| {
                // Stands in for a readout inherited from a previous graph:
                // nothing in the timings global backs it.
                panel
                    .displayed_timings
                    .insert(blur, EvalReadout::of(std::time::Duration::from_millis(42)));
            })
            .unwrap();

        for _ in 0..3 {
            window
                .update(cx, |panel, window, cx| {
                    let _ = panel.render(window, cx);
                })
                .unwrap();
        }
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.displayed_timings.contains_key(&blur),
                    "render must not rebuild the readouts either"
                );
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.add_node_from_template("blur", (0.0, 0.0), cx);
            })
            .unwrap();

        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.displayed_timings.is_empty(),
                    "a graph change re-resolves the readouts from the global, \
                     which holds nothing for this network"
                );
            })
            .unwrap();
    }

    /// A bypassed node draws no readout (the pass-through records no timing),
    /// so its measurement must not reach the repaint gate: a change nobody
    /// can see must not cost a frame.
    #[gpui::test]
    fn a_bypassed_node_contributes_no_readout(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        cx.update(|cx| {
            let mut timings = crate::project_state::NodeEvalTimings::default();
            timings.0.insert(blur, std::time::Duration::from_millis(12));
            cx.set_global(timings);
        });
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.displayed_timings.contains_key(&blur),
                    "a live node's readout is collected"
                );
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.set_bypass(&[blur], true, cx);
            })
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.graph.node(blur).expect("blur node").metadata.bypassed,
                    "the node is bypassed"
                );
                assert!(
                    panel.displayed_timings.is_empty(),
                    "a bypassed node draws no readout, so it collects none"
                );
            })
            .unwrap();

        // A new measurement for the bypassed node changes nothing visible.
        cx.update(|cx| {
            let mut timings = crate::project_state::NodeEvalTimings::default();
            timings.0.insert(blur, std::time::Duration::from_millis(90));
            cx.set_global(timings);
        });
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.displayed_timings.is_empty());
            })
            .unwrap();
    }

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

    /// New shape generators default to the composition center (the test
    /// project's root comp is 1920x1080), while non-shape nodes and the
    /// registry template defaults stay untouched.
    #[gpui::test]
    fn added_shape_node_defaults_to_comp_center(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.add_node_from_template("shape.ellipse", (0.0, 0.0), cx);
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            let ellipse = graph
                .nodes()
                .find(|n| n.type_key == "shape.ellipse")
                .expect("ellipse node");
            let vector = |key: &str| match ellipse
                .parameters
                .iter()
                .find(|p| p.key == key)
                .map(|p| &p.value)
            {
                Some(ParameterValue::Channel2(chs)) => chs
                    .iter()
                    .map(|ch| match ch.source {
                        ravel_core::animation::channel::ChannelSource::Constant(v) => v,
                        ref other => panic!("unexpected {key} component: {other:?}"),
                    })
                    .collect::<Vec<_>>(),
                other => panic!("unexpected {key} parameter: {other:?}"),
            };
            assert_eq!(vector("center"), vec![960.0, 540.0]);
            assert_eq!(
                vector("radius"),
                vec![50.0, 50.0],
                "non-center params keep defaults"
            );
        });
    }

    #[gpui::test]
    fn edge_drop_adds_and_connects_node_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.viewport = Viewport {
                    x: 50.0,
                    y: 30.0,
                    zoom: 2.0,
                };
                panel.add_node_from_edge_drop(
                    "blur",
                    PortHit {
                        node_id: blur,
                        is_output: true,
                        port_index: 0,
                        center: (0.0, 0.0),
                    },
                    (250.0, 130.0),
                    cx,
                );
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 2);
            assert_eq!(graph.edges().count(), 1);
            assert!(
                graph
                    .nodes()
                    .any(|node| { node.id != blur && node.metadata.position == (100.0, 50.0) })
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 1);
            assert_eq!(graph.edges().count(), 0);
        });
    }

    /// A palette accept takes the same document path as the add-node context
    /// menu: exactly one node added, reverted by exactly one undo step.
    #[gpui::test]
    fn palette_accept_adds_node_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(
                    None,
                    (250.0, 130.0),
                    point(px(0.0), px(0.0)),
                    window,
                    cx,
                );
                assert!(panel.palette.is_some());
                panel.accept_palette("blur", window, cx);
                assert!(panel.palette.is_none(), "accept closes the palette");
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 2);
        });
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 1);
        });
    }

    /// `MED-APP-27`: Tab opens the palette where the pointer is, so the same
    /// palette does not appear in two different places depending on how it
    /// was invoked. Off the canvas it still falls back to the center.
    #[gpui::test]
    fn the_keyboard_palette_opens_under_the_pointer(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        window
            .update(cx, |panel, _window, _cx| {
                panel.canvas_size.set((800.0, 600.0));

                panel.last_pointer = Some((210.0, 90.0));
                assert_eq!(panel.pointer_or_canvas_center(), (210.0, 90.0));

                panel.last_pointer = Some((-30.0, 90.0));
                assert_eq!(
                    panel.pointer_or_canvas_center(),
                    (400.0, 300.0),
                    "a pointer off the canvas falls back to the center"
                );

                panel.last_pointer = Some((210.0, 1000.0));
                assert_eq!(
                    panel.pointer_or_canvas_center(),
                    (400.0, 300.0),
                    "a drag that carried the pointer past the bottom edge too"
                );

                panel.last_pointer = None;
                assert_eq!(
                    panel.pointer_or_canvas_center(),
                    (400.0, 300.0),
                    "a pointer that never entered the canvas falls back too"
                );
            })
            .unwrap();
    }

    /// A wire-invoked palette offers only connectable types, and its accept
    /// is the same document change as the former edge-drop menu (one undo).
    #[gpui::test]
    fn palette_wire_accept_filters_and_connects_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(
                    Some(PortHit {
                        node_id: blur,
                        is_output: true,
                        port_index: 0,
                        center: (0.0, 0.0),
                    }),
                    (250.0, 130.0),
                    point(px(0.0), px(0.0)),
                    window,
                    cx,
                );
                let palette = &panel.palette.as_ref().expect("palette open").palette;
                let offered: Vec<String> = palette.read_with(cx, |palette, _| {
                    palette
                        .visible
                        .iter()
                        .map(|&index| palette.candidates[index].type_key.clone())
                        .collect()
                });
                assert!(
                    offered.iter().any(|key| key == "blur"),
                    "blur takes a frame buffer: {offered:?}"
                );
                assert!(
                    !offered.iter().any(|key| key == "constant"),
                    "constant has no inputs: {offered:?}"
                );
                panel.accept_palette("blur", window, cx);
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 2);
            assert_eq!(graph.edges().count(), 1);
        });
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            assert_eq!(graph.nodes().count(), 1);
            assert_eq!(graph.edges().count(), 0);
        });
    }

    /// Closing the palette drops everything typed or filtered: the next open
    /// builds a fresh entity with an empty query, no category filter, and
    /// the selection back on the first row.
    #[gpui::test]
    fn closing_and_reopening_the_palette_keeps_no_state(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        // Everything happens in one update: rendering a reopened palette
        // would paint the query `Input`, which needs a
        // `gpui_component::Root` window root the panel test window does not
        // have — so the palette is closed again before the update ends.
        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(None, (10.0, 10.0), point(px(0.0), px(0.0)), window, cx);
                let first = panel
                    .palette
                    .as_ref()
                    .expect("palette open")
                    .palette
                    .clone();
                first.update(cx, |palette, cx| {
                    palette
                        .input
                        .update(cx, |input, cx| input.set_value("blur", window, cx));
                    palette.set_category_filter(Some(NodeCategory::Image), cx);
                    palette.move_selection(1, cx);
                });
                panel.dismiss_palette(cx);
                assert!(panel.palette.is_none());

                panel.open_search_palette(None, (10.0, 10.0), point(px(0.0), px(0.0)), window, cx);
                let second = panel
                    .palette
                    .as_ref()
                    .expect("palette open")
                    .palette
                    .clone();
                assert_ne!(
                    first.entity_id(),
                    second.entity_id(),
                    "a reopen builds a fresh palette entity"
                );
                second.read_with(cx, |palette, cx| {
                    assert!(palette.query(cx).is_empty(), "query must not survive");
                    assert_eq!(palette.category_filter, None);
                    assert_eq!(palette.selected, 0);
                });
                panel.dismiss_palette(cx);
            })
            .unwrap();
    }

    /// Accepting through the palette records the type as recently used.
    #[gpui::test]
    fn accepting_a_node_records_it_as_recent(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(None, (10.0, 10.0), point(px(0.0), px(0.0)), window, cx);
                panel.accept_palette("rasterize", window, cx);
                assert_eq!(
                    panel.recent_types.first().map(String::as_str),
                    Some("rasterize")
                );
            })
            .unwrap();
    }

    /// The edge-drop accept path records the recents the same way (a wire
    /// drop is just another way to add a node).
    #[gpui::test]
    fn edge_drop_accept_records_the_type_as_recent(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.add_node_from_edge_drop(
                    "blur",
                    PortHit {
                        node_id: blur,
                        is_output: true,
                        port_index: 0,
                        center: (0.0, 0.0),
                    },
                    (200.0, 100.0),
                    cx,
                );
                assert_eq!(panel.recent_types.first().map(String::as_str), Some("blur"));
            })
            .unwrap();
    }

    /// A document change from another panel (or undo) replaces the graph the
    /// palette's placement context refers to: the palette closes instead of
    /// accepting into stale coordinates, and focus returns to the canvas.
    #[gpui::test]
    fn a_document_change_dismisses_an_open_palette(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(
                    Some(PortHit {
                        node_id: blur,
                        is_output: true,
                        port_index: 0,
                        center: (0.0, 0.0),
                    }),
                    (10.0, 10.0),
                    point(px(0.0), px(0.0)),
                    window,
                    cx,
                );
                let palette_focus = panel
                    .palette
                    .as_ref()
                    .expect("palette open")
                    .palette
                    .read(cx)
                    .input_focus_handle(cx);
                assert!(
                    window.focused(cx).is_some_and(|f| f == palette_focus),
                    "the palette input holds focus while open"
                );

                // A change from outside the panel: drop the layer's whole
                // network (stand-in for any external Document edit).
                let document = project.read(cx).document().clone();
                let empty =
                    ravel_ui::document::update_layer(&document, path.comp, path.layer, |layer| {
                        layer.network = Graph::new();
                    })
                    .expect("layer exists");
                project.update(cx, |project, cx| {
                    project.commit_document(empty, InvalidationHint::Structural, cx);
                });

                panel.refresh_from_document(cx);

                assert!(
                    panel.palette.is_none(),
                    "a graph swap dismisses the palette"
                );
            })
            .unwrap();

        // The refocus is deferred (see `dismiss_palette`): it has run by
        // the time the update above unwound. A second update never paints
        // the palette (it is gone), so no `gpui_component::Root` is needed.
        window
            .update(cx, |panel, window, cx| {
                assert!(
                    window
                        .focused(cx)
                        .is_some_and(|f| f == panel.focus_handle(cx)),
                    "focus returns to the canvas"
                );
            })
            .unwrap();
    }

    /// Switching or closing the network under an open palette runs the same
    /// teardown (palette gone, focus back on the canvas).
    #[gpui::test]
    fn closing_the_network_dismisses_the_palette(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.open_search_palette(None, (10.0, 10.0), point(px(0.0), px(0.0)), window, cx);
                assert!(panel.palette.is_some());

                panel.close_network(cx);

                assert!(panel.palette.is_none());
            })
            .unwrap();

        window
            .update(cx, |panel, window, cx| {
                assert!(
                    window
                        .focused(cx)
                        .is_some_and(|f| f == panel.focus_handle(cx)),
                    "focus returns to the canvas"
                );
            })
            .unwrap();
    }

    /// A custom port's name is also its parameter key, and an exposed
    /// parameter declaration binds to that key (REQ-PROJ-006). Renaming the
    /// port through the panel has to carry the declaration with it in the same
    /// Document commit — the graph and the project's external contract cannot
    /// be one undo step apart.
    #[gpui::test]
    fn renaming_a_port_moves_the_exposed_declaration_in_the_same_commit(cx: &mut TestAppContext) {
        use ravel_core::exposed::{
            ExposedBinding, ExposedParameter, ExposedParameters, ExposedValue,
        };

        let (window, project, _path, _blur) = setup(cx);
        let in_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let in_node =
                    ravel_core::graph::Node::new(in_id, ravel_core::network::NET_IN_TYPE_KEY)
                        .with_output("t", ravel_core::id::DataTypeId::SCALAR);
                let graph = panel.graph.clone().add_node(in_node).unwrap();
                panel.commit_graph(graph, None, cx);
                panel
                    .add_custom_port(in_id, "headline", CustomPortType::Float, cx)
                    .expect("a float port is allowed at a layer root");
            })
            .unwrap();

        project.update(cx, |project, cx| {
            let declarations = ExposedParameters::from_declarations([ExposedParameter::inferred(
                "headline",
                ExposedValue::Float(0.0),
                ExposedBinding::new(in_id, "headline"),
            )
            .unwrap()])
            .unwrap();
            let doc = project
                .document()
                .clone()
                .with_exposed_parameters(declarations);
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                panel
                    .rename_custom_port(in_id, "headline", "title", cx)
                    .expect("the port is custom");
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let declaration = project
                .document()
                .exposed_parameters
                .get("headline")
                .expect("the declaration survives an edit to the port behind it");
            assert_eq!(
                declaration.binding(),
                &ExposedBinding::new(in_id, "title"),
                "the binding followed the parameter key"
            );
        });

        // The two halves are one undo step, in both directions. A rename that
        // could be half-undone would leave the project's external contract
        // pointing at a key the graph no longer has — with nothing in the UI
        // to show for it and no edit that repairs it.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            port_and_binding(&project, in_id, cx),
            (Some("headline".to_string()), Some("headline".to_string())),
            "undo takes the port name and the binding back together"
        );

        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            port_and_binding(&project, in_id, cx),
            (Some("title".to_string()), Some("title".to_string())),
            "and redo moves both forward again"
        );
    }

    /// The custom port's name on the In node, and the parameter key the
    /// `headline` declaration is bound to — the two halves a rename has to
    /// keep in step.
    fn port_and_binding(
        project: &Entity<ProjectState>,
        in_id: NodeId,
        cx: &mut TestAppContext,
    ) -> (Option<String>, Option<String>) {
        project.read_with(cx, |project, _| {
            let document = project.document();
            let node = document
                .compositions
                .values()
                .flat_map(|comp| comp.layers.iter())
                .find_map(|layer| layer.network.node(in_id))
                .expect("the In node");
            let port = node
                .outputs
                .iter()
                .map(|port| port.name.clone())
                .find(|name| name == "headline" || name == "title");
            let binding = document
                .exposed_parameters
                .get("headline")
                .map(|declaration| declaration.binding().key.clone());
            (port, binding)
        })
    }

    #[gpui::test]
    fn edge_drop_grows_new_scatter_variadic_input_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);
        let source_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let source = panel
                    .registry
                    .create_node("shape.rect", source_id)
                    .expect("shape template");
                let graph = panel.graph.clone().add_node(source).unwrap();
                panel.commit_graph(graph, None, cx);
                panel.add_node_from_edge_drop(
                    "scatter.grid",
                    PortHit {
                        node_id: source_id,
                        is_output: true,
                        port_index: 0,
                        center: (0.0, 0.0),
                    },
                    (200.0, 100.0),
                    cx,
                );
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            let scatter = graph
                .nodes()
                .find(|node| node.type_key == "scatter.grid")
                .expect("scatter node");
            assert_eq!(scatter.inputs.len(), 2);
            assert_eq!(
                graph
                    .edges()
                    .filter(|edge| edge.target == scatter.id)
                    .count(),
                1
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert!(graph.nodes().all(|node| node.type_key != "scatter.grid"));
            assert!(graph.node(source_id).is_some());
        });
    }

    #[gpui::test]
    fn edge_drop_from_input_replaces_existing_edge(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        let existing_source = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let source = panel
                    .registry
                    .create_node("blur", existing_source)
                    .expect("blur template");
                let graph = panel
                    .graph
                    .clone()
                    .add_node(source)
                    .unwrap()
                    .add_edge(
                        EdgeId::next(),
                        existing_source,
                        OutputPortIndex(0),
                        blur,
                        InputPortIndex(0),
                    )
                    .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.add_node_from_edge_drop(
                    "blur",
                    PortHit {
                        node_id: blur,
                        is_output: false,
                        port_index: 0,
                        center: (0.0, 0.0),
                    },
                    (200.0, 100.0),
                    cx,
                );
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            let incoming: Vec<_> = graph
                .edges()
                .filter(|edge| edge.target == blur && edge.target_port == InputPortIndex(0))
                .collect();
            assert_eq!(incoming.len(), 1);
            assert_ne!(incoming[0].source, existing_source);
            assert_eq!(graph.nodes().count(), 3);
        });
    }

    #[gpui::test]
    fn variadic_ports_grow_and_compact_with_edge_undo(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);
        let source_id = NodeId::next();
        let scatter_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let source = panel
                    .registry
                    .create_node("shape.rect", source_id)
                    .expect("shape template");
                let scatter = panel
                    .registry
                    .create_node("scatter.grid", scatter_id)
                    .expect("scatter template");
                let graph = panel
                    .graph
                    .clone()
                    .add_node(source)
                    .unwrap()
                    .add_node(scatter)
                    .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.toggle_param_port(scatter_id, "count_x", cx);
                panel.connect_ports(
                    source_id,
                    OutputPortIndex(0),
                    scatter_id,
                    InputPortIndex(0),
                    cx,
                );
            })
            .unwrap();

        let assert_graph = |expected_ports, expected_edges, cx: &TestAppContext| {
            project.read_with(cx, |project, _| {
                let graph = resolve_network(project.document(), &path).unwrap();
                let scatter = graph.node(scatter_id).unwrap();
                assert_eq!(scatter.inputs.len(), expected_ports);
                let param = scatter.inputs.last().expect("parameter port");
                assert_eq!(param.name, "count_x");
                assert!(param.is_param);
                assert!(!param.is_variadic);
                assert_eq!(graph.edge_count(), expected_edges);
            });
        };
        assert_graph(3, 1, cx);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_graph(2, 0, cx);
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_graph(3, 1, cx);

        window
            .update(cx, |panel, _window, cx| {
                let edge_id = panel.graph.edges().next().expect("connected edge").id;
                panel.remove_edge(edge_id, cx);
            })
            .unwrap();
        assert_graph(2, 0, cx);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_graph(3, 1, cx);
    }

    #[gpui::test]
    fn duplicate_scatter_preserves_variadic_ports_verbatim(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);
        let first_source = NodeId::next();
        let second_source = NodeId::next();
        let scatter_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let source_a = panel
                    .registry
                    .create_node("shape.rect", first_source)
                    .expect("shape template");
                let source_b = panel
                    .registry
                    .create_node("shape.ellipse", second_source)
                    .expect("shape template");
                let scatter = panel
                    .registry
                    .create_node("scatter.grid", scatter_id)
                    .expect("scatter template");
                let graph = panel
                    .graph
                    .clone()
                    .add_node(source_a)
                    .unwrap()
                    .add_node(source_b)
                    .unwrap()
                    .add_node(scatter)
                    .unwrap();
                let graph = connect_edge_and_update_variadic_inputs(
                    graph,
                    EdgeId::next(),
                    first_source,
                    OutputPortIndex(0),
                    scatter_id,
                    InputPortIndex(0),
                )
                .unwrap();
                let graph = connect_edge_and_update_variadic_inputs(
                    graph,
                    EdgeId::next(),
                    second_source,
                    OutputPortIndex(0),
                    scatter_id,
                    InputPortIndex(1),
                )
                .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.set_selected_nodes([scatter_id].into_iter().collect(), cx);
                panel.duplicate_selected(cx);
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            let scatters: Vec<_> = graph
                .nodes()
                .filter(|node| node.type_key == "scatter.grid")
                .collect();
            assert_eq!(scatters.len(), 2);
            for scatter in scatters {
                assert_eq!(scatter.inputs.len(), 3);
                assert_eq!(scatter.inputs[0].name, "instance_source");
                assert_eq!(scatter.inputs[1].name, "instance_source_2");
                assert_eq!(scatter.inputs[2].name, "instance_source_3");
                assert!(scatter.inputs.iter().all(|input| input.is_variadic));
            }
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).unwrap();
            assert_eq!(
                graph
                    .nodes()
                    .filter(|node| node.type_key == "scatter.grid")
                    .count(),
                1
            );
        });
    }

    /// Changing `attribute.set`'s `type` reshapes its `value`, re-types the
    /// exposed parameter port, and drops the edge that can no longer feed it
    /// — all in the single Document snapshot one undo step restores.
    #[gpui::test]
    fn changing_attribute_set_type_retypes_value_and_its_port_in_one_undo(cx: &mut TestAppContext) {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);
        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ));
            cx.set_global(crate::panels::CanvasSelection::default());
        });

        let (set_id, driver_id) = (NodeId::next(), NodeId::next());
        let path = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let network = Graph::new()
                .add_node(registry.create_node("attribute.set", set_id).unwrap())
                .unwrap()
                .add_node(registry.create_node("constant", driver_id).unwrap())
                .unwrap()
                .expose_param_port(set_id, "value")
                .unwrap();
            let port = network
                .node(set_id)
                .unwrap()
                .param_port_index("value")
                .unwrap();
            let network = network
                .add_edge(
                    ravel_core::id::EdgeId::next(),
                    driver_id,
                    ravel_core::id::OutputPortIndex(0),
                    set_id,
                    port,
                )
                .unwrap();
            let layer_id = LayerId::next();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                Layer::new(layer_id, "Attr", network).with_time(0, 0, 300),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            NetworkPath::layer(comp_id, layer_id)
        });

        let inspect = |cx: &mut TestAppContext| {
            project.read_with(cx, |project, _| {
                let graph = resolve_network(project.document(), &path).expect("network");
                let node = graph.node(set_id).expect("attribute.set").clone();
                let port = node.param_port_index("value");
                let accepted = port.map(|p| node.inputs[p.0 as usize].accepted_types.clone());
                let arity = node
                    .parameters
                    .iter()
                    .find(|p| p.key == "value")
                    .and_then(|p| p.value.channels())
                    .map(|chs| chs.len());
                (arity, accepted, graph.edge_count())
            })
        };
        assert_eq!(
            inspect(cx),
            (Some(1), Some(vec![DataTypeId::SCALAR]), 1),
            "a fresh attribute.set has a scalar value driven by one edge"
        );

        let window = cx.add_window(|window, cx| {
            NodeEditorPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        window
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
                panel.apply_property_change(
                    &[set_id],
                    "type",
                    &PropertyValue::String("vec3".into()),
                    true,
                    cx,
                );
            })
            .unwrap();

        assert_eq!(
            inspect(cx),
            (Some(3), Some(vec![DataTypeId::VEC3]), 0),
            "value widened to 3 components, its port became VEC3, and the \
             scalar edge that can no longer feed it was dropped"
        );

        // One Document undo restores the value, the port and the edge together.
        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert_eq!(
            inspect(cx),
            (Some(1), Some(vec![DataTypeId::SCALAR]), 1),
            "the whole retype is one undo step"
        );
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
                change(panel, blur, 10.0, false, cx);
                change(panel, blur, 20.0, false, cx);
                change(panel, blur, 42.0, true, cx);
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
                change(panel, blur, 9999.0, true, cx);
            })
            .unwrap();
        assert!(
            (blur_radius(&project, &path, blur, cx)
                - ravel_core::registry::builtin::MAX_BLUR_RADIUS)
                .abs()
                < f32::EPSILON
        );
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
            assert!((curve.sample(0.0) - original).abs() < f32::EPSILON);
        });

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!((blur_radius(&project, &path, blur, cx) - original).abs() < f32::EPSILON);
    }

    /// Add a registry node of `type_key` to the layer's open network and
    /// return its id, so a toggle test can reach a parameter kind the blur
    /// node does not have.
    fn add_node_of(
        window: &gpui::WindowHandle<NodeEditorPanel>,
        project: &Entity<ProjectState>,
        path: &NetworkPath,
        type_key: &str,
        cx: &mut TestAppContext,
    ) -> NodeId {
        let id = NodeId::next();
        project.update(cx, |project, cx| {
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let node = registry.create_node(type_key, id).expect("registry node");
            let graph = resolve_network(project.document(), path)
                .expect("network")
                .clone()
                .add_node(node)
                .unwrap();
            let doc = replace_network(project.document(), path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        window
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
            })
            .unwrap();
        id
    }

    fn param_of(
        project: &Entity<ProjectState>,
        path: &NetworkPath,
        node: NodeId,
        key: &str,
        cx: &mut TestAppContext,
    ) -> ParameterValue {
        project.read_with(cx, |project, _| {
            resolve_network(project.document(), path)
                .expect("network")
                .node(node)
                .expect("node")
                .parameters
                .iter()
                .find(|p| p.key == key)
                .expect("parameter")
                .value
                .clone()
        })
    }

    /// The key toggle re-types a constant `Int` to an `IntChannel` holding the
    /// same number, and toggling the last key back off leaves the channel
    /// holding it as a constant — the same round trip a `Float` makes, and the
    /// number survives both halves of it. One Document undo per click.
    #[gpui::test]
    fn toggle_param_keyframe_round_trips_an_int_param(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let polygon = add_node_of(&window, &project, &path, "shape.polygon", cx);
        assert_eq!(
            param_of(&project, &path, polygon, "sides", cx),
            ParameterValue::Int(6)
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(polygon, "sides", cx);
            })
            .unwrap();
        let ParameterValue::IntChannel(channel) = param_of(&project, &path, polygon, "sides", cx)
        else {
            panic!("sides re-typed to an int channel");
        };
        let ChannelSource::Keyframes(curve) = &channel.source else {
            panic!("keyed at the current frame: {:?}", channel.source);
        };
        assert_eq!(curve.len(), 1);
        assert_eq!(curve.sample(0.0), 6.0, "the key holds the number it had");

        // Off again: the last key goes and the value is still 6.
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(polygon, "sides", cx);
            })
            .unwrap();
        let ParameterValue::IntChannel(channel) = param_of(&project, &path, polygon, "sides", cx)
        else {
            panic!("an unkeyed int channel stays an int channel");
        };
        assert!(matches!(channel.source, ChannelSource::Constant(v) if v == 6.0));

        // One click, one undo step: the first undo restores the keyed channel,
        // the second the constant `Int`.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(matches!(
            param_of(&project, &path, polygon, "sides", cx),
            ParameterValue::IntChannel(_)
        ));
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            param_of(&project, &path, polygon, "sides", cx),
            ParameterValue::Int(6)
        );
    }

    /// The same round trip for a string, which has one variant more to travel:
    /// keying re-types it to `StringSteps` and removing the last key returns a
    /// plain `String` holding the text that key had.
    #[gpui::test]
    fn toggle_param_keyframe_round_trips_a_string_param(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let layer_ref = add_node_of(&window, &project, &path, "layer.ref", cx);
        assert_eq!(
            param_of(&project, &path, layer_ref, "port", cx),
            ParameterValue::String("frame".into())
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(layer_ref, "port", cx);
            })
            .unwrap();
        let ParameterValue::StringSteps(steps) = param_of(&project, &path, layer_ref, "port", cx)
        else {
            panic!("port re-typed to a step curve");
        };
        assert_eq!(steps.len(), 1);
        assert!(steps.contains_key(0));
        assert_eq!(steps.sample(0.0), "frame", "the key holds the text it had");

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(layer_ref, "port", cx);
            })
            .unwrap();
        assert_eq!(
            param_of(&project, &path, layer_ref, "port", cx),
            ParameterValue::String("frame".into()),
            "the last key removed returns the constant spelling"
        );
    }

    /// The value the parameter had before it was keyed is what removing the
    /// last key restores — not whichever key happened to go last.
    ///
    /// Two keys, edited to two different strings, then both removed: the
    /// parameter must come back holding the original constant. Returning the
    /// removed key's value instead passes the single-key round trip above and
    /// fails here, which is the whole reason this test exists separately.
    #[gpui::test]
    fn removing_the_last_string_key_restores_the_original_constant(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let layer_ref = add_node_of(&window, &project, &path, "layer.ref", cx);
        assert_eq!(
            param_of(&project, &path, layer_ref, "port", cx),
            ParameterValue::String("frame".into()),
            "the original constant"
        );

        // Key at frame 0, then edit that key to something else, so the curve
        // no longer holds the original anywhere among its keys.
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(layer_ref, "port", cx);
                panel.apply_property_change(
                    &[layer_ref],
                    "port",
                    &ravel_ui::properties::PropertyValue::String("edited".into()),
                    true,
                    cx,
                );
            })
            .unwrap();
        let ParameterValue::StringSteps(steps) = param_of(&project, &path, layer_ref, "port", cx)
        else {
            panic!("still a step curve");
        };
        assert_eq!(steps.sample(0.0), "edited");
        assert_eq!(
            steps.default_value(),
            "frame",
            "the default still carries the original"
        );

        // Remove the only key: the original comes back, not "edited".
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(layer_ref, "port", cx);
            })
            .unwrap();
        assert_eq!(
            param_of(&project, &path, layer_ref, "port", cx),
            ParameterValue::String("frame".into()),
            "removing the last key restores the constant the parameter was keyed from"
        );
    }

    /// A media node's `asset_id` holds a raw `AssetId` as a **string**, and
    /// `node_asset_reference` reads a plain `String` and nothing else — so
    /// animating it would hide the reference from the id watermark scan and let
    /// a later mint reuse an id a key still names. The row carries no toggle
    /// and the toggle refuses the parameter when called directly.
    #[gpui::test]
    fn toggle_param_keyframe_refuses_a_media_asset_reference(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let media = add_node_of(&window, &project, &path, "media", cx);
        let before = param_of(&project, &path, media, "asset_id", cx);
        assert!(matches!(before, ParameterValue::String(_)));

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(media, "asset_id", cx);
            })
            .unwrap();
        assert_eq!(
            param_of(&project, &path, media, "asset_id", cx),
            before,
            "an asset reference is left exactly as it was"
        );
    }

    /// The same rule for the *port* half: a Scalar wire into an identifier
    /// parameter makes the referenced id a function of the frame, which the id
    /// scan cannot see either. Exposing is refused; removing a port that
    /// already exists is not, so a document holding one is never stuck.
    #[gpui::test]
    fn toggle_param_port_refuses_to_expose_an_identifier(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let layer_ref = add_node_of(&window, &project, &path, "layer.ref", cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_port(layer_ref, "layer", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            assert!(
                resolve_network(project.document(), &path)
                    .expect("network")
                    .node(layer_ref)
                    .expect("layer.ref node")
                    .param_port_index("layer")
                    .is_none(),
                "an identifier parameter cannot be exposed as a port"
            );
        });

        // A plain `Int` on another node still exposes, so the refusal is the
        // parameter's identity and not "ints cannot be driven".
        let polygon = add_node_of(&window, &project, &path, "shape.polygon", cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_port(polygon, "sides", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            assert!(
                resolve_network(project.document(), &path)
                    .expect("network")
                    .node(polygon)
                    .expect("polygon node")
                    .param_port_index("sides")
                    .is_some(),
                "an ordinary animatable count is still exposable"
            );
        });

        // A port that already exists — put there through the graph API, which
        // this fix deliberately leaves open — can still be removed from the UI.
        // Refusing that half too would strand a document holding one.
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path)
                .expect("network")
                .clone()
                .expose_param_port(layer_ref, "layer")
                .expect("the graph API still allows it");
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        window
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
                panel.toggle_param_port(layer_ref, "layer", cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            assert!(
                resolve_network(project.document(), &path)
                    .expect("network")
                    .node(layer_ref)
                    .expect("layer.ref node")
                    .param_port_index("layer")
                    .is_none(),
                "an existing identifier port is still removable"
            );
        });
    }

    /// An identifier parameter is not animatable: `layer.ref`'s `layer` names a
    /// layer, not a number, and a curve through it would leave
    /// `Document::id_watermarks` with no finite set of ids to reserve. The
    /// toggle refuses it even when called directly, not only when the row
    /// hides the button.
    #[gpui::test]
    fn toggle_param_keyframe_refuses_an_identifier_param(cx: &mut TestAppContext) {
        let (window, project, path, _) = setup(cx);
        let layer_ref = add_node_of(&window, &project, &path, "layer.ref", cx);
        let before = param_of(&project, &path, layer_ref, "layer", cx);
        assert!(matches!(before, ParameterValue::Int(_)));

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_keyframe(layer_ref, "layer", cx);
            })
            .unwrap();
        assert_eq!(
            param_of(&project, &path, layer_ref, "layer", cx),
            before,
            "an identifier parameter is left exactly as it was"
        );
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
                change(panel, blur, 10.0, false, cx);
                change(panel, blur, 42.0, true, cx);
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
            assert!((curve.sample(0.0) - 42.0).abs() < f32::EPSILON);
        });
    }

    /// Structural edits (delete) go through the document, and undoing the
    /// document restores the editor's display graph via the observer.
    #[gpui::test]
    fn delete_and_document_undo_roundtrip(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes([blur].into_iter().collect(), cx);
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
                panel.set_selected_nodes([in_id, out_id, blur].into_iter().collect(), cx);
                panel.copy_selected(cx);
                let clipboard = panel.clipboard.as_ref().expect("copy stored nodes");
                assert_eq!(clipboard.nodes.len(), 1);
                assert_eq!(clipboard.nodes[0].id, blur);

                // Delete removes the blur node but keeps both boundaries.
                panel.delete_selected(cx);
                assert!(panel.graph.node(blur).is_none());
                assert_eq!(count(panel, is_in), 1);
                assert_eq!(count(panel, is_out), 1);

                // Duplicate of a boundary-only selection is a no-op.
                panel.set_selected_nodes([in_id, out_id].into_iter().collect(), cx);
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

    /// Collapse takes only what the selection is allowed to give it: the
    /// network's own In / Out interface and the compiler's synthetic nodes
    /// stay behind, and the whole move is one Document undo step
    /// (REQ-LAYER-003).
    #[gpui::test]
    fn collapse_takes_one_undo_step_and_leaves_the_boundary_behind(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);

        let in_id = NodeId::next();
        let out_id = NodeId::next();
        let synthetic_id = NodeId::next();
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path).unwrap().clone();
            let mut synthetic = Node::new(synthetic_id, "comp.opacity")
                .with_output("output", DataTypeId::FRAME_BUFFER);
            synthetic.metadata.synthetic = true;
            let graph = graph
                .add_node(
                    Node::new(in_id, ravel_core::network::NET_IN_TYPE_KEY)
                        .with_output(ravel_core::network::PORT_TIME, DataTypeId::SCALAR),
                )
                .unwrap()
                .add_node(
                    Node::new(out_id, ravel_core::network::NET_OUT_TYPE_KEY)
                        .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
                )
                .unwrap()
                .add_node(synthetic)
                .unwrap();
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        let subnet = window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes(
                    [in_id, out_id, synthetic_id, blur].into_iter().collect(),
                    cx,
                );
                panel.collapse_to_subnet(&[in_id, out_id, synthetic_id, blur], cx);

                assert!(ravel_core::network::is_in_node(
                    panel.graph.node(in_id).expect("the In node stays")
                ));
                assert!(ravel_core::network::is_out_node(
                    panel.graph.node(out_id).expect("the Out node stays")
                ));
                assert!(
                    panel
                        .graph
                        .node(synthetic_id)
                        .expect("the synthetic node stays")
                        .metadata
                        .synthetic
                );
                assert!(
                    panel.graph.node(blur).is_none(),
                    "the one collapsible node moved a level down"
                );

                let subnet = panel
                    .graph
                    .nodes()
                    .find(|node| ravel_core::network::is_subnet_node(node))
                    .expect("a subnet node took its place")
                    .id;
                assert_eq!(
                    NodeEditorPanel::selected_nodes(cx),
                    [subnet].into_iter().collect::<HashSet<_>>(),
                    "the selection follows the nodes into the node that owns them"
                );
                subnet
            })
            .unwrap();

        // One undo, and the network is exactly what it was.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.graph.node(blur).is_some());
                assert!(panel.graph.node(subnet).is_none());
            })
            .unwrap();
    }

    /// Extract is the way back, and it also takes one undo step.
    #[gpui::test]
    fn extract_returns_the_collapsed_nodes_to_the_network(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        let _ = path;

        window
            .update(cx, |panel, _window, cx| {
                panel.collapse_to_subnet(&[blur], cx);
                let subnet = panel
                    .graph
                    .nodes()
                    .find(|node| ravel_core::network::is_subnet_node(node))
                    .expect("a subnet node")
                    .id;
                panel.extract_subnet(subnet, cx);

                assert!(panel.graph.node(blur).is_some(), "the node came back");
                assert!(
                    !panel
                        .graph
                        .nodes()
                        .any(|node| ravel_core::network::is_subnet_node(node)),
                    "the subnet node is gone"
                );
                assert_eq!(
                    NodeEditorPanel::selected_nodes(cx),
                    [blur].into_iter().collect::<HashSet<_>>(),
                    "what came out is what is selected"
                );
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel
                        .graph
                        .nodes()
                        .any(|node| ravel_core::network::is_subnet_node(node)),
                    "one undo puts the subnet node back"
                );
            })
            .unwrap();
    }

    // ----- auto layout (NGR-2) ----------------------------------------------

    /// Every drawn node as `(x, y, w, h)` in network coordinates.
    fn drawn_rects(panel: &NodeEditorPanel) -> Vec<(f32, f32, f32, f32)> {
        panel
            .graph
            .nodes()
            .filter(|node| !node.metadata.synthetic)
            .map(|node| {
                let (w, h) = panel
                    .node_sizes
                    .get(&node.id)
                    .copied()
                    .unwrap_or((160.0, 60.0));
                let zoom = panel.viewport.zoom;
                (
                    node.metadata.position.0,
                    node.metadata.position.1,
                    w / zoom,
                    h / zoom,
                )
            })
            .collect()
    }

    fn rects_overlap(rects: &[(f32, f32, f32, f32)]) -> bool {
        rects.iter().enumerate().any(|(i, a)| {
            rects[i + 1..]
                .iter()
                .any(|b| a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3)
        })
    }

    fn positions(panel: &NodeEditorPanel) -> HashMap<NodeId, (f32, f32)> {
        panel
            .graph
            .nodes()
            .map(|node| (node.id, node.metadata.position))
            .collect()
    }

    /// Add `count` extra blur nodes, all stacked on the origin with the one
    /// `setup` already made.
    fn pile_up_nodes(
        project: &Entity<ProjectState>,
        path: &NetworkPath,
        count: usize,
        cx: &mut TestAppContext,
    ) -> Vec<NodeId> {
        let ids: Vec<NodeId> = (0..count).map(|_| NodeId::next()).collect();
        project.update(cx, |project, cx| {
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let mut graph = resolve_network(project.document(), path).unwrap().clone();
            for &id in &ids {
                let node = registry.create_node("blur", id).expect("blur node");
                graph = graph.add_node(node).unwrap();
            }
            let doc = replace_network(project.document(), path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        ids
    }

    /// The alignment writes `NodeMetadata::position`, so it is a document edit
    /// like any other: one undo puts every node back where it was.
    #[gpui::test]
    fn one_undo_restores_every_position_the_alignment_moved(cx: &mut TestAppContext) {
        let (window, project, path, _blur) = setup(cx);
        pile_up_nodes(&project, &path, 3, cx);

        let before = window
            .update(cx, |panel, _window, _cx| positions(panel))
            .unwrap();
        assert!(
            rects_overlap(
                &window
                    .update(cx, |panel, _window, _cx| drawn_rects(panel))
                    .unwrap()
            ),
            "the fixture starts with nodes on top of each other"
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.clear_selected_nodes(cx);
                panel.auto_layout_nodes(cx);
                assert_ne!(positions(panel), before, "the alignment moved something");
                assert!(!rects_overlap(&drawn_rects(panel)));
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(positions(panel), before, "one undo is the whole alignment");
            })
            .unwrap();
    }

    /// A partial selection is a promise: nothing outside it moves.
    #[gpui::test]
    fn aligning_a_selection_leaves_every_other_node_alone(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        let extra = pile_up_nodes(&project, &path, 3, cx);

        let before = window
            .update(cx, |panel, _window, _cx| positions(panel))
            .unwrap();
        let selected: HashSet<NodeId> = [blur, extra[0]].into_iter().collect();

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes(selected.clone(), cx);
                panel.auto_layout_nodes(cx);
                let after = positions(panel);
                for (id, position) in &before {
                    if selected.contains(id) {
                        continue;
                    }
                    assert_eq!(after.get(id), Some(position), "{id:?} must not move");
                }
                assert_ne!(after[&extra[0]], before[&extra[0]]);
            })
            .unwrap();
    }

    /// The case the command exists for, in the gesture the user actually
    /// makes: collapse, then press the alignment key. The collapse leaves its
    /// new subnet node *selected*, and that one-node selection has to reach
    /// the whole network or the alignment would move nothing.
    #[gpui::test]
    fn aligning_right_after_a_collapse_pulls_the_nodes_apart(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        let extra = pile_up_nodes(&project, &path, 2, cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.collapse_to_subnet(&[blur], cx);
                assert_eq!(
                    NodeEditorPanel::selected_nodes(cx).len(),
                    1,
                    "the collapse leaves its subnet node selected"
                );
                assert!(
                    rects_overlap(&drawn_rects(panel)),
                    "the collapse left the subnet node overlapping"
                );

                panel.auto_layout_nodes(cx);
                assert!(!rects_overlap(&drawn_rects(panel)));
                assert_eq!(panel.graph.nodes().count(), extra.len() + 1);
            })
            .unwrap();
    }

    // ----- edge style persistence (NGR-3) ------------------------------------

    /// The style used to live and die with the panel. It is a setting now, so
    /// a panel built after the choice starts on it.
    #[gpui::test]
    fn the_chosen_edge_style_outlives_the_panel_that_chose_it(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);
        cx.update(|cx| {
            crate::app_settings::install(crate::app_settings::GlobalSettingsFile::default(), cx)
        });

        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(panel.edge_style, EdgeStyle::Bezier, "the default is Bezier");
                panel.set_edge_style(EdgeStyle::Step, cx);
            })
            .unwrap();

        // A second panel is what "close it and open it again" amounts to.
        let reopened = cx.add_window(|window, cx| {
            NodeEditorPanel::new(ravel_ui::layout::PanelInstanceId(1), window, cx)
        });
        reopened
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.edge_style, EdgeStyle::Step);
            })
            .unwrap();
    }

    /// The Collapse / Extract items are offered on every node and only ever
    /// disabled, so what the menu shows is what the core would accept.
    #[test]
    fn subnet_menu_items_track_what_the_core_accepts() {
        let interface = Node::new(NodeId::new(1), ravel_core::network::NET_IN_TYPE_KEY)
            .with_output(ravel_core::network::PORT_TIME, DataTypeId::SCALAR);
        let plain = Node::new(NodeId::new(2), "blur").with_input("a", &[DataTypeId::SCALAR]);
        let mut subnet = Node::new(NodeId::new(3), ravel_core::network::SUBNET_TYPE_KEY);
        ravel_core::network::seed_subnet_node(&mut subnet);
        let hollow = Node::new(NodeId::new(4), ravel_core::network::SUBNET_TYPE_KEY);
        let graph = Graph::new()
            .add_node(interface)
            .unwrap()
            .add_node(plain)
            .unwrap()
            .add_node(subnet)
            .unwrap()
            .add_node(hollow)
            .unwrap();

        // A boundary node alone gives the collapse nothing to move.
        let model = subnet_menu_model(&graph, &[NodeId::new(1)]);
        assert!(!model.collapse);
        assert_eq!(model.extract, None);

        // An ordinary node collapses; it is not a subnet, so it cannot be
        // extracted.
        let model = subnet_menu_model(&graph, &[NodeId::new(2)]);
        assert!(model.collapse);
        assert_eq!(model.extract, None);

        // A subnet node is both: it can be nested one level deeper, and it
        // can be opened up.
        let model = subnet_menu_model(&graph, &[NodeId::new(3)]);
        assert!(model.collapse);
        assert_eq!(model.extract, Some(NodeId::new(3)));

        // A `subnet` node without an inner graph has nothing to give back.
        assert_eq!(subnet_menu_model(&graph, &[NodeId::new(4)]).extract, None);

        // Extract names one node, so several targets give no answer.
        assert_eq!(
            subnet_menu_model(&graph, &[NodeId::new(2), NodeId::new(3)]).extract,
            None
        );
    }

    /// A layer network whose In node carries two custom scalar ports, each
    /// wired to its own input of a sink node. Returns the In node and the
    /// sink.
    fn setup_custom_ports(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<NodeEditorPanel>,
        Entity<ProjectState>,
        NetworkPath,
        NodeId,
        NodeId,
    ) {
        let (window, project, path, _blur) = setup(cx);
        let in_id = NodeId::next();
        let sink_id = NodeId::next();
        project.update(cx, |project, cx| {
            let graph = resolve_network(project.document(), &path).unwrap().clone();
            // Both built-in outputs are declared up front: load-time
            // normalization appends a missing frame index, and an In node
            // that grew one behind the test's back would shift every index
            // the assertions name.
            let in_node = Node::new(in_id, ravel_core::network::NET_IN_TYPE_KEY)
                .with_output(
                    ravel_core::network::PORT_BASE_GEOMETRY,
                    DataTypeId::GEOMETRY,
                )
                .with_output(ravel_core::network::PORT_FRAME_INDEX, DataTypeId::SCALAR)
                .with_output("amount", DataTypeId::SCALAR)
                .with_param("amount", ParameterValue::Float(1.0))
                .with_output("gain", DataTypeId::SCALAR)
                .with_param("gain", ParameterValue::Float(1.0));
            let sink = Node::new(sink_id, "test")
                .with_input("a", &[DataTypeId::SCALAR])
                .with_input("b", &[DataTypeId::SCALAR]);
            let graph = graph
                .add_node(in_node)
                .unwrap()
                .add_node(sink)
                .unwrap()
                .add_edge(
                    EdgeId::next(),
                    in_id,
                    OutputPortIndex(2),
                    sink_id,
                    InputPortIndex(0),
                )
                .unwrap()
                .add_edge(
                    EdgeId::next(),
                    in_id,
                    OutputPortIndex(3),
                    sink_id,
                    InputPortIndex(1),
                )
                .unwrap();
            let doc = replace_network(project.document(), &path, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        window
            .update(cx, |panel, _window, cx| panel.refresh_from_document(cx))
            .unwrap();
        (window, project, path, in_id, sink_id)
    }

    /// The menu model a right-click on the In node's output port `name`
    /// carries into the Delete item.
    fn in_port_model(in_id: NodeId, name: &str) -> PortMenuModel {
        PortMenuModel {
            node_id: in_id,
            side: PortSide::Output,
            name: name.to_string(),
            enabled: true,
        }
    }

    /// The In node's custom ports, in declaration order.
    fn custom_port_names(panel: &NodeEditorPanel, in_id: NodeId) -> Vec<String> {
        panel
            .graph
            .node(in_id)
            .expect("the In node")
            .outputs
            .iter()
            .map(|port| port.name.clone())
            .collect()
    }

    /// Deleting a custom port takes its edges with it and leaves every other
    /// port's wiring intact — the remaining port moves down an index, and the
    /// edge has to move with it.
    #[gpui::test]
    fn deleting_a_custom_port_drops_its_edges_and_keeps_the_rest(cx: &mut TestAppContext) {
        let (window, project, _path, in_id, sink_id) = setup_custom_ports(cx);

        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(panel.graph.edges().count(), 2);
                panel.delete_port_from_menu(&in_port_model(in_id, "amount"), cx);

                assert_eq!(panel.port_error, None, "the delete was accepted");
                assert_eq!(
                    custom_port_names(panel, in_id),
                    vec![
                        ravel_core::network::PORT_BASE_GEOMETRY.to_string(),
                        ravel_core::network::PORT_FRAME_INDEX.to_string(),
                        "gain".to_string()
                    ]
                );
                assert!(
                    !panel
                        .graph
                        .node(in_id)
                        .unwrap()
                        .parameters
                        .iter()
                        .any(|p| p.key == "amount"),
                    "the port's parameter goes with it"
                );

                // One edge left: the surviving port's, re-pointed at its new
                // output index.
                let edges: Vec<_> = panel.graph.edges().collect();
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].source, in_id);
                assert_eq!(edges[0].source_port, OutputPortIndex(2));
                assert_eq!(edges[0].target, sink_id);
                assert_eq!(edges[0].target_port, InputPortIndex(1));
            })
            .unwrap();

        // Port, parameter and edge came back in one undo step.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    custom_port_names(panel, in_id),
                    vec![
                        ravel_core::network::PORT_BASE_GEOMETRY.to_string(),
                        ravel_core::network::PORT_FRAME_INDEX.to_string(),
                        "amount".to_string(),
                        "gain".to_string()
                    ]
                );
                assert_eq!(panel.graph.edges().count(), 2);
            })
            .unwrap();
    }

    /// A port edit that changes nothing does not reach the document: no undo
    /// step, no dirty flag. Setting the group a port already has is the case
    /// the Properties row produces twice per commit (Enter, then blur), and a
    /// move that runs into a fixed neighbour is the other one.
    #[gpui::test]
    fn a_port_edit_that_changes_nothing_pushes_no_undo_step(cx: &mut TestAppContext) {
        let (window, project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel
                    .set_custom_port_group(in_id, "amount", "Size", cx)
                    .expect("assign the group");
            })
            .unwrap();
        let after_assign = project.read_with(cx, |project, _| project.mirror_epoch());

        window
            .update(cx, |panel, _window, cx| {
                // The same group again: `network::set_custom_port_group` hands
                // back the graph it was given.
                panel
                    .set_custom_port_group(in_id, "amount", "Size", cx)
                    .expect("the repeat is accepted");
                // And a move that cannot go anywhere: `amount` is already as
                // far forward as a custom port may sit.
                panel
                    .move_custom_port(in_id, "amount", -1, cx)
                    .expect("the blocked move is accepted");
            })
            .unwrap();

        assert_eq!(
            project.read_with(cx, |project, _| project.mirror_epoch()),
            after_assign,
            "neither no-op reached the document"
        );

        // One undo returns to no group at all — proof the repeat did not
        // stack a second step on top of the assignment.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.graph.node(in_id).unwrap().param_groups.is_empty(),
                    "the assignment was the only step"
                );
            })
            .unwrap();
    }

    /// Renaming a custom port from the menu keeps the port where it is, and
    /// with it every edge; one undo puts the old name back.
    #[gpui::test]
    fn renaming_a_custom_port_keeps_its_index_and_edges(cx: &mut TestAppContext) {
        let (window, project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, window, cx| {
                panel
                    .begin_port_rename(in_id, "amount".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists, so the editor opens");
                panel.commit_port_rename("strength".into(), cx);

                assert_eq!(panel.port_error, None);
                assert!(panel.port_rename.is_none(), "a commit closes the editor");
                assert_eq!(
                    custom_port_names(panel, in_id),
                    vec![
                        ravel_core::network::PORT_BASE_GEOMETRY.to_string(),
                        ravel_core::network::PORT_FRAME_INDEX.to_string(),
                        "strength".to_string(),
                        "gain".to_string()
                    ]
                );
                assert!(
                    panel
                        .graph
                        .node(in_id)
                        .unwrap()
                        .parameters
                        .iter()
                        .any(|p| p.key == "strength"),
                    "the parameter is carried along"
                );
                assert_eq!(panel.graph.edges().count(), 2);

                // The blur that follows Enter reports the same text; the
                // closed editor swallows it instead of renaming twice.
                panel.commit_port_rename("strength".into(), cx);
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    custom_port_names(panel, in_id).contains(&"amount".to_string()),
                    "one undo is enough, so only one rename was committed"
                );
            })
            .unwrap();
    }

    /// A refused rename says why and keeps the editor open, so the name can be
    /// corrected without reopening the menu. The blur that follows the refused
    /// Enter must not repeat the same failure.
    #[gpui::test]
    fn a_refused_port_rename_keeps_the_editor_open_with_its_reason(cx: &mut TestAppContext) {
        let (window, _project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, window, cx| {
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");

                // A name the sibling port already holds.
                panel.commit_port_rename("amount".into(), cx);
                assert_eq!(
                    panel.port_error.as_deref(),
                    Some(ravel_i18n::translate("properties.ports.error.duplicate").as_str())
                );
                assert!(
                    panel.port_rename.is_some(),
                    "the editor stays open to be corrected"
                );
                assert!(custom_port_names(panel, in_id).contains(&"gain".to_string()));

                // The blur behind the refused Enter carries the same text.
                panel.commit_port_rename("amount".into(), cx);
                assert!(
                    panel.port_rename.is_none(),
                    "the repeat closes the editor rather than failing again"
                );
                assert!(panel.port_error.is_some(), "and the reason stays readable");
            })
            .unwrap();

        // A retry under another name goes through, and clears the notice.
        window
            .update(cx, |panel, window, cx| {
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                assert_eq!(panel.port_error, None, "a new edit clears the notice");
                panel.commit_port_rename("volume".into(), cx);
                assert_eq!(panel.port_error, None);
                assert!(custom_port_names(panel, in_id).contains(&"volume".to_string()));
            })
            .unwrap();
    }

    /// Escape abandons the rename, and a port that disappears under an open
    /// editor closes it — a later blur then has nothing to commit to.
    #[gpui::test]
    fn a_port_rename_is_abandoned_by_escape_and_by_a_vanished_port(cx: &mut TestAppContext) {
        let (window, _project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, window, cx| {
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                panel.cancel_port_rename(cx);
                assert!(panel.port_rename.is_none());
                assert!(custom_port_names(panel, in_id).contains(&"gain".to_string()));

                // Now delete the port under an open editor.
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                panel.delete_port_from_menu(&in_port_model(in_id, "gain"), cx);
                assert!(panel.port_rename.is_none());

                // The Input's late blur finds no editor and renames nothing.
                panel.commit_port_rename("volume".into(), cx);
                assert!(
                    !custom_port_names(panel, in_id).contains(&"volume".to_string()),
                    "a late blur must not resurrect the port"
                );
            })
            .unwrap();
    }

    /// Every edge names its ports by index, and the panel must never be able
    /// to write one that is out of range.
    fn assert_edge_indices_in_range(panel: &NodeEditorPanel) {
        for edge in panel.graph.edges() {
            let source = panel.graph.node(edge.source).expect("edge source");
            assert!(
                (edge.source_port.0 as usize) < source.outputs.len(),
                "edge {:?} points past the source's output list",
                edge.id
            );
            let target = panel.graph.node(edge.target).expect("edge target");
            assert!(
                (edge.target_port.0 as usize) < target.inputs.len(),
                "edge {:?} points past the target's input list",
                edge.id
            );
        }
    }

    /// A wire drag holds its source port by index, and `Graph::add_edge`
    /// checks neither the index nor the type — dropping through a stale
    /// `PortHit` would write an edge to a slot nothing reads instead of
    /// failing. So a port list that changes under a live drag cancels it,
    /// whether the change came from this panel's own menu or arrived from the
    /// document.
    #[gpui::test]
    fn a_live_wire_drag_is_cancelled_when_the_port_list_moves(cx: &mut TestAppContext) {
        let (window, project, _path, in_id, _sink) = setup_custom_ports(cx);

        // Dragging from the last custom output — the one an earlier removal
        // reindexes.
        let drag_from_gain = |panel: &mut NodeEditorPanel| {
            let index = panel
                .graph
                .node(in_id)
                .unwrap()
                .outputs
                .iter()
                .position(|p| p.name == "gain")
                .expect("the gain port") as u32;
            panel.drag = DragMode::Connect {
                from: PortHit {
                    node_id: in_id,
                    is_output: true,
                    port_index: index,
                    center: (0.0, 0.0),
                },
                to_point: (0.0, 0.0),
                snap: None,
            };
        };

        // 1. An edit made here. The port edit writes straight into
        //    `self.graph`, so the document observer finds nothing to re-sync
        //    and only the edit funnel can catch this.
        window
            .update(cx, |panel, _window, cx| {
                drag_from_gain(panel);
                panel.delete_port_from_menu(&in_port_model(in_id, "amount"), cx);
                assert!(
                    matches!(panel.drag, DragMode::None),
                    "the drag's port index no longer names the port it started on"
                );
                assert_edge_indices_in_range(panel);
            })
            .unwrap();

        // 2. An edit that arrives from the document (undo here, the Properties
        //    Ports section or another window in practice).
        window
            .update(cx, |panel, _window, _cx| drag_from_gain(panel))
            .unwrap();
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    matches!(panel.drag, DragMode::None),
                    "a document change reindexed the ports under the drag"
                );
                assert_edge_indices_in_range(panel);
            })
            .unwrap();
    }

    /// A pan or a box selection holds no port index, so an unrelated port edit
    /// leaves it alone — cancelling every gesture would make the Properties
    /// panel interrupt work in the node editor for no reason.
    #[gpui::test]
    fn a_port_edit_leaves_gestures_without_port_indices_alone(cx: &mut TestAppContext) {
        let (window, _project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.drag = DragMode::SelectBox {
                    start: (0.0, 0.0),
                    current: (10.0, 10.0),
                };
                panel.delete_port_from_menu(&in_port_model(in_id, "amount"), cx);
                assert!(matches!(panel.drag, DragMode::SelectBox { .. }));
            })
            .unwrap();
    }

    /// The Delete item carries the name the menu was built with, and the click
    /// that runs it blurs an open rename editor first — so the name can be
    /// stale by the time it executes. Deleting whatever now sits at that slot
    /// would be a destructive guess, so a name that is gone is a no-op, and a
    /// no-op is not something to report.
    #[gpui::test]
    fn deleting_a_port_that_was_renamed_first_does_nothing_quietly(cx: &mut TestAppContext) {
        let (window, _project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, window, cx| {
                // The menu is built while the port is still called "gain".
                let menu_model = in_port_model(in_id, "gain");

                // Clicking the item blurs the editor, which renames it.
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                panel.commit_port_rename("volume".into(), cx);
                assert!(custom_port_names(panel, in_id).contains(&"volume".to_string()));

                let before = custom_port_names(panel, in_id);
                panel.delete_port_from_menu(&menu_model, cx);
                assert_eq!(
                    custom_port_names(panel, in_id),
                    before,
                    "the renamed port is not the one the user pointed at by name"
                );
                assert_eq!(panel.port_error, None, "and nothing went wrong to report");
            })
            .unwrap();
    }

    /// Closing the rename editor moves focus back to the canvas — but only
    /// when the Input being dropped is what holds it. An editor torn down
    /// after the user has already clicked into another panel must not pull the
    /// focus back out of it (`.agents/rules/gpui.md`, focus ownership).
    ///
    /// Only the negative half is covered here: focusing an `InputState` needs
    /// a real platform window, which the test harness does not provide.
    #[gpui::test]
    fn closing_a_rename_editor_that_has_no_focus_does_not_take_any(cx: &mut TestAppContext) {
        let (window, _project, _path, in_id, _sink) = setup_custom_ports(cx);

        let before = window
            .update(cx, |panel, window, cx| {
                let before = window.focused(cx);
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                // Nothing focused the editor: opening one never grabs focus by
                // itself, the click that opens it does.
                panel.cancel_port_rename(cx);
                before
            })
            .unwrap();
        cx.update(|_cx| {});
        assert_eq!(
            window
                .update(cx, |_, window, cx| window.focused(cx))
                .unwrap(),
            before,
            "the teardown left the focus where it was"
        );
    }

    /// The refusal notice describes a graph state. A document change replaces
    /// that state, so the message goes with it instead of hanging over an
    /// editor it no longer applies to.
    #[gpui::test]
    fn a_document_change_clears_the_port_refusal_notice(cx: &mut TestAppContext) {
        let (window, project, _path, in_id, _sink) = setup_custom_ports(cx);

        window
            .update(cx, |panel, window, cx| {
                panel
                    .begin_port_rename(in_id, "gain".into(), (10.0, 20.0), window, cx)
                    .expect("the port exists");
                panel.commit_port_rename("amount".into(), cx);
                assert!(panel.port_error.is_some(), "a duplicate name is refused");
            })
            .unwrap();

        // An edit arrives from the document — here the undo of the commit that
        // built the ports, in practice any edit from another panel or window.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.port_error, None);
                assert!(
                    panel.port_rename.is_none(),
                    "and the editor it belonged to closed with it"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn duplicate_does_not_replace_the_copy_clipboard(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);
        let other = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes(HashSet::from([blur]), cx);
                panel.copy_selected(cx);

                let graph = panel
                    .graph
                    .clone()
                    .add_node(Node::new(other, "math.scalar"))
                    .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.set_selected_nodes(HashSet::from([other]), cx);
                panel.duplicate_selected(cx);

                let clipboard = panel.clipboard.as_ref().expect("copy remains available");
                assert_eq!(clipboard.nodes.len(), 1);
                assert_eq!(clipboard.nodes[0].id, blur);

                panel.paste((20.0, 20.0), cx);
                assert_eq!(
                    panel
                        .graph
                        .nodes()
                        .filter(|node| node.type_key == "blur")
                        .count(),
                    2,
                    "Paste still duplicates the node copied before Duplicate"
                );
                assert_eq!(
                    panel
                        .graph
                        .nodes()
                        .filter(|node| node.type_key == "math.scalar")
                        .count(),
                    2,
                    "Duplicate created the independently selected node"
                );
            })
            .unwrap();
    }

    /// Every node id in a graph hierarchy, the subnet graphs included.
    fn hierarchy_ids(graph: &Graph, out: &mut HashSet<NodeId>) {
        for node in graph.nodes() {
            out.insert(node.id);
            if let Some(inner) = node.subnet.as_deref() {
                hierarchy_ids(inner, out);
            }
        }
    }

    /// Pasting a subnet has to renumber the graph it owns, at every depth.
    /// `NodeId`s are global and the evaluator keys its processor registry by
    /// the bare id, so a copy whose inner nodes kept their ids would make the
    /// original and the copy fight over one entry.
    #[gpui::test]
    fn pasting_a_subnet_renumbers_its_whole_inner_hierarchy(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        let deep = Node::new(NodeId::next(), "math.scalar");
        let deep_id = deep.id;
        let inner_inner = Graph::new().add_node(deep).unwrap();
        let mid = Node::new(NodeId::next(), "subnet").with_subnet(inner_inner);
        let mid_id = mid.id;
        let inner = Graph::new().add_node(mid).unwrap();
        let outer_id = NodeId::next();

        let (original, pasted) = window
            .update(cx, |panel, _window, cx| {
                let graph = panel
                    .graph
                    .clone()
                    .add_node(Node::new(outer_id, "subnet").with_subnet(inner))
                    .unwrap();
                panel.commit_graph(graph, None, cx);

                panel.set_selected_nodes(HashSet::from([outer_id]), cx);
                panel.copy_selected(cx);
                panel.paste((20.0, 20.0), cx);

                let subnets: Vec<NodeId> = panel
                    .graph
                    .nodes()
                    .filter(|node| node.type_key == "subnet")
                    .map(|node| node.id)
                    .collect();
                assert_eq!(subnets.len(), 2, "the paste added a second subnet node");
                let copy_id = *subnets
                    .iter()
                    .find(|id| **id != outer_id)
                    .expect("the copy is the subnet that is not the original");

                let ids = |root: NodeId| {
                    let mut set = HashSet::new();
                    let node = panel.graph.node(root).expect("subnet node");
                    set.insert(node.id);
                    hierarchy_ids(node.subnet.as_deref().expect("inner graph"), &mut set);
                    set
                };
                (ids(outer_id), ids(copy_id))
            })
            .unwrap();

        assert_eq!(original.len(), 3, "outer + mid + deep");
        assert_eq!(pasted.len(), 3);
        assert!(
            original.is_disjoint(&pasted),
            "the copy shares no node id with the original at any depth \
             (original {original:?}, copy {pasted:?})"
        );
        assert!(!pasted.contains(&mid_id) && !pasted.contains(&deep_id));
    }

    /// A pasted subnet's inner edges point at the pasted inner nodes.
    #[gpui::test]
    fn pasting_a_subnet_repoints_its_inner_edges(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        let source =
            Node::new(NodeId::next(), "math.scalar").with_output("out", DataTypeId::SCALAR);
        let source_id = source.id;
        let sink = Node::new(NodeId::next(), "math.scalar").with_input("in", &[DataTypeId::SCALAR]);
        let sink_id = sink.id;
        let inner = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(sink)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                sink_id,
                InputPortIndex(0),
            )
            .unwrap();
        let outer_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let graph = panel
                    .graph
                    .clone()
                    .add_node(Node::new(outer_id, "subnet").with_subnet(inner))
                    .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.set_selected_nodes(HashSet::from([outer_id]), cx);
                panel.copy_selected(cx);
                panel.paste((20.0, 20.0), cx);

                let copy = panel
                    .graph
                    .nodes()
                    .find(|node| node.type_key == "subnet" && node.id != outer_id)
                    .expect("the pasted subnet");
                let copied_inner = copy.subnet.as_deref().expect("inner graph");
                let edge = copied_inner
                    .edges()
                    .next()
                    .expect("the inner edge survived");
                assert!(
                    copied_inner.node(edge.source).is_some()
                        && copied_inner.node(edge.target).is_some(),
                    "the inner edge stays inside the copy"
                );
                assert_ne!(edge.source, source_id);
                assert_ne!(edge.target, sink_id);
            })
            .unwrap();
    }

    /// A parameter inside a pasted subnet that is driven by another node
    /// inside the same subnet has to follow the copy.
    ///
    /// `ChannelSource::NodeOutput` is a second way a graph names a node, one
    /// the edge list does not carry, and it is remapped by the same pass that
    /// renumbers the nodes. Left pointing at the original, the copy would be
    /// animated by a node in a different subnet — a dependency the user cannot
    /// see anywhere in the network they are looking at.
    #[gpui::test]
    fn pasting_a_subnet_repoints_its_inner_node_output_bindings(cx: &mut TestAppContext) {
        let (window, _project, _path, _blur) = setup(cx);

        let driver =
            Node::new(NodeId::next(), "math.scalar").with_output("out", DataTypeId::SCALAR);
        let driver_id = driver.id;
        let driven = Node::new(NodeId::next(), "math.scalar").with_param(
            "value",
            ParameterValue::Channel(ravel_core::animation::channel::AnimationChannel::new(
                ravel_core::animation::channel::ChannelSource::NodeOutput(
                    driver_id,
                    OutputPortIndex(0),
                ),
            )),
        );
        let inner = Graph::new()
            .add_node(driver)
            .unwrap()
            .add_node(driven)
            .unwrap();
        let outer_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                let graph = panel
                    .graph
                    .clone()
                    .add_node(Node::new(outer_id, "subnet").with_subnet(inner))
                    .unwrap();
                panel.commit_graph(graph, None, cx);
                panel.set_selected_nodes(HashSet::from([outer_id]), cx);
                panel.copy_selected(cx);
                panel.paste((20.0, 20.0), cx);

                let copy = panel
                    .graph
                    .nodes()
                    .find(|node| node.type_key == "subnet" && node.id != outer_id)
                    .expect("the pasted subnet");
                let copied_inner = copy.subnet.as_deref().expect("inner graph");
                // Found by shape, not by id: an id-keyed lookup would fail on
                // the renumbering this test is not about and never reach the
                // binding assertion below.
                let bound = copied_inner
                    .nodes()
                    .find(|node| !node.parameters.is_empty())
                    .expect("the driven node came across");
                let source_kind = match &bound.parameters[0].value {
                    ParameterValue::Channel(channel) => channel.source.clone(),
                    other => panic!("unexpected parameter: {other:?}"),
                };
                let ravel_core::animation::channel::ChannelSource::NodeOutput(source, port) =
                    source_kind
                else {
                    panic!("the binding collapsed instead of being remapped");
                };
                assert_eq!(port, OutputPortIndex(0));
                assert_ne!(source, driver_id, "the copy does not name the original");
                assert!(
                    copied_inner.node(source).is_some(),
                    "it names the driver inside the copy"
                );
            })
            .unwrap();
    }

    /// The `CanvasSelection` global reflects every selection mutation that
    /// the node editor performs (click, clear, delete, duplicate, network
    /// switch). External consumers will read this global for bbox/tool
    /// overlay.
    #[gpui::test]
    fn canvas_selection_global_tracks_node_editor_selection(cx: &mut TestAppContext) {
        let (window, _project, path, blur) = setup(cx);

        let read_sel = |cx: &mut TestAppContext| {
            cx.read(|cx| {
                cx.try_global::<crate::panels::CanvasSelection>()
                    .cloned()
                    .unwrap_or_default()
            })
        };

        // Initially empty after open_network.
        let sel = read_sel(cx);
        assert!(sel.nodes.is_empty());
        assert_eq!(sel.path.as_ref(), Some(&path));

        // Programmatic selection propagates to the global.
        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes([blur].into_iter().collect(), cx);
            })
            .unwrap();
        let sel = read_sel(cx);
        assert_eq!(sel.nodes.len(), 1);
        assert!(sel.nodes.contains(&blur));

        // Delete clears the global.
        window
            .update(cx, |panel, _window, cx| {
                panel.delete_selected(cx);
            })
            .unwrap();
        let sel = read_sel(cx);
        assert!(sel.nodes.is_empty());
    }

    /// The node ids the Properties target carries are ordered by id, not by
    /// whatever order the selection's `HashSet` happens to iterate in.
    ///
    /// Consumers follow `ids.first()`: Properties resolves the keyframe target
    /// from it, and the request assembly declares the scoped evaluation target
    /// an inspection panel reads from it. An unsorted publication makes that
    /// "first" node vary between runs of the same selection — and differ from
    /// the Viewer's publication of the *same* nodes, which has always sorted.
    #[gpui::test]
    fn the_published_properties_selection_is_ordered_by_node_id(cx: &mut TestAppContext) {
        let (window, _project, path, _blur) = setup(cx);
        // Ids that are not in ascending order to begin with, so "sorted" and
        // "as they were written down" cannot be the same answer.
        let ids: Vec<NodeId> = [907, 431, 998, 102, 555, 880, 213, 660]
            .into_iter()
            .map(NodeId::new)
            .collect();
        let selection: HashSet<NodeId> = ids.iter().copied().collect();
        let mut expected = ids.clone();
        expected.sort_by_key(|id| id.raw());
        // Vacuity guard: publishing the set unsorted would hand over its hash
        // order, so a run where that order is already ascending proves nothing
        // and has to say so rather than pass.
        let hash_order: Vec<NodeId> = selection.iter().copied().collect();
        assert_ne!(
            hash_order, expected,
            "this run's hash order is already sorted, so it cannot tell a \
             sorted publication from an unsorted one",
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes(selection.clone(), cx);
                panel.notify_properties_selection(cx);

                let crate::panels::PropertiesTarget::Nodes {
                    network,
                    ids: published,
                } = &cx.global::<crate::panels::SelectedPropertiesTarget>().0
                else {
                    panic!("the node selection was not published as a node target");
                };
                assert_eq!(network, &path);
                assert_eq!(published, &expected);
                assert!(
                    published.windows(2).all(|w| w[0].raw() < w[1].raw()),
                    "the published ids are not strictly ascending: {published:?}",
                );
            })
            .unwrap();
    }

    /// `selection_matches` is what lets `refresh_from_document` skip
    /// republishing an identical `CanvasSelection` on every graph change
    /// (HIGH-07). It has to require the network context *and* the node set to
    /// agree: either alone would let a selection from another network read as
    /// a match and suppress a publish that was needed.
    #[gpui::test]
    fn selection_matches_requires_both_context_and_node_set(cx: &mut TestAppContext) {
        let (window, _project, path, blur) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                // `open_network` in `setup` leaves the published selection
                // empty in this context.
                assert!(panel.selection_matches(&HashSet::new(), cx));
                assert!(!panel.selection_matches(&[blur].into_iter().collect(), cx));

                panel.set_selected_nodes([blur].into_iter().collect(), cx);
                assert!(panel.selection_matches(&[blur].into_iter().collect(), cx));
                // Same context, different node set.
                assert!(!panel.selection_matches(&HashSet::new(), cx));

                // Same node ids published under a different network's path
                // must not read as a match against this panel's open network.
                cx.set_global(crate::panels::CanvasSelection {
                    path: Some(NetworkPath::layer(path.comp, LayerId::next())),
                    nodes: [blur].into_iter().collect(),
                });
                assert!(!panel.selection_matches(&[blur].into_iter().collect(), cx));
            })
            .unwrap();
    }

    /// The other side of the HIGH-07 guard: it may only suppress a *no-op*
    /// republish. When a document change actually prunes the selected node out
    /// of the graph, the panel still has to publish the shrunken selection —
    /// the Viewer and Outliner read that global to drop gesture targets and
    /// highlighting for a node that no longer exists.
    #[gpui::test]
    fn pruning_the_selected_node_still_republishes_the_selection(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes([blur].into_iter().collect(), cx);
            })
            .unwrap();

        // Counts publications, not value changes: `set_global` wakes observers
        // on every call, which is the cost the guard removes.
        struct SelectionProbe {
            publishes: usize,
            _sub: Subscription,
        }
        let probe = cx.new(|cx| SelectionProbe {
            publishes: 0,
            _sub: cx.observe_global::<crate::panels::CanvasSelection>(
                |this: &mut SelectionProbe, _cx| this.publishes += 1,
            ),
        });
        cx.run_until_parked();
        let before = probe.read_with(cx, |probe, _| probe.publishes);

        // Replace the layer's network with an empty graph: the same network
        // stays open, but the selected node is gone from it.
        project.update(cx, |project, cx| {
            let document =
                ravel_ui::document::replace_network(project.document(), &path, Graph::new())
                    .unwrap();
            project.apply_document(document, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        assert!(
            probe.read_with(cx, |probe, _| probe.publishes) > before,
            "pruning the selected node out of the graph must republish"
        );
        let sel = cx.read(|cx| {
            cx.try_global::<crate::panels::CanvasSelection>()
                .cloned()
                .unwrap_or_default()
        });
        assert!(
            sel.nodes.is_empty(),
            "the pruned node must not remain in the published selection"
        );
    }

    /// A rubber band publishes only when the set it encloses actually
    /// changes. Every mouse move recomputes that set, and republishing an
    /// unchanged one wakes the Viewer and Outliner through `CanvasSelection`
    /// and makes the Properties panel re-resolve every section through
    /// `SelectedPropertiesTarget` — the visible thrash while dragging a band.
    #[gpui::test]
    fn a_band_move_over_the_same_nodes_publishes_nothing(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        struct Probe {
            publishes: usize,
            _sub: Subscription,
        }
        let selection = cx.new(|cx| Probe {
            publishes: 0,
            _sub: cx.observe_global::<crate::panels::CanvasSelection>(|this: &mut Probe, _cx| {
                this.publishes += 1
            }),
        });
        let properties = cx.new(|cx| Probe {
            publishes: 0,
            _sub: cx.observe_global::<crate::panels::SelectedPropertiesTarget>(
                |this: &mut Probe, _cx| this.publishes += 1,
            ),
        });
        cx.run_until_parked();
        let counts = |cx: &mut TestAppContext| {
            (
                selection.read_with(cx, |probe, _| probe.publishes),
                properties.read_with(cx, |probe, _| probe.publishes),
            )
        };
        let before = counts(cx);

        // The band first encloses the node: both globals move.
        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.publish_band_selection([blur].into_iter().collect(), cx));
            })
            .unwrap();
        cx.run_until_parked();
        let after_first = counts(cx);
        assert!(
            after_first.0 > before.0 && after_first.1 > before.1,
            "the first enclosing move publishes both globals"
        );

        // Every later move that still encloses exactly the same node is
        // silent — including the Properties target, which is what thrashed.
        window
            .update(cx, |panel, _window, cx| {
                for _ in 0..5 {
                    assert!(!panel.publish_band_selection([blur].into_iter().collect(), cx));
                }
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            counts(cx),
            after_first,
            "a band that encloses the same nodes must publish nothing"
        );

        // Shrinking the band back to nothing is a real change again.
        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.publish_band_selection(HashSet::new(), cx));
            })
            .unwrap();
        cx.run_until_parked();
        let after_empty = counts(cx);
        assert!(after_empty.0 > after_first.0 && after_empty.1 > after_first.1);
    }

    /// The guard that matters to every caller, seen on its own: the band test
    /// above stops at `publish_band_selection`, so it would still pass with
    /// the drop inside `set_selected_nodes` removed. `CanvasSelection` is a
    /// durable global — writing the set it already holds wakes the Viewer and
    /// the Outliner for nothing — so the drop lives where all callers pass.
    #[gpui::test]
    fn republishing_the_same_selection_writes_nothing(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        struct Probe {
            publishes: usize,
            _sub: Subscription,
        }
        let probe = cx.new(|cx| Probe {
            publishes: 0,
            _sub: cx.observe_global::<crate::panels::CanvasSelection>(|this: &mut Probe, _cx| {
                this.publishes += 1
            }),
        });
        cx.run_until_parked();
        let publishes = |cx: &mut TestAppContext| probe.read_with(cx, |probe, _| probe.publishes);

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes([blur].into_iter().collect(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let after_first = publishes(cx);
        assert!(after_first > 0, "the first selection is published");

        window
            .update(cx, |panel, _window, cx| {
                panel.set_selected_nodes([blur].into_iter().collect(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            publishes(cx),
            after_first,
            "the same set published twice must write the global once"
        );
    }

    /// The hover popover opens only once the dwell timer actually fires, and
    /// stays closed when a gesture is active at fire time (DISC-2). The
    /// state-machine transitions are unit-tested in
    /// `node_editor::hover_popover`; this drives the panel's real timer path.
    #[gpui::test]
    fn hover_popover_opens_after_the_dwell_and_not_during_a_gesture(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        // Arm the dwell on the blur node: before it elapses nothing opens.
        window
            .update(cx, |panel, _window, cx| {
                let (_repaint, arm) = panel.hover_popover.pointer_moved(Some(blur));
                assert!(arm, "hovering a node arms the dwell");
                panel.arm_hover_dwell(cx);
                assert_eq!(
                    panel.hover_popover.open_target(),
                    None,
                    "no popover before the dwell"
                );
            })
            .unwrap();
        cx.executor().advance_clock(HOVER_DWELL * 2);
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.hover_popover.open_target(),
                    Some(blur),
                    "the popover opens once the dwell fires"
                );
            })
            .unwrap();

        // Re-arm, then start a gesture: the timer firing mid-gesture is
        // suppressed.
        window
            .update(cx, |panel, _window, cx| {
                panel.hover_popover.cancel();
                panel.hover_popover.pointer_moved(Some(blur));
                panel.arm_hover_dwell(cx);
                panel.drag = DragMode::Pan {
                    start_mouse: (0.0, 0.0),
                    start_viewport: (0.0, 0.0),
                };
            })
            .unwrap();
        cx.executor().advance_clock(HOVER_DWELL * 2);
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.hover_popover.open_target(),
                    None,
                    "a gesture at fire time keeps the popover closed"
                );
            })
            .unwrap();
    }

    /// The popover anchor wrapper is laid out at the hovered node's
    /// bottom-left corner in window coordinates: gpui-component's Popover
    /// resolves its position from the wrapper's prepaint bounds, so the
    /// wrapper itself must land on the node — positioning only the trigger
    /// child would leave the popover opening at the canvas origin.
    #[gpui::test]
    fn the_popover_anchor_tracks_the_hovered_nodes_position(cx: &mut TestAppContext) {
        let (window, _project, _path, blur) = setup(cx);

        // Open the popover through the real dwell path.
        window
            .update(cx, |panel, _window, cx| {
                panel.hover_popover.pointer_moved(Some(blur));
                panel.arm_hover_dwell(cx);
            })
            .unwrap();
        cx.executor().advance_clock(HOVER_DWELL * 2);
        cx.run_until_parked();

        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.update(|window, _cx| window.refresh());
        visual.run_until_parked();
        let bounds = visual
            .debug_bounds("node-hover-popover-anchor")
            .expect("the anchor wrapper renders while the popover is open");

        let (expected_x, expected_y) = window
            .update(cx, |panel, _window, _cx| {
                let node = panel.graph.node(blur).expect("blur node");
                let (sx, sy) = panel
                    .viewport
                    .flow_to_screen(node.metadata.position.0, node.metadata.position.1);
                let (_, h) = panel
                    .node_sizes
                    .get(&blur)
                    .copied()
                    .unwrap_or((node_width(panel.viewport.zoom), 60.0));
                let (ox, oy) = panel.canvas_origin.get();
                (ox + sx, oy + sy + h)
            })
            .unwrap();

        let origin_x: f32 = bounds.origin.x.into();
        let origin_y: f32 = bounds.origin.y.into();
        assert!(
            (origin_x - expected_x).abs() < 1.0,
            "anchor x {origin_x} must match the node's left edge {expected_x}"
        );
        assert!(
            (origin_y - expected_y).abs() < 1.0,
            "anchor y {origin_y} must match the node's bottom edge {expected_y}"
        );
    }

    /// Closing the network cancels the hover popover; reopening the same
    /// network must not resurrect it without a fresh dwell — the reopened
    /// network carries the same node ids.
    #[gpui::test]
    fn closing_and_reopening_the_network_cancels_the_hover_popover(cx: &mut TestAppContext) {
        let (window, _project, path, blur) = setup(cx);
        window
            .update(cx, |panel, _window, _cx| {
                panel.hover_popover.pointer_moved(Some(blur));
                let generation = panel.hover_popover.generation();
                assert!(panel.hover_popover.dwell_elapsed(generation));
                assert_eq!(panel.hover_popover.open_target(), Some(blur));
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| panel.close_network(cx))
            .unwrap();
        window
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx)
            })
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.hover_popover.open_target(),
                    None,
                    "reopening must not resurrect the popover without a dwell"
                );
            })
            .unwrap();
    }

    /// Deleting the hovered node closes the popover instead of anchoring it
    /// to a stale id (the `refresh_from_document` prune).
    #[gpui::test]
    fn deleting_the_hovered_node_closes_the_popover(cx: &mut TestAppContext) {
        let (window, project, path, blur) = setup(cx);
        window
            .update(cx, |panel, _window, _cx| {
                panel.hover_popover.pointer_moved(Some(blur));
                let generation = panel.hover_popover.generation();
                assert!(panel.hover_popover.dwell_elapsed(generation));
                assert_eq!(panel.hover_popover.open_target(), Some(blur));
            })
            .unwrap();

        project.update(cx, |project, cx| {
            let document =
                ravel_ui::document::replace_network(project.document(), &path, Graph::new())
                    .unwrap();
            project.apply_document(document, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.hover_popover.open_target(),
                    None,
                    "the popover must not anchor to a deleted node"
                );
            })
            .unwrap();
    }
}

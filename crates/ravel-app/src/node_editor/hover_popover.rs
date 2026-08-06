// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hover-dwell info popover for the node editor canvas (DISC-2).
//!
//! The canvas is custom-painted, so there is no per-node element to hang a
//! popover on. The pieces here split into three testable layers:
//!
//! - [`HoverPopover`] is a pure state machine: the panel feeds it pointer
//!   moves and gesture cancels, and a timer task reports the elapsed dwell
//!   keyed by a generation counter so a stale timer can never open the
//!   popover. It opens only after [`HOVER_DWELL`] on a stationary target and
//!   never during a gesture (node move, connection drag, rectangle select,
//!   pan) — the panel calls [`HoverPopover::cancel`] when one starts.
//! - [`hover_info`] builds the content model from the document graph alone:
//!   labels/descriptions resolve through `crate::node_locale`, and animated
//!   parameters are sampled from their stored channels at the displayed
//!   frame. No evaluation request is involved, so an open popover cannot
//!   trigger re-evaluation; during playback the values simply follow the
//!   frame the panel is redrawn for.
//! - [`hover_popover_element`] wires both to gpui-component's `Popover` in
//!   controlled mode (`.open(...)`): the panel owns open/close, so the
//!   popover never moves focus and never dismisses itself.

use gpui::{
    Anchor, AnyElement, App, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Point, RenderOnce, SharedString, Stateful, Styled, Window, div, px,
};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Icon, Selectable, h_flex, v_flex};
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::id::{DataTypeId, NodeId};
use ravel_core::registry::{NodeCategory, NodeRegistry};
use ravel_i18n::t;
use ravel_ui::properties::node::channel_display_value;
use std::time::Duration;

use crate::assets::RavelIcon;

/// Hover dwell before the popover opens (plan: "about 500 ms").
pub const HOVER_DWELL: Duration = Duration::from_millis(500);

/// Hover tracking for the canvas popover.
///
/// `generation` invalidates pending dwell timers: every target change or
/// cancel bumps it, and the timer task reports the generation it was armed
/// with — a mismatch means the hover moved on or a gesture intervened.
#[derive(Default)]
pub struct HoverPopover {
    target: Option<NodeId>,
    open: bool,
    generation: u64,
}

impl HoverPopover {
    /// The node currently hovered (whether or not the popover opened).
    pub fn target(&self) -> Option<NodeId> {
        self.target
    }

    /// The current dwell generation, for arming a timer.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The node whose popover is open, if any.
    pub fn open_target(&self) -> Option<NodeId> {
        self.open.then_some(self.target).flatten()
    }

    /// Pointer moved while idle. Returns `(repaint, arm)`: `repaint` when an
    /// open popover just closed, `arm` when the target changed to a node and
    /// a dwell timer should be (re)armed.
    pub fn pointer_moved(&mut self, node: Option<NodeId>) -> (bool, bool) {
        if self.target == node {
            return (false, false);
        }
        let was_open = self.open;
        self.target = node;
        self.open = false;
        self.generation += 1;
        (was_open, node.is_some())
    }

    /// A gesture started or the view moved under the pointer: suppress the
    /// popover and invalidate any pending dwell. Returns true when an open
    /// popover just closed (repaint needed).
    pub fn cancel(&mut self) -> bool {
        self.target = None;
        self.generation += 1;
        std::mem::take(&mut self.open)
    }

    /// The dwell timer armed at `generation` elapsed: open when the hover is
    /// still on that same target. Returns true when the popover newly opened.
    pub fn dwell_elapsed(&mut self, generation: u64) -> bool {
        if generation != self.generation || self.open || self.target.is_none() {
            return false;
        }
        self.open = true;
        true
    }
}

/// One row of the popover's port list: port name plus its data type.
pub struct PortRow {
    pub name: String,
    pub type_name: String,
}

/// One row of the parameter list: key, current value at the sampled frame,
/// and the optional localized description.
pub struct ParamRow {
    pub key: String,
    pub value: String,
    pub description: Option<String>,
}

/// Everything the popover shows about one node.
pub struct NodeHoverInfo {
    pub type_key: String,
    pub label: String,
    pub category: Option<NodeCategory>,
    pub description: Option<String>,
    pub inputs: Vec<PortRow>,
    pub outputs: Vec<PortRow>,
    pub params: Vec<ParamRow>,
}

/// Build the popover content for `node`, sampling animated channels at
/// `frame` (the owning layer's local frame, as in the Properties panel).
/// Purely document-derived: no evaluation request is issued.
///
/// `eval` supplies the vocabulary an expression-driven channel may name; it
/// is not an evaluation of the graph.
pub fn hover_info(
    node: &Node,
    registry: &NodeRegistry,
    frame: u64,
    eval: &EvalContext,
) -> NodeHoverInfo {
    NodeHoverInfo {
        type_key: node.type_key.clone(),
        label: crate::node_locale::display_label(node, registry),
        category: registry
            .get(&node.type_key)
            .map(|template| template.category),
        description: crate::node_locale::description(&node.type_key),
        inputs: node
            .inputs
            .iter()
            .map(|port| PortRow {
                name: port.name.clone(),
                type_name: data_types_name(&port.accepted_types),
            })
            .collect(),
        outputs: node
            .outputs
            .iter()
            .map(|port| PortRow {
                name: port.name.clone(),
                type_name: data_type_name(port.data_type),
            })
            .collect(),
        params: node
            .parameters
            .iter()
            .map(|param| ParamRow {
                key: param.key.clone(),
                value: param_value_display(&param.value, frame, eval),
                description: crate::node_locale::param_description(&node.type_key, &param.key),
            })
            .collect(),
    }
}

/// Localized short name of a port data type; unknown types show their raw id.
pub fn data_type_name(data_type: DataTypeId) -> String {
    let key = match data_type {
        DataTypeId::FRAME_BUFFER => "node_graph.popover.port_type.frame_buffer",
        DataTypeId::SCALAR => "node_graph.popover.port_type.scalar",
        DataTypeId::VEC2 => "node_graph.popover.port_type.vec2",
        DataTypeId::VEC3 => "node_graph.popover.port_type.vec3",
        DataTypeId::VEC4 => "node_graph.popover.port_type.vec4",
        DataTypeId::COLOR => "node_graph.popover.port_type.color",
        DataTypeId::TIME_CODE => "node_graph.popover.port_type.time_code",
        DataTypeId::AUDIO_BUFFER => "node_graph.popover.port_type.audio_buffer",
        DataTypeId::PLAIN_TEXT => "node_graph.popover.port_type.plain_text",
        DataTypeId::GEOMETRY => "node_graph.popover.port_type.geometry",
        DataTypeId::FIELD => "node_graph.popover.port_type.field",
        DataTypeId::SCENE => "node_graph.popover.port_type.scene",
        _ => return format!("#{}", data_type.raw()),
    };
    t!(key)
}

/// Display name of an input port's accepted types, joined when it accepts
/// several.
fn data_types_name(types: &[DataTypeId]) -> String {
    types
        .iter()
        .map(|ty| data_type_name(*ty))
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_float(value: f32) -> String {
    format!("{value:.2}")
}

/// Display string of a parameter value at `frame`: static values render
/// directly; animated channels are sampled from their stored curves (display
/// only — the evaluator is not involved).
fn param_value_display(value: &ParameterValue, frame: u64, eval: &EvalContext) -> String {
    match value {
        ParameterValue::Float(v) => format_float(*v),
        ParameterValue::Int(v) => v.to_string(),
        ParameterValue::Bool(v) => {
            t!(if *v {
                "node_graph.popover.bool_true"
            } else {
                "node_graph.popover.bool_false"
            })
        }
        ParameterValue::String(v) => v.clone(),
        ParameterValue::Channel(ch) => format_float(channel_display_value(ch, frame, eval)),
        ParameterValue::Channel2(chs) => format!(
            "({})",
            chs.iter()
                .map(|ch| format_float(channel_display_value(ch, frame, eval)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ParameterValue::Channel3(chs) => format!(
            "({})",
            chs.iter()
                .map(|ch| format_float(channel_display_value(ch, frame, eval)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ParameterValue::Channel4(chs) => format!(
            "({})",
            chs.iter()
                .map(|ch| format_float(channel_display_value(ch, frame, eval)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        // Path control points are edited on the canvas; the popover carries
        // the same summary as the Properties row.
        ParameterValue::PathPoints(points) => {
            format!("{} {}", points.len(), t!("node_graph.popover.points"))
        }
        ParameterValue::Curve(_) => t!("node_graph.popover.curve"),
    }
}

/// Zero-size trigger for the popover. The canvas paints its nodes itself,
/// so there is no per-node element to trigger from; `Popover` only needs a
/// trigger to exist (it renders nothing without one). Positioning is *not*
/// done here: the popover resolves its anchor from the bounds of its own
/// trigger-wrapper div (`Popover::on_prepaint`), so the wrapper — not this
/// child — must be the element placed at the node (see
/// [`hover_popover_element`]).
#[derive(IntoElement)]
struct CanvasAnchor {
    selected: bool,
}

impl Selectable for CanvasAnchor {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for CanvasAnchor {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().w_0().h_0()
    }
}

fn section_header(title: String, muted: Hsla) -> Div {
    div()
        .mt_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(muted)
        .child(SharedString::from(title))
}

fn named_value_row(name: String, value: String, muted: Hsla) -> Div {
    h_flex()
        .justify_between()
        .gap_2()
        .child(div().text_xs().child(SharedString::from(name)))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(value)),
        )
}

/// Build the popover element. `anchor` is the node's bottom-left corner in
/// canvas-area coordinates (the popover opens below it, snapping into the
/// window near the edges). Always render this element — open or closed — so
/// the keyed `PopoverState` survives across frames.
///
/// The returned element is a zero-size, absolutely positioned wrapper:
/// gpui-component's `Popover` resolves its anchor from the bounds of its
/// trigger-wrapper div at prepaint time, so the wrapper itself must be
/// laid out at the node's screen position (positioning the trigger *child*
/// would not move the wrapper, and the popover would open at the canvas
/// origin). Being zero-sized it can never intercept canvas input.
///
/// Controlled mode: the panel's hover tracking owns `open`, and
/// `overlay_closable(false)` keeps the popover from dismissing itself, so it
/// never moves focus and keyboard shortcuts keep working while it is open.
pub fn hover_popover_element(
    info: Option<&NodeHoverInfo>,
    anchor: Point<Pixels>,
    open: bool,
    cx: &App,
) -> Stateful<Div> {
    let colors = cx.theme().colors;
    let popover = Popover::new("node-hover-popover")
        .anchor(Anchor::TopLeft)
        .trigger(CanvasAnchor { selected: false })
        .open(open)
        .overlay_closable(false)
        .w(px(300.));

    let Some(info) = info else {
        return anchor_wrapper(anchor, popover);
    };

    let mut content = v_flex().gap_1();
    let mut header = h_flex()
        .gap_2()
        .items_center()
        .child(Icon::new(RavelIcon::for_node_type(
            &info.type_key,
            info.category,
        )))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(info.label.clone())),
        );
    if let Some(category) = info.category {
        header = header.child(div().text_xs().text_color(colors.muted_foreground).child(
            SharedString::from(crate::panels::node_editor::node_category_label(category)),
        ));
    }
    content = content.child(header);

    if let Some(description) = &info.description {
        content = content.child(
            div()
                .text_xs()
                .text_color(colors.muted_foreground)
                .child(SharedString::from(description.clone())),
        );
    }

    if !info.inputs.is_empty() {
        content = content.child(section_header(
            t!("node_graph.popover.inputs"),
            colors.muted_foreground,
        ));
        for row in &info.inputs {
            content = content.child(named_value_row(
                row.name.clone(),
                row.type_name.clone(),
                colors.muted_foreground,
            ));
        }
    }

    if !info.outputs.is_empty() {
        content = content.child(section_header(
            t!("node_graph.popover.outputs"),
            colors.muted_foreground,
        ));
        for row in &info.outputs {
            content = content.child(named_value_row(
                row.name.clone(),
                row.type_name.clone(),
                colors.muted_foreground,
            ));
        }
    }

    if !info.params.is_empty() {
        content = content.child(section_header(
            t!("node_graph.popover.parameters"),
            colors.muted_foreground,
        ));
        for row in &info.params {
            content = content.child(named_value_row(
                row.key.clone(),
                row.value.clone(),
                colors.muted_foreground,
            ));
            if let Some(description) = &row.description {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(SharedString::from(description.clone())),
                );
            }
        }
    }

    let content: AnyElement = content.into_any_element();
    anchor_wrapper(anchor, popover.child(content))
}

/// The zero-size, absolutely positioned wrapper that places the popover's
/// trigger-wrapper div at `anchor`. The `debug_selector` is a release noop;
/// tests read the wrapper's rendered bounds through it.
fn anchor_wrapper(anchor: Point<Pixels>, popover: Popover) -> Stateful<Div> {
    div()
        .id("node-hover-popover-anchor")
        .debug_selector(|| "node-hover-popover-anchor".into())
        .absolute()
        .left(anchor.x)
        .top(anchor.y)
        .w_0()
        .h_0()
        .child(popover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;

    /// Display context for the popover: only `fps` and the resolutions are
    /// read, and only by an expression-driven channel.
    fn eval() -> EvalContext {
        EvalContext::new(0, ravel_core::types::FrameRate::new(30, 1), (1920, 1080))
    }
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    /// Before the dwell elapses the popover must not be open.
    #[test]
    fn popover_does_not_open_before_the_dwell_elapses() {
        let mut hover = HoverPopover::default();
        let (repaint, arm) = hover.pointer_moved(Some(NodeId::new(1)));
        assert!(arm, "hovering a node arms the dwell timer");
        assert!(!repaint);
        assert_eq!(hover.open_target(), None, "no popover before the dwell");
    }

    /// A gesture starting mid-dwell cancels the hover; the timer that was
    /// already running reports a stale generation and must not open.
    #[test]
    fn popover_does_not_open_during_a_gesture() {
        let mut hover = HoverPopover::default();
        hover.pointer_moved(Some(NodeId::new(1)));
        let armed = hover.generation();
        hover.cancel();
        assert!(
            !hover.dwell_elapsed(armed),
            "the dwell firing mid-gesture stays suppressed"
        );
        assert_eq!(hover.open_target(), None);
    }

    /// An open popover closes when a gesture starts.
    #[test]
    fn a_gesture_closes_an_open_popover() {
        let mut hover = HoverPopover::default();
        hover.pointer_moved(Some(NodeId::new(1)));
        assert!(hover.dwell_elapsed(hover.generation()));
        assert_eq!(hover.open_target(), Some(NodeId::new(1)));
        assert!(hover.cancel(), "closing an open popover repaints");
        assert_eq!(hover.open_target(), None);
    }

    /// Moving to another node re-arms the dwell; the first node's timer is
    /// stale and must not open the popover over the new target.
    #[test]
    fn a_stale_dwell_timer_does_not_open_the_popover() {
        let mut hover = HoverPopover::default();
        hover.pointer_moved(Some(NodeId::new(1)));
        let stale = hover.generation();
        hover.pointer_moved(Some(NodeId::new(2)));
        assert!(!hover.dwell_elapsed(stale), "stale generation is ignored");
        assert!(hover.dwell_elapsed(hover.generation()));
        assert_eq!(hover.open_target(), Some(NodeId::new(2)));
    }

    /// Moving off the canvas closes the popover and arms nothing.
    #[test]
    fn leaving_the_node_closes_the_popover() {
        let mut hover = HoverPopover::default();
        hover.pointer_moved(Some(NodeId::new(1)));
        hover.dwell_elapsed(hover.generation());
        let (repaint, arm) = hover.pointer_moved(None);
        assert!(repaint, "closing the open popover repaints");
        assert!(!arm, "empty canvas arms no timer");
        assert_eq!(hover.open_target(), None);
    }

    /// A type without a locale description omits the description section
    /// rather than showing a fallback.
    #[test]
    fn hover_info_omits_the_description_when_the_type_has_none() {
        let node = Node::new(NodeId::new(1), "plugin.unknown")
            .with_param("strength", ParameterValue::Float(1.0));
        let info = hover_info(&node, &registry(), 0, &eval());
        assert_eq!(info.description, None);
        assert_eq!(info.params[0].description, None);
        assert_eq!(info.label, "plugin.unknown", "label falls back to the key");
    }

    /// Ports list names with their data types. Store-agnostic: whatever the
    /// i18n state, the rows go through `data_type_name`. The positive
    /// direction with an actual catalog loaded is covered by the
    /// `node_hover_popover` integration test — initializing the global i18n
    /// store here would leak into every other test of this binary, which
    /// runs with an empty store.
    #[test]
    fn hover_info_lists_ports_with_type_names() {
        let registry = registry();
        let node = registry
            .create_node("merge", NodeId::new(1))
            .expect("merge is registered");
        let info = hover_info(&node, &registry, 0, &eval());
        assert!(info.inputs.len() >= 2, "merge takes A and B");
        assert!(
            info.inputs
                .iter()
                .all(|row| row.type_name == data_type_name(DataTypeId::FRAME_BUFFER)),
            "merge inputs are frame buffers: {:?}",
            info.inputs.iter().map(|r| &r.type_name).collect::<Vec<_>>()
        );
        assert_eq!(info.outputs.len(), 1);
    }

    /// Animated parameters display the value sampled at the current frame
    /// from the stored curve — display-only, so building the popover content
    /// never issues an evaluation request.
    #[test]
    fn hover_info_samples_animated_params_at_the_current_frame() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let node = Node::new(NodeId::new(1), "blur").with_param(
            "radius",
            ParameterValue::Channel(AnimationChannel::keyframes(curve)),
        );
        let info = hover_info(&node, &registry(), 5, &eval());
        assert_eq!(info.params[0].value, "50.00", "sampled at frame 5");
    }

    /// Unknown data types render their raw id instead of a broken key.
    #[test]
    fn an_unknown_data_type_shows_its_raw_id() {
        assert_eq!(data_type_name(DataTypeId::new(999)), "#999");
    }
}

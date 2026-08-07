// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Properties panel — GPUI view for inspecting and editing node parameters
//! and layer shell attributes.
//!
//! The panel never stores value snapshots: the `SelectedPropertiesTarget`
//! global only identifies the target (owning composition + layer id, or
//! owning network + node ids) and the panel resolves current values from
//! the [`ProjectState`] document whenever it builds or refreshes sections.
//! It observes the `ProjectState` entity (edits, undo/redo, live gesture
//! updates) and the shared `PlaybackPosition` (animated values track the
//! playhead), refreshing values in place so in-flight scrub gestures keep
//! their widget entities.
//!
//! Node edits call the node editor directly through the durable
//! `NodeEditorHandle` registry (the editor owns the network context). The
//! call is deferred so detached panels never update an entity in another
//! window from within their own update. Layer targets edit the document
//! directly through [`ProjectState`]: shell attributes
//! (timing / transform / opacity / blend / adjustment) and the In node's
//! custom parameters (REQ-LAYER-002) map back via
//! `ravel_ui::properties::layer::apply_layer_field`, with the usual
//! scrub-gesture undo granularity (live `Change`s apply, the ending
//! `Commit` records one Document undo step).
//!
//! Curve parameters (`PropertyField::Curve`) render as one row with a
//! thumbnail that expands an inline [`ParamCurveEditor`] directly underneath
//! itself. Which rows are expanded and how tall each editor is, is **panel
//! view state**: it never reaches the [`ProjectState`] document, so expanding
//! or collapsing a row records no undo step and undo never changes it. Any
//! number of rows can be expanded at once. Point edits follow the scrub
//! gesture contract (live `Change`s apply, the ending `Commit` records one
//! Document undo step).
//!
//! Animatable fields (shell transform/opacity channels, channel-backed
//! custom parameters, node `Float`/`Channel*` parameters) carry a small
//! ◆/◇ toggle left of their label that inserts or removes a keyframe at
//! the current layer-local frame (REQ-LAYER-004). Layer toggles edit the
//! document through [`ProjectState`]; node toggles route to the node
//! editor through the `NodeEditorHandle` global.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::Sizable;
use gpui_component::accordion::Accordion;
use gpui_component::checkbox::Checkbox;
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{SelectEvent, SelectState};
use gpui_component::tooltip::Tooltip;
use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::composition::{AssetMetadata, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::exposed::{
    ExposedBinding, ExposedParameter, ExposedParameterError, ExposedParameters,
};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::id::{CompId, NodeId};
use ravel_core::network::{CustomPortType, NetworkError};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::document::{CompositionSettings, resolve_network, update_composition, update_layer};
use ravel_ui::keyframes::layer_local_frame;
use ravel_ui::properties::composition::{apply_composition_field, sections_for_composition};
use ravel_ui::properties::exposed::{ExposedRow, exposed_section};
use ravel_ui::properties::expression;
use ravel_ui::properties::layer::{
    CUSTOM_FIELD_PREFIX, apply_layer_field, in_node_id, layer_field_keyframed, sections_for_layer,
    sections_for_layers, toggle_layer_keyframe,
};
use ravel_ui::properties::node::sections_for_node;
use ravel_ui::properties::{DrivenParam, PropertyField, PropertySection, PropertyValue};
use std::sync::Arc;

use crate::assets::RavelIcon;
use crate::project_state::ProjectState;
use crate::widgets::{
    ParamCurveEditor, ParamCurveEditorState, ParamCurveEvent, ScrubEvent, ScrubInput,
    ScrubInputState, curve_thumbnail,
};

use super::{PropertiesTarget, SelectedPropertiesTarget, port_error_message};

/// Localized display label for a property field key. Custom In-node
/// parameters show their bare name; other unknown keys (dynamic node
/// parameters) fall back to the key rather than the lookup path.
fn field_label(key: &str) -> String {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        return name.to_string();
    }
    let lookup = format!("properties.field.{key}");
    let translated = ravel_i18n::translate(&lookup);
    if translated == lookup {
        key.to_string()
    } else {
        translated
    }
}

/// Display text of a read-only value. `ravel-ui` has no i18n dependency, so a
/// value that names a *state* rather than carrying data (a merged boolean of a
/// multi-layer selection, a layer's source kind) is emitted as a locale key and
/// translated here; data values (a layer name, an id, a colour) are not keys
/// and pass through.
///
/// A phrase that swallows a number ("Network (3 nodes)", "300 frames") arrives
/// as a key with the count appended, because the word order around the number
/// is the translator's to choose: the count fills the phrase's `{count}`
/// placeholder here, at the display boundary, and never by concatenation in
/// the headless crate.
///
/// `pub` for the `localized_display_text` integration test, which loads the
/// real locale catalogs (the lib unit tests run with an empty i18n store).
pub fn read_only_value(value: &str) -> String {
    if let Some((key, count)) = ravel_ui::properties::split_counted_value(value) {
        let translated = ravel_i18n::translate(key);
        return translated.replace("{count}", count);
    }
    let translated = ravel_i18n::translate(value);
    if translated == value {
        value.to_string()
    } else {
        translated
    }
}

/// Display text of one option of an [`PropertyField::Enum`] row.
///
/// Enum options are stored values, so most of them are data (`Normal`,
/// `2: pcm_s16le 44100 Hz 1 ch`) and pass through. An option that names a
/// *state* instead — the Parent picker's
/// [`ravel_ui::properties::layer::PARENT_NONE`] — is emitted as a locale key
/// for the same reason read-only state words are, and is translated here at
/// the display boundary.
///
/// The panel keeps the raw options beside the labels it builds from them, so
/// `SelectEvent::Confirm`'s translated answer maps back to the stored value
/// and the language in use never changes what an edit writes.
fn enum_option_label(option: &str) -> String {
    read_only_value(option)
}

/// Append the node type's description to the Node Info section when the
/// locale defines one. This is the keyboard-reachable counterpart of the
/// node editor's hover popover (DISC-2): the popover is pointer-only, so
/// the same description must be reachable from the focused Properties
/// panel. The resolved text is emitted as a literal value (user-visible
/// prose, not a key), and a type without a description gets no field.
///
/// `pub` for the `node_hover_popover` integration test, which loads the
/// real locale catalog (the lib unit tests run with an empty i18n store).
pub fn append_node_description(sections: &mut [PropertySection], type_key: &str) {
    let Some(description) = crate::node_locale::description(type_key) else {
        return;
    };
    if let Some(info) = sections.first_mut() {
        info.fields.push(PropertyField::ReadOnly {
            key: "description".into(),
            value: description,
        });
    }
}

/// A field label cell: always one line, ellipsized when the panel is too
/// narrow for it. `min_w_0` allows the shrink that `truncate` needs — without
/// it the cell keeps its intrinsic text width and the row wraps instead.
fn field_label_cell(label: impl Into<SharedString>, muted: Hsla) -> Div {
    div()
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(muted)
        .child(label.into())
}

fn kv_row(key: &str, value: &str, muted: Hsla, fg: Hsla) -> Div {
    div()
        .flex()
        .justify_between()
        .items_center()
        .gap_2()
        .px_1()
        .py(px(1.0))
        .child(field_label_cell(key.to_string(), muted))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(fg)
                .child(SharedString::from(value.to_string())),
        )
}

fn scrub_row(key: &str, scrub: Option<&Entity<ScrubInputState>>, muted: Hsla, fg: Hsla) -> Div {
    let mut row = div()
        .flex()
        .justify_between()
        .items_center()
        .gap_2()
        .px_1()
        .py(px(1.0))
        .child(field_label_cell(field_label(key), muted));
    if let Some(entity) = scrub {
        row = row.child(
            div()
                .flex_shrink_0()
                .min_w(px(64.0))
                .child(ScrubInput::new(entity)),
        );
    } else {
        row = row.text_color(fg);
    }
    row
}

/// Localized name of a custom port type, for the Ports rows and the type
/// menu. `None` is a port whose wire type no menu entry describes (only a
/// hand-built graph produces one); it still needs a word.
fn port_type_label(port_type: Option<CustomPortType>) -> String {
    let name = match port_type {
        Some(CustomPortType::Float) => "float",
        Some(CustomPortType::Int) => "int",
        Some(CustomPortType::Bool) => "bool",
        Some(CustomPortType::Vec2) => "vec2",
        Some(CustomPortType::Vec3) => "vec3",
        Some(CustomPortType::Color) => "color",
        Some(CustomPortType::Geometry) => "geometry",
        Some(CustomPortType::Field) => "field",
        Some(CustomPortType::FrameBuffer) => "frame_buffer",
        Some(CustomPortType::Text) => "text",
        None => "unknown",
    };
    ravel_i18n::translate(&format!("properties.ports.type.{name}"))
}

/// Width of the reorder-handle column: two [`port_button`]s side by side.
const PORT_HANDLE_GUTTER: f32 = 28.0;

/// A built-in port's row: the name and the type the shell gave it, both
/// muted and neither editable. The row exists so the list matches the node on
/// the canvas — hiding `base_geometry` would make Properties disagree with
/// what the user can see and wire.
fn fixed_port_row(row: &ravel_ui::properties::PortRow, gutter: bool, muted: Hsla) -> Div {
    div().child(
        div()
            .id(SharedString::from(format!("port-fixed-{}", row.name)))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_1()
            .py(px(1.0))
            .tooltip(|window, cx| Tooltip::new(t!("properties.ports.builtin")).build(window, cx))
            // Aligned with the editable rows' reorder handles so the list
            // reads as one column of names.
            .child(
                div()
                    .flex()
                    .items_center()
                    .children(gutter.then(|| div().w(px(PORT_HANDLE_GUTTER))))
                    .child(field_label_cell(row.name.clone(), muted)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(port_type_label(row.port_type))),
            ),
    )
}

/// A small icon button of the Ports section (move, remove, add).
fn port_button(
    id: String,
    icon: impl Into<Icon>,
    tooltip: String,
    color: Hsla,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex_shrink_0()
        .w(px(14.0))
        .cursor_pointer()
        .child(icon.into().size_3().text_color(color))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| on_click(window, cx))
}

/// One editable port row: reorder handles, the name Input, the type Select,
/// and the remove button.
///
/// The handles are enabled from the neighbouring rows: a custom port never
/// steps over a built-in one, so a row whose neighbour on that side is fixed
/// (or missing) cannot move that way. `network::move_custom_port` is the
/// authority and refuses the same move; this only keeps the panel from
/// offering a button that would do nothing.
///
/// `gutter` is false when the list has nothing to reorder at all, in which
/// case the handle column is not reserved: an indent that never fills reads as
/// stray padding, and it pushed the whole Ports section out of line with every
/// other row in the panel.
fn custom_port_row(
    row: &ravel_ui::properties::PortRow,
    neighbours: (bool, bool),
    gutter: bool,
    ports: &PortWidgets,
    panel: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
) -> Div {
    let (can_move_up, can_move_down) = neighbours;
    let name_input = ports.names.iter().find(|(n, _)| n == &row.name);
    let type_select = ports.types.iter().find(|(n, _)| n == &row.name);

    let mut handles = div().flex().flex_shrink_0().items_center();
    for (offset, enabled, icon, tooltip_key) in [
        (
            -1,
            can_move_up,
            gpui_component::IconName::ChevronUp,
            "properties.ports.move_up",
        ),
        (
            1,
            can_move_down,
            gpui_component::IconName::ChevronDown,
            "properties.ports.move_down",
        ),
    ] {
        if !enabled {
            handles = handles.child(div().w(px(14.0)));
            continue;
        }
        let panel = panel.clone();
        let name = row.name.clone();
        handles = handles.child(port_button(
            format!("port-move-{offset}-{}", row.name),
            icon,
            ravel_i18n::translate(tooltip_key),
            muted,
            move |_window, cx| {
                let name = name.clone();
                panel
                    .update(cx, move |this, cx| this.move_port(&name, offset, cx))
                    .ok();
            },
        ));
    }

    let remove = {
        let panel = panel.clone();
        let name = row.name.clone();
        port_button(
            format!("port-remove-{}", row.name),
            gpui_component::IconName::Delete,
            t!("properties.ports.remove"),
            muted,
            move |_window, cx| {
                let name = name.clone();
                panel
                    .update(cx, move |this, cx| this.remove_port(&name, cx))
                    .ok();
            },
        )
    };

    let mut fields = div().flex().flex_grow().min_w_0().items_center().gap_1();
    if let Some((_, input)) = name_input {
        fields = fields.child(
            div()
                .flex_grow()
                .min_w_0()
                .child(Input::new(input).xsmall()),
        );
    }
    if let Some((_, select)) = type_select {
        fields = fields.child(
            div()
                .flex_shrink_0()
                .w(px(104.0))
                .child(gpui_component::select::Select::new(select).xsmall()),
        );
    }

    div()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .py(px(1.0))
        .children(gutter.then_some(handles))
        .child(fields)
        .child(remove)
}

/// The trailing row: a name to type, the type the port gets, and the button
/// that creates it.
fn add_port_row(
    ports: &PortWidgets,
    gutter: bool,
    panel: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
) -> Div {
    let Some((name, port_type)) = ports.add.as_ref() else {
        return div();
    };
    let panel = panel.clone();
    div()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .py(px(1.0))
        .children(gutter.then(|| div().w(px(PORT_HANDLE_GUTTER))))
        .child(
            div()
                .flex_grow()
                .min_w_0()
                .child(Input::new(name).xsmall().w_full()),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(104.0))
                .child(gpui_component::select::Select::new(port_type).xsmall()),
        )
        .child(port_button(
            "port-add".into(),
            gpui_component::IconName::Plus,
            t!("properties.ports.add"),
            muted,
            move |_window, cx| {
                panel.update(cx, |this, cx| this.add_port(cx)).ok();
            },
        ))
}

/// One declaration row: the reorder handles, the name Input, the type and
/// default it declares, the remove button, a description Input underneath, and
/// the reason it does not resolve when it does not.
///
/// The type and the default are **read-only**. Both are fixed when the
/// parameter is exposed and both are derived from that parameter
/// (`ravel_core::exposed::apply::seed_value`); letting the panel retype a
/// declaration would let the user build a contract `apply` refuses, which the
/// row could then only report as broken.
fn exposed_row(
    row: &ExposedRow,
    neighbours: (bool, bool),
    widgets: &ExposedWidgets,
    panel: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
    fg: Hsla,
    danger: Hsla,
) -> Div {
    let (can_move_up, can_move_down) = neighbours;
    let name_input = widgets.names.iter().find(|(n, _)| n == &row.name);
    let description_input = widgets.descriptions.iter().find(|(n, _)| n == &row.name);

    let mut handles = div().flex().flex_shrink_0().items_center();
    for (offset, enabled, icon, tooltip_key) in [
        (
            -1,
            can_move_up,
            gpui_component::IconName::ChevronUp,
            "properties.exposed.move_up",
        ),
        (
            1,
            can_move_down,
            gpui_component::IconName::ChevronDown,
            "properties.exposed.move_down",
        ),
    ] {
        if !enabled {
            handles = handles.child(div().w(px(14.0)));
            continue;
        }
        let panel = panel.clone();
        let name = row.name.clone();
        handles = handles.child(port_button(
            format!("exposed-move-{offset}-{}", row.name),
            icon,
            ravel_i18n::translate(tooltip_key),
            muted,
            move |_window, cx| {
                let name = name.clone();
                panel
                    .update(cx, move |this, cx| this.move_declaration(&name, offset, cx))
                    .ok();
            },
        ));
    }

    let remove = {
        let panel = panel.clone();
        let name = row.name.clone();
        port_button(
            format!("exposed-remove-{}", row.name),
            gpui_component::IconName::Delete,
            t!("properties.exposed.remove"),
            muted,
            move |_window, cx| {
                let name = name.clone();
                panel
                    .update(cx, move |this, cx| this.remove_declaration(&name, cx))
                    .ok();
            },
        )
    };

    let mut head = div().flex().items_center().gap_1().px_1().py(px(1.0));
    head = head.child(handles);
    if let Some((_, input)) = name_input {
        head = head.child(
            div()
                .flex_grow()
                .min_w_0()
                .child(Input::new(input).xsmall()),
        );
    }
    // The command-line spelling of the type, then the default a caller gets
    // when they supply nothing. Both are syntax, so neither is translated.
    head = head.child(
        div()
            .flex_shrink_0()
            .max_w(px(120.0))
            .overflow_hidden()
            .text_xs()
            .text_color(muted)
            .child(SharedString::from(format!(
                "{} = {}",
                row.value_type, row.default
            ))),
    );
    head = head.child(remove);

    let mut body = div().flex().flex_col().child(head);
    if let Some((_, input)) = description_input {
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .pb(px(1.0))
                .child(div().w(px(28.0)))
                .child(
                    div()
                        .flex_grow()
                        .min_w_0()
                        .child(Input::new(input).xsmall().w_full()),
                ),
        );
    }
    if let Some(issue) = row.issue {
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .pb(px(2.0))
                .child(div().w(px(28.0)))
                .child(
                    Icon::new(gpui_component::IconName::TriangleAlert)
                        .size_3()
                        .text_color(danger),
                )
                .child(
                    div()
                        .flex_grow()
                        .min_w_0()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(ravel_i18n::translate(issue))),
                ),
        );
    }
    let _ = fg;
    body
}

/// Per-parameter declaration toggle: exposes the parameter as a project input,
/// or reveals the declaration that already does.
///
/// Sibling of [`port_toggle_button`] and deliberately a different affordance:
/// a *port* makes a parameter drivable from inside the graph, a *declaration*
/// makes it settable from outside the project. They are independent, so a
/// parameter can carry both.
fn exposed_toggle_button(
    key: &str,
    declared: bool,
    node_id: NodeId,
    panel: &WeakEntity<PropertiesGpuiPanel>,
    active: Hsla,
    muted: Hsla,
) -> Stateful<Div> {
    let (icon, color) = if declared {
        (RavelIcon::SquareFilled, active)
    } else {
        (RavelIcon::Square, muted)
    };
    let key = key.to_string();
    // Unlike the port toggle, this edits the document from the Properties panel
    // itself: the declarations belong to the project, not to the network the
    // node editor owns, so there is nothing to route through it.
    let panel = panel.clone();
    div()
        .id(SharedString::from(format!("exposed-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .cursor_pointer()
        .child(Icon::new(icon).size_3().text_color(color))
        .tooltip(|window, cx| Tooltip::new(t!("properties.toggle.exposed")).build(window, cx))
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            let key = key.clone();
            panel
                .update(cx, move |this, cx| {
                    this.expose_parameter(node_id, &key, cx);
                    cx.notify();
                })
                .ok();
        })
}

/// Synthetic scrub keys for the components of a `Vector` field
/// (`center#x`, `center#y`, ...).
fn vector_component_keys(key: &str, count: usize) -> Vec<String> {
    const SUFFIXES: [&str; 4] = ["x", "y", "z", "w"];
    (0..count.min(SUFFIXES.len()))
        .map(|i| format!("{key}#{}", SUFFIXES[i]))
        .collect()
}

/// Default height of an expanded curve editor, and the bounds the resize
/// drag keeps it between. The minimum leaves room for the editor's own
/// toolbar (the selected point, the interpolation buttons, the view range)
/// plus a usable graph above it.
const CURVE_EDITOR_HEIGHT: f32 = 200.0;
const CURVE_EDITOR_MIN_HEIGHT: f32 = 120.0;
const CURVE_EDITOR_MAX_HEIGHT: f32 = 560.0;
/// Height of the grab strip under an expanded curve editor.
const CURVE_RESIZE_HANDLE_HEIGHT: f32 = 6.0;

/// Drag payload for a curve editor's height handle, identified by the row's
/// field key.
#[derive(Clone)]
struct DragCurveHeight(SharedString);

impl Render for DragCurveHeight {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// An in-flight curve-editor height drag: the row being resized, the pointer
/// y it started at, and the height it had then.
struct CurveResize {
    key: String,
    start_y: f32,
    start_height: f32,
}

/// The collapsed curve row: label plus a thumbnail of the curve. Clicking
/// anywhere on the row toggles the inline editor underneath it — panel view
/// state that never reaches the document.
fn curve_row(
    key: &str,
    curve: &ravel_core::param_curve::CurveParam,
    expanded: bool,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
    fg: Hsla,
) -> Div {
    let editor = editor.clone();
    let field_key = key.to_string();
    let icon = if expanded {
        gpui_component::IconName::ChevronDown
    } else {
        gpui_component::IconName::ChevronRight
    };
    div().child(
        div()
            .id(SharedString::from(format!("curve-row-{key}")))
            .flex()
            .justify_between()
            .items_center()
            .gap_2()
            .px_1()
            .py(px(1.0))
            .cursor_pointer()
            .child(field_label_cell(field_label(key), muted))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .w(px(48.0))
                            .h(px(14.0))
                            .child(curve_thumbnail(curve.clone(), fg)),
                    )
                    .child(Icon::new(icon).size_3().text_color(muted)),
            )
            .tooltip(|window, cx| Tooltip::new(t!("properties.curve.expand")).build(window, cx))
            .on_click(move |_event, _window, cx| {
                editor
                    .update(cx, |this, cx| {
                        this.toggle_curve_expanded(&field_key, cx);
                    })
                    .ok();
            }),
    )
}

/// The expanded curve editor plus the strip that drags its height.
fn curve_editor_body(
    key: &str,
    state: &Entity<ParamCurveEditorState>,
    height: f32,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
) -> Div {
    let handle_key = SharedString::from(key.to_string());
    let begin = editor.clone();
    let moving = editor.clone();
    let ending = editor.clone();
    let drag_key = handle_key.clone();
    div()
        .flex()
        .flex_col()
        .px_1()
        .pb(px(2.0))
        .child(
            div()
                .h(px(height))
                .w_full()
                .child(ParamCurveEditor::new(state)),
        )
        .child(
            div()
                .id(SharedString::from(format!("curve-resize-{key}")))
                .h(px(CURVE_RESIZE_HANDLE_HEIGHT))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::ResizeUpDown)
                .child(div().w(px(24.0)).h(px(2.0)).rounded(px(1.0)).bg(muted))
                .on_mouse_down(MouseButton::Left, {
                    let key = handle_key.clone();
                    move |event: &MouseDownEvent, _window, cx| {
                        let key = key.to_string();
                        let y: f32 = event.position.y.into();
                        begin
                            .update(cx, |this, _cx| this.begin_curve_resize(key, y))
                            .ok();
                    }
                })
                .on_drag(DragCurveHeight(drag_key.clone()), |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                })
                .on_drag_move(move |event: &DragMoveEvent<DragCurveHeight>, _window, cx| {
                    let DragCurveHeight(dragged) = event.drag(cx);
                    if dragged != &drag_key {
                        return;
                    }
                    let y: f32 = event.event.position.y.into();
                    moving
                        .update(cx, |this, cx| this.curve_resize_to(y, cx))
                        .ok();
                })
                .on_mouse_up(
                    MouseButton::Left,
                    move |_event: &MouseUpEvent, _window, cx| {
                        ending.update(cx, |this, _cx| this.end_curve_resize()).ok();
                    },
                ),
        )
}

#[allow(clippy::too_many_arguments)]
fn build_field_row(
    field: &PropertyField,
    scrubs: &[(String, Entity<ScrubInputState>)],
    strings: &[(String, Entity<InputState>)],
    selects: &[(String, Entity<SelectState<Vec<SharedString>>>)],
    colors: &[(String, Entity<ColorPickerState>)],
    expanded_curves: &std::collections::HashSet<String>,
    ports: &PortWidgets,
    declarations: &ExposedWidgets,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    node_ids: &[NodeId],
    muted: Hsla,
    fg: Hsla,
    danger: Hsla,
) -> Div {
    match field {
        PropertyField::Curve { key, curve } => {
            curve_row(key, curve, expanded_curves.contains(key), editor, muted, fg)
        }

        PropertyField::ReadOnly { key, value } => {
            kv_row(&field_label(key), &read_only_value(value), muted, fg)
        }

        // The interface node's port list. Built-in and custom ports share one
        // list so it reads as the node's interface; only the custom rows are
        // editable.
        PropertyField::PortList { rows, .. } => {
            let mut list = div().flex().flex_col();
            // The gutter is reserved only when some handle in the list will
            // actually be enabled — that is, when two custom rows are
            // *adjacent*, which is the exact condition `movable` below tests.
            // Counting custom rows instead would reserve dead space whenever
            // they are separated by a fixed row (a renamed legacy port sitting
            // beside the restored built-in one), since neither can move.
            // Decided once for the whole list rather than per row, so the names
            // stay in one column.
            let gutter = rows.windows(2).any(|pair| !pair[0].fixed && !pair[1].fixed);
            for (index, row) in rows.iter().enumerate() {
                if row.fixed {
                    list = list.child(fixed_port_row(row, gutter, muted));
                    continue;
                }
                let movable = |neighbour: Option<&ravel_ui::properties::PortRow>| {
                    neighbour.is_some_and(|row| !row.fixed)
                };
                let neighbours = (
                    movable(index.checked_sub(1).and_then(|i| rows.get(i))),
                    movable(rows.get(index + 1)),
                );
                list = list.child(custom_port_row(
                    row, neighbours, gutter, ports, editor, muted,
                ));
            }
            list = list.child(add_port_row(ports, gutter, editor, muted));
            if let Some(message) = &ports.error {
                list = list.child(
                    div()
                        .px_1()
                        .py(px(1.0))
                        .text_xs()
                        .text_color(danger)
                        .child(message.clone()),
                );
            }
            list
        }

        // The project's declarations. There is no trailing add row: a
        // declaration is created by exposing a parameter, which is where the
        // binding comes from (see `PropertyField::ExposedList`).
        PropertyField::ExposedList { rows, .. } => {
            let mut list = div().flex().flex_col();
            if rows.is_empty() {
                list = list.child(
                    div()
                        .px_1()
                        .py(px(2.0))
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(t!("properties.exposed.empty"))),
                );
            }
            for (index, row) in rows.iter().enumerate() {
                let neighbours = (index > 0, index + 1 < rows.len());
                list = list.child(exposed_row(
                    row,
                    neighbours,
                    declarations,
                    editor,
                    muted,
                    fg,
                    danger,
                ));
            }
            if let Some(message) = &declarations.error {
                list = list.child(
                    div()
                        .px_1()
                        .py(px(1.0))
                        .text_xs()
                        .text_color(danger)
                        .child(message.clone()),
                );
            }
            list
        }

        PropertyField::Float { key, .. } | PropertyField::Int { key, .. } => {
            let scrub = scrubs.iter().find(|(k, _)| k == key).map(|(_, e)| e);
            scrub_row(key, scrub, muted, fg)
        }

        PropertyField::Bool { key, value } => {
            let editor = editor.clone();
            let field_key = key.clone();
            let node_ids = node_ids.to_vec();
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_2()
                .px_1()
                .py(px(1.0))
                .child(field_label_cell(field_label(key), muted))
                .child(
                    Checkbox::new(SharedString::from(format!("bool-{key}")))
                        .checked(*value)
                        .on_click(move |checked: &bool, _window, cx| {
                            let value = PropertyValue::Bool(*checked);
                            let key = field_key.clone();
                            let node_ids = node_ids.clone();
                            editor
                                .update(cx, move |this, cx| {
                                    this.route_change(&key, value, true, &node_ids, cx);
                                    cx.notify();
                                })
                                .ok();
                        }),
                )
        }

        PropertyField::String { key, .. } => {
            let input = strings.iter().find(|(k, _)| k == key).map(|(_, e)| e);
            let mut row = div()
                .flex()
                .flex_col()
                .px_1()
                .py(px(1.0))
                .child(field_label_cell(field_label(key), muted));
            if let Some(input) = input {
                row = row.child(Input::new(input).small().w_full());
            }
            row
        }

        PropertyField::Enum { key, value, .. } => {
            let select = selects.iter().find(|(k, _)| k == key);
            let mut row = div().flex().flex_col().px_1().py(px(1.0)).child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(field_label_cell(field_label(key), muted))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(fg)
                            .child(SharedString::from(enum_option_label(value))),
                    ),
            );
            if let Some((_, entity)) = select {
                row = row.child(gpui_component::select::Select::new(entity).small().w_full());
            }
            row
        }

        PropertyField::Color { key, r, g, b, .. } => {
            let picker = colors.iter().find(|(k, _)| k == key).map(|(_, e)| e);
            let mut row = div()
                .flex()
                .justify_between()
                .items_center()
                .gap_2()
                .px_1()
                .py(px(1.0))
                .child(field_label_cell(field_label(key), muted));
            if let Some(entity) = picker {
                row = row.child(ColorPicker::new(entity).small());
            } else {
                row = row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(fg)
                        .child(SharedString::from(format!("({r:.2}, {g:.2}, {b:.2})"))),
                );
            }
            row
        }

        PropertyField::Vector {
            key, components, ..
        } => {
            let keys = vector_component_keys(key, components.len());
            let entities: Vec<&Entity<ScrubInputState>> = keys
                .iter()
                .filter_map(|ck| scrubs.iter().find(|(k, _)| k == ck).map(|(_, e)| e))
                .collect();
            let mut row = div()
                .flex()
                .justify_between()
                .items_center()
                .gap_2()
                .px_1()
                .py(px(1.0))
                .child(field_label_cell(field_label(key), muted));
            if entities.len() == components.len() {
                let mut cell = div().flex().flex_shrink_0().gap_1();
                for entity in entities {
                    cell = cell.child(div().min_w(px(56.0)).child(ScrubInput::new(entity)));
                }
                row = row.child(cell);
            } else {
                let parts: Vec<String> = components.iter().map(|v| format!("{v:.3}")).collect();
                row = row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(fg)
                        .child(SharedString::from(format!("[{}]", parts.join(", ")))),
                );
            }
            row
        }
    }
}

/// Click target of a per-field key-toggle button: layer fields edit the
/// document through this panel; node fields route to the node editor,
/// which owns the network context.
#[derive(Clone)]
enum KeyTarget {
    Layer(WeakEntity<PropertiesGpuiPanel>),
    Node(NodeId),
}

/// Whether the node parameter `key` has a keyframe at `local_frame` (all
/// components for multi-component parameters). Without a local frame a
/// keyframed source counts as keyed. `None` when the parameter is missing
/// or not animatable (`Int` / `Bool` / `String` are constant-only in v1,
/// REQ-LAYER-004).
fn node_param_keyed(node: &Node, key: &str, local_frame: Option<u64>) -> Option<bool> {
    fn has_key(channel: &AnimationChannel, local_frame: Option<u64>) -> bool {
        match (&channel.source, local_frame) {
            (ChannelSource::Keyframes(curve), Some(frame)) => {
                curve.keyframes().iter().any(|k| k.frame == frame)
            }
            (ChannelSource::Keyframes(_), None) => true,
            _ => false,
        }
    }
    let param = node.parameters.iter().find(|p| p.key == key)?;
    match &param.value {
        ParameterValue::Float(_) => Some(false),
        ParameterValue::Channel(channel) => Some(has_key(channel, local_frame)),
        ParameterValue::Channel2(channels) => {
            Some(channels.iter().all(|ch| has_key(ch, local_frame)))
        }
        ParameterValue::Channel3(channels) => {
            Some(channels.iter().all(|ch| has_key(ch, local_frame)))
        }
        ParameterValue::Channel4(channels) => {
            Some(channels.iter().all(|ch| has_key(ch, local_frame)))
        }
        _ => None,
    }
}

/// The small ◆/◇ keyframe toggle shown left of an animatable field's
/// label: filled (theme primary) when a key sits at the current frame,
/// hollow (muted) otherwise.
fn key_toggle_button(
    key: &str,
    keyed: bool,
    target: &KeyTarget,
    active: Hsla,
    muted: Hsla,
) -> Stateful<Div> {
    let (icon, color) = if keyed {
        (RavelIcon::DiamondFilled, active)
    } else {
        (RavelIcon::Diamond, muted)
    };
    let button = div()
        .id(SharedString::from(format!("key-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .cursor_pointer()
        .child(Icon::new(icon).size_3().text_color(color))
        .tooltip(|window, cx| Tooltip::new(t!("properties.toggle.keyframe")).build(window, cx));
    match target {
        KeyTarget::Layer(panel) => {
            let panel = panel.clone();
            let key = key.to_string();
            button.on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                panel
                    .update(cx, |this, cx| {
                        this.toggle_key(&key, cx);
                        cx.notify();
                    })
                    .ok();
            })
        }
        KeyTarget::Node(node_id) => {
            let node_id = *node_id;
            let key = key.to_string();
            button.on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                let editor = cx
                    .try_global::<super::NodeEditorHandle>()
                    .and_then(|handle| handle.0.upgrade());
                if let Some(editor) = editor {
                    editor.update(cx, |editor, cx| {
                        editor.toggle_param_keyframe(node_id, &key, cx);
                    });
                }
            })
        }
    }
}

/// What one parameter row needs to show about its expressions.
///
/// One entry per animation channel the parameter carries, `None` where that
/// component is not driven by an expression. A vector parameter can be
/// partially driven, so this is per component rather than per row.
#[derive(Clone, Debug, PartialEq, Default)]
struct ExpressionRow {
    components: Vec<Option<ExpressionComponent>>,
    /// Whether clicking the badge would attach anything.
    ///
    /// False for a row whose every component is driven by something an
    /// expression would have to destroy — a keyframe curve, a node output, an
    /// audio source, a blend. The badge is drawn dead for those rather than
    /// accepting a click that quietly does nothing.
    attachable: bool,
}

impl ExpressionRow {
    /// Whether any component is driven — what the row badge shows.
    fn is_attached(&self) -> bool {
        self.components.iter().any(Option::is_some)
    }

    /// Which components are driven. Editing a source does not change this, so
    /// it is the part a widget rebuild has to watch: the set of Inputs.
    fn shape(&self) -> Vec<bool> {
        self.components.iter().map(Option::is_some).collect()
    }
}

/// Expression text the author has typed but not confirmed.
#[derive(Clone, Debug, PartialEq)]
struct ExpressionDraft {
    source: String,
    /// Why the draft does not compile, if it does not — recomputed on every
    /// keystroke, which is what puts the error on screen during editing.
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ExpressionComponent {
    source: String,
    /// Why the source does not compile, if it does not.
    ///
    /// Carried from `ExpressionError`'s own `Display`, which already names the
    /// line and column. It is **not** a locale key: it quotes identifiers the
    /// author typed and spans of their own source, the same way the node
    /// description passes through untranslated.
    error: Option<String>,
}

/// The expression shape of every row, for the rebuild guard.
fn expression_shape(rows: &[(String, ExpressionRow)]) -> Vec<(String, Vec<bool>)> {
    rows.iter()
        .map(|(key, row)| (key.clone(), row.shape()))
        .collect()
}

/// The expression badge shown left of a channel-backed field's label:
/// filled (theme primary) when any component is driven by an expression,
/// muted when a click would attach one, and dimmed to the border colour when
/// it would not.
///
/// Clicking it attaches an expression to every component that holds a plain
/// constant, seeded with the value already on screen, or detaches every one
/// and freezes that value — the same one-click, one-undo-step contract as the
/// keyframe toggle beside it.
///
/// The dead state is the visible half of the rule in
/// `ravel_ui::properties::expression`: attaching would overwrite whatever
/// drives the parameter, so it refuses. Drawing the badge live and swallowing
/// the click would leave the author clicking a control that never responds, so
/// the badge greys out and its tooltip says why.
fn expression_toggle_button(
    key: &str,
    attached: bool,
    attachable: bool,
    node_id: NodeId,
    active: Hsla,
    muted: Hsla,
    disabled: Hsla,
) -> Stateful<Div> {
    // Detaching is always available once something is attached; only the
    // attach direction can be refused.
    let live = attached || attachable;
    let color = match (attached, live) {
        (true, _) => active,
        (false, true) => muted,
        (false, false) => disabled,
    };
    let key = key.to_string();
    let mut badge = div()
        .id(SharedString::from(format!("expression-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .child(Icon::new(RavelIcon::Expression).size_3().text_color(color));
    badge = if live {
        badge.cursor_pointer().tooltip(|window, cx| {
            Tooltip::new(t!("properties.toggle.expression")).build(window, cx)
        })
    } else {
        badge.cursor_default().tooltip(|window, cx| {
            Tooltip::new(t!("properties.toggle.expression_blocked")).build(window, cx)
        })
    };
    badge.on_mouse_down(MouseButton::Left, move |_, _window, cx| {
        if !live {
            return;
        }
        let editor = cx
            .try_global::<super::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade());
        if let Some(editor) = editor {
            editor.update(cx, |editor, cx| {
                editor.toggle_param_expression(node_id, &key, cx);
            });
        }
    })
}

/// The expression editor under a driven row: one source box per driven
/// component, each with its compile error beneath it.
///
/// The error is shown and the box stays editable — a source that does not
/// compile is a state the document holds, not an edit to refuse. A vector
/// component that is *not* driven contributes no box, so a partially driven
/// parameter shows only the parts that have an expression.
///
/// A component with a draft shows the **draft's** error rather than the
/// document's, which is what makes the message track the text on screen while
/// it is being typed.
fn expression_editor_body(
    key: &str,
    row: &ExpressionRow,
    inputs: &[(String, usize, Entity<InputState>)],
    drafts: &[(String, usize, ExpressionDraft)],
    mono: SharedString,
    muted: Hsla,
    danger: Hsla,
) -> Div {
    const COMPONENT_LABELS: [&str; 4] = ["x", "y", "z", "w"];
    let multi = row.components.len() > 1;
    let mut body = div().flex().flex_col().w_full().pl(px(18.0)).pb(px(2.0));

    for (component, stored) in row.components.iter().enumerate() {
        let Some(stored) = stored else {
            continue;
        };
        let Some((_, _, state)) = inputs
            .iter()
            .find(|(k, index, _)| k == key && *index == component)
        else {
            continue;
        };
        let mut line = div().flex().items_center().gap_1().w_full();
        // Axis letters are left untranslated, as everywhere else in the UI.
        if multi {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .w(px(12.0))
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(
                        COMPONENT_LABELS
                            .get(component)
                            .copied()
                            .unwrap_or_default()
                            .to_string(),
                    )),
            );
        }
        body = body.child(
            line.child(
                div()
                    .flex_grow()
                    .min_w_0()
                    // Expression source is code: monospaced so operators and
                    // nesting line up, and so the column a compile error points
                    // at is countable. `font_family` replaces the family without
                    // clearing the inherited Japanese fallbacks (see
                    // `crate::fonts`), which an expression can contain through a
                    // string literal.
                    .font_family(mono.clone())
                    .child(Input::new(state).small().w_full()),
            ),
        );
        let error = drafts
            .iter()
            .find(|(k, index, _)| k == key && *index == component)
            .map_or(stored.error.as_ref(), |(_, _, draft)| draft.error.as_ref());
        if let Some(error) = error {
            body = body.child(
                div()
                    .px_1()
                    .py(px(1.0))
                    .text_xs()
                    .text_color(danger)
                    // Same family as the source above it: the message quotes
                    // fragments of the expression.
                    .font_family(mono.clone())
                    .child(SharedString::from(error.clone())),
            );
        }
    }
    body
}

/// Discriminant fingerprint of the sections' fields: key plus variant kind.
/// A same-target refresh whose shape changed (e.g. a parameter switched
/// between editable and driven read-only) must rebuild widget bindings.
fn fields_shape(
    sections: &[PropertySection],
) -> Vec<(String, std::mem::Discriminant<PropertyField>)> {
    sections
        .iter()
        .flat_map(|section| &section.fields)
        .map(|field| (field_shape_key(field), std::mem::discriminant(field)))
        .collect()
}

/// The part of a field's identity a rebuild has to watch.
///
/// For every value field that is the key: a new value reaches the widget
/// through `refresh_values`. A **port list** contributes its rows too — the
/// list is the node's shape, so adding, removing, renaming, retyping or
/// reordering a port changes which widgets exist and what they hold, and no
/// value-refresh path can rename a row's Input. Only a rebuild can.
///
/// An **enum** contributes its options for the same reason. Some option lists
/// are derived from the document rather than fixed by the parameter — the
/// Parent picker's sibling layers, the audio stream picker's streams — and
/// `refresh_values` cannot restock a `SelectState`. A Select left holding a
/// renamed or deleted layer would offer, and then write, a layer id the
/// composition no longer has.
fn field_shape_key(field: &PropertyField) -> String {
    use std::fmt::Write as _;
    match field {
        PropertyField::PortList { key, rows, .. } => {
            let mut shape = key.clone();
            for row in rows {
                let _ = write!(shape, "\n{}\t{:?}\t{}", row.name, row.port_type, row.fixed);
            }
            shape
        }
        // A declaration list changes shape the same way a port list does: the
        // rows *are* the widgets, so adding, removing, renaming or reordering
        // one has to rebuild them. The issue is fingerprinted too — it decides
        // whether the row renders its warning line.
        PropertyField::ExposedList { key, rows } => {
            let mut shape = key.clone();
            for row in rows {
                let _ = write!(
                    shape,
                    "\n{}\t{}\t{}\t{}\t{:?}",
                    row.name, row.value_type, row.default, row.description, row.issue
                );
            }
            shape
        }
        PropertyField::Enum { key, options, .. } => {
            let mut shape = key.clone();
            for option in options {
                let _ = write!(shape, "\n{option}");
            }
            shape
        }
        _ => field.key().to_string(),
    }
}

/// Exposure state of a node parameter for the per-row port toggle
/// (param-input-ports-plan Phase 4).
#[derive(Clone, Copy, PartialEq)]
enum PortToggleState {
    /// Exposable but not exposed.
    Unexposed,
    /// Exposed, no connection.
    Exposed,
    /// Exposed and driven by an edge (unexposing also removes it).
    Connected,
}

/// Per-parameter port toggle (○ / ◎ / ●): clicking exposes or unexposes the
/// parameter as an input port through the node editor (one structural
/// Document undo step; unexposing removes connected edges with it).
fn port_toggle_button(
    key: &str,
    state: PortToggleState,
    node_id: NodeId,
    active: Hsla,
    muted: Hsla,
) -> Stateful<Div> {
    let (icon, color) = match state {
        PortToggleState::Unexposed => (RavelIcon::Circle, muted),
        PortToggleState::Exposed => (RavelIcon::CircleDot, active),
        PortToggleState::Connected => (RavelIcon::CircleFilled, active),
    };
    let key = key.to_string();
    div()
        .id(SharedString::from(format!("port-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .cursor_pointer()
        .child(Icon::new(icon).size_3().text_color(color))
        .tooltip(|window, cx| Tooltip::new(t!("properties.toggle.port")).build(window, cx))
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            let editor = cx
                .try_global::<super::NodeEditorHandle>()
                .and_then(|handle| handle.0.upgrade());
            if let Some(editor) = editor {
                editor.update(cx, |editor, cx| {
                    editor.toggle_param_port(node_id, &key, cx);
                    cx.notify();
                });
            }
        })
}

struct ScrubBinding {
    state: Entity<ScrubInputState>,
    #[allow(dead_code)]
    sub: Subscription,
}

struct SelectBinding {
    #[allow(dead_code)]
    state: Entity<SelectState<Vec<SharedString>>>,
    #[allow(dead_code)]
    sub: Subscription,
}

struct StringBinding {
    state: Entity<InputState>,
    #[allow(dead_code)]
    sub: Subscription,
}

struct ColorBinding {
    state: Entity<ColorPickerState>,
    #[allow(dead_code)]
    sub: Subscription,
}

struct CurveBinding {
    state: Entity<ParamCurveEditorState>,
    #[allow(dead_code)]
    sub: Subscription,
}

/// The trailing "add a port" row of the Ports section: a name to type and the
/// type the new port gets. It is not bound to any port, so it lives beside
/// the per-row bindings rather than in them.
struct PortAddBinding {
    name: Entity<InputState>,
    port_type: Entity<SelectState<Vec<SharedString>>>,
    #[allow(dead_code)]
    sub: Subscription,
}

/// The name Input and type Select of the Ports section's trailing add row.
type PortAddWidgets = (Entity<InputState>, Entity<SelectState<Vec<SharedString>>>);

/// Everything the Ports section renders with, collected from the panel before
/// `render` walks the sections (render itself only reads).
#[derive(Clone)]
struct PortWidgets {
    names: Vec<(String, Entity<InputState>)>,
    types: Vec<(String, Entity<SelectState<Vec<SharedString>>>)>,
    add: Option<PortAddWidgets>,
    error: Option<SharedString>,
}

/// Everything the declarations section renders with, collected from the panel
/// before `render` walks the sections (render itself only reads).
#[derive(Clone)]
struct ExposedWidgets {
    names: Vec<(String, Entity<InputState>)>,
    descriptions: Vec<(String, Entity<InputState>)>,
    error: Option<SharedString>,
}

/// The message shown under the declarations list for a refused edit.
///
/// The vocabulary is the core's ([`ExposedParameterError`]) so that the panel
/// never invents a reason of its own; `UnknownName` has no key because the
/// panel only ever names rows it just rendered, so reaching it means the list
/// and the document disagreed — a bug, not something to explain to the user.
fn exposed_error_message(err: &ExposedParameterError) -> SharedString {
    let key = match err {
        ExposedParameterError::EmptyName => "properties.exposed.error.empty_name",
        ExposedParameterError::DuplicateName(_) => "properties.exposed.error.duplicate",
        ExposedParameterError::DefaultTypeMismatch { .. }
        | ExposedParameterError::NonFiniteDefault(_)
        | ExposedParameterError::UnknownName(_) => "properties.exposed.error.failed",
    };
    SharedString::from(ravel_i18n::translate(key))
}

/// Quiet period after the last `ColorPickerEvent::Change` before the edit
/// commits one Document undo step. The picker emits a change per slider
/// tick with no gesture-end event, so live changes apply uncommitted and
/// the commit is debounced (matching the scrub-gesture undo granularity).
const COLOR_COMMIT_QUIET: std::time::Duration = std::time::Duration::from_millis(300);

/// Panel color fields are plain 0-1 RGBA; the picker widget speaks `Hsla`.
fn hsla_from_rgba(r: f32, g: f32, b: f32, a: f32) -> Hsla {
    Hsla::from(Rgba { r, g, b, a })
}

/// What kind of target the current widgets were built for. Same-identity
/// target updates (undo refresh, live document sync) update values in place
/// so an in-flight scrub gesture keeps its widget entities.
fn same_target(current: &PropertiesTarget, next: &PropertiesTarget) -> bool {
    !matches!(current, PropertiesTarget::Empty) && current == next
}

/// A single-layer target resolved from the document: the owning composition,
/// the layer, and the playhead frame.
///
/// The composition comes along because the Parent picker lists the layer's
/// siblings, and because the eval context's frame rate and resolution are
/// the composition's.
type ResolvedLayer = (Arc<ravel_core::composition::Composition>, Layer, u64);

/// A multi-layer target resolved from the document: the owning composition,
/// the surviving layers, and the playhead frame.
type ResolvedLayers = (Arc<ravel_core::composition::Composition>, Vec<Layer>, u64);

pub struct PropertiesGpuiPanel {
    sections: Vec<PropertySection>,
    target: PropertiesTarget,
    project: Option<Entity<ProjectState>>,
    registry: NodeRegistry,
    scrubs: Vec<(String, ScrubBinding)>,
    strings: Vec<(String, StringBinding)>,
    selects: Vec<(String, SelectBinding)>,
    colors: Vec<(String, ColorBinding)>,
    curves: Vec<(String, CurveBinding)>,
    /// Expression state of the selected node's parameters, keyed by field key.
    ///
    /// Derived from the document on every refresh rather than held as edit
    /// state: the source of truth is the `ChannelSource::Expression` in the
    /// graph, and an editor that kept its own copy would show a stale source
    /// after an undo.
    expressions: Vec<(String, ExpressionRow)>,
    /// One text Input per expression-driven component, keyed by field key and
    /// component index. Retained for the same reason every other Input is: a
    /// half-typed expression has to survive a document refresh.
    expression_inputs: Vec<(String, usize, StringBinding)>,
    /// Uncommitted expression text, keyed by field key and component index.
    ///
    /// The draft is the whole of this editor's edit state, and it exists
    /// because an expression is the one field in the panel that has to report
    /// errors *while* it is typed. Committing on every keystroke would show
    /// the error but fill the undo history with half-typed sources, so a
    /// keystroke writes here instead: the draft is compiled for its message,
    /// the document is not touched, and Enter or blur is what commits.
    ///
    /// It is also what keeps a stale box from overwriting the document. A
    /// component with no draft has not been typed into since its last commit,
    /// so `sync_expression_widgets` may replace its text with the document's
    /// — which is how an undo reaches the box — and blur has nothing to
    /// commit. Without that gate, an undo would leave the old text in a
    /// focused box and the following blur would write it straight back,
    /// undoing the undo.
    expression_drafts: Vec<(String, usize, ExpressionDraft)>,
    /// Row widgets of the Ports section, keyed by port name: the name Input
    /// and the type Select of every editable row, plus the trailing add row.
    ///
    /// The panel owns them for the reason it owns every other widget — a row's
    /// half-typed name has to survive a document refresh — and they are
    /// rebuilt whenever the port list itself changes, which `fields_shape`
    /// detects by fingerprinting the rows.
    port_names: Vec<(String, StringBinding)>,
    port_types: Vec<(String, SelectBinding)>,
    port_add: Option<PortAddBinding>,
    /// The type menu the current Ports section offers, in the order the
    /// Selects list it. `SelectEvent::Confirm` hands back the *translated*
    /// label, so the types are kept beside it to map the answer back.
    port_type_options: Vec<CustomPortType>,
    /// The last refused port edit, shown under the list. A rejected name or
    /// type is something the user typed and has to see; it is cleared by the
    /// next successful edit and by a target change, where it would be a
    /// message about a node nobody is looking at.
    port_error: Option<SharedString>,
    /// The port rename this panel has already sent, as `(old name, new name)`.
    ///
    /// The row's name Input commits on Enter *and* on blur, and one gesture
    /// produces both — see [`Self::rename_port`], which drops the repeat. The
    /// record lasts until the widgets are rebuilt (the successful rename does
    /// that itself) or the target changes, so a rename that was *refused* can
    /// be retried under a different name straight away.
    committed_port_rename: Option<(String, String)>,
    /// Row widgets of the declarations section, keyed by declaration name: the
    /// name Input and the description Input of every row.
    ///
    /// Owned for the same reason as the Ports section's widgets — a half-typed
    /// name has to survive a document refresh — and rebuilt whenever the list
    /// itself changes, which `fields_shape` detects by fingerprinting the rows.
    exposed_names: Vec<(String, StringBinding)>,
    exposed_descriptions: Vec<(String, StringBinding)>,
    /// The last refused declaration edit, shown under the list.
    exposed_error: Option<SharedString>,
    /// The declaration rename this panel has already sent, with the same
    /// Enter-then-Blur duplicate guard as [`Self::rename_port`].
    committed_exposed_rename: Option<(String, String)>,
    /// Curve rows whose inline editor is open, and the height each open
    /// editor was dragged to.
    ///
    /// This is **view state and stays out of the document**: an expansion is
    /// not an edit, so it records no undo step and undo never collapses a
    /// row. Several rows can be open at once (curves are compared against
    /// their neighbours, so expansion is not exclusive). Both maps are
    /// dropped when the panel's target changes — a bare key like `points`
    /// says nothing about the node it came from, so carrying an expansion
    /// across targets would open an unrelated row.
    expanded_curves: std::collections::HashSet<String>,
    curve_heights: std::collections::HashMap<String, f32>,
    curve_resize: Option<CurveResize>,
    /// Uncommitted color edit awaiting its debounced undo commit, with the
    /// generation guard that cancels superseded commits.
    pending_color_commit: Option<(String, PropertyValue)>,
    color_commit_generation: u64,
    needs_rebuild: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    playback_sub: Subscription,
}

impl PropertiesGpuiPanel {
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());

        let selection_sub = cx.observe_global::<SelectedPropertiesTarget>(|this: &mut Self, cx| {
            let target = cx
                .try_global::<SelectedPropertiesTarget>()
                .cloned()
                .unwrap_or_default();
            let same = same_target(&this.target, &target.0);
            this.target = target.0;
            if same {
                // Same target, new values (undo, timeline drag, playhead
                // move): refresh in place so scrub gestures survive —
                // unless the field shape changed (a parameter became
                // driven or editable again), where stale widget bindings
                // would edit through a read-only row.
                this.refresh_values_checked(cx);
            } else {
                // A pending color commit must not land on the new target.
                this.pending_color_commit = None;
                this.color_commit_generation += 1;
                // A refusal names a port on the target that is going away,
                // and a rename record names a row that is going with it.
                this.port_error = None;
                this.committed_port_rename = None;
                this.exposed_error = None;
                this.committed_exposed_rename = None;
                // Curve expansion is per-target view state (see the field
                // docs): a new target starts with every curve row collapsed,
                // so returning to a node shows it collapsed again.
                this.expanded_curves.clear();
                this.curve_heights.clear();
                this.curve_resize = None;
                this.needs_rebuild = true;
            }
            cx.notify();
        });

        // Any document change (edit, undo/redo, live gesture update)
        // re-resolves the current target's values in place — the same
        // semantics as a same-target republish, so an in-flight scrub
        // gesture is never destroyed.
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, project, cx| {
                if matches!(this.target, PropertiesTarget::Empty) {
                    return;
                }
                // Re-resolving every section is what makes this expensive, and
                // a notify that left the document alone cannot have changed a
                // value. The target-republish and playhead paths below are
                // separate and stay unconditional.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.refresh_values_checked(cx);
                cx.notify();
            })
        });

        // Sections sample animated channels at the playhead's layer-local
        // frame; follow it so displayed values and the ◆/◇ state track
        // playback — for node and layer targets alike.
        let playback_sub = cx.observe_global::<super::PlaybackPosition>(|this: &mut Self, cx| {
            if !matches!(this.target, PropertiesTarget::Empty) {
                this.refresh_values(cx);
                cx.notify();
            }
        });

        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        Self {
            sections: Vec::new(),
            // A panel opened *because* something was selected has to show it.
            // The selection lives in a durable global, and the observer above
            // only fires on later writes, so a panel created after the write —
            // which is exactly what `CommandId::ProjectExposedParameters` does
            // when Properties is closed — would open on the empty state and
            // stay there until the user selected something else.
            target: cx
                .try_global::<SelectedPropertiesTarget>()
                .cloned()
                .unwrap_or_default()
                .0,
            project,
            registry,
            scrubs: Vec::new(),
            strings: Vec::new(),
            selects: Vec::new(),
            colors: Vec::new(),
            curves: Vec::new(),
            port_names: Vec::new(),
            port_types: Vec::new(),
            port_add: None,
            port_type_options: Vec::new(),
            port_error: None,
            committed_port_rename: None,
            exposed_names: Vec::new(),
            exposed_descriptions: Vec::new(),
            exposed_error: None,
            committed_exposed_rename: None,
            expanded_curves: std::collections::HashSet::new(),
            curve_heights: std::collections::HashMap::new(),
            curve_resize: None,
            pending_color_commit: None,
            color_commit_generation: 0,
            expressions: Vec::new(),
            expression_inputs: Vec::new(),
            expression_drafts: Vec::new(),
            // The target above may already name something, and nothing has
            // built its widgets yet.
            needs_rebuild: true,
            focus_handle,
            focus_subscriptions,
            selection_sub,
            project_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            playback_sub,
        }
    }

    /// Refresh values in place, rebuilding widget bindings only when the
    /// field shape changed (a parameter became driven or editable again) —
    /// the shape check that keeps a stale widget from editing through a
    /// read-only row.
    fn refresh_values_checked(&mut self, cx: &mut Context<Self>) {
        let before = fields_shape(&self.sections);
        let before_expressions = expression_shape(&self.expressions);
        self.refresh_values(cx);
        if fields_shape(&self.sections) != before
            || expression_shape(&self.expressions) != before_expressions
        {
            self.needs_rebuild = true;
        }
    }

    /// The frame under the playhead from the shared `PlaybackPosition`
    /// global (0 when unset, e.g. in tests without playback).
    fn playback_frame(cx: &App) -> u64 {
        cx.try_global::<super::PlaybackPosition>()
            .map(|position| position.frame)
            .unwrap_or(0)
    }

    /// Resolve the current layer target from the live document: the layer
    /// itself plus the eval context inputs (playhead frame, comp fps and
    /// resolution). `None` when the layer or comp is gone (delete, undo) —
    /// the panel then shows the empty state.
    fn resolved_layer(&self, cx: &App) -> Option<ResolvedLayer> {
        let PropertiesTarget::Layer { comp_id, layer_id } = &self.target else {
            return None;
        };
        let comp = self
            .project
            .as_ref()?
            .read(cx)
            .document()
            .get_composition(*comp_id)?
            .clone();
        let layer = comp.get_layer(*layer_id)?.clone();
        let frame = Self::playback_frame(cx);
        Some((comp, layer, frame))
    }

    /// Metadata of the asset the layer's audio source points at, for the
    /// stream picker's options. Read from the document's asset table (filled
    /// at import time): nothing here opens a media file, so the section
    /// builder stays pure.
    fn audio_asset_metadata(&self, layer: &Layer, cx: &App) -> Option<AssetMetadata> {
        let audio = layer.audio.as_ref()?;
        Some(
            self.project
                .as_ref()?
                .read(cx)
                .document()
                .get_media_asset(&audio.asset_id)?
                .metadata
                .clone(),
        )
    }

    /// Resolve a multi-layer target from the live document, dropping layers
    /// that are gone (delete, undo) and keeping selection order. `None` when
    /// the composition itself is gone or nothing is left to show.
    fn resolved_layers(&self, cx: &App) -> Option<ResolvedLayers> {
        let PropertiesTarget::Layers { comp_id, layer_ids } = &self.target else {
            return None;
        };
        let comp = self
            .project
            .as_ref()?
            .read(cx)
            .document()
            .get_composition(*comp_id)?
            .clone();
        let layers: Vec<Layer> = layer_ids
            .iter()
            .filter_map(|id| comp.get_layer(*id).cloned())
            .collect();
        if layers.is_empty() {
            return None;
        }
        let frame = Self::playback_frame(cx);
        Some((comp, layers, frame))
    }

    /// Resolve the current node target from the live document: the selected
    /// nodes, the first node's driven parameters, and the layer-local frame
    /// under the playhead (the same frame edits and the key toggle apply
    /// to, REQ-LAYER-004/006). `None` when the network or every selected
    /// node is gone.
    fn resolved_nodes(&self, cx: &App) -> Option<(Vec<Arc<Node>>, Vec<DrivenParam>, u64)> {
        let PropertiesTarget::Nodes { network, ids } = &self.target else {
            return None;
        };
        let document = self.project.as_ref()?.read(cx).document();
        let graph = resolve_network(document, network)?;
        let nodes: Vec<Arc<Node>> = ids
            .iter()
            .filter_map(|id| graph.node(*id).cloned())
            .collect();
        let first = nodes.first()?.clone();
        let driven = super::node_editor::driven_params(graph, &first, &self.registry);
        let frame = document
            .get_composition(network.comp)
            .and_then(|comp| comp.get_layer(network.layer))
            .map(|layer| layer_local_frame(layer, Self::playback_frame(cx)))
            .unwrap_or(0);
        Some((nodes, driven, frame))
    }

    /// The context an expression-driven node parameter is *displayed*
    /// through: the owning composition's frame rate and resolution.
    ///
    /// A parameter expression may name `fps`, `res.*` and `comp.*`, so the row
    /// cannot show its value without them. Nothing here evaluates the graph —
    /// the numbers come straight out of the composition's settings. Falls back
    /// to a 30 fps, 1×1 context when the network's composition cannot be
    /// resolved, which is also when there is no row to draw.
    fn node_eval_context(&self, cx: &App) -> EvalContext {
        let resolved = (|| {
            let PropertiesTarget::Nodes { network, .. } = &self.target else {
                return None;
            };
            let document = self.project.as_ref()?.read(cx).document();
            let comp = document.get_composition(network.comp)?;
            Some(EvalContext::new(
                Self::playback_frame(cx),
                comp.frame_rate,
                comp.resolution,
            ))
        })();
        resolved.unwrap_or_else(|| EvalContext::new(0, FrameRate::new(30, 1), (1, 1)))
    }

    /// The expression state of the selected node's parameters.
    ///
    /// Only a node target has one: layer shell properties are edited through
    /// `apply_layer_field`, which has no expression path yet, so offering the
    /// badge there would advertise something no click could reach.
    fn expression_rows(&self, cx: &App) -> Vec<(String, ExpressionRow)> {
        let Some((nodes, driven, _)) = self.resolved_nodes(cx) else {
            return Vec::new();
        };
        let Some(node) = nodes.first() else {
            return Vec::new();
        };
        node.parameters
            .iter()
            // A parameter driven by a connected port renders read-only; its
            // stored expression is inert, so the row must not offer to edit it.
            .filter(|parameter| !driven.iter().any(|d| d.key == parameter.key))
            .filter_map(|parameter| {
                let count = expression::channel_count(&parameter.value)?;
                let components = (0..count)
                    .map(|component| {
                        expression::component_expression(&parameter.value, component).map(
                            |stored| ExpressionComponent {
                                source: stored.source().to_string(),
                                error: stored.error().map(|error| error.to_string()),
                            },
                        )
                    })
                    .collect();
                Some((
                    parameter.key.clone(),
                    ExpressionRow {
                        components,
                        attachable: expression::can_attach(&parameter.value),
                    },
                ))
            })
            .collect()
    }

    /// Resolve the current composition target's settings from the live
    /// document. `None` once the composition is gone (deleted, undone) — the
    /// panel then shows its empty state instead of a stale composition.
    fn resolved_composition(&self, cx: &App) -> Option<CompositionSettings> {
        let PropertiesTarget::Composition { comp_id } = &self.target else {
            return None;
        };
        let comp = self
            .project
            .as_ref()?
            .read(cx)
            .document()
            .get_composition(*comp_id)?;
        Some(CompositionSettings::from_composition(comp))
    }

    /// Route a composition field edit into the document (REQ-UI-013).
    ///
    /// Resolution, frame rate, duration, and background change what the
    /// compiled chain renders, so they invalidate structurally; a rename only
    /// changes what the Outliner and the tab show.
    fn apply_composition_change(
        &mut self,
        key: &str,
        value: PropertyValue,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let PropertiesTarget::Composition { comp_id } = &self.target else {
            return;
        };
        let comp_id: CompId = *comp_id;
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(mut settings) = self.resolved_composition(cx) else {
            return;
        };
        if !apply_composition_field(&mut settings, key, &value) {
            return;
        }
        let hint = if key == ravel_ui::properties::composition::FIELD_NAME {
            InvalidationHint::None
        } else {
            InvalidationHint::Structural
        };
        project.update(cx, |project, cx| {
            let Some(doc) =
                update_composition(project.document(), comp_id, |comp| settings.apply_to(comp))
            else {
                return;
            };
            if commit {
                project.commit_document(doc, hint, cx);
            } else {
                project.apply_document(doc, hint, cx);
            }
        });
    }

    /// Route a layer field edit into the document (REQ-LAYER-009).
    fn apply_layer_change(
        &mut self,
        key: &str,
        value: PropertyValue,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let PropertiesTarget::Layer { comp_id, layer_id } = &self.target else {
            return;
        };
        let comp_id: CompId = *comp_id;
        let layer_id = *layer_id;
        let Some(project) = self.project.clone() else {
            return;
        };
        // Channel-backed fields insert/update a key at the layer-local
        // frame under the playhead (REQ-LAYER-004/006); both the frame and
        // the layer's timing come from the live document at call time.
        let Some(layer) = project
            .read(cx)
            .document()
            .get_composition(comp_id)
            .and_then(|comp| comp.get_layer(layer_id))
            .cloned()
        else {
            return;
        };
        let local_frame = layer_local_frame(&layer, Self::playback_frame(cx));

        // Custom parameter edits invalidate the In node; solo/mute/blend/
        // adjustment change the compiled merge chain (REQ-LAYER-007).
        let hint = if key.starts_with(CUSTOM_FIELD_PREFIX) {
            in_node_id(&layer)
                .map(|id| InvalidationHint::Params(vec![id]))
                .unwrap_or(InvalidationHint::None)
        } else {
            match key {
                // `parent` is structural for the same reason as the merge
                // flags: `compile.rs` wires an edge from the parent's
                // synthetic Transform node, so re-parenting changes the
                // compiled graph's shape, not just a value in it.
                "blend_mode" | "solo" | "muted" | "adjustment" | "parent" => {
                    InvalidationHint::Structural
                }
                _ => InvalidationHint::None,
            }
        };

        let key = key.to_string();
        project.update(cx, |project, cx| {
            let mut applied = false;
            let doc = update_layer(project.document(), comp_id, layer_id, |l| {
                applied = apply_layer_field(l, &key, &value, local_frame);
            });
            let Some(doc) = doc else {
                return;
            };
            if !applied {
                return;
            }
            if commit {
                project.commit_document(doc, hint, cx);
            } else {
                project.apply_document(doc, hint, cx);
            }
        });
    }

    /// Toggle a keyframe at the current layer-local frame on the layer
    /// field `key` (REQ-LAYER-004): inserts a key holding the current
    /// value (converting a constant custom `Float` parameter to a
    /// channel), or removes the key from every component. One Document
    /// undo step per click.
    fn toggle_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let PropertiesTarget::Layer { comp_id, layer_id } = &self.target else {
            return;
        };
        let comp_id = *comp_id;
        let layer_id = *layer_id;
        let frame = Self::playback_frame(cx);
        let Some(project) = self.project.clone() else {
            return;
        };
        let hint = if key.starts_with(CUSTOM_FIELD_PREFIX) {
            project
                .read(cx)
                .document()
                .get_composition(comp_id)
                .and_then(|comp| comp.get_layer(layer_id))
                .and_then(in_node_id)
                .map(|id| InvalidationHint::Params(vec![id]))
                .unwrap_or(InvalidationHint::None)
        } else {
            InvalidationHint::None
        };

        let key = key.to_string();
        project.update(cx, |project, cx| {
            let mut toggled = false;
            // Apply to the document's latest layer: the local frame is
            // derived from its current timing.
            let doc = update_layer(project.document(), comp_id, layer_id, |l| {
                let local_frame = layer_local_frame(l, frame);
                toggled = toggle_layer_keyframe(l, &key, local_frame).is_some();
            });
            let Some(doc) = doc else {
                return;
            };
            if toggled {
                project.commit_document(doc, hint, cx);
            }
        });
        // The document observer refreshes the displayed toggle state.
    }

    /// Open or close the inline curve editor of the row `key`.
    ///
    /// Expansion is view state only: nothing here touches the document, so
    /// the toggle records no undo step and rows stay independent (opening
    /// one never closes another).
    fn toggle_curve_expanded(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded_curves.remove(key) {
            self.expanded_curves.insert(key.to_string());
        }
        cx.notify();
    }

    /// Whether the curve row `key` is currently expanded.
    #[cfg(test)]
    fn is_curve_expanded(&self, key: &str) -> bool {
        self.expanded_curves.contains(key)
    }

    /// Height of the row `key`'s expanded editor.
    fn curve_height(&self, key: &str) -> f32 {
        self.curve_heights
            .get(key)
            .copied()
            .unwrap_or(CURVE_EDITOR_HEIGHT)
    }

    fn begin_curve_resize(&mut self, key: String, pointer_y: f32) {
        let start_height = self.curve_height(&key);
        self.curve_resize = Some(CurveResize {
            key,
            start_y: pointer_y,
            start_height,
        });
    }

    fn curve_resize_to(&mut self, pointer_y: f32, cx: &mut Context<Self>) {
        let Some(resize) = &self.curve_resize else {
            return;
        };
        let height = (resize.start_height + (pointer_y - resize.start_y))
            .clamp(CURVE_EDITOR_MIN_HEIGHT, CURVE_EDITOR_MAX_HEIGHT);
        self.curve_heights.insert(resize.key.clone(), height);
        cx.notify();
    }

    fn end_curve_resize(&mut self) {
        self.curve_resize = None;
    }

    /// Run `f` against the live node editor after this panel's current update.
    /// Panels can be detached into separate windows, so cross-window entity
    /// updates must always pass through this deferred boundary.
    fn with_node_editor(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(
            &mut super::node_editor::NodeEditorPanel,
            &mut Context<super::node_editor::NodeEditorPanel>,
        ) + 'static,
    ) {
        let Some(editor) = cx
            .try_global::<super::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade())
        else {
            return;
        };
        cx.defer(move |cx| {
            editor.update(cx, |editor, cx| f(editor, cx));
        });
    }

    // ----- Ports section (REQ-LAYER-002, REQ-LAYER-003) ---------------------

    /// The interface node whose ports the Ports section edits: the first
    /// selected node, the one every section is built from.
    fn port_node_id(&self) -> Option<NodeId> {
        match &self.target {
            PropertiesTarget::Nodes { ids, .. } => ids.first().copied(),
            _ => None,
        }
    }

    /// Run one custom-port edit on the live node editor and keep its refusal.
    ///
    /// The same deferred boundary as [`Self::with_node_editor`] — the editor
    /// may live in another window — with the result routed back here so the
    /// section can show why an edit did not happen. `cx.defer` hands both
    /// updates the App context in turn, so neither entity update nests inside
    /// the other.
    fn route_port_edit(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(
            &mut super::node_editor::NodeEditorPanel,
            NodeId,
            &mut Context<super::node_editor::NodeEditorPanel>,
        ) -> Result<(), NetworkError>
        + 'static,
    ) {
        let Some(node_id) = self.port_node_id() else {
            return;
        };
        let Some(editor) = cx
            .try_global::<super::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade())
        else {
            return;
        };
        let panel = cx.entity().downgrade();
        cx.defer(move |cx| {
            let result = editor.update(cx, |editor, cx| edit(editor, node_id, cx));
            panel
                .update(cx, |this, cx| {
                    this.port_error = result.as_ref().err().map(port_error_message);
                    cx.notify();
                })
                .ok();
        });
    }

    /// Show `key`'s message under the port list without touching the graph —
    /// for the refusals the panel itself makes (an empty name never reaches
    /// the core, which would report it as a duplicate of nothing).
    fn refuse_port_edit(&mut self, message: String, cx: &mut Context<Self>) {
        self.port_error = Some(SharedString::from(message));
        cx.notify();
    }

    /// The Ports row named `name` as the panel last resolved it.
    fn port_row(&self, name: &str) -> Option<&ravel_ui::properties::PortRow> {
        self.sections
            .iter()
            .flat_map(|section| &section.fields)
            .find_map(|field| match field {
                PropertyField::PortList { rows, .. } => rows.iter().find(|row| row.name == name),
                _ => None,
            })
    }

    /// The custom port type behind a Select's current label. The Select
    /// carries translated text, so the answer comes from the menu the panel
    /// built it from.
    fn selected_port_type(
        &self,
        state: &Entity<SelectState<Vec<SharedString>>>,
        cx: &App,
    ) -> Option<CustomPortType> {
        let label = state.read(cx).selected_value().cloned()?;
        self.port_type_for_label(&label)
    }

    fn port_type_for_label(&self, label: &str) -> Option<CustomPortType> {
        self.port_type_options
            .iter()
            .copied()
            .find(|port_type| port_type_label(Some(*port_type)) == label)
    }

    /// Add the port the trailing row describes. The row's Input is not
    /// cleared here — the successful add changes the port list, which
    /// rebuilds the section with a fresh empty row.
    fn add_port(&mut self, cx: &mut Context<Self>) {
        let Some(add) = &self.port_add else {
            return;
        };
        let name = add.name.read(cx).value().trim().to_string();
        let port_type = self.selected_port_type(&add.port_type, cx);
        if name.is_empty() {
            self.refuse_port_edit(t!("properties.ports.error.empty_name"), cx);
            return;
        }
        let Some(port_type) = port_type else {
            return;
        };
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.add_custom_port(node_id, &name, port_type, cx)
        });
    }

    /// Commit a row's edited name on Enter or blur.
    ///
    /// The Input reports both, and Enter is normally followed by one: the
    /// second report carries the *same* pair, because the row's old name is
    /// baked into its subscription and the Input still holds the new text.
    /// Sending it twice would ask the graph to rename a port the first call
    /// already renamed away, and the `PortNotFound` that comes back would put
    /// a failure under a rename that succeeded. So the pair is recorded when
    /// it is sent and an identical repeat is dropped — the same guard shape as
    /// [`Self::apply_color_change`]'s pending commit, released by the rebuild
    /// (or the target change) that retires the widget.
    fn rename_port(&mut self, old_name: &str, new_name: String, cx: &mut Context<Self>) {
        let new_name = new_name.trim().to_string();
        if new_name == old_name {
            return;
        }
        if new_name.is_empty() {
            self.refuse_port_edit(t!("properties.ports.error.empty_name"), cx);
            return;
        }
        let rename = (old_name.to_string(), new_name);
        if self.committed_port_rename.as_ref() == Some(&rename) {
            return;
        }
        self.committed_port_rename = Some(rename.clone());
        let (old_name, new_name) = rename;
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.rename_custom_port(node_id, &old_name, &new_name, cx)
        });
    }

    fn retype_port(&mut self, name: &str, label: &str, cx: &mut Context<Self>) {
        let Some(port_type) = self.port_type_for_label(label) else {
            return;
        };
        // Re-picking the type a row already has is not an edit. A Select emits
        // `Confirm` for the entry that is already selected, so this is the
        // ordinary path rather than a corner case, and `set_custom_port_type`
        // answers it with the graph it was given — which `commit_graph` would
        // still record, leaving an undo step that undoes to an identical
        // document.
        if self
            .port_row(name)
            .is_some_and(|row| row.port_type == Some(port_type))
        {
            return;
        }
        let name = name.to_string();
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.set_custom_port_type(node_id, &name, port_type, cx)
        });
    }

    fn remove_port(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.to_string();
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.remove_custom_port(node_id, &name, cx)
        });
    }

    /// Move a row one slot, which always changes the order.
    ///
    /// No "did anything happen?" guard like [`Self::retype_port`]'s: a handle
    /// is only rendered when the neighbour in that direction exists and is not
    /// fixed, which is exactly when `move_custom_port` moves the port. The two
    /// stopping conditions and the two enablement conditions are the same
    /// pair, so a rendered handle never produces an unchanged graph.
    fn move_port(&mut self, name: &str, offset: i32, cx: &mut Context<Self>) {
        let name = name.to_string();
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.move_custom_port(node_id, &name, offset, cx)
        });
    }

    // ----- Exposed parameter declarations (REQ-PROJ-006) --------------------

    /// The layer-local frame under the playhead for the current node target —
    /// the frame this panel shows animated values at, and the frame a
    /// declaration seeds its default from. `0` when the target is not a node
    /// selection, which is also when nothing reads it.
    fn node_frame(&self, cx: &App) -> u64 {
        self.resolved_nodes(cx)
            .map(|(_, _, frame)| frame)
            .unwrap_or(0)
    }

    /// Which of the first selected node's parameters can be, or already are,
    /// part of the project's external contract (REQ-PROJ-006), keyed by field.
    ///
    /// Both answers come from the core: `seed_value` decides what a parameter
    /// can be declared as, `bound_to` what already is. A parameter nothing can
    /// declare is absent from the map and gets no toggle, rather than one that
    /// always refuses.
    fn exposed_states(
        &self,
        sections: &[PropertySection],
        cx: &App,
    ) -> std::collections::HashMap<String, bool> {
        let PropertiesTarget::Nodes { ids, .. } = &self.target else {
            return std::collections::HashMap::new();
        };
        let (Some(node_id), Some(project)) = (ids.first().copied(), self.project.as_ref()) else {
            return std::collections::HashMap::new();
        };
        let frame = self.node_frame(cx);
        let document = project.read(cx).document();
        sections
            .iter()
            .flat_map(|section| &section.fields)
            .filter(|field| {
                !matches!(
                    field,
                    PropertyField::PortList { .. } | PropertyField::ExposedList { .. }
                )
            })
            .filter_map(|field| {
                let key = field.key();
                let binding = ExposedBinding::new(node_id, key);
                let declared = document.exposed_parameters.bound_to(node_id, key).is_some();
                (declared
                    || ravel_core::exposed::apply::seed_value(document, &binding, frame).is_some())
                .then(|| (key.to_string(), declared))
            })
            .collect()
    }

    /// Run one declaration edit against the document and keep its refusal.
    ///
    /// Declarations live on the `Document`, so unlike a port edit there is no
    /// node editor to route through: the panel edits the project directly, the
    /// way a layer field edit does. `edit` reports whether anything changed, so
    /// a no-op (renaming a row to the name it already has, pressing "up" on the
    /// first row) records no undo step that would undo to an identical
    /// document.
    ///
    /// Every accepted edit is **one** `commit_document`, which is what makes
    /// each operation one undo step. The hint is `None`: a declaration is not
    /// part of the compiled graph — `apply` writes its value into the document
    /// once, before evaluation — so nothing downstream needs invalidating.
    fn edit_declarations(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut ExposedParameters) -> Result<bool, ExposedParameterError>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let mut refusal = None;
        project.update(cx, |project, cx| {
            let mut declarations = project.document().exposed_parameters.clone();
            match edit(&mut declarations) {
                Ok(true) => {
                    let document = project
                        .document()
                        .clone()
                        .with_exposed_parameters(declarations);
                    project.commit_document(document, InvalidationHint::None, cx);
                }
                Ok(false) => {}
                Err(err) => refusal = Some(exposed_error_message(&err)),
            }
        });
        self.exposed_error = refusal;
        cx.notify();
    }

    /// Show a message under the declarations list without touching the
    /// document — for the refusals the panel makes itself.
    fn refuse_declaration_edit(&mut self, key: &str, cx: &mut Context<Self>) {
        self.exposed_error = Some(SharedString::from(ravel_i18n::translate(key)));
        cx.notify();
    }

    /// Commit a row's edited name on Enter or blur.
    ///
    /// The same Enter-then-Blur duplicate guard as [`Self::rename_port`]: one
    /// gesture reports both, carrying the same pair, and sending it twice would
    /// put an `UnknownName` under a rename that succeeded.
    fn rename_declaration(&mut self, old_name: &str, new_name: String, cx: &mut Context<Self>) {
        let new_name = new_name.trim().to_string();
        if new_name == old_name {
            return;
        }
        if new_name.is_empty() {
            self.refuse_declaration_edit("properties.exposed.error.empty_name", cx);
            return;
        }
        let rename = (old_name.to_string(), new_name);
        if self.committed_exposed_rename.as_ref() == Some(&rename) {
            return;
        }
        self.committed_exposed_rename = Some(rename.clone());
        let (old_name, new_name) = rename;
        self.edit_declarations(cx, move |declarations| {
            declarations.rename(&old_name, &new_name).map(|()| true)
        });
    }

    /// Commit a row's edited description on Enter or blur. Unlike a rename this
    /// cannot collide, so the only guard is "did the text actually change".
    fn describe_declaration(&mut self, name: &str, description: String, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let current = project
            .read(cx)
            .document()
            .exposed_parameters
            .get(name)
            .map(|declaration| declaration.description().to_string());
        if current.as_deref() == Some(description.as_str()) {
            return;
        }
        let name = name.to_string();
        self.edit_declarations(cx, move |declarations| {
            declarations
                .set_description(&name, description)
                .map(|()| true)
        });
    }

    fn remove_declaration(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.to_string();
        self.edit_declarations(cx, move |declarations| {
            Ok(declarations.remove(&name).is_some())
        });
    }

    /// Move a row one slot. The handle is only rendered when a neighbour
    /// exists in that direction, which is exactly when `shift` moves it, so a
    /// rendered handle never records an undo step that changes nothing.
    fn move_declaration(&mut self, name: &str, offset: i32, cx: &mut Context<Self>) {
        let name = name.to_string();
        self.edit_declarations(cx, move |declarations| declarations.shift(&name, offset));
    }

    /// Declare `key` on `node_id` as a project input, named after the
    /// parameter.
    ///
    /// The type and the default come from
    /// [`ravel_core::exposed::apply::seed_value`] — the one place that maps a
    /// parameter onto the value space of the external contract — so a
    /// declaration made here always binds back to the parameter it came from.
    /// A parameter that has no place in a contract (a path, a curve, a media
    /// node with no asset) is refused with the core's reason rather than
    /// declared and then reported as broken.
    ///
    /// The default is seeded at the playhead's layer-local frame, the frame
    /// this panel is showing the value at: exposing a keyframed parameter
    /// gives the contract the number the user can see, not a `0.0` chosen by
    /// nothing. That the animated components will not *take* a caller's value
    /// is reported by `resolve` in the declarations list, next to the row.
    ///
    /// Exposing is **not** a toggle back off. Removing a declaration removes a
    /// name callers may already be passing on a command line, so it is done
    /// deliberately from the declarations list, not by clicking the same 14px
    /// icon that created it. Clicking an already-exposed parameter says so
    /// instead.
    fn expose_parameter(&mut self, node_id: NodeId, key: &str, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let binding = ExposedBinding::new(node_id, key);
        let frame = self.node_frame(cx);
        let document = project.read(cx).document();
        if document.exposed_parameters.bound_to(node_id, key).is_some() {
            self.refuse_declaration_edit("properties.exposed.error.already_exposed", cx);
            return;
        }
        let Some(seed) = ravel_core::exposed::apply::seed_value(document, &binding, frame) else {
            self.refuse_declaration_edit("properties.exposed.error.not_exposable", cx);
            return;
        };
        let name = key.to_string();
        self.edit_declarations(cx, move |declarations| {
            let declaration = ExposedParameter::inferred(name, seed, binding)?;
            declarations.insert(declaration).map(|()| true)
        });
    }

    /// Route a field edit to its target: document-owned targets edit the
    /// document here, while node targets call the owning node editor.
    fn route_change(
        &mut self,
        key: &str,
        value: PropertyValue,
        commit: bool,
        node_ids: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        if matches!(self.target, PropertiesTarget::Layer { .. }) {
            self.apply_layer_change(key, value, commit, cx);
            return;
        }
        if matches!(self.target, PropertiesTarget::Composition { .. }) {
            self.apply_composition_change(key, value, commit, cx);
            return;
        }
        // A multi-layer target has no editable field (REQ-UI-013 v1): a widget
        // left over from the previous single-layer target must not route an
        // edit at a selection this panel cannot apply it to.
        if matches!(self.target, PropertiesTarget::Layers { .. }) {
            return;
        }
        // Defensive: a stale widget binding (e.g. an in-flight scrub whose
        // parameter just became driven by a connected port) must not edit
        // the inert stored fallback.
        if let PropertiesTarget::Nodes { .. } = &self.target
            && let Some((_, driven, _)) = self.resolved_nodes(cx)
            && driven.iter().any(|d| d.key == key)
        {
            return;
        }
        if node_ids.is_empty() {
            return;
        }
        let node_ids = node_ids.to_vec();
        let key = key.to_string();
        self.with_node_editor(cx, move |editor, cx| {
            editor.apply_property_change(&node_ids, &key, &value, commit, cx);
        });
    }

    /// Commit the current text once on Enter or blur. Updating the retained
    /// section value before routing also suppresses the blur that follows an
    /// Enter from creating a second undo step.
    fn commit_string_change(
        &mut self,
        key: &str,
        value: String,
        node_ids: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .any(|field| {
                matches!(
                    field,
                    PropertyField::String {
                        key: field_key,
                        value: current,
                    } if field_key == key && current == &value
                )
            });
        if unchanged {
            return;
        }
        let property_value = PropertyValue::String(value);
        self.update_field_value(key, &property_value);
        self.route_change(key, property_value, true, node_ids, cx);
    }

    /// Apply a color picker change live and debounce the undo commit: the
    /// picker emits `Change` per slider tick without a gesture-end event,
    /// so the commit fires after [`COLOR_COMMIT_QUIET`] of silence (one
    /// undo step per picker gesture, REQ-LAYER-009 granularity).
    fn apply_color_change(
        &mut self,
        key: &str,
        value: PropertyValue,
        node_ids: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        self.route_change(key, value.clone(), false, node_ids, cx);
        self.color_commit_generation += 1;
        let generation = self.color_commit_generation;
        self.pending_color_commit = Some((key.to_string(), value));
        let ids = node_ids.to_vec();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COLOR_COMMIT_QUIET).await;
            this.update(cx, |this, cx| {
                if this.color_commit_generation != generation {
                    return;
                }
                let Some((key, value)) = this.pending_color_commit.take() else {
                    return;
                };
                this.route_change(&key, value, true, &ids, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Push the sections' current color values into idle picker widgets so
    /// undo, playback, and external edits refresh the swatch
    /// (`ColorPickerState::set_value` needs a `Window`, so this runs from
    /// `render` rather than the global observers). A pending uncommitted
    /// edit means the picker is the source of truth — skip the sync.
    fn sync_color_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_color_commit.is_some() {
            return;
        }
        let mut updates: Vec<(Entity<ColorPickerState>, Hsla)> = Vec::new();
        for section in &self.sections {
            for field in &section.fields {
                let PropertyField::Color { key, r, g, b, a } = field else {
                    continue;
                };
                let Some((_, binding)) = self.colors.iter().find(|(k, _)| k == key) else {
                    continue;
                };
                let differs = binding.state.read(cx).value().is_none_or(|current| {
                    let current = Rgba::from(current);
                    (current.r - r).abs() > 1e-3
                        || (current.g - g).abs() > 1e-3
                        || (current.b - b).abs() > 1e-3
                        || (current.a - a).abs() > 1e-3
                });
                if differs {
                    updates.push((binding.state.clone(), hsla_from_rgba(*r, *g, *b, *a)));
                }
            }
        }
        for (state, value) in updates {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// Push refreshed field values into idle text inputs. Both the focus query
    /// and `InputState::set_value` need a `Window`, so refresh observers update
    /// `sections` and the next render performs this synchronization.
    fn sync_string_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut updates: Vec<(Entity<InputState>, String)> = Vec::new();
        for section in &self.sections {
            for field in &section.fields {
                let PropertyField::String { key, value } = field else {
                    continue;
                };
                let Some((_, binding)) = self.strings.iter().find(|(k, _)| k == key) else {
                    continue;
                };
                let state = binding.state.read(cx);
                if !state.focus_handle(cx).is_focused(window) && state.value().as_ref() != value {
                    updates.push((binding.state.clone(), value.clone()));
                }
            }
        }
        for (state, value) in updates {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// Push refreshed enum values into retained select widgets (same
    /// Window-dependent render-time pattern as `sync_string_widgets`), so
    /// external changes — undo/redo, a same-target refresh — reach the
    /// dropdown selection.
    fn sync_select_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        type SelectUpdate = (
            Entity<SelectState<Vec<SharedString>>>,
            Option<gpui_component::IndexPath>,
        );
        let mut updates: Vec<SelectUpdate> = Vec::new();
        for section in &self.sections {
            for field in &section.fields {
                let PropertyField::Enum {
                    key,
                    value,
                    options,
                } = field
                else {
                    continue;
                };
                let Some((_, binding)) = self.selects.iter().find(|(k, _)| k == key) else {
                    continue;
                };
                // The Select holds the option's *label*, so compare like for
                // like — a value that is a locale key would otherwise never
                // match and the index would be re-set on every render.
                let selected = enum_option_label(value);
                let current = binding.state.read(cx).selected_value().cloned();
                if current.as_deref() != Some(selected.as_str()) {
                    let idx = options
                        .iter()
                        .position(|o| o == value)
                        .map(|i| gpui_component::IndexPath::default().row(i));
                    updates.push((binding.state.clone(), idx));
                }
            }
        }
        for (state, idx) in updates {
            state.update(cx, |state, cx| state.set_selected_index(idx, window, cx));
        }
    }

    fn update_field_value(&mut self, key: &str, value: &PropertyValue) {
        for section in &mut self.sections {
            for field in &mut section.fields {
                if field.key() != key {
                    continue;
                }
                match (field, value) {
                    (PropertyField::Float { value: v, .. }, PropertyValue::Float(new)) => {
                        *v = *new;
                    }
                    (PropertyField::Int { value: v, .. }, PropertyValue::Int(new)) => {
                        *v = *new;
                    }
                    (PropertyField::Bool { value: v, .. }, PropertyValue::Bool(new)) => {
                        *v = *new;
                    }
                    (PropertyField::String { value: v, .. }, PropertyValue::String(new)) => {
                        *v = new.clone();
                    }
                    (PropertyField::Enum { value: v, .. }, PropertyValue::String(new)) => {
                        *v = new.clone();
                    }
                    (
                        PropertyField::Color { r, g, b, a, .. },
                        PropertyValue::Color {
                            r: nr,
                            g: ng,
                            b: nb,
                            a: na,
                        },
                    ) => {
                        (*r, *g, *b, *a) = (*nr, *ng, *nb, *na);
                    }
                    (PropertyField::Vector { components, .. }, PropertyValue::Vector(new)) => {
                        components.clone_from(new);
                    }
                    (PropertyField::Curve { curve, .. }, PropertyValue::Curve(new)) => {
                        curve.clone_from(new);
                    }
                    _ => {}
                }
            }
        }
    }

    fn sections_for_target(&self, cx: &App) -> Vec<PropertySection> {
        match &self.target {
            PropertiesTarget::Empty => Vec::new(),
            PropertiesTarget::Nodes { network, .. } => match self.resolved_nodes(cx) {
                // Animated channels display their value at the playhead's
                // layer-local frame — the same frame edits and the key
                // toggle apply to (REQ-LAYER-004/006).
                Some((nodes, driven, frame)) => {
                    let node = nodes.first().expect("non-empty");
                    // The Ports section of an interface node offers the types
                    // this network's position admits (REQ-LAYER-002/003).
                    let eval = self.node_eval_context(cx);
                    let mut sections = sections_for_node(
                        node,
                        &self.registry,
                        frame,
                        &eval,
                        &driven,
                        network.context(),
                    );
                    append_node_description(&mut sections, &node.type_key);
                    sections
                }
                None => Vec::new(),
            },
            PropertiesTarget::Layer { .. } => match self.resolved_layer(cx) {
                Some((comp, layer, frame)) => {
                    let ctx =
                        ravel_core::eval::EvalContext::new(frame, comp.frame_rate, comp.resolution);
                    // The audio stream picker lists the streams the asset
                    // table already recorded at import time — reading the
                    // document, never probing the file (audio-plan unit 4).
                    let audio_asset = self.audio_asset_metadata(&layer, cx);
                    sections_for_layer(&layer, &comp, &ctx, audio_asset.as_ref())
                }
                None => Vec::new(),
            },
            // A multi-layer selection is read-only in v1: the count plus the
            // fields the layers agree on (REQ-UI-013).
            PropertiesTarget::Layers { .. } => match self.resolved_layers(cx) {
                Some((comp, layers, frame)) => {
                    let ctx =
                        ravel_core::eval::EvalContext::new(frame, comp.frame_rate, comp.resolution);
                    let layers: Vec<&Layer> = layers.iter().collect();
                    sections_for_layers(&layers, &comp, &ctx)
                }
                None => Vec::new(),
            },
            // Composition settings are plain fields: no channels, no
            // playhead, nothing to sample (REQ-UI-013).
            PropertiesTarget::Composition { .. } => match self.resolved_composition(cx) {
                Some(settings) => sections_for_composition(&settings),
                None => Vec::new(),
            },
            // A media asset shows a placeholder until unit 6 builds the real
            // inspector (metadata, path editing, relink).
            PropertiesTarget::MediaAsset { .. } => Vec::new(),
            // The project's external parameter contract (REQ-PROJ-006). The
            // section exists even when nothing is declared, so the list has
            // somewhere to say so.
            PropertiesTarget::Project => match &self.project {
                Some(project) => vec![exposed_section(project.read(cx).document())],
                None => Vec::new(),
            },
        }
    }

    /// Update section values (and idle scrub widgets) from the current
    /// target without recreating widget entities, so an in-flight scrub
    /// keeps its state.
    fn refresh_values(&mut self, cx: &mut Context<Self>) {
        self.sections = self.sections_for_target(cx);
        self.expressions = self.expression_rows(cx);
        let mut updates: Vec<(String, f32)> = Vec::new();
        for section in &self.sections {
            for field in &section.fields {
                match field {
                    PropertyField::Float { value, .. } => {
                        updates.push((field.key().to_string(), *value));
                    }
                    PropertyField::Int { value, .. } => {
                        updates.push((field.key().to_string(), *value as f32));
                    }
                    PropertyField::Vector {
                        key, components, ..
                    } => {
                        let keys = vector_component_keys(key, components.len());
                        updates.extend(keys.into_iter().zip(components.iter().copied()));
                    }
                    // Color pickers and string inputs refresh during render:
                    // their focus/value APIs need a `Window`, which global
                    // observers do not have.
                    _ => {}
                }
            }
        }
        for (key, value) in updates {
            if let Some((_, binding)) = self.scrubs.iter().find(|(k, _)| k == &key) {
                binding.state.update(cx, |state, cx| {
                    if !state.is_dragging() {
                        state.set_value(value);
                        cx.notify();
                    }
                });
            }
        }

        // Curve editors follow the document the same way: an in-flight point
        // drag owns its curve until the gesture ends (`set_curve` is a no-op
        // while dragging), so undo and external edits reach idle editors only.
        let curves: Vec<(String, ravel_core::param_curve::CurveParam)> = self
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .filter_map(|field| match field {
                PropertyField::Curve { key, curve } => Some((key.clone(), curve.clone())),
                _ => None,
            })
            .collect();
        for (key, curve) in curves {
            if let Some((_, binding)) = self.curves.iter().find(|(k, _)| k == &key) {
                binding.state.update(cx, |state, cx| {
                    if state.curve() != &curve {
                        state.set_curve_synced(curve, cx);
                        cx.notify();
                    }
                });
            }
        }
    }

    fn rebuild_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let span = tracing::debug_span!("rebuild_widgets");
        let _guard = span.enter();
        self.needs_rebuild = false;
        self.expression_inputs.clear();
        self.scrubs.clear();
        self.strings.clear();
        self.selects.clear();
        self.colors.clear();
        self.curves.clear();
        self.port_names.clear();
        self.port_types.clear();
        self.port_add = None;
        self.port_type_options.clear();
        self.exposed_names.clear();
        self.exposed_descriptions.clear();
        // The rename records belong to the Inputs being replaced here.
        self.committed_port_rename = None;
        self.committed_exposed_rename = None;

        let sections = self.sections_for_target(cx);
        self.expressions = self.expression_rows(cx);
        let node_ids = match &self.target {
            PropertiesTarget::Nodes { ids, .. } => ids.clone(),
            _ => Vec::new(),
        };
        // A draft belongs to a component that still has a box. Once the
        // component stops being driven — detached, or the target changed —
        // there is nothing to commit it into, and keeping it would let a stale
        // source reappear in an unrelated parameter that happens to share the
        // key.
        let driven = self.expressions.clone();
        self.expression_drafts.retain(|(key, component, _)| {
            driven
                .iter()
                .find(|(field_key, _)| field_key == key)
                .and_then(|(_, row)| row.components.get(*component))
                .is_some_and(Option::is_some)
        });
        self.build_expression_inputs(&node_ids, window, cx);

        for section in &sections {
            for field in &section.fields {
                // (value, hard range, ui range, integer?) for numeric fields.
                let numeric = match field {
                    PropertyField::Float {
                        value,
                        range,
                        ui_range,
                        ..
                    } => Some((*value, range.clone(), ui_range.clone(), false)),
                    PropertyField::Int {
                        value,
                        range,
                        ui_range,
                        ..
                    } => Some((
                        *value as f32,
                        range
                            .clone()
                            .map(|r| (*r.start() as f32)..=(*r.end() as f32)),
                        ui_range
                            .clone()
                            .map(|r| (*r.start() as f32)..=(*r.end() as f32)),
                        true,
                    )),
                    _ => None,
                };

                if let Some((value, hard, ui, integer)) = numeric {
                    let key = field.key().to_string();
                    let state = ScrubInputState::new(value)
                        .hard_range(hard)
                        .ui_range(ui)
                        .integer(integer);
                    let entity = cx.new(|_| state);
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub = cx.subscribe(&entity, move |this, _state, event: &ScrubEvent, cx| {
                        let (val, commit) = match event {
                            ScrubEvent::Change(v) => (*v, false),
                            ScrubEvent::Commit(v) => (*v, true),
                        };
                        let value = if integer {
                            PropertyValue::Int(val.round() as i32)
                        } else {
                            PropertyValue::Float(val)
                        };
                        this.route_change(&field_key, value, commit, &ids, cx);
                    });
                    self.scrubs.push((key, ScrubBinding { state: entity, sub }));
                }

                if let PropertyField::String { key, value } = field {
                    let entity =
                        cx.new(|cx| InputState::new(window, cx).default_value(value.clone()));
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub = cx.subscribe_in(
                        &entity,
                        window,
                        move |this, state, event: &InputEvent, _window, cx| match event {
                            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                                let value = state.read(cx).value().to_string();
                                this.commit_string_change(&field_key, value, &ids, cx);
                            }
                            InputEvent::Change | InputEvent::Focus => {}
                        },
                    );
                    self.strings
                        .push((key.clone(), StringBinding { state: entity, sub }));
                }

                if let PropertyField::Vector {
                    key,
                    components,
                    range,
                    ui_range,
                    ..
                } = field
                {
                    let component_keys = vector_component_keys(key, components.len());
                    for (component, (component_key, value)) in
                        component_keys.into_iter().zip(components).enumerate()
                    {
                        let state = ScrubInputState::new(*value)
                            .hard_range(range.clone())
                            .ui_range(ui_range.clone());
                        let entity = cx.new(|_| state);
                        let field_key = key.clone();
                        let ids = node_ids.clone();
                        let sub =
                            cx.subscribe(&entity, move |this, _state, event: &ScrubEvent, cx| {
                                let (val, commit) = match event {
                                    ScrubEvent::Change(v) => (*v, false),
                                    ScrubEvent::Commit(v) => (*v, true),
                                };
                                // The other components keep their current
                                // section values.
                                let Some(PropertyField::Vector { components, .. }) = this
                                    .sections
                                    .iter()
                                    .flat_map(|s| &s.fields)
                                    .find(|f| f.key() == field_key)
                                else {
                                    return;
                                };
                                let mut components = components.clone();
                                if component >= components.len() {
                                    return;
                                }
                                components[component] = val;
                                let value = PropertyValue::Vector(components);
                                this.route_change(&field_key, value, commit, &ids, cx);
                            });
                        self.scrubs
                            .push((component_key, ScrubBinding { state: entity, sub }));
                    }
                }

                if let PropertyField::Color { key, r, g, b, a } = field {
                    let entity = cx.new(|cx| {
                        ColorPickerState::new(window, cx)
                            .default_value(hsla_from_rgba(*r, *g, *b, *a))
                    });
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub = cx.subscribe(
                        &entity,
                        move |this, _state, event: &ColorPickerEvent, cx| {
                            let ColorPickerEvent::Change(Some(hsla)) = event else {
                                return;
                            };
                            // Note: the picker speaks display-referred Hsla;
                            // parameter colors are stored as plain 0-1 RGBA
                            // with no transfer function (the pipeline is not
                            // color-managed yet, REQ-COLOR is a later
                            // milestone).
                            let rgba = Rgba::from(*hsla);
                            let value = PropertyValue::Color {
                                r: rgba.r,
                                g: rgba.g,
                                b: rgba.b,
                                a: rgba.a,
                            };
                            this.apply_color_change(&field_key, value, &ids, cx);
                        },
                    );
                    self.colors
                        .push((key.clone(), ColorBinding { state: entity, sub }));
                }

                if let PropertyField::Curve { key, curve } = field {
                    let entity = cx.new(|cx| ParamCurveEditorState::new(curve.clone(), cx));
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub =
                        cx.subscribe(&entity, move |this, _state, event: &ParamCurveEvent, cx| {
                            // Same gesture granularity as a scrub: live point
                            // moves apply without undo, the gesture's Commit
                            // records one Document undo step.
                            let (curve, commit) = match event {
                                ParamCurveEvent::Change(curve) => (curve.clone(), false),
                                ParamCurveEvent::Commit(curve) => (curve.clone(), true),
                            };
                            let value = PropertyValue::Curve(curve);
                            this.update_field_value(&field_key, &value);
                            this.route_change(&field_key, value, commit, &ids, cx);
                        });
                    self.curves
                        .push((key.clone(), CurveBinding { state: entity, sub }));
                }

                if let PropertyField::Enum {
                    key,
                    value,
                    options,
                } = field
                {
                    let items: Vec<SharedString> = options
                        .iter()
                        .map(|option| SharedString::from(enum_option_label(option)))
                        .collect();
                    let selected_idx = options.iter().position(|o| o == value);
                    let idx_path =
                        selected_idx.map(|i| gpui_component::IndexPath::default().row(i));
                    let entity = cx.new(|cx| SelectState::new(items.clone(), idx_path, window, cx));
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    // The Select answers with the *label*; the stored options
                    // travel beside it so the edit writes the value, not the
                    // wording (the Ports type menu does the same).
                    let stored = options.clone();
                    let sub = cx.subscribe_in(
                        &entity,
                        window,
                        move |this, _state, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                            if let SelectEvent::Confirm(Some(val)) = event {
                                let Some(option) = items
                                    .iter()
                                    .position(|label| label == val)
                                    .and_then(|index| stored.get(index))
                                else {
                                    return;
                                };
                                let value = PropertyValue::String(option.clone());
                                this.route_change(&field_key, value, true, &ids, cx);
                            }
                        },
                    );
                    self.selects
                        .push((key.clone(), SelectBinding { state: entity, sub }));
                }

                if let PropertyField::PortList { rows, options, .. } = field {
                    self.build_port_widgets(rows, options, window, cx);
                }

                if let PropertyField::ExposedList { rows, .. } = field {
                    self.build_exposed_widgets(rows, window, cx);
                }
            }
        }
        self.sections = sections;
    }

    /// Build the declarations section's widgets: a name Input and a
    /// description Input per row.
    ///
    /// There is no widget for the type or the default. Both are decided when
    /// the parameter is exposed and are read back from the declaration, so an
    /// editor for them would offer to build a contract the core would refuse.
    fn build_exposed_widgets(
        &mut self,
        rows: &[ExposedRow],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for row in rows {
            let entity = cx.new(|cx| InputState::new(window, cx).default_value(row.name.clone()));
            let old_name = row.name.clone();
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, state, event: &InputEvent, _window, cx| match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        let value = state.read(cx).value().to_string();
                        this.rename_declaration(&old_name, value, cx);
                    }
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
            self.exposed_names
                .push((row.name.clone(), StringBinding { state: entity, sub }));

            let entity = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(row.description.clone())
                    .placeholder(SharedString::from(t!("properties.exposed.description")))
            });
            let name = row.name.clone();
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, state, event: &InputEvent, _window, cx| match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        let value = state.read(cx).value().to_string();
                        this.describe_declaration(&name, value, cx);
                    }
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
            self.exposed_descriptions
                .push((row.name.clone(), StringBinding { state: entity, sub }));
        }
    }

    /// Build one text Input per expression-driven component.
    ///
    /// Committing on Enter *and* on blur is what makes the editor
    /// non-obstructive: the author can click away mid-expression and the text
    /// is kept. `set_param_expression` stores whatever is in the box, compiling
    /// or not, so nothing here inspects the source before sending it.
    ///
    /// `Change` does **not** commit. It records a draft and compiles it for
    /// the error message, so the author sees a syntax error as they type
    /// without every keystroke becoming an undo step. This is the one text
    /// field in the panel that behaves this way; the rest commit on blur
    /// alone, because only an expression has an error worth showing before the
    /// edit is finished.
    ///
    /// A rebuilt Input is seeded from its draft when there is one, so widgets
    /// replaced mid-edit do not drop the author's half-typed source.
    fn build_expression_inputs(
        &mut self,
        node_ids: &[NodeId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.expressions.clone();
        for (key, row) in rows {
            for (component, stored) in row.components.iter().enumerate() {
                let Some(stored) = stored else {
                    continue;
                };
                let initial = self
                    .expression_draft(&key, component)
                    .map_or_else(|| stored.source.clone(), |draft| draft.source.clone());
                let entity = cx.new(|cx| InputState::new(window, cx).default_value(initial));
                let field_key = key.clone();
                let ids = node_ids.to_vec();
                let sub = cx.subscribe_in(
                    &entity,
                    window,
                    move |this, state, event: &InputEvent, _window, cx| match event {
                        InputEvent::PressEnter { .. } | InputEvent::Blur => {
                            this.commit_expression_draft(&field_key, component, &ids, cx);
                        }
                        InputEvent::Change => {
                            let source = state.read(cx).value().to_string();
                            this.note_expression_draft(&field_key, component, source, cx);
                        }
                        InputEvent::Focus => {}
                    },
                );
                self.expression_inputs.push((
                    key.clone(),
                    component,
                    StringBinding { state: entity, sub },
                ));
            }
        }
    }

    /// The uncommitted text of one component, if the author has typed into it
    /// since its last commit.
    fn expression_draft(&self, key: &str, component: usize) -> Option<&ExpressionDraft> {
        self.expression_drafts
            .iter()
            .find(|(k, index, _)| k == key && *index == component)
            .map(|(_, _, draft)| draft)
    }

    /// The source one component holds in the document.
    fn committed_expression(&self, key: &str, component: usize) -> Option<&str> {
        self.expressions
            .iter()
            .find(|(field_key, _)| field_key == key)
            .and_then(|(_, row)| row.components.get(component))
            .and_then(|stored| stored.as_ref())
            .map(|stored| stored.source.as_str())
    }

    /// Record a keystroke without touching the document, and refresh the error
    /// shown beneath the box.
    ///
    /// Text that matches the document again *clears* the draft rather than
    /// storing one: the component is back to committed, so an external change
    /// may sync into it and a following blur has nothing to write.
    fn note_expression_draft(
        &mut self,
        key: &str,
        component: usize,
        source: String,
        cx: &mut Context<Self>,
    ) {
        self.expression_drafts
            .retain(|(k, index, _)| !(k == key && *index == component));
        if self.committed_expression(key, component) != Some(source.as_str()) {
            let error = expression::compile_error(&source);
            self.expression_drafts.push((
                key.to_string(),
                component,
                ExpressionDraft { source, error },
            ));
        }
        cx.notify();
    }

    /// Commit the draft of one component, if it has one.
    ///
    /// No draft means nothing was typed since the last commit, so there is
    /// nothing to write — and writing anyway is precisely the bug this guards:
    /// the box's text lags the document until the next render, so a blur after
    /// an undo would re-commit the value the undo removed.
    fn commit_expression_draft(
        &mut self,
        key: &str,
        component: usize,
        node_ids: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .expression_drafts
            .iter()
            .position(|(k, i, _)| k == key && *i == component)
        else {
            return;
        };
        let (_, _, draft) = self.expression_drafts.remove(index);
        self.commit_expression_change(key, component, draft.source, node_ids, cx);
    }

    /// Push committed expression sources into idle boxes, so an undo, a redo
    /// or an external edit reaches the text the author is looking at.
    ///
    /// `InputState::set_value` needs a `Window`, so this runs from `render`
    /// like the other widget syncs. A component with a draft is skipped: the
    /// author is mid-edit and owns the text until they confirm or discard it.
    fn sync_expression_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut updates: Vec<(Entity<InputState>, String)> = Vec::new();
        for (key, component, binding) in &self.expression_inputs {
            if self.expression_draft(key, *component).is_some() {
                continue;
            }
            let Some(source) = self.committed_expression(key, *component) else {
                continue;
            };
            if binding.state.read(cx).value().as_ref() != source {
                updates.push((binding.state.clone(), source.to_string()));
            }
        }
        for (state, value) in updates {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// Send one component's edited source to the node editor, which owns the
    /// graph and the undo step.
    ///
    /// The panel does not check whether the source compiles: a broken
    /// expression is stored, shown with its error, and evaluated as the
    /// channel default. Refusing to commit it would throw away exactly the
    /// text the author is still working on.
    fn commit_expression_change(
        &mut self,
        key: &str,
        component: usize,
        source: String,
        node_ids: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .expressions
            .iter()
            .find(|(field_key, _)| field_key == key)
            .and_then(|(_, row)| row.components.get(component))
            .and_then(|stored| stored.as_ref())
            .is_some_and(|stored| stored.source == source);
        if unchanged {
            return;
        }
        let Some(node_id) = node_ids.first().copied() else {
            return;
        };
        let key = key.to_string();
        self.with_node_editor(cx, move |editor, cx| {
            editor.set_param_expression(node_id, &key, component, &source, cx);
        });
    }

    /// Build the Ports section's widgets: a name Input and a type Select per
    /// editable row, plus the trailing add row.
    ///
    /// Built-in rows get none. They are shown so the list matches the node,
    /// but the shell owns them: `network::is_fixed_port` refuses every edit to
    /// one, so offering a widget would only promise something the core would
    /// then reject.
    fn build_port_widgets(
        &mut self,
        rows: &[ravel_ui::properties::PortRow],
        options: &[CustomPortType],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.port_type_options = options.to_vec();
        let labels: Vec<SharedString> = options
            .iter()
            .map(|port_type| SharedString::from(port_type_label(Some(*port_type))))
            .collect();

        for row in rows.iter().filter(|row| !row.fixed) {
            let entity = cx.new(|cx| InputState::new(window, cx).default_value(row.name.clone()));
            let old_name = row.name.clone();
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, state, event: &InputEvent, _window, cx| match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        let value = state.read(cx).value().to_string();
                        this.rename_port(&old_name, value, cx);
                    }
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
            self.port_names
                .push((row.name.clone(), StringBinding { state: entity, sub }));

            // A row whose wire type no menu entry describes starts unselected
            // rather than silently claiming to be the first type in the list.
            let selected = options
                .iter()
                .position(|port_type| Some(*port_type) == row.port_type)
                .map(|index| gpui_component::IndexPath::default().row(index));
            let entity = cx.new(|cx| SelectState::new(labels.clone(), selected, window, cx));
            let name = row.name.clone();
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, _state, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                    if let SelectEvent::Confirm(Some(label)) = event {
                        this.retype_port(&name, label, cx);
                    }
                },
            );
            self.port_types
                .push((row.name.clone(), SelectBinding { state: entity, sub }));
        }

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(SharedString::from(t!("properties.ports.new_name")))
        });
        let sub = cx.subscribe_in(
            &name,
            window,
            move |this, _state, event: &InputEvent, _window, cx| {
                // Enter adds; a blur does not, so clicking away from a
                // half-typed name abandons it instead of creating a port.
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_port(cx);
                }
            },
        );
        let port_type = cx.new(|cx| {
            SelectState::new(
                labels,
                Some(gpui_component::IndexPath::default().row(0)),
                window,
                cx,
            )
        });
        self.port_add = Some(PortAddBinding {
            name,
            port_type,
            sub,
        });
    }
}

impl Focusable for PropertiesGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PropertiesGpuiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_rebuild {
            self.rebuild_widgets(window, cx);
        }
        // Widget-state consumption, same as the rebuild above: propagate
        // refreshed section colors into retained picker widgets.
        self.sync_color_widgets(window, cx);
        self.sync_string_widgets(window, cx);
        self.sync_expression_widgets(window, cx);
        self.sync_select_widgets(window, cx);

        let mut content = div()
            .id("properties-panel")
            // Test hook for `VisualTestContext::debug_bounds` (noop in
            // release builds).
            .debug_selector(|| "properties-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .text_xs()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle);

        if self.sections.is_empty() {
            // A media asset target has no sections yet (unit 6 builds the
            // real inspector); say so instead of showing the generic empty
            // state, which would read as "nothing selected".
            let message = match &self.target {
                PropertiesTarget::MediaAsset { .. } => {
                    t!("panel.properties.media_asset_placeholder")
                }
                _ => t!("panel.properties.empty"),
            };
            content = content.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(message)),
            );
        } else {
            let sections = self.sections.clone();
            let scrub_entities: Vec<(String, Entity<ScrubInputState>)> = self
                .scrubs
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone()))
                .collect();
            let string_entities: Vec<(String, Entity<InputState>)> = self
                .strings
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone()))
                .collect();
            let select_entities: Vec<(String, Entity<SelectState<Vec<SharedString>>>)> = self
                .selects
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone()))
                .collect();
            let color_entities: Vec<(String, Entity<ColorPickerState>)> = self
                .colors
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone()))
                .collect();
            // Curve rows: the editor entity, whether the row is open, and the
            // height it was dragged to — all panel view state.
            let curve_entities: Vec<(String, Entity<ParamCurveEditorState>, f32)> = self
                .curves
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone(), self.curve_height(k)))
                .collect();
            let expanded_curves = self.expanded_curves.clone();
            let expression_entities: Vec<(String, usize, Entity<InputState>)> = self
                .expression_inputs
                .iter()
                .map(|(k, component, b)| (k.clone(), *component, b.state.clone()))
                .collect();
            let expression_rows = self.expressions.clone();
            let expression_drafts = self.expression_drafts.clone();
            let port_widgets = PortWidgets {
                names: self
                    .port_names
                    .iter()
                    .map(|(k, b)| (k.clone(), b.state.clone()))
                    .collect(),
                types: self
                    .port_types
                    .iter()
                    .map(|(k, b)| (k.clone(), b.state.clone()))
                    .collect(),
                add: self
                    .port_add
                    .as_ref()
                    .map(|add| (add.name.clone(), add.port_type.clone())),
                error: self.port_error.clone(),
            };
            let exposed_widgets = ExposedWidgets {
                names: self
                    .exposed_names
                    .iter()
                    .map(|(k, b)| (k.clone(), b.state.clone()))
                    .collect(),
                descriptions: self
                    .exposed_descriptions
                    .iter()
                    .map(|(k, b)| (k.clone(), b.state.clone()))
                    .collect(),
                error: self.exposed_error.clone(),
            };
            let muted = cx.theme().colors.muted_foreground;
            let fg = cx.theme().colors.foreground;
            let danger = cx.theme().colors.danger;
            let mono_family = cx.theme().mono_font_family.clone();
            // Dimmer than `muted`: a control that is present but cannot act.
            let disabled = cx.theme().colors.border;
            // Active-state color of the ◆/◎/● toggles: theme primary, so
            // keyed / exposed states stand out from the muted chrome.
            let active = cx.theme().colors.primary;
            let editor = cx.entity().downgrade();
            let node_ids = match &self.target {
                PropertiesTarget::Nodes { ids, .. } => ids.clone(),
                _ => Vec::new(),
            };

            // Keyframe state (◆/◇) per animatable field key
            // (REQ-LAYER-004), read from the live document: layer fields
            // ask the resolved layer, node fields the resolved first node,
            // both at the playhead's layer-local frame.
            let key_target: Option<KeyTarget> = match &self.target {
                PropertiesTarget::Layer { .. } => Some(KeyTarget::Layer(cx.entity().downgrade())),
                PropertiesTarget::Nodes { ids, .. } => ids.first().copied().map(KeyTarget::Node),
                // Composition settings cannot be animated, and a multi-layer
                // selection is read-only, so neither offers a keyframe toggle.
                PropertiesTarget::Composition { .. }
                | PropertiesTarget::Layers { .. }
                | PropertiesTarget::MediaAsset { .. }
                | PropertiesTarget::Project
                | PropertiesTarget::Empty => None,
            };
            let resolved_layer = match &self.target {
                PropertiesTarget::Layer { .. } => self.resolved_layer(cx),
                _ => None,
            };
            let resolved_nodes = match &self.target {
                PropertiesTarget::Nodes { .. } => self.resolved_nodes(cx),
                _ => None,
            };
            let key_states: std::collections::HashMap<String, bool> = match &self.target {
                PropertiesTarget::Layer { .. } => match &resolved_layer {
                    Some((_, layer, frame)) => {
                        let local_frame = layer_local_frame(layer, *frame);
                        sections
                            .iter()
                            .flat_map(|section| &section.fields)
                            .filter_map(|field| {
                                layer_field_keyframed(layer, field.key(), local_frame)
                                    .map(|keyed| (field.key().to_string(), keyed))
                            })
                            .collect()
                    }
                    None => std::collections::HashMap::new(),
                },
                PropertiesTarget::Nodes { .. } => match &resolved_nodes {
                    // Driven parameters render read-only; their stored
                    // keyframes are inert, so the key toggle is hidden.
                    Some((nodes, driven, frame)) => {
                        let node = nodes.first().expect("resolved nodes are non-empty");
                        sections
                            .iter()
                            .flat_map(|section| &section.fields)
                            .filter(|field| !driven.iter().any(|d| d.key == field.key()))
                            .filter_map(|field| {
                                node_param_keyed(node, field.key(), Some(*frame))
                                    .map(|keyed| (field.key().to_string(), keyed))
                            })
                            .collect()
                    }
                    None => std::collections::HashMap::new(),
                },
                PropertiesTarget::Composition { .. }
                | PropertiesTarget::Layers { .. }
                | PropertiesTarget::MediaAsset { .. }
                | PropertiesTarget::Project
                | PropertiesTarget::Empty => std::collections::HashMap::new(),
            };

            // Per-parameter port toggle states for the first selected node
            // (param-input-ports-plan Phase 4).
            let port_states: std::collections::HashMap<String, PortToggleState> =
                match &resolved_nodes {
                    Some((nodes, driven, _)) => {
                        let node = nodes.first().expect("resolved nodes are non-empty");
                        if node.supports_param_ports() {
                            node.parameters
                                .iter()
                                .filter(|p| p.value.port_data_type().is_some())
                                .map(|p| {
                                    let state = if driven.iter().any(|d| d.key == p.key) {
                                        PortToggleState::Connected
                                    } else if node.param_port_index(&p.key).is_some() {
                                        PortToggleState::Exposed
                                    } else {
                                        PortToggleState::Unexposed
                                    };
                                    (p.key.clone(), state)
                                })
                                .collect()
                        } else {
                            std::collections::HashMap::new()
                        }
                    }
                    None => std::collections::HashMap::new(),
                };
            let port_node = match &self.target {
                PropertiesTarget::Nodes { ids, .. } => ids.first().copied(),
                _ => None,
            };

            // Which of the first selected node's parameters can be, or already
            // are, part of the project's external contract (REQ-PROJ-006).
            // Both answers come from the core: `seed_value` decides what can be
            // declared, `bound_to` what already is. The panel only draws them.
            let exposed_states = self.exposed_states(&sections, cx);

            let mut accordion = Accordion::new("properties-accordion")
                .multiple(true)
                .small();
            for section in sections {
                let fields = section.fields.clone();
                let title: SharedString = ravel_i18n::translate(&section.title).into();
                let scrubs = scrub_entities.clone();
                let strings = string_entities.clone();
                let selects = select_entities.clone();
                let colors = color_entities.clone();
                let curves = curve_entities.clone();
                let expanded_curves = expanded_curves.clone();
                let ports = port_widgets.clone();
                let declarations = exposed_widgets.clone();
                let editor = editor.clone();
                let node_ids = node_ids.clone();
                let key_target = key_target.clone();
                let key_states = key_states.clone();
                let port_states = port_states.clone();
                let exposed_states = exposed_states.clone();
                let expression_entities = expression_entities.clone();
                let expression_rows = expression_rows.clone();
                let expression_drafts = expression_drafts.clone();
                let mono_family = mono_family.clone();

                accordion = accordion.item(move |item| {
                    let mut container = div().flex().flex_col().w_full();
                    for field in &fields {
                        let row = build_field_row(
                            field,
                            &scrubs,
                            &strings,
                            &selects,
                            &colors,
                            &expanded_curves,
                            &ports,
                            &declarations,
                            &editor,
                            &node_ids,
                            muted,
                            fg,
                            danger,
                        );
                        // The inline curve editor sits directly under its own
                        // row, so several open editors stay readable and each
                        // one stays next to the parameter it edits.
                        let curve_body = match field {
                            PropertyField::Curve { key, .. } if expanded_curves.contains(key) => {
                                curves.iter().find(|(k, _, _)| k == key).map(
                                    |(key, state, height)| {
                                        curve_editor_body(key, state, *height, &editor, muted)
                                    },
                                )
                            }
                            _ => None,
                        };
                        let key_button = match (&key_target, key_states.get(field.key())) {
                            (Some(target), Some(keyed)) => Some(key_toggle_button(
                                field.key(),
                                *keyed,
                                target,
                                active,
                                muted,
                            )),
                            _ => None,
                        };
                        let expression_row = expression_rows
                            .iter()
                            .find(|(k, _)| k == field.key())
                            .map(|(_, row)| row);
                        // The editor sits directly under its own row, like the
                        // inline curve editor, so a vector's components stay
                        // next to the values they drive.
                        let expression_body = match (port_node, expression_row) {
                            (Some(_), Some(row)) if row.is_attached() => {
                                Some(expression_editor_body(
                                    field.key(),
                                    row,
                                    &expression_entities,
                                    &expression_drafts,
                                    mono_family.clone(),
                                    muted,
                                    danger,
                                ))
                            }
                            _ => None,
                        };
                        let expression_button = match (port_node, expression_row) {
                            (Some(node_id), Some(row)) => Some(expression_toggle_button(
                                field.key(),
                                row.is_attached(),
                                row.attachable,
                                node_id,
                                active,
                                muted,
                                disabled,
                            )),
                            _ => None,
                        };
                        let port_button = match (port_node, port_states.get(field.key())) {
                            (Some(node_id), Some(state)) => Some(port_toggle_button(
                                field.key(),
                                *state,
                                node_id,
                                active,
                                muted,
                            )),
                            _ => None,
                        };
                        let exposed_button = match (port_node, exposed_states.get(field.key())) {
                            (Some(node_id), Some(declared)) => Some(exposed_toggle_button(
                                field.key(),
                                *declared,
                                node_id,
                                &editor,
                                active,
                                muted,
                            )),
                            _ => None,
                        };
                        if key_button.is_none()
                            && port_button.is_none()
                            && exposed_button.is_none()
                            && expression_button.is_none()
                        {
                            container = container.child(row);
                        } else {
                            let mut wrapper = div().flex().items_center();
                            if let Some(button) = exposed_button {
                                wrapper = wrapper.child(button);
                            }
                            if let Some(button) = port_button {
                                wrapper = wrapper.child(button);
                            }
                            if let Some(button) = key_button {
                                wrapper = wrapper.child(button);
                            }
                            if let Some(button) = expression_button {
                                wrapper = wrapper.child(button);
                            }
                            container = container
                                .child(wrapper.child(div().flex_grow().min_w_0().child(row)));
                        }
                        if let Some(body) = curve_body {
                            container = container.child(body);
                        }
                        if let Some(body) = expression_body {
                            container = container.child(body);
                        }
                    }
                    item.title(title.clone()).open(true).child(container)
                });
            }
            // The Accordion fills its parent (`size_full` with `flex_1`
            // items whose content is `overflow_hidden`), so as a direct
            // child of the scroll container it would squash the sections
            // into the panel height and clip them instead of overflowing.
            // A shrink-proof wrapper with an indefinite height lets the
            // accordion measure to its content, giving the root something
            // to scroll.
            content = content.child(
                div()
                    .id("properties-scroll-content")
                    // Test hook for `VisualTestContext::debug_bounds`.
                    .debug_selector(|| "properties-scroll-content".into())
                    .w_full()
                    .flex_shrink_0()
                    .child(accordion),
            );
        }

        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one.
    use core::prelude::v1::test;
    use gpui::TestAppContext;
    use ravel_core::animation::channel::ParameterExpression;
    use ravel_core::composition::{AudioSource, BlendMode, Layer};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, LayerId};
    use ravel_core::network as net;
    use ravel_core::param_curve::CurveParam;
    use ravel_ui::properties::layer::{PARENT_NONE, parse_parent_option};

    fn network_with_custom_param() -> Graph {
        use ravel_core::animation::channel::AnimationChannel;
        let in_node = Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(1.0))
            .with_output("tint", DataTypeId::COLOR)
            .with_param(
                "tint",
                ParameterValue::Channel4([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                ]),
            );
        let out = Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out)
            .unwrap()
    }

    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<PropertiesGpuiPanel>,
        Entity<ProjectState>,
        CompId,
        LayerId,
    ) {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ));
            cx.set_global(SelectedPropertiesTarget::default());
        });

        let (comp_id, lid) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let lid = LayerId::next();
            let layer = Layer::new(lid, "L", network_with_custom_param()).with_time(0, 0, 300);
            let doc = ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (comp_id, lid)
        });

        let window = cx.add_window(|window, cx| {
            PropertiesGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        window
            .update(cx, |panel, _window, _cx| {
                panel.target = PropertiesTarget::Layer {
                    comp_id,
                    layer_id: lid,
                };
            })
            .unwrap();
        (window, project, comp_id, lid)
    }

    fn layer(
        project: &Entity<ProjectState>,
        comp: CompId,
        lid: LayerId,
        cx: &mut TestAppContext,
    ) -> Layer {
        project.read_with(cx, |project, _| {
            project
                .document()
                .get_composition(comp)
                .unwrap()
                .get_layer(lid)
                .unwrap()
                .clone()
        })
    }

    fn setup_node_target(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<PropertiesGpuiPanel>,
        gpui::WindowHandle<super::super::node_editor::NodeEditorPanel>,
        Entity<ProjectState>,
        ravel_ui::document::NetworkPath,
        NodeId,
    ) {
        let node_id = NodeId::next();
        let node = Node::new(node_id, "test")
            .with_param("amount", ParameterValue::Float(1.0))
            .with_param("name", ParameterValue::String("Original".into()))
            .with_param("enabled", ParameterValue::Bool(false))
            .with_param(
                "tint",
                ParameterValue::Channel4([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                ]),
            );
        setup_target_for_node(cx, node)
    }

    /// Selects `node` in a fresh layer network and returns the Properties
    /// panel bound to it, plus the node editor the panel routes edits through.
    fn setup_target_for_node(
        cx: &mut TestAppContext,
        node: Node,
    ) -> (
        gpui::WindowHandle<PropertiesGpuiPanel>,
        gpui::WindowHandle<super::super::node_editor::NodeEditorPanel>,
        Entity<ProjectState>,
        ravel_ui::document::NetworkPath,
        NodeId,
    ) {
        let (properties, project, comp_id, lid) = setup(cx);
        let node_id = node.id;
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, lid, |layer| {
                layer.network = layer.network.clone().add_node(node.clone()).unwrap();
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        let path = ravel_ui::document::NetworkPath::layer(comp_id, lid);
        cx.update(|cx| {
            cx.set_global(super::super::CanvasSelection {
                path: Some(path.clone()),
                nodes: [node_id].into_iter().collect(),
            });
        });
        let editor = cx.add_window(|window, cx| {
            super::super::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });
        editor
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
            })
            .unwrap();
        properties
            .update(cx, |panel, window, cx| panel.rebuild_widgets(window, cx))
            .unwrap();

        (properties, editor, project, path, node_id)
    }

    /// Selects the In node of the layer network `setup` builds and returns
    /// the Properties panel bound to it, plus the node editor its port edits
    /// route through.
    fn setup_in_node_target(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<PropertiesGpuiPanel>,
        Entity<ProjectState>,
        ravel_ui::document::NetworkPath,
        NodeId,
    ) {
        let (properties, project, comp_id, lid) = setup(cx);
        let path = ravel_ui::document::NetworkPath::layer(comp_id, lid);
        let in_id = project.read_with(cx, |project, _| {
            let graph = resolve_network(project.document(), &path).expect("network");
            net::find_in_node(graph)
                .expect("the layer network has an In node")
                .id
        });
        cx.update(|cx| {
            cx.set_global(super::super::CanvasSelection {
                path: Some(path.clone()),
                nodes: [in_id].into_iter().collect(),
            });
        });
        let editor = cx.add_window(|window, cx| {
            super::super::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });
        editor
            .update(cx, |panel, _window, cx| {
                panel.open_network(path.clone(), cx);
            })
            .unwrap();
        properties
            .update(cx, |panel, window, cx| panel.rebuild_widgets(window, cx))
            .unwrap();
        (properties, project, path, in_id)
    }

    /// The Ports section's rows as `(name, type, fixed)`, re-resolved from the
    /// live document.
    fn port_rows(
        properties: &gpui::WindowHandle<PropertiesGpuiPanel>,
        cx: &mut TestAppContext,
    ) -> Vec<(String, Option<CustomPortType>, bool)> {
        properties
            .update(cx, |panel, window, cx| {
                panel.refresh_values(cx);
                panel.rebuild_widgets(window, cx);
                panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .find_map(|field| match field {
                        PropertyField::PortList { rows, .. } => Some(
                            rows.iter()
                                .map(|row| (row.name.clone(), row.port_type, row.fixed))
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .expect("the In node has a Ports section")
            })
            .unwrap()
    }

    fn node_parameter(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        key: &str,
        cx: &mut TestAppContext,
    ) -> ParameterValue {
        project.read_with(cx, |project, _| {
            resolve_network(project.document(), path)
                .and_then(|graph| graph.node(node_id))
                .and_then(|node| node.parameters.iter().find(|param| param.key == key))
                .map(|param| param.value.clone())
                .unwrap_or_else(|| panic!("missing {key} parameter"))
        })
    }

    /// A multi-layer target shows the count and the fields the layers agree on,
    /// read-only: no editable widget is built, and a routed edit is refused so a
    /// widget left from the previous single-layer target cannot write through it
    /// (REQ-UI-013 v1).
    #[gpui::test]
    fn a_multi_layer_target_is_read_only(cx: &mut TestAppContext) {
        let (window, project, comp_id, first) = setup(cx);
        let second = project.update(cx, |project, cx| {
            let second = LayerId::next();
            let mut layer =
                Layer::new(second, "Other", network_with_custom_param()).with_time(0, 0, 300);
            layer.muted = true;
            let doc = ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            second
        });

        window
            .update(cx, |panel, window, cx| {
                panel.target = PropertiesTarget::Layers {
                    comp_id,
                    layer_ids: vec![first, second],
                };
                panel.rebuild_widgets(window, cx);
                panel.refresh_values(cx);

                let field = |key: &str| {
                    panel
                        .sections
                        .iter()
                        .flat_map(|section| &section.fields)
                        .find(|field| field.key() == key)
                        .cloned()
                        .unwrap_or_else(|| panic!("{key} missing"))
                };
                let read_only = |key: &str| match field(key) {
                    ravel_ui::properties::PropertyField::ReadOnly { value, .. } => value,
                    other => panic!("{key} must be read-only: {other:?}"),
                };
                assert_eq!(read_only("selected_count"), "2");
                assert_eq!(read_only("name"), ravel_ui::properties::layer::MIXED_VALUE);
                assert_eq!(read_only("muted"), ravel_ui::properties::layer::MIXED_VALUE);
                assert_eq!(read_only("start_frame"), "0", "a shared value resolves");
                // A merged boolean is a locale key; the panel translates it
                // at the display boundary (the loaded catalog decides the word).
                assert_eq!(read_only("locked"), ravel_ui::properties::layer::VALUE_OFF);
                assert!(
                    panel.scrubs.is_empty() && panel.strings.is_empty(),
                    "a read-only target builds no editable widget"
                );

                // A stale binding from the previous target must not edit.
                panel.route_change("position_x", PropertyValue::Float(42.0), true, &[], cx);
            })
            .unwrap();

        let eval = ravel_core::eval::EvalContext::new(
            0,
            ravel_core::types::FrameRate::new(30, 1),
            (16, 16),
        );
        assert_eq!(
            layer(&project, comp_id, first, cx).transform.position[0].evaluate(0.0, &eval),
            0.0,
            "a multi-layer target applies no edit in v1"
        );
    }

    /// A shell scrub gesture edits the document with one undo step.
    #[gpui::test]
    fn shell_edit_lands_in_the_document_with_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change("position_x", PropertyValue::Float(10.0), false, cx);
                panel.apply_layer_change("position_x", PropertyValue::Float(30.0), true, cx);
            })
            .unwrap();
        let eval = ravel_core::eval::EvalContext::new(
            0,
            ravel_core::types::FrameRate::new(30, 1),
            (16, 16),
        );
        assert!(
            (layer(&project, comp_id, lid, cx).transform.position[0].evaluate(0.0, &eval) - 30.0)
                .abs()
                < f32::EPSILON
        );

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!(
            layer(&project, comp_id, lid, cx).transform.position[0].evaluate(0.0, &eval) == 0.0
        );
    }

    /// The Audio section uses the same shell-channel toggle and Document undo
    /// path as opacity. Redo is part of the assertion because `undo()` also
    /// returns true when it merely reverts an uncommitted preview.
    #[gpui::test]
    fn audio_gain_keyframe_is_one_undo_step_and_redoes(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, lid, |layer| {
                layer.audio = Some(AudioSource::new("music", 0));
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        window
            .update(cx, |panel, _window, cx| panel.toggle_key("gain", cx))
            .unwrap();
        let is_keyframed = |layer: &Layer| {
            matches!(
                layer.audio.as_ref().unwrap().gain.source,
                ChannelSource::Keyframes(ref curve)
                    if curve.keyframes().iter().any(|key| key.frame == 0)
            )
        };
        assert!(is_keyframed(&layer(&project, comp_id, lid, cx)));

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(matches!(
            layer(&project, comp_id, lid, cx)
                .audio
                .as_ref()
                .unwrap()
                .gain
                .source,
            ChannelSource::Constant(value) if (value - 1.0).abs() < f32::EPSILON
        ));

        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!(
            is_keyframed(&layer(&project, comp_id, lid, cx)),
            "redo must restore the committed gain keyframe"
        );
    }

    /// The Audio section's stream picker lists the asset's audio streams from
    /// the document (never a probe) and applies the selected container index
    /// to the shell (audio-plan unit 4).
    #[gpui::test]
    fn audio_stream_picker_lists_and_applies_the_container_streams(cx: &mut TestAppContext) {
        use ravel_core::composition::{AudioStreamMetadata, MediaAssetEntry};

        let (window, project, comp_id, lid) = setup(cx);
        project.update(cx, |project, cx| {
            let mut entry = MediaAssetEntry::from_absolute("/media/clip.mov");
            entry.metadata.audio_stream_count = 2;
            entry.metadata.audio_streams = vec![
                AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("aac".into()),
                    sample_rate: 48_000,
                    channels: 2,
                },
                AudioStreamMetadata {
                    stream_index: 2,
                    codec: Some("pcm_s16le".into()),
                    sample_rate: 44_100,
                    channels: 1,
                },
            ];
            let doc = project
                .document()
                .clone()
                .with_media_asset_entry("clip".to_string(), entry);
            let doc = update_layer(&doc, comp_id, lid, |layer| {
                layer.audio = Some(AudioSource::new("clip", 1));
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        let options = window
            .update(cx, |panel, _window, cx| {
                panel.refresh_values(cx);
                panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .find_map(|field| match field {
                        PropertyField::Enum { key, options, .. } if key == "stream_index" => {
                            Some(options.clone())
                        }
                        _ => None,
                    })
                    .expect("stream picker")
            })
            .unwrap();
        assert_eq!(
            options,
            ["1: aac 48000 Hz 2 ch", "2: pcm_s16le 44100 Hz 1 ch"],
            "the streams recorded on the asset, not a probe"
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change(
                    "stream_index",
                    PropertyValue::String(options[1].clone()),
                    true,
                    cx,
                );
            })
            .unwrap();
        assert_eq!(
            layer(&project, comp_id, lid, cx)
                .audio
                .as_ref()
                .unwrap()
                .stream_index,
            2
        );
    }

    /// Enter commits the string edit, and the following blur is ignored as
    /// unchanged so the layer rename records exactly one undo step.
    #[gpui::test]
    fn string_edit_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.rebuild_widgets(window, cx);
                panel.commit_string_change("name", "Renamed".into(), &[], cx);
                panel.commit_string_change("name", "Renamed".into(), &[], cx);
            })
            .unwrap();
        assert_eq!(layer(&project, comp_id, lid, cx).name, "Renamed");

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert_eq!(layer(&project, comp_id, lid, cx).name, "L");
    }

    /// A color picker gesture (multiple `Change` events) applies live and
    /// records exactly one Document undo step after the debounce quiet
    /// period.
    #[gpui::test]
    fn color_picker_gesture_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        let tint = |l: &Layer| -> f32 {
            let eval = ravel_core::eval::EvalContext::new(
                0,
                ravel_core::types::FrameRate::new(30, 1),
                (16, 16),
            );
            let ParameterValue::Channel4(chs) = &net::find_in_node(&l.network)
                .unwrap()
                .parameters
                .iter()
                .find(|p| p.key == "tint")
                .unwrap()
                .value
            else {
                panic!("expected Channel4");
            };
            chs[0].evaluate(0.0, &eval)
        };

        window
            .update(cx, |panel, _window, cx| {
                for r in [0.2, 0.4, 0.6] {
                    panel.apply_color_change(
                        "custom.tint",
                        PropertyValue::Color {
                            r,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        },
                        &[],
                        cx,
                    );
                }
            })
            .unwrap();
        // Live changes applied, commit still pending.
        assert!((tint(&layer(&project, comp_id, lid, cx)) - 0.6).abs() < 1e-6);

        cx.executor().advance_clock(COLOR_COMMIT_QUIET * 2);
        cx.run_until_parked();

        // One undo restores the pre-gesture color.
        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!((tint(&layer(&project, comp_id, lid, cx)) - 1.0).abs() < 1e-6);
    }

    /// Blend / adjustment edits route through with a structural hint (the
    /// compiled merge chain changes shape).
    #[gpui::test]
    fn compositing_edits_apply(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change(
                    "blend_mode",
                    PropertyValue::String("Screen".into()),
                    true,
                    cx,
                );
                panel.apply_layer_change("adjustment", PropertyValue::Bool(true), true, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, lid, cx);
        assert_eq!(l.blend_mode, BlendMode::Screen);
        assert!(l.adjustment);
    }

    /// Node-target booleans use the same deferred direct-call route as the
    /// other node parameter editors and still produce one undo step.
    #[gpui::test]
    fn node_bool_edit_routes_as_one_commit(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_node_target(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.route_change("enabled", PropertyValue::Bool(true), true, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            node_parameter(&project, &path, node_id, "enabled", cx),
            ParameterValue::Bool(true)
        );
        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert_eq!(
            node_parameter(&project, &path, node_id, "enabled", cx),
            ParameterValue::Bool(false)
        );
        // Redo proves the edit was *committed*: `DocumentStore::undo` also
        // returns true when it merely reverts an uncommitted live preview, and
        // that path leaves nothing to redo.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "enabled", cx),
            ParameterValue::Bool(true)
        );
    }

    /// Live node scrubs refresh the displayed section through the document
    /// observer, without the removed self-observed one-shot Global. The final
    /// call still commits the whole gesture as one undo step.
    #[gpui::test]
    fn node_scrub_refreshes_display_and_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_node_target(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.route_change("amount", PropertyValue::Float(10.0), false, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "amount"), Some(10.0));
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.route_change("amount", PropertyValue::Float(20.0), true, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            ParameterValue::Float(20.0)
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            ParameterValue::Float(1.0)
        );
        // The gesture really committed: only a committed step can be redone
        // (undo of an uncommitted preview is a revert with no redo entry).
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            ParameterValue::Float(20.0)
        );
    }

    /// Enter followed by blur is still de-duplicated locally while the actual
    /// node write uses the deferred direct call.
    #[gpui::test]
    fn node_string_edit_commits_once_without_self_observation(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_node_target(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.commit_string_change("name", "Renamed".into(), &[node_id], cx);
                panel.commit_string_change("name", "Renamed".into(), &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            node_parameter(&project, &path, node_id, "name", cx),
            ParameterValue::String("Renamed".into())
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "name", cx),
            ParameterValue::String("Original".into())
        );
        // Exactly one committed step: the redo restores the rename, and a
        // second undo would have to reach past it if the blur had committed
        // again.
        project.update(cx, |project, cx| {
            assert!(project.redo(cx));
            assert!(!project.redo(cx), "the blur did not commit a second step");
        });
        assert_eq!(
            node_parameter(&project, &path, node_id, "name", cx),
            ParameterValue::String("Renamed".into())
        );
    }

    /// Node color edits remain live while the quiet-period commit is pending,
    /// then record exactly one undo step through the same direct route.
    #[gpui::test]
    fn node_color_picker_debounce_commits_once_without_self_observation(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_node_target(cx);

        window
            .update(cx, |panel, _window, cx| {
                for r in [0.2, 0.4, 0.6] {
                    panel.apply_color_change(
                        "tint",
                        PropertyValue::Color {
                            r,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        },
                        &[node_id],
                        cx,
                    );
                }
            })
            .unwrap();
        cx.run_until_parked();
        let ParameterValue::Channel4(channels) =
            node_parameter(&project, &path, node_id, "tint", cx)
        else {
            panic!("tint remains a color channel");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(0.6)));

        cx.executor().advance_clock(COLOR_COMMIT_QUIET * 2);
        cx.run_until_parked();
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let ParameterValue::Channel4(channels) =
            node_parameter(&project, &path, node_id, "tint", cx)
        else {
            panic!("tint remains a color channel");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(1.0)));

        // The quiet period committed once: the redo brings the last live value
        // back, and there is no second step behind it.
        project.update(cx, |project, cx| {
            assert!(project.redo(cx));
            assert!(!project.redo(cx), "the gesture is one undo step");
        });
        let ParameterValue::Channel4(channels) =
            node_parameter(&project, &path, node_id, "tint", cx)
        else {
            panic!("tint remains a color channel");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(0.6)));
    }

    /// Custom In-node parameters edit the layer's network (REQ-LAYER-002).
    #[gpui::test]
    fn custom_parameter_edit_updates_the_in_node(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change("custom.amount", PropertyValue::Float(7.5), true, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, lid, cx);
        let value = net::find_in_node(&l.network)
            .unwrap()
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .and_then(|p| p.value.as_float());
        assert_eq!(value, Some(7.5));
    }

    /// The key toggle converts a constant custom parameter into a keyframed
    /// channel in the document, and one undo restores the constant
    /// (REQ-LAYER-004).
    #[gpui::test]
    fn key_toggle_converts_the_custom_param_and_undoes(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_key("custom.amount", cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, lid, cx);
        let param = net::find_in_node(&l.network)
            .unwrap()
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(channel) = &param.value else {
            panic!("custom param converted to a channel: {:?}", param.value);
        };
        let ravel_core::animation::channel::ChannelSource::Keyframes(curve) = &channel.source
        else {
            panic!("keyed at the current frame: {:?}", channel.source);
        };
        assert_eq!(curve.len(), 1);
        assert!((curve.sample(0.0) - 1.0).abs() < f32::EPSILON);

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        let l = layer(&project, comp_id, lid, cx);
        let value = net::find_in_node(&l.network)
            .unwrap()
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .and_then(|p| p.value.as_float());
        assert_eq!(value, Some(1.0));
    }

    /// When the sections exceed the panel height, the scroll container must
    /// see the full content height (regression: the Accordion's fill sizing
    /// squashed the sections into the panel height, so nothing scrolled).
    #[gpui::test]
    fn overflowing_sections_give_the_root_scrollable_content(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _lid) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.rebuild_widgets(window, cx);
            })
            .unwrap();

        // Shrink the window so the sections cannot possibly fit.
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(320.0), px(160.0)));
        cx.run_until_parked();

        let root = visual
            .debug_bounds("properties-panel")
            .expect("panel root bounds");
        let content = visual
            .debug_bounds("properties-scroll-content")
            .expect("scroll content bounds");
        assert!(
            content.size.height > root.size.height,
            "content {:?} must overflow the panel {:?}",
            content.size,
            root.size,
        );
    }

    /// Current value shown for a `Float` field, as displayed by the panel.
    fn displayed_float(panel: &PropertiesGpuiPanel, key: &str) -> Option<f32> {
        panel
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .find_map(|field| match field {
                PropertyField::Float {
                    key: field_key,
                    value,
                    ..
                } if field_key == key => Some(*value),
                _ => None,
            })
    }

    /// A document edit committed outside the panel (no republish of
    /// `SelectedPropertiesTarget`) is reflected in the displayed value.
    #[gpui::test]
    fn external_commit_refreshes_the_displayed_value(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        window
            .update(cx, |panel, _window, cx| panel.refresh_values(cx))
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(0.0));
            })
            .unwrap();

        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, lid, |l| {
                l.transform.position[0] = AnimationChannel::constant(42.0);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(42.0));
            })
            .unwrap();
    }

    /// Moving the playhead updates the displayed value of a keyframed
    /// layer channel (previously layer targets skipped playback updates).
    #[gpui::test]
    fn playback_position_updates_animated_layer_values(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, lid, |l| {
                let mut curve = ravel_core::animation::curve::KeyframeCurve::new();
                curve.insert(
                    0,
                    0.0,
                    ravel_core::animation::interpolation::Interpolation::Linear,
                );
                curve.insert(
                    100,
                    100.0,
                    ravel_core::animation::interpolation::Interpolation::Linear,
                );
                l.transform.position[0] = AnimationChannel::new(ChannelSource::Keyframes(curve));
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        window
            .update(cx, |panel, _window, cx| panel.refresh_values(cx))
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(0.0));
            })
            .unwrap();

        cx.update(|cx| {
            cx.set_global(crate::panels::PlaybackPosition {
                frame: 50,
                fps: ravel_core::types::FrameRate::new(30, 1),
            });
        });

        window
            .update(cx, |panel, _window, _cx| {
                let value = displayed_float(panel, "position_x").expect("position_x field");
                assert!((value - 50.0).abs() < 1e-3, "expected 50.0, got {value}");
            })
            .unwrap();
    }

    /// Undoing a panel edit is reflected in the displayed value.
    #[gpui::test]
    fn undo_refreshes_the_displayed_value(cx: &mut TestAppContext) {
        let (window, project, _comp_id, _lid) = setup(cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change("position_x", PropertyValue::Float(30.0), true, cx);
            })
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(30.0));
            })
            .unwrap();

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });

        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(0.0));
            })
            .unwrap();
    }

    /// Deleting the shown layer leaves the panel in the empty state.
    #[gpui::test]
    fn deleted_layer_shows_the_empty_state(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        window
            .update(cx, |panel, _window, cx| panel.refresh_values(cx))
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(!panel.sections.is_empty());
            })
            .unwrap();

        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::remove_layer(project.document(), comp_id, lid).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.sections.is_empty());
            })
            .unwrap();
    }

    // ----- Parent picker (layer-shell-wiring plan, unit 5) ------------------

    /// The `parent` row as the panel currently resolves it.
    fn parent_row(panel: &PropertiesGpuiPanel) -> (String, Vec<String>) {
        panel
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .find_map(|field| match field {
                PropertyField::Enum {
                    key,
                    value,
                    options,
                } if key == "parent" => Some((value.clone(), options.clone())),
                _ => None,
            })
            .expect("the layer sections carry a Parent picker")
    }

    /// Add a second layer under the shown one, to parent it to.
    fn add_sibling(
        project: &Entity<ProjectState>,
        comp_id: CompId,
        name: &str,
        cx: &mut TestAppContext,
    ) -> LayerId {
        project.update(cx, |project, cx| {
            let lid = LayerId::next();
            let layer = Layer::new(lid, name, network_with_custom_param()).with_time(0, 0, 300);
            let doc = ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            lid
        })
    }

    /// Picking a parent reaches the document and one undo takes it back —
    /// the same granularity as every other shell edit.
    #[gpui::test]
    fn picking_a_parent_reaches_the_document_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        let other = add_sibling(&project, comp_id, "Parent", cx);
        window
            .update(cx, |panel, _window, cx| panel.refresh_values(cx))
            .unwrap();

        let option = window
            .update(cx, |panel, _window, _cx| {
                let (value, options) = parent_row(panel);
                assert_eq!(value, PARENT_NONE, "the layer starts unparented");
                options
                    .into_iter()
                    .find(|option| parse_parent_option(option) == Some(other))
                    .expect("the sibling is offered as a parent")
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change("parent", PropertyValue::String(option), true, cx);
            })
            .unwrap();
        assert_eq!(layer(&project, comp_id, lid, cx).parent, Some(other));

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            layer(&project, comp_id, lid, cx).parent,
            None,
            "one undo takes the whole re-parenting back"
        );
        project.update(cx, |project, cx| {
            assert!(project.redo(cx));
            assert!(!project.redo(cx), "the edit is one undo step");
        });
        assert_eq!(layer(&project, comp_id, lid, cx).parent, Some(other));
    }

    /// Clearing the link is one undo step too — "(none)" is an ordinary
    /// option of the picker, not a separate gesture.
    #[gpui::test]
    fn clearing_the_parent_is_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        let other = add_sibling(&project, comp_id, "Parent", cx);
        project.update(cx, |project, cx| {
            let doc =
                update_layer(project.document(), comp_id, lid, |l| l.parent = Some(other)).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_layer_change(
                    "parent",
                    PropertyValue::String(PARENT_NONE.into()),
                    true,
                    cx,
                );
            })
            .unwrap();
        assert_eq!(layer(&project, comp_id, lid, cx).parent, None);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(layer(&project, comp_id, lid, cx).parent, Some(other));
    }

    /// A layer never offers itself or one of its descendants as a parent: the
    /// picker is what keeps a parenting cycle out of the document.
    #[gpui::test]
    fn the_picker_omits_the_candidates_that_would_close_a_cycle(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        let child = add_sibling(&project, comp_id, "Child", cx);
        let grandchild = add_sibling(&project, comp_id, "Grandchild", cx);
        let free = add_sibling(&project, comp_id, "Free", cx);
        project.update(cx, |project, cx| {
            let doc =
                update_layer(project.document(), comp_id, child, |l| l.parent = Some(lid)).unwrap();
            let doc = update_layer(&doc, comp_id, grandchild, |l| l.parent = Some(child)).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.refresh_values(cx);
                let (_, options) = parent_row(panel);
                let offered: Vec<Option<LayerId>> = options
                    .iter()
                    .map(|option| parse_parent_option(option))
                    .collect();
                assert_eq!(
                    offered,
                    [None, Some(free)],
                    "only (none) and the unrelated layer: {options:?}"
                );
            })
            .unwrap();
    }

    /// The picker's options come from the document, so a change to the stack
    /// restocks the Select. A widget left holding the old list would offer —
    /// and then write — a layer id that is no longer there.
    #[gpui::test]
    fn a_change_to_the_stack_restocks_the_parent_picker(cx: &mut TestAppContext) {
        /// Identity of the picker's Select widget: a rebuild replaces the
        /// entity, an in-place value refresh keeps it.
        fn select_id(panel: &PropertiesGpuiPanel) -> gpui::EntityId {
            panel
                .selects
                .iter()
                .find(|(key, _)| key == "parent")
                .map(|(_, binding)| binding.state.entity_id())
                .expect("the Parent picker has a Select")
        }

        let (window, project, comp_id, _lid) = setup(cx);
        let before = window
            .update(cx, |panel, window, cx| {
                panel.rebuild_widgets(window, cx);
                assert_eq!(parent_row(panel).1, [PARENT_NONE], "nothing to parent to");
                select_id(panel)
            })
            .unwrap();

        let other = add_sibling(&project, comp_id, "Parent", cx);
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, _cx| {
                let (_, options) = parent_row(panel);
                assert_eq!(options.len(), 2);
                assert_eq!(parse_parent_option(&options[1]), Some(other));
                assert_ne!(
                    select_id(panel),
                    before,
                    "the Select is rebuilt from the new option list — an in-place \
                     value refresh cannot restock it"
                );
            })
            .unwrap();
    }

    /// An enum's options are part of its field shape: they come from the
    /// document for the Parent and audio-stream pickers, and only a rebuild
    /// can restock the widget built from them.
    #[test]
    fn an_enum_field_shape_covers_its_options() {
        let picker = |options: &[&str]| PropertyField::Enum {
            key: "parent".into(),
            value: PARENT_NONE.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
        };
        assert_eq!(
            field_shape_key(&picker(&[PARENT_NONE, "2: L"])),
            field_shape_key(&picker(&[PARENT_NONE, "2: L"])),
        );
        assert_ne!(
            field_shape_key(&picker(&[PARENT_NONE])),
            field_shape_key(&picker(&[PARENT_NONE, "2: L"])),
        );
        assert_ne!(
            field_shape_key(&picker(&[PARENT_NONE, "2: L"])),
            field_shape_key(&picker(&[PARENT_NONE, "2: Renamed"])),
        );
    }

    /// Deleting the parent leaves the child unparented rather than holding a
    /// layer id the composition no longer has.
    #[gpui::test]
    fn deleting_the_parent_layer_unparents_the_child(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);
        let other = add_sibling(&project, comp_id, "Parent", cx);
        project.update(cx, |project, cx| {
            let doc =
                update_layer(project.document(), comp_id, lid, |l| l.parent = Some(other)).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::remove_layer(project.document(), comp_id, other).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });

        assert_eq!(layer(&project, comp_id, lid, cx).parent, None);
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(parent_row(panel).0, PARENT_NONE);
            })
            .unwrap();

        // Undo restores both the layer and the link it carried.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(layer(&project, comp_id, lid, cx).parent, Some(other));
    }

    // ----- inline curve editor (properties parameter-editor plan, unit 2) ---

    /// A node with two curve parameters, so expansion can be shown to be
    /// per-row rather than exclusive.
    fn curve_node() -> Node {
        Node::new(NodeId::next(), "test")
            .with_param("amount", ParameterValue::Float(1.0))
            .with_param(
                "points",
                ParameterValue::Curve(CurveParam::linear([(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)])),
            )
            .with_param(
                "shape",
                ParameterValue::Curve(CurveParam::linear([(0.0, 1.0), (1.0, 0.0)])),
            )
    }

    /// Widget size the headless curve gestures below are expressed in.
    const CURVE_TEST_SIZE: (f32, f32) = (200.0, 100.0);

    fn curve_editor_state(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Entity<ParamCurveEditorState> {
        window
            .update(cx, |panel, _window, _cx| {
                panel
                    .curves
                    .iter()
                    .find(|(k, _)| k == key)
                    .unwrap_or_else(|| panic!("{key} has no curve editor"))
                    .1
                    .state
                    .clone()
            })
            .unwrap()
    }

    /// The stored curve of a node parameter, or `None` once the node is gone.
    fn node_curve(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Option<CurveParam> {
        project.read_with(cx, |project, _| {
            resolve_network(project.document(), path)
                .and_then(|graph| graph.node(node_id))
                .and_then(|node| node.parameters.iter().find(|param| param.key == key))
                .and_then(|param| match &param.value {
                    ParameterValue::Curve(curve) => Some(curve.clone()),
                    _ => None,
                })
        })
    }

    /// Widget position of a data point inside the row's editor.
    fn curve_widget_pos(
        state: &Entity<ParamCurveEditorState>,
        x: f32,
        y: f32,
        cx: &mut TestAppContext,
    ) -> crate::widgets::curve_editor::CurvePoint {
        state.read_with(cx, |state, _| {
            crate::widgets::param_curve_editor::transform_for(state.view(), CURVE_TEST_SIZE)
                .data_to_widget(crate::widgets::curve_editor::CurvePoint::new(
                    x as f64, y as f64,
                ))
        })
    }

    /// Curve parameters reach the panel as curve rows with a curve editor
    /// bound to each, and rows expand independently — a curve is compared
    /// against its neighbours, so expansion is not an exclusive accordion.
    #[gpui::test]
    fn curve_rows_expand_independently(cx: &mut TestAppContext) {
        let (window, _editor, _project, _path, _node_id) = setup_target_for_node(cx, curve_node());

        window
            .update(cx, |panel, _window, cx| {
                let keys: Vec<&str> = panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .filter_map(|field| match field {
                        PropertyField::Curve { key, .. } => Some(key.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(keys, vec!["points", "shape"]);
                assert_eq!(panel.curves.len(), 2, "one editor per curve row");

                assert!(!panel.is_curve_expanded("points"));
                panel.toggle_curve_expanded("points", cx);
                panel.toggle_curve_expanded("shape", cx);
                assert!(panel.is_curve_expanded("points"));
                assert!(
                    panel.is_curve_expanded("shape"),
                    "expanding one row must not collapse the other"
                );

                panel.toggle_curve_expanded("points", cx);
                assert!(!panel.is_curve_expanded("points"));
                assert!(panel.is_curve_expanded("shape"));
            })
            .unwrap();
    }

    /// Expanding and collapsing is view state: it changes no value and pushes
    /// nothing onto the undo stack — the first undo still reaches the commit
    /// that added the node.
    #[gpui::test]
    fn expanding_a_curve_row_changes_no_value_and_records_no_undo_step(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let before = node_curve(&project, &path, node_id, "points", cx).expect("curve parameter");

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_curve_expanded("points", cx);
                panel.toggle_curve_expanded("shape", cx);
                panel.toggle_curve_expanded("shape", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            node_curve(&project, &path, node_id, "points", cx).as_ref(),
            Some(&before),
            "expansion must not touch the value"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            node_curve(&project, &path, node_id, "points", cx).is_none(),
            "the first undo reached the node's own commit, so no expansion \
             step was pushed in between"
        );
    }

    /// Dragging a control point applies live and commits once: one gesture,
    /// one Document undo step.
    #[gpui::test]
    fn dragging_a_curve_point_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let original = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_curve_expanded("points", cx)
            })
            .unwrap();

        let state = curve_editor_state(&window, "points", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });
        let start = curve_widget_pos(&state, 0.5, 0.5, cx);
        let mid = curve_widget_pos(&state, 0.5, 0.7, cx);
        let end = curve_widget_pos(&state, 0.5, 0.9, cx);

        state.update(cx, |state, cx| {
            state.pointer_down(start, 1, cx);
            state.drag_to(mid, cx);
        });
        cx.run_until_parked();
        let live = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        assert!(
            (live.evaluate(0.5) - 0.7).abs() < 1e-3,
            "the live drag applies to the document: {live:?}"
        );

        state.update(cx, |state, cx| {
            state.drag_to(end, cx);
            state.end_drag(cx);
        });
        cx.run_until_parked();
        let committed = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        assert!(
            (committed.evaluate(0.5) - 0.9).abs() < 1e-3,
            "{committed:?}"
        );

        // One undo for the whole gesture, and it really committed (only a
        // committed step can be redone).
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_curve(&project, &path, node_id, "points", cx).as_ref(),
            Some(&original)
        );
        // Undo restored the value without touching the view: the row the
        // gesture was made in is still open.
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.is_curve_expanded("points"));
            })
            .unwrap();
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!(
            (node_curve(&project, &path, node_id, "points", cx)
                .expect("curve")
                .evaluate(0.5)
                - 0.9)
                .abs()
                < 1e-3
        );
    }

    /// The selected point's value fields write through to the Document with
    /// the usual gesture granularity: live changes apply, the commit records
    /// one undo step.
    #[gpui::test]
    fn editing_the_selected_point_numerically_reaches_the_document(cx: &mut TestAppContext) {
        use crate::widgets::param_curve_editor::PointAxis;
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let original = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        let state = curve_editor_state(&window, "points", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });

        let pointer = curve_widget_pos(&state, 0.5, 0.5, cx);
        state.update(cx, |state, cx| {
            state.pointer_down(pointer, 1, cx);
            state.end_drag(cx);
            assert!(state.selected_point().is_some(), "the point is selected");
            state.set_selected_component(PointAxis::Output, 0.9, false, cx);
            state.set_selected_component(PointAxis::Output, 0.75, true, cx);
        });
        cx.run_until_parked();

        let edited = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        assert!((edited.evaluate(0.5) - 0.75).abs() < 1e-4, "{edited:?}");
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_curve(&project, &path, node_id, "points", cx).as_ref(),
            Some(&original),
            "one undo step for the whole edit"
        );
        project.update(cx, |project, cx| assert!(project.redo(cx)));
    }

    /// Zooming and fitting the editor's view is view state: it changes no
    /// value and records no undo step.
    #[gpui::test]
    fn changing_the_curve_view_range_never_touches_the_document(cx: &mut TestAppContext) {
        use crate::widgets::param_curve_editor::ViewPoint;
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let before = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        let state = curve_editor_state(&window, "points", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });

        state.update(cx, |state, cx| {
            state.zoom(2.0, false, ViewPoint::new(100.0, 50.0), cx);
            state.zoom(1.0, true, ViewPoint::new(100.0, 50.0), cx);
            state.fit(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            node_curve(&project, &path, node_id, "points", cx).as_ref(),
            Some(&before)
        );
        // The first undo still reaches the commit that added the node, so no
        // view change was pushed in between.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(node_curve(&project, &path, node_id, "points", cx).is_none());
    }

    /// Adding and removing a point each reach the document as their own undo
    /// step.
    #[gpui::test]
    fn adding_and_removing_curve_points_reach_the_document(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let state = curve_editor_state(&window, "shape", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });
        assert_eq!(
            node_curve(&project, &path, node_id, "shape", cx)
                .expect("curve")
                .len(),
            2
        );

        // A double-click on empty space adds a point where the pointer is.
        let empty = curve_widget_pos(&state, 0.25, 0.9, cx);
        state.update(cx, |state, cx| state.pointer_down(empty, 2, cx));
        cx.run_until_parked();
        let added = node_curve(&project, &path, node_id, "shape", cx).expect("curve");
        assert_eq!(added.len(), 3);
        assert!((added.evaluate(0.25) - 0.9).abs() < 1e-3, "{added:?}");

        // A double-click on that point removes it again.
        let point = curve_widget_pos(&state, 0.25, 0.9, cx);
        state.update(cx, |state, cx| state.pointer_down(point, 2, cx));
        cx.run_until_parked();
        assert_eq!(
            node_curve(&project, &path, node_id, "shape", cx)
                .expect("curve")
                .len(),
            2
        );

        // Two edits, two undo steps.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_curve(&project, &path, node_id, "shape", cx)
                .expect("curve")
                .len(),
            3
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_curve(&project, &path, node_id, "shape", cx)
                .expect("curve")
                .len(),
            2
        );
    }

    /// Expansion belongs to the target the panel is showing: selecting
    /// another node collapses the rows, and coming back shows them collapsed
    /// (a bare key like `points` says nothing about the node it came from).
    #[gpui::test]
    fn switching_the_target_collapses_curve_rows(cx: &mut TestAppContext) {
        let (window, _editor, _project, path, node_id) = setup_target_for_node(cx, curve_node());
        let target = PropertiesTarget::Nodes {
            network: path.clone(),
            ids: vec![node_id],
        };
        cx.update(|cx| cx.set_global(SelectedPropertiesTarget(target.clone())));
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_curve_expanded("points", cx);
                assert!(panel.is_curve_expanded("points"));
            })
            .unwrap();

        // Selecting the layer instead, then this node again.
        cx.update(|cx| cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(!panel.is_curve_expanded("points"));
            })
            .unwrap();
        cx.update(|cx| cx.set_global(SelectedPropertiesTarget(target)));
        window
            .update(cx, |panel, window, cx| {
                panel.rebuild_widgets(window, cx);
                assert!(
                    !panel.is_curve_expanded("points"),
                    "returning to the node shows the row collapsed"
                );
                assert_eq!(panel.curves.len(), 2, "the editors are rebuilt");
            })
            .unwrap();
    }

    /// Selecting the In node shows its whole interface, the shell's ports
    /// marked apart from the user's (REQ-LAYER-002).
    #[gpui::test]
    fn the_ports_section_separates_builtin_and_custom_ports(cx: &mut TestAppContext) {
        let (properties, _project, _path, _in_id) = setup_in_node_target(cx);
        assert_eq!(
            port_rows(&properties, cx),
            vec![
                (
                    net::PORT_BASE_GEOMETRY.to_string(),
                    Some(CustomPortType::Geometry),
                    true
                ),
                (
                    net::PORT_TIME.to_string(),
                    Some(CustomPortType::Float),
                    true
                ),
                ("amount".to_string(), Some(CustomPortType::Float), false),
                ("tint".to_string(), Some(CustomPortType::Color), false),
            ]
        );
    }

    /// Add → retype → reorder → remove, each driven from the Ports section
    /// and each landing as exactly one Document undo step: undoing back
    /// through the four returns the list it had before each one.
    #[gpui::test]
    fn each_port_edit_is_one_undo_step(cx: &mut TestAppContext) {
        let (properties, project, _path, _in_id) = setup_in_node_target(cx);
        let mut history = vec![port_rows(&properties, cx)];

        // Add: the trailing row's name plus the type its Select shows
        // (the first the context offers, Float at a layer root).
        let add_name = properties
            .update(cx, |panel, _window, _cx| {
                panel.port_add.as_ref().expect("an add row").name.clone()
            })
            .unwrap();
        properties
            .update(cx, |_panel, window, cx| {
                add_name.update(cx, |state, cx| state.set_value("gain", window, cx));
            })
            .unwrap();
        properties
            .update(cx, |panel, _window, cx| panel.add_port(cx))
            .unwrap();
        cx.run_until_parked();
        history.push(port_rows(&properties, cx));
        assert_eq!(
            history.last().unwrap().last(),
            Some(&("gain".to_string(), Some(CustomPortType::Float), false))
        );

        // Retype: the Select hands back its translated label, which the
        // panel maps back to the type.
        properties
            .update(cx, |panel, _window, cx| {
                panel.retype_port("gain", &port_type_label(Some(CustomPortType::Vec2)), cx);
            })
            .unwrap();
        cx.run_until_parked();
        history.push(port_rows(&properties, cx));
        assert_eq!(
            history.last().unwrap().last(),
            Some(&("gain".to_string(), Some(CustomPortType::Vec2), false))
        );

        // Reorder: one slot earlier, past `tint` but never past the shell's.
        properties
            .update(cx, |panel, _window, cx| panel.move_port("gain", -1, cx))
            .unwrap();
        cx.run_until_parked();
        history.push(port_rows(&properties, cx));
        assert_eq!(
            history
                .last()
                .unwrap()
                .iter()
                .map(|(name, _, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec![
                net::PORT_BASE_GEOMETRY,
                net::PORT_TIME,
                "amount",
                "gain",
                "tint"
            ]
        );

        // Remove.
        properties
            .update(cx, |panel, _window, cx| panel.remove_port("gain", cx))
            .unwrap();
        cx.run_until_parked();
        history.push(port_rows(&properties, cx));
        assert!(
            history
                .last()
                .unwrap()
                .iter()
                .all(|(name, _, _)| name != "gain")
        );

        while history.len() > 1 {
            let expected = history[history.len() - 2].clone();
            project.update(cx, |project, cx| assert!(project.undo(cx)));
            assert_eq!(
                port_rows(&properties, cx),
                expected,
                "edit {} is a single undo step",
                history.len() - 1
            );
            history.pop();
        }
    }

    /// A refused edit says why. Dropping it silently would look like the
    /// panel ignored the user, and the graph never changes either way.
    #[gpui::test]
    fn a_refused_port_edit_shows_its_reason(cx: &mut TestAppContext) {
        let (properties, _project, _path, _in_id) = setup_in_node_target(cx);
        let before = port_rows(&properties, cx);

        // A name another port already holds — the core refuses it.
        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_port("tint", "amount".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let message = properties
            .update(cx, |panel, _window, _cx| panel.port_error.clone())
            .unwrap();
        assert_eq!(
            message.as_deref(),
            Some(ravel_i18n::translate("properties.ports.error.duplicate").as_str())
        );
        assert_eq!(port_rows(&properties, cx), before, "and nothing moved");

        // An empty name never reaches the graph: the panel refuses it itself,
        // because the core would report it as a duplicate of nothing.
        properties
            .update(cx, |panel, _window, cx| panel.add_port(cx))
            .unwrap();
        cx.run_until_parked();
        let message = properties
            .update(cx, |panel, _window, _cx| panel.port_error.clone())
            .unwrap();
        assert_eq!(
            message.as_deref(),
            Some(ravel_i18n::translate("properties.ports.error.empty_name").as_str())
        );
        assert_eq!(port_rows(&properties, cx), before);
    }

    /// A row's name Input reports Enter *and* the blur that follows it. The
    /// second report carries the same pair — the old name is baked into the
    /// subscription and the Input still holds the new text — so it must not
    /// reach the graph: the port is already renamed, and the `PortNotFound`
    /// coming back would put a failure under an edit that worked.
    #[gpui::test]
    fn a_renames_own_blur_does_not_commit_it_twice(cx: &mut TestAppContext) {
        let (properties, project, _path, _in_id) = setup_in_node_target(cx);

        // Enter, then the blur of the same widget, before any rebuild.
        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_port("amount", "gain".into(), cx);
                panel.rename_port("amount", "gain".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            properties
                .update(cx, |panel, _window, _cx| panel.port_error.clone())
                .unwrap(),
            None,
            "the rename succeeded, so nothing is reported"
        );
        let rows = port_rows(&properties, cx);
        assert!(rows.iter().any(|(name, _, _)| name == "gain"));
        assert!(rows.iter().all(|(name, _, _)| name != "amount"));

        // And it was one edit: a single undo puts the old name back.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let rows = port_rows(&properties, cx);
        assert!(
            rows.iter().any(|(name, _, _)| name == "amount"),
            "one undo is enough, so only one rename was committed"
        );
    }

    /// A refused rename can be retried immediately under another name: the
    /// repeat guard keys on the pair, not on the row.
    #[gpui::test]
    fn a_refused_rename_can_be_retried_with_another_name(cx: &mut TestAppContext) {
        let (properties, _project, _path, _in_id) = setup_in_node_target(cx);

        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_port("tint", "amount".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            properties
                .update(cx, |panel, _window, _cx| panel.port_error.is_some())
                .unwrap()
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_port("tint", "shade".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            port_rows(&properties, cx)
                .iter()
                .any(|(name, _, _)| name == "shade")
        );
    }

    /// Re-picking the type a row already has is not an edit. The Select emits
    /// `Confirm` for the entry that is already selected, and committing the
    /// unchanged graph would leave an undo step that undoes to an identical
    /// document.
    #[gpui::test]
    fn retyping_a_row_to_its_current_type_records_nothing(cx: &mut TestAppContext) {
        let (properties, project, _path, _in_id) = setup_in_node_target(cx);
        let before = port_rows(&properties, cx);

        properties
            .update(cx, |panel, _window, cx| {
                panel.retype_port("amount", &port_type_label(Some(CustomPortType::Float)), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(port_rows(&properties, cx), before);

        // The undo stack is untouched: the next undo reaches the layer the
        // fixture committed, not a no-op port edit in front of it.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            properties
                .update(cx, |panel, _window, cx| {
                    panel.refresh_values(cx);
                    panel.sections.is_empty()
                })
                .unwrap(),
            "undo removed the layer itself, so no port edit was stacked on top"
        );
    }

    /// The shell's ports get no widgets: `is_fixed_port` refuses every edit
    /// to one, so offering an Input or a Select would promise something the
    /// core then rejects.
    #[gpui::test]
    fn builtin_port_rows_carry_no_editors(cx: &mut TestAppContext) {
        let (properties, _project, _path, _in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, _cx| {
                let named: Vec<&str> = panel.port_names.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(named, vec!["amount", "tint"]);
                let typed: Vec<&str> = panel.port_types.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(typed, vec!["amount", "tint"]);
            })
            .unwrap();
    }

    /// A type without a locale description gets no description field — same
    /// rule as the popover, which skips the section. The positive direction
    /// (a type with a description) needs a real catalog and lives in the
    /// `node_hover_popover` integration test, because initializing the
    /// global i18n store here would leak into every other test of this
    /// binary (they run with an empty store).
    #[test]
    fn node_info_section_omits_the_description_when_the_type_has_none() {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let node = Node::new(NodeId::new(1), "plugin.unknown");
        let eval = EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        let mut sections = sections_for_node(
            &node,
            &registry,
            0,
            &eval,
            &[],
            ravel_core::network::NetworkContext::LayerRoot,
        );
        let fields_before = sections[0].fields.len();
        append_node_description(&mut sections, &node.type_key);
        assert_eq!(sections[0].fields.len(), fields_before);
    }

    // ----- Exposed parameter declarations (REQ-PROJ-006, EXPO-5) -----------

    /// The project's declarations, as the panel's Project target renders them.
    fn declaration_rows(
        properties: &gpui::WindowHandle<PropertiesGpuiPanel>,
        cx: &mut TestAppContext,
    ) -> Vec<ExposedRow> {
        properties
            .update(cx, |panel, window, cx| {
                let restore = panel.target.clone();
                panel.target = PropertiesTarget::Project;
                panel.refresh_values(cx);
                panel.rebuild_widgets(window, cx);
                let rows = panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .find_map(|field| match field {
                        PropertyField::ExposedList { rows, .. } => Some(rows.clone()),
                        _ => None,
                    })
                    .expect("the Project target has a declarations section");
                panel.target = restore;
                panel.refresh_values(cx);
                panel.rebuild_widgets(window, cx);
                rows
            })
            .unwrap()
    }

    // ---- expression rows --------------------------------------------------

    fn node_param(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        key: &str,
        cx: &mut TestAppContext,
    ) -> ParameterValue {
        project.read_with(cx, |project, _| {
            resolve_network(project.document(), path)
                .expect("network")
                .node(node_id)
                .expect("node")
                .parameters
                .iter()
                .find(|p| p.key == key)
                .expect("parameter")
                .value
                .clone()
        })
    }

    fn expression_row(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> ExpressionRow {
        window
            .read_with(cx, |panel, _| {
                panel
                    .expressions
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, row)| row.clone())
                    .expect("an expression row for the parameter")
            })
            .unwrap()
    }

    fn declaration_names(
        properties: &gpui::WindowHandle<PropertiesGpuiPanel>,
        cx: &mut TestAppContext,
    ) -> Vec<String> {
        declaration_rows(properties, cx)
            .into_iter()
            .map(|row| row.name)
            .collect()
    }

    /// `CommandId::ProjectExposedParameters` sets the target and *then* opens
    /// Properties, so a panel that has to be created must read the selection
    /// that is already there — the observer only sees later writes.
    #[gpui::test]
    fn a_panel_opened_after_the_selection_shows_it(cx: &mut TestAppContext) {
        let (_properties, project, _path, in_id) = setup_in_node_target(cx);
        cx.update(|cx| {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Project));
        });
        project.update(cx, |project, cx| {
            let declaration = ExposedParameter::inferred(
                "amount",
                ravel_core::exposed::ExposedValue::Float(1.0),
                ExposedBinding::new(in_id, "amount"),
            )
            .expect("a float defaults to a float");
            let document = project.document().clone().with_exposed_parameters(
                ExposedParameters::from_declarations([declaration]).expect("one name"),
            );
            project.commit_document(document, InvalidationHint::None, cx);
        });
        cx.run_until_parked();

        let opened = cx.add_window(|window, cx| {
            PropertiesGpuiPanel::new(ravel_ui::layout::PanelInstanceId(1), window, cx)
        });
        cx.run_until_parked();
        // Deliberately no `rebuild_widgets` call: the panel adopted the
        // standing selection in `new` *and* marked itself as owing a build, so
        // its first render already produced the sections. Building them here
        // would let the test pass with either half of the fix reverted.
        let rows = opened
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.target, PropertiesTarget::Project);
                panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .find_map(|field| match field {
                        PropertyField::ExposedList { rows, .. } => Some(rows.clone()),
                        _ => None,
                    })
            })
            .unwrap();
        let rows = rows.expect("the new panel opens on the declarations, not the empty state");
        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            ["amount"]
        );
    }

    /// The toggle has to appear on the parameter rows of a selected node, or
    /// there is no way to expose anything: this is what the on-device check
    /// looks at, and what a missing one would show as a row with no □.
    #[gpui::test]
    fn every_declarable_parameter_row_offers_the_toggle(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        let states = properties
            .update(cx, |panel, _window, cx| {
                let sections = panel.sections.clone();
                panel.exposed_states(&sections, cx)
            })
            .unwrap();
        let mut keys: Vec<&str> = states.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["amount", "tint"],
            "the In node's two parameters are declarable; its read-only info fields are not"
        );
        assert!(
            states.values().all(|declared| !declared),
            "nothing is declared yet"
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        let states = properties
            .update(cx, |panel, _window, cx| {
                let sections = panel.sections.clone();
                panel.exposed_states(&sections, cx)
            })
            .unwrap();
        assert_eq!(
            states.get("amount"),
            Some(&true),
            "the row shows it declared"
        );
        assert_eq!(states.get("tint"), Some(&false));
    }

    /// Exposing a keyframed parameter takes its value **at the playhead** —
    /// the number the panel is showing — not a `0.0` chosen by nothing.
    ///
    /// The default is what a caller gets when they omit `--param`, so seeding
    /// it with a placeholder puts a value in the contract that no part of the
    /// document ever chose. That the animated components will not take a
    /// caller's value is a separate thing, reported on the row.
    #[gpui::test]
    fn exposing_an_animated_parameter_seeds_its_value_at_the_playhead(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let (properties, project, path, in_id) = setup_in_node_target(cx);

        // `tint`'s red channel runs 0.0 at frame 0 to 1.0 at frame 100.
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(100, 1.0, Interpolation::Linear);
        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::update_layer(
                project.document(),
                path.comp,
                path.layer,
                |layer| {
                    layer.network = layer
                        .network
                        .clone()
                        .set_params(
                            in_id,
                            &[ravel_core::graph::Parameter {
                                key: "tint".into(),
                                value: ParameterValue::Channel4([
                                    AnimationChannel::new(ChannelSource::Keyframes(curve.clone())),
                                    AnimationChannel::constant(1.0),
                                    AnimationChannel::constant(1.0),
                                    AnimationChannel::constant(1.0),
                                ]),
                            }],
                        )
                        .expect("the In node has a tint parameter");
                },
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        cx.update(|cx| {
            cx.set_global(crate::panels::PlaybackPosition {
                frame: 50,
                fps: ravel_core::types::FrameRate::new(30, 1),
            });
        });
        cx.run_until_parked();

        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "tint", cx);
            })
            .unwrap();
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            let declaration = project
                .document()
                .exposed_parameters
                .get("tint")
                .expect("the declaration is in the document");
            let ravel_core::exposed::ExposedValue::Color(color) = declaration.default_value()
            else {
                panic!("a four-channel parameter declares a colour");
            };
            assert!(
                (color.r - 0.5).abs() < 1e-3,
                "the red channel is seeded at the playhead (expected 0.5, got {})",
                color.r
            );
            assert_eq!((color.g, color.b, color.a), (1.0, 1.0, 1.0));
        });
    }

    /// Exposing takes the parameter's own type and value, so the declaration
    /// binds back to the parameter it came from with no further input.
    #[gpui::test]
    fn exposing_a_parameter_declares_it_with_the_parameters_own_value(cx: &mut TestAppContext) {
        let (properties, project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();

        let rows = declaration_rows(&properties, cx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "amount");
        assert_eq!(rows[0].value_type, "float");
        assert_eq!(rows[0].default, "1");
        assert_eq!(rows[0].issue, None, "the declaration reaches its parameter");

        project.read_with(cx, |project, _| {
            let declaration = project
                .document()
                .exposed_parameters
                .get("amount")
                .expect("the declaration is in the document");
            assert_eq!(declaration.binding(), &ExposedBinding::new(in_id, "amount"));
        });
    }

    /// A parameter with no place in an external contract gets the core's
    /// reason, not a declaration that would then report itself broken.
    #[gpui::test]
    fn a_parameter_that_cannot_be_a_contract_is_refused(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "not a parameter", cx);
                assert_eq!(
                    panel.exposed_error.as_deref(),
                    Some(ravel_i18n::translate("properties.exposed.error.not_exposable").as_str())
                );
            })
            .unwrap();
        assert!(declaration_names(&properties, cx).is_empty());
    }

    #[gpui::test]
    fn exposing_an_already_exposed_parameter_says_so(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
                assert_eq!(
                    panel.exposed_error.as_deref(),
                    Some(
                        ravel_i18n::translate("properties.exposed.error.already_exposed").as_str()
                    )
                );
            })
            .unwrap();
        assert_eq!(declaration_names(&properties, cx), ["amount"]);
    }

    /// EXPO-5's refusal case: the name is the contract, so two declarations may
    /// not answer to one name, and the user has to see why.
    #[gpui::test]
    fn renaming_a_declaration_onto_an_existing_name_is_refused(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
                panel.expose_parameter(in_id, "tint", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_names(&properties, cx), ["amount", "tint"]);

        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "tint".into(), cx);
                assert_eq!(
                    panel.exposed_error.as_deref(),
                    Some(ravel_i18n::translate("properties.exposed.error.duplicate").as_str())
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            declaration_names(&properties, cx),
            ["amount", "tint"],
            "a refused rename changes nothing"
        );

        // And the row can be retried under a free name straight away.
        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "gain".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_names(&properties, cx), ["gain", "tint"]);
    }

    /// The name Input reports Enter *and* the blur that follows it, carrying
    /// the same pair; the second must not reach the document.
    #[gpui::test]
    fn a_declaration_renames_own_blur_does_not_commit_it_twice(cx: &mut TestAppContext) {
        let (properties, project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        let revision = project.read_with(cx, |project, _| project.document().clone());

        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "gain".into(), cx);
                panel.rename_declaration("amount", "gain".into(), cx);
                assert_eq!(
                    panel.exposed_error, None,
                    "the repeat is dropped, not failed"
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_names(&properties, cx), ["gain"]);

        // One undo puts the old name back: the repeat recorded no second step.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(declaration_names(&properties, cx), ["amount"]);
        project.read_with(cx, |project, _| {
            assert_eq!(
                project.document().exposed_parameters,
                revision.exposed_parameters
            );
        });
    }

    /// Every declaration edit is exactly one undo step, the same contract the
    /// Ports section holds to.
    #[gpui::test]
    fn each_declaration_edit_is_one_undo_step(cx: &mut TestAppContext) {
        let (properties, project, _path, in_id) = setup_in_node_target(cx);
        let mut history = vec![declaration_rows(&properties, cx)];

        for key in ["amount", "tint"] {
            properties
                .update(cx, |panel, _window, cx| {
                    panel.expose_parameter(in_id, key, cx);
                })
                .unwrap();
            cx.run_until_parked();
            history.push(declaration_rows(&properties, cx));
        }
        assert_eq!(
            history
                .last()
                .unwrap()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["amount", "tint"]
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "gain".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        history.push(declaration_rows(&properties, cx));

        properties
            .update(cx, |panel, _window, cx| {
                panel.describe_declaration("gain", "How much".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        history.push(declaration_rows(&properties, cx));
        assert_eq!(history.last().unwrap()[0].description, "How much");

        properties
            .update(cx, |panel, _window, cx| {
                panel.move_declaration("gain", 1, cx);
            })
            .unwrap();
        cx.run_until_parked();
        history.push(declaration_rows(&properties, cx));
        assert_eq!(
            history
                .last()
                .unwrap()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["tint", "gain"]
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.remove_declaration("gain", cx);
            })
            .unwrap();
        cx.run_until_parked();
        history.push(declaration_rows(&properties, cx));
        assert_eq!(
            history
                .last()
                .unwrap()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["tint"]
        );

        while history.len() > 1 {
            let expected = history[history.len() - 2].clone();
            project.update(cx, |project, cx| assert!(project.undo(cx)));
            assert_eq!(
                declaration_rows(&properties, cx),
                expected,
                "edit {} is a single undo step",
                history.len() - 1
            );
            history.pop();
        }
    }

    /// A no-op is not an edit: it must not leave an undo step that undoes to
    /// an identical document.
    #[gpui::test]
    fn a_declaration_edit_that_changes_nothing_records_nothing(cx: &mut TestAppContext) {
        let (properties, project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        let before = project.read_with(cx, |project, _| project.document().clone());

        properties
            .update(cx, |panel, _window, cx| {
                // Up on the only row, the description it already has, and a
                // removal of a declaration that is not there.
                panel.move_declaration("amount", -1, cx);
                panel.describe_declaration("amount", String::new(), cx);
                panel.remove_declaration("absent", cx);
            })
            .unwrap();
        cx.run_until_parked();

        project.update(cx, |project, cx| {
            // The single step still on the stack is the expose.
            assert!(project.undo(cx));
        });
        project.read_with(cx, |project, _| {
            assert!(
                project.document().exposed_parameters.is_empty(),
                "the no-ops recorded nothing, so one undo removed the declaration"
            );
        });
        assert!(!before.exposed_parameters.is_empty());
    }

    /// The panel does not decide that a declaration is broken — it shows the
    /// reason `resolve` gives, in the user's language.
    #[gpui::test]
    fn an_unresolved_declaration_shows_the_cores_reason(cx: &mut TestAppContext) {
        let (properties, project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_rows(&properties, cx)[0].issue, None);

        // Rebind the declaration at a node that is not there. Doing it through
        // the document rather than the panel is the point: whatever leaves a
        // binding dangling, the row reports it.
        project.update(cx, |project, cx| {
            let declaration = project
                .document()
                .exposed_parameters
                .get("amount")
                .expect("declared")
                .clone()
                .with_binding(ExposedBinding::new(NodeId::next(), "amount"));
            let declarations =
                ExposedParameters::from_declarations([declaration]).expect("one name");
            let document = project
                .document()
                .clone()
                .with_exposed_parameters(declarations);
            project.commit_document(document, InvalidationHint::None, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            declaration_rows(&properties, cx)[0].issue,
            Some(ravel_ui::properties::exposed::ISSUE_NODE_MISSING)
        );
    }

    /// The contract the whole mechanism exists for: a declaration the UI made
    /// is a declaration the headless CLI path reads and applies. If the panel
    /// ever mints one its own way, this is what catches it.
    #[gpui::test]
    fn a_declaration_made_in_the_panel_is_one_the_headless_path_applies(cx: &mut TestAppContext) {
        use ravel_core::exposed::ExposedValue;
        use ravel_core::exposed::apply::{AssetContext, apply};
        use ravel_core::exposed::listing::ExposedListing;

        let (properties, project, path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.expose_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "gain".into(), cx);
                panel.describe_declaration("gain", "How much".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();

        let document = project.read_with(cx, |project, _| project.document().clone());

        // The listing a CLI reads: name, type, default, description, resolved.
        let listing = ExposedListing::of(&document);
        assert_eq!(listing.parameters.len(), 1);
        let entry = &listing.parameters[0];
        assert_eq!(entry.name, "gain");
        assert_eq!(entry.value_type, ravel_core::exposed::ExposedType::Float);
        assert_eq!(entry.default, ExposedValue::Float(1.0));
        assert_eq!(entry.description, "How much");
        assert!(
            entry.resolved,
            "the panel's declaration reaches its parameter"
        );

        // And the value a caller supplies reaches the parameter.
        let applied = apply(
            document,
            &[("gain".to_string(), ExposedValue::Float(4.0))]
                .into_iter()
                .collect(),
            AssetContext::default(),
        )
        .expect("the declared type accepts a float");
        assert!(applied.issues.is_empty());
        let graph = resolve_network(&applied.document, &path).expect("network");
        let value = graph
            .node(in_id)
            .expect("the In node")
            .parameters
            .iter()
            .find(|parameter| parameter.key == "amount")
            .expect("the bound parameter")
            .value
            .clone();
        assert_eq!(value, ParameterValue::Float(4.0));
    }

    fn ramp_curve() -> ravel_core::animation::curve::KeyframeCurve {
        use ravel_core::animation::interpolation::Interpolation;
        let mut curve = ravel_core::animation::curve::KeyframeCurve::with_default(0.0);
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        curve
    }

    fn node_with_amount(source: ChannelSource) -> Node {
        Node::new(NodeId::next(), "test").with_param(
            "amount",
            ParameterValue::Channel(AnimationChannel::new(source)),
        )
    }

    /// `Blend(Keyframes, Expression)` is a state the core supports (EXPR-2).
    /// A badge that only matched the top of the source read it as undriven,
    /// and the click that followed replaced the whole blend with a literal.
    #[gpui::test]
    fn a_blend_holding_an_expression_lights_the_badge(cx: &mut TestAppContext) {
        let blend = ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp_curve())),
            Box::new(ChannelSource::Expression(ParameterExpression::new(
                "frame * 4",
            ))),
            ravel_core::animation::blend::BlendMode::Mix,
            0.5,
        );
        let (window, _editor, _project, _path, _node_id) =
            setup_target_for_node(cx, node_with_amount(blend));

        let row = expression_row(&window, "amount", cx);
        assert!(
            row.is_attached(),
            "the nested expression must light the badge"
        );
        assert_eq!(
            row.components[0].as_ref().map(|c| c.source.as_str()),
            Some("frame * 4"),
            "and the editor must show the nested source"
        );
        // Attaching would have to overwrite the blend, so the badge is dead in
        // that direction; the click detaches instead.
        assert!(!row.attachable);
    }

    /// The toggle on a blend detaches, and detaching freezes the expression
    /// where it sits. Replacing the blend with a literal would delete the
    /// curve the author blended with.
    #[gpui::test]
    fn toggling_a_blend_freezes_the_expression_and_keeps_the_blend(cx: &mut TestAppContext) {
        let blend = ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp_curve())),
            Box::new(ChannelSource::Expression(ParameterExpression::new(
                "frame * 4",
            ))),
            ravel_core::animation::blend::BlendMode::Mix,
            0.5,
        );
        let (_window, editor, project, path, node_id) =
            setup_target_for_node(cx, node_with_amount(blend));

        editor
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_expression(node_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();

        let ParameterValue::Channel(channel) = node_param(&project, &path, node_id, "amount", cx)
        else {
            panic!("expected a channel");
        };
        let ChannelSource::Blend(a, b, mode, factor) = &channel.source else {
            panic!("the blend must survive the toggle, not be replaced wholesale");
        };
        assert!(
            matches!(**a, ChannelSource::Keyframes(_)),
            "the keyframes the author blended with must survive"
        );
        assert!(matches!(**b, ChannelSource::Constant(v) if v == 0.0));
        assert_eq!(*mode, ravel_core::animation::blend::BlendMode::Mix);
        assert_eq!(*factor, 0.5);
    }

    /// Attaching over a keyframe curve would destroy the animation and leave
    /// the "return to a constant or keyframes" operation nothing to return to.
    /// The badge is drawn dead and the click changes nothing.
    #[gpui::test]
    fn a_keyframed_parameter_refuses_to_attach_an_expression(cx: &mut TestAppContext) {
        let (window, editor, project, path, node_id) =
            setup_target_for_node(cx, node_with_amount(ChannelSource::Keyframes(ramp_curve())));

        let row = expression_row(&window, "amount", cx);
        assert!(!row.is_attached());
        assert!(
            !row.attachable,
            "the badge must be dead, not silently inert"
        );

        let before = node_param(&project, &path, node_id, "amount", cx);
        editor
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_expression(node_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            node_param(&project, &path, node_id, "amount", cx),
            before,
            "the curve must be untouched"
        );
    }

    // ---- editing an expression --------------------------------------------

    /// Type into one component's box the way the author does: the text lands
    /// in the widget and the `Change` handler records it as a draft. Neither
    /// touches the document.
    fn type_expression(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        component: usize,
        text: &str,
        cx: &mut TestAppContext,
    ) {
        window
            .update(cx, |panel, window, cx| {
                let state = panel
                    .expression_inputs
                    .iter()
                    .find(|(k, index, _)| k == key && *index == component)
                    .map(|(_, _, binding)| binding.state.clone())
                    .expect("an input for the driven component");
                state.update(cx, |state, cx| state.set_value(text, window, cx));
                panel.note_expression_draft(key, component, text.to_string(), cx);
            })
            .unwrap();
    }

    fn input_text(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        component: usize,
        cx: &mut TestAppContext,
    ) -> String {
        window
            .read_with(cx, |panel, cx| {
                panel
                    .expression_inputs
                    .iter()
                    .find(|(k, index, _)| k == key && *index == component)
                    .map(|(_, _, binding)| binding.state.read(cx).value().to_string())
                    .expect("an input for the driven component")
            })
            .unwrap()
    }

    fn expression_node() -> Node {
        node_with_amount(ChannelSource::Expression(ParameterExpression::new("1")))
    }

    fn committed_source(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        cx: &mut TestAppContext,
    ) -> String {
        let value = node_param(project, path, node_id, "amount", cx);
        expression::component_expression(&value, 0)
            .expect("a driven component")
            .source()
            .to_string()
    }

    /// The completion criterion EXPR-4 states: a syntax error is shown *while
    /// editing*, and showing it does not block the edit. Waiting for blur is
    /// not "while editing", and committing every keystroke to get there would
    /// fill the undo history with half-typed sources.
    #[gpui::test]
    fn a_syntax_error_shows_while_typing_without_reaching_the_document(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());

        type_expression(&window, "amount", 0, "frame *", cx);

        let error = window
            .read_with(cx, |panel, _| {
                panel
                    .expression_draft("amount", 0)
                    .expect("the keystroke is held as a draft")
                    .error
                    .clone()
            })
            .unwrap();
        assert!(error.is_some(), "the error must be visible while typing");
        assert_eq!(
            committed_source(&project, &path, node_id, cx),
            "1",
            "and the document must not have moved"
        );
    }

    /// Blur commits the draft — including one that does not compile, which is
    /// stored rather than refused so the author can stop mid-expression.
    #[gpui::test]
    fn blur_commits_the_draft_and_clears_it(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());

        type_expression(&window, "amount", 0, "frame *", cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.commit_expression_draft("amount", 0, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(committed_source(&project, &path, node_id, cx), "frame *");
        assert!(
            window
                .read_with(cx, |panel, _| panel.expression_draft("amount", 0).is_none())
                .unwrap(),
            "the draft is spent once committed"
        );
    }

    /// The regression this mechanism exists for. An undo restores the previous
    /// source, but the box still shows the text that was undone; a blur that
    /// wrote it back would silently reverse the undo. The box is resynced and
    /// the blur, having no draft, writes nothing.
    #[gpui::test]
    fn undo_resyncs_the_box_and_the_following_blur_does_not_recommit(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());

        type_expression(&window, "amount", 0, "frame * 2", cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.commit_expression_draft("amount", 0, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(committed_source(&project, &path, node_id, cx), "frame * 2");

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(committed_source(&project, &path, node_id, cx), "1");

        // The render-time sync is what carries the undo into the widget.
        window
            .update(cx, |panel, window, cx| {
                panel.sync_expression_widgets(window, cx);
            })
            .unwrap();
        assert_eq!(input_text(&window, "amount", 0, cx), "1");

        // Blurring the box afterwards must not resurrect the undone source.
        window
            .update(cx, |panel, _window, cx| {
                panel.commit_expression_draft("amount", 0, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(committed_source(&project, &path, node_id, cx), "1");
    }

    /// Commit is gated on there being a draft, not on what the box holds.
    ///
    /// The resync normally keeps the two together, but the gate is what makes
    /// a blur harmless in its own right: a box whose text disagrees with the
    /// document — for any reason, including a refresh the widget has not
    /// caught up with — writes nothing unless the author actually typed into
    /// it. Committing the widget's text instead is what let a blur after an
    /// undo put the undone source straight back.
    #[gpui::test]
    fn a_blur_commits_nothing_when_the_author_typed_nothing(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());

        // Text in the box, deliberately no draft: the author did not type it.
        window
            .update(cx, |panel, window, cx| {
                let state = panel
                    .expression_inputs
                    .iter()
                    .find(|(k, index, _)| k == "amount" && *index == 0)
                    .map(|(_, _, binding)| binding.state.clone())
                    .expect("an input");
                state.update(cx, |state, cx| state.set_value("frame * 2", window, cx));
            })
            .unwrap();
        assert_eq!(input_text(&window, "amount", 0, cx), "frame * 2");

        window
            .update(cx, |panel, _window, cx| {
                panel.commit_expression_draft("amount", 0, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            committed_source(&project, &path, node_id, cx),
            "1",
            "a blur with nothing typed must leave the document alone"
        );
    }

    /// A sync must never overwrite text the author is still typing, even
    /// though an undo landed while they typed.
    #[gpui::test]
    fn a_draft_outranks_the_document_until_it_is_committed(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());

        type_expression(&window, "amount", 0, "sin(tim", cx);
        window
            .update(cx, |panel, window, cx| {
                panel.sync_expression_widgets(window, cx);
            })
            .unwrap();

        assert_eq!(input_text(&window, "amount", 0, cx), "sin(tim");
        assert_eq!(committed_source(&project, &path, node_id, cx), "1");
    }

    /// Widgets are rebuilt whenever the row shape changes. A rebuild that
    /// dropped the draft would throw away the half-typed source.
    #[gpui::test]
    fn a_draft_survives_a_widget_rebuild(cx: &mut TestAppContext) {
        let (window, _editor, _project, _path, _node_id) =
            setup_target_for_node(cx, expression_node());

        type_expression(&window, "amount", 0, "frame *", cx);
        window
            .update(cx, |panel, window, cx| panel.rebuild_widgets(window, cx))
            .unwrap();

        assert_eq!(input_text(&window, "amount", 0, cx), "frame *");
        assert!(
            window
                .read_with(cx, |panel, _| panel.expression_draft("amount", 0).is_some())
                .unwrap()
        );
    }

    /// Editing one component of a vector must leave its neighbours' sources
    /// exactly as they are.
    #[gpui::test]
    fn editing_one_component_of_a_vector_leaves_the_others(cx: &mut TestAppContext) {
        let source = |text: &str| {
            AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new(text)))
        };
        let node = Node::new(NodeId::next(), "test").with_param(
            "offset",
            ParameterValue::Channel3([source("1"), source("2"), source("3")]),
        );
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, node);

        type_expression(&window, "offset", 1, "frame * 4", cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.commit_expression_draft("offset", 1, &[node_id], cx);
            })
            .unwrap();
        cx.run_until_parked();

        let value = node_param(&project, &path, node_id, "offset", cx);
        let source_of = |component: usize| {
            expression::component_expression(&value, component)
                .expect("driven")
                .source()
                .to_string()
        };
        assert_eq!(source_of(0), "1");
        assert_eq!(source_of(1), "frame * 4");
        assert_eq!(source_of(2), "3");
    }

    /// Per-component attach and detach on a vector: the neighbours keep what
    /// they hold, driven or not.
    #[gpui::test]
    fn attaching_a_vector_leaves_a_keyframed_component_alone(cx: &mut TestAppContext) {
        let node = Node::new(NodeId::next(), "test").with_param(
            "offset",
            ParameterValue::Channel2([
                AnimationChannel::keyframes(ramp_curve()),
                AnimationChannel::constant(3.0),
            ]),
        );
        let (window, editor, project, path, node_id) = setup_target_for_node(cx, node);

        assert!(expression_row(&window, "offset", cx).attachable);
        editor
            .update(cx, |panel, _window, cx| {
                panel.toggle_param_expression(node_id, "offset", cx);
            })
            .unwrap();
        cx.run_until_parked();

        let ParameterValue::Channel2(channels) = node_param(&project, &path, node_id, "offset", cx)
        else {
            panic!("expected a channel pair");
        };
        assert!(
            matches!(channels[0].source, ChannelSource::Keyframes(_)),
            "the keyframed component keeps its curve"
        );
        let ChannelSource::Expression(expression) = &channels[1].source else {
            panic!("the constant component gains an expression");
        };
        assert_eq!(expression.source(), "3");
    }
}

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
use ravel_core::color::ColorSpace;
use ravel_core::composition::{AssetMetadata, Document, Layer};
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
use ravel_ui::keyframes::{PropertyRowId, layer_local_frame};
use ravel_ui::panels::timeline::PropertyGroup;
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
    ParamCurveEditor, ParamCurveEditorState, ParamCurveEvent, ParamRampEditor,
    ParamRampEditorState, ParamRampEvent, ScrubEvent, ScrubInput, ScrubInputState, curve_thumbnail,
    ramp_thumbnail,
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
    // The Properties group this port's parameter sits under (PGRP-4). Absent
    // for a port with no parameter, which has nothing to group.
    if let Some((_, input)) = ports.groups.iter().find(|(n, _)| n == &row.name) {
        fields = fields.child(
            div()
                .flex_shrink_0()
                .w(px(88.0))
                .child(Input::new(input).xsmall().w_full()),
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
/// or withdraws the declaration that already does.
///
/// Sibling of [`port_toggle_button`] and deliberately a different affordance:
/// a *port* makes a parameter drivable from inside the graph, a *declaration*
/// makes it settable from outside the project. They are independent, so a
/// parameter can carry both.
///
/// `declared` only picks the icon and the tooltip. The click reads the
/// document again ([`PropertiesGpuiPanel::toggle_exposed_parameter`]) so a
/// flag rendered one frame ago cannot decide which half runs.
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
        .tooltip(move |window, cx| {
            let text = if declared {
                t!("properties.toggle.exposed_remove")
            } else {
                t!("properties.toggle.exposed")
            };
            Tooltip::new(text).build(window, cx)
        })
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            let key = key.clone();
            panel
                .update(cx, move |this, cx| {
                    this.toggle_exposed_parameter(node_id, &key, cx);
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

/// Default height of an expanded inline editor (curve or ramp), and the
/// bounds the resize drag keeps it between. The minimum leaves room for the
/// editor's own toolbar (the selected point or stop, the interpolation
/// buttons, the view range) plus a usable graph or band above it.
const INLINE_EDITOR_HEIGHT: f32 = 200.0;
const INLINE_EDITOR_MIN_HEIGHT: f32 = 120.0;
const INLINE_EDITOR_MAX_HEIGHT: f32 = 560.0;
/// Height of the grab strip under an expanded inline editor.
const INLINE_RESIZE_HANDLE_HEIGHT: f32 = 6.0;

/// Drag payload for an inline editor's height handle, identified by the row's
/// field key.
#[derive(Clone)]
struct DragRowHeight(SharedString);

impl Render for DragRowHeight {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// An in-flight inline-editor height drag: the row being resized, the pointer
/// y it started at, and the height it had then.
struct RowResize {
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
                        this.toggle_row_expanded(&field_key, cx);
                    })
                    .ok();
            }),
    )
}

/// The collapsed ramp row: label plus a gradient band of the ramp. Clicking
/// anywhere on the row toggles the inline editor underneath it — the same
/// panel view state a curve row toggles, so a ramp and a curve can be open at
/// once and neither closes the other.
fn ramp_row(
    key: &str,
    ramp: &ravel_core::param_ramp::RampParam,
    expanded: bool,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
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
            .id(SharedString::from(format!("ramp-row-{key}")))
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
                            .rounded(px(2.0))
                            .overflow_hidden()
                            .child(ramp_thumbnail(ramp.clone())),
                    )
                    .child(Icon::new(icon).size_3().text_color(muted)),
            )
            .tooltip(|window, cx| Tooltip::new(t!("properties.ramp.expand")).build(window, cx))
            .on_click(move |_event, _window, cx| {
                editor
                    .update(cx, |this, cx| {
                        this.toggle_row_expanded(&field_key, cx);
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
        .child(row_resize_strip(key, editor, muted))
}

/// The expanded ramp editor: the band and its toolbar, the colour picker for
/// the selected stop, and the same height strip a curve row has.
///
/// The picker sits in the panel rather than in the widget because
/// `ColorPickerState` needs a `Window` to be created and refreshed — the same
/// reason every other picker in this panel lives here.
fn ramp_editor_body(
    key: &str,
    state: &Entity<ParamRampEditorState>,
    picker: &Entity<ColorPickerState>,
    height: f32,
    has_selection: bool,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
) -> Div {
    let mut swatch_row = div()
        .flex()
        .items_center()
        .gap_2()
        .px_1()
        .py(px(1.0))
        .text_xs()
        .text_color(muted)
        .child(
            div()
                .flex_shrink_0()
                .child(SharedString::from(t!("properties.ramp.color"))),
        );
    swatch_row = if has_selection {
        swatch_row.child(ColorPicker::new(picker).small())
    } else {
        // No stop selected: the picker would edit nothing, so the row says so
        // rather than offering a control whose changes are dropped.
        swatch_row.child(
            div()
                .min_w_0()
                .truncate()
                .child(SharedString::from(t!("properties.ramp.no_selection"))),
        )
    };
    div()
        .flex()
        .flex_col()
        .px_1()
        .pb(px(2.0))
        .child(
            div()
                .h(px(height))
                .w_full()
                .child(ParamRampEditor::new(state)),
        )
        .child(swatch_row)
        .child(row_resize_strip(key, editor, muted))
}

/// The grab strip under an expanded inline editor, shared by curve and ramp
/// rows so both resize identically.
fn row_resize_strip(
    key: &str,
    editor: &WeakEntity<PropertiesGpuiPanel>,
    muted: Hsla,
) -> Stateful<Div> {
    let handle_key = SharedString::from(key.to_string());
    let begin = editor.clone();
    let moving = editor.clone();
    let ending = editor.clone();
    let ending_out = editor.clone();
    let drag_key = handle_key.clone();
    div()
        .id(SharedString::from(format!("row-resize-{key}")))
        .h(px(INLINE_RESIZE_HANDLE_HEIGHT))
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
                    .update(cx, |this, _cx| this.begin_row_resize(key, y))
                    .ok();
            }
        })
        .on_drag(DragRowHeight(drag_key.clone()), |drag, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| drag.clone())
        })
        .on_drag_move(move |event: &DragMoveEvent<DragRowHeight>, _window, cx| {
            let DragRowHeight(dragged) = event.drag(cx);
            if dragged != &drag_key {
                return;
            }
            if event.event.pressed_button != Some(MouseButton::Left) {
                moving
                    .update(cx, |this, _cx| this.end_row_resize_without_pointer())
                    .ok();
                return;
            }
            let y: f32 = event.event.position.y.into();
            moving.update(cx, |this, cx| this.row_resize_to(y, cx)).ok();
        })
        .on_mouse_up(
            MouseButton::Left,
            move |_event: &MouseUpEvent, _window, cx| {
                ending.update(cx, |this, _cx| this.end_row_resize()).ok();
            },
        )
        .on_mouse_up_out(
            MouseButton::Left,
            move |_event: &MouseUpEvent, _window, cx| {
                ending_out
                    .update(cx, |this, _cx| this.end_row_resize())
                    .ok();
            },
        )
}

#[allow(clippy::too_many_arguments)]
fn build_field_row(
    field: &PropertyField,
    scrubs: &[(String, Entity<ScrubInputState>)],
    strings: &[(String, Entity<InputState>)],
    selects: &[(String, Entity<SelectState<Vec<SharedString>>>)],
    colors: &[(String, Entity<ColorPickerState>)],
    expanded_rows: &std::collections::HashSet<String>,
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
            curve_row(key, curve, expanded_rows.contains(key), editor, muted, fg)
        }

        PropertyField::Ramp { key, ramp } => {
            ramp_row(key, ramp, expanded_rows.contains(key), editor, muted)
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
                // No picker widget for this field: the readout still shows
                // the display encoding rather than the working-space value
                // behind it (`CM-3`).
                let display = ColorSpace::DISPLAY.from_linear([*r, *g, *b]);
                row = row.child(div().flex_shrink_0().text_xs().text_color(fg).child(
                    SharedString::from(format!(
                        "({:.2}, {:.2}, {:.2})",
                        display[0], display[1], display[2]
                    )),
                ));
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
/// or not animatable (`Bool` is constant-only in v1, REQ-LAYER-004; so are
/// `PathPoints`, `Curve` and `Ramp`).
///
/// `None` for an **identifier** parameter too, whatever its kind: a
/// `layer.ref` target or a `precomp` composition is a raw id, and animating
/// one has no meaning to give
/// (`ravel_core::composition::validate::is_identifier_parameter` is the single
/// place that decides which those are). This is what keeps the toggle off
/// those rows — hiding it here rather than refusing the click is the honest
/// answer, because a toggle that does nothing is worse than no toggle.
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
    if ravel_core::composition::validate::is_identifier_parameter(&node.type_key, key) {
        return None;
    }
    let param = node.parameters.iter().find(|p| p.key == key)?;
    match &param.value {
        ParameterValue::Float(_) => Some(false),
        ParameterValue::Channel(channel) => Some(has_key(channel, local_frame)),
        // An animatable int is the same channel with the same keys; a constant
        // `Int` is its unkeyed spelling, so the row offers the toggle.
        ParameterValue::Int(_) => Some(false),
        ParameterValue::IntChannel(channel) => Some(has_key(channel, local_frame)),
        // A step curve keeps its own keys, so it answers directly. Without a
        // local frame any key at all counts as keyed, matching the
        // keyframed-source reading above.
        ParameterValue::String(_) => Some(false),
        ParameterValue::StringSteps(steps) => Some(match local_frame {
            Some(frame) => steps.contains_key(frame),
            None => !steps.is_empty(),
        }),
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

/// The fold identity of each of `sections`, in order: `Some((type_key, group))`
/// for one of `node`'s parameter groups, `None` for a section that is always
/// open (PGRP-3).
///
/// The parameter sections sit directly after the single info section
/// (`sections_for_node`), so a group is found **by position** and confirmed by
/// the title. Matching on the title alone would let a group the user named
/// after another section's heading — an In node's instance group is free text
/// — fold that section along with its own.
fn param_group_keys(
    node: &Node,
    registry: &NodeRegistry,
    sections: &[PropertySection],
) -> Vec<Option<(String, String)>> {
    let groups = ravel_ui::properties::node::param_group_titles(node, registry);
    sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            index
                .checked_sub(1)
                .and_then(|group| groups.get(group))
                .filter(|(_, title)| title == &section.title)
                .map(|(group, _)| (node.type_key.clone(), group.clone()))
        })
        .collect()
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
                let _ = write!(
                    shape,
                    "\n{}\t{:?}\t{}\t{:?}",
                    row.name, row.port_type, row.fixed, row.group
                );
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

/// A ramp row's widgets: the inline editor and the colour picker that edits
/// its selected stop. The picker is here rather than inside the editor because
/// `ColorPickerState` needs a `Window` — the constraint that puts every other
/// picker in this panel too.
struct RampBinding {
    state: Entity<ParamRampEditorState>,
    picker: Entity<ColorPickerState>,
    #[allow(dead_code)]
    subs: [Subscription; 2],
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
    groups: Vec<(String, Entity<InputState>)>,
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

/// Working-space colour → the picker's display-referred `Hsla`.
///
/// Parameter colours are linear light from `.ravprj` v8 on (`CM-2`); a colour
/// picker — its swatch, its sliders and its hex field — is a display, so it
/// gets the display encoding, exactly like the viewer. Without this a
/// migrated project would show every colour far darker than it renders.
/// [`rgba_from_hsla`] is the inverse and the two must stay a pair.
fn hsla_from_rgba(r: f32, g: f32, b: f32, a: f32) -> Hsla {
    let display = ColorSpace::DISPLAY.from_linear([r, g, b]);
    Hsla::from(Rgba {
        r: display[0],
        g: display[1],
        b: display[2],
        a,
    })
}

/// The picker's display-referred value → a working-space colour.
fn rgba_from_hsla(hsla: Hsla) -> [f32; 4] {
    let rgba = Rgba::from(hsla);
    let linear = ColorSpace::DISPLAY.to_linear([rgba.r, rgba.g, rgba.b]);
    [linear[0], linear[1], linear[2], rgba.a]
}

/// What kind of target the current widgets were built for. Same-identity
/// target updates (undo refresh, live document sync) update values in place
/// so an in-flight scrub gesture keeps its widget entities.
/// Whether any of `layer_ids` shows a value sampled at the playhead. A layer
/// the document does not have answers "yes": the panel is about to resolve
/// something else, and guessing "no" there would freeze whatever replaces it.
fn animated_layer(
    document: &Document,
    comp_id: ravel_core::id::CompId,
    layer_ids: &[ravel_core::id::LayerId],
) -> bool {
    let Some(comp) = document.get_composition(comp_id) else {
        return true;
    };
    layer_ids.iter().any(|layer_id| {
        let Some(layer) = comp.get_layer(*layer_id) else {
            return true;
        };
        // The shell half. `property_rows` emits every `SHELL_GROUPS` entry
        // unconditionally (plus audio gain when the layer has audio), which is
        // exactly what the shell sections display, so asking `row_channels` for
        // each group covers it.
        let shell = ravel_ui::keyframes::SHELL_GROUPS
            .iter()
            .copied()
            .chain(layer.audio.is_some().then_some(PropertyGroup::AudioGain))
            .any(|group| {
                ravel_ui::keyframes::row_channels(layer, &PropertyRowId::Shell(group))
                    .unwrap_or_default()
                    .iter()
                    .any(|channel| animated_channel(channel))
            });
        // The network half walks the parameters themselves, **not**
        // `property_rows`. That row enumeration exists for the Timeline's
        // keyframe tree, so `keyframed_channel_names` drops any parameter with
        // no `Keyframes` component — an `Expression`, a `NodeOutput`, an
        // `AudioReactive` source or a `Blend` of them gets no row at all. Those
        // are still values Properties samples at the playhead
        // (`custom_parameters_section` evaluates every `Channel*` at the
        // layer-local frame), so keying the gate on the tree's rows froze them
        // on screen. Same class of bug as inheriting a stale gate across a
        // target switch: the predicate was conservative, the enumeration was not.
        shell || animated_graph(&layer.network)
    })
}

/// Whether `graph` holds a parameter whose displayed value is sampled at the
/// frame, subnets included.
///
/// Wider than what a Layer target shows today — `custom_parameters_section`
/// displays the root In node's custom parameters only — and deliberately so:
/// evaluation recurses through the network boundary into subnets, this costs one
/// allocation-free walk, and the failure it guards against is a value frozen on
/// screen. Erring wide spends a rebuild; erring narrow shows a wrong number.
///
/// Incoming edges are *not* treated as animating, unlike the `Nodes` branch. A
/// `Nodes` target renders a driven parameter's value, so what feeds it matters
/// there; a Layer target reads `param.value` directly and never displays a
/// value pulled through an edge. Counting edges here would mark every connected
/// network animated and retire the gate altogether.
fn animated_graph(graph: &ravel_core::graph::Graph) -> bool {
    graph.nodes().any(|node| {
        node.parameters
            .iter()
            .any(|parameter| animated_parameter(&parameter.value))
            || node.subnet.as_deref().is_some_and(animated_graph)
    })
}

/// Whether this channel's value can differ from one frame to the next.
///
/// Only a plain constant cannot. Keyframes, expressions, node outputs,
/// audio-reactive sources and blends of any of them all resolve against the
/// evaluation frame.
fn animated_channel(channel: &ravel_core::animation::channel::AnimationChannel) -> bool {
    !matches!(
        channel.source,
        ravel_core::animation::channel::ChannelSource::Constant(_)
    )
}

/// The same question for a node parameter: its channel components, if it has
/// any.
fn animated_parameter(value: &ParameterValue) -> bool {
    match value {
        ParameterValue::Channel(channel) | ParameterValue::IntChannel(channel) => {
            animated_channel(channel)
        }
        ParameterValue::Channel2(channels) => channels.iter().any(animated_channel),
        ParameterValue::Channel3(channels) => channels.iter().any(animated_channel),
        ParameterValue::Channel4(channels) => channels.iter().any(animated_channel),
        // A step curve needs two keys to show a different string from one
        // frame to the next; one key or none is the same value everywhere.
        ParameterValue::StringSteps(steps) => steps.len() > 1,
        ParameterValue::Float(_)
        | ParameterValue::Int(_)
        | ParameterValue::Bool(_)
        | ParameterValue::String(_)
        | ParameterValue::PathPoints(_)
        | ParameterValue::Curve(_)
        | ParameterValue::Ramp(_) => false,
    }
}

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
    ramps: Vec<(String, RampBinding)>,
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
    /// Group Input per custom In port that carries a parameter (PGRP-4).
    /// Shorter than `port_names`: a wire-only custom type has no parameter to
    /// group.
    port_groups: Vec<(String, StringBinding)>,
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
    expanded_rows: std::collections::HashSet<String>,
    row_heights: std::collections::HashMap<String, f32>,
    row_resize: Option<RowResize>,
    /// Uncommitted color edit awaiting its debounced undo commit — the key,
    /// the value and the nodes it addresses — with the generation guard that
    /// cancels superseded commits.
    ///
    /// The nodes travel with it because the commit may have to be *flushed*
    /// from somewhere that no longer knows them (a target switch, a second
    /// gesture on another row); see [`Self::flush_pending_color_commit`].
    pending_color_commit: Option<(String, PropertyValue, Vec<NodeId>)>,
    color_commit_generation: u64,
    needs_rebuild: bool,
    /// A shape refresh found that the row owning a live gesture disappeared.
    /// The next render must end that gesture before dropping its bindings.
    end_gesture_before_rebuild: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    /// Whether anything the panel currently shows is sampled at the playhead
    /// frame — an animated channel, an expression, a driven parameter.
    ///
    /// Recomputed by every [`Self::refresh_values`], so it describes the target
    /// and document the sections were last built from. When it is false the
    /// playhead observer has nothing to do: re-resolving the target and
    /// rebuilding every section string 30–60 times a second would produce the
    /// identical panel (`MED-UI-02`). It starts true so a panel that has not
    /// resolved anything yet still follows the playhead.
    playhead_sensitive: bool,
    #[allow(dead_code)]
    playback_sub: Subscription,
    /// Pays off the syncs skipped while the panel was behind another tab
    /// (see [`super::on_became_visible`]).
    #[allow(dead_code)]
    visibility_sub: Subscription,
    /// Folding a group writes the Global; a second Properties panel has to
    /// repaint from it rather than wait for an unrelated refresh
    /// (`CollapsedParamGroupsState`).
    #[allow(dead_code)]
    collapsed_groups_sub: Subscription,
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

        let collapsed_groups_sub =
            cx.observe_global::<super::CollapsedParamGroupsState>(|_this, cx| cx.notify());
        let selection_sub =
            cx.observe_global::<SelectedPropertiesTarget>(move |this: &mut Self, cx| {
                let target = cx
                    .try_global::<SelectedPropertiesTarget>()
                    .cloned()
                    .unwrap_or_default();
                let same = same_target(&this.target, &target.0);
                if same {
                    this.target = target.0;
                    // Same target, new values (undo, timeline drag, playhead
                    // move): refresh in place so scrub gestures survive —
                    // unless the field shape changed (a parameter became
                    // driven or editable again), where stale widget bindings
                    // would edit through a read-only row.
                    //
                    // Gated on the document epoch, which is what makes a node
                    // parameter drag cost one re-resolve per mouse move instead of
                    // two (`MED-UI-06`): the node editor republishes this identical
                    // target from `refresh_from_document` on every move, and the
                    // `ProjectState` notify of the same move asks for the same work.
                    // Whichever arrives first resolves the sections and records the
                    // epoch; the other finds it recorded and returns. A republish
                    // with no document change behind it cannot have new values —
                    // the playhead has its own observer below.
                    //
                    // A hidden panel resolves nothing and records no epoch, so
                    // the values it did not resolve stay owed until
                    // `visibility_sub` below brings it back.
                    if super::is_instance_visible(instance, cx) {
                        let epoch = this
                            .project
                            .as_ref()
                            .map(|project| project.read(cx).mirror_epoch());
                        match epoch {
                            Some(epoch) if !this.mirror_epoch.advanced(epoch) => {}
                            _ => this.refresh_values_checked(cx),
                        }
                    }
                } else {
                    // An in-flight gesture belongs to the target being left and
                    // its live value is already in the document: end it here,
                    // while `self.target` still names the row it came from,
                    // instead of dropping it into an unrelated undo step — or,
                    // worse, onto the target being switched to.
                    this.end_gestures(cx);
                    this.target = target.0;
                    // A refusal names a port on the target that is going away,
                    // and a rename record names a row that is going with it.
                    this.port_error = None;
                    this.committed_port_rename = None;
                    this.exposed_error = None;
                    this.committed_exposed_rename = None;
                    // Curve expansion is per-target view state (see the field
                    // docs): a new target starts with every curve row collapsed,
                    // so returning to a node shows it collapsed again.
                    this.expanded_rows.clear();
                    this.row_heights.clear();
                    this.row_resize = None;
                    this.needs_rebuild = true;
                    // This branch rebuilds the widgets in `render` rather than
                    // calling `refresh_values`, so nothing here recomputes the
                    // playhead gate. Reopening it is what keeps a switch from a
                    // static target to an animated one from inheriting "nothing
                    // follows the playhead" and freezing the new target's values:
                    // the next playhead move refreshes once and settles the flag on
                    // what is actually on screen.
                    this.playhead_sensitive = true;
                }
                cx.notify();
            });

        // Any document change (edit, undo/redo, live gesture update)
        // re-resolves the current target's values in place — the same
        // semantics as a same-target republish, so an in-flight scrub
        // gesture is never destroyed.
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, move |this: &mut Self, project, cx| {
                if matches!(this.target, PropertiesTarget::Empty) {
                    return;
                }
                // Behind another tab nobody can read these values, so the
                // resolve waits for the panel to come back — *before* the
                // epoch gate below, so the skipped change stays owed.
                if !super::is_instance_visible(instance, cx) {
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
        let playback_sub =
            cx.observe_global::<super::PlaybackPosition>(move |this: &mut Self, cx| {
                if matches!(this.target, PropertiesTarget::Empty) {
                    return;
                }
                // Playback behind another tab paints nothing here. Unlike the
                // document paths there is no epoch to leave alone: the forced
                // sync on the way back re-samples at whatever frame the playhead
                // has reached by then.
                if !super::is_instance_visible(instance, cx) {
                    return;
                }
                // Nothing on display is sampled at the playhead, so moving it
                // cannot change a value, a ◆/◇ state or a string. Re-resolving the
                // target here is what made a paused-looking panel cost a full
                // section rebuild on every playback frame (`MED-UI-02`).
                if !this.playhead_sensitive {
                    return;
                }
                this.refresh_values(cx);
                cx.notify();
            });

        // Coming back into view pays off everything the three observers above
        // skipped while hidden, in one resolve.
        let visibility_sub = super::on_became_visible(instance, cx, |this, cx| {
            if matches!(this.target, PropertiesTarget::Empty) {
                return;
            }
            // Adopt the epoch of what is being resolved now, so the next
            // document notify is gated as usual instead of resolving twice.
            if let Some(project) = this.project.clone() {
                let epoch = project.read(cx).mirror_epoch();
                this.mirror_epoch.advanced(epoch);
            }
            this.refresh_values_checked(cx);
            cx.notify();
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
            ramps: Vec::new(),
            port_names: Vec::new(),
            port_types: Vec::new(),
            port_groups: Vec::new(),
            port_add: None,
            port_type_options: Vec::new(),
            port_error: None,
            committed_port_rename: None,
            exposed_names: Vec::new(),
            exposed_descriptions: Vec::new(),
            exposed_error: None,
            committed_exposed_rename: None,
            expanded_rows: std::collections::HashSet::new(),
            row_heights: std::collections::HashMap::new(),
            row_resize: None,
            pending_color_commit: None,
            color_commit_generation: 0,
            expressions: Vec::new(),
            expression_inputs: Vec::new(),
            expression_drafts: Vec::new(),
            // The target above may already name something, and nothing has
            // built its widgets yet.
            needs_rebuild: true,
            end_gesture_before_rebuild: false,
            focus_handle,
            focus_subscriptions,
            selection_sub,
            collapsed_groups_sub,
            project_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            playhead_sensitive: true,
            playback_sub,
            visibility_sub,
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
            if self.gesture_row_disappeared(cx) {
                self.end_gesture_before_rebuild = true;
            }
        }
    }

    /// Whether an edit gesture is mid-flight, i.e. a widget still owes this
    /// panel the commit that ends it.
    ///
    /// A scrub is two events (`Change` … `Commit`) delivered through a
    /// subscription the panel owns, and a color gesture is a live change plus
    /// a debounced commit; in both cases the document already holds an
    /// uncommitted value. Rebuilding the widgets drops the subscription that
    /// was going to deliver the commit, so the value stays in the document
    /// with no undo entry in front of it and Undo jumps past the gesture
    /// entirely. The rebuild waits instead — the gesture is short, and the
    /// stale-binding risk a rebuild guards against is already covered where it
    /// matters by the driven-parameter check in [`Self::route_change`].
    ///
    /// This only holds the rebuild off while the panel keeps pointing at the
    /// same target. A target switch ends the gesture instead, on the row it
    /// belongs to — see [`Self::end_gestures`].
    fn gesture_in_flight(&self, cx: &App) -> bool {
        self.pending_color_commit.is_some()
            || self.row_resize.is_some()
            || self
                .scrubs
                .iter()
                .any(|(_, binding)| binding.state.read(cx).is_dragging())
            || self
                .curves
                .iter()
                .any(|(_, binding)| binding.state.read(cx).gesture_in_flight(cx))
            || self
                .ramps
                .iter()
                .any(|(_, binding)| binding.state.read(cx).gesture_in_flight(cx))
    }

    /// Whether a shape refresh removed the editable row that owns a gesture.
    /// A same-row refresh (including an expression collapsing to a plain
    /// numeric value) keeps the existing binding alive; a driven conversion
    /// or a removed node does not, so waiting for release would wait forever.
    fn gesture_row_disappeared(&self, cx: &App) -> bool {
        let field_for = |key: &str| {
            self.sections
                .iter()
                .flat_map(|section| &section.fields)
                .find(|field| field.key() == key)
        };

        if self
            .pending_color_commit
            .as_ref()
            .is_some_and(|(key, _, _)| !matches!(field_for(key), Some(PropertyField::Color { .. })))
        {
            return true;
        }

        if self.scrubs.iter().any(|(key, binding)| {
            binding.state.read(cx).is_dragging()
                && !Self::field_accepts_scrub(
                    field_for(key)
                        .or_else(|| key.split_once('#').and_then(|(key, _)| field_for(key))),
                    key,
                )
        }) {
            return true;
        }

        if self.curves.iter().any(|(key, binding)| {
            binding.state.read(cx).gesture_in_flight(cx)
                && !matches!(field_for(key), Some(PropertyField::Curve { .. }))
        }) {
            return true;
        }

        if self.ramps.iter().any(|(key, binding)| {
            binding.state.read(cx).gesture_in_flight(cx)
                && !matches!(field_for(key), Some(PropertyField::Ramp { .. }))
        }) {
            return true;
        }

        self.row_resize.as_ref().is_some_and(|resize| {
            !matches!(field_for(&resize.key), Some(PropertyField::Curve { .. }))
                && !matches!(field_for(&resize.key), Some(PropertyField::Ramp { .. }))
        })
    }

    fn field_accepts_scrub(field: Option<&PropertyField>, scrub_key: &str) -> bool {
        match field {
            Some(PropertyField::Float { key, .. }) | Some(PropertyField::Int { key, .. }) => {
                key == scrub_key
            }
            Some(PropertyField::Vector {
                key, components, ..
            }) => vector_component_keys(key, components.len())
                .iter()
                .any(|key| key == scrub_key),
            _ => false,
        }
    }

    /// End every in-flight edit gesture, taking the undo step it owes, before
    /// the panel stops pointing at the row the gesture belongs to — either
    /// because the target changes or because a shape refresh removes that
    /// row. The latter cannot deliver a mouse-up, so `render` calls this
    /// before rebuilding the bindings.
    ///
    /// A widget's `Commit` travels on a subscription, and GPUI delivers an
    /// emitted event *after* the callback that triggered it returns — by then
    /// `self.target` names the newly selected row, and `route_change` would
    /// write a scrub of layer A onto layer B. So the drags end here and their
    /// bindings are dropped, which leaves the queued event without a
    /// subscriber, and the commit is taken directly: the gesture's value is
    /// already in the live document, and `commit` differs from `apply` only in
    /// recording the undo step, so committing the live document as it stands
    /// is the snapshot the routed commit would have produced.
    fn end_gestures(&mut self, cx: &mut Context<Self>) {
        let mut ended = self.row_resize.take().is_some();
        // The debounced color commit has no widget event racing the switch, so
        // it routes normally — while `self.target` is still the old one.
        let flushed = self.pending_color_commit.is_some();
        self.flush_pending_color_commit(cx);

        let mut moved = false;
        for (_, binding) in &self.scrubs {
            if binding.state.read(cx).is_dragging() {
                ended = true;
                moved |= binding.state.update(cx, |state, cx| state.end_drag(cx)) == Some(true);
            }
        }
        for (_, binding) in &self.curves {
            if binding.state.read(cx).gesture_in_flight(cx) {
                ended = true;
                moved |= binding.state.update(cx, |state, cx| state.end_gestures(cx));
            }
        }
        for (_, binding) in &self.ramps {
            if binding.state.read(cx).gesture_in_flight(cx) {
                ended = true;
                moved |= binding.state.update(cx, |state, cx| state.end_gestures(cx));
            }
        }
        if !ended {
            return;
        }
        // Both `end_drag`s emit — a `Commit`, or a `Change` for a gesture that
        // settled back where it started. Dropping the bindings now is what
        // keeps either from reaching the target being switched to.
        self.scrubs.clear();
        self.curves.clear();
        self.ramps.clear();
        // The color flush already recorded a step over the same live document.
        if !moved || flushed {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        project.update(cx, |project, cx| {
            let live = project.document().clone();
            // The document is unchanged by this call, so nothing needs
            // re-evaluating: only the undo step is new.
            project.commit_document(live, InvalidationHint::None, cx);
        });
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
                .get_media_asset(audio.asset_id)?
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

    /// Record which parameter groups are folded after a click in the section
    /// accordion (PGRP-3).
    ///
    /// `keys` is the `(type_key, group)` fold identity of each section in
    /// order (`None` for a section that cannot be folded away) and `open` the
    /// indices the accordion reports as open. The accordion hands over the
    /// whole open set rather than the header that moved, and it fires on any
    /// click that bubbles out of the list — a scrub, a checkbox, a text field
    /// — so every fold is written through
    /// [`crate::panels::set_param_group_collapsed`], which reports whether it
    /// changed anything, and only a real change repaints.
    ///
    /// The fold is UI state: it never reaches the document, so it records no
    /// undo step, and the project save path carries it to `ui_state.json`.
    fn apply_param_group_folds(
        &mut self,
        keys: &[Option<(String, String)>],
        open: &[usize],
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        for (index, key) in keys.iter().enumerate() {
            let Some((type_key, group)) = key else {
                continue;
            };
            changed |= crate::panels::set_param_group_collapsed(
                type_key,
                group,
                !open.contains(&index),
                cx,
            );
        }
        if changed {
            cx.notify();
        }
    }

    /// Open or close the inline curve editor of the row `key`.
    ///
    /// Expansion is view state only: nothing here touches the document, so
    /// the toggle records no undo step and rows stay independent (opening
    /// one never closes another).
    fn toggle_row_expanded(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded_rows.remove(key) {
            self.expanded_rows.insert(key.to_string());
        }
        cx.notify();
    }

    /// Whether the curve row `key` is currently expanded.
    #[cfg(test)]
    fn is_row_expanded(&self, key: &str) -> bool {
        self.expanded_rows.contains(key)
    }

    /// Height of the row `key`'s expanded editor.
    fn row_height(&self, key: &str) -> f32 {
        self.row_heights
            .get(key)
            .copied()
            .unwrap_or(INLINE_EDITOR_HEIGHT)
    }

    fn begin_row_resize(&mut self, key: String, pointer_y: f32) {
        let start_height = self.row_height(&key);
        self.row_resize = Some(RowResize {
            key,
            start_y: pointer_y,
            start_height,
        });
    }

    fn row_resize_to(&mut self, pointer_y: f32, cx: &mut Context<Self>) {
        let Some(resize) = &self.row_resize else {
            return;
        };
        let height = (resize.start_height + (pointer_y - resize.start_y))
            .clamp(INLINE_EDITOR_MIN_HEIGHT, INLINE_EDITOR_MAX_HEIGHT);
        self.row_heights.insert(resize.key.clone(), height);
        cx.notify();
    }

    fn end_row_resize(&mut self) {
        self.row_resize = None;
    }

    fn end_row_resize_without_pointer(&mut self) {
        self.end_row_resize();
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

    /// Commit a row's edited group on Enter or blur (PGRP-4).
    ///
    /// No repeat guard like [`Self::rename_port`]'s: this edit does not change
    /// the row's identity, so the second report of an Enter-then-blur pair
    /// carries the value the graph already holds and
    /// `network::set_custom_port_group` answers it with the graph it was given
    /// — no undo step, nothing to suppress.
    fn set_port_group(&mut self, name: &str, group: String, cx: &mut Context<Self>) {
        if self
            .port_row(name)
            .is_some_and(|row| row.group.as_deref() == Some(group.trim()))
        {
            return;
        }
        let name = name.to_string();
        self.route_port_edit(cx, move |editor, node_id, cx| {
            editor.set_custom_port_group(node_id, &name, &group, cx)
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
    /// Declaring twice is refused rather than ignored: the caller that reaches
    /// here with the parameter already declared (the node graph menu) has no
    /// second declaration to make, and saying so beats a silent no-op.
    /// The Properties checkbox does not take this path when declared — it
    /// withdraws instead, through [`Self::toggle_exposed_parameter`].
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

    /// Flip whether `key` on `node_id` is declared as a project input.
    ///
    /// The icon beside a parameter is drawn as a checkbox, so it has to behave
    /// like one — the sibling `toggle_param_port` in the node editor branches
    /// the same way. Which half runs is decided from the document, not from
    /// the `declared` flag the row was rendered with, so a stale frame cannot
    /// turn a withdrawal into a second declaration.
    ///
    /// Withdrawing removes a name a caller may already be passing on a command
    /// line, which is why it stayed one-way for a while. It is one undo step
    /// like every other declaration edit, and the declarations list has always
    /// offered the same removal without a confirmation, so the asymmetry was
    /// protecting nothing and only made the checkbox lie.
    ///
    /// A binding is not unique — [`ExposedParameters::bound_to`] says so, and
    /// two declarations may drive one parameter. The checkbox is per
    /// *parameter*, not per declaration, so unchecking it withdraws **every**
    /// declaration bound to that parameter in one undo step. Removing only the
    /// first would leave the box filled and the click looking ignored.
    fn toggle_exposed_parameter(&mut self, node_id: NodeId, key: &str, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let declared: Vec<String> = project
            .read(cx)
            .document()
            .exposed_parameters
            .iter()
            .filter(|declaration| {
                let binding = declaration.binding();
                binding.node == node_id && binding.key == key
            })
            .map(|declaration| declaration.name().to_string())
            .collect();
        if declared.is_empty() {
            self.expose_parameter(node_id, key, cx);
            return;
        }
        self.edit_declarations(cx, move |declarations| {
            Ok(declared
                .iter()
                .filter(|name| declarations.remove(name).is_some())
                .count()
                > 0)
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
        // One slot carries the pending commit, so a gesture on a *different*
        // row would overwrite an edit that is already live in the document.
        // Reusing the slot for the same row is the debounce doing its job —
        // the later value supersedes the earlier one and both belong to the
        // same undo step — so only a different row flushes first.
        if self
            .pending_color_commit
            .as_ref()
            .is_some_and(|(pending, _, ids)| pending != key || ids != node_ids)
        {
            self.flush_pending_color_commit(cx);
        }
        self.route_change(key, value.clone(), false, node_ids, cx);
        self.color_commit_generation += 1;
        let generation = self.color_commit_generation;
        self.pending_color_commit = Some((key.to_string(), value, node_ids.to_vec()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COLOR_COMMIT_QUIET).await;
            this.update(cx, |this, cx| {
                if this.color_commit_generation != generation {
                    return;
                }
                this.flush_pending_color_commit(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Commit a pending color edit before the slot that carries it is cleared
    /// or reused.
    ///
    /// The live `Change` already reached the document through `apply_document`
    /// (no undo step); dropping the commit would leave that value folded into
    /// whatever undo step comes next. Bumping the generation cancels the timer
    /// that was going to do this.
    fn flush_pending_color_commit(&mut self, cx: &mut Context<Self>) {
        let Some((key, value, ids)) = self.pending_color_commit.take() else {
            return;
        };
        self.color_commit_generation += 1;
        self.route_change(&key, value, true, &ids, cx);
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
                // Compared in the working space, which is the space the
                // field is in — converting the picker back is the only way
                // the two are comparable at all.
                let differs = binding.state.read(cx).value().is_none_or(|current| {
                    let current = rgba_from_hsla(current);
                    (current[0] - r).abs() > 1e-3
                        || (current[1] - g).abs() > 1e-3
                        || (current[2] - b).abs() > 1e-3
                        || (current[3] - a).abs() > 1e-3
                });
                if differs {
                    updates.push((binding.state.clone(), hsla_from_rgba(*r, *g, *b, *a)));
                }
            }
        }
        // A ramp row's picker shows the *selected stop*, which lives in the
        // editor's view state rather than in the field, so it is synced from
        // there — but for the same reasons and on the same render-time terms.
        for (_, binding) in &self.ramps {
            let Some(stop) = binding.state.read(cx).selected_stop() else {
                continue;
            };
            let differs = binding.picker.read(cx).value().is_none_or(|current| {
                let current = rgba_from_hsla(current);
                (current[0] - stop.color.r).abs() > 1e-3
                    || (current[1] - stop.color.g).abs() > 1e-3
                    || (current[2] - stop.color.b).abs() > 1e-3
                    || (current[3] - stop.color.a).abs() > 1e-3
            });
            if differs {
                updates.push((
                    binding.picker.clone(),
                    hsla_from_rgba(stop.color.r, stop.color.g, stop.color.b, stop.color.a),
                ));
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
                    (PropertyField::Ramp { ramp, .. }, PropertyValue::Ramp(new)) => {
                        ramp.clone_from(new);
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
    /// Whether anything this target shows is sampled at the playhead frame.
    ///
    /// Conservative by construction: only a channel that is a plain constant
    /// is frame-independent, and a node with any incoming edge is treated as
    /// following the playhead because a driven value follows whatever feeds
    /// it. Anything unresolvable answers "yes" — a panel that refreshes when
    /// it did not need to is a wasted rebuild, one that skips when it should
    /// not have is a frozen value on screen.
    fn target_follows_the_playhead(&self, cx: &App) -> bool {
        let Some(project) = &self.project else {
            return true;
        };
        let document = project.read(cx).document();
        match &self.target {
            PropertiesTarget::Empty => false,
            PropertiesTarget::Layer { comp_id, layer_id } => {
                animated_layer(document, *comp_id, std::slice::from_ref(layer_id))
            }
            PropertiesTarget::Layers { comp_id, layer_ids } => {
                animated_layer(document, *comp_id, layer_ids)
            }
            PropertiesTarget::Nodes { network, ids } => {
                let Some(graph) = ravel_ui::document::resolve_network(document, network) else {
                    return true;
                };
                ids.iter().any(|id| {
                    let Some(node) = graph.node(*id) else {
                        return true;
                    };
                    // A driven parameter shows whatever its upstream produces,
                    // which the playhead can move.
                    if graph.edges().any(|edge| edge.target == *id) {
                        return true;
                    }
                    node.parameters
                        .iter()
                        .any(|param| animated_parameter(&param.value))
                })
            }
            // Composition settings, a media asset's metadata and the project's
            // exposed parameter declarations are all frame-independent.
            PropertiesTarget::Composition { .. }
            | PropertiesTarget::MediaAsset { .. }
            | PropertiesTarget::Project => false,
        }
    }

    fn refresh_values(&mut self, cx: &mut Context<Self>) {
        super::sync_probe::record(super::sync_probe::PanelSync::PropertiesRefresh);
        self.playhead_sensitive = self.target_follows_the_playhead(cx);
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

        // Ramp editors follow the document on exactly the same terms.
        let ramps: Vec<(String, ravel_core::param_ramp::RampParam)> = self
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .filter_map(|field| match field {
                PropertyField::Ramp { key, ramp } => Some((key.clone(), ramp.clone())),
                _ => None,
            })
            .collect();
        for (key, ramp) in ramps {
            if let Some((_, binding)) = self.ramps.iter().find(|(k, _)| k == &key) {
                binding.state.update(cx, |state, cx| {
                    if state.ramp() != &ramp {
                        state.set_ramp_synced(ramp, cx);
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
        self.ramps.clear();
        self.port_names.clear();
        self.port_types.clear();
        self.port_groups.clear();
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
                            // The picker speaks display-referred `Hsla`;
                            // parameter colours are working-space linear
                            // light (`CM-2`), so the edit is decoded on the
                            // way in.
                            let rgba = rgba_from_hsla(*hsla);
                            let value = PropertyValue::Color {
                                r: rgba[0],
                                g: rgba[1],
                                b: rgba[2],
                                a: rgba[3],
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

                if let PropertyField::Ramp { key, ramp } = field {
                    let state = cx.new(|cx| ParamRampEditorState::new(ramp.clone(), cx));
                    // Nothing is selected yet, so the picker opens on the
                    // ramp's first stop rather than on an arbitrary colour;
                    // `sync_color_widgets` takes it over from the selection.
                    let seed = ramp.stops().first().map(|stop| {
                        hsla_from_rgba(stop.color.r, stop.color.g, stop.color.b, stop.color.a)
                    });
                    let picker = cx.new(|cx| {
                        let picker = ColorPickerState::new(window, cx);
                        match seed {
                            Some(value) => picker.default_value(value),
                            None => picker,
                        }
                    });
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let state_sub =
                        cx.subscribe(&state, move |this, _state, event: &ParamRampEvent, cx| {
                            // Same gesture granularity as a curve: live stop
                            // moves apply without undo, the gesture's Commit
                            // records one Document undo step.
                            let (ramp, commit) = match event {
                                ParamRampEvent::Change(ramp) => (ramp.clone(), false),
                                ParamRampEvent::Commit(ramp) => (ramp.clone(), true),
                            };
                            let value = PropertyValue::Ramp(ramp);
                            this.update_field_value(&field_key, &value);
                            this.route_change(&field_key, value, commit, &ids, cx);
                        });
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let edited = state.clone();
                    let picker_sub = cx.subscribe(
                        &picker,
                        move |this, _picker, event: &ColorPickerEvent, cx| {
                            let ColorPickerEvent::Change(Some(hsla)) = event else {
                                return;
                            };
                            let rgba = rgba_from_hsla(*hsla);
                            let color =
                                ravel_core::types::Color::new(rgba[0], rgba[1], rgba[2], rgba[3]);
                            // No stop selected — or one that just went away —
                            // means there is nothing to recolour.
                            let Some(ramp) =
                                edited.update(cx, |state, cx| state.set_selected_color(color, cx))
                            else {
                                return;
                            };
                            let value = PropertyValue::Ramp(ramp);
                            this.update_field_value(&field_key, &value);
                            // The picker emits a change per slider tick with
                            // no gesture-end event, so the undo step is
                            // debounced exactly as a Color row's is.
                            this.apply_color_change(&field_key, value, &ids, cx);
                        },
                    );
                    self.ramps.push((
                        key.clone(),
                        RampBinding {
                            state,
                            picker,
                            subs: [state_sub, picker_sub],
                        },
                    ));
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

            // Only a port that has a parameter can carry a group; a wire-only
            // custom type has none, so it gets no Input at all rather than one
            // whose edit the core would refuse.
            let Some(group) = row.group.clone() else {
                continue;
            };
            let entity = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(SharedString::from(t!("properties.ports.group")))
                    .default_value(group)
            });
            let name = row.name.clone();
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, state, event: &InputEvent, _window, cx| match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        let value = state.read(cx).value().to_string();
                        this.set_port_group(&name, value, cx);
                    }
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
            self.port_groups
                .push((row.name.clone(), StringBinding { state: entity, sub }));
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
        // A same-row refresh requested during a gesture waits for its normal
        // release. If the row itself disappeared, release cannot arrive, so
        // end it while its old bindings still exist before rebuilding.
        if self.needs_rebuild {
            if self.end_gesture_before_rebuild {
                self.end_gesture_before_rebuild = false;
                self.end_gestures(cx);
            }
            if !self.gesture_in_flight(cx) {
                self.rebuild_widgets(window, cx);
            }
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
                .map(|(k, b)| (k.clone(), b.state.clone(), self.row_height(k)))
                .collect();
            // Ramp rows: the editor entity, its picker, the height it was
            // dragged to, and whether a stop is selected (the picker is only
            // offered when there is something for it to edit).
            type RampRowWidgets = (
                String,
                Entity<ParamRampEditorState>,
                Entity<ColorPickerState>,
                f32,
                bool,
            );
            let ramp_entities: Vec<RampRowWidgets> = self
                .ramps
                .iter()
                .map(|(k, b)| {
                    (
                        k.clone(),
                        b.state.clone(),
                        b.picker.clone(),
                        self.row_height(k),
                        b.state.read(cx).selected_stop().is_some(),
                    )
                })
                .collect();
            let expanded_rows = self.expanded_rows.clone();
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
                groups: self
                    .port_groups
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
                                // An identifier parameter cannot be exposed as
                                // a port (a wire would make the reference a
                                // function of the frame), so it carries no
                                // toggle — unless a port already exists, where
                                // the toggle is the only way to remove it. Same
                                // split as `toggle_param_port`, which refuses
                                // the exposing half and nothing else.
                                .filter(|p| {
                                    !ravel_core::composition::validate::is_identifier_parameter(
                                        &node.type_key,
                                        &p.key,
                                    ) || node.param_port_index(&p.key).is_some()
                                })
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

            // Which sections are node-parameter groups the user may fold away,
            // and under which `(type_key, group)` the fold is remembered
            // (PGRP-3). `None` for every other section — the info and ports
            // sections stay open, as they always were.
            let param_group_keys = match &resolved_nodes {
                Some((nodes, ..)) => param_group_keys(
                    nodes.first().expect("resolved nodes are non-empty"),
                    &self.registry,
                    &sections,
                ),
                None => vec![None; sections.len()],
            };

            let mut accordion = Accordion::new("properties-accordion")
                .multiple(true)
                .small();
            for (index, section) in sections.into_iter().enumerate() {
                let fields = section.fields.clone();
                let title: SharedString = ravel_i18n::translate(&section.title).into();
                // A section with no fold identity is always open.
                let open = !param_group_keys
                    .get(index)
                    .and_then(Option::as_ref)
                    .is_some_and(|(type_key, group)| {
                        crate::panels::is_param_group_collapsed(type_key, group, cx)
                    });
                let scrubs = scrub_entities.clone();
                let strings = string_entities.clone();
                let selects = select_entities.clone();
                let colors = color_entities.clone();
                let curves = curve_entities.clone();
                let ramps = ramp_entities.clone();
                let expanded_rows = expanded_rows.clone();
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
                            &expanded_rows,
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
                            PropertyField::Curve { key, .. } if expanded_rows.contains(key) => {
                                curves.iter().find(|(k, _, _)| k == key).map(
                                    |(key, state, height)| {
                                        curve_editor_body(key, state, *height, &editor, muted)
                                    },
                                )
                            }
                            // A ramp row expands the same way, into the same
                            // slot, from the same shared expansion state.
                            PropertyField::Ramp { key, .. } if expanded_rows.contains(key) => ramps
                                .iter()
                                .find(|(k, ..)| k == key)
                                .map(|(key, state, picker, height, selected)| {
                                    ramp_editor_body(
                                        key, state, picker, *height, *selected, &editor, muted,
                                    )
                                }),
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
                    item.title(title.clone()).open(open).child(container)
                });
            }
            // The Accordion reports the whole open set rather than the header
            // that moved, and it fires on any click that bubbles out of the
            // list — including the property rows. So this writes each fold
            // through `set_param_group_collapsed`, which returns whether it
            // changed anything, and only a real change repaints.
            accordion = accordion.on_toggle_click({
                let keys = param_group_keys.clone();
                let editor = editor.clone();
                move |open, _window, cx| {
                    editor
                        .update(cx, |this, cx| this.apply_param_group_folds(&keys, open, cx))
                        .ok();
                }
            });
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
    use ravel_core::id::{DataTypeId, EdgeId, LayerId, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::param_curve::CurveParam;
    use ravel_ui::properties::layer::{PARENT_NONE, parse_parent_option};

    /// The key toggle appears on `Int` and `String` rows — those parameters
    /// are animatable now — and on the animated spellings of both, reporting
    /// whether a key sits at the frame.
    #[test]
    fn int_and_string_rows_offer_the_key_toggle() {
        use ravel_core::animation::{AnimationChannel, Interpolation, KeyframeCurve, StepCurve};
        let mut curve = KeyframeCurve::new();
        curve.insert(4, 6.0, Interpolation::Linear);
        let mut steps = StepCurve::new("a".to_string());
        steps.insert(4, "b".to_string());
        let node = Node::new(NodeId::new(1), "shape.polygon")
            .with_param("sides", ParameterValue::Int(6))
            .with_param("label", ParameterValue::String("hi".into()))
            .with_param(
                "keyed_sides",
                ParameterValue::IntChannel(AnimationChannel::keyframes(curve)),
            )
            .with_param("keyed_label", ParameterValue::StringSteps(steps));

        assert_eq!(node_param_keyed(&node, "sides", Some(4)), Some(false));
        assert_eq!(node_param_keyed(&node, "label", Some(4)), Some(false));
        assert_eq!(node_param_keyed(&node, "keyed_sides", Some(4)), Some(true));
        assert_eq!(node_param_keyed(&node, "keyed_sides", Some(5)), Some(false));
        assert_eq!(node_param_keyed(&node, "keyed_label", Some(4)), Some(true));
        assert_eq!(node_param_keyed(&node, "keyed_label", Some(5)), Some(false));
        // Without a local frame, any key at all counts as keyed.
        assert_eq!(node_param_keyed(&node, "keyed_label", None), Some(true));
    }

    /// An identifier parameter carries no toggle whatever its kind: the row
    /// must not offer to animate a reference (`layer.ref`'s `layer`, `precomp`'s
    /// `comp_id`). A same-named parameter on any other node type is unaffected.
    #[test]
    fn identifier_rows_carry_no_key_toggle() {
        let layer_ref = Node::new(NodeId::new(1), "layer.ref")
            .with_param("layer", ParameterValue::Int(3))
            .with_param("port", ParameterValue::String("frame".into()));
        assert_eq!(node_param_keyed(&layer_ref, "layer", Some(0)), None);
        assert_eq!(
            node_param_keyed(&layer_ref, "port", Some(0)),
            Some(false),
            "only the identifier is excluded, not the whole node"
        );

        let precomp =
            Node::new(NodeId::new(2), "precomp").with_param("comp_id", ParameterValue::Int(1));
        assert_eq!(node_param_keyed(&precomp, "comp_id", Some(0)), None);

        let unrelated =
            Node::new(NodeId::new(3), "scatter.grid").with_param("layer", ParameterValue::Int(3));
        assert_eq!(
            node_param_keyed(&unrelated, "layer", Some(0)),
            Some(false),
            "the key alone does not make a parameter an identifier"
        );
    }

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

    /// Red component of the layer's custom `tint` channel at frame 0.
    fn tint_red(layer: &Layer) -> f32 {
        let eval = ravel_core::eval::EvalContext::new(
            0,
            ravel_core::types::FrameRate::new(30, 1),
            (16, 16),
        );
        let ParameterValue::Channel4(channels) = &net::find_in_node(&layer.network)
            .unwrap()
            .parameters
            .iter()
            .find(|p| p.key == "tint")
            .unwrap()
            .value
        else {
            panic!("expected Channel4");
        };
        channels[0].evaluate(0.0, &eval)
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

    /// Apply a graph change that makes a scalar parameter driven without
    /// adding a separate undo step. This models an external graph update
    /// arriving while Properties still owns the old row's gesture.
    fn document_with_driven_amount(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        cx: &mut TestAppContext,
    ) -> Document {
        let document = project.read_with(cx, |project, _| project.document().clone());
        let graph = resolve_network(&document, path).expect("network").clone();
        let graph = graph
            .expose_param_port(node_id, "amount")
            .expect("amount can be exposed");
        let target_port = graph
            .node(node_id)
            .and_then(|node| node.param_port_index("amount"))
            .expect("the exposed amount port");
        let source_id = NodeId::next();
        let graph = graph
            .add_node(Node::new(source_id, "test").with_output("value", DataTypeId::SCALAR))
            .expect("source node");
        let graph = graph
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                node_id,
                target_port,
            )
            .expect("source drives amount");
        ravel_ui::document::replace_network(&document, path, graph).expect("replace network")
    }

    fn apply_document(project: &Entity<ProjectState>, document: Document, cx: &mut TestAppContext) {
        project.update(cx, |project, cx| {
            project.apply_document(document, InvalidationHint::Structural, cx);
        });
    }

    fn document_without_node(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        cx: &mut TestAppContext,
    ) -> Document {
        let document = project.read_with(cx, |project, _| project.document().clone());
        let graph = resolve_network(&document, path).expect("network").clone();
        let graph = graph.remove_node(node_id).expect("node exists");
        ravel_ui::document::replace_network(&document, path, graph).expect("replace network")
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
                layer.audio = Some(AudioSource::new(ravel_core::id::AssetId::next(), 0));
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
        use ravel_core::id::AssetId;

        let (window, project, comp_id, lid) = setup(cx);
        let clip = AssetId::new(1);
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
                .with_media_asset_entry(clip, entry);
            let doc = update_layer(&doc, comp_id, lid, |layer| {
                layer.audio = Some(AudioSource::new(clip, 1));
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

    /// The other half of the same discipline: a pending color commit is not
    /// dropped when the slot carrying it goes away.
    ///
    /// The picker has no gesture-end event, so the commit is debounced. A
    /// target switch inside the quiet window used to clear the slot — the
    /// live value was already in the document, so the color survived with no
    /// undo step of its own and folded into whatever the user did next.
    #[gpui::test]
    fn a_color_commit_survives_a_target_switch_inside_the_quiet_window(cx: &mut TestAppContext) {
        let (window, project, comp_id, lid) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_color_change(
                    "custom.tint",
                    PropertyValue::Color {
                        r: 0.25,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                    &[],
                    cx,
                );
            })
            .unwrap();
        assert!((tint_red(&layer(&project, comp_id, lid, cx)) - 0.25).abs() < 1e-6);

        // Well inside the quiet window, the user selects something else.
        cx.update(|cx| {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Composition {
                comp_id,
            }));
        });
        cx.run_until_parked();
        window
            .read_with(cx, |panel, _| {
                assert!(
                    panel.pending_color_commit.is_none(),
                    "the switch consumed the pending commit"
                );
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            (tint_red(&layer(&project, comp_id, lid, cx)) - 1.0).abs() < 1e-6,
            "one undo returns the pre-gesture color"
        );
        // Only a committed step can be redone.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!((tint_red(&layer(&project, comp_id, lid, cx)) - 0.25).abs() < 1e-6);
    }

    /// The same slot, reused rather than cleared: a second color gesture on a
    /// *different* row inside the quiet window commits the first one instead
    /// of overwriting it. Reusing the slot for the same row stays a
    /// supersede — that is the debounce merging one gesture's ticks.
    #[gpui::test]
    fn a_second_color_gesture_on_another_row_commits_the_first(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::AnimationChannel;
        let color = |r: f32| {
            ParameterValue::Channel4([
                AnimationChannel::constant(r),
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(1.0),
            ])
        };
        let node = Node::new(NodeId::next(), "test")
            .with_param("tint", color(1.0))
            .with_param("rim", color(1.0));
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, node);

        let red = |key: &str, cx: &mut TestAppContext| {
            let ParameterValue::Channel4(channels) =
                node_parameter(&project, &path, node_id, key, cx)
            else {
                panic!("{key} stays a color channel");
            };
            let ChannelSource::Constant(value) = channels[0].source else {
                panic!("{key} stays constant");
            };
            value
        };

        window
            .update(cx, |panel, _window, cx| {
                panel.apply_color_change(
                    "tint",
                    PropertyValue::Color {
                        r: 0.25,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                    &[node_id],
                    cx,
                );
                // No clock advance: the first commit is still pending.
                panel.apply_color_change(
                    "rim",
                    PropertyValue::Color {
                        r: 0.5,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                    &[node_id],
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(COLOR_COMMIT_QUIET * 2);
        cx.run_until_parked();

        assert!((red("tint", cx) - 0.25).abs() < 1e-6);
        assert!((red("rim", cx) - 0.5).abs() < 1e-6);

        // Two gestures, two undo steps — the first was not swallowed.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!((red("rim", cx) - 1.0).abs() < 1e-6);
        assert!(
            (red("tint", cx) - 0.25).abs() < 1e-6,
            "the first edit stands"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!((red("tint", cx) - 1.0).abs() < 1e-6);
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

    /// A rebuild asked for *during* a scrub must wait for the gesture to end.
    ///
    /// Scrubbing an expression-driven parameter collapses the channel to a
    /// plain float on the first live `Change`, so the row's shape changes and
    /// `refresh_values_checked` asks for new widgets. Rebuilding there would
    /// drop the `ScrubBinding` — and with it the subscription the
    /// gesture-ending `Commit` travels on — leaving the live value in the
    /// document with no undo step in front of it, so Undo jumps to whatever
    /// the user did before the scrub.
    #[gpui::test]
    fn a_scrub_survives_a_rebuild_requested_mid_gesture(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());
        window
            .update(cx, |panel, window, cx| panel.rebuild_widgets(window, cx))
            .unwrap();

        let scrub = window
            .read_with(cx, |panel, _| {
                panel
                    .scrubs
                    .iter()
                    .find(|(key, _)| key == "amount")
                    .map(|(_, binding)| binding.state.clone())
                    .expect("a scrub widget for the amount row")
            })
            .unwrap();

        // Drag: the live `Change` writes the document without recording undo.
        scrub.update(cx, |state, cx| {
            state.begin_drag(0.0);
            state.drag_to(120.0, &gpui::Modifiers::default(), cx);
        });
        cx.run_until_parked();
        let scrubbed = scrub.read_with(cx, |state, _| state.value());
        assert_ne!(scrubbed, 1.0, "the drag moved the value");
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            ParameterValue::Float(scrubbed),
            "the live change collapsed the expression channel"
        );

        // The shape change asked for a rebuild, and the frame that ran above
        // (the panel notified, so it drew) had to leave it pending: the
        // rebuild is where the widgets — and the subscription carrying the
        // commit — would be dropped.
        window
            .read_with(cx, |panel, cx| {
                assert!(panel.gesture_in_flight(cx));
                assert!(
                    panel.needs_rebuild,
                    "the rebuild is requested and still waiting for the gesture"
                );
                assert!(
                    panel.scrubs.iter().any(|(key, binding)| key == "amount"
                        && binding.state.entity_id() == scrub.entity_id()),
                    "the widget carrying the pending commit is still bound"
                );
            })
            .unwrap();

        // Release: the `Commit` reaches the panel and records the undo step.
        scrub.update(cx, |state, cx| {
            state.end_drag(cx);
        });
        cx.run_until_parked();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let restored = node_parameter(&project, &path, node_id, "amount", cx);
        assert!(
            expression::component_expression(&restored, 0).is_some(),
            "one undo returns the pre-scrub expression, got {restored:?}"
        );
        // Only a *committed* step can be redone: undoing an uncommitted live
        // preview also returns true but leaves nothing behind it.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            ParameterValue::Float(scrubbed)
        );
    }

    /// If an external graph update drives the parameter while its scrub row
    /// is still dragging, the editable row disappears. The panel must end the
    /// old gesture before rebuilding and record exactly one gesture step.
    #[gpui::test]
    fn a_scrub_ends_before_rebuild_when_its_row_becomes_driven(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) =
            setup_target_for_node(cx, expression_node());
        let before = node_parameter(&project, &path, node_id, "amount", cx);
        let scrub = window
            .read_with(cx, |panel, _| {
                panel
                    .scrubs
                    .iter()
                    .find(|(key, _)| key == "amount")
                    .expect("amount scrub")
                    .1
                    .state
                    .clone()
            })
            .unwrap();
        scrub.update(cx, |state, cx| {
            state.begin_drag(0.0);
            state.drag_to(100.0, &gpui::Modifiers::default(), cx);
        });
        cx.run_until_parked();

        apply_document(
            &project,
            document_with_driven_amount(&project, &path, node_id, cx),
            cx,
        );
        cx.run_until_parked();

        window
            .read_with(cx, |panel, cx| {
                assert!(!panel.gesture_in_flight(cx));
                assert!(!panel.needs_rebuild, "the rebuild was not left pending");
                assert!(
                    panel.scrubs.iter().all(|(key, _)| key != "amount"),
                    "the stale editable binding was dropped"
                );
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_parameter(&project, &path, node_id, "amount", cx),
            before,
            "the forced gesture end took exactly one undo step"
        );
    }

    /// The other side of the same rule: keeping a widget alive across a
    /// *target* switch would aim it at the wrong row.
    ///
    /// A `Commit` travels on a subscription and GPUI delivers it after the
    /// callback that switched the target has returned, and a `ScrubBinding`
    /// carries no target of its own — so a scrub of layer A, released after
    /// layer B was selected, would route through `self.target` and silently
    /// rewrite a value on B. The switch ends the gesture on A instead.
    #[gpui::test]
    fn a_scrub_in_flight_commits_on_the_target_it_started_on(cx: &mut TestAppContext) {
        let (window, project, comp_id, a) = setup(cx);
        let b = project.update(cx, |project, cx| {
            let b = LayerId::next();
            let layer = Layer::new(b, "B", network_with_custom_param()).with_time(0, 0, 300);
            let doc = ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            b
        });
        let position_x = |lid: LayerId, cx: &mut TestAppContext| {
            let eval = ravel_core::eval::EvalContext::new(
                0,
                ravel_core::types::FrameRate::new(30, 1),
                (16, 16),
            );
            layer(&project, comp_id, lid, cx).transform.position[0].evaluate(0.0, &eval)
        };

        window
            .update(cx, |panel, window, cx| panel.rebuild_widgets(window, cx))
            .unwrap();
        let scrub = window
            .read_with(cx, |panel, _| {
                panel
                    .scrubs
                    .iter()
                    .find(|(key, _)| key == "position_x")
                    .map(|(_, binding)| binding.state.clone())
                    .expect("a scrub widget for the layer's position")
            })
            .unwrap();

        scrub.update(cx, |state, cx| {
            state.begin_drag(0.0);
            state.drag_to(120.0, &gpui::Modifiers::default(), cx);
        });
        cx.run_until_parked();
        let scrubbed = scrub.read_with(cx, |state, _| state.value());
        assert_ne!(scrubbed, 0.0, "the drag moved the value");

        // The user selects another layer with the pointer still down.
        cx.update(|cx| {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Layer {
                comp_id,
                layer_id: b,
            }));
        });
        cx.run_until_parked();
        // The pointer comes up on a widget the panel no longer owns.
        scrub.update(cx, |state, cx| {
            state.end_drag(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            position_x(b, cx),
            0.0,
            "the layer selected mid-drag was never scrubbed"
        );
        assert!(
            (position_x(a, cx) - scrubbed).abs() < 1e-6,
            "the scrubbed value stayed on the layer it was scrubbed on"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            position_x(a, cx),
            0.0,
            "one undo returns the pre-scrub value"
        );
        // Only a committed step can be redone.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!((position_x(a, cx) - scrubbed).abs() < 1e-6);
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

    /// Publish which panel instances are at the front of a tab area, the way
    /// `WindowHost::show_tree` does.
    fn set_visible(instances: &[ravel_ui::layout::PanelInstanceId], cx: &mut TestAppContext) {
        let visible = instances.iter().copied().collect();
        cx.update(|cx| cx.set_global(crate::panels::VisiblePanels(visible)));
        cx.run_until_parked();
    }

    /// The visibility gate delays work, it does not drop it (`MED-UI-02`): an
    /// edit made while the panel sits behind another tab is not resolved
    /// there, and *is* resolved the moment the tab comes back.
    ///
    /// Both halves are one test because the second is only meaningful after
    /// the first: a panel that never skipped anything has nothing to catch up
    /// on, so a passing catch-up assertion would prove nothing on its own.
    #[gpui::test]
    fn a_hidden_panel_catches_up_when_it_returns(cx: &mut TestAppContext) {
        let panel_instance = ravel_ui::layout::PanelInstanceId(0);
        let (window, project, comp_id, lid) = setup(cx);
        window
            .update(cx, |panel, _window, cx| panel.refresh_values(cx))
            .unwrap();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(displayed_float(panel, "position_x"), Some(0.0));
            })
            .unwrap();

        // Behind another tab: nothing is published for this instance.
        set_visible(&[], cx);
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, lid, |l| {
                l.transform.position[0] = AnimationChannel::constant(42.0);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    displayed_float(panel, "position_x"),
                    Some(0.0),
                    "a hidden panel must not resolve the edit"
                );
            })
            .unwrap();

        // Back at the front. The skipped edit is owed, and this is where it is
        // paid: the epoch was deliberately left unrecorded while hidden.
        set_visible(&[panel_instance], cx);
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    displayed_float(panel, "position_x"),
                    Some(42.0),
                    "returning to the front must show the edit made while hidden"
                );
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

    /// A folded parameter group is remembered under `(type_key, group)` — not
    /// per node — and unfolding forgets it again. The project save path reads
    /// the same global into `ui_state.json`, which is what carries the fold
    /// across a restart; the default (nothing recorded) is all-expanded.
    ///
    /// Sections that are not parameter groups (the info section here) are
    /// never recorded: they have no fold identity, so a click that reports
    /// them closed must not put anything in the set.
    #[gpui::test]
    fn folding_a_parameter_group_records_it_by_node_type(cx: &mut TestAppContext) {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let node = registry
            .create_node("scatter.grid", NodeId::next())
            .expect("scatter.grid is registered");
        let (window, _editor, _project, _path, _node_id) = setup_target_for_node(cx, node.clone());

        window
            .update(cx, |panel, _window, cx| {
                let groups = ravel_ui::properties::node::param_group_titles(&node, &panel.registry);
                assert_eq!(
                    groups.iter().map(|(g, _)| g.as_str()).collect::<Vec<_>>(),
                    vec!["layout", "source"],
                    "scatter.grid declares both groups and leaves nothing ungrouped"
                );
                // The fold identity of each section, from the same helper
                // `render` uses. Only the parameter sections have one.
                let keys = param_group_keys(&node, &panel.registry, &panel.sections);
                assert_eq!(
                    keys.iter().filter(|key| key.is_some()).count(),
                    2,
                    "the info and ports sections are not foldable"
                );
                let source = keys
                    .iter()
                    .position(|key| key.as_ref().is_some_and(|(_, g)| g == "source"))
                    .expect("the Source section is foldable");

                assert!(
                    crate::panels::collapsed_param_groups(cx).is_empty(),
                    "nothing is folded before the first click"
                );

                // Every section open but Source — and the info section
                // reported closed too, which must record nothing.
                let open: Vec<usize> = (0..keys.len()).filter(|i| *i != source).collect();
                panel.apply_param_group_folds(&keys, &open, cx);
                assert!(crate::panels::is_param_group_collapsed(
                    "scatter.grid",
                    "source",
                    cx
                ));
                assert!(!crate::panels::is_param_group_collapsed(
                    "scatter.grid",
                    "layout",
                    cx
                ));
                assert_eq!(
                    crate::panels::collapsed_param_groups(cx).len(),
                    1,
                    "only the parameter group is recorded"
                );

                // Unfolding it forgets the entry rather than storing "open".
                let open: Vec<usize> = (0..keys.len()).collect();
                panel.apply_param_group_folds(&keys, &open, cx);
                assert!(crate::panels::collapsed_param_groups(cx).is_empty());
            })
            .unwrap();
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

                assert!(!panel.is_row_expanded("points"));
                panel.toggle_row_expanded("points", cx);
                panel.toggle_row_expanded("shape", cx);
                assert!(panel.is_row_expanded("points"));
                assert!(
                    panel.is_row_expanded("shape"),
                    "expanding one row must not collapse the other"
                );

                panel.toggle_row_expanded("points", cx);
                assert!(!panel.is_row_expanded("points"));
                assert!(panel.is_row_expanded("shape"));
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
                panel.toggle_row_expanded("points", cx);
                panel.toggle_row_expanded("shape", cx);
                panel.toggle_row_expanded("shape", cx);
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
                panel.toggle_row_expanded("points", cx)
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
                assert!(panel.is_row_expanded("points"));
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

    /// A curve drag is an edit gesture like any other: the target switch ends
    /// it on the row it started on instead of letting its `Commit` land after
    /// the panel has moved on (and, before that, instead of dropping it).
    #[gpui::test]
    fn a_curve_drag_commits_on_the_target_it_started_on(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        let original = node_curve(&project, &path, node_id, "points", cx).expect("curve");
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_row_expanded("points", cx)
            })
            .unwrap();

        let state = curve_editor_state(&window, "points", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });
        let start = curve_widget_pos(&state, 0.5, 0.5, cx);
        let end = curve_widget_pos(&state, 0.5, 0.9, cx);
        state.update(cx, |state, cx| {
            state.pointer_down(start, 1, cx);
            state.drag_to(end, cx);
        });
        cx.run_until_parked();
        window
            .read_with(cx, |panel, cx| assert!(panel.gesture_in_flight(cx)))
            .unwrap();

        // Another target is selected with the pointer still down.
        cx.update(|cx| {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Composition {
                comp_id: path.comp,
            }));
        });
        cx.run_until_parked();
        state.update(cx, |state, cx| {
            state.end_drag(cx);
        });
        cx.run_until_parked();

        let curve = |cx: &mut TestAppContext| {
            node_curve(&project, &path, node_id, "points", cx).expect("curve")
        };
        assert!(
            (curve(cx).evaluate(0.5) - 0.9).abs() < 1e-3,
            "{:?}",
            curve(cx)
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(curve(cx), original);
        // Only a committed step can be redone.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!((curve(cx).evaluate(0.5) - 0.9).abs() < 1e-3);
    }

    /// A curve row removed while its point is being dragged cannot deliver a
    /// release. The panel ends the old editor before rebuilding and keeps one
    /// undo step for the gesture.
    #[gpui::test]
    fn a_curve_drag_ends_before_rebuild_when_its_row_disappears(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, curve_node());
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_row_expanded("points", cx)
            })
            .unwrap();
        let state = curve_editor_state(&window, "points", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), CURVE_TEST_SIZE)
        });
        let start = curve_widget_pos(&state, 0.5, 0.5, cx);
        let end = curve_widget_pos(&state, 0.5, 0.8, cx);
        state.update(cx, |state, cx| {
            state.pointer_down(start, 1, cx);
            state.drag_to(end, cx);
        });
        cx.run_until_parked();

        apply_document(
            &project,
            document_without_node(&project, &path, node_id, cx),
            cx,
        );
        cx.run_until_parked();

        window
            .read_with(cx, |panel, cx| {
                assert!(!panel.gesture_in_flight(cx));
                assert!(!panel.needs_rebuild);
                assert!(panel.curves.is_empty(), "the removed editor was dropped");
            })
            .unwrap();
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(node_curve(&project, &path, node_id, "points", cx).is_some());
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
                panel.toggle_row_expanded("points", cx);
                assert!(panel.is_row_expanded("points"));
            })
            .unwrap();

        // Selecting the layer instead, then this node again.
        cx.update(|cx| cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty)));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(!panel.is_row_expanded("points"));
            })
            .unwrap();
        cx.update(|cx| cx.set_global(SelectedPropertiesTarget(target)));
        window
            .update(cx, |panel, window, cx| {
                panel.rebuild_widgets(window, cx);
                assert!(
                    !panel.is_row_expanded("points"),
                    "returning to the node shows the row collapsed"
                );
                assert_eq!(panel.curves.len(), 2, "the editors are rebuilt");
            })
            .unwrap();
    }

    // ----- inline gradient editor (properties parameter-editor plan, unit 4)

    /// A node with a curve *and* a ramp, so the two row kinds can be shown to
    /// share one expansion behaviour.
    fn ramp_node() -> Node {
        use ravel_core::param_ramp::RampParam;
        Node::new(NodeId::next(), "test")
            .with_param(
                "points",
                ParameterValue::Curve(CurveParam::linear([(0.0, 0.0), (1.0, 1.0)])),
            )
            .with_param(
                "stops",
                ParameterValue::Ramp(RampParam::linear([
                    (0.0, ravel_core::types::Color::BLACK),
                    (1.0, ravel_core::types::Color::WHITE),
                ])),
            )
    }

    /// Widget size the headless ramp gestures below are expressed in: 200 px
    /// wide, so one ramp position unit is 200 px.
    const RAMP_TEST_SIZE: (f32, f32) = (200.0, 60.0);

    fn ramp_editor_state(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Entity<ParamRampEditorState> {
        window
            .update(cx, |panel, _window, _cx| {
                panel
                    .ramps
                    .iter()
                    .find(|(k, _)| k == key)
                    .unwrap_or_else(|| panic!("{key} has no ramp editor"))
                    .1
                    .state
                    .clone()
            })
            .unwrap()
    }

    fn ramp_picker(
        window: &gpui::WindowHandle<PropertiesGpuiPanel>,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Entity<ColorPickerState> {
        window
            .update(cx, |panel, _window, _cx| {
                panel
                    .ramps
                    .iter()
                    .find(|(k, _)| k == key)
                    .expect("ramp binding")
                    .1
                    .picker
                    .clone()
            })
            .unwrap()
    }

    /// The stored ramp of a node parameter, or `None` once the node is gone.
    fn node_ramp(
        project: &Entity<ProjectState>,
        path: &ravel_ui::document::NetworkPath,
        node_id: NodeId,
        key: &str,
        cx: &mut TestAppContext,
    ) -> Option<ravel_core::param_ramp::RampParam> {
        project.read_with(cx, |project, _| {
            resolve_network(project.document(), path)
                .and_then(|graph| graph.node(node_id))
                .and_then(|node| node.parameters.iter().find(|param| param.key == key))
                .and_then(|param| match &param.value {
                    ParameterValue::Ramp(ramp) => Some(ramp.clone()),
                    _ => None,
                })
        })
    }

    /// A ramp parameter reaches the panel as a ramp row with an editor bound
    /// to it, and it expands through the *same* state a curve row uses: both
    /// can be open at once, and neither closes the other. This is the unit 4
    /// completion criterion "the expansion behaviour matches unit 2" — they
    /// match because there is one implementation, not two.
    #[gpui::test]
    fn ramp_rows_expand_through_the_same_state_as_curve_rows(cx: &mut TestAppContext) {
        let (window, _editor, _project, _path, _node_id) = setup_target_for_node(cx, ramp_node());

        window
            .update(cx, |panel, _window, cx| {
                let kinds: Vec<&str> = panel
                    .sections
                    .iter()
                    .flat_map(|section| &section.fields)
                    .filter_map(|field| match field {
                        PropertyField::Curve { key, .. } | PropertyField::Ramp { key, .. } => {
                            Some(key.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(kinds, vec!["points", "stops"]);
                assert_eq!(panel.ramps.len(), 1, "one editor per ramp row");

                assert!(!panel.is_row_expanded("stops"));
                panel.toggle_row_expanded("stops", cx);
                panel.toggle_row_expanded("points", cx);
                assert!(panel.is_row_expanded("stops"));
                assert!(
                    panel.is_row_expanded("points"),
                    "expanding a ramp must not collapse a curve"
                );

                // The height drag is the same state too.
                assert_eq!(panel.row_height("stops"), INLINE_EDITOR_HEIGHT);
                panel.begin_row_resize("stops".to_string(), 0.0);
                panel.row_resize_to(40.0, cx);
                panel.end_row_resize();
                assert_eq!(panel.row_height("stops"), INLINE_EDITOR_HEIGHT + 40.0);
                assert_eq!(
                    panel.row_height("points"),
                    INLINE_EDITOR_HEIGHT,
                    "each row keeps its own height"
                );

                panel.toggle_row_expanded("stops", cx);
                assert!(!panel.is_row_expanded("stops"));
                assert!(panel.is_row_expanded("points"));
            })
            .unwrap();
    }

    /// Losing the left button during an inline-editor resize clears the
    /// resize state, so later drag moves cannot keep changing its height.
    #[gpui::test]
    fn losing_the_button_ends_an_inline_editor_resize(cx: &mut TestAppContext) {
        let (window, _editor, _project, _path, _node_id) = setup_target_for_node(cx, ramp_node());
        window
            .update(cx, |panel, _window, cx| {
                panel.begin_row_resize("stops".into(), 10.0);
                panel.row_resize_to(50.0, cx);
                let height = panel.row_height("stops");
                assert!(panel.row_resize.is_some());

                panel.end_row_resize_without_pointer();
                assert!(panel.row_resize.is_none());
                panel.row_resize_to(500.0, cx);
                assert_eq!(panel.row_height("stops"), height);
            })
            .unwrap();
    }

    /// Expanding and collapsing a ramp row is view state: it changes no value
    /// and pushes nothing onto the undo stack.
    #[gpui::test]
    fn expanding_a_ramp_row_changes_no_value_and_records_no_undo_step(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let before = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp parameter");

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_row_expanded("stops", cx);
                panel.toggle_row_expanded("stops", cx);
                panel.toggle_row_expanded("stops", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx).as_ref(),
            Some(&before),
            "expansion must not touch the value"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            node_ramp(&project, &path, node_id, "stops", cx).is_none(),
            "the first undo reached the node's own commit, so no expansion \
             step was pushed in between"
        );
    }

    /// Dragging a stop applies live and commits once: one gesture, one
    /// Document undo step — and the stop never leaves the `0..=1` band.
    #[gpui::test]
    fn dragging_a_ramp_stop_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let original = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_row_expanded("stops", cx)
            })
            .unwrap();

        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });

        // Grab the stop at 0 and drag it right, then far past the band's end.
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 1, cx);
            state.drag_to(40.0, cx);
        });
        cx.run_until_parked();
        let live = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert!(
            (live.stops()[0].position - 0.2).abs() < 1e-3,
            "the live drag applies to the document: {live:?}"
        );

        state.update(cx, |state, cx| {
            state.drag_to(10_000.0, cx);
            state.end_drag(cx);
        });
        cx.run_until_parked();
        let committed = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert_eq!(committed.len(), 2, "no stop was merged away");
        assert!(
            committed
                .stops()
                .iter()
                .all(|stop| (0.0..=1.0).contains(&stop.position)),
            "a stop cannot leave the band: {committed:?}"
        );

        // One undo for the whole gesture, and it really committed (only a
        // committed step can be redone).
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx).as_ref(),
            Some(&original)
        );
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.is_row_expanded("stops"), "undo left the view alone");
            })
            .unwrap();
        project.update(cx, |project, cx| assert!(project.redo(cx)));
    }

    /// A ramp drag is an edit gesture like any other: the target switch ends
    /// it on the row it started on rather than dropping its undo step.
    #[gpui::test]
    fn a_ramp_drag_commits_on_the_target_it_started_on(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let original = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 1, cx);
            state.drag_to(60.0, cx);
        });
        cx.run_until_parked();
        window
            .read_with(cx, |panel, cx| assert!(panel.gesture_in_flight(cx)))
            .unwrap();

        cx.update(|cx| {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Composition {
                comp_id: path.comp,
            }));
        });
        cx.run_until_parked();

        let moved = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert!((moved.stops()[0].position - 0.3).abs() < 1e-3, "{moved:?}");
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx).as_ref(),
            Some(&original)
        );
        // Only a committed step can be redone.
        project.update(cx, |project, cx| assert!(project.redo(cx)));
    }

    /// A ramp row removed while a stop is being dragged cannot deliver a
    /// release. The panel ends the old editor before rebuilding and keeps one
    /// undo step for the gesture.
    #[gpui::test]
    fn a_ramp_drag_ends_before_rebuild_when_its_row_disappears(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 1, cx);
            state.drag_to(60.0, cx);
        });
        cx.run_until_parked();

        apply_document(
            &project,
            document_without_node(&project, &path, node_id, cx),
            cx,
        );
        cx.run_until_parked();

        window
            .read_with(cx, |panel, cx| {
                assert!(!panel.gesture_in_flight(cx));
                assert!(!panel.needs_rebuild);
                assert!(panel.ramps.is_empty(), "the removed editor was dropped");
            })
            .unwrap();
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(node_ramp(&project, &path, node_id, "stops", cx).is_some());
    }

    /// Adding and removing a stop each reach the document as their own undo
    /// step, and the last stop is never removed.
    #[gpui::test]
    fn adding_and_removing_ramp_stops_reach_the_document(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .len(),
            2
        );

        // A double-click on empty band adds a stop where the pointer is.
        state.update(cx, |state, cx| state.pointer_down(100.0, 2, cx));
        cx.run_until_parked();
        let added = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert_eq!(added.len(), 3);
        assert!((added.stops()[1].position - 0.5).abs() < 1e-3, "{added:?}");

        // A double-click on that stop removes it again.
        state.update(cx, |state, cx| state.pointer_down(100.0, 2, cx));
        cx.run_until_parked();
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .len(),
            2
        );

        // Two edits, two undo steps.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .len(),
            3
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .len(),
            2
        );
    }

    /// The floor is one stop, not two: a one-stop ramp is a legitimate flat
    /// colour, but a ramp with none has no colour to evaluate.
    #[gpui::test]
    fn the_last_ramp_stop_cannot_be_removed(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });

        state.update(cx, |state, cx| state.pointer_down(0.0, 2, cx));
        cx.run_until_parked();
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .len(),
            1
        );

        state.update(cx, |state, cx| state.pointer_down(200.0, 2, cx));
        cx.run_until_parked();
        let ramp = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert_eq!(ramp.len(), 1, "the last stop stays: {ramp:?}");
        assert_eq!(
            ramp.evaluate(0.5),
            ravel_core::types::Color::WHITE,
            "and it still answers everywhere"
        );
    }

    /// The picker beside the editor recolours the *selected* stop and reaches
    /// the document through the same debounced commit a Color row uses: one
    /// gesture, one undo step.
    #[gpui::test]
    fn recolouring_the_selected_stop_reaches_the_document_once(cx: &mut TestAppContext) {
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let original = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        let state = ramp_editor_state(&window, "stops", cx);
        state.read_with(cx, |state, _| {
            state.set_bounds_for_tests((0.0, 0.0), RAMP_TEST_SIZE)
        });
        let picker = ramp_picker(&window, "stops", cx);

        // Nothing is selected yet, so a picker change edits nothing.
        picker.update(cx, |_, cx| {
            cx.emit(ColorPickerEvent::Change(Some(gpui::red())));
        });
        cx.run_until_parked();
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx).as_ref(),
            Some(&original),
            "a picker with no selected stop edits nothing"
        );

        // Select the stop at 1.0 (white) and drive the picker.
        state.update(cx, |state, cx| {
            state.pointer_down(200.0, 1, cx);
            state.end_drag(cx);
        });
        // Mid grey, not a primary: `r > b` holds for pure red whether or not
        // the display encoding is undone, so a primary cannot tell a working
        // conversion from a missing one. Half-way in display light is about
        // 0.21 linear, and the stored stop is linear (`CM-2`).
        let picked: Hsla = gpui::rgb(0x808080).into();
        let rgba = Rgba::from(picked);
        let expected = ColorSpace::DISPLAY.to_linear([rgba.r, rgba.g, rgba.b])[0];
        for _ in 0..3 {
            picker.update(cx, |_, cx| {
                cx.emit(ColorPickerEvent::Change(Some(picked)));
            });
        }
        cx.run_until_parked();
        let live = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert!(
            (live.evaluate(1.0).r - expected).abs() < 1e-3,
            "the live change applied in linear light: {live:?} vs {expected}"
        );
        assert_eq!(live.evaluate(0.0), original.evaluate(0.0), "only one stop");

        cx.executor().advance_clock(COLOR_COMMIT_QUIET * 2);
        cx.run_until_parked();
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx).as_ref(),
            Some(&original),
            "the whole picker gesture is one undo step"
        );
        project.update(cx, |project, cx| {
            assert!(project.redo(cx));
            assert!(!project.redo(cx), "there is no second step behind it");
        });
    }

    /// Switching the interpolation is one click, one undo step, and it keeps
    /// the stops it was switched over.
    #[gpui::test]
    fn switching_the_ramp_interpolation_reaches_the_document(cx: &mut TestAppContext) {
        use ravel_core::param_ramp::RampInterpolation;
        let (window, _editor, project, path, node_id) = setup_target_for_node(cx, ramp_node());
        let state = ramp_editor_state(&window, "stops", cx);
        state.update(cx, |state, cx| {
            state.set_interpolation(RampInterpolation::Constant, cx)
        });
        cx.run_until_parked();
        let edited = node_ramp(&project, &path, node_id, "stops", cx).expect("ramp");
        assert_eq!(edited.interpolation(), RampInterpolation::Constant);
        assert_eq!(edited.len(), 2);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            node_ramp(&project, &path, node_id, "stops", cx)
                .expect("ramp")
                .interpolation(),
            RampInterpolation::Linear
        );
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

    /// The Ports row's group cell assigns an In node's custom parameter to a
    /// Properties group, and the parameters split into sections accordingly
    /// (PGRP-4). Clearing the cell takes the parameter out of the group again.
    #[gpui::test]
    fn the_ports_group_cell_assigns_a_custom_parameter_to_a_section(cx: &mut TestAppContext) {
        let (properties, project, path, in_id) = setup_in_node_target(cx);

        let group_of = |properties: &gpui::WindowHandle<PropertiesGpuiPanel>,
                        cx: &mut TestAppContext,
                        name: &str| {
            properties
                .update(cx, |panel, _window, _cx| {
                    panel.port_row(name).and_then(|row| row.group.clone())
                })
                .unwrap()
        };
        let section_titles = |properties: &gpui::WindowHandle<PropertiesGpuiPanel>,
                              cx: &mut TestAppContext| {
            properties
                .update(cx, |panel, _window, _cx| {
                    panel
                        .sections
                        .iter()
                        .map(|section| section.title.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap()
        };

        assert_eq!(
            group_of(&properties, cx, "amount").as_deref(),
            Some(""),
            "a custom parameter starts in no group"
        );
        assert_eq!(
            group_of(&properties, cx, net::PORT_TIME),
            None,
            "a fixed port has no parameter, so no group cell"
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.set_port_group("amount", " Look ".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            project.read_with(cx, |project, _| {
                resolve_network(project.document(), &path)
                    .and_then(|graph| graph.node(in_id))
                    .and_then(|node| node.param_groups.get("amount").cloned())
            }),
            Some("Look".to_string()),
            "the graph holds the trimmed group name"
        );
        assert_eq!(
            properties
                .update(cx, |panel, _window, _cx| panel.port_error.clone())
                .unwrap(),
            None
        );
        assert_eq!(
            section_titles(&properties, cx),
            vec![
                "properties.section.node_info".to_string(),
                // `tint` is still ungrouped, so the leading section stays.
                "properties.section.parameters".to_string(),
                "Look".to_string(),
                "properties.section.ports".to_string(),
            ]
        );

        // Clearing the cell takes it back out; whitespace counts as empty.
        properties
            .update(cx, |panel, _window, cx| {
                panel.set_port_group("amount", "   ".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            section_titles(&properties, cx),
            vec![
                "properties.section.node_info".to_string(),
                "properties.section.parameters".to_string(),
                "properties.section.ports".to_string(),
            ]
        );
        assert_eq!(group_of(&properties, cx, "amount").as_deref(), Some(""));
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

    /// `MED-APP-26`: the icon is drawn as a checkbox, so a second click has to
    /// take the declaration back off rather than refuse.
    #[gpui::test]
    fn the_exposed_checkbox_declares_and_withdraws(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_names(&properties, cx), ["amount"]);

        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
                assert_eq!(
                    panel.exposed_error, None,
                    "withdrawing is the other half of the toggle, not a refusal"
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert!(declaration_names(&properties, cx).is_empty());
    }

    /// A hand-written `.ravprj` may bind one parameter twice — the core says
    /// so in `bound_to`. The checkbox is per parameter, so unchecking it has
    /// to clear both, or the click looks ignored.
    #[gpui::test]
    fn the_exposed_checkbox_withdraws_every_declaration_on_the_parameter(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        properties
            .update(cx, |panel, _window, cx| {
                panel.edit_declarations(cx, |declarations| {
                    let twin = ravel_core::exposed::ExposedParameter::inferred(
                        "amount_again",
                        ravel_core::exposed::ExposedValue::Float(1.0),
                        ExposedBinding::new(in_id, "amount"),
                    )?;
                    declarations.insert(twin).map(|()| true)
                });
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            declaration_names(&properties, cx),
            ["amount", "amount_again"]
        );

        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(declaration_names(&properties, cx).is_empty());
    }

    /// The toggle follows the declaration a rename moved, not the parameter
    /// key: withdrawing has to remove the row that is actually bound.
    #[gpui::test]
    fn the_exposed_checkbox_withdraws_a_renamed_declaration(cx: &mut TestAppContext) {
        let (properties, _project, _path, in_id) = setup_in_node_target(cx);
        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        properties
            .update(cx, |panel, _window, cx| {
                panel.rename_declaration("amount", "opacity".to_string(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(declaration_names(&properties, cx), ["opacity"]);

        properties
            .update(cx, |panel, _window, cx| {
                panel.toggle_exposed_parameter(in_id, "amount", cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(declaration_names(&properties, cx).is_empty());
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

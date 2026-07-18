// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Properties panel — GPUI view for inspecting and editing node parameters
//! and layer shell attributes.
//!
//! Node edits keep flowing through the legacy `PropertyChanged` global to
//! the node editor (which owns the network context). Layer targets edit the
//! document directly through [`ProjectState`]: shell attributes
//! (timing / transform / opacity / blend / adjustment) and the In node's
//! custom parameters (REQ-LAYER-002) map back via
//! `ravel_ui::properties::layer::apply_layer_field`, with the usual
//! scrub-gesture undo granularity (live `Change`s apply, the ending
//! `Commit` records one Document undo step).
//!
//! Animatable fields (shell transform/opacity channels, channel-backed
//! custom parameters, node `Float`/`Channel*` parameters) carry a small
//! ◆/◇ toggle left of their label that inserts or removes a keyframe at
//! the current layer-local frame (REQ-LAYER-004). Layer toggles edit the
//! document through [`ProjectState`]; node toggles route to the node
//! editor through the `NodeEditorHandle` global.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Sizable;
use gpui_component::accordion::Accordion;
use gpui_component::checkbox::Checkbox;
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{SelectEvent, SelectState};
use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::document::update_layer;
use ravel_ui::keyframes::layer_local_frame;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::layer::{
    CUSTOM_FIELD_PREFIX, apply_layer_field, in_node_id, layer_field_keyframed, sections_for_layer,
    toggle_layer_keyframe,
};
use ravel_ui::properties::node::sections_for_node;
use ravel_ui::properties::{PropertyField, PropertySection, PropertyValue};

use crate::project_state::ProjectState;
use crate::widgets::{ScrubEvent, ScrubInput, ScrubInputState};

use super::{PropertiesTarget, SelectedPropertiesTarget};

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

fn kv_row(key: &str, value: &str, muted: Hsla, fg: Hsla) -> Div {
    div()
        .flex()
        .justify_between()
        .items_center()
        .px_1()
        .py(px(1.0))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(key.to_string())),
        )
        .child(
            div()
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
        .px_1()
        .py(px(1.0))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(field_label(key))),
        );
    if let Some(entity) = scrub {
        row = row.child(div().min_w(px(64.0)).child(ScrubInput::new(entity)));
    } else {
        row = row.text_color(fg);
    }
    row
}

/// Synthetic scrub keys for the components of a `Vector` field
/// (`center#x`, `center#y`, ...).
fn vector_component_keys(key: &str, count: usize) -> Vec<String> {
    const SUFFIXES: [&str; 4] = ["x", "y", "z", "w"];
    (0..count.min(SUFFIXES.len()))
        .map(|i| format!("{key}#{}", SUFFIXES[i]))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_field_row(
    field: &PropertyField,
    scrubs: &[(String, Entity<ScrubInputState>)],
    strings: &[(String, Entity<InputState>)],
    selects: &[(String, Entity<SelectState<Vec<SharedString>>>)],
    colors: &[(String, Entity<ColorPickerState>)],
    editor: &WeakEntity<PropertiesGpuiPanel>,
    node_ids: &[NodeId],
    muted: Hsla,
    fg: Hsla,
) -> Div {
    match field {
        PropertyField::ReadOnly { key, value } => kv_row(&field_label(key), value, muted, fg),

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
                .px_1()
                .py(px(1.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(field_label(key))),
                )
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
            let mut row = div().flex().flex_col().px_1().py(px(1.0)).child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(field_label(key))),
            );
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(field_label(key))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(fg)
                            .child(SharedString::from(value.clone())),
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
                .px_1()
                .py(px(1.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(field_label(key))),
                );
            if let Some(entity) = picker {
                row = row.child(ColorPicker::new(entity).small());
            } else {
                row = row.child(
                    div()
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
                .px_1()
                .py(px(1.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(field_label(key))),
                );
            if entities.len() == components.len() {
                let mut cell = div().flex().gap_1();
                for entity in entities {
                    cell = cell.child(div().min_w(px(56.0)).child(ScrubInput::new(entity)));
                }
                row = row.child(cell);
            } else {
                let parts: Vec<String> = components.iter().map(|v| format!("{v:.3}")).collect();
                row = row.child(
                    div()
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
/// label: filled (accent) when a key sits at the current frame, hollow
/// (muted) otherwise.
fn key_toggle_button(
    key: &str,
    keyed: bool,
    target: &KeyTarget,
    accent: Hsla,
    muted: Hsla,
) -> Stateful<Div> {
    let (glyph, color) = if keyed {
        ("◆", accent)
    } else {
        ("◇", muted)
    };
    let button = div()
        .id(SharedString::from(format!("key-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .text_xs()
        .text_color(color)
        .cursor_pointer()
        .child(glyph);
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
    accent: Hsla,
    muted: Hsla,
) -> Stateful<Div> {
    let (glyph, color) = match state {
        PortToggleState::Unexposed => ("○", muted),
        PortToggleState::Exposed => ("◎", accent),
        PortToggleState::Connected => ("●", accent),
    };
    let key = key.to_string();
    div()
        .id(SharedString::from(format!("port-toggle-{key}")))
        .flex_shrink_0()
        .w(px(14.0))
        .text_xs()
        .text_color(color)
        .cursor_pointer()
        .child(glyph)
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
fn target_identity(target: &PropertiesTarget) -> Option<(Option<Vec<NodeId>>, Option<LayerId>)> {
    match target {
        PropertiesTarget::Empty => None,
        PropertiesTarget::Nodes { ids, .. } => Some((Some(ids.clone()), None)),
        PropertiesTarget::Layer { layer, .. } => Some((None, Some(layer.id))),
    }
}

pub struct PropertiesGpuiPanel {
    sections: Vec<PropertySection>,
    target: PropertiesTarget,
    project: Option<Entity<ProjectState>>,
    registry: NodeRegistry,
    scrubs: Vec<(String, ScrubBinding)>,
    strings: Vec<(String, StringBinding)>,
    selects: Vec<(String, SelectBinding)>,
    colors: Vec<(String, ColorBinding)>,
    /// Uncommitted color edit awaiting its debounced undo commit, with the
    /// generation guard that cancels superseded commits.
    pending_color_commit: Option<(String, PropertyValue)>,
    color_commit_generation: u64,
    needs_rebuild: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    playback_sub: Subscription,
}

impl PropertiesGpuiPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });

        let selection_sub = cx.observe_global::<SelectedPropertiesTarget>(|this: &mut Self, cx| {
            let target = cx
                .try_global::<SelectedPropertiesTarget>()
                .cloned()
                .unwrap_or_default();
            let same = target_identity(&this.target).is_some()
                && target_identity(&this.target) == target_identity(&target.0);
            this.target = target.0;
            if same {
                // Same target, new values (undo, timeline drag, playhead
                // move): refresh in place so scrub gestures survive.
                this.refresh_values(cx);
            } else {
                // A pending color commit must not land on the new target.
                this.pending_color_commit = None;
                this.color_commit_generation += 1;
                this.needs_rebuild = true;
            }
            cx.notify();
        });

        cx.observe_global::<super::PropertyChanged>(|this: &mut Self, cx| {
            if let Some(changed) = cx.try_global::<super::PropertyChanged>().cloned() {
                this.update_field_value(&changed.key, &changed.value);
                cx.notify();
            }
        })
        .detach();

        // Node-target sections sample animated channels at the playhead's
        // layer-local frame; follow it so displayed values and the ◆/◇
        // state track playback.
        let playback_sub = cx.observe_global::<super::PlaybackPosition>(|this: &mut Self, cx| {
            if matches!(this.target, PropertiesTarget::Nodes { .. }) {
                this.refresh_values(cx);
                cx.notify();
            }
        });

        let focus_handle = cx.focus_handle();
        let focus_subscriptions =
            super::track_panel_focus(PanelKind::Properties, &focus_handle, window, cx);

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        Self {
            sections: Vec::new(),
            target: PropertiesTarget::Empty,
            project,
            registry,
            scrubs: Vec::new(),
            strings: Vec::new(),
            selects: Vec::new(),
            colors: Vec::new(),
            pending_color_commit: None,
            color_commit_generation: 0,
            needs_rebuild: false,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            selection_sub,
            playback_sub,
        }
    }

    /// Route a layer field edit into the document (REQ-LAYER-009).
    fn apply_layer_change(
        &mut self,
        key: &str,
        value: PropertyValue,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let PropertiesTarget::Layer {
            comp_id,
            layer,
            frame,
            ..
        } = &self.target
        else {
            return;
        };
        let comp_id: CompId = *comp_id;
        let layer_id = layer.id;
        // Channel-backed fields insert/update a key at the layer-local
        // frame under the playhead (REQ-LAYER-004/006).
        let local_frame = layer_local_frame(layer, *frame);
        let Some(project) = self.project.clone() else {
            return;
        };

        // Custom parameter edits invalidate the In node; solo/mute/blend/
        // adjustment change the compiled merge chain (REQ-LAYER-007).
        let hint = if key.starts_with(CUSTOM_FIELD_PREFIX) {
            in_node_id(layer)
                .map(|id| InvalidationHint::Params(vec![id]))
                .unwrap_or(InvalidationHint::None)
        } else {
            match key {
                "blend_mode" | "solo" | "muted" | "adjustment" => InvalidationHint::Structural,
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
        let PropertiesTarget::Layer {
            comp_id,
            layer,
            frame,
            ..
        } = &self.target
        else {
            return;
        };
        let comp_id = *comp_id;
        let layer_id = layer.id;
        let frame = *frame;
        let hint = if key.starts_with(CUSTOM_FIELD_PREFIX) {
            in_node_id(layer)
                .map(|id| InvalidationHint::Params(vec![id]))
                .unwrap_or(InvalidationHint::None)
        } else {
            InvalidationHint::None
        };
        let Some(project) = self.project.clone() else {
            return;
        };

        let key = key.to_string();
        project.update(cx, |project, cx| {
            let mut toggled = false;
            // Apply to the document's latest layer: the local frame is
            // derived from its current timing, not the panel snapshot.
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

        // Resync the inspected snapshot so the toggle state re-renders
        // (the timeline also republishes, but the panel must not depend
        // on it).
        let refreshed = project
            .read(cx)
            .document()
            .get_composition(comp_id)
            .and_then(|comp| comp.get_layer(layer_id))
            .cloned();
        if let Some(refreshed) = refreshed
            && let PropertiesTarget::Layer { layer, .. } = &mut self.target
        {
            **layer = refreshed;
        }
        self.refresh_values(cx);
        cx.notify();
    }

    /// Route a field edit to its target: layer targets edit the document,
    /// node targets signal the node editor through `PropertyChanged`.
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
        if node_ids.is_empty() {
            return;
        }
        cx.set_global(super::PropertyChanged {
            node_ids: node_ids.to_vec(),
            key: key.to_string(),
            value,
            commit,
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
                    _ => {}
                }
            }
        }
    }

    /// The owning layer's local frame at the playhead for the node target,
    /// resolved through the node editor's network context (0 when the
    /// editor has no context, e.g. in tests without one).
    fn node_target_local_frame(&self, cx: &App) -> u64 {
        cx.try_global::<super::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade())
            .and_then(|editor| editor.read(cx).current_local_frame(cx))
            .unwrap_or(0)
    }

    fn sections_for_target(&self, cx: &App) -> Vec<PropertySection> {
        match &self.target {
            PropertiesTarget::Empty => Vec::new(),
            PropertiesTarget::Nodes { nodes, driven, .. } => match nodes.first() {
                // Animated channels display their value at the playhead's
                // layer-local frame — the same frame edits and the key
                // toggle apply to (REQ-LAYER-004/006).
                Some(first) => {
                    let frame = self.node_target_local_frame(cx);
                    sections_for_node(first, &self.registry, frame, driven)
                }
                None => Vec::new(),
            },
            PropertiesTarget::Layer {
                layer,
                frame,
                fps,
                resolution,
                ..
            } => {
                let ctx = ravel_core::eval::EvalContext::new(*frame, *fps, *resolution);
                sections_for_layer(layer, &ctx)
            }
        }
    }

    /// Update section values (and idle scrub widgets) from the current
    /// target without recreating widget entities, so an in-flight scrub
    /// keeps its state.
    fn refresh_values(&mut self, cx: &mut Context<Self>) {
        self.sections = self.sections_for_target(cx);
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
    }

    fn rebuild_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let span = tracing::debug_span!("rebuild_widgets");
        let _guard = span.enter();
        self.needs_rebuild = false;
        self.scrubs.clear();
        self.strings.clear();
        self.selects.clear();
        self.colors.clear();

        let sections = self.sections_for_target(cx);
        let node_ids = match &self.target {
            PropertiesTarget::Nodes { ids, .. } => ids.clone(),
            _ => Vec::new(),
        };

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
                        if matches!(this.target, PropertiesTarget::Layer { .. }) {
                            this.apply_layer_change(&field_key, value, commit, cx);
                            return;
                        }
                        if ids.is_empty() {
                            return;
                        }
                        cx.set_global(super::PropertyChanged {
                            node_ids: ids.clone(),
                            key: field_key.clone(),
                            value,
                            commit,
                        });
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
                                if matches!(this.target, PropertiesTarget::Layer { .. }) {
                                    this.apply_layer_change(&field_key, value, commit, cx);
                                    return;
                                }
                                if ids.is_empty() {
                                    return;
                                }
                                cx.set_global(super::PropertyChanged {
                                    node_ids: ids.clone(),
                                    key: field_key.clone(),
                                    value,
                                    commit,
                                });
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

                if let PropertyField::Enum {
                    key,
                    value,
                    options,
                } = field
                {
                    let items: Vec<SharedString> = options
                        .iter()
                        .map(|s| SharedString::from(s.clone()))
                        .collect();
                    let selected_idx = options.iter().position(|o| o == value);
                    let idx_path =
                        selected_idx.map(|i| gpui_component::IndexPath::default().row(i));
                    let entity = cx.new(|cx| SelectState::new(items, idx_path, window, cx));
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub = cx.subscribe_in(
                        &entity,
                        window,
                        move |this, _state, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                            if let SelectEvent::Confirm(Some(val)) = event {
                                let value = PropertyValue::String(val.to_string());
                                if matches!(this.target, PropertiesTarget::Layer { .. }) {
                                    this.apply_layer_change(&field_key, value, true, cx);
                                    return;
                                }
                                if ids.is_empty() {
                                    return;
                                }
                                cx.set_global(super::PropertyChanged {
                                    node_ids: ids.clone(),
                                    key: field_key.clone(),
                                    value,
                                    commit: true,
                                });
                            }
                        },
                    );
                    self.selects
                        .push((key.clone(), SelectBinding { state: entity, sub }));
                }
            }
        }
        self.sections = sections;
    }
}

impl Panel for PropertiesGpuiPanel {
    fn panel_name(&self) -> &'static str {
        "properties"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(PanelKind::Properties, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        super::tab_title(
            Some(PanelKind::Properties),
            SharedString::from(t!("panel.properties")),
            color,
        )
    }
}

impl EventEmitter<PanelEvent> for PropertiesGpuiPanel {}

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

        let mut content = div()
            .id("properties-panel")
            .size_full()
            .flex()
            .flex_col()
            .text_xs()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle);

        if self.sections.is_empty() {
            content = content.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(t!("panel.properties.empty"))),
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
            let muted = cx.theme().colors.muted_foreground;
            let fg = cx.theme().colors.foreground;
            let accent = cx.theme().colors.accent;
            let editor = cx.entity().downgrade();
            let node_ids = match &self.target {
                PropertiesTarget::Nodes { ids, .. } => ids.clone(),
                _ => Vec::new(),
            };

            // Keyframe state (◆/◇) per animatable field key
            // (REQ-LAYER-004). Layer fields ask the layer snapshot; node
            // fields read the node snapshot at the node editor's current
            // layer-local frame.
            let key_target: Option<KeyTarget> = match &self.target {
                PropertiesTarget::Layer { .. } => Some(KeyTarget::Layer(cx.entity().downgrade())),
                PropertiesTarget::Nodes { ids, .. } => ids.first().copied().map(KeyTarget::Node),
                PropertiesTarget::Empty => None,
            };
            let node_local_frame = if matches!(self.target, PropertiesTarget::Nodes { .. }) {
                Some(self.node_target_local_frame(cx))
            } else {
                None
            };
            let key_states: std::collections::HashMap<String, bool> = match &self.target {
                PropertiesTarget::Layer { layer, frame, .. } => {
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
                PropertiesTarget::Nodes { nodes, driven, .. } => match nodes.first() {
                    // Driven parameters render read-only; their stored
                    // keyframes are inert, so the key toggle is hidden.
                    Some(node) => sections
                        .iter()
                        .flat_map(|section| &section.fields)
                        .filter(|field| !driven.iter().any(|d| d.key == field.key()))
                        .filter_map(|field| {
                            node_param_keyed(node, field.key(), node_local_frame)
                                .map(|keyed| (field.key().to_string(), keyed))
                        })
                        .collect(),
                    None => std::collections::HashMap::new(),
                },
                PropertiesTarget::Empty => std::collections::HashMap::new(),
            };

            // Per-parameter port toggle states for the first selected node
            // (param-input-ports-plan Phase 4).
            let port_states: std::collections::HashMap<String, PortToggleState> = match &self.target
            {
                PropertiesTarget::Nodes { nodes, driven, .. } => match nodes.first() {
                    Some(node) if node.supports_param_ports() => node
                        .parameters
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
                        .collect(),
                    _ => std::collections::HashMap::new(),
                },
                _ => std::collections::HashMap::new(),
            };
            let port_node = match &self.target {
                PropertiesTarget::Nodes { ids, .. } => ids.first().copied(),
                _ => None,
            };

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
                let editor = editor.clone();
                let node_ids = node_ids.clone();
                let key_target = key_target.clone();
                let key_states = key_states.clone();
                let port_states = port_states.clone();

                accordion = accordion.item(move |item| {
                    let mut container = div().flex().flex_col().w_full();
                    for field in &fields {
                        let row = build_field_row(
                            field, &scrubs, &strings, &selects, &colors, &editor, &node_ids, muted,
                            fg,
                        );
                        let key_button = match (&key_target, key_states.get(field.key())) {
                            (Some(target), Some(keyed)) => Some(key_toggle_button(
                                field.key(),
                                *keyed,
                                target,
                                accent,
                                muted,
                            )),
                            _ => None,
                        };
                        let port_button = match (port_node, port_states.get(field.key())) {
                            (Some(node_id), Some(state)) => Some(port_toggle_button(
                                field.key(),
                                *state,
                                node_id,
                                accent,
                                muted,
                            )),
                            _ => None,
                        };
                        if key_button.is_none() && port_button.is_none() {
                            container = container.child(row);
                            continue;
                        }
                        let mut wrapper = div().flex().items_center();
                        if let Some(button) = port_button {
                            wrapper = wrapper.child(button);
                        }
                        if let Some(button) = key_button {
                            wrapper = wrapper.child(button);
                        }
                        container =
                            container.child(wrapper.child(div().flex_grow().min_w_0().child(row)));
                    }
                    item.title(title.clone()).open(true).child(container)
                });
            }
            content = content.child(accordion);
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
    use ravel_core::composition::{BlendMode, Layer};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::DataTypeId;
    use ravel_core::network as net;
    use std::sync::Arc;

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

        let window = cx.add_window(PropertiesGpuiPanel::new);
        window
            .update(cx, |panel, _window, cx| {
                let layer = project
                    .read(cx)
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(lid)
                    .unwrap()
                    .clone();
                panel.target = PropertiesTarget::Layer {
                    comp_id,
                    layer: Box::new(layer),
                    frame: 0,
                    fps: ravel_core::types::FrameRate::new(30, 1),
                    resolution: (16, 16),
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
            (layer(&project, comp_id, lid, cx).transform.position[0].evaluate(0, &eval) - 30.0)
                .abs()
                < f32::EPSILON
        );

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert!(layer(&project, comp_id, lid, cx).transform.position[0].evaluate(0, &eval) == 0.0);
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
            chs[0].evaluate(0, &eval)
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

    /// Node-target booleans use the same committed PropertyChanged route as
    /// the other node parameter editors.
    #[gpui::test]
    fn node_bool_edit_routes_as_one_commit(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _lid) = setup(cx);
        let node_id = NodeId::next();

        window
            .update(cx, |panel, _window, cx| {
                panel.target = PropertiesTarget::Nodes {
                    ids: vec![node_id],
                    nodes: vec![Arc::new(
                        Node::new(node_id, "test.bool")
                            .with_param("enabled", ParameterValue::Bool(false)),
                    )],
                    driven: Vec::new(),
                };
                panel.route_change("enabled", PropertyValue::Bool(true), true, &[node_id], cx);
            })
            .unwrap();

        cx.update(|cx| {
            let changed = cx.global::<crate::panels::PropertyChanged>();
            assert_eq!(changed.node_ids, vec![node_id]);
            assert_eq!(changed.key, "enabled");
            assert!(matches!(changed.value, PropertyValue::Bool(true)));
            assert!(changed.commit);
        });
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
        assert!((curve.sample(0) - 1.0).abs() < f32::EPSILON);

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
}

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

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Sizable;
use gpui_component::accordion::Accordion;
use gpui_component::checkbox::Checkbox;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::select::{SelectEvent, SelectState};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::document::update_layer;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::layer::{
    CUSTOM_FIELD_PREFIX, apply_layer_field, in_node_id, sections_for_layer,
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

#[allow(clippy::too_many_arguments)]
fn build_field_row(
    field: &PropertyField,
    scrubs: &[(String, Entity<ScrubInputState>)],
    selects: &[(String, Entity<SelectState<Vec<SharedString>>>)],
    bool_editor: Option<&WeakEntity<PropertiesGpuiPanel>>,
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
            let Some(panel) = bool_editor else {
                return kv_row(&field_label(key), &value.to_string(), muted, fg);
            };
            let panel = panel.clone();
            let field_key = key.clone();
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
                            panel
                                .update(cx, move |this, cx| {
                                    this.apply_layer_change(&key, value, true, cx);
                                    cx.notify();
                                })
                                .ok();
                        }),
                )
        }

        PropertyField::String { key, value } => kv_row(&field_label(key), value, muted, fg),

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

        PropertyField::Color { key, r, g, b, .. } => kv_row(
            &field_label(key),
            &format!("({r:.2}, {g:.2}, {b:.2})"),
            muted,
            fg,
        ),
    }
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
    selects: Vec<(String, SelectBinding)>,
    needs_rebuild: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
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
            selects: Vec::new(),
            needs_rebuild: false,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            selection_sub,
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
        let PropertiesTarget::Layer { comp_id, layer, .. } = &self.target else {
            return;
        };
        let comp_id: CompId = *comp_id;
        let layer_id = layer.id;
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
                applied = apply_layer_field(l, &key, &value);
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
                    _ => {}
                }
            }
        }
    }

    fn sections_for_target(&self) -> Vec<PropertySection> {
        match &self.target {
            PropertiesTarget::Empty => Vec::new(),
            PropertiesTarget::Nodes { nodes, .. } => match nodes.first() {
                Some(first) => sections_for_node(first, &self.registry),
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
        self.sections = self.sections_for_target();
        for section in &self.sections {
            for field in &section.fields {
                let value = match field {
                    PropertyField::Float { value, .. } => *value,
                    PropertyField::Int { value, .. } => *value as f32,
                    _ => continue,
                };
                if let Some((_, binding)) = self.scrubs.iter().find(|(k, _)| k == field.key()) {
                    binding.state.update(cx, |state, cx| {
                        if !state.is_dragging() {
                            state.set_value(value);
                            cx.notify();
                        }
                    });
                }
            }
        }
    }

    fn rebuild_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let span = tracing::debug_span!("rebuild_widgets");
        let _guard = span.enter();
        self.needs_rebuild = false;
        self.scrubs.clear();
        self.selects.clear();

        let sections = self.sections_for_target();
        let is_layer_target = matches!(self.target, PropertiesTarget::Layer { .. });
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

        let _ = is_layer_target;
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
            let select_entities: Vec<(String, Entity<SelectState<Vec<SharedString>>>)> = self
                .selects
                .iter()
                .map(|(k, b)| (k.clone(), b.state.clone()))
                .collect();
            let muted = cx.theme().colors.muted_foreground;
            let fg = cx.theme().colors.foreground;
            // Layer shell booleans (solo/muted/locked/adjustment) are
            // editable; node bools stay display-only for now.
            let bool_editor = matches!(self.target, PropertiesTarget::Layer { .. })
                .then(|| cx.entity().downgrade());

            let mut accordion = Accordion::new("properties-accordion")
                .multiple(true)
                .small();
            for section in sections {
                let fields = section.fields.clone();
                let title: SharedString = ravel_i18n::translate(&section.title).into();
                let scrubs = scrub_entities.clone();
                let selects = select_entities.clone();
                let bool_editor = bool_editor.clone();

                accordion = accordion.item(move |item| {
                    let mut container = div().flex().flex_col().w_full();
                    for field in &fields {
                        let row = build_field_row(
                            field,
                            &scrubs,
                            &selects,
                            bool_editor.as_ref(),
                            muted,
                            fg,
                        );
                        container = container.child(row);
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

    fn network_with_custom_param() -> Graph {
        let in_node = Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(1.0));
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
}

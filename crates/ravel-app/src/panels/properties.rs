// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Properties panel — GPUI view for inspecting and editing node parameters.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Sizable;
use gpui_component::accordion::Accordion;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::select::{SelectEvent, SelectState};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::layer::sections_for_layer;
use ravel_ui::properties::node::sections_for_node;
use ravel_ui::properties::{PropertyField, PropertySection, PropertyValue};

use crate::widgets::{ScrubEvent, ScrubInput, ScrubInputState};

use super::{PropertiesTarget, SelectedPropertiesTarget};

/// Localized display label for a property field key. Unknown keys (dynamic
/// node parameters) fall back to the bare key rather than the lookup path.
fn field_label(key: &str) -> String {
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

fn build_field_row(
    field: &PropertyField,
    _node_ids: &[ravel_core::id::NodeId],
    scrubs: &[(String, Entity<ScrubInputState>)],
    selects: &[(String, Entity<SelectState<Vec<SharedString>>>)],
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
            kv_row(&field_label(key), &value.to_string(), muted, fg)
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

pub struct PropertiesGpuiPanel {
    sections: Vec<PropertySection>,
    target: PropertiesTarget,
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
        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });

        let selection_sub = cx.observe_global::<SelectedPropertiesTarget>(|this: &mut Self, cx| {
            let target = cx
                .try_global::<SelectedPropertiesTarget>()
                .cloned()
                .unwrap_or_default();
            this.target = target.0;
            this.needs_rebuild = true;
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

    fn rebuild_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let span = tracing::debug_span!("rebuild_widgets");
        let _guard = span.enter();
        self.needs_rebuild = false;
        self.scrubs.clear();
        self.selects.clear();

        let sections = match &self.target {
            PropertiesTarget::Empty => {
                self.sections = Vec::new();
                return;
            }
            PropertiesTarget::Nodes { nodes, .. } => {
                if let Some(first) = nodes.first() {
                    sections_for_node(first, &self.registry)
                } else {
                    self.sections = Vec::new();
                    return;
                }
            }
            PropertiesTarget::Layer {
                layer,
                frame,
                fps,
                resolution,
            } => {
                let ctx = ravel_core::eval::EvalContext::new(*frame, *fps, *resolution);
                sections_for_layer(layer, &ctx)
            }
        };

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
                    let sub =
                        cx.subscribe(&entity, move |_this, _state, event: &ScrubEvent, cx| {
                            // Layer targets have no mutation path yet; an
                            // empty-id event would push a no-op undo snapshot.
                            if ids.is_empty() {
                                return;
                            }
                            let (val, commit) = match event {
                                ScrubEvent::Change(v) => (*v, false),
                                ScrubEvent::Commit(v) => (*v, true),
                            };
                            let value = if integer {
                                PropertyValue::Int(val.round() as i32)
                            } else {
                                PropertyValue::Float(val)
                            };
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
                        move |_this,
                              _state,
                              event: &SelectEvent<Vec<SharedString>>,
                              _window,
                              cx| {
                            if let SelectEvent::Confirm(Some(val)) = event {
                                if ids.is_empty() {
                                    return;
                                }
                                cx.set_global(super::PropertyChanged {
                                    node_ids: ids.clone(),
                                    key: field_key.clone(),
                                    value: PropertyValue::String(val.to_string()),
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

        let node_ids = match &self.target {
            PropertiesTarget::Nodes { ids, .. } => ids.clone(),
            _ => Vec::new(),
        };

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

            let mut accordion = Accordion::new("properties-accordion")
                .multiple(true)
                .small();
            for section in sections {
                let fields = section.fields.clone();
                let title: SharedString = ravel_i18n::translate(&section.title).into();
                let ids = node_ids.clone();
                let scrubs = scrub_entities.clone();
                let selects = select_entities.clone();

                accordion = accordion.item(move |item| {
                    let mut container = div().flex().flex_col().w_full();
                    for field in &fields {
                        let row = build_field_row(field, &ids, &scrubs, &selects, muted, fg);
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

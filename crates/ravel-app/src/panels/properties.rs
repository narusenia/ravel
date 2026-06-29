// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Properties panel — GPUI view for inspecting and editing node parameters.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Sizable;
use gpui_component::accordion::Accordion;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::slider::{SliderEvent, SliderState};
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::node::sections_for_node;
use ravel_ui::properties::{PropertyField, PropertySection, PropertyValue};

use super::{PropertiesTarget, SelectedPropertiesTarget};

fn kv_row(key: &str, value: &str, muted: Hsla, fg: Hsla) -> Div {
    div()
        .flex()
        .justify_between()
        .px_2()
        .py(px(2.0))
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

fn build_field_row(
    field: &PropertyField,
    _node_ids: &[ravel_core::id::NodeId],
    sliders: &[(String, Entity<SliderState>)],
    muted: Hsla,
    fg: Hsla,
) -> Div {
    match field {
        PropertyField::ReadOnly { key, value } => kv_row(key, value, muted, fg),

        PropertyField::Float { key, .. } => {
            let slider = sliders.iter().find(|(k, _)| k == key);
            let mut row = div().flex().flex_col().gap_1().px_2().py(px(2.0)).child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(key.clone())),
            );
            if let Some((_, entity)) = slider {
                row = row.child(gpui_component::slider::Slider::new(entity));
            }
            row
        }

        PropertyField::Bool { key, value } => kv_row(key, &value.to_string(), muted, fg),

        PropertyField::Int { key, value, .. } => kv_row(key, &value.to_string(), muted, fg),

        PropertyField::String { key, value } => kv_row(key, value, muted, fg),

        PropertyField::Enum { key, value, .. } => kv_row(key, value, muted, fg),

        PropertyField::Color { key, r, g, b, .. } => {
            kv_row(key, &format!("({r:.2}, {g:.2}, {b:.2})"), muted, fg)
        }
    }
}

struct SliderBinding {
    state: Entity<SliderState>,
    #[allow(dead_code)]
    sub: Subscription,
}

pub struct PropertiesGpuiPanel {
    sections: Vec<PropertySection>,
    target: PropertiesTarget,
    sliders: Vec<(String, SliderBinding)>,
    needs_rebuild: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
}

impl PropertiesGpuiPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
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

        Self {
            sections: Vec::new(),
            target: PropertiesTarget::Empty,
            sliders: Vec::new(),
            needs_rebuild: false,
            focus_handle: cx.focus_handle(),
            focused_sub,
            selection_sub,
        }
    }

    fn rebuild_widgets(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.needs_rebuild = false;
        self.sliders.clear();

        let sections = match &self.target {
            PropertiesTarget::Empty => {
                self.sections = Vec::new();
                return;
            }
            PropertiesTarget::Nodes { nodes, .. } => {
                if let Some(first) = nodes.first() {
                    sections_for_node(first)
                } else {
                    self.sections = Vec::new();
                    return;
                }
            }
        };

        let node_ids = match &self.target {
            PropertiesTarget::Nodes { ids, .. } => ids.clone(),
            _ => Vec::new(),
        };

        for section in &sections {
            for field in &section.fields {
                if let PropertyField::Float {
                    key,
                    value,
                    range,
                    step,
                } = field
                {
                    let mut state = SliderState::new().default_value(*value);
                    if let Some(r) = range {
                        state = state.min(*r.start()).max(*r.end());
                    } else {
                        state = state.min(-10.0).max(10.0);
                    }
                    if let Some(s) = step {
                        state = state.step(*s);
                    }
                    let entity = cx.new(|_| state);
                    let field_key = key.clone();
                    let ids = node_ids.clone();
                    let sub =
                        cx.subscribe(&entity, move |_this, _state, event: &SliderEvent, cx| {
                            if let SliderEvent::Change(val) = event {
                                cx.set_global(super::PropertyChanged {
                                    node_ids: ids.clone(),
                                    key: field_key.clone(),
                                    value: PropertyValue::Float(val.start()),
                                });
                            }
                        });
                    self.sliders
                        .push((key.clone(), SliderBinding { state: entity, sub }));
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
        div()
            .text_xs()
            .text_color(color)
            .child(SharedString::from(t!("panel.properties")))
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

        let focus = self.focus_handle.clone();

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
            .track_focus(&self.focus_handle)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                focus.focus(window, cx);
                cx.set_global(super::FocusedPanelGlobal(Some(PanelKind::Properties)));
            });

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
            let slider_entities: Vec<(String, Entity<SliderState>)> = self
                .sliders
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
                let title: SharedString = section.title.clone().into();
                let ids = node_ids.clone();
                let sliders = slider_entities.clone();

                accordion = accordion.item(move |item| {
                    let mut container = div().flex().flex_col().w_full();
                    for field in &fields {
                        let row = build_field_row(field, &ids, &sliders, muted, fg);
                        container = container.child(row);
                    }
                    item.title(title.clone()).child(container)
                });
            }
            content = content.child(accordion);
        }

        content
    }
}

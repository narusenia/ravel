// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Custom node renderer for the Ravel node editor.
//!
//! Renders nodes with a header, vertical input/output port lists with
//! type-colored dots, and inline parameter display.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::label::Label;
use gpui_flow::FlowNode;
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::id::NodeId;
use std::collections::HashMap;
use std::sync::Arc;

use super::port_colors::port_color;

pub fn render_ravel_node(
    flow_node: &FlowNode,
    node_cache: &HashMap<NodeId, Arc<Node>>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let node_id = flow_node
        .id
        .strip_prefix('n')
        .and_then(|s| s.parse::<u64>().ok())
        .map(NodeId::new);

    let core_node = node_id.and_then(|id| node_cache.get(&id));

    match core_node {
        Some(node) => render_node_content(node, cx),
        None => render_fallback(&flow_node.label),
    }
}

fn render_node_content(node: &Node, cx: &App) -> AnyElement {
    let colors = cx.theme().colors;
    let label = node.metadata.label.as_deref().unwrap_or(&node.type_key);

    let mut content = div().flex().flex_col().gap_1().min_w(px(120.0));

    content = content.child(
        div()
            .flex()
            .items_center()
            .pb_1()
            .border_b_1()
            .border_color(Hsla {
                a: 0.2,
                ..colors.border
            })
            .child(Label::new(label.to_string()).text_sm()),
    );

    let ports = div().flex().flex_row().gap_3();

    let mut inputs = div().flex().flex_col().gap(px(2.0));
    let mut has_inputs = false;
    for input in &node.inputs {
        has_inputs = true;
        let type_color = input
            .accepted_types
            .first()
            .map(|t| port_color(*t))
            .unwrap_or(colors.muted_foreground);
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(type_color))
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(input.name.clone()),
            );
        inputs = inputs.child(row);
    }

    let mut outputs = div().flex().flex_col().gap(px(2.0)).items_end();
    let mut has_outputs = false;
    for output in &node.outputs {
        has_outputs = true;
        let type_color = port_color(output.data_type);
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(output.name.clone()),
            )
            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(type_color));
        outputs = outputs.child(row);
    }

    let ports = ports
        .when(has_inputs, |el| el.child(inputs))
        .when(has_inputs && has_outputs, |el| el.child(div().flex_grow()))
        .when(has_outputs, |el| el.child(outputs));

    content = content.child(ports);

    if !node.parameters.is_empty() {
        let params = div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .pt_1()
            .border_t_1()
            .border_color(Hsla {
                a: 0.15,
                ..colors.border
            });
        let mut params = params;
        for param in &node.parameters {
            let val_str = match &param.value {
                ParameterValue::Float(v) => format!("{v:.2}"),
                ParameterValue::Int(v) => v.to_string(),
                ParameterValue::Bool(v) => v.to_string(),
                ParameterValue::String(v) => v.clone(),
            };
            params = params.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(param.key.clone()),
                    )
                    .child(div().text_xs().text_color(colors.foreground).child(val_str)),
            );
        }
        content = content.child(params);
    }

    content.into_any_element()
}

fn render_fallback(label: &SharedString) -> AnyElement {
    div()
        .min_w(px(80.0))
        .text_sm()
        .child(label.to_string())
        .into_any_element()
}

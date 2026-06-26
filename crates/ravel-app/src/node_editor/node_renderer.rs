// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Custom node renderer for the Ravel node editor.
//!
//! Renders nodes with a header, vertical input/output port lists with
//! type-colored dots, and inline parameter display.
//!
//! Text color is inherited from gpui-flow's `text_color` setting on the
//! node wrapper. We intentionally avoid gpui-component's `Label` here
//! because it hardcodes `cx.theme().foreground` which conflicts with
//! the flow graph's color scheme.

use gpui::prelude::FluentBuilder;
use gpui::*;
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
    _cx: &mut App,
) -> AnyElement {
    let node_id = flow_node
        .id
        .strip_prefix('n')
        .and_then(|s| s.parse::<u64>().ok())
        .map(NodeId::new);

    let core_node = node_id.and_then(|id| node_cache.get(&id));

    match core_node {
        Some(node) => render_node_content(node),
        None => render_fallback(&flow_node.label),
    }
}

fn render_node_content(node: &Node) -> AnyElement {
    let label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
    let muted = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.65,
        a: 1.0,
    };
    let separator = Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.4,
        a: 0.3,
    };

    let mut content = div().flex().flex_col().gap_1().min_w(px(120.0));

    content = content.child(
        div()
            .flex()
            .items_center()
            .pb_1()
            .border_b_1()
            .border_color(separator)
            .text_sm()
            .child(label.to_string()),
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
            .unwrap_or(muted);
        inputs = inputs.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(type_color))
                .child(div().text_xs().text_color(muted).child(input.name.clone())),
        );
    }

    let mut outputs = div().flex().flex_col().gap(px(2.0)).items_end();
    let mut has_outputs = false;
    for output in &node.outputs {
        has_outputs = true;
        let type_color = port_color(output.data_type);
        outputs = outputs.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(output.name.clone()))
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(type_color)),
        );
    }

    let ports = ports
        .when(has_inputs, |el| el.child(inputs))
        .when(has_inputs && has_outputs, |el| el.child(div().flex_grow()))
        .when(has_outputs, |el| el.child(outputs));

    content = content.child(ports);

    if !node.parameters.is_empty() {
        let mut params = div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .pt_1()
            .border_t_1()
            .border_color(separator);
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
                    .child(div().text_xs().text_color(muted).child(param.key.clone()))
                    .child(div().text_xs().child(val_str)),
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

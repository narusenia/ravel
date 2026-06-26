// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI node editor panel: hosts gpui-flow FlowGraph with Ravel adapter.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_flow::{FlowGraph, FlowState};
use ravel_core::graph::{Graph, Node};
use ravel_core::id::{EdgeId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;

use crate::node_editor::adapter::graph_to_flow;
use crate::node_editor::node_renderer::render_ravel_node;

pub struct NodeEditorPanel {
    #[allow(dead_code)]
    graph: Graph,
    flow: Entity<FlowGraph>,
    #[allow(dead_code)]
    flow_state: Entity<FlowState>,
    #[allow(dead_code)]
    node_cache: HashMap<NodeId, Arc<Node>>,
    #[allow(dead_code)]
    registry: NodeRegistry,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl NodeEditorPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let graph = Self::build_demo_graph(&registry);

        let mut node_cache = HashMap::new();
        for node in graph.nodes() {
            node_cache.insert(node.id, Arc::clone(node));
        }

        let (flow_nodes, flow_edges) = graph_to_flow(&graph);
        let flow_state = cx.new(|_| FlowState::new(flow_nodes, flow_edges));

        let node_cache_for_renderer = node_cache.clone();
        let flow = cx.new(|cx| {
            let colors = cx.theme().colors;
            let bg = hsla_to_u32(colors.background);
            let grid = hsla_to_u32(Hsla {
                a: 0.15,
                ..colors.border
            });
            let node_bg = hsla_to_u32(colors.list);
            let node_border = hsla_to_u32(colors.border);
            let text = hsla_to_u32(colors.foreground);

            FlowGraph::new(flow_state.clone(), cx)
                .bg_color(bg)
                .grid_color(grid)
                .node_bg_color(node_bg)
                .node_border_color(node_border)
                .text_color(text)
                .default_renderer(move |flow_node, window, cx| {
                    render_ravel_node(flow_node, &node_cache_for_renderer, window, cx)
                })
        });

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });

        Self {
            graph,
            flow,
            flow_state,
            node_cache,
            registry,
            focus_handle: cx.focus_handle(),
            focused_sub,
        }
    }

    fn build_demo_graph(registry: &NodeRegistry) -> Graph {
        let blur_id = NodeId::new(1);
        let const_id = NodeId::new(2);
        let merge_id = NodeId::new(3);

        let mut blur = registry.create_node("blur", blur_id).unwrap();
        blur.metadata.position = (300.0, 100.0);

        let mut constant = registry.create_node("constant", const_id).unwrap();
        constant.metadata.position = (50.0, 100.0);

        let mut merge = registry.create_node("merge", merge_id).unwrap();
        merge.metadata.position = (550.0, 150.0);

        Graph::new()
            .add_node(constant)
            .unwrap()
            .add_node(blur)
            .unwrap()
            .add_node(merge)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                blur_id,
                ravel_core::id::OutputPortIndex(0),
                merge_id,
                ravel_core::id::InputPortIndex(0),
            )
            .unwrap()
    }
}

impl Panel for NodeEditorPanel {
    fn panel_name(&self) -> &'static str {
        "node_graph"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(ravel_ui::panel::PanelKind::NodeGraph, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        div()
            .text_xs()
            .text_color(color)
            .child(SharedString::from(t!("panel.node_graph")))
    }
}

impl EventEmitter<PanelEvent> for NodeEditorPanel {}

impl Focusable for NodeEditorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NodeEditorPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        div()
            .id("node-editor-panel")
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                focus.focus(window, cx);
                cx.set_global(super::FocusedPanelGlobal(Some(
                    ravel_ui::panel::PanelKind::NodeGraph,
                )));
            })
            .child(self.flow.clone())
    }
}

fn hsla_to_u32(c: Hsla) -> u32 {
    let rgba: Rgba = c.into();
    let r = (rgba.r.clamp(0.0, 1.0) * 255.0) as u32;
    let g = (rgba.g.clamp(0.0, 1.0) * 255.0) as u32;
    let b = (rgba.b.clamp(0.0, 1.0) * 255.0) as u32;
    (r << 16) | (g << 8) | b
}

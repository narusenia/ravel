// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Outliner panel: the project-structure view (REQ-UI-013).
//!
//! Rows come pre-flattened from [`ravel_ui::panels::outliner`], so rendering is
//! a straight walk over a list — no graph traversal in `render()`. Clicking
//! writes the shared globals the rest of the UI already follows:
//! `LayerSelection` for layer rows (shared with the Timeline),
//! `CanvasSelection` for node rows (shared with the node editor), and
//! `ProjectState::set_active_composition` for a composition switch.
//!
//! Rows of a composition that is not active are browsable but inert on a single
//! click: the `LayerSelection.comp == ActiveComposition` invariant means a
//! selection there has to switch composition first, which is what their
//! double-click does.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_i18n::t;
use ravel_ui::document::NetworkPath;
use ravel_ui::panel::PanelKind;
use ravel_ui::panels::outliner::{OutlinerKey, OutlinerPanel, OutlinerRow, OutlinerRowKind};
use std::collections::HashSet;

use crate::assets::RavelIcon;
use crate::project_state::ProjectState;

const ROW_HEIGHT: f32 = 22.0;
const INDENT_PER_DEPTH: f32 = 12.0;
const DISCLOSURE_SIZE: f32 = 14.0;

pub struct OutlinerGpuiPanel {
    state: OutlinerPanel,
    /// The app-wide document state; `None` only when the panel outlives it.
    project: Option<Entity<ProjectState>>,
    /// The flattened tree, rebuilt from the document whenever it or the
    /// expansion state changes (never inside `render()`).
    rows: Vec<OutlinerRow>,
    /// Highlight for a clicked composition row. Compositions have no shared
    /// selection state yet — `PropertiesTarget::Composition` arrives with the
    /// composition commands — so this stays panel-local and carries no meaning
    /// for other panels. Layer and node selection is never kept here.
    selected_comp: Option<CompId>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    #[allow(dead_code)]
    active_comp_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    canvas_selection_sub: Subscription,
}

impl OutlinerGpuiPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, _project, cx| {
                this.rebuild_rows(cx);
            })
        });

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        // A composition switch changes which rows are interactive, and the
        // newly active composition opens so its layers are reachable.
        let active_comp_sub = cx.observe_global::<super::ActiveComposition>(|this, cx| {
            if let Some(comp) = super::active_composition(cx) {
                this.state.set_expanded(OutlinerKey::Comp(comp), true);
            }
            this.selected_comp = None;
            this.rebuild_rows(cx);
        });
        // Selection highlighting only: the rows themselves do not change.
        let selection_sub = cx.observe_global::<super::LayerSelection>(|_this, cx| cx.notify());
        let canvas_selection_sub =
            cx.observe_global::<super::CanvasSelection>(|_this, cx| cx.notify());

        let focus_handle = cx.focus_handle();
        let focus_subscriptions =
            super::track_panel_focus(PanelKind::Outliner, &focus_handle, window, cx);

        let mut panel = Self {
            state: OutlinerPanel::new(),
            project,
            rows: Vec::new(),
            selected_comp: None,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            project_sub,
            active_comp_sub,
            selection_sub,
            canvas_selection_sub,
        };
        panel.rebuild_rows(cx);
        panel
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let rows = match &self.project {
            Some(project) => self.state.rows(project.read(cx).document()),
            None => Vec::new(),
        };
        if self.rows != rows {
            self.rows = rows;
        }
        // The document may have dropped the highlighted composition.
        if let Some(comp) = self.selected_comp
            && !self
                .rows
                .iter()
                .any(|row| matches!(row.kind, OutlinerRowKind::Comp { comp: id } if id == comp))
        {
            self.selected_comp = None;
        }
        cx.notify();
    }

    // ----- row interaction --------------------------------------------------

    fn toggle_row(&mut self, key: OutlinerKey, cx: &mut Context<Self>) {
        self.state.toggle_expanded(key);
        self.rebuild_rows(cx);
    }

    /// Make `comp` the composition the whole UI edits. `ProjectState` is the
    /// only writer of `ActiveComposition`: it drops the compiled chain and
    /// re-requests the viewer evaluation with the switch.
    fn activate_composition(&mut self, comp: CompId, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        project.update(cx, |project, cx| {
            project.set_active_composition(Some(comp), cx);
        });
    }

    /// Select a layer of the active composition and publish it as the
    /// Properties subject. The node editor opens the layer's network by
    /// observing `LayerSelection`.
    fn select_layer(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        self.selected_comp = None;
        super::set_layer_selection(vec![layer], cx);
        cx.set_global(super::SelectedPropertiesTarget(
            super::PropertiesTarget::Layer {
                comp_id: comp,
                layer_id: layer,
            },
        ));
        cx.notify();
    }

    /// Select a node of a layer network: the layer selection moves with it (a
    /// node row implies its layer), the canvas selection carries the network
    /// the node lives in, and Properties inspects the node.
    fn select_node(&mut self, comp: CompId, layer: LayerId, node: NodeId, cx: &mut Context<Self>) {
        let path = NetworkPath::layer(comp, layer);
        self.selected_comp = None;
        super::set_layer_selection(vec![layer], cx);
        cx.set_global(super::CanvasSelection {
            path: Some(path.clone()),
            nodes: HashSet::from([node]),
        });
        cx.set_global(super::SelectedPropertiesTarget(
            super::PropertiesTarget::Nodes {
                network: path.clone(),
                ids: vec![node],
            },
        ));
        // The editor follows the *layer* through `LayerSelection`, which says
        // nothing about subnet depth: a dive into a subnet of this same layer
        // would keep a network open that does not hold the selected node. Ask
        // for the exact network instead. `CanvasSelection` is already set, so
        // opening it keeps this selection.
        self.with_node_editor(cx, move |editor, cx| editor.open_network(path, cx));
        cx.notify();
    }

    /// Bring `node` into view in the node editor without changing its zoom.
    fn center_on_node(
        &mut self,
        comp: CompId,
        layer: LayerId,
        node: NodeId,
        cx: &mut Context<Self>,
    ) {
        let path = NetworkPath::layer(comp, layer);
        self.with_node_editor(cx, move |editor, cx| {
            editor.center_on_node(path, node, cx);
        });
    }

    /// Dive into a subnet node's own network (REQ-LAYER-003).
    fn enter_subnet(&mut self, comp: CompId, layer: LayerId, node: NodeId, cx: &mut Context<Self>) {
        let path = NetworkPath::layer(comp, layer);
        self.with_node_editor(cx, move |editor, cx| {
            editor.enter_subnet_at(path, node, cx);
        });
    }

    /// Show a whole layer network in the node editor (layer-row double-click).
    fn fit_layer_network(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        let path = NetworkPath::layer(comp, layer);
        self.with_node_editor(cx, move |editor, cx| editor.open_and_fit(path, cx));
    }

    /// Run `f` against the live node editor. The panels can be detached into
    /// separate windows, so the update is deferred past this entity's own
    /// update, exactly as the registry handle is used elsewhere.
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

    /// Click semantics (REQ-UI-013):
    ///
    /// * composition — single selects, double makes it active;
    /// * layer / node of the active composition — single selects, double also
    ///   moves the node editor's view (a subnet node dives instead);
    /// * layer / node of another composition — single does nothing (the rows
    ///   are drawn dimmed), double switches composition *and* selects, which is
    ///   the only way `LayerSelection.comp == ActiveComposition` allows a
    ///   cross-composition selection to happen.
    fn on_row_click(&mut self, index: usize, click_count: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        let double = click_count >= 2;
        let active = super::active_composition(cx) == Some(row.comp());

        match row.kind {
            OutlinerRowKind::Comp { comp } => {
                if double {
                    self.activate_composition(comp, cx);
                } else {
                    self.selected_comp = Some(comp);
                    cx.notify();
                }
            }
            OutlinerRowKind::Layer { comp, layer } => {
                if double {
                    if !active {
                        self.activate_composition(comp, cx);
                    }
                    self.select_layer(comp, layer, cx);
                    self.fit_layer_network(comp, layer, cx);
                } else if active {
                    self.select_layer(comp, layer, cx);
                }
            }
            OutlinerRowKind::Node {
                comp,
                layer,
                node,
                subnet,
                ..
            } => {
                if double {
                    if !active {
                        self.activate_composition(comp, cx);
                    }
                    self.select_node(comp, layer, node, cx);
                    if subnet {
                        self.enter_subnet(comp, layer, node, cx);
                    } else {
                        self.center_on_node(comp, layer, node, cx);
                    }
                } else if active {
                    self.select_node(comp, layer, node, cx);
                }
            }
            OutlinerRowKind::UnusedGroup { comp, layer, .. } => {
                self.toggle_row(OutlinerKey::Unused(comp, layer), cx);
            }
        }
    }

    // ----- rendering --------------------------------------------------------

    /// Whether the row is the current selection (layer and node rows follow
    /// the shared globals; a composition row follows this panel's highlight).
    fn is_row_selected(&self, row: &OutlinerRow, cx: &App) -> bool {
        match row.kind {
            OutlinerRowKind::Comp { comp } => self.selected_comp == Some(comp),
            OutlinerRowKind::Layer { comp, layer } => {
                let selection = super::layer_selection(cx);
                selection.comp() == Some(comp) && selection.contains(layer)
            }
            OutlinerRowKind::Node {
                comp, layer, node, ..
            } => cx.try_global::<super::CanvasSelection>().is_some_and(|s| {
                s.path.as_ref() == Some(&NetworkPath::layer(comp, layer)) && s.nodes.contains(&node)
            }),
            OutlinerRowKind::UnusedGroup { .. } => false,
        }
    }

    fn row_icon(row: &OutlinerRow) -> Icon {
        match row.kind {
            OutlinerRowKind::Comp { .. } => Icon::new(IconName::Frame),
            OutlinerRowKind::Layer { .. } => Icon::new(RavelIcon::Timeline),
            OutlinerRowKind::Node { .. } => Icon::new(RavelIcon::NodeGraph),
            OutlinerRowKind::UnusedGroup { .. } => Icon::new(IconName::FolderClosed),
        }
    }

    fn row_label(row: &OutlinerRow) -> SharedString {
        match row.kind {
            OutlinerRowKind::UnusedGroup { count, .. } => {
                SharedString::from(format!("{} ({count})", t!("outliner.unused")))
            }
            _ => SharedString::from(row.label.clone()),
        }
    }

    fn render_row(&self, index: usize, row: &OutlinerRow, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let selected = self.is_row_selected(row, cx);
        // Rows of a composition that is not active read as browsable but
        // inert; the composition row itself stays fully legible.
        let inactive_child = !matches!(row.kind, OutlinerRowKind::Comp { .. })
            && super::active_composition(cx) != Some(row.comp());
        let text_color = if inactive_child {
            Hsla {
                a: 0.5,
                ..colors.muted_foreground
            }
        } else if matches!(row.kind, OutlinerRowKind::UnusedGroup { .. }) {
            colors.muted_foreground
        } else {
            colors.foreground
        };
        let is_active_comp = matches!(row.kind, OutlinerRowKind::Comp { comp }
            if super::active_composition(cx) == Some(comp));

        let mut content = div()
            .id(SharedString::from(format!("outliner-row-{index}")))
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_1()
            .pl(px(4.0 + row.depth as f32 * INDENT_PER_DEPTH))
            .pr_1()
            .text_xs()
            .text_color(text_color)
            .when(selected, |row| row.bg(colors.list_active))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    this.on_row_click(index, event.click_count, cx);
                }),
            );

        // Disclosure triangle: toggling expansion must not select the row.
        content = content.child(match (row.expandable, row.key()) {
            (true, Some(key)) => div()
                .w(px(DISCLOSURE_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(if row.expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size_3()
                    .text_color(colors.muted_foreground),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        this.toggle_row(key, cx);
                    }),
                ),
            _ => div().w(px(DISCLOSURE_SIZE)),
        });

        content = content
            .child(Self::row_icon(row).size_3p5().text_color(text_color))
            .child(
                div()
                    .flex_grow()
                    .overflow_x_hidden()
                    .when(is_active_comp, |label| {
                        label.font_weight(FontWeight::SEMIBOLD)
                    })
                    .child(Self::row_label(row)),
            );

        // Badges: a node already shown above, and a node owning a subnet.
        if let OutlinerRowKind::Node {
            subnet, reference, ..
        } = row.kind
        {
            if reference {
                content = content.child(
                    div()
                        .id(SharedString::from(format!("outliner-ref-{index}")))
                        .child(
                            Icon::new(IconName::ExternalLink)
                                .size_3()
                                .text_color(colors.muted_foreground),
                        )
                        .tooltip(|window, cx| {
                            Tooltip::new(t!("outliner.reference")).build(window, cx)
                        }),
                );
            }
            if subnet {
                content = content.child(
                    div()
                        .id(SharedString::from(format!("outliner-subnet-{index}")))
                        .child(
                            Icon::new(IconName::Network)
                                .size_3()
                                .text_color(colors.muted_foreground),
                        )
                        .tooltip(|window, cx| {
                            Tooltip::new(t!("outliner.subnet")).build(window, cx)
                        }),
                );
            }
        }

        content
    }
}

impl Panel for OutlinerGpuiPanel {
    fn panel_name(&self) -> &'static str {
        PanelKind::Outliner.panel_id()
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(PanelKind::Outliner, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        super::tab_title(
            Some(PanelKind::Outliner),
            SharedString::from(t!("panel.outliner")),
            color,
        )
    }
}

impl EventEmitter<PanelEvent> for OutlinerGpuiPanel {}

impl Focusable for OutlinerGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OutlinerGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let mut content = div()
            .id("outliner-panel")
            .debug_selector(|| "outliner-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.list)
            .overflow_y_scroll()
            .track_focus(&self.focus_handle);

        // Composition 0 is a legitimate state, not an error: the panel says so
        // instead of drawing an empty list.
        if self.rows.is_empty() {
            return content.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("outliner.empty"))),
            );
        }

        let rows = self.rows.clone();
        for (index, row) in rows.iter().enumerate() {
            content = content.child(self.render_row(index, row, cx));
        }
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back to
    // the built-in one so `#[gpui::test]` and `#[test]` both resolve.
    use crate::panels::node_editor::NodeEditorPanel;
    use core::prelude::v1::test;
    use gpui::TestAppContext;
    use ravel_core::composition::{Composition, Layer};
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::runtime::InvalidationHint;
    use ravel_core::types::FrameRate;

    /// `net.out ← blur`: one node row per layer.
    fn network() -> (Graph, NodeId) {
        let out = Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let out_id = out.id;
        let blur = Node::new(NodeId::next(), "blur")
            .with_input("source", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let blur_id = blur.id;
        let graph = Graph::new()
            .add_node(out)
            .unwrap()
            .add_node(blur)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                blur_id,
                OutputPortIndex(0),
                out_id,
                InputPortIndex(0),
            )
            .unwrap();
        (graph, blur_id)
    }

    struct Fixture {
        window: WindowHandle<OutlinerGpuiPanel>,
        editor: WindowHandle<NodeEditorPanel>,
        project: Entity<ProjectState>,
        root: CompId,
        root_layer: LayerId,
        root_node: NodeId,
        other: CompId,
        other_layer: LayerId,
        other_node: NodeId,
    }

    /// Two compositions, each with one layer holding a two-node network. The
    /// document root stays active, as at startup.
    fn setup(cx: &mut TestAppContext) -> Fixture {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ));
            cx.set_global(super::super::SelectedPropertiesTarget::default());
            cx.set_global(super::super::CanvasSelection::default());
        });

        let (root, root_layer, root_node, other, other_layer, other_node) =
            project.update(cx, |project, cx| {
                let root = project.document().root_comp.expect("root comp");
                let (root_network, root_node) = network();
                let root_layer = LayerId::next();
                let doc = ravel_ui::document::add_layer(
                    project.document(),
                    root,
                    Layer::new(root_layer, "Root layer", root_network).with_time(0, 0, 100),
                )
                .unwrap();

                let (other_network, other_node) = network();
                let other_layer = LayerId::next();
                let other_id = CompId::next();
                let other_comp =
                    Composition::new(other_id, "Other", (1280, 720), FrameRate::new(24, 1), 120)
                        .add_layer(
                            Layer::new(other_layer, "Other layer", other_network)
                                .with_time(0, 0, 120),
                        );
                let mut doc = doc;
                doc.compositions
                    .insert(other_id, std::sync::Arc::new(other_comp));
                project.commit_document(doc, InvalidationHint::Structural, cx);
                (
                    root,
                    root_layer,
                    root_node,
                    other_id,
                    other_layer,
                    other_node,
                )
            });

        let editor = cx.add_window(NodeEditorPanel::new);
        let window = cx.add_window(OutlinerGpuiPanel::new);
        Fixture {
            window,
            editor,
            project,
            root,
            root_layer,
            root_node,
            other,
            other_layer,
            other_node,
        }
    }

    impl Fixture {
        /// Index of the row pointing at `kind`-matching content, panicking with
        /// the visible tree when it is missing.
        fn row_index(
            &self,
            cx: &mut TestAppContext,
            matcher: impl Fn(&OutlinerRow) -> bool,
        ) -> usize {
            self.window
                .update(cx, |panel, _window, _cx| {
                    panel
                        .rows
                        .iter()
                        .position(&matcher)
                        .unwrap_or_else(|| panic!("row not found in {:?}", panel.rows))
                })
                .unwrap()
        }

        fn click(&self, cx: &mut TestAppContext, index: usize, clicks: usize) {
            self.window
                .update(cx, |panel, _window, cx| {
                    panel.on_row_click(index, clicks, cx)
                })
                .unwrap();
            cx.run_until_parked();
        }

        /// Click the row the matcher finds, resolving the index first so the
        /// tree is re-read after every state change.
        fn click_row(
            &self,
            cx: &mut TestAppContext,
            matcher: impl Fn(&OutlinerRow) -> bool,
            clicks: usize,
        ) {
            let index = self.row_index(cx, matcher);
            self.click(cx, index, clicks);
        }

        fn expand_layer(&self, cx: &mut TestAppContext, comp: CompId, layer: LayerId) {
            self.window
                .update(cx, |panel, _window, cx| {
                    panel
                        .state
                        .set_expanded(OutlinerKey::Layer(comp, layer), true);
                    panel.rebuild_rows(cx);
                })
                .unwrap();
        }
    }

    fn is_comp(comp: CompId) -> impl Fn(&OutlinerRow) -> bool {
        move |row| matches!(row.kind, OutlinerRowKind::Comp { comp: id } if id == comp)
    }

    fn is_layer(layer: LayerId) -> impl Fn(&OutlinerRow) -> bool {
        move |row| matches!(row.kind, OutlinerRowKind::Layer { layer: id, .. } if id == layer)
    }

    fn is_node(node: NodeId) -> impl Fn(&OutlinerRow) -> bool {
        move |row| matches!(row.kind, OutlinerRowKind::Node { node: id, .. } if id == node)
    }

    /// Both compositions, their layers, and the layer's node chain are
    /// reachable in one tree (REQ-UI-013 three levels).
    #[gpui::test]
    fn the_tree_shows_compositions_layers_and_nodes(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.expand_layer(cx, f.root, f.root_layer);

        f.window
            .update(cx, |panel, _window, _cx| {
                let depths: Vec<(usize, &OutlinerRowKind)> = panel
                    .rows
                    .iter()
                    .map(|row| (row.depth, &row.kind))
                    .collect();
                assert!(
                    depths
                        .iter()
                        .any(|(d, kind)| *d == 0 && matches!(kind, OutlinerRowKind::Comp { .. })),
                    "compositions at depth 0: {depths:?}"
                );
                assert!(
                    depths
                        .iter()
                        .any(|(d, kind)| *d == 1 && matches!(kind, OutlinerRowKind::Layer { .. })),
                    "layers at depth 1: {depths:?}"
                );
                assert!(
                    depths
                        .iter()
                        .any(|(d, kind)| *d == 2 && matches!(kind, OutlinerRowKind::Node { .. })),
                    "layer nodes at depth 2: {depths:?}"
                );
            })
            .unwrap();
    }

    /// A composition row: single click highlights it, double click makes it the
    /// composition the whole UI edits.
    #[gpui::test]
    fn a_composition_row_selects_on_one_click_and_activates_on_two(cx: &mut TestAppContext) {
        let f = setup(cx);
        let other_row = f.row_index(cx, is_comp(f.other));

        f.click(cx, other_row, 1);
        cx.update(|cx| {
            assert_eq!(
                super::super::active_composition(cx),
                Some(f.root),
                "a single click must not switch composition"
            );
        });
        f.window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.selected_comp, Some(f.other));
            })
            .unwrap();

        f.click_row(cx, is_comp(f.other), 2);
        cx.update(|cx| {
            assert_eq!(super::super::active_composition(cx), Some(f.other));
            let selection = super::super::layer_selection(cx);
            assert_eq!(
                selection.comp(),
                Some(f.other),
                "LayerSelection.comp == ActiveComposition"
            );
            assert!(selection.is_empty(), "a switch starts with no selection");
        });
        f.project.read_with(cx, |project, cx| {
            assert_eq!(
                project.playback_params(cx),
                Some((FrameRate::new(24, 1), 120)),
                "the transport follows the switch"
            );
        });
    }

    /// A layer row writes the shared selection, so the Timeline highlight, the
    /// Properties subject, and the node editor's network all move with it.
    #[gpui::test]
    fn a_layer_row_selects_the_layer_and_opens_its_network(cx: &mut TestAppContext) {
        let f = setup(cx);
        let row = f.row_index(cx, is_layer(f.root_layer));

        f.click(cx, row, 1);

        cx.update(|cx| {
            let selection = super::super::layer_selection(cx);
            assert_eq!(selection.comp(), Some(f.root));
            assert_eq!(selection.layers(), [f.root_layer]);
            assert!(matches!(
                cx.global::<super::super::SelectedPropertiesTarget>().0,
                super::super::PropertiesTarget::Layer { comp_id, layer_id }
                    if comp_id == f.root && layer_id == f.root_layer
            ));
        });
        f.editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(
                    editor.context(),
                    Some(&NetworkPath::layer(f.root, f.root_layer)),
                    "the editor follows LayerSelection"
                );
            })
            .unwrap();
    }

    /// A node row selects the node itself: the canvas selection (node editor
    /// highlight, Viewer bbox) and the Properties target point at it, and the
    /// layer selection moves to its layer.
    #[gpui::test]
    fn a_node_row_selects_the_node_in_its_layer_network(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.expand_layer(cx, f.root, f.root_layer);
        let row = f.row_index(cx, is_node(f.root_node));

        f.click(cx, row, 1);

        let path = NetworkPath::layer(f.root, f.root_layer);
        cx.update(|cx| {
            let canvas = cx.global::<super::super::CanvasSelection>();
            assert_eq!(canvas.path.as_ref(), Some(&path));
            assert!(canvas.nodes.contains(&f.root_node));
            assert_eq!(
                super::super::layer_selection(cx).layers(),
                [f.root_layer],
                "a node row implies its layer"
            );
            assert!(matches!(
                &cx.global::<super::super::SelectedPropertiesTarget>().0,
                super::super::PropertiesTarget::Nodes { network, ids }
                    if network == &path && ids == &[f.root_node]
            ));
        });
        f.editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&path));
            })
            .unwrap();
    }

    /// Rows of a composition that is not active are browsable, but selecting in
    /// them is only possible as one gesture with the composition switch —
    /// otherwise `LayerSelection.comp == ActiveComposition` would break.
    #[gpui::test]
    fn an_inactive_compositions_rows_need_a_double_click(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.expand_layer(cx, f.other, f.other_layer);
        let layer_row = f.row_index(cx, is_layer(f.other_layer));

        f.click(cx, layer_row, 1);
        cx.update(|cx| {
            assert_eq!(super::super::active_composition(cx), Some(f.root));
            assert!(
                super::super::layer_selection(cx).is_empty(),
                "a single click in another composition selects nothing"
            );
        });

        f.click_row(cx, is_layer(f.other_layer), 2);
        cx.update(|cx| {
            assert_eq!(super::super::active_composition(cx), Some(f.other));
            let selection = super::super::layer_selection(cx);
            assert_eq!(selection.comp(), Some(f.other));
            assert_eq!(selection.layers(), [f.other_layer]);
        });

        // Same for a node row: switch and select in one gesture.
        f.click_row(cx, is_comp(f.root), 2);
        f.expand_layer(cx, f.other, f.other_layer);
        f.click_row(cx, is_node(f.other_node), 2);
        cx.update(|cx| {
            assert_eq!(super::super::active_composition(cx), Some(f.other));
            let canvas = cx.global::<super::super::CanvasSelection>();
            assert_eq!(
                canvas.path.as_ref(),
                Some(&NetworkPath::layer(f.other, f.other_layer))
            );
            assert!(canvas.nodes.contains(&f.other_node));
        });
        f.editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(
                    editor.context(),
                    Some(&NetworkPath::layer(f.other, f.other_layer)),
                    "the editor shows the network the selected node lives in"
                );
            })
            .unwrap();
    }

    /// A selection made in the Timeline highlights the same row here — both
    /// panels read one selection.
    #[gpui::test]
    fn a_selection_made_elsewhere_highlights_the_row(cx: &mut TestAppContext) {
        let f = setup(cx);
        cx.update(|cx| super::super::set_layer_selection(vec![f.root_layer], cx));
        cx.run_until_parked();

        f.window
            .update(cx, |panel, _window, cx| {
                let row = panel
                    .rows
                    .iter()
                    .find(|row| is_layer(f.root_layer)(row))
                    .expect("layer row");
                assert!(panel.is_row_selected(row, cx));
            })
            .unwrap();
    }

    /// Composition 0 draws the empty state instead of an empty list.
    #[gpui::test]
    fn a_document_without_compositions_has_no_rows(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            doc.compositions.clear();
            doc.root_comp = None;
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.set_active_composition(None, cx);
        });
        cx.run_until_parked();

        f.window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.rows.is_empty());
                assert_eq!(panel.selected_comp, None);
            })
            .unwrap();
    }
}

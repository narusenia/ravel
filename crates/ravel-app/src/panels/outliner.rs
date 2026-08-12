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
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::document::{
    NetworkPath, duplicate_layers, remove_layers, reorder_layer, update_layer,
};
use ravel_ui::panels::layer_selection::{LayerClickMode, layer_selection_after_click};
use ravel_ui::panels::outliner::{OutlinerKey, OutlinerPanel, OutlinerRow, OutlinerRowKind};
use std::collections::HashSet;

use crate::assets::RavelIcon;
use crate::project_state::ProjectState;

const HEADER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 22.0;
const INDENT_PER_DEPTH: f32 = 12.0;
const DISCLOSURE_SIZE: f32 = 14.0;

/// Builtin layer templates offered by the composition row's Add Layer
/// submenu, in the Layer menu's order.
const LAYER_ADD_COMMANDS: [CommandId; 5] = [
    CommandId::LayerAddSolid,
    CommandId::LayerAddShape,
    CommandId::LayerAddVideo,
    CommandId::LayerAddAudio,
    CommandId::LayerAddNull,
];

/// A layer row being dragged to a new position in its composition's stack.
/// The live document carries the moves; `changed` decides whether the gesture
/// records an undo step when it ends.
struct LayerDrag {
    comp: CompId,
    layer: LayerId,
    changed: bool,
}

fn outliner_row_cursor(dragging: bool) -> CursorStyle {
    if dragging {
        CursorStyle::ResizeUpDown
    } else {
        CursorStyle::PointingHand
    }
}

/// Inline rename of a layer row. The subscription commits the edited name on
/// Enter or blur and is dropped with the rename.
struct LayerRename {
    comp: CompId,
    layer: LayerId,
    input: Entity<InputState>,
    #[allow(dead_code)]
    sub: Subscription,
}

pub struct OutlinerGpuiPanel {
    state: OutlinerPanel,
    /// The app-wide document state; `None` only when the panel outlives it.
    project: Option<Entity<ProjectState>>,
    /// The flattened tree, rebuilt from the document whenever it or the
    /// expansion state changes (never inside `render()`).
    rows: Vec<OutlinerRow>,
    /// In-flight layer reorder, `None` outside a drag.
    layer_drag: Option<LayerDrag>,
    /// In-flight inline rename, `None` when no row is being renamed.
    rename: Option<LayerRename>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    active_comp_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    canvas_selection_sub: Subscription,
    #[allow(dead_code)]
    properties_target_sub: Subscription,
}

impl OutlinerGpuiPanel {
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, project, cx| {
                // Rebuilding walks every composition, layer, and node, so it
                // only runs when what the tree shows actually moved — the
                // composition-switch observer below has its own path.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.rebuild_rows(cx);
            })
        });

        // A composition switch changes which rows are interactive, and the
        // newly active composition opens so its layers are reachable.
        let active_comp_sub = cx.observe_global::<super::ActiveComposition>(|this, cx| {
            if let Some(comp) = super::active_composition(cx) {
                this.state.set_expanded(OutlinerKey::Comp(comp), true);
            }
            this.rebuild_rows(cx);
        });
        // Selection highlighting only: the rows themselves do not change.
        let selection_sub = cx.observe_global::<super::LayerSelection>(|_this, cx| cx.notify());
        let canvas_selection_sub =
            cx.observe_global::<super::CanvasSelection>(|_this, cx| cx.notify());
        // A composition row's highlight *is* the Properties composition
        // target, so it repaints with it (a layer or node selection made
        // anywhere replaces that target and un-highlights the row).
        let properties_target_sub =
            cx.observe_global::<super::SelectedPropertiesTarget>(|_this, cx| cx.notify());

        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);

        let mut panel = Self {
            state: OutlinerPanel::new(),
            project,
            rows: Vec::new(),
            layer_drag: None,
            rename: None,
            focus_handle,
            focus_subscriptions,
            project_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            active_comp_sub,
            selection_sub,
            canvas_selection_sub,
            properties_target_sub,
        };
        panel.rebuild_rows(cx);
        panel
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        super::sync_probe::record(super::sync_probe::PanelSync::OutlinerRows);
        self.rows = match &self.project {
            Some(project) => self.state.rows(project.read(cx).document()),
            None => Vec::new(),
        };
        // An inline rename whose layer is gone (deleted, undone) has no row to
        // render into: drop it instead of keeping an invisible editor whose
        // blur would try to name a layer that no longer exists.
        if let Some(rename) = &self.rename {
            let (comp, layer) = (rename.comp, rename.layer);
            let alive = self.project.as_ref().is_some_and(|project| {
                project
                    .read(cx)
                    .document()
                    .get_composition(comp)
                    .is_some_and(|c| c.get_layer(layer).is_some())
            });
            if !alive {
                self.rename = None;
            }
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

    /// Select a composition: it becomes the Properties subject, which is also
    /// what the composition commands act on
    /// ([`super::command_target_composition`]) — so Settings / Duplicate /
    /// Delete from the menu, the header buttons, or the row's context menu all
    /// apply to the row the user picked. Selecting does *not* switch the active
    /// composition; that is the row's double-click.
    fn select_composition(&mut self, comp: CompId, cx: &mut Context<Self>) {
        cx.set_global(super::SelectedPropertiesTarget(
            super::PropertiesTarget::Composition { comp_id: comp },
        ));
        cx.notify();
    }

    /// Select a layer of the active composition and publish it as the
    /// Properties subject. The node editor opens the layer's network by
    /// observing `LayerSelection`.
    fn select_layer(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        self.select_layer_with_mode(comp, layer, LayerClickMode::Replace, cx);
    }

    /// Select a layer, extending the current selection when a modifier asks for
    /// it: Shift ranges over the composition's stack, the platform modifier
    /// toggles (REQ-UI-013). The arithmetic is the same headless function the
    /// Timeline uses, so a modified click means one thing in both panels.
    fn select_layer_with_mode(
        &mut self,
        comp: CompId,
        layer: LayerId,
        mode: LayerClickMode,
        cx: &mut Context<Self>,
    ) {
        let order: Vec<LayerId> = self
            .project
            .as_ref()
            .and_then(|project| project.read(cx).document().get_composition(comp))
            .map(|comp| comp.layers.iter().map(|layer| layer.id).collect())
            .unwrap_or_default();
        let selection = super::layer_selection(cx);
        let layers = layer_selection_after_click(selection.layers(), &order, layer, mode);
        super::set_layer_selection(layers, cx);
        super::publish_layer_properties_target(cx);
        cx.notify();
    }

    /// Select a layer for an operation aimed at the row under the cursor (right
    /// click): a selection that already holds the layer is kept, so opening the
    /// context menu on one of several selected rows does not throw the rest of
    /// the selection away.
    fn select_layer_for_menu(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        if super::layer_selection(cx).contains(layer) {
            // The selection stands, but the right click still points Properties
            // at it (a right click has always done that).
            super::publish_layer_properties_target(cx);
            cx.notify();
            return;
        }
        self.select_layer(comp, layer, cx);
    }

    /// Select a node of a layer network: the layer selection moves with it (a
    /// node row implies its layer), the canvas selection carries the network
    /// the node lives in, and Properties inspects the node.
    fn select_node(&mut self, comp: CompId, layer: LayerId, node: NodeId, cx: &mut Context<Self>) {
        let path = NetworkPath::layer(comp, layer);
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

    // ----- layer operations (REQ-UI-013) ------------------------------------

    /// Begin dragging a layer row to a new position in its composition's stack.
    /// Only the active composition's rows drag: the stack order is a document
    /// edit, and reordering a composition the UI does not show would be an
    /// invisible change.
    fn start_layer_drag(&mut self, comp: CompId, layer: LayerId) {
        self.layer_drag = Some(LayerDrag {
            comp,
            layer,
            changed: false,
        });
    }

    /// Move the dragged layer onto the row under the cursor, live. Each move
    /// applies without recording undo; [`Self::end_layer_drag`] records one
    /// step for the whole gesture (the Timeline's reorder convention).
    fn drag_over_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(drag) = self.layer_drag.as_ref() else {
            return;
        };
        let (comp, dragged) = (drag.comp, drag.layer);
        let Some(target) = self.rows.get(index).and_then(|row| match row.kind {
            // A node row stands for its layer: dropping on one lands the drag
            // on the layer that owns it rather than doing nothing.
            OutlinerRowKind::Layer { comp: c, layer }
            | OutlinerRowKind::Node { comp: c, layer, .. }
            | OutlinerRowKind::UnusedGroup { comp: c, layer, .. }
                if c == comp =>
            {
                Some(layer)
            }
            _ => None,
        }) else {
            return;
        };
        if target == dragged {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let moved = project.update(cx, |project, cx| {
            let Some(to_index) = project
                .document()
                .get_composition(comp)
                .and_then(|c| c.layers.iter().position(|l| l.id == target))
            else {
                return false;
            };
            match reorder_layer(project.document(), comp, dragged, to_index) {
                Some(doc) => {
                    project.apply_document(doc, InvalidationHint::Structural, cx);
                    true
                }
                None => false,
            }
        });
        if moved && let Some(drag) = self.layer_drag.as_mut() {
            drag.changed = true;
        }
    }

    /// Finish a reorder: the gesture's live edits become one undo step.
    fn end_layer_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.layer_drag.take() else {
            return;
        };
        if !drag.changed {
            cx.notify();
            return;
        }
        if let Some(project) = self.project.clone() {
            project.update(cx, |project, cx| {
                let doc = project.document().clone();
                project.commit_document(doc, InvalidationHint::Structural, cx);
            });
        }
        cx.notify();
    }

    /// Start renaming a layer row in place. The caller focuses the input — a
    /// panel never grabs focus on its own (`.agents/rules/gpui.md`).
    fn begin_rename(
        &mut self,
        comp: CompId,
        layer: LayerId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        let name = self
            .project
            .as_ref()?
            .read(cx)
            .document()
            .get_composition(comp)?
            .get_layer(layer)?
            .name
            .clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, state, event: &InputEvent, _window, cx| match event {
                // Enter and blur both commit: leaving the field is the same
                // intent as confirming it (the Properties name field's rule).
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let name = state.read(cx).value().to_string();
                    this.commit_rename(name, cx);
                }
                _ => {}
            },
        );
        self.rename = Some(LayerRename {
            comp,
            layer,
            input: input.clone(),
            sub,
        });
        cx.notify();
        Some(input)
    }

    /// Apply an edited layer name as one undo step. A blank or unchanged name
    /// just closes the editor — a nameless layer is unreachable in the tree.
    fn commit_rename(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        cx.notify();
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        project.update(cx, |project, cx| {
            let unchanged = project
                .document()
                .get_composition(rename.comp)
                .and_then(|c| c.get_layer(rename.layer))
                .is_some_and(|layer| layer.name == name);
            if unchanged {
                return;
            }
            // Renaming does not change what the composition renders.
            if let Some(doc) = update_layer(project.document(), rename.comp, rename.layer, |l| {
                l.name = name.clone();
            }) {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
    }

    /// Abandon an inline rename, keeping the layer's current name.
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.notify();
        }
    }

    /// The layers an operation on the row `layer` applies to: the whole
    /// selection when the row is part of it, otherwise just that row — the same
    /// rule the Timeline uses, so the two panels stay interchangeable
    /// (REQ-UI-013 bulk editing).
    fn operation_targets(&self, layer: LayerId, cx: &App) -> Vec<LayerId> {
        let selection = super::layer_selection(cx);
        if selection.contains(layer) {
            selection.layers().to_vec()
        } else {
            vec![layer]
        }
    }

    /// Deep-copy the operation targets of `layer` above their originals, as one
    /// undo step, and select the copies (the Timeline's Duplicate does the same).
    fn duplicate_layer(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let targets = self.operation_targets(layer, cx);
        let copies = project.update(cx, |project, cx| {
            let (doc, copies) = duplicate_layers(project.document(), comp, &targets)?;
            project.commit_document(doc, InvalidationHint::Structural, cx);
            Some(copies)
        });
        let Some(copies) = copies.filter(|copies| !copies.is_empty()) else {
            return;
        };
        super::set_layer_selection(copies, cx);
        super::publish_layer_properties_target(cx);
        cx.notify();
    }

    /// Delete the operation targets of `layer` and their owned networks as one
    /// undo step (REQ-LAYER-009). Locked layers are protected — checked against
    /// the document rather than the row — and stay selected.
    fn delete_layer(&mut self, comp: CompId, layer: LayerId, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let targets = self.operation_targets(layer, cx);
        let deleted = project.update(cx, |project, cx| {
            match remove_layers(project.document(), comp, &targets) {
                Some(doc) => {
                    project.commit_document(doc, InvalidationHint::Structural, cx);
                    true
                }
                None => false,
            }
        });
        if !deleted {
            return;
        }
        let selection = super::layer_selection(cx);
        let remaining: Vec<LayerId> = selection
            .layers()
            .iter()
            .copied()
            .filter(|id| self.layer_exists(comp, *id, cx))
            .collect();
        if remaining.len() != selection.layers().len() {
            super::set_layer_selection(remaining, cx);
            super::publish_layer_properties_target(cx);
        }
    }

    /// Whether the document still holds `layer` in `comp`.
    fn layer_exists(&self, comp: CompId, layer: LayerId, cx: &App) -> bool {
        self.project.as_ref().is_some_and(|project| {
            project
                .read(cx)
                .document()
                .get_composition(comp)
                .is_some_and(|composition| composition.get_layer(layer).is_some())
        })
    }

    /// Whether a layer is locked in the live document (locked rows offer no
    /// destructive operations).
    fn layer_is_locked(&self, comp: CompId, layer: LayerId, cx: &App) -> bool {
        self.project.as_ref().is_some_and(|project| {
            project
                .read(cx)
                .document()
                .get_composition(comp)
                .and_then(|c| c.get_layer(layer))
                .is_none_or(|l| l.locked)
        })
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
    ///
    /// `mode` carries the held modifiers for a layer row (Shift ranges, the
    /// platform modifier toggles). Everything else selects one subject: a
    /// composition row, a node row (which stands for exactly its layer), and any
    /// double click, whose second job — switching composition, moving the node
    /// editor's view — has no multi-selection meaning.
    fn on_row_click(
        &mut self,
        index: usize,
        click_count: usize,
        mode: LayerClickMode,
        cx: &mut Context<Self>,
    ) {
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
                    self.select_composition(comp, cx);
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
                    self.select_layer_with_mode(comp, layer, mode, cx);
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
            OutlinerRowKind::Comp { comp } => matches!(
                cx.try_global::<super::SelectedPropertiesTarget>().map(|t| &t.0),
                Some(super::PropertiesTarget::Composition { comp_id }) if *comp_id == comp
            ),
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

    fn row_icon(&self, row: &OutlinerRow, cx: &App) -> Icon {
        match row.kind {
            OutlinerRowKind::Comp { .. } => Icon::new(IconName::Frame),
            OutlinerRowKind::Layer { .. } => Icon::new(RavelIcon::Timeline),
            OutlinerRowKind::Node {
                comp, layer, node, ..
            } => Icon::new(self.node_row_icon(comp, layer, node, cx)),
            OutlinerRowKind::UnusedGroup { .. } => Icon::new(IconName::FolderClosed),
        }
    }

    /// Type icon of a node row, resolved from the live document and the
    /// template registry. A node the document no longer holds (or a panel
    /// that outlived its project) keeps the generic node icon.
    fn node_row_icon(&self, comp: CompId, layer: LayerId, node: NodeId, cx: &App) -> RavelIcon {
        let Some(project) = &self.project else {
            return RavelIcon::NodeGraph;
        };
        let project = project.read(cx);
        let Some(type_key) = project
            .document()
            .get_composition(comp)
            .and_then(|composition| composition.get_layer(layer))
            .and_then(|layer| layer.network.node(node))
            .map(|node| node.type_key.as_str())
        else {
            return RavelIcon::NodeGraph;
        };
        let category = project.registry().get(type_key).map(|t| t.category);
        RavelIcon::for_node_type(type_key, category)
    }

    fn row_label(&self, row: &OutlinerRow, cx: &App) -> SharedString {
        match row.kind {
            OutlinerRowKind::UnusedGroup { count, .. } => {
                SharedString::from(format!("{} ({count})", t!("outliner.unused")))
            }
            // Node rows carry the raw metadata label from `ravel-ui`; the
            // localized form (user rename → locale entry → type key) is
            // resolved here, where i18n is available.
            OutlinerRowKind::Node {
                comp, layer, node, ..
            } => {
                let label = self.project.as_ref().and_then(|project| {
                    let project = project.read(cx);
                    let node = project
                        .document()
                        .get_composition(comp)?
                        .get_layer(layer)?
                        .network
                        .node(node)?;
                    Some(crate::node_locale::display_label(node, project.registry()))
                });
                SharedString::from(label.unwrap_or_else(|| row.label.clone()))
            }
            _ => SharedString::from(row.label.clone()),
        }
    }

    fn render_row(&self, index: usize, row: &OutlinerRow, cx: &mut Context<Self>) -> AnyElement {
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
            // Rows must not shrink: a shrinkable row lets the flex container
            // squash the whole list into the panel height instead of
            // overflowing it, so the scroll container never has anything to
            // scroll (the same trap as `properties-scroll-content`).
            .flex_shrink_0()
            // Test hook for `VisualTestContext::debug_bounds` (noop in release
            // builds). Only the first row carries it: the selector map is keyed
            // by a `&'static str`, so a per-index name is not addressable.
            .when(index == 0, |row| {
                row.debug_selector(|| "outliner-row-first".into())
            })
            .flex()
            .items_center()
            .gap_1()
            .pl(px(4.0 + row.depth as f32 * INDENT_PER_DEPTH))
            .pr_1()
            .text_xs()
            .text_color(text_color)
            .when(selected, |row| row.bg(colors.list_active))
            .cursor(outliner_row_cursor(self.layer_drag.is_some()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let mode = LayerClickMode::from_modifiers(
                        event.modifiers.shift,
                        event.modifiers.platform,
                    );
                    this.on_row_click(index, event.click_count, mode, cx);
                }),
            )
            // Dragging a row over another row of the same composition
            // reorders the stack live (REQ-UI-013 unit 5).
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_over_row(index, cx);
                    }
                }),
            );

        // A layer row of the active composition can be dragged; the drag
        // starts here so the row's own click semantics stay untouched.
        if let OutlinerRowKind::Layer { comp, layer } = row.kind
            && !inactive_child
        {
            content = content.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    // A modified click is building a selection, not reordering
                    // the stack.
                    if event.modifiers.shift || event.modifiers.platform {
                        return;
                    }
                    this.start_layer_drag(comp, layer);
                    cx.notify();
                }),
            );
        }

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

        let renaming = match (&self.rename, row.kind) {
            (Some(rename), OutlinerRowKind::Layer { comp, layer })
                if rename.comp == comp && rename.layer == layer =>
            {
                Some(rename.input.clone())
            }
            _ => None,
        };
        content = content.child(self.row_icon(row, cx).size_3p5().text_color(text_color));
        content = match renaming {
            Some(input) => {
                // Raw key handling, the approved exception for text entry
                // (`.agents/rules/gpui.md`): `InputState` emits no event for
                // Escape, and its Enter action does not reach a subscriber
                // here (the same is true of the Properties name field), so the
                // row confirms and cancels the edit itself. Blur still commits.
                let commit_input = input.clone();
                content
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                        match event.keystroke.key.as_str() {
                            // GPUI names the key "enter"; "return" is
                            // accepted too so a platform that reports the
                            // physical key name still confirms.
                            "enter" | "return" => {
                                let name = commit_input.read(cx).value().to_string();
                                this.commit_rename(name, cx);
                            }
                            "escape" => this.cancel_rename(cx),
                            _ => {}
                        }
                    }))
                    .child(div().flex_grow().child(Input::new(&input).xsmall()))
            }
            None => content.child(
                // `min_w_0` lets the label shrink below its text width so
                // `truncate` can ellipsize it; without it the label keeps its
                // intrinsic width and pushes the trailing badges out of view.
                div()
                    .flex_grow()
                    .min_w_0()
                    .truncate()
                    .when(index == 0, |label| {
                        label.debug_selector(|| "outliner-row-first-label".into())
                    })
                    .when(is_active_comp, |label| {
                        label.font_weight(FontWeight::SEMIBOLD)
                    })
                    .child(self.row_label(row, cx)),
            ),
        };

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

        // Layer rows of the active composition carry the layer operations.
        // Like the Timeline's layer menu these call the panel directly: they
        // act on the row under the cursor, not on "the focused thing".
        if let OutlinerRowKind::Layer { comp, layer } = row.kind
            && !inactive_child
        {
            let locked = self.layer_is_locked(comp, layer, cx);
            // Delete acts on the whole selection when the row is part of it, so
            // it is only unavailable when every target is locked; Rename edits
            // this row alone and follows the row's own lock.
            let all_locked = self
                .operation_targets(layer, cx)
                .iter()
                .all(|target| self.layer_is_locked(comp, *target, cx));
            let entity = cx.entity().downgrade();
            return content
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        this.select_layer_for_menu(comp, layer, cx);
                    }),
                )
                .context_menu(move |menu, _window, _cx| {
                    let rename_entity = entity.clone();
                    let duplicate_entity = entity.clone();
                    let delete_entity = entity.clone();
                    menu.item(
                        PopupMenuItem::new(t!("outliner.menu.rename"))
                            .disabled(locked)
                            .on_click(move |_, window, cx| {
                                let _ = rename_entity.update(cx, |this, cx| {
                                    // Focus belongs to the click, not to the
                                    // panel's own construction.
                                    if let Some(input) = this.begin_rename(comp, layer, window, cx)
                                    {
                                        input.update(cx, |state, cx| state.focus(window, cx));
                                    }
                                });
                            }),
                    )
                    .item(PopupMenuItem::new(t!("outliner.menu.duplicate")).on_click(
                        move |_, _window, cx| {
                            let _ = duplicate_entity.update(cx, |this, cx| {
                                this.duplicate_layer(comp, layer, cx);
                            });
                        },
                    ))
                    .item(
                        PopupMenuItem::new(t!("outliner.menu.delete"))
                            .disabled(all_locked)
                            .on_click(move |_, _window, cx| {
                                let _ = delete_entity.update(cx, |this, cx| {
                                    this.delete_layer(comp, layer, cx);
                                });
                            }),
                    )
                })
                .into_any_element();
        }

        // Composition rows carry the management commands. Right-click selects
        // the row first, so the dispatched command — the same Action the menu
        // bar sends — acts on the composition under the cursor.
        if let OutlinerRowKind::Comp { comp } = row.kind {
            return content
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        this.select_composition(comp, cx);
                    }),
                )
                .context_menu(move |menu, window, cx| {
                    // Layer creation targets the *active* composition, so the
                    // submenu only appears on the row that is active — an
                    // "Add Layer" on some other composition's row would put
                    // the layer somewhere else entirely.
                    let menu = if is_active_comp {
                        menu.submenu(
                            t!("outliner.menu.add_layer"),
                            window,
                            cx,
                            |sub, _window, _cx| {
                                LAYER_ADD_COMMANDS.iter().fold(sub, |sub, command| {
                                    let command = *command;
                                    sub.item(PopupMenuItem::new(t!(command.label_key())).on_click(
                                        move |_, window, cx| {
                                            window.dispatch_action(
                                                crate::workspace::command_action(command),
                                                cx,
                                            );
                                        },
                                    ))
                                })
                            },
                        )
                        .separator()
                    } else {
                        menu
                    };
                    menu.item(
                        PopupMenuItem::new(t!("menu.composition.settings")).on_click(
                            |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(crate::workspace::CompositionSettings),
                                    cx,
                                );
                            },
                        ),
                    )
                    .item(
                        PopupMenuItem::new(t!("menu.composition.duplicate")).on_click(
                            |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(crate::workspace::CompositionDuplicate),
                                    cx,
                                );
                            },
                        ),
                    )
                    .item(
                        PopupMenuItem::new(t!("menu.composition.delete")).on_click(
                            |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(crate::workspace::CompositionDelete),
                                    cx,
                                );
                            },
                        ),
                    )
                })
                .into_any_element();
        }

        content.into_any_element()
    }
}

impl Focusable for OutlinerGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl OutlinerGpuiPanel {
    /// Toolbar button that dispatches a command Action — the same Action the
    /// menu bar and keybindings send, so there is one execution path
    /// (`.agents/rules/gpui.md`).
    fn command_button(
        id: &'static str,
        icon: impl Into<Icon>,
        tooltip: SharedString,
        action: impl Fn() -> Box<dyn Action> + 'static,
        colors: &gpui_component::ThemeColor,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .text_color(colors.muted_foreground)
            .hover(|style| style.bg(colors.list_active))
            .child(icon.into().size_3())
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(move |_event, window, cx| {
                window.dispatch_action(action(), cx);
            })
    }

    /// Panel header: composition management buttons. The trailing three act on
    /// the selected (or active) composition and are hidden without one.
    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let has_target = super::command_target_composition(cx).is_some();
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .h(px(HEADER_HEIGHT))
            .px_1()
            .border_b_1()
            .border_color(colors.border)
            .child(Self::command_button(
                "outliner-comp-new",
                IconName::Plus,
                SharedString::from(t!("menu.composition.new")),
                || Box::new(crate::workspace::CompositionNew),
                &colors,
            ))
            .when(has_target, |header| {
                header
                    .child(Self::command_button(
                        "outliner-comp-settings",
                        IconName::Settings,
                        SharedString::from(t!("menu.composition.settings")),
                        || Box::new(crate::workspace::CompositionSettings),
                        &colors,
                    ))
                    .child(Self::command_button(
                        "outliner-comp-duplicate",
                        IconName::Copy,
                        SharedString::from(t!("menu.composition.duplicate")),
                        || Box::new(crate::workspace::CompositionDuplicate),
                        &colors,
                    ))
                    .child(Self::command_button(
                        "outliner-comp-delete",
                        IconName::Delete,
                        SharedString::from(t!("menu.composition.delete")),
                        || Box::new(crate::workspace::CompositionDelete),
                        &colors,
                    ))
            })
    }
}

impl Render for OutlinerGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let mut tree = div()
            .id("outliner-tree")
            .debug_selector(|| "outliner-panel".into())
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .cursor(outliner_row_cursor(self.layer_drag.is_some()));

        // Composition 0 is a legitimate state, not an error: the panel says so
        // and the header's New button is the way out of it.
        if self.rows.is_empty() {
            tree = tree.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("outliner.empty"))),
            );
        } else {
            let rows = self.rows.clone();
            for (index, row) in rows.iter().enumerate() {
                tree = tree.child(self.render_row(index, row, cx));
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.list)
            .track_focus(&self.focus_handle)
            // A reorder ends wherever the button is released: over the empty
            // area below the rows, or outside the panel entirely — otherwise
            // the gesture's live edits would never become an undo step.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.end_layer_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.end_layer_drag(cx);
                }),
            )
            .child(self.render_header(cx))
            .child(tree)
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

    #[test]
    fn layer_reorder_cursor_changes_only_during_drag() {
        assert_eq!(outliner_row_cursor(false), CursorStyle::PointingHand);
        assert_eq!(outliner_row_cursor(true), CursorStyle::ResizeUpDown);
    }

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

        let editor = cx.add_window(|window, cx| {
            NodeEditorPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        let window = cx.add_window(|window, cx| {
            OutlinerGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
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
            self.click_with(cx, index, clicks, LayerClickMode::Replace);
        }

        /// Click with a modifier-derived mode (Shift range, Cmd toggle).
        fn click_with(
            &self,
            cx: &mut TestAppContext,
            index: usize,
            clicks: usize,
            mode: LayerClickMode,
        ) {
            self.window
                .update(cx, |panel, _window, cx| {
                    panel.on_row_click(index, clicks, mode, cx)
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

        /// Click the row the matcher finds with a modifier-derived mode.
        fn click_row_with(
            &self,
            cx: &mut TestAppContext,
            matcher: impl Fn(&OutlinerRow) -> bool,
            mode: LayerClickMode,
        ) {
            let index = self.row_index(cx, matcher);
            self.click_with(cx, index, 1, mode);
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
        cx.update(|cx| {
            assert!(
                matches!(
                    cx.global::<super::super::SelectedPropertiesTarget>().0,
                    super::super::PropertiesTarget::Composition { comp_id } if comp_id == f.other
                ),
                "a composition row publishes itself as the Properties subject, \
                 which is what the composition commands act on"
            );
            assert_eq!(
                super::super::command_target_composition(cx),
                Some(f.other),
                "Settings / Duplicate / Delete follow the clicked row"
            );
        });

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

    /// Shift-clicking ranges over the composition's stack and the platform
    /// modifier toggles, exactly as in the Timeline (REQ-UI-013) — the panels
    /// share both the arithmetic and the selection they write. Several selected
    /// layers close the node editor and switch Properties to the read-only
    /// multi-layer subject.
    #[gpui::test]
    fn modified_row_clicks_build_a_multi_layer_selection(cx: &mut TestAppContext) {
        let f = setup(cx);
        // Stack order in the document: root_layer, b, c.
        let (b, c) = f.project.update(cx, |project, cx| {
            let (b, c) = (LayerId::next(), LayerId::next());
            let doc = ravel_ui::document::add_layer(
                project.document(),
                f.root,
                Layer::new(b, "B", network().0).with_time(0, 0, 100),
            )
            .unwrap();
            let doc = ravel_ui::document::add_layer(
                &doc,
                f.root,
                Layer::new(c, "C", network().0).with_time(0, 0, 100),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (b, c)
        });
        cx.run_until_parked();

        f.click_row(cx, is_layer(f.root_layer), 1);
        f.click_row_with(cx, is_layer(c), LayerClickMode::Range);
        cx.update(|cx| {
            assert_eq!(
                super::super::layer_selection(cx).layers(),
                [f.root_layer, b, c],
                "the range spans the stack from the anchor"
            );
            assert!(
                matches!(
                    &cx.global::<super::super::SelectedPropertiesTarget>().0,
                    super::super::PropertiesTarget::Layers { comp_id, layer_ids }
                        if *comp_id == f.root && layer_ids == &vec![f.root_layer, b, c]
                ),
                "Properties inspects the whole selection"
            );
        });
        f.editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(
                    editor.context(),
                    None,
                    "a single-layer editor closes for a multi-layer selection"
                );
            })
            .unwrap();

        f.click_row_with(cx, is_layer(b), LayerClickMode::Toggle);
        cx.update(|cx| {
            assert_eq!(
                super::super::layer_selection(cx).layers(),
                [f.root_layer, c],
                "the toggle drops the clicked layer"
            );
        });

        // Back to one layer: the editor reopens that layer's network.
        f.click_row(cx, is_layer(c), 1);
        cx.update(|cx| {
            assert_eq!(super::super::layer_selection(cx).layers(), [c]);
        });
        f.editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(f.root, c)));
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
            })
            .unwrap();
    }

    // ----- layer operations (REQ-UI-013 unit 5) ------------------------------

    impl Fixture {
        /// Add a second layer on top of the active composition's stack.
        fn add_layer(&self, cx: &mut TestAppContext, name: &str) -> LayerId {
            let comp = self.root;
            let id = LayerId::next();
            self.project.update(cx, |project, cx| {
                let (network, _) = network();
                let doc = ravel_ui::document::add_layer(
                    project.document(),
                    comp,
                    Layer::new(id, name, network).with_time(0, 0, 100),
                )
                .unwrap();
                project.commit_document(doc, InvalidationHint::Structural, cx);
            });
            cx.run_until_parked();
            id
        }

        /// Layer names bottom-most first — the composition's own stack order,
        /// which the Timeline and the Outliner both display top-most first.
        fn stack(&self, cx: &mut TestAppContext) -> Vec<String> {
            self.project.read_with(cx, |project, _| {
                project
                    .document()
                    .get_composition(self.root)
                    .unwrap()
                    .layers
                    .iter()
                    .map(|layer| layer.name.clone())
                    .collect()
            })
        }
    }

    /// Dragging a row onto another row of the same composition reorders the
    /// stack, and the whole gesture is one undo step.
    #[gpui::test]
    fn dragging_a_layer_row_reorders_the_stack_in_one_undo_step(cx: &mut TestAppContext) {
        let f = setup(cx);
        let top = f.add_layer(cx, "Top layer");
        assert_eq!(f.stack(cx), ["Root layer", "Top layer"]);

        // Drag the top row (index 1 in the tree) onto the bottom layer's row.
        let bottom_row = f.row_index(cx, is_layer(f.root_layer));
        f.window
            .update(cx, |panel, _window, cx| {
                panel.start_layer_drag(f.root, top);
                panel.drag_over_row(bottom_row, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Top layer", "Root layer"],
            "the dragged layer takes the target's place"
        );

        // The mouse-up turns the gesture's live edits into one undo step.
        f.window
            .update(cx, |panel, _window, cx| panel.end_layer_drag(cx))
            .unwrap();
        cx.run_until_parked();

        f.project.update(cx, |project, cx| project.undo(cx));
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Root layer", "Top layer"],
            "one undo restores the order the drag started from"
        );
    }

    /// A drag that never leaves its own row changes nothing and records nothing.
    #[gpui::test]
    fn a_drag_that_does_not_move_records_no_undo_step(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.add_layer(cx, "Top layer");
        let before = f
            .project
            .read_with(cx, |project, _| project.document().clone());
        let own_row = f.row_index(cx, is_layer(f.root_layer));

        f.window
            .update(cx, |panel, _window, cx| {
                panel.start_layer_drag(f.root, f.root_layer);
                panel.drag_over_row(own_row, cx);
                panel.end_layer_drag(cx);
            })
            .unwrap();
        cx.run_until_parked();

        f.project.read_with(cx, |project, _| {
            assert!(*project.document() == before, "no document edit");
        });
    }

    /// A row of a composition that is not active cannot be dragged into the
    /// active one's stack.
    #[gpui::test]
    fn a_drag_never_crosses_compositions(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.expand_layer(cx, f.other, f.other_layer);
        let other_row = f.row_index(cx, is_layer(f.other_layer));

        f.window
            .update(cx, |panel, _window, cx| {
                panel.start_layer_drag(f.root, f.root_layer);
                panel.drag_over_row(other_row, cx);
            })
            .unwrap();
        cx.run_until_parked();

        f.project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(f.other)
                    .unwrap()
                    .layer_count(),
                1,
                "the other composition is untouched"
            );
            assert_eq!(
                project
                    .document()
                    .get_composition(f.root)
                    .unwrap()
                    .layer_count(),
                1
            );
        });
    }

    /// Renaming commits the edited name once; a blank name is not an edit.
    #[gpui::test]
    fn renaming_a_layer_commits_once_and_ignores_a_blank_name(cx: &mut TestAppContext) {
        let f = setup(cx);

        f.window
            .update(cx, |panel, window, cx| {
                assert!(
                    panel
                        .begin_rename(f.root, f.root_layer, window, cx)
                        .is_some()
                );
                assert!(panel.rename.is_some());
                panel.commit_rename("Background".into(), cx);
                assert!(panel.rename.is_none(), "committing closes the editor");
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Background"]);

        // A blank name closes the editor without touching the document.
        f.window
            .update(cx, |panel, window, cx| {
                panel.begin_rename(f.root, f.root_layer, window, cx);
                panel.commit_rename("   ".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Background"], "a blank name is not an edit");

        f.project.update(cx, |project, cx| project.undo(cx));
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Root layer"],
            "one undo restores the original name"
        );
    }

    /// Duplicate inserts the copy above the original and selects it; Delete
    /// removes the layer and drops a selection that pointed at it.
    #[gpui::test]
    fn duplicating_and_deleting_a_layer_row(cx: &mut TestAppContext) {
        let f = setup(cx);

        f.window
            .update(cx, |panel, _window, cx| {
                panel.duplicate_layer(f.root, f.root_layer, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Root layer", "Root layer copy"],
            "the copy sits directly above its source"
        );
        let copy = cx.update(|cx| super::super::selected_layer(cx)).unwrap();
        assert_ne!(copy, f.root_layer, "the copy is selected, not the source");

        f.window
            .update(cx, |panel, _window, cx| {
                panel.delete_layer(f.root, copy, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Root layer"]);
        cx.update(|cx| {
            assert!(
                super::super::layer_selection(cx).is_empty(),
                "deleting the selected layer clears the selection"
            );
        });

        f.project.update(cx, |project, cx| project.undo(cx));
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Root layer", "Root layer copy"]);
    }

    /// A rename left open when its layer disappears is dropped, and committing
    /// it afterwards is not an edit.
    #[gpui::test]
    fn a_rename_of_a_vanished_layer_commits_nothing(cx: &mut TestAppContext) {
        let f = setup(cx);

        f.window
            .update(cx, |panel, window, cx| {
                panel.begin_rename(f.root, f.root_layer, window, cx);
            })
            .unwrap();
        f.window
            .update(cx, |panel, _window, cx| {
                panel.delete_layer(f.root, f.root_layer, cx)
            })
            .unwrap();
        cx.run_until_parked();

        f.window
            .update(cx, |panel, _window, cx| {
                assert!(
                    panel.rename.is_none(),
                    "the editor closes with the row it belonged to"
                );
                // A late blur must not recreate or rename anything.
                panel.commit_rename("Ghost".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();

        f.project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(f.root)
                    .unwrap()
                    .layer_count(),
                0
            );
        });
    }

    /// Duplicate and Delete aimed at a row of a multi-selection act on the whole
    /// selection, each as one undo step (REQ-UI-013 bulk editing).
    #[gpui::test]
    fn bulk_duplicate_and_delete_act_on_the_whole_selection(cx: &mut TestAppContext) {
        let f = setup(cx);
        let second = f.project.update(cx, |project, cx| {
            let second = LayerId::next();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                f.root,
                Layer::new(second, "Second", network().0).with_time(0, 0, 100),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            second
        });
        cx.run_until_parked();

        f.click_row(cx, is_layer(f.root_layer), 1);
        f.click_row_with(cx, is_layer(second), LayerClickMode::Toggle);
        f.window
            .update(cx, |panel, _window, cx| {
                panel.duplicate_layer(f.root, f.root_layer, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Root layer", "Root layer copy", "Second", "Second copy"],
            "each selected layer gained a copy above it"
        );
        let copies = cx.update(|cx| super::super::layer_selection(cx).layers().to_vec());
        assert_eq!(copies.len(), 2, "the copies are selected");
        assert!(!copies.contains(&f.root_layer) && !copies.contains(&second));

        f.window
            .update(cx, |panel, _window, cx| {
                panel.delete_layer(f.root, copies[0], cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Root layer", "Second"]);
        cx.update(|cx| assert!(super::super::layer_selection(cx).is_empty()));

        f.project.update(cx, |project, cx| project.undo(cx));
        cx.run_until_parked();
        assert_eq!(
            f.stack(cx),
            ["Root layer", "Root layer copy", "Second", "Second copy"],
            "one undo brings back every deleted copy"
        );
    }

    /// A locked layer is protected from deletion, exactly as in the Timeline.
    #[gpui::test]
    fn a_locked_layer_cannot_be_deleted(cx: &mut TestAppContext) {
        let f = setup(cx);
        f.project.update(cx, |project, cx| {
            let doc = ravel_ui::document::update_layer(
                project.document(),
                f.root,
                f.root_layer,
                |layer| layer.locked = true,
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        f.window
            .update(cx, |panel, _window, cx| {
                assert!(panel.layer_is_locked(f.root, f.root_layer, cx));
                panel.delete_layer(f.root, f.root_layer, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(f.stack(cx), ["Root layer"], "the locked layer survives");
    }

    /// A tree far taller and wider than the panel, so every row overflows and
    /// every label outruns the available width. Returns a visual context whose
    /// window is already too small to fit it.
    fn overflowing_tree(cx: &mut TestAppContext) -> (Fixture, gpui::VisualTestContext) {
        let f = setup(cx);
        f.project.update(cx, |project, cx| {
            // The first row — the one the debug selectors can address — is the
            // root composition, so its name carries the long-label case.
            let mut doc = ravel_ui::document::update_composition(project.document(), f.root, |c| {
                Composition {
                    name: "Shape 1 copy copy copy copy copy copy copy copy".into(),
                    ..c
                }
            })
            .unwrap();
            for i in 0..40 {
                let (network, _) = network();
                doc = ravel_ui::document::add_layer(
                    &doc,
                    f.root,
                    Layer::new(
                        LayerId::next(),
                        format!("Shape {i} copy copy copy copy copy copy copy copy"),
                        network,
                    )
                    .with_time(0, 0, 100),
                )
                .unwrap();
            }
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        let visual = gpui::VisualTestContext::from_window(f.window.into(), cx);
        visual.simulate_resize(size(px(200.0), px(140.0)));
        cx.run_until_parked();
        (f, visual)
    }

    /// A name too long for the panel ellipsizes on one line (regression: it
    /// wrapped onto a second and third line, overflowing the fixed row height
    /// and pushing the trailing badges out of view).
    #[gpui::test]
    fn a_row_label_too_long_for_the_panel_stays_on_one_line(cx: &mut TestAppContext) {
        let (_f, mut visual) = overflowing_tree(cx);

        let panel = visual.debug_bounds("outliner-panel").expect("tree bounds");
        let label = visual
            .debug_bounds("outliner-row-first-label")
            .expect("first row label bounds");

        assert!(
            label.size.width <= panel.size.width,
            "label {:?} must stay inside the tree {:?}",
            label.size,
            panel.size,
        );
        assert!(
            label.size.height <= px(ROW_HEIGHT),
            "label {:?} must stay on one line within the {ROW_HEIGHT}px row",
            label.size,
        );
    }

    /// A tree taller than the panel scrolls (regression: shrinkable rows let
    /// the flex container squash the list into the panel height, so the scroll
    /// container never had anything to scroll).
    #[gpui::test]
    fn an_overflowing_tree_keeps_its_row_height_and_scrolls(cx: &mut TestAppContext) {
        let (_f, mut visual) = overflowing_tree(cx);

        let panel = visual.debug_bounds("outliner-panel").expect("tree bounds");
        let row = visual
            .debug_bounds("outliner-row-first")
            .expect("first row bounds");
        assert_eq!(
            row.size.height,
            px(ROW_HEIGHT),
            "a row must not shrink, or the tree never overflows and cannot scroll",
        );

        // The overflow is genuinely scrollable: a wheel event over the tree
        // moves the rows up.
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: panel.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-60.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        let scrolled = visual
            .debug_bounds("outliner-row-first")
            .expect("first row bounds after scrolling");
        assert!(
            scrolled.origin.y < row.origin.y,
            "the wheel must scroll the tree: first row moved from {:?} to {:?}",
            row.origin,
            scrolled.origin,
        );
    }
}

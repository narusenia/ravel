// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal Viewer panel: displays the FrameBuffer from the current evaluation
//! result. `ProjectState`'s background evaluation publishes the outcome via
//! [`super::ViewerFrame`]; this panel converts a frame into a GPUI
//! [`RenderImage`] once per update and draws it with the `img` element (one
//! textured quad) instead of the previous per-pixel-run `paint_quad` ladder,
//! which degraded to one quad per pixel on gradient/media content. A failed
//! evaluation drops the stale frame and shows a black frame with a small
//! error overlay, so structural edits (e.g. deleting a Geometry node feeding
//! a Rasterize) are immediately visible instead of leaving stale content.

mod viewport;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, Selectable as _, Sizable as _};
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use ravel_core::types::FrameBuffer;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use smallvec::SmallVec;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::{
    CanvasSelection, ToolState, ViewerFrame, is_panel_focused, tab_title, track_panel_focus,
};
use crate::assets::RavelIcon;
use crate::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::id::{CompId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use ravel_core::runtime::InvalidationHint;
use ravel_ui::document::NetworkPath;
use viewport::ViewerViewport;

use super::param_edit::edited_float_param;

pub const KEY_CONTEXT: &str = "Viewer";

#[derive(Clone, Copy)]
struct PanDrag {
    pointer_start: (f32, f32),
    offset_start: (f32, f32),
}

#[derive(Clone)]
struct MoveOrigin {
    node: NodeId,
    center: (f32, f32),
    path_points: Option<Vec<ravel_core::graph::PathPoint>>,
}

/// One network taking part in a move drag: its shape-node origins and the
/// layer-local frame the parameter writes target (REQ-LAYER-006 — each layer has
/// its own local time, so a multi-layer drag cannot share one frame).
#[derive(Clone)]
struct MoveTarget {
    network: NetworkPath,
    origins: Vec<MoveOrigin>,
    local_frame: u64,
}

/// A move gesture over one or more networks. A node selection contributes one
/// target (the open network); a multi-layer selection contributes one per
/// selected layer, and the whole gesture is still a single undo step
/// (REQ-UI-013).
#[derive(Clone)]
struct MoveDrag {
    pointer_start: (f32, f32),
    targets: Vec<MoveTarget>,
    original_document: Document,
    changed: bool,
}

impl MoveDrag {
    /// Every node the gesture writes, for the invalidation hint.
    fn node_ids(&self) -> Vec<NodeId> {
        self.targets
            .iter()
            .flat_map(|target| target.origins.iter().map(|origin| origin.node))
            .collect()
    }
}

/// The shape a drawing-tool drag creates (REQ-UI-011 unit 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShapeDrawKind {
    Rect,
    Ellipse,
}

impl ShapeDrawKind {
    fn from_tool(tool: ravel_ui::ToolKind) -> Option<Self> {
        match tool {
            ravel_ui::ToolKind::Rect => Some(Self::Rect),
            ravel_ui::ToolKind::Ellipse => Some(Self::Ellipse),
            _ => None,
        }
    }

    fn type_key(self) -> &'static str {
        match self {
            Self::Rect => "shape.rect",
            Self::Ellipse => "shape.ellipse",
        }
    }
}

/// Drag-derived shape extents in comp space: `center` plus the half extents
/// (rect half width/height, ellipse radii).
#[derive(Clone, Copy, Debug, PartialEq)]
struct DragGeometry {
    center: (f32, f32),
    half: (f32, f32),
}

/// State of a created-but-uncommitted shape drag.
#[derive(Clone)]
struct CreatedShape {
    network: NetworkPath,
    node: NodeId,
    /// Last applied geometry: a release at zero extent cancels instead of
    /// committing an invisible zero-size shape.
    geo: DragGeometry,
}

/// Rect/Ellipse tool drag. The node is created on the first mouse move, not
/// on mouse-down, so a plain click leaves the document (and the selection)
/// untouched.
#[derive(Clone)]
struct ShapeDrag {
    kind: ShapeDrawKind,
    /// Comp-space drag start.
    start: (f32, f32),
    /// Selection from before the creation, restored on Escape cancel.
    previous_selection: CanvasSelection,
    original_document: Document,
    created: Option<CreatedShape>,
}

#[derive(Clone)]
struct PenSession {
    network: NetworkPath,
    node: NodeId,
    previous_selection: CanvasSelection,
    original_document: Document,
    active_point: Option<usize>,
    drag_start: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathHandleKind {
    Point,
    InTangent,
    OutTangent,
}

#[derive(Clone)]
struct PathEditDrag {
    network: NetworkPath,
    node: NodeId,
    point: usize,
    handle: PathHandleKind,
    original: Vec<ravel_core::graph::PathPoint>,
    closed: bool,
    pointer_start: (f32, f32),
    original_document: Document,
    changed: bool,
}

pub struct ViewerPanel {
    /// The current frame converted for GPUI rendering. Rebuilt only when
    /// [`ViewerFrame`] changes, never during `render()`.
    image: Option<Arc<RenderImage>>,
    /// The latest evaluation error, shown over the composition's black quad.
    error: Option<SharedString>,
    composition_resolution: Option<(u32, u32)>,
    viewport: ViewerViewport,
    viewport_origin: Rc<Cell<(f32, f32)>>,
    viewport_size: Rc<Cell<(f32, f32)>>,
    pan_drag: Option<PanDrag>,
    move_drag: Option<MoveDrag>,
    shape_drag: Option<ShapeDrag>,
    pen_session: Option<PenSession>,
    path_edit_drag: Option<PathEditDrag>,
    /// Proportional (3x3) grid overlay toggle.
    show_grid: bool,
    /// Action-safe (90%) / title-safe (80%) overlay toggle.
    show_safe_areas: bool,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    viewer_sub: Subscription,
    #[allow(dead_code)]
    tool_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    layer_selection_sub: Subscription,
}

impl ViewerPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(PanelKind::Viewer, &focus_handle, window, cx);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| cx.notify());
        let tool_sub = cx.observe_global::<ToolState>(|this, cx| {
            let state = cx.try_global::<ToolState>().cloned().unwrap_or_default();
            // A deliberate tool switch ends the multi-click pen transaction
            // before another tool can mutate the same uncommitted document.
            // H-hold is transient navigation and must preserve the session.
            if this.pen_session.is_some()
                && state.active != ravel_ui::ToolKind::Pen
                && !state.hand_hold
            {
                this.finalize_pen_session(false, cx);
            }
            cx.notify();
        });
        let selection_sub = cx.observe_global::<CanvasSelection>(|this, cx| {
            // Node Editor delete/undo can invalidate a gesture target while
            // the Viewer is not receiving pointer events. Release every
            // stale gesture here so subsequent tools route normally.
            if this
                .pen_session
                .as_ref()
                .is_some_and(|session| this.session_points(session, cx).is_none())
            {
                this.pen_session = None;
            }
            if this.path_edit_drag.as_ref().is_some_and(|drag| {
                !document_has_node(&drag.network, drag.node, this.project(cx), cx)
            }) {
                this.path_edit_drag = None;
            }
            if this.move_drag.as_ref().is_some_and(|drag| {
                drag.targets.iter().any(|target| {
                    target.origins.iter().any(|origin| {
                        !document_has_node(&target.network, origin.node, this.project(cx), cx)
                    })
                })
            }) {
                this.move_drag = None;
            }
            cx.notify();
        });

        // The layer bboxes are drawn from the shared layer selection, and the
        // multi-layer move gesture belongs to it: a selection that no longer
        // holds a dragged layer ends the drag instead of moving what is no
        // longer selected.
        let layer_selection_sub = cx.observe_global::<super::LayerSelection>(|this, cx| {
            let selection = super::layer_selection(cx);
            if this.move_drag.as_ref().is_some_and(|drag| {
                drag.targets.iter().any(|target| {
                    selection.comp() != Some(target.network.comp)
                        || !selection.contains(target.network.layer)
                })
            }) {
                // Cancel rather than forget: the gesture has uncommitted
                // document updates, and no one else would revert them — unlike
                // the node path above, where the document already changed
                // (a deleted node) and reverting would undo that change.
                this.cancel_move(cx);
            }
            cx.notify();
        });

        let viewer_sub = cx.observe_global::<ViewerFrame>(|this: &mut Self, cx| {
            let vf = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
            let content = viewer_content(vf);
            this.error = content.error;
            this.composition_resolution = content.composition_resolution;
            // `ImageSource::Render` bypasses gpui's image cache, so atlas
            // entries are only freed by an explicit drop_image. Without this
            // every published frame would leak VRAM (one texture per scrub
            // tick). Deferred so `drop_image` sees every window, including
            // one that may be checked out for the current update.
            if let Some(old) = std::mem::replace(&mut this.image, content.image) {
                cx.defer(move |cx| cx.drop_image(old, None));
            }
            cx.notify();
        });

        // Release the last frame's atlas entry when the panel goes away.
        cx.on_release(|this: &mut Self, cx| {
            if let Some(old) = this.image.take() {
                cx.drop_image(old, None);
            }
        })
        .detach();

        let initial = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
        let content = viewer_content(initial);

        Self {
            image: content.image,
            error: content.error,
            composition_resolution: content.composition_resolution,
            viewport: ViewerViewport::default(),
            viewport_origin: Rc::new(Cell::new((0.0, 0.0))),
            viewport_size: Rc::new(Cell::new((0.0, 0.0))),
            pan_drag: None,
            move_drag: None,
            shape_drag: None,
            pen_session: None,
            path_edit_drag: None,
            show_grid: false,
            show_safe_areas: false,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            viewer_sub,
            tool_sub,
            selection_sub,
            layer_selection_sub,
        }
    }

    /// Current zoom relative to composition pixels (100% = 1 comp px per
    /// screen px). In Fit mode this reflects the current panel size.
    pub fn zoom_percent(&self) -> f32 {
        self.composition_resolution
            .map(|resolution| self.viewport.zoom(self.viewport_size.get(), resolution) * 100.0)
            .unwrap_or(100.0)
    }

    /// Restore resize-aware contain fit.
    pub fn zoom_to_fit(&mut self) {
        self.viewport.zoom_to_fit();
    }

    /// Set an explicit composition-pixel zoom, preserving the panel center.
    pub fn set_zoom_percent(&mut self, percent: f32) {
        let Some(resolution) = self.composition_resolution else {
            return;
        };
        let size = self.viewport_size.get();
        self.viewport.zoom_toward(
            percent / 100.0,
            (size.0 * 0.5, size.1 * 0.5),
            size,
            resolution,
        );
    }

    fn local_position(&self, position: Point<Pixels>) -> (f32, f32) {
        let origin = self.viewport_origin.get();
        (
            <Pixels as Into<f32>>::into(position.x) - origin.0,
            <Pixels as Into<f32>>::into(position.y) - origin.1,
        )
    }

    fn comp_position(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        let resolution = self.composition_resolution?;
        let rect = self.viewport.rect(self.viewport_size.get(), resolution);
        screen_to_comp(self.local_position(position), rect, resolution)
    }

    fn project(&self, cx: &App) -> Option<Entity<ProjectState>> {
        cx.try_global::<ProjectStateHandle>()?.0.upgrade()
    }

    fn publish_selection(network: NetworkPath, nodes: HashSet<NodeId>, cx: &mut App) {
        let target = if nodes.is_empty() {
            super::PropertiesTarget::Empty
        } else {
            let mut ids: Vec<_> = nodes.iter().copied().collect();
            ids.sort_by_key(|id| id.raw());
            super::PropertiesTarget::Nodes {
                network: network.clone(),
                ids,
            }
        };
        cx.set_global(CanvasSelection {
            path: Some(network),
            nodes,
        });
        cx.set_global(super::SelectedPropertiesTarget(target));
    }

    fn select_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if cx
            .try_global::<ToolState>()
            .map(|state| state.active)
            .unwrap_or_default()
            != ravel_ui::ToolKind::Select
        {
            return;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return;
        };
        // Several selected layers: no network is open, so there is nothing to
        // pick inside one — the gesture moves the selected layers instead.
        if super::layer_selection(cx).layers().len() >= 2 {
            self.layer_move_mouse_down(pointer, cx);
            return;
        }
        let Some(selection) = cx.try_global::<CanvasSelection>().cloned() else {
            return;
        };
        let Some(network) = selection.path.clone() else {
            return;
        };
        let Some(position) = cx.try_global::<super::PlaybackPosition>().copied() else {
            return;
        };
        let Some(resolution) = self.composition_resolution else {
            return;
        };
        let Some(project) = self.project(cx) else {
            return;
        };
        let document = project.read(cx).document().clone();
        let Some(comp) = document.get_composition(network.comp) else {
            return;
        };
        let Some(layer) = comp.get_layer(network.layer) else {
            return;
        };
        let Some(graph) = ravel_ui::document::resolve_network(&document, &network) else {
            return;
        };
        let eval = EvalContext::new(position.frame, position.fps, resolution);
        let shell = world_matrix(comp, layer, &eval);
        // Network parameters live in layer-local time (REQ-LAYER-006): the
        // hit test and the drag origins below must sample the same frame the
        // keyframe writes target.
        let local_frame = ravel_ui::keyframes::layer_local_frame(layer, position.frame);
        let hit = hit_test_shape_nodes(graph, pointer, local_frame, &eval, &shell);
        let nodes = selection_after_click(&selection.nodes, hit, event.modifiers.shift);
        // Publish both the durable selection and its Properties projection,
        // including a plain click on an already-selected node. This mirrors
        // the Node Editor and restores node Properties if another panel had
        // temporarily published a different target.
        Self::publish_selection(network.clone(), nodes.clone(), cx);

        if event.modifiers.shift || hit.is_none() || !shell.is_identity() {
            return;
        }
        let origins: Vec<_> = nodes
            .iter()
            .filter_map(|id| {
                let node = graph.node(*id)?;
                let bounds = shape_node_bounds(node, local_frame, &eval)?;
                Some(MoveOrigin {
                    node: *id,
                    center: (
                        sample_float_param(node, "center_x", local_frame, &eval)
                            .unwrap_or(bounds.x + bounds.w * 0.5),
                        sample_float_param(node, "center_y", local_frame, &eval)
                            .unwrap_or(bounds.y + bounds.h * 0.5),
                    ),
                    path_points: path_points(node).map(<[ravel_core::graph::PathPoint]>::to_vec),
                })
            })
            .collect();
        if !origins.is_empty() {
            self.move_drag = Some(MoveDrag {
                pointer_start: pointer,
                targets: vec![MoveTarget {
                    network,
                    origins,
                    local_frame,
                }],
                original_document: document,
                changed: false,
            });
        }
    }

    /// Mouse-down with several layers selected: start moving all of them when
    /// the pointer is inside one of their bboxes (REQ-UI-013).
    ///
    /// Only layers whose compositing chain transform is identity take part — the
    /// drag writes comp-space deltas into layer-local `center_x` / `center_y`
    /// parameters (the REQ-UI-011 reconstruction), which is only the same thing
    /// under an identity shell. A transformed layer keeps its bbox but does not
    /// move, exactly as a transformed layer refuses a node move today.
    fn layer_move_mouse_down(&mut self, pointer: (f32, f32), cx: &mut Context<Self>) {
        let selection = super::layer_selection(cx);
        let Some(comp_id) = selection.comp() else {
            return;
        };
        let Some(position) = cx.try_global::<super::PlaybackPosition>().copied() else {
            return;
        };
        let Some(resolution) = self.composition_resolution else {
            return;
        };
        let Some(project) = self.project(cx) else {
            return;
        };
        let document = project.read(cx).document().clone();
        let Some(comp) = document.get_composition(comp_id) else {
            return;
        };
        let eval = EvalContext::new(position.frame, position.fps, resolution);

        let mut hit = false;
        let mut targets = Vec::new();
        for layer_id in selection.layers() {
            let Some(layer) = comp.get_layer(*layer_id) else {
                continue;
            };
            let Some(rect) = layer_comp_rect(comp, layer, position.frame, &eval) else {
                continue;
            };
            let shell = world_matrix(comp, layer, &eval);
            if !shell.is_identity() {
                // A transformed layer is not movable, so pressing inside its
                // bbox must not drag the rest of the selection either: the
                // press has to land on something this gesture can actually move.
                continue;
            }
            let local_frame = ravel_ui::keyframes::layer_local_frame(layer, position.frame);
            let origins: Vec<MoveOrigin> = layer_shape_nodes(layer, local_frame, &eval)
                .into_iter()
                .filter_map(|node| {
                    let bounds = shape_node_bounds(node, local_frame, &eval)?;
                    Some(MoveOrigin {
                        node: node.id,
                        center: (
                            sample_float_param(node, "center_x", local_frame, &eval)
                                .unwrap_or(bounds.x + bounds.w * 0.5),
                            sample_float_param(node, "center_y", local_frame, &eval)
                                .unwrap_or(bounds.y + bounds.h * 0.5),
                        ),
                        path_points: path_points(node)
                            .map(<[ravel_core::graph::PathPoint]>::to_vec),
                    })
                })
                .collect();
            if origins.is_empty() {
                continue;
            }
            hit |= rect_contains(&rect, pointer);
            targets.push(MoveTarget {
                network: NetworkPath::layer(comp_id, *layer_id),
                origins,
                local_frame,
            });
        }
        // A click outside every selected layer is not a move: it leaves the
        // selection alone (the panels that own it decide deselection).
        if !hit || targets.is_empty() {
            return;
        }
        self.move_drag = Some(MoveDrag {
            pointer_start: pointer,
            targets,
            original_document: document,
            changed: false,
        });
    }

    fn move_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.move_drag.clone() else {
            return;
        };
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        // A zero delta still re-applies the origins: dragging away and back
        // to the start must restore the original centers instead of leaving
        // the last nonzero preview in the document.
        let delta = (
            pointer.0 - drag.pointer_start.0,
            pointer.1 - drag.pointer_start.1,
        );
        let Some(project) = self.project(cx) else {
            return;
        };
        let ids = drag.node_ids();
        let mut applied = false;
        project.update(cx, |project, cx| {
            // Every target's edit lands in ONE document, so a multi-layer move
            // is one preview and — through `move_ended` — one undo step.
            let mut document = project.document().clone();
            for target in &drag.targets {
                let Some(mut graph) =
                    ravel_ui::document::resolve_network(&document, &target.network).cloned()
                else {
                    continue;
                };
                let mut target_applied = false;
                for origin in &target.origins {
                    let Some(node) = graph.node(origin.node) else {
                        continue;
                    };
                    let Some(updated) = moved_shape_node(
                        node,
                        origin.center,
                        origin.path_points.as_deref(),
                        delta,
                        target.local_frame,
                    ) else {
                        continue;
                    };
                    graph = graph.replace_node(Arc::new(updated));
                    target_applied = true;
                }
                if !target_applied {
                    continue;
                }
                let Some(next) =
                    ravel_ui::document::replace_network(&document, &target.network, graph)
                else {
                    continue;
                };
                document = next;
                applied = true;
            }
            if !applied {
                return;
            }
            project.apply_document(document, InvalidationHint::Params(ids.clone()), cx);
        });
        if applied {
            // `changed` tracks the LAST applied delta: a gesture released at
            // its start point needs neither a commit (mouse-up) nor a revert
            // (Escape) — the applied document already matches the committed
            // snapshot.
            if let Some(active) = &mut self.move_drag {
                active.changed = delta != (0.0, 0.0);
            }
            cx.notify();
        }
    }

    fn move_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.move_drag.take() else {
            return;
        };
        if !drag.changed {
            return;
        }
        let ids = drag.node_ids();
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.commit_document(
                    project.document().clone(),
                    InvalidationHint::Params(ids),
                    cx,
                );
            });
        }
        cx.notify();
    }

    fn cancel_move(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.move_drag.take() else {
            return;
        };
        if !drag.changed {
            return;
        }
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.restore_document_snapshot(drag.original_document, cx);
            });
        }
        cx.notify();
    }

    /// Restore a selection captured before a cancelled shape creation,
    /// including the "no network open" state that [`Self::publish_selection`]
    /// cannot express.
    fn restore_selection(selection: CanvasSelection, cx: &mut App) {
        let target = match &selection.path {
            Some(network) if !selection.nodes.is_empty() => {
                let mut ids: Vec<_> = selection.nodes.iter().copied().collect();
                ids.sort_by_key(|id| id.raw());
                super::PropertiesTarget::Nodes {
                    network: network.clone(),
                    ids,
                }
            }
            _ => super::PropertiesTarget::Empty,
        };
        cx.set_global(selection);
        cx.set_global(super::SelectedPropertiesTarget(target));
    }

    /// Rect/Ellipse tool mouse-down: record the pending drag. Nothing is
    /// created yet — a click without a drag must not touch the document.
    fn shape_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let tool = cx
            .try_global::<ToolState>()
            .map(|state| state.active)
            .unwrap_or_default();
        let Some(kind) = ShapeDrawKind::from_tool(tool) else {
            return;
        };
        let Some(pointer) = self.comp_position(event.position) else {
            return;
        };
        let previous_selection = cx
            .try_global::<CanvasSelection>()
            .cloned()
            .unwrap_or_default();
        let Some(project) = self.project(cx) else {
            return;
        };
        let original_document = project.read(cx).document().clone();
        // The drag writes comp-space coordinates into layer-local node
        // parameters, so — like the move tool — drawing is only possible on
        // layers whose shell transform is identity (inverse-transform
        // editing is v2). Layers auto-created for the gesture always have an
        // identity shell.
        if let Some(path) = &previous_selection.path {
            let Some(position) = cx.try_global::<super::PlaybackPosition>().copied() else {
                return;
            };
            let Some(resolution) = self.composition_resolution else {
                return;
            };
            let Some(project) = self.project(cx) else {
                return;
            };
            let document = project.read(cx).document();
            let Some(comp) = document.get_composition(path.comp) else {
                return;
            };
            let Some(layer) = comp.get_layer(path.layer) else {
                return;
            };
            let eval = EvalContext::new(position.frame, position.fps, resolution);
            let shell = world_matrix(comp, layer, &eval);
            if !shell.is_identity() {
                return;
            }
        }
        self.shape_drag = Some(ShapeDrag {
            kind,
            start: pointer,
            previous_selection,
            original_document,
            created: None,
        });
    }

    fn shape_dragged(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.shape_drag.clone() else {
            return;
        };
        let Some(pointer) = self.comp_position(event.position) else {
            return;
        };
        let geo = drag_geometry(
            drag.start,
            pointer,
            event.modifiers.shift,
            event.modifiers.alt,
        );
        let Some(project) = self.project(cx) else {
            return;
        };
        match &drag.created {
            // Live preview: overwrite the new node's parameters (plain Floats
            // on a freshly created node) without recording history.
            Some(created) => {
                let mut applied = false;
                project.update(cx, |project, cx| {
                    let document = project.document();
                    let Some(mut graph) =
                        ravel_ui::document::resolve_network(document, &created.network).cloned()
                    else {
                        return;
                    };
                    let Some(node) = graph.node(created.node) else {
                        return;
                    };
                    let updated = drawn_shape_node(node.as_ref().clone(), drag.kind, geo);
                    graph = graph.replace_node(Arc::new(updated));
                    let Some(document) = ravel_ui::document::replace_network(
                        project.document(),
                        &created.network,
                        graph,
                    ) else {
                        return;
                    };
                    project.apply_document(
                        document,
                        InvalidationHint::Params(vec![created.node]),
                        cx,
                    );
                    applied = true;
                });
                if applied {
                    if let Some(active) = &mut self.shape_drag
                        && let Some(created) = &mut active.created
                    {
                        created.geo = geo;
                    }
                    cx.notify();
                }
            }
            // First actual drag: create the Shape template layer when no
            // network is open, then the node plus its auto-wiring, all as one
            // uncommitted document update so the whole gesture stays a single
            // undo step.
            None => {
                let active_path = cx
                    .try_global::<CanvasSelection>()
                    .and_then(|selection| selection.path.clone());
                let mut created = None;
                project.update(cx, |project, cx| {
                    let document = project.document().clone();
                    let created_shape = match active_path {
                        Some(path) => {
                            create_drawn_shape(&document, &path, project.registry(), drag.kind, geo)
                                .map(|(doc, node)| (doc, path, node))
                        }
                        None => {
                            // No open network: the new template layer goes
                            // into the composition the UI is editing.
                            let Some(comp) = crate::panels::active_composition(cx) else {
                                return;
                            };
                            create_layer_with_drawn_shape(
                                &document,
                                comp,
                                project.registry(),
                                drag.kind,
                                geo,
                            )
                        }
                    };
                    let Some((document, network, node)) = created_shape else {
                        return;
                    };
                    project.apply_document(document, InvalidationHint::Structural, cx);
                    created = Some((network, node));
                });
                if let Some((network, node)) = created {
                    // Select the new node so the bbox/handles and Properties
                    // track it immediately, exactly like a click selection.
                    Self::publish_selection(network.clone(), HashSet::from([node]), cx);
                    if let Some(active) = &mut self.shape_drag {
                        active.created = Some(CreatedShape { network, node, geo });
                    }
                    cx.notify();
                }
            }
        }
    }

    /// Mouse-up: commit the whole creation (template layer + node + wiring)
    /// as one undo step. A drag released at zero extent creates nothing.
    fn shape_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.shape_drag.take() else {
            return;
        };
        let Some(created) = &drag.created else {
            return;
        };
        if drag_geometry_degenerate(created.geo) {
            self.shape_drag = Some(drag);
            self.cancel_shape(cx);
            return;
        }
        let node = created.node;
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.commit_document(
                    project.document().clone(),
                    InvalidationHint::Params(vec![node]),
                    cx,
                );
            });
        }
        cx.notify();
    }

    /// Escape / lost-button cancel: revert the uncommitted creation (removing
    /// an auto-created template layer with it) and restore the selection.
    fn cancel_shape(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.shape_drag.take() else {
            return;
        };
        if drag.created.is_none() {
            return;
        }
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.restore_document_snapshot(drag.original_document, cx);
            });
        }
        Self::restore_selection(drag.previous_selection, cx);
        cx.notify();
    }

    fn path_handle_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        let tool = cx
            .try_global::<ToolState>()
            .map(|state| state.active)
            .unwrap_or_default();
        if !matches!(tool, ravel_ui::ToolKind::Select | ravel_ui::ToolKind::Pen)
            || self.pen_session.is_some()
        {
            return false;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return false;
        };
        let Some(selection) = cx.try_global::<CanvasSelection>().cloned() else {
            return false;
        };
        let Some(network) = selection.path.clone() else {
            return false;
        };
        let selected: Vec<_> = selection.nodes.iter().copied().collect();
        let [node] = selected.as_slice() else {
            return false;
        };
        let node = *node;
        let Some(project) = self.project(cx) else {
            return false;
        };
        let document = project.read(cx).document();
        let Some(comp) = document.get_composition(network.comp) else {
            return false;
        };
        let Some(layer) = comp.get_layer(network.layer) else {
            return false;
        };
        let Some(resolution) = self.composition_resolution else {
            return false;
        };
        let Some(position) = cx.try_global::<super::PlaybackPosition>().copied() else {
            return false;
        };
        let eval = EvalContext::new(position.frame, position.fps, resolution);
        if !world_matrix(comp, layer, &eval).is_identity() {
            return false;
        }
        let Some(graph) = ravel_ui::document::resolve_network(document, &network) else {
            return false;
        };
        let Some(points) = graph.node(node).and_then(|node| path_points(node)) else {
            return false;
        };
        let closed = graph.node(node).is_some_and(|node| path_closed(node));
        let threshold = self.comp_hit_radius(8.0).unwrap_or(8.0);
        let Some((point, handle)) = path_handle_hit(points, pointer, threshold) else {
            return false;
        };
        self.path_edit_drag = Some(PathEditDrag {
            network,
            node,
            point,
            handle,
            original: points.to_vec(),
            closed,
            pointer_start: pointer,
            original_document: document.clone(),
            changed: false,
        });
        true
    }

    fn comp_hit_radius(&self, pixels: f32) -> Option<f32> {
        let resolution = self.composition_resolution?;
        let rect = self.viewport.rect(self.viewport_size.get(), resolution);
        (rect.width > 0.0).then_some(pixels * resolution.0 as f32 / rect.width)
    }

    fn pen_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if cx
            .try_global::<ToolState>()
            .map(|state| state.active)
            .unwrap_or_default()
            != ravel_ui::ToolKind::Pen
        {
            return;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return;
        };

        if let Some(session) = self.pen_session.clone() {
            if let Some(mut points) = self.session_points(&session, cx) {
                let close_radius = self.comp_hit_radius(8.0).unwrap_or(8.0);
                if pen_should_close(&points, pointer, close_radius) {
                    self.finalize_pen_session(true, cx);
                    return;
                }
                points.push(corner_path_point(pointer));
                let active_point = points.len() - 1;
                if self.apply_path_points(&session.network, session.node, points, false, cx)
                    && let Some(active) = &mut self.pen_session
                {
                    active.active_point = Some(active_point);
                    active.drag_start = pointer;
                }
                return;
            }
            // The selected in-progress node may be deleted through the Node
            // Editor. Drop that stale UI transaction and let this same click
            // start a fresh path instead of targeting the missing node.
            self.pen_session = None;
        }

        let previous_selection = cx
            .try_global::<CanvasSelection>()
            .cloned()
            .unwrap_or_default();
        if !self.pen_drawing_allowed(&previous_selection, cx) {
            return;
        }
        let active_path = previous_selection.path.clone();
        let Some(project) = self.project(cx) else {
            return;
        };
        let mut created = None;
        let original_document = project.read(cx).document().clone();
        project.update(cx, |project, cx| {
            let document = project.document().clone();
            let result = match active_path {
                Some(ref path) => create_custom_path(
                    &document,
                    path,
                    project.registry(),
                    vec![corner_path_point(pointer)],
                )
                .map(|(doc, node)| (doc, path.clone(), node)),
                None => {
                    let comp = crate::panels::active_composition(cx)?;
                    create_layer_with_custom_path(
                        &document,
                        comp,
                        project.registry(),
                        vec![corner_path_point(pointer)],
                    )
                }
            };
            let (document, network, node) = result?;
            project.apply_document(document, InvalidationHint::Structural, cx);
            created = Some((network, node));
            Some(())
        });
        if let Some((network, node)) = created {
            Self::publish_selection(network.clone(), HashSet::from([node]), cx);
            self.pen_session = Some(PenSession {
                network,
                node,
                previous_selection,
                original_document,
                active_point: Some(0),
                drag_start: pointer,
            });
            cx.notify();
        }
    }

    fn pen_drawing_allowed(&self, selection: &CanvasSelection, cx: &App) -> bool {
        let Some(path) = &selection.path else {
            return true;
        };
        let Some(project) = self.project(cx) else {
            return false;
        };
        let document = project.read(cx).document();
        let Some(comp) = document.get_composition(path.comp) else {
            return false;
        };
        let Some(layer) = comp.get_layer(path.layer) else {
            return false;
        };
        let Some(position) = cx.try_global::<super::PlaybackPosition>().copied() else {
            return false;
        };
        let Some(resolution) = self.composition_resolution else {
            return false;
        };
        let eval = EvalContext::new(position.frame, position.fps, resolution);
        world_matrix(comp, layer, &eval).is_identity()
    }

    fn session_points(
        &self,
        session: &PenSession,
        cx: &App,
    ) -> Option<Vec<ravel_core::graph::PathPoint>> {
        let project = self.project(cx)?;
        let document = project.read(cx).document();
        let graph = ravel_ui::document::resolve_network(document, &session.network)?;
        Some(path_points(graph.node(session.node)?)?.to_vec())
    }

    fn apply_path_points(
        &self,
        network: &NetworkPath,
        node: NodeId,
        points: Vec<ravel_core::graph::PathPoint>,
        closed: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(project) = self.project(cx) else {
            return false;
        };
        let mut applied = false;
        project.update(cx, |project, cx| {
            let Some(mut graph) =
                ravel_ui::document::resolve_network(project.document(), network).cloned()
            else {
                return;
            };
            let Some(current) = graph.node(node) else {
                return;
            };
            let updated = custom_path_node(current.as_ref().clone(), points.clone(), closed);
            graph = graph.replace_node(Arc::new(updated));
            let Some(document) =
                ravel_ui::document::replace_network(project.document(), network, graph)
            else {
                return;
            };
            project.apply_document(document, InvalidationHint::Params(vec![node]), cx);
            applied = true;
        });
        if applied {
            cx.notify();
        }
        applied
    }

    fn pen_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(session) = self.pen_session.clone() else {
            return;
        };
        let Some(index) = session.active_point else {
            return;
        };
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        let Some(mut points) = self.session_points(&session, cx) else {
            return;
        };
        let Some(point) = points.get_mut(index) else {
            return;
        };
        *point = smooth_path_point(session.drag_start, pointer);
        self.apply_path_points(&session.network, session.node, points, false, cx);
    }

    fn pen_point_ended(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.pen_session
            && session.active_point.take().is_some()
        {
            cx.notify();
        }
    }

    fn finalize_pen_session(&mut self, closed: bool, cx: &mut Context<Self>) {
        let Some(session) = self.pen_session.take() else {
            return;
        };
        let Some(points) = self.session_points(&session, cx) else {
            // External deletion already committed the document. There is no
            // live preview left to commit or revert.
            cx.notify();
            return;
        };
        if points.len() < 2 {
            if let Some(project) = self.project(cx) {
                project.update(cx, |project, cx| {
                    project.restore_document_snapshot(session.original_document, cx);
                });
            }
            Self::restore_selection(session.previous_selection, cx);
        } else {
            self.apply_path_points(&session.network, session.node, points, closed, cx);
            if let Some(project) = self.project(cx) {
                project.update(cx, |project, cx| {
                    project.commit_document(
                        project.document().clone(),
                        InvalidationHint::Params(vec![session.node]),
                        cx,
                    );
                });
            }
            Self::publish_selection(session.network, HashSet::from([session.node]), cx);
        }
        cx.notify();
    }

    fn path_edit_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.path_edit_drag.clone() else {
            return;
        };
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        let delta = (
            pointer.0 - drag.pointer_start.0,
            pointer.1 - drag.pointer_start.1,
        );
        let points = edited_path_points(&drag.original, drag.point, drag.handle, delta);
        if self.apply_path_points(&drag.network, drag.node, points, drag.closed, cx)
            && let Some(active) = &mut self.path_edit_drag
        {
            active.changed = delta != (0.0, 0.0);
        }
    }

    fn path_edit_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.path_edit_drag.take() else {
            return;
        };
        if drag.changed
            && let Some(project) = self.project(cx)
        {
            project.update(cx, |project, cx| {
                project.commit_document(
                    project.document().clone(),
                    InvalidationHint::Params(vec![drag.node]),
                    cx,
                );
            });
        }
        cx.notify();
    }

    fn cancel_path_edit(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.path_edit_drag.take() else {
            return;
        };
        let changed = drag.changed;
        if changed && let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.restore_document_snapshot(drag.original_document, cx);
            });
        }
        cx.notify();
    }

    fn tool_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let active = cx
            .try_global::<ToolState>()
            .map(|s| s.active)
            .unwrap_or_default();

        const TOOLS: [ravel_ui::ToolKind; 6] = [
            ravel_ui::ToolKind::Select,
            ravel_ui::ToolKind::Pen,
            ravel_ui::ToolKind::Rect,
            ravel_ui::ToolKind::Ellipse,
            ravel_ui::ToolKind::Hand,
            ravel_ui::ToolKind::Zoom,
        ];

        let entity = cx.entity().downgrade();
        let mut row = div()
            .flex()
            .items_center()
            .gap_0p5()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().colors.border);

        for tool in TOOLS {
            let is_active = tool == active;
            let entity = entity.clone();
            let btn = Button::new(SharedString::from(tool.label_key()))
                .icon(Icon::new(RavelIcon::for_tool(tool)).size_3p5())
                .ghost()
                .xsmall()
                .selected(is_active)
                .tooltip(t!(tool.label_key()))
                .on_click(move |_, _window, cx| {
                    entity
                        .update(cx, |_this, cx| {
                            let mut state =
                                cx.try_global::<ToolState>().cloned().unwrap_or_default();
                            state.active = tool;
                            cx.set_global(state);
                            cx.notify();
                        })
                        .ok();
                });
            row = row.child(btn);
        }
        row
    }

    /// AE-style bottom toolbar: zoom readout with preset menu, Fit, 100%,
    /// and the grid / safe-area overlay toggles.
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let zoom_label = SharedString::from(format!("{:.0}%", self.zoom_percent()));
        let entity = cx.entity().downgrade();
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap_1()
            .px_1()
            .py(px(2.0))
            .border_t_1()
            .border_color(cx.theme().colors.border)
            .child(
                Button::new("viewer-zoom-presets")
                    .xsmall()
                    .ghost()
                    .label(zoom_label)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for percent in [25.0f32, 50.0, 100.0, 200.0, 400.0] {
                            let entity = entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(format!("{percent:.0}%")))
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                this.set_zoom_percent(percent);
                                                cx.notify();
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("viewer-fit")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::ZoomFit))
                    .tooltip(t!("viewer.fit"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.zoom_to_fit();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("viewer-actual-size")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::ZoomActualSize))
                    .tooltip(t!("viewer.actual_size"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.set_zoom_percent(100.0);
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("viewer-grid")
                    .xsmall()
                    .ghost()
                    .selected(self.show_grid)
                    .icon(Icon::new(RavelIcon::GridOverlay))
                    .tooltip(t!("viewer.grid"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_grid = !this.show_grid;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("viewer-safe-areas")
                    .xsmall()
                    .ghost()
                    .selected(self.show_safe_areas)
                    .icon(Icon::new(RavelIcon::SafeAreas))
                    .tooltip(t!("viewer.safe_areas"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_safe_areas = !this.show_safe_areas;
                        cx.notify();
                    })),
            )
    }
}

/// Overlay line color: light gray that stays readable over both the black
/// frame and bright content.
fn overlay_line_color() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.3)
}

/// 3x3 proportional grid over the composition rectangle.
fn paint_proportional_grid(window: &mut Window, frame: Bounds<Pixels>) {
    let color = overlay_line_color();
    for i in 1..3 {
        let t = i as f32 / 3.0;
        let x = frame.origin.x + frame.size.width * t;
        window.paint_quad(fill(
            Bounds {
                origin: point(x, frame.origin.y),
                size: size(px(1.0), frame.size.height),
            },
            color,
        ));
        let y = frame.origin.y + frame.size.height * t;
        window.paint_quad(fill(
            Bounds {
                origin: point(frame.origin.x, y),
                size: size(frame.size.width, px(1.0)),
            },
            color,
        ));
    }
}

/// Action-safe (90%) and title-safe (80%) rectangles, centered in the
/// composition rectangle.
fn paint_safe_areas(window: &mut Window, frame: Bounds<Pixels>) {
    for fraction in [0.9f32, 0.8] {
        let width = frame.size.width * fraction;
        let height = frame.size.height * fraction;
        let origin = point(
            frame.origin.x + (frame.size.width - width) * 0.5,
            frame.origin.y + (frame.size.height - height) * 0.5,
        );
        paint_rect_outline(
            window,
            Bounds {
                origin,
                size: size(width, height),
            },
        );
    }
}

/// 1px outline drawn as four quads (`paint_quad` has no stroke mode).
fn paint_rect_outline(window: &mut Window, rect: Bounds<Pixels>) {
    paint_rect_outline_colored(window, rect, overlay_line_color());
}

fn paint_rect_outline_colored(window: &mut Window, rect: Bounds<Pixels>, color: Hsla) {
    let line = px(1.0);
    let edges = [
        Bounds {
            origin: rect.origin,
            size: size(rect.size.width, line),
        },
        Bounds {
            origin: point(rect.origin.x, rect.origin.y + rect.size.height - line),
            size: size(rect.size.width, line),
        },
        Bounds {
            origin: rect.origin,
            size: size(line, rect.size.height),
        },
        Bounds {
            origin: point(rect.origin.x + rect.size.width - line, rect.origin.y),
            size: size(line, rect.size.height),
        },
    ];
    for edge in edges {
        window.paint_quad(fill(edge, color));
    }
}

struct ViewerContent {
    image: Option<Arc<RenderImage>>,
    error: Option<SharedString>,
    composition_resolution: Option<(u32, u32)>,
}

/// Split a published [`ViewerFrame`] into durable panel content. Black is
/// painted as a quad, so Blank and Error do not allocate composition-sized
/// textures.
fn viewer_content(vf: ViewerFrame) -> ViewerContent {
    match vf {
        ViewerFrame::Frame {
            buffer,
            composition_resolution,
        } => ViewerContent {
            image: frame_buffer_to_render_image(&buffer),
            error: None,
            composition_resolution: Some(composition_resolution),
        },
        ViewerFrame::Blank {
            composition_resolution,
        } => ViewerContent {
            image: None,
            error: None,
            composition_resolution,
        },
        ViewerFrame::Error {
            message,
            composition_resolution,
        } => ViewerContent {
            image: None,
            error: Some(message),
            composition_resolution,
        },
    }
}

impl Panel for ViewerPanel {
    fn panel_name(&self) -> &'static str {
        "viewer"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let display = t!(PanelKind::Viewer.label_key());
        let focused = is_panel_focused(PanelKind::Viewer, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        tab_title(Some(PanelKind::Viewer), SharedString::from(display), color)
    }
}

impl EventEmitter<PanelEvent> for ViewerPanel {}

impl Focusable for ViewerPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ViewerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors.border;
        let bg = cx.theme().colors.background;

        let viewport = self.viewport;
        let composition_resolution = self.composition_resolution;
        let image = self.image.clone();
        let viewport_origin = self.viewport_origin.clone();
        let viewport_size = self.viewport_size.clone();
        let show_grid = self.show_grid;
        let show_safe_areas = self.show_safe_areas;

        let bbox_rects: Vec<CompRect> = (|| {
            let sel = cx.try_global::<CanvasSelection>()?.clone();
            let comp_res = composition_resolution?;
            let pos = cx.try_global::<super::PlaybackPosition>().copied()?;
            let project = cx.try_global::<ProjectStateHandle>()?.0.upgrade()?;
            let doc = project.read(cx).document().clone();
            Some(selection_comp_rects(
                &sel, &doc, pos.frame, pos.fps, comp_res,
            ))
        })()
        .unwrap_or_default();
        // Layer-level bboxes stand in for node bboxes exactly when several
        // layers are selected (REQ-UI-013): no network is open then, so there is
        // no node selection, and what is outlined is what a drag moves. They
        // carry no handles — scaling a layer selection is not an operation.
        let layer_bbox_rects: Vec<CompRect> = (|| {
            let selection = super::layer_selection(cx);
            if selection.layers().len() < 2 {
                return None;
            }
            let comp_res = composition_resolution?;
            let pos = cx.try_global::<super::PlaybackPosition>().copied()?;
            let project = cx.try_global::<ProjectStateHandle>()?.0.upgrade()?;
            Some(layer_selection_comp_rects(
                project.read(cx).document(),
                selection.comp()?,
                selection.layers(),
                pos.frame,
                pos.fps,
                comp_res,
            ))
        })()
        .unwrap_or_default();
        let path_overlay = (|| {
            let tool = cx.try_global::<ToolState>()?.active;
            if !matches!(tool, ravel_ui::ToolKind::Select | ravel_ui::ToolKind::Pen) {
                return None;
            }
            let selection = cx.try_global::<CanvasSelection>()?;
            let resolution = composition_resolution?;
            let position = cx.try_global::<super::PlaybackPosition>().copied()?;
            let project = cx.try_global::<ProjectStateHandle>()?.0.upgrade()?;
            selected_path_overlay(
                selection,
                project.read(cx).document(),
                position.frame,
                position.fps,
                resolution,
            )
        })();
        // A bright semantic info color keeps the editable path legible over
        // both dark footage and the black composition background.
        let path_color = cx.theme().colors.info;

        let content = div().relative().size_full().overflow_hidden().child(
            canvas(
                move |bounds: Bounds<Pixels>, _window, _cx| {
                    viewport_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                    viewport_size.set((bounds.size.width.into(), bounds.size.height.into()));
                },
                move |bounds: Bounds<Pixels>, _, window, _cx| {
                    let Some(resolution) = composition_resolution else {
                        return;
                    };
                    let panel_size = (bounds.size.width.into(), bounds.size.height.into());
                    let rect = viewport.rect(panel_size, resolution);
                    let frame_bounds = Bounds {
                        origin: point(bounds.origin.x + px(rect.x), bounds.origin.y + px(rect.y)),
                        size: size(px(rect.width), px(rect.height)),
                    };
                    window.paint_quad(fill(frame_bounds, rgb(0x000000)));
                    if let Some(image) = image.clone()
                        && let Err(err) =
                            window.paint_image(frame_bounds, Corners::default(), image, 0, false)
                    {
                        tracing::error!(%err, "failed to paint viewer image");
                    }
                    if show_grid {
                        paint_proportional_grid(window, frame_bounds);
                    }
                    if show_safe_areas {
                        paint_safe_areas(window, frame_bounds);
                    }
                    paint_selection_bbox(window, frame_bounds, resolution, &bbox_rects, true);
                    paint_selection_bbox(
                        window,
                        frame_bounds,
                        resolution,
                        &layer_bbox_rects,
                        false,
                    );
                    if let Some(overlay) = &path_overlay {
                        paint_path_overlay(window, frame_bounds, resolution, overlay, path_color);
                    }
                },
            )
            .size_full(),
        );

        let content = if let Some(message) = &self.error {
            let label = t!("viewer.eval_error");
            content.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().colors.danger)
                            .child(SharedString::from(format!("{label}: {message}"))),
                    ),
            )
        } else if self.composition_resolution.is_none() {
            content.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(t!("viewer.no_output"))),
            )
        } else {
            content
        };

        // The interaction surface is the canvas area only, so toolbar
        // clicks and wheel events never zoom or pan the composition.
        let content = div()
            .id("viewer-canvas-area")
            .flex_1()
            .min_h_0()
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.pen_point_ended(cx);
                    let Some(resolution) = this.composition_resolution else {
                        return;
                    };
                    let pointer_start = this.local_position(event.position);
                    let offset_start = this
                        .viewport
                        .begin_pan(this.viewport_size.get(), resolution);
                    this.pan_drag = Some(PanDrag {
                        pointer_start,
                        offset_start,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if !this.path_handle_mouse_down(event, cx) {
                        this.select_mouse_down(event, cx);
                        this.shape_mouse_down(event, cx);
                        this.pen_mouse_down(event, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    if this.pan_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.move_ended(cx);
                    this.shape_ended(cx);
                    this.pen_point_ended(cx);
                    this.path_edit_ended(cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                match event.pressed_button {
                    Some(MouseButton::Middle) => {
                        this.cancel_move(cx);
                        this.cancel_shape(cx);
                        this.cancel_path_edit(cx);
                        let Some(drag) = this.pan_drag else {
                            return;
                        };
                        let pointer = this.local_position(event.position);
                        this.viewport.set_offset((
                            drag.offset_start.0 + pointer.0 - drag.pointer_start.0,
                            drag.offset_start.1 + pointer.1 - drag.pointer_start.1,
                        ));
                        cx.notify();
                    }
                    Some(MouseButton::Left) => {
                        this.pan_drag = None;
                        if this.path_edit_drag.is_some() {
                            this.path_edit_dragged(event.position, cx);
                        } else if this
                            .pen_session
                            .as_ref()
                            .is_some_and(|s| s.active_point.is_some())
                        {
                            this.pen_dragged(event.position, cx);
                        } else if this.shape_drag.is_some() {
                            this.shape_dragged(event, cx);
                        } else {
                            this.move_dragged(event.position, cx);
                        }
                    }
                    _ => {
                        this.pan_drag = None;
                        this.pen_point_ended(cx);
                        this.cancel_move(cx);
                        this.cancel_shape(cx);
                        this.cancel_path_edit(cx);
                    }
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let Some(resolution) = this.composition_resolution else {
                    return;
                };
                let delta = event.delta.pixel_delta(px(20.0));
                let dy: f32 = delta.y.into();
                if dy == 0.0 {
                    return;
                }
                let current = this.viewport.zoom(this.viewport_size.get(), resolution);
                let requested = current * (-dy * 0.002).exp();
                this.viewport.zoom_toward(
                    requested,
                    this.local_position(event.position),
                    this.viewport_size.get(),
                    resolution,
                );
                cx.notify();
            }))
            .child(content);

        div()
            .id("viewer-panel")
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .border_t_1()
            .border_color(border_color)
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key.as_str() == "escape" && this.move_drag.is_some() {
                    this.cancel_move(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "escape" && this.shape_drag.is_some() {
                    this.cancel_shape(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "escape" && this.path_edit_drag.is_some()
                {
                    this.cancel_path_edit(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "escape" && this.pen_session.is_some() {
                    this.finalize_pen_session(false, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "h" && !event.is_held {
                    let mut state = cx.try_global::<ToolState>().cloned().unwrap_or_default();
                    if !state.hand_hold {
                        state.previous = state.active;
                        state.active = ravel_ui::ToolKind::Hand;
                        state.hand_hold = true;
                        cx.set_global(state);
                        cx.notify();
                    }
                }
            }))
            .on_key_up(cx.listener(|_this, event: &KeyUpEvent, _window, cx| {
                if event.keystroke.key.as_str() == "h" {
                    let mut state = cx.try_global::<ToolState>().cloned().unwrap_or_default();
                    if state.hand_hold {
                        state.active = state.previous;
                        state.hand_hold = false;
                        cx.set_global(state);
                        cx.notify();
                    }
                }
            }))
            .child(self.tool_toolbar(cx))
            .child(content)
            .child(self.toolbar(cx))
    }
}

/// Convert a straight-alpha RGBA f32 [`FrameBuffer`] into the straight-alpha
/// BGRA u8 [`RenderImage`] GPUI's `img` element consumes (the same layout the
/// built-in decoders produce). Returns `None` for degenerate dimensions.
fn frame_buffer_to_render_image(fb: &FrameBuffer) -> Option<Arc<RenderImage>> {
    let span = tracing::debug_span!(
        "frame_to_render_image",
        width = fb.width,
        height = fb.height
    );
    let _guard = span.enter();
    if fb.width == 0 || fb.height == 0 {
        return None;
    }
    let expected = fb.width as usize * fb.height as usize * 4;
    if fb.data.len() != expected {
        return None;
    }

    let mut bytes = Vec::with_capacity(expected);
    for pixel in fb.data.chunks_exact(4) {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        // BGRA order.
        bytes.push(to_u8(pixel[2]));
        bytes.push(to_u8(pixel[1]));
        bytes.push(to_u8(pixel[0]));
        bytes.push(to_u8(pixel[3]));
    }

    let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(fb.width, fb.height, bytes)?;
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        ImageFrame::new(buffer),
        1,
    ))))
}

// ---------------------------------------------------------------------------
// Selection bbox overlay (REQ-UI-011 unit 3)
// ---------------------------------------------------------------------------

use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Graph, Node, ParameterValue, PathPoint};
use ravel_core::types::{FrameRate, Vec2};

fn sample_float_param(node: &Node, key: &str, frame: u64, ctx: &EvalContext) -> Option<f32> {
    let param = node.parameters.iter().find(|p| p.key == key)?;
    match &param.value {
        ParameterValue::Float(v) => Some(*v),
        ParameterValue::Channel(ch) => Some(ch.evaluate(frame as f64, ctx)),
        _ => None,
    }
}

fn document_has_node(
    network: &NetworkPath,
    node: NodeId,
    project: Option<Entity<ProjectState>>,
    cx: &App,
) -> bool {
    project.is_some_and(|project| {
        ravel_ui::document::resolve_network(project.read(cx).document(), network)
            .is_some_and(|graph| graph.node(node).is_some())
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CompRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct PathOverlay {
    points: Vec<PathPoint>,
    closed: bool,
}

fn screen_to_comp(
    local: (f32, f32),
    rect: viewport::Rect,
    comp_resolution: (u32, u32),
) -> Option<(f32, f32)> {
    if rect.width <= 0.0 || comp_resolution.0 == 0 {
        return None;
    }
    let zoom = rect.width / comp_resolution.0 as f32;
    Some(((local.0 - rect.x) / zoom, (local.1 - rect.y) / zoom))
}

#[cfg(test)]
fn comp_to_screen(comp: (f32, f32), rect: viewport::Rect, comp_width: u32) -> (f32, f32) {
    let zoom = rect.width / comp_width as f32;
    (rect.x + comp.0 * zoom, rect.y + comp.1 * zoom)
}

fn rect_contains(rect: &CompRect, point: (f32, f32)) -> bool {
    point.0 >= rect.x
        && point.0 <= rect.x + rect.w
        && point.1 >= rect.y
        && point.1 <= rect.y + rect.h
}

fn hit_test_shape_nodes(
    graph: &Graph,
    point: (f32, f32),
    frame: u64,
    ctx: &EvalContext,
    shell: &Affine,
) -> Option<NodeId> {
    let mut candidates: Vec<_> = graph.nodes().collect();
    candidates.sort_by_key(|node| std::cmp::Reverse(node.metadata.z));
    candidates.into_iter().find_map(|node| {
        let bounds = shape_node_bounds(node, frame, ctx)?;
        let bounds = if shell.is_identity() {
            bounds
        } else {
            transform_rect(&bounds, shell)
        };
        rect_contains(&bounds, point).then_some(node.id)
    })
}

fn selection_after_click(
    current: &HashSet<NodeId>,
    hit: Option<NodeId>,
    shift: bool,
) -> HashSet<NodeId> {
    let Some(hit) = hit else {
        return HashSet::new();
    };
    if shift {
        let mut updated = current.clone();
        if !updated.insert(hit) {
            updated.remove(&hit);
        }
        updated
    } else if current.contains(&hit) {
        current.clone()
    } else {
        HashSet::from([hit])
    }
}

fn moved_shape_node(
    node: &Node,
    origin: (f32, f32),
    original_path: Option<&[PathPoint]>,
    delta: (f32, f32),
    local_frame: u64,
) -> Option<Node> {
    let mut updated = node.clone();
    if let Some(parameter) = updated
        .parameters
        .iter_mut()
        .find(|param| param.key == "points")
        && let ParameterValue::PathPoints(points) = &mut parameter.value
    {
        let original = original_path?;
        if points.len() != original.len() {
            return None;
        }
        points.clone_from_slice(original);
        for point in points {
            offset_vec2(&mut point.p, delta);
        }
        return Some(updated);
    }
    for (key, value) in [
        ("center_x", origin.0 + delta.0),
        ("center_y", origin.1 + delta.1),
    ] {
        let parameter = updated
            .parameters
            .iter_mut()
            .find(|param| param.key == key)?;
        parameter.value = edited_float_param(&parameter.value, value, Some(local_frame));
    }
    Some(updated)
}

fn offset_vec2(value: &mut Vec2, delta: (f32, f32)) {
    value.0 += delta.0;
    value.1 += delta.1;
}

fn path_points(node: &Node) -> Option<&[PathPoint]> {
    node.parameters
        .iter()
        .find(|param| param.key == "points")
        .and_then(|param| match &param.value {
            ParameterValue::PathPoints(points) => Some(points.as_slice()),
            _ => None,
        })
}

fn path_closed(node: &Node) -> bool {
    node.parameters
        .iter()
        .find(|param| param.key == "closed")
        .is_some_and(|param| matches!(&param.value, ParameterValue::Bool(true)))
}

fn corner_path_point(position: (f32, f32)) -> PathPoint {
    PathPoint {
        p: Vec2(position.0, position.1),
        in_tan: Vec2(0.0, 0.0),
        out_tan: Vec2(0.0, 0.0),
    }
}

fn smooth_path_point(anchor: (f32, f32), handle: (f32, f32)) -> PathPoint {
    let tangent = (handle.0 - anchor.0, handle.1 - anchor.1);
    PathPoint {
        p: Vec2(anchor.0, anchor.1),
        in_tan: Vec2(-tangent.0, -tangent.1),
        out_tan: Vec2(tangent.0, tangent.1),
    }
}

fn distance_squared(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

fn pen_should_close(points: &[PathPoint], pointer: (f32, f32), radius: f32) -> bool {
    points.len() >= 2
        && points.first().is_some_and(|point| {
            distance_squared((point.p.0, point.p.1), pointer) <= radius * radius
        })
}

fn path_handle_hit(
    points: &[PathPoint],
    pointer: (f32, f32),
    radius: f32,
) -> Option<(usize, PathHandleKind)> {
    let radius_squared = radius * radius;
    for (index, point) in points.iter().enumerate() {
        for (handle, tangent) in [
            (PathHandleKind::InTangent, point.in_tan),
            (PathHandleKind::OutTangent, point.out_tan),
        ] {
            if tangent == Vec2(0.0, 0.0) {
                continue;
            }
            let position = (point.p.0 + tangent.0, point.p.1 + tangent.1);
            if distance_squared(position, pointer) <= radius_squared {
                return Some((index, handle));
            }
        }
        if distance_squared((point.p.0, point.p.1), pointer) <= radius_squared {
            return Some((index, PathHandleKind::Point));
        }
    }
    None
}

fn edited_path_points(
    original: &[PathPoint],
    index: usize,
    handle: PathHandleKind,
    delta: (f32, f32),
) -> Vec<PathPoint> {
    let mut points = original.to_vec();
    let Some(point) = points.get_mut(index) else {
        return points;
    };
    match handle {
        PathHandleKind::Point => {
            offset_vec2(&mut point.p, delta);
        }
        PathHandleKind::InTangent => offset_vec2(&mut point.in_tan, delta),
        PathHandleKind::OutTangent => offset_vec2(&mut point.out_tan, delta),
    }
    points
}

fn custom_path_node(mut node: Node, points: Vec<PathPoint>, closed: bool) -> Node {
    for parameter in &mut node.parameters {
        match parameter.key.as_str() {
            "points" => parameter.value = ParameterValue::PathPoints(points.clone()),
            "closed" => parameter.value = ParameterValue::Bool(closed),
            _ => {}
        }
    }
    node
}

// ---------------------------------------------------------------------------
// Shape drawing tools (REQ-UI-011 unit 5)
// ---------------------------------------------------------------------------

/// Map a comp-space drag to shape extents. Plain drag stretches corner to
/// corner, Shift constrains to a square/circle, Alt draws from the center
/// outward (the drag start becomes the center).
fn drag_geometry(start: (f32, f32), current: (f32, f32), shift: bool, alt: bool) -> DragGeometry {
    let dx = current.0 - start.0;
    let dy = current.1 - start.1;
    if alt {
        let half = if shift {
            let m = dx.abs().max(dy.abs());
            (m, m)
        } else {
            (dx.abs(), dy.abs())
        };
        DragGeometry {
            center: start,
            half,
        }
    } else {
        let end = if shift {
            let m = dx.abs().max(dy.abs());
            // A zero-delta axis still needs a stable nonzero direction, or an
            // axis-aligned Shift drag would collapse that axis to zero.
            let (sx, sy) = (
                if dx < 0.0 { -1.0 } else { 1.0 },
                if dy < 0.0 { -1.0 } else { 1.0 },
            );
            (start.0 + m * sx, start.1 + m * sy)
        } else {
            current
        };
        DragGeometry {
            center: ((start.0 + end.0) * 0.5, (start.1 + end.1) * 0.5),
            half: (
                ((end.0 - start.0) * 0.5).abs(),
                ((end.1 - start.1) * 0.5).abs(),
            ),
        }
    }
}

/// A drag with a zero extent on either axis creates nothing: the resulting
/// shape would be invisible.
fn drag_geometry_degenerate(geo: DragGeometry) -> bool {
    geo.half.0 == 0.0 || geo.half.1 == 0.0
}

/// Overwrite a freshly created shape node's parameters with the drag
/// geometry (rect takes full extents, ellipse takes radii). Values are plain
/// Floats: the node comes straight from the registry, so there are no
/// channels to preserve.
fn drawn_shape_node(mut node: Node, kind: ShapeDrawKind, geo: DragGeometry) -> Node {
    let values: [(&str, f32); 4] = match kind {
        ShapeDrawKind::Rect => [
            ("center_x", geo.center.0),
            ("center_y", geo.center.1),
            ("width", geo.half.0 * 2.0),
            ("height", geo.half.1 * 2.0),
        ],
        ShapeDrawKind::Ellipse => [
            ("center_x", geo.center.0),
            ("center_y", geo.center.1),
            ("radius_x", geo.half.0),
            ("radius_y", geo.half.1),
        ],
    };
    for (key, value) in values {
        if let Some(param) = node.parameters.iter_mut().find(|p| p.key == key) {
            param.value = ParameterValue::Float(value);
        }
    }
    node
}

/// Wiring target for a freshly drawn shape: the `geometry` input of a
/// rasterize node with no incoming edge. `Graph::nodes` iterates a hash map,
/// so candidates are ordered by node id for a deterministic pick. When every
/// rasterize input is occupied the shape is left unwired (REQ-UI-011: no
/// implicit merge insertion, no edge replacement).
fn free_rasterize_geometry_input(graph: &Graph) -> Option<(NodeId, InputPortIndex)> {
    let mut candidates: Vec<_> = graph
        .nodes()
        .filter(|node| node.type_key == "rasterize")
        .collect();
    candidates.sort_by_key(|node| node.id.raw());
    candidates.into_iter().find_map(|node| {
        let index = node
            .inputs
            .iter()
            .position(|port| port.name == "geometry")?;
        let port = InputPortIndex(index as u32);
        let occupied = graph
            .edges()
            .any(|edge| edge.target == node.id && edge.target_port == port);
        (!occupied).then_some((node.id, port))
    })
}

/// Add a drawn shape node to the network at `path` and auto-wire its
/// geometry output to a free rasterize geometry input, if one exists. The
/// node lands at the conventional offset from its rasterize (matching the
/// Shape layer template layout) and on top of the z stack.
fn create_drawn_shape(
    doc: &Document,
    path: &NetworkPath,
    registry: &ravel_core::registry::NodeRegistry,
    kind: ShapeDrawKind,
    geo: DragGeometry,
) -> Option<(Document, NodeId)> {
    let graph = ravel_ui::document::resolve_network(doc, path)?.clone();
    let mut node = registry.create_node(kind.type_key(), NodeId::next())?;
    let target = free_rasterize_geometry_input(&graph);
    node.metadata.position = target
        .and_then(|(id, _)| graph.node(id))
        .map(|rasterize| {
            (
                rasterize.metadata.position.0 - 240.0,
                rasterize.metadata.position.1 + 180.0,
            )
        })
        .unwrap_or((0.0, 0.0));
    node.metadata.z = graph
        .nodes()
        .filter(|n| !n.metadata.synthetic)
        .map(|n| n.metadata.z)
        .max()
        .map_or(0, |z| z + 1);
    let source_port = node
        .outputs
        .iter()
        .position(|port| port.name == "output")
        .map(|index| OutputPortIndex(index as u32))?;
    let node = drawn_shape_node(node, kind, geo);
    let node_id = node.id;
    let mut graph = graph.add_node(node).ok()?;
    if let Some((target, target_port)) = target {
        graph = super::node_editor::connect_edge_and_update_variadic_inputs(
            graph,
            EdgeId::next(),
            node_id,
            source_port,
            target,
            target_port,
        )?;
    }
    let doc = ravel_ui::document::replace_network(doc, path, graph)?;
    Some((doc, node_id))
}

/// Auto-create a Shape template layer for a drawing gesture and make the
/// drawn shape its wired content: the template's placeholder generator is
/// repurposed when it matches the drawn type, otherwise removed (its edges
/// with it) so the drawn node takes the freed rasterize geometry input.
/// Drawing into the freshly stamped layer therefore displays immediately,
/// and the whole creation still unwinds as one undo step.
fn create_layer_with_drawn_shape(
    doc: &Document,
    comp: ravel_core::id::CompId,
    registry: &ravel_core::registry::NodeRegistry,
    kind: ShapeDrawKind,
    geo: DragGeometry,
) -> Option<(Document, NetworkPath, NodeId)> {
    let template = ravel_core::composition::templates::builtin_layer_template("shape")?;
    let (doc, layer) =
        match ravel_ui::document::add_layer_from_template(doc, comp, template, registry) {
            Ok(Some(pair)) => pair,
            Ok(None) => return None,
            Err(err) => {
                tracing::error!(%err, "shape template instantiation failed");
                return None;
            }
        };
    let path = NetworkPath::layer(comp, layer);
    let graph = ravel_ui::document::resolve_network(&doc, &path)?.clone();
    let placeholder = graph
        .nodes()
        .find(|node| node.type_key.starts_with("shape."))?
        .id;
    if graph.node(placeholder)?.type_key == kind.type_key() {
        // Same generator type: the placeholder becomes the drawn shape.
        let node = graph.node(placeholder)?;
        let updated = drawn_shape_node(node.as_ref().clone(), kind, geo);
        let graph = graph.replace_node(Arc::new(updated));
        let doc = ravel_ui::document::replace_network(&doc, &path, graph)?;
        Some((doc, path, placeholder))
    } else {
        // Different generator type: dropping the placeholder frees the
        // rasterize geometry input for the drawn node.
        let graph = graph.remove_node(placeholder).ok()?;
        let doc = ravel_ui::document::replace_network(&doc, &path, graph)?;
        let (doc, node) = create_drawn_shape(&doc, &path, registry, kind, geo)?;
        Some((doc, path, node))
    }
}

fn create_custom_path(
    doc: &Document,
    path: &NetworkPath,
    registry: &ravel_core::registry::NodeRegistry,
    points: Vec<PathPoint>,
) -> Option<(Document, NodeId)> {
    let graph = ravel_ui::document::resolve_network(doc, path)?.clone();
    let target = free_rasterize_geometry_input(&graph);
    let mut node = registry.create_node("shape.custom_path", NodeId::next())?;
    node.metadata.position = target
        .and_then(|(id, _)| graph.node(id))
        .map(|rasterize| {
            (
                rasterize.metadata.position.0 - 240.0,
                rasterize.metadata.position.1 + 180.0,
            )
        })
        .unwrap_or((0.0, 0.0));
    node.metadata.z = graph
        .nodes()
        .filter(|candidate| !candidate.metadata.synthetic)
        .map(|candidate| candidate.metadata.z)
        .max()
        .map_or(0, |z| z + 1);
    let source_port = node
        .outputs
        .iter()
        .position(|port| port.name == "output")
        .map(|index| OutputPortIndex(index as u32))?;
    let node = custom_path_node(node, points, false);
    let node_id = node.id;
    let mut graph = graph.add_node(node).ok()?;
    if let Some((target, target_port)) = target {
        graph = super::node_editor::connect_edge_and_update_variadic_inputs(
            graph,
            EdgeId::next(),
            node_id,
            source_port,
            target,
            target_port,
        )?;
    }
    let doc = ravel_ui::document::replace_network(doc, path, graph)?;
    Some((doc, node_id))
}

fn create_layer_with_custom_path(
    doc: &Document,
    comp: ravel_core::id::CompId,
    registry: &ravel_core::registry::NodeRegistry,
    points: Vec<PathPoint>,
) -> Option<(Document, NetworkPath, NodeId)> {
    let template = ravel_core::composition::templates::builtin_layer_template("shape")?;
    let (doc, layer) =
        match ravel_ui::document::add_layer_from_template(doc, comp, template, registry) {
            Ok(Some(pair)) => pair,
            Ok(None) => return None,
            Err(err) => {
                tracing::error!(%err, "shape template instantiation failed");
                return None;
            }
        };
    let path = NetworkPath::layer(comp, layer);
    let graph = ravel_ui::document::resolve_network(&doc, &path)?.clone();
    let placeholder = graph
        .nodes()
        .find(|node| node.type_key.starts_with("shape."))?
        .id;
    let graph = graph.remove_node(placeholder).ok()?;
    let doc = ravel_ui::document::replace_network(&doc, &path, graph)?;
    let (doc, node) = create_custom_path(&doc, &path, registry, points)?;
    Some((doc, path, node))
}

/// Parameter-derived AABB of a shape node (half extents around the center).
/// Polygon and star use the (outer) radius as a square bound — a conservative
/// AABB that never under-covers the actual vertices.
fn shape_node_bounds(node: &Node, frame: u64, ctx: &EvalContext) -> Option<CompRect> {
    if node.type_key == "shape.custom_path" {
        let points = path_points(node)?;
        let first = points.first()?;
        let (mut min_x, mut min_y) = (first.p.0, first.p.1);
        let (mut max_x, mut max_y) = (first.p.0, first.p.1);
        for point in &points[1..] {
            min_x = min_x.min(point.p.0);
            min_y = min_y.min(point.p.1);
            max_x = max_x.max(point.p.0);
            max_y = max_y.max(point.p.1);
        }
        return Some(CompRect {
            x: min_x,
            y: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        });
    }
    let half = match node.type_key.as_str() {
        "shape.rect" => (
            sample_float_param(node, "width", frame, ctx)? * 0.5,
            sample_float_param(node, "height", frame, ctx)? * 0.5,
        ),
        "shape.ellipse" => (
            sample_float_param(node, "radius_x", frame, ctx)?,
            sample_float_param(node, "radius_y", frame, ctx)?,
        ),
        "shape.polygon" => {
            let r = sample_float_param(node, "radius", frame, ctx)?;
            (r, r)
        }
        "shape.star" => {
            let r = sample_float_param(node, "outer_radius", frame, ctx)?;
            (r, r)
        }
        _ => return None,
    };
    let cx = sample_float_param(node, "center_x", frame, ctx)?;
    let cy = sample_float_param(node, "center_y", frame, ctx)?;
    Some(CompRect {
        x: cx - half.0,
        y: cy - half.1,
        w: half.0 * 2.0,
        h: half.1 * 2.0,
    })
}

fn transform_rect(r: &CompRect, m: &Affine) -> CompRect {
    let corners = [
        (r.x, r.y),
        (r.x + r.w, r.y),
        (r.x, r.y + r.h),
        (r.x + r.w, r.y + r.h),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        let (tx, ty) = m.apply(x, y);
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }
    CompRect {
        x: min_x,
        y: min_y,
        w: max_x - min_x,
        h: max_y - min_y,
    }
}

fn union_rect(a: CompRect, b: CompRect) -> CompRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    CompRect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

/// The shape nodes of a layer's own network that carry comp-space geometry —
/// what a layer-level bbox outlines and what a layer-level move drags. Nested
/// subnets are not descended into: their nodes' parameters are not addressable
/// from the layer network, so a drag could not write them.
fn layer_shape_nodes<'a>(layer: &'a Layer, frame: u64, ctx: &EvalContext) -> Vec<&'a Node> {
    layer
        .network
        .nodes()
        .filter(|node| shape_node_bounds(node, frame, ctx).is_some())
        .map(std::sync::Arc::as_ref)
        .collect()
}

/// Comp-space bounds of a whole layer: the union of its shape nodes' bounds put
/// through the layer's compositing chain transform (REQ-UI-013 multi-selection).
///
/// `None` when the layer draws nothing with known bounds — a media or
/// effects-only network has no geometry to measure, so it gets no bbox rather
/// than a guessed one.
fn layer_comp_rect(
    comp: &Composition,
    layer: &Layer,
    frame: u64,
    ctx: &EvalContext,
) -> Option<CompRect> {
    // Network parameters live in layer-local time (REQ-LAYER-006).
    let local_frame = ravel_ui::keyframes::layer_local_frame(layer, frame);
    let bounds = layer_shape_nodes(layer, local_frame, ctx)
        .into_iter()
        .filter_map(|node| shape_node_bounds(node, local_frame, ctx))
        .reduce(union_rect)?;
    let shell = world_matrix(comp, layer, ctx);
    Some(if shell.is_identity() {
        bounds
    } else {
        transform_rect(&bounds, &shell)
    })
}

/// One bbox per selected layer that has measurable geometry, in selection order.
fn layer_selection_comp_rects(
    document: &Document,
    comp_id: CompId,
    layers: &[LayerId],
    frame: u64,
    fps: FrameRate,
    comp_resolution: (u32, u32),
) -> Vec<CompRect> {
    let Some(comp) = document.get_composition(comp_id) else {
        return Vec::new();
    };
    let ctx = EvalContext::new(frame, fps, comp_resolution);
    layers
        .iter()
        .filter_map(|id| layer_comp_rect(comp, comp.get_layer(*id)?, frame, &ctx))
        .collect()
}

fn selection_comp_rects(
    selection: &CanvasSelection,
    document: &Document,
    frame: u64,
    fps: FrameRate,
    comp_resolution: (u32, u32),
) -> Vec<CompRect> {
    let Some(path) = &selection.path else {
        return Vec::new();
    };
    if selection.nodes.is_empty() {
        return Vec::new();
    }
    let Some(comp) = document.get_composition(path.comp) else {
        return Vec::new();
    };
    let Some(layer) = comp.get_layer(path.layer) else {
        return Vec::new();
    };
    let Some(graph) = ravel_ui::document::resolve_network(document, path) else {
        return Vec::new();
    };
    let ctx = EvalContext::new(frame, fps, comp_resolution);
    let shell = world_matrix(comp, layer, &ctx);
    let is_identity = shell.is_identity();
    // Network parameters live in layer-local time (REQ-LAYER-006).
    let local_frame = ravel_ui::keyframes::layer_local_frame(layer, frame);

    selection
        .nodes
        .iter()
        .filter_map(|id| {
            let node = graph.node(*id)?;
            let rect = shape_node_bounds(node, local_frame, &ctx)?;
            Some(if is_identity {
                rect
            } else {
                transform_rect(&rect, &shell)
            })
        })
        .collect()
}

fn selected_path_overlay(
    selection: &CanvasSelection,
    document: &Document,
    frame: u64,
    fps: FrameRate,
    comp_resolution: (u32, u32),
) -> Option<PathOverlay> {
    let path = selection.path.as_ref()?;
    let selected: Vec<_> = selection.nodes.iter().copied().collect();
    let [node_id] = selected.as_slice() else {
        return None;
    };
    let node_id = *node_id;
    let comp = document.get_composition(path.comp)?;
    let layer = comp.get_layer(path.layer)?;
    let graph = ravel_ui::document::resolve_network(document, path)?;
    let node = graph.node(node_id)?;
    let points = path_points(node)?;
    let ctx = EvalContext::new(frame, fps, comp_resolution);
    let shell = world_matrix(comp, layer, &ctx);
    Some(PathOverlay {
        points: points
            .iter()
            .map(|point| transform_path_point(*point, &shell))
            .collect(),
        closed: path_closed(node),
    })
}

fn transform_point(point: (f32, f32), transform: &Affine) -> (f32, f32) {
    transform.apply(point.0, point.1)
}

fn transform_path_point(point: PathPoint, transform: &Affine) -> PathPoint {
    let anchor = transform_point((point.p.0, point.p.1), transform);
    let incoming = transform_point(
        (point.p.0 + point.in_tan.0, point.p.1 + point.in_tan.1),
        transform,
    );
    let outgoing = transform_point(
        (point.p.0 + point.out_tan.0, point.p.1 + point.out_tan.1),
        transform,
    );
    PathPoint {
        p: Vec2(anchor.0, anchor.1),
        in_tan: Vec2(incoming.0 - anchor.0, incoming.1 - anchor.1),
        out_tan: Vec2(outgoing.0 - anchor.0, outgoing.1 - anchor.1),
    }
}

fn paint_path_overlay(
    window: &mut Window,
    frame_bounds: Bounds<Pixels>,
    comp_resolution: (u32, u32),
    overlay: &PathOverlay,
    color: Hsla,
) {
    let zoom_x = f32::from(frame_bounds.size.width) / comp_resolution.0 as f32;
    let zoom_y = f32::from(frame_bounds.size.height) / comp_resolution.1 as f32;
    let origin_x: f32 = frame_bounds.origin.x.into();
    let origin_y: f32 = frame_bounds.origin.y.into();
    let screen = |position: (f32, f32)| {
        (
            origin_x + position.0 * zoom_x,
            origin_y + position.1 * zoom_y,
        )
    };
    let anchors: Vec<_> = overlay.points.iter().map(|point| point.p).collect();
    let incoming: Vec<_> = overlay.points.iter().map(|point| point.in_tan).collect();
    let outgoing: Vec<_> = overlay.points.iter().map(|point| point.out_tan).collect();
    let polyline = ravel_nodes::flatten::flatten_path(
        &anchors,
        Some(&incoming),
        Some(&outgoing),
        overlay.closed,
    );
    let paint_curve = |window: &mut Window, width: f32, stroke: Hsla| {
        let Some(first) = polyline.first() else {
            return;
        };
        let first = screen((first.0, first.1));
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(point(px(first.0), px(first.1)));
        for vertex in &polyline[1..] {
            let vertex = screen((vertex.0, vertex.1));
            path.line_to(point(px(vertex.0), px(vertex.1)));
        }
        if overlay.closed && polyline.len() > 1 {
            path.line_to(point(px(first.0), px(first.1)));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, stroke);
        }
    };
    paint_curve(window, 3.0, color);

    for control in &overlay.points {
        let anchor = screen((control.p.0, control.p.1));
        for tangent in [control.in_tan, control.out_tan] {
            if tangent == Vec2(0.0, 0.0) {
                continue;
            }
            let handle = screen((control.p.0 + tangent.0, control.p.1 + tangent.1));
            let mut line = PathBuilder::stroke(px(1.0));
            line.move_to(point(px(anchor.0), px(anchor.1)));
            line.line_to(point(px(handle.0), px(handle.1)));
            if let Ok(path) = line.build() {
                window.paint_path(path, color);
            }
            paint_path_handle(window, handle, color, false);
        }
        paint_path_handle(window, anchor, color, true);
    }
}

fn paint_path_handle(window: &mut Window, center: (f32, f32), color: Hsla, anchor: bool) {
    let size_px = if anchor { 7.0 } else { 5.0 };
    let half = size_px * 0.5;
    let bounds = Bounds {
        origin: point(px(center.0 - half), px(center.1 - half)),
        size: size(px(size_px), px(size_px)),
    };
    window.paint_quad(fill(bounds, color));
}

/// Screen-pixel side length of a selection handle (zoom-independent).
const SELECTION_HANDLE_PX: f32 = 7.0;

/// The eight handle anchor points of a screen-space bbox: four corners and
/// the four edge midpoints.
fn selection_handle_centers(x: f32, y: f32, w: f32, h: f32) -> [(f32, f32); 8] {
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    [
        (x, y),
        (cx, y),
        (x + w, y),
        (x, cy),
        (x + w, cy),
        (x, y + h),
        (cx, y + h),
        (x + w, y + h),
    ]
}

/// One selection handle: an accent-bordered white square centered on the
/// anchor, drawn at a constant screen size so it stays legible at any zoom.
fn paint_selection_handle(window: &mut Window, center: (f32, f32), color: Hsla) {
    let half = SELECTION_HANDLE_PX * 0.5;
    let outer = Bounds {
        origin: point(px(center.0 - half), px(center.1 - half)),
        size: size(px(SELECTION_HANDLE_PX), px(SELECTION_HANDLE_PX)),
    };
    window.paint_quad(fill(outer, color));
    let inner = Bounds {
        origin: point(px(center.0 - half + 1.0), px(center.1 - half + 1.0)),
        size: size(px(SELECTION_HANDLE_PX - 2.0), px(SELECTION_HANDLE_PX - 2.0)),
    };
    window.paint_quad(fill(inner, hsla(0.0, 0.0, 1.0, 1.0)));
}

/// Outline every rect, with the eight transform handles when `handles` is set
/// (a node selection). A layer-level selection is drawn without them: there is
/// no layer-level scale gesture behind them.
fn paint_selection_bbox(
    window: &mut Window,
    frame_bounds: Bounds<Pixels>,
    comp_resolution: (u32, u32),
    rects: &[CompRect],
    handles: bool,
) {
    if rects.is_empty() {
        return;
    }
    let zoom_x = f32::from(frame_bounds.size.width) / comp_resolution.0 as f32;
    let zoom_y = f32::from(frame_bounds.size.height) / comp_resolution.1 as f32;
    let origin_x: f32 = frame_bounds.origin.x.into();
    let origin_y: f32 = frame_bounds.origin.y.into();
    let color = hsla(0.58, 0.7, 0.6, 0.9);

    for r in rects {
        let screen_x = origin_x + r.x * zoom_x;
        let screen_y = origin_y + r.y * zoom_y;
        let screen_w = r.w * zoom_x;
        let screen_h = r.h * zoom_y;
        let bounds = Bounds {
            origin: point(px(screen_x), px(screen_y)),
            size: size(px(screen_w), px(screen_h)),
        };
        paint_rect_outline_colored(window, bounds, color);
        if !handles {
            continue;
        }
        for center in selection_handle_centers(screen_x, screen_y, screen_w, screen_h) {
            paint_selection_handle(window, center, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one for these plain unit tests.
    use core::prelude::v1::test;

    fn fb(width: u32, height: u32, pixel: [f32; 4]) -> FrameBuffer {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            data.extend_from_slice(&pixel);
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    #[test]
    fn converts_rgba_f32_to_bgra_u8() {
        let frame = fb(2, 2, [1.0, 0.5, 0.0, 1.0]);
        let image = frame_buffer_to_render_image(&frame).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        // BGRA: blue=0, green=128, red=255, alpha=255.
        assert_eq!(&bytes[..4], &[0, 128, 255, 255]);
        assert_eq!(image.size(0).width.0, 2);
        assert_eq!(image.size(0).height.0, 2);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let frame = fb(1, 1, [2.0, -1.0, 0.25, 1.5]);
        let image = frame_buffer_to_render_image(&frame).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[64, 0, 255, 255]);
    }

    fn shape_node(type_key: &str, params: &[(&str, f32)]) -> Node {
        let mut node = Node::new(ravel_core::id::NodeId::next(), type_key);
        for (key, value) in params {
            node = node.with_param(*key, ParameterValue::Float(*value));
        }
        node
    }

    fn eval_ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    #[test]
    fn rect_bounds_use_full_width_and_height() {
        let node = shape_node(
            "shape.rect",
            &[
                ("center_x", 100.0),
                ("center_y", 50.0),
                ("width", 80.0),
                ("height", 40.0),
            ],
        );
        let r = shape_node_bounds(&node, 0, &eval_ctx()).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (60.0, 30.0, 80.0, 40.0));
    }

    #[test]
    fn ellipse_bounds_use_radii() {
        let node = shape_node(
            "shape.ellipse",
            &[
                ("center_x", 0.0),
                ("center_y", 0.0),
                ("radius_x", 30.0),
                ("radius_y", 20.0),
            ],
        );
        let r = shape_node_bounds(&node, 0, &eval_ctx()).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (-30.0, -20.0, 60.0, 40.0));
    }

    #[test]
    fn polygon_and_star_bounds_are_radius_squares() {
        let polygon = shape_node(
            "shape.polygon",
            &[("center_x", 10.0), ("center_y", 10.0), ("radius", 25.0)],
        );
        let r = shape_node_bounds(&polygon, 0, &eval_ctx()).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (-15.0, -15.0, 50.0, 50.0));

        let star = shape_node(
            "shape.star",
            &[
                ("center_x", 0.0),
                ("center_y", 0.0),
                ("outer_radius", 40.0),
                ("inner_radius", 15.0),
            ],
        );
        let r = shape_node_bounds(&star, 0, &eval_ctx()).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (-40.0, -40.0, 80.0, 80.0));
    }

    /// Guards against registry drift: every shape template registered by
    /// `register_builtins` must yield bounds from its actual default
    /// parameters (a renamed parameter would return `None` here).
    #[test]
    fn registry_shape_defaults_yield_bounds() {
        use ravel_core::registry::NodeRegistry;
        use ravel_core::registry::builtin::register_builtins;

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let expected = [
            ("shape.rect", 100.0, 100.0),
            ("shape.ellipse", 100.0, 100.0),
            ("shape.polygon", 100.0, 100.0),
            ("shape.star", 100.0, 100.0),
        ];
        for (type_key, w, h) in expected {
            let node = registry
                .create_node(type_key, ravel_core::id::NodeId::next())
                .unwrap_or_else(|| panic!("{type_key}: registered template"));
            let r = shape_node_bounds(&node, 0, &eval_ctx())
                .unwrap_or_else(|| panic!("{type_key}: bounds from default parameters"));
            assert_eq!((r.w, r.h), (w, h), "{type_key}: default extents");
        }
    }

    #[test]
    fn non_shape_nodes_have_no_bounds() {
        let node = shape_node("scatter.grid", &[("center_x", 0.0), ("center_y", 0.0)]);
        assert!(shape_node_bounds(&node, 0, &eval_ctx()).is_none());
    }

    #[test]
    fn animated_center_samples_the_frame() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let node = Node::new(ravel_core::id::NodeId::next(), "shape.rect")
            .with_param(
                "center_x",
                ParameterValue::Channel(AnimationChannel::keyframes(curve)),
            )
            .with_param("center_y", ParameterValue::Float(0.0))
            .with_param("width", ParameterValue::Float(10.0))
            .with_param("height", ParameterValue::Float(10.0));
        let r = shape_node_bounds(&node, 5, &eval_ctx()).unwrap();
        assert_eq!((r.x, r.w), (45.0, 10.0));
    }

    #[test]
    fn hit_test_uses_frontmost_shape_and_reports_misses() {
        let mut back = shape_node(
            "shape.rect",
            &[
                ("center_x", 50.0),
                ("center_y", 50.0),
                ("width", 40.0),
                ("height", 40.0),
            ],
        );
        back.metadata.z = 2;
        let back_id = back.id;
        let mut front = back.clone();
        front.id = NodeId::next();
        front.metadata.z = 8;
        let front_id = front.id;
        let graph = Graph::new()
            .add_node(back)
            .unwrap()
            .add_node(front)
            .unwrap();
        let identity = Affine::IDENTITY;

        assert_eq!(
            hit_test_shape_nodes(&graph, (50.0, 50.0), 0, &eval_ctx(), &identity),
            Some(front_id)
        );
        assert_eq!(
            hit_test_shape_nodes(&graph, (200.0, 200.0), 0, &eval_ctx(), &identity),
            None
        );
        assert_ne!(front_id, back_id);
    }

    #[test]
    fn hit_test_applies_shell_transform() {
        let node = shape_node(
            "shape.rect",
            &[
                ("center_x", 20.0),
                ("center_y", 20.0),
                ("width", 20.0),
                ("height", 20.0),
            ],
        );
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let translated = Affine([1.0, 0.0, 100.0, 0.0, 1.0, 50.0]);

        assert_eq!(
            hit_test_shape_nodes(&graph, (120.0, 70.0), 0, &eval_ctx(), &translated),
            Some(id)
        );
        assert_eq!(
            hit_test_shape_nodes(&graph, (20.0, 20.0), 0, &eval_ctx(), &translated),
            None
        );
    }

    #[test]
    fn click_selection_replaces_keeps_toggles_and_clears() {
        let first = NodeId::next();
        let second = NodeId::next();
        let selected = HashSet::from([first]);

        assert_eq!(
            selection_after_click(&selected, Some(first), false),
            selected
        );
        assert_eq!(
            selection_after_click(&selected, Some(second), false),
            HashSet::from([second])
        );
        assert_eq!(
            selection_after_click(&selected, Some(second), true),
            HashSet::from([first, second])
        );
        assert!(selection_after_click(&selected, Some(first), true).is_empty());
        assert!(selection_after_click(&selected, None, false).is_empty());
        assert!(selection_after_click(&selected, None, true).is_empty());
    }

    #[test]
    fn move_center_uses_origin_plus_delta() {
        let node = shape_node(
            "shape.rect",
            &[
                ("center_x", 10.0),
                ("center_y", 20.0),
                ("width", 40.0),
                ("height", 30.0),
            ],
        );
        let moved = moved_shape_node(&node, (10.0, 20.0), None, (4.5, -2.0), 7).unwrap();
        assert_eq!(
            sample_float_param(&moved, "center_x", 7, &eval_ctx()),
            Some(14.5)
        );
        assert_eq!(
            sample_float_param(&moved, "center_y", 7, &eval_ctx()),
            Some(18.0)
        );
    }

    #[test]
    fn zero_delta_restores_the_origin() {
        let node = shape_node(
            "shape.rect",
            &[
                ("center_x", 10.0),
                ("center_y", 20.0),
                ("width", 40.0),
                ("height", 30.0),
            ],
        );
        let moved = moved_shape_node(&node, (10.0, 20.0), None, (0.0, 0.0), 0).unwrap();
        assert_eq!(
            sample_float_param(&moved, "center_x", 0, &eval_ctx()),
            Some(10.0)
        );
        assert_eq!(
            sample_float_param(&moved, "center_y", 0, &eval_ctx()),
            Some(20.0)
        );
    }

    fn comp_with_layers(layers: Vec<Layer>) -> Composition {
        use ravel_core::id::CompId;
        let mut comp = Composition::new(
            CompId::next(),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        );
        for layer in layers {
            comp.layers.push_back(layer);
        }
        comp
    }

    /// The overlay geometry and the rendered pixels come from the same matrix
    /// (`ravel_core::composition::transform::world_matrix`), so a muted parent
    /// — which still transforms its children (REQ-LAYER-001) — moves the
    /// child's bbox exactly as far as it moves the child's image. Before this
    /// was shared, the viewer stopped the chain at the muted parent and drew
    /// the bbox at the wrong place.
    #[test]
    fn layer_bbox_follows_a_muted_parents_transform() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::id::LayerId;

        let mut parent = Layer::new(LayerId::next(), "parent", Graph::new());
        parent.transform.position = [
            AnimationChannel::constant(100.0),
            AnimationChannel::constant(50.0),
        ];
        let network = Graph::new()
            .add_node(shape_node(
                "shape.rect",
                &[
                    ("center_x", 0.0),
                    ("center_y", 0.0),
                    ("width", 20.0),
                    ("height", 20.0),
                ],
            ))
            .unwrap();
        let child = Layer::new(LayerId::next(), "child", network)
            .with_time(0, 0, 300)
            .with_parent(parent.id);

        for muted in [false, true] {
            parent.muted = muted;
            let comp = comp_with_layers(vec![parent.clone(), child.clone()]);
            let m = world_matrix(&comp, &child, &eval_ctx());
            assert_eq!(
                (m.0[2], m.0[5]),
                (100.0, 50.0),
                "the parent transform applies regardless of mute (muted = {muted})"
            );
            let rect = layer_comp_rect(&comp, &child, 0, &eval_ctx()).unwrap();
            assert_eq!(
                (rect.x, rect.y, rect.w, rect.h),
                (90.0, 40.0, 20.0, 20.0),
                "the bbox lands where the render lands (muted = {muted})"
            );
        }
    }

    #[test]
    fn parent_cycles_terminate() {
        use ravel_core::id::LayerId;

        let a_id = LayerId::next();
        let b_id = LayerId::next();
        let a = Layer::new(a_id, "a", Graph::new()).with_parent(b_id);
        let b = Layer::new(b_id, "b", Graph::new()).with_parent(a_id);
        let comp = comp_with_layers(vec![a.clone(), b]);
        let m = world_matrix(&comp, &a, &eval_ctx());
        assert!(m.is_identity());
    }

    /// A layer's bbox is the union of its shape nodes, put through the layer's
    /// shell transform (REQ-UI-013 multi-selection).
    #[test]
    fn layer_bbox_unions_shape_nodes_and_follows_the_shell() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::id::LayerId;

        let left = shape_node(
            "shape.rect",
            &[
                ("center_x", 0.0),
                ("center_y", 0.0),
                ("width", 100.0),
                ("height", 100.0),
            ],
        );
        let right = shape_node(
            "shape.ellipse",
            &[
                ("center_x", 200.0),
                ("center_y", 0.0),
                ("radius_x", 50.0),
                ("radius_y", 10.0),
            ],
        );
        let network = Graph::new()
            .add_node(left)
            .unwrap()
            .add_node(right)
            .unwrap();
        let mut layer = Layer::new(LayerId::next(), "shapes", network).with_time(0, 0, 300);
        let comp = comp_with_layers(vec![layer.clone()]);
        let rect = layer_comp_rect(&comp, &layer, 0, &eval_ctx()).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.w, rect.h),
            (-50.0, -50.0, 300.0, 100.0),
            "the union spans both shapes"
        );

        // A shell translation moves the whole bbox with the layer.
        layer.transform.position = [
            AnimationChannel::constant(10.0),
            AnimationChannel::constant(20.0),
        ];
        let comp = comp_with_layers(vec![layer.clone()]);
        let moved = layer_comp_rect(&comp, &layer, 0, &eval_ctx()).unwrap();
        assert_eq!((moved.x, moved.y), (-40.0, -30.0));
        assert_eq!((moved.w, moved.h), (rect.w, rect.h));

        // A layer that draws nothing measurable gets no bbox rather than a
        // guessed one.
        let empty = Layer::new(LayerId::next(), "null", Graph::new());
        let comp = comp_with_layers(vec![empty.clone()]);
        assert!(layer_comp_rect(&comp, &empty, 0, &eval_ctx()).is_none());
    }

    /// The selection's rects come out in selection order, skipping layers with
    /// nothing to measure.
    #[test]
    fn layer_selection_rects_skip_unmeasurable_layers() {
        use ravel_core::id::LayerId;

        let network = Graph::new()
            .add_node(shape_node(
                "shape.rect",
                &[
                    ("center_x", 10.0),
                    ("center_y", 10.0),
                    ("width", 20.0),
                    ("height", 20.0),
                ],
            ))
            .unwrap();
        let shapes = Layer::new(LayerId::next(), "shapes", network).with_time(0, 0, 300);
        let null = Layer::new(LayerId::next(), "null", Graph::new()).with_time(0, 0, 300);
        let comp = comp_with_layers(vec![shapes.clone(), null.clone()]);
        let comp_id = comp.id;
        let document = Document::default().with_composition(comp);

        let rects = layer_selection_comp_rects(
            &document,
            comp_id,
            &[null.id, shapes.id],
            0,
            FrameRate::new(30, 1),
            (1920, 1080),
        );
        assert_eq!(rects.len(), 1, "only the measurable layer draws a bbox");
        assert_eq!((rects[0].x, rects[0].y), (0.0, 0.0));
    }

    #[test]
    fn handle_centers_cover_corners_and_edge_midpoints() {
        let centers = selection_handle_centers(10.0, 20.0, 100.0, 50.0);
        let expected = [
            (10.0, 20.0),
            (60.0, 20.0),
            (110.0, 20.0),
            (10.0, 45.0),
            (110.0, 45.0),
            (10.0, 70.0),
            (60.0, 70.0),
            (110.0, 70.0),
        ];
        assert_eq!(centers, expected);
    }

    #[test]
    fn screen_comp_conversion_round_trips() {
        let viewport = ViewerViewport::default();
        let resolution = (1920, 1080);
        let rect = viewport.rect((1000.0, 800.0), resolution);
        let comp = (731.25, 412.5);
        let screen = comp_to_screen(comp, rect, resolution.0);
        let round_trip = screen_to_comp(screen, rect, resolution).unwrap();
        assert!((round_trip.0 - comp.0).abs() < 1e-4);
        assert!((round_trip.1 - comp.1).abs() < 1e-4);
    }

    #[test]
    fn rejects_degenerate_frames() {
        assert!(frame_buffer_to_render_image(&fb(0, 4, [0.0; 4])).is_none());
        let mismatched = FrameBuffer {
            width: 4,
            height: 4,
            data: Arc::from(vec![0.0f32; 8]),
        };
        assert!(frame_buffer_to_render_image(&mismatched).is_none());
    }

    // -----------------------------------------------------------------------
    // Shape drawing tools (REQ-UI-011 unit 5)
    // -----------------------------------------------------------------------

    fn registry() -> ravel_core::registry::NodeRegistry {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        registry
    }

    fn doc_with_network(network: Graph) -> (Document, NetworkPath) {
        use ravel_core::id::{CompId, LayerId};
        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let comp = Composition::new(comp_id, "Comp", (1920, 1080), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(layer_id, "Layer", network).with_time(0, 0, 300));
        (
            Document::default().with_composition(comp),
            NetworkPath::layer(comp_id, layer_id),
        )
    }

    #[test]
    fn drag_geometry_stretches_corner_to_corner() {
        let geo = drag_geometry((10.0, 20.0), (110.0, 70.0), false, false);
        assert_eq!(geo.center, (60.0, 45.0));
        assert_eq!(geo.half, (50.0, 25.0));
        // Reversed drag direction gives the same rect.
        let geo = drag_geometry((110.0, 70.0), (10.0, 20.0), false, false);
        assert_eq!(geo.center, (60.0, 45.0));
        assert_eq!(geo.half, (50.0, 25.0));
    }

    #[test]
    fn drag_geometry_shift_constrains_to_square() {
        // The longer axis wins, keeping the drag direction's signs.
        let geo = drag_geometry((0.0, 0.0), (100.0, 40.0), true, false);
        assert_eq!(geo.center, (50.0, 50.0));
        assert_eq!(geo.half, (50.0, 50.0));

        let geo = drag_geometry((100.0, 100.0), (40.0, 70.0), true, false);
        assert_eq!(geo.center, (70.0, 70.0));
        assert_eq!(geo.half, (30.0, 30.0));
    }

    #[test]
    fn drag_geometry_alt_draws_from_center() {
        let geo = drag_geometry((50.0, 50.0), (90.0, 70.0), false, true);
        assert_eq!(geo.center, (50.0, 50.0));
        assert_eq!(geo.half, (40.0, 20.0));
    }

    #[test]
    fn drag_geometry_shift_alt_draws_circle_from_center() {
        let geo = drag_geometry((50.0, 50.0), (90.0, 70.0), true, true);
        assert_eq!(geo.center, (50.0, 50.0));
        assert_eq!(geo.half, (40.0, 40.0));
    }

    #[test]
    fn drag_geometry_shift_axis_aligned_drag_stays_square() {
        // A perfectly horizontal/vertical Shift drag must not collapse the
        // zero-delta axis (stable direction instead of `0.0.signum()`).
        let geo = drag_geometry((10.0, 10.0), (50.0, 10.0), true, false);
        assert_eq!(geo.half, (20.0, 20.0));
        let geo = drag_geometry((10.0, 10.0), (10.0, 50.0), true, false);
        assert_eq!(geo.half, (20.0, 20.0));
    }

    #[test]
    fn zero_extent_on_either_axis_is_degenerate() {
        assert!(drag_geometry_degenerate(drag_geometry(
            (10.0, 10.0),
            (10.0, 50.0),
            false,
            false
        )));
        assert!(drag_geometry_degenerate(drag_geometry(
            (10.0, 10.0),
            (50.0, 10.0),
            false,
            false
        )));
        assert!(drag_geometry_degenerate(drag_geometry(
            (10.0, 10.0),
            (10.0, 10.0),
            false,
            false
        )));
        assert!(!drag_geometry_degenerate(drag_geometry(
            (10.0, 10.0),
            (11.0, 11.0),
            false,
            false
        )));
        // Shift keeps an axis-aligned drag non-degenerate.
        assert!(!drag_geometry_degenerate(drag_geometry(
            (10.0, 10.0),
            (50.0, 10.0),
            true,
            false
        )));
    }

    #[test]
    fn drawn_rect_maps_drag_to_size_params() {
        let node = registry()
            .create_node("shape.rect", NodeId::next())
            .unwrap();
        let node = drawn_shape_node(
            node,
            ShapeDrawKind::Rect,
            DragGeometry {
                center: (60.0, 45.0),
                half: (50.0, 25.0),
            },
        );
        let ctx = eval_ctx();
        assert_eq!(sample_float_param(&node, "center_x", 0, &ctx), Some(60.0));
        assert_eq!(sample_float_param(&node, "center_y", 0, &ctx), Some(45.0));
        assert_eq!(sample_float_param(&node, "width", 0, &ctx), Some(100.0));
        assert_eq!(sample_float_param(&node, "height", 0, &ctx), Some(50.0));
    }

    #[test]
    fn drawn_ellipse_maps_drag_to_radii() {
        let node = registry()
            .create_node("shape.ellipse", NodeId::next())
            .unwrap();
        let node = drawn_shape_node(
            node,
            ShapeDrawKind::Ellipse,
            DragGeometry {
                center: (10.0, 20.0),
                half: (30.0, 15.0),
            },
        );
        let ctx = eval_ctx();
        assert_eq!(sample_float_param(&node, "center_x", 0, &ctx), Some(10.0));
        assert_eq!(sample_float_param(&node, "center_y", 0, &ctx), Some(20.0));
        assert_eq!(sample_float_param(&node, "radius_x", 0, &ctx), Some(30.0));
        assert_eq!(sample_float_param(&node, "radius_y", 0, &ctx), Some(15.0));
    }

    #[test]
    fn wiring_target_prefers_free_rasterize_deterministically() {
        let registry = registry();
        let a = registry.create_node("rasterize", NodeId::next()).unwrap();
        let b = registry.create_node("rasterize", NodeId::next()).unwrap();
        let (first, second) = if a.id.raw() < b.id.raw() {
            (a.id, b.id)
        } else {
            (b.id, a.id)
        };
        let graph = Graph::new().add_node(a).unwrap().add_node(b).unwrap();

        // Both free: the lowest node id wins (hash-map iteration is unordered).
        assert_eq!(
            free_rasterize_geometry_input(&graph),
            Some((first, InputPortIndex(0)))
        );

        // Occupy the first rasterize: the second becomes the target.
        let source = registry.create_node("shape.rect", NodeId::next()).unwrap();
        let source_id = source.id;
        let graph = graph
            .add_node(source)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                first,
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(
            free_rasterize_geometry_input(&graph),
            Some((second, InputPortIndex(0)))
        );

        // Both occupied: no target (REQ-UI-011: unwired, no merge insertion).
        let graph = graph
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                second,
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(free_rasterize_geometry_input(&graph), None);
    }

    #[test]
    fn wiring_target_is_none_without_rasterize() {
        let graph = Graph::new()
            .add_node(
                registry()
                    .create_node("shape.rect", NodeId::next())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(free_rasterize_geometry_input(&graph), None);
    }

    #[test]
    fn created_shape_wires_into_free_rasterize_input() {
        let registry = registry();
        let rasterize = registry.create_node("rasterize", NodeId::next()).unwrap();
        let rasterize_id = rasterize.id;
        let network = Graph::new().add_node(rasterize).unwrap();
        let (doc, path) = doc_with_network(network);

        let geo = DragGeometry {
            center: (60.0, 45.0),
            half: (50.0, 25.0),
        };
        let (doc, node_id) =
            create_drawn_shape(&doc, &path, &registry, ShapeDrawKind::Rect, geo).unwrap();
        let graph = ravel_ui::document::resolve_network(&doc, &path).unwrap();

        let outgoing: Vec<_> = graph.edges().filter(|e| e.source == node_id).collect();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].target, rasterize_id);
        assert_eq!(outgoing[0].target_port, InputPortIndex(0));

        let node = graph.node(node_id).unwrap();
        let ctx = eval_ctx();
        assert_eq!(sample_float_param(node, "width", 0, &ctx), Some(100.0));
        assert_eq!(sample_float_param(node, "height", 0, &ctx), Some(50.0));
        // bbox/hit-test integration: the drawn node yields its dragged bounds.
        let bounds = shape_node_bounds(node, 0, &ctx).unwrap();
        assert_eq!(
            (bounds.x, bounds.y, bounds.w, bounds.h),
            (10.0, 20.0, 100.0, 50.0)
        );
    }

    /// In the auto-created Shape layer the drawn rect takes over the
    /// template's placeholder generator (same type), so it stays wired and
    /// displays immediately.
    #[test]
    fn drawn_rect_reuses_the_template_placeholder() {
        let registry = registry();
        let (doc, _path) = doc_with_network(Graph::new());
        let comp = doc.root_comp.unwrap();

        let geo = DragGeometry {
            center: (100.0, 100.0),
            half: (40.0, 20.0),
        };
        let (doc, path, node_id) =
            create_layer_with_drawn_shape(&doc, comp, &registry, ShapeDrawKind::Rect, geo).unwrap();
        let graph = ravel_ui::document::resolve_network(&doc, &path).unwrap();

        // No extra node: the four template nodes are all there is.
        assert_eq!(graph.nodes().count(), 4);
        let node = graph.node(node_id).unwrap();
        assert_eq!(node.type_key, "shape.rect");
        let ctx = eval_ctx();
        assert_eq!(sample_float_param(node, "width", 0, &ctx), Some(80.0));
        assert_eq!(sample_float_param(node, "height", 0, &ctx), Some(40.0));
        // The template wiring survived: the drawn rect feeds the rasterize.
        let rasterize_id = graph
            .nodes()
            .find(|n| n.type_key == "rasterize")
            .unwrap()
            .id;
        assert!(
            graph
                .edges()
                .any(|e| e.source == node_id && e.target == rasterize_id)
        );
        assert_eq!(doc.validate(), Ok(()));
    }

    /// The drawn ellipse cannot reuse the rect placeholder, so the
    /// placeholder is removed and the new node takes the freed rasterize
    /// geometry input.
    #[test]
    fn drawn_ellipse_replaces_the_template_placeholder() {
        let registry = registry();
        let (doc, _path) = doc_with_network(Graph::new());
        let comp = doc.root_comp.unwrap();

        let geo = DragGeometry {
            center: (100.0, 100.0),
            half: (40.0, 40.0),
        };
        let (doc, path, node_id) =
            create_layer_with_drawn_shape(&doc, comp, &registry, ShapeDrawKind::Ellipse, geo)
                .unwrap();
        let graph = ravel_ui::document::resolve_network(&doc, &path).unwrap();

        assert!(graph.nodes().all(|n| n.type_key != "shape.rect"));
        assert_eq!(graph.nodes().count(), 4, "rect removed, ellipse added");
        let rasterize_id = graph
            .nodes()
            .find(|n| n.type_key == "rasterize")
            .unwrap()
            .id;
        let outgoing: Vec<_> = graph.edges().filter(|e| e.source == node_id).collect();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].target, rasterize_id);
        assert_eq!(outgoing[0].target_port, InputPortIndex(0));
        let layer = doc
            .get_composition(comp)
            .unwrap()
            .get_layer(path.layer)
            .unwrap();
        assert!(layer.has_frame_output());
        assert_eq!(doc.validate(), Ok(()));
    }

    /// The whole gesture — auto-created template layer, node, and wiring —
    /// collapses into one Document undo step: intermediate states go through
    /// `apply`, only the final document is committed.
    #[test]
    fn shape_creation_is_one_undo_step() {
        use ravel_ui::document::DocumentStore;
        let registry = registry();
        let (doc, _path) = doc_with_network(Graph::new());
        let original_layers = ravel_ui::document::root_composition(&doc)
            .unwrap()
            .layer_count();
        let mut store = DocumentStore::new(doc);

        // Mid-gesture: auto-create the Shape template layer with the drawn
        // shape as its content (no history).
        let comp = store.document().root_comp.unwrap();
        let geo = DragGeometry {
            center: (100.0, 100.0),
            half: (40.0, 40.0),
        };
        let (doc, _path, _node) = create_layer_with_drawn_shape(
            store.document(),
            comp,
            &registry,
            ShapeDrawKind::Rect,
            geo,
        )
        .unwrap();
        store.apply(doc.clone());
        // Mouse-up: one commit for the whole creation.
        store.commit(doc);

        let layers = |store: &DocumentStore| {
            ravel_ui::document::root_composition(store.document())
                .unwrap()
                .layer_count()
        };
        assert_eq!(layers(&store), original_layers + 1);
        assert!(store.undo());
        assert_eq!(layers(&store), original_layers, "one undo removes it all");
        assert!(!store.can_undo(), "no intermediate steps remain");
    }

    // -----------------------------------------------------------------------
    // Pen tool (REQ-UI-011 unit 7)
    // -----------------------------------------------------------------------

    #[test]
    fn pen_points_cover_corner_and_smooth_symmetric_math() {
        let corner = corner_path_point((10.0, 20.0));
        assert_eq!(corner.p, Vec2(10.0, 20.0));
        assert_eq!(corner.in_tan, Vec2(0.0, 0.0));
        assert_eq!(corner.out_tan, Vec2(0.0, 0.0));

        let smooth = smooth_path_point((10.0, 20.0), (16.0, 12.0));
        assert_eq!(smooth.p, Vec2(10.0, 20.0));
        assert_eq!(smooth.out_tan, Vec2(6.0, -8.0));
        assert_eq!(smooth.in_tan, Vec2(-6.0, 8.0));
    }

    #[test]
    fn pen_close_hit_requires_two_points_and_first_point_proximity() {
        let points = vec![
            corner_path_point((10.0, 10.0)),
            corner_path_point((50.0, 50.0)),
        ];
        assert!(pen_should_close(&points, (13.0, 14.0), 5.0));
        assert!(!pen_should_close(&points, (16.0, 10.0), 5.0));
        assert!(!pen_should_close(&points[..1], (10.0, 10.0), 5.0));
    }

    #[test]
    fn custom_path_bounds_use_control_points_not_tangent_extremes() {
        let node = custom_path_node(
            registry()
                .create_node("shape.custom_path", NodeId::next())
                .unwrap(),
            vec![
                PathPoint {
                    p: Vec2(10.0, 20.0),
                    in_tan: Vec2(-1000.0, -1000.0),
                    out_tan: Vec2(1000.0, 1000.0),
                },
                corner_path_point((50.0, 80.0)),
            ],
            false,
        );
        assert_eq!(
            shape_node_bounds(&node, 0, &eval_ctx()),
            Some(CompRect {
                x: 10.0,
                y: 20.0,
                w: 40.0,
                h: 60.0,
            })
        );
    }

    #[test]
    fn moving_custom_path_preserves_tangent_offsets() {
        let node = custom_path_node(
            registry()
                .create_node("shape.custom_path", NodeId::next())
                .unwrap(),
            vec![PathPoint {
                p: Vec2(10.0, 20.0),
                in_tan: Vec2(-3.0, 4.0),
                out_tan: Vec2(5.0, -6.0),
            }],
            false,
        );
        let original = path_points(&node).unwrap().to_vec();
        let moved = moved_shape_node(&node, (10.0, 20.0), Some(&original), (7.0, -2.0), 0).unwrap();
        let point = path_points(&moved).unwrap()[0];
        assert_eq!(point.p, Vec2(17.0, 18.0));
        assert_eq!(point.in_tan, Vec2(-3.0, 4.0));
        assert_eq!(point.out_tan, Vec2(5.0, -6.0));

        let repeated =
            moved_shape_node(&moved, (10.0, 20.0), Some(&original), (7.0, -2.0), 0).unwrap();
        assert_eq!(
            path_points(&repeated),
            path_points(&moved),
            "repeated preview events must recompute from the drag origin"
        );
    }

    #[test]
    fn path_handle_editing_moves_only_the_requested_vector() {
        let original = vec![PathPoint {
            p: Vec2(10.0, 20.0),
            in_tan: Vec2(-3.0, 4.0),
            out_tan: Vec2(5.0, -6.0),
        }];
        let edited = edited_path_points(&original, 0, PathHandleKind::OutTangent, (2.0, 3.0));
        assert_eq!(edited[0].p, original[0].p);
        assert_eq!(edited[0].in_tan, original[0].in_tan);
        assert_eq!(edited[0].out_tan, Vec2(7.0, -3.0));

        let moved_point = edited_path_points(&original, 0, PathHandleKind::Point, (2.0, 3.0));
        assert_eq!(moved_point[0].p, Vec2(12.0, 23.0));
        assert_eq!(moved_point[0].in_tan, original[0].in_tan);
        assert_eq!(moved_point[0].out_tan, original[0].out_tan);

        assert_eq!(
            path_handle_hit(&[corner_path_point((10.0, 20.0))], (10.0, 20.0), 5.0),
            Some((0, PathHandleKind::Point)),
            "zero tangents must not mask their corner point"
        );
    }

    #[test]
    fn custom_path_replaces_template_placeholder_and_keeps_wiring() {
        let registry = registry();
        let (doc, _path) = doc_with_network(Graph::new());
        let comp = doc.root_comp.unwrap();
        let (doc, path, node) = create_layer_with_custom_path(
            &doc,
            comp,
            &registry,
            vec![
                corner_path_point((10.0, 10.0)),
                corner_path_point((100.0, 100.0)),
            ],
        )
        .unwrap();
        let graph = ravel_ui::document::resolve_network(&doc, &path).unwrap();
        assert!(
            graph
                .nodes()
                .all(|candidate| candidate.type_key != "shape.rect")
        );
        assert_eq!(graph.node(node).unwrap().type_key, "shape.custom_path");
        let rasterize = graph
            .nodes()
            .find(|candidate| candidate.type_key == "rasterize")
            .unwrap();
        assert!(
            graph
                .edges()
                .any(|edge| edge.source == node && edge.target == rasterize.id)
        );
        assert_eq!(doc.validate(), Ok(()));
    }

    #[test]
    fn custom_path_creation_is_one_undo_step() {
        use ravel_ui::document::DocumentStore;
        let registry = registry();
        let (doc, path) = doc_with_network(
            Graph::new()
                .add_node(registry.create_node("rasterize", NodeId::next()).unwrap())
                .unwrap(),
        );
        let mut store = DocumentStore::new(doc);

        let (doc, node) = create_custom_path(
            store.document(),
            &path,
            &registry,
            vec![corner_path_point((10.0, 10.0))],
        )
        .unwrap();
        store.apply(doc);
        let graph = ravel_ui::document::resolve_network(store.document(), &path).unwrap();
        let updated = custom_path_node(
            graph.node(node).unwrap().as_ref().clone(),
            vec![
                corner_path_point((10.0, 10.0)),
                smooth_path_point((50.0, 50.0), (60.0, 50.0)),
            ],
            true,
        );
        let graph = graph.clone().replace_node(Arc::new(updated));
        let doc = ravel_ui::document::replace_network(store.document(), &path, graph).unwrap();
        store.apply(doc.clone());
        store.commit(doc);

        assert!(store.undo());
        assert!(
            ravel_ui::document::resolve_network(store.document(), &path)
                .unwrap()
                .node(node)
                .is_none(),
            "one undo removes the session node and wiring"
        );
        assert!(!store.can_undo(), "no point preview became an undo step");
    }

    #[test]
    fn one_point_pen_session_reverts_without_a_node() {
        use ravel_ui::document::DocumentStore;
        let registry = registry();
        let (doc, path) = doc_with_network(Graph::new());
        let mut store = DocumentStore::new(doc);
        let (doc, node) = create_custom_path(
            store.document(),
            &path,
            &registry,
            vec![corner_path_point((10.0, 10.0))],
        )
        .unwrap();
        store.apply(doc);
        assert!(store.revert());
        assert!(
            ravel_ui::document::resolve_network(store.document(), &path)
                .unwrap()
                .node(node)
                .is_none()
        );
        assert!(!store.can_undo());
    }

    // -----------------------------------------------------------------------
    // Multi-layer move (REQ-UI-013 unit 6)
    // -----------------------------------------------------------------------

    /// Two layers, each holding one rect node, selected together in the active
    /// composition. The panel is given a 1:1 viewport, so comp coordinates and
    /// pointer pixels are the same numbers.
    fn multi_layer_setup(
        cx: &mut TestAppContext,
    ) -> (
        WindowHandle<ViewerPanel>,
        Entity<ProjectState>,
        ravel_core::id::CompId,
        Vec<ravel_core::id::LayerId>,
    ) {
        use ravel_core::id::LayerId;

        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(ProjectStateHandle(project.downgrade()));
            cx.set_global(crate::panels::SelectedPropertiesTarget::default());
            cx.set_global(CanvasSelection::default());
            cx.set_global(crate::panels::PlaybackPosition::default());
        });

        let rect = |center: (f32, f32)| {
            Graph::new()
                .add_node(shape_node(
                    "shape.rect",
                    &[
                        ("center_x", center.0),
                        ("center_y", center.1),
                        ("width", 100.0),
                        ("height", 100.0),
                    ],
                ))
                .unwrap()
        };
        let (comp_id, layers) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let ids = vec![LayerId::next(), LayerId::next()];
            let mut doc = project.document().clone();
            for (index, id) in ids.iter().enumerate() {
                doc = ravel_ui::document::add_layer(
                    &doc,
                    comp_id,
                    Layer::new(*id, format!("L{index}"), rect((100.0 * index as f32, 0.0)))
                        .with_time(0, 0, 300),
                )
                .unwrap();
            }
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (comp_id, ids)
        });
        cx.update(|cx| crate::panels::set_layer_selection(layers.clone(), cx));

        let window = cx.add_window(ViewerPanel::new);
        window
            .update(cx, |panel, _window, _cx| {
                panel.composition_resolution = Some((1920, 1080));
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
            })
            .unwrap();
        (window, project, comp_id, layers)
    }

    fn rect_center(
        project: &Entity<ProjectState>,
        comp_id: ravel_core::id::CompId,
        layer: ravel_core::id::LayerId,
        cx: &mut TestAppContext,
    ) -> (f32, f32) {
        project.read_with(cx, |project, _| {
            let layer = project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer)
                .unwrap()
                .clone();
            let node = layer.network.nodes().next().unwrap().clone();
            (
                sample_float_param(&node, "center_x", 0, &eval_ctx()).unwrap(),
                sample_float_param(&node, "center_y", 0, &eval_ctx()).unwrap(),
            )
        })
    }

    /// Dragging inside one selected layer's bbox moves every selected layer by
    /// the same comp-space delta, and the whole gesture is one undo step.
    #[gpui::test]
    fn a_multi_layer_drag_moves_the_selection_in_one_undo(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let before: Vec<(f32, f32)> = layers
            .iter()
            .map(|layer| rect_center(&project, comp_id, *layer, cx))
            .collect();

        window
            .update(cx, |panel, _window, cx| {
                // (0, 0) is inside the first layer's rect (centered there).
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                assert_eq!(
                    panel.move_drag.as_ref().map(|drag| drag.targets.len()),
                    Some(2),
                    "every selected layer joins the gesture"
                );
                panel.move_dragged(point(px(40.0), px(25.0)), cx);
                panel.move_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        for (layer, origin) in layers.iter().zip(&before) {
            let moved = rect_center(&project, comp_id, *layer, cx);
            assert_eq!(
                moved,
                (origin.0 + 40.0, origin.1 + 25.0),
                "each selected layer moved by the drag delta"
            );
        }

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        for (layer, origin) in layers.iter().zip(&before) {
            assert_eq!(
                rect_center(&project, comp_id, *layer, cx),
                *origin,
                "one undo restores every layer"
            );
        }
        // One undo was enough for the whole gesture: the layers themselves are
        // still there (the next undo step is their creation).
        project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layers
                    .len(),
                2
            );
        });
    }

    #[gpui::test]
    fn cancelling_a_move_discards_a_foreign_commit_of_its_preview(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let snapshot = project.read_with(cx, |project, _| project.document().clone());

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                panel.move_dragged(point(px(40.0), px(25.0)), cx);
            })
            .unwrap();

        project.update(cx, |project, cx| {
            let polluted =
                ravel_ui::document::update_layer(project.document(), comp_id, layers[0], |layer| {
                    layer.name = "foreign commit".into()
                })
                .unwrap();
            project.commit_document(polluted, InvalidationHint::Structural, cx);
        });

        window
            .update(cx, |panel, _window, cx| panel.cancel_move(cx))
            .unwrap();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "Escape restores the gesture-begin document, not the foreign commit"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            project.read_with(cx, |project, _| {
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layer_count()
            }),
            0,
            "the polluted commit was removed rather than left in undo history"
        );
    }

    /// A transformed layer cannot be moved by this gesture (the drag writes
    /// comp-space deltas into layer-local parameters), so pressing inside *its*
    /// bbox must not drag the rest of the selection either.
    #[gpui::test]
    fn a_press_on_a_transformed_layer_starts_no_drag(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::AnimationChannel;

        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        // The second layer's rect is centered at (100, 0); rotating its shell
        // keeps the bbox but takes it out of the movable set.
        project.update(cx, |project, cx| {
            let doc =
                ravel_ui::document::update_layer(project.document(), comp_id, layers[1], |layer| {
                    layer.transform.rotation = AnimationChannel::constant(45.0)
                })
                .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, cx| {
                // Inside the rotated layer's bbox only.
                panel.layer_move_mouse_down((140.0, 0.0), cx);
                assert!(
                    panel.move_drag.is_none(),
                    "pressing a layer this gesture cannot move starts nothing"
                );
                // Inside the untransformed layer: the gesture starts, and only
                // the movable layer takes part.
                panel.layer_move_mouse_down((-30.0, 0.0), cx);
                assert_eq!(
                    panel.move_drag.as_ref().map(|drag| drag.targets.len()),
                    Some(1),
                    "the transformed layer keeps its bbox but does not move"
                );
            })
            .unwrap();
    }

    /// A press outside every selected layer starts nothing — the click belongs
    /// to whoever owns deselection, not to a move.
    #[gpui::test]
    fn a_press_outside_the_selected_layers_starts_no_drag(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _layers) = multi_layer_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((900.0, 700.0), cx);
                assert!(panel.move_drag.is_none());
            })
            .unwrap();
    }

    /// Each target writes at its OWN layer-local frame (REQ-LAYER-006): two
    /// layers with different `start_frame` and keyframed centers must key the
    /// frames their own timing maps the playhead to, not one shared frame.
    #[gpui::test]
    fn a_multi_layer_drag_keys_each_layer_at_its_own_local_frame(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        use ravel_core::id::LayerId;

        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);
        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(ProjectStateHandle(project.downgrade()));
            cx.set_global(crate::panels::SelectedPropertiesTarget::default());
            cx.set_global(CanvasSelection::default());
            // The playhead sits at comp frame 10.
            cx.set_global(crate::panels::PlaybackPosition {
                frame: 10,
                fps: FrameRate::new(30, 1),
            });
        });

        // center_x is animated with a flat curve, so both layers read 100 at
        // every local frame and the drag has one obvious expected value.
        let animated_rect = || {
            let mut curve = KeyframeCurve::new();
            curve.insert(0, 100.0, Interpolation::Linear);
            curve.insert(60, 100.0, Interpolation::Linear);
            let node = Node::new(ravel_core::id::NodeId::next(), "shape.rect")
                .with_param(
                    "center_x",
                    ParameterValue::Channel(AnimationChannel::keyframes(curve)),
                )
                .with_param("center_y", ParameterValue::Float(100.0))
                .with_param("width", ParameterValue::Float(100.0))
                .with_param("height", ParameterValue::Float(100.0));
            Graph::new().add_node(node).unwrap()
        };
        let (comp_id, early, late) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let (early, late) = (LayerId::next(), LayerId::next());
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                // local frame 10 under the playhead
                Layer::new(early, "early", animated_rect()).with_time(0, 0, 300),
            )
            .unwrap();
            let doc = ravel_ui::document::add_layer(
                &doc,
                comp_id,
                // start_frame 10 => local frame 0 under the playhead
                Layer::new(late, "late", animated_rect()).with_time(10, 0, 300),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (comp_id, early, late)
        });
        cx.update(|cx| crate::panels::set_layer_selection(vec![early, late], cx));

        let window = cx.add_window(ViewerPanel::new);
        window
            .update(cx, |panel, _window, cx| {
                panel.composition_resolution = Some((1920, 1080));
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
                // (100, 100) is the shared rect center.
                panel.layer_move_mouse_down((100.0, 100.0), cx);
                panel.move_dragged(point(px(150.0), px(100.0)), cx);
                panel.move_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        let curve_of = |layer: ravel_core::id::LayerId, cx: &mut TestAppContext| {
            project.read_with(cx, |project, _| {
                let node = project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layer)
                    .unwrap()
                    .network
                    .nodes()
                    .next()
                    .unwrap()
                    .clone();
                let param = node
                    .parameters
                    .iter()
                    .find(|param| param.key == "center_x")
                    .unwrap()
                    .clone();
                match param.value {
                    ParameterValue::Channel(channel) => match channel.source {
                        ChannelSource::Keyframes(curve) => curve,
                        other => panic!("center_x lost its keyframes: {other:?}"),
                    },
                    other => panic!("center_x lost its channel: {other:?}"),
                }
            })
        };

        let keys = |curve: &ravel_core::animation::curve::KeyframeCurve| {
            curve
                .keyframes()
                .iter()
                .map(|key| (key.frame, key.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&curve_of(early, cx)),
            vec![(0, 100.0), (10, 150.0), (60, 100.0)],
            "the layer starting at 0 is keyed at local frame 10, and only there"
        );
        assert_eq!(
            keys(&curve_of(late, cx)),
            vec![(0, 150.0), (60, 100.0)],
            "the layer starting at 10 is keyed at its own local frame 0"
        );
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal Viewer panel: displays the frame from the current evaluation
//! result. `ProjectState`'s background evaluation publishes either a CPU
//! [`RenderImage`] fallback or a display-encoded GPU texture via
//! [`super::ViewerFrame`]. The shared-device path uses GPUI's custom surface
//! primitive directly; unsupported hosts keep the existing textured-quad
//! fallback. A failed evaluation drops the stale frame and shows a black
//! frame with a small error overlay, so structural edits (e.g. deleting a
//! Geometry node feeding a Rasterize) are immediately visible instead of
//! leaving stale content.

pub mod field;
pub mod geometry;
pub mod motion_path;
pub mod overlay;
pub mod snap;
mod viewport;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, Selectable as _, Sizable as _};
use ravel_i18n::t;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::{CanvasSelection, ToolState, ViewerFrame, track_panel_focus};
use crate::assets::RavelIcon;
use crate::panels::media_bin::{DraggedAsset, add_assets_as_layers, dropped_asset_ids};
use crate::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::id::{CompId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use ravel_core::runtime::InvalidationHint;
use ravel_gpu::GpuFrameBuffer;
use ravel_ui::document::NetworkPath;
use viewport::ViewerViewport;

use super::param_edit::edited_vector_param;
use overlay::{
    ActiveDrag, DragModifiers, LabelPlacement, OverlayColors, OverlayContext, OverlayEdit,
    OverlayHandle, OverlayPainter, OverlayRegistry, OverlayResults, ShellHandle,
};
use snap::{SnapGuides, SnapLines};

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
    /// Snap candidates and the rectangle the gesture moves, both as they stood
    /// at press time. Captured once rather than recomputed per move: the
    /// candidates do not change during a drag, and reading them from the
    /// previewed document would measure the layer this gesture is moving.
    snap: SnapTarget,
    changed: bool,
}

/// What a gesture needs to snap: the candidate lines, and the composition-space
/// rectangle whose edges and centre are pulled onto them. `rect` is `None` when
/// the gesture has nothing measurable to align — snapping then stands down
/// rather than guessing a rectangle.
#[derive(Clone, Default)]
struct SnapTarget {
    lines: SnapLines,
    rect: Option<CompRect>,
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
    /// Candidates for the drawing pointer. The drawn rectangle is not known
    /// until the move, so only the lines are captured here and the moving
    /// "rectangle" is the pointer itself.
    snap_lines: SnapLines,
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
pub enum PathHandleKind {
    Point,
    InTangent,
    OutTangent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewerPointerHint {
    #[default]
    Empty,
    Drawing,
    MovableBody,
    PathAnchor,
    PathTangent,
    PenClose,
    /// Layer shell scale, on the ↖↘ diagonal.
    ResizeUpLeftDownRight,
    /// Layer shell scale, on the ↗↙ diagonal.
    ResizeUpRightDownLeft,
    /// Layer shell scale, horizontal edge grip.
    ResizeLeftRight,
    /// Layer shell scale, vertical edge grip.
    ResizeUpDown,
    /// Layer shell rotation, in the ring outside a corner grip.
    Rotate,
    /// The layer shell's anchor marker.
    ShellAnchor,
}

impl ViewerPointerHint {
    fn cursor(self) -> CursorStyle {
        match self {
            Self::Empty => CursorStyle::Arrow,
            Self::Drawing | Self::PathTangent => CursorStyle::Crosshair,
            // GPUI-CE has no generic `Move` cursor. OpenHand communicates the
            // same grab-to-move affordance and matches the Node Editor.
            Self::MovableBody => CursorStyle::OpenHand,
            // Both anchors are "a point you can pick up"; one glyph for both
            // keeps the promise consistent across overlays.
            Self::PathAnchor | Self::ShellAnchor => CursorStyle::PointingHand,
            Self::PenClose => CursorStyle::DragCopy,
            // The scale grips finally have a gesture behind them, which is
            // what `done/pointer-feedback-plan.md` was waiting for before
            // assigning `Resize*` (a cursor is a promise about what works).
            Self::ResizeUpLeftDownRight => CursorStyle::ResizeUpLeftDownRight,
            Self::ResizeUpRightDownLeft => CursorStyle::ResizeUpRightDownLeft,
            Self::ResizeLeftRight => CursorStyle::ResizeLeftRight,
            Self::ResizeUpDown => CursorStyle::ResizeUpDown,
            // GPUI-CE has no rotation cursor and no custom bitmaps, so the 24
            // built-ins have to supply a stand-in. `DragLink` is the only one
            // whose glyph carries a curved arrow — it reads as "turn" rather
            // than "move", and nothing else in the Viewer uses it.
            Self::Rotate => CursorStyle::DragLink,
        }
    }
}

fn viewer_pointer_hint_transition(
    current: ViewerPointerHint,
    next: ViewerPointerHint,
    dragging: bool,
) -> Option<ViewerPointerHint> {
    (!dragging && current != next).then_some(next)
}

fn viewer_drag_cursor(
    pan: bool,
    moving: bool,
    drawing_shape: bool,
    drawing_pen: bool,
    path_handle: Option<PathHandleKind>,
    shell_handle: Option<ViewerPointerHint>,
) -> Option<CursorStyle> {
    // A shell grip keeps the cursor the pointer showed before the press: the
    // gesture is the one the hover promised, so changing the glyph mid-drag
    // would only unsay it.
    if let Some(hint) = shell_handle {
        return Some(hint.cursor());
    }
    if pan || moving || path_handle == Some(PathHandleKind::Point) {
        Some(CursorStyle::ClosedHand)
    } else if drawing_shape || drawing_pen || path_handle.is_some() {
        Some(CursorStyle::Crosshair)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ViewerBackgroundMode {
    #[default]
    Composition,
    Checkerboard,
    Solid,
}

impl ViewerBackgroundMode {
    const ALL: [Self; 3] = [Self::Composition, Self::Checkerboard, Self::Solid];

    fn label_key(self) -> &'static str {
        match self {
            Self::Composition => "viewer.background_composition",
            Self::Checkerboard => "viewer.background_checkerboard",
            Self::Solid => "viewer.background_solid",
        }
    }
}

/// One in-flight overlay handle drag. The context captured at press time is
/// what [`overlay::ViewerOverlay::drag`] reads, so every move recomputes the
/// edit from the original state instead of compounding onto its own preview.
#[derive(Clone)]
struct OverlayHandleDrag {
    handle: OverlayHandle,
    press_context: OverlayContext,
    /// The zero-delta edit, kept so the gesture can tell whether its target
    /// still exists after another panel changes the document.
    press_edit: OverlayEdit,
    pointer_start: (f32, f32),
    original_document: Document,
    /// Snap candidates and the shell's bbox at press time. `rect` is `None`
    /// for the grips that move a single point — they snap that point instead,
    /// through [`snap_rect_for_handle`].
    snap: SnapTarget,
    /// Invalidation the applied edits ask for, committed with the gesture.
    invalidation: InvalidationHint,
    changed: bool,
}

pub struct ViewerPanel {
    /// The current frame converted for GPUI rendering. Rebuilt only when
    /// [`ViewerFrame`] changes, never during `render()`.
    image: Option<Arc<RenderImage>>,
    /// The current display-encoded texture for GPUI's native surface path.
    gpu_frame: Option<GpuFrameBuffer>,
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
    handle_drag: Option<OverlayHandleDrag>,
    /// The lines the drag in flight is snapped to. Read back only while a
    /// gesture is live (see [`Self::overlay_context`]), so a guide cannot
    /// outlive the correction it reports.
    snap_guides: SnapGuides,
    pointer_hint: ViewerPointerHint,
    /// Proportional (3x3) grid overlay toggle.
    show_grid: bool,
    /// Action-safe (90%) / title-safe (80%) overlay toggle.
    show_safe_areas: bool,
    /// Selection bounding-box overlay toggle. On by default — the outline is
    /// what tells the user which shape a gesture will act on.
    show_geometry_bounds: bool,
    /// Evaluated geometry point / instance markers.
    show_geometry_points: bool,
    /// Evaluated geometry path outlines.
    show_geometry_paths: bool,
    /// The geometry attribute drawn as arrows, or `None` for none.
    geometry_arrow_attr: Option<SharedString>,
    /// Element index labels over the drawn geometry marks.
    show_geometry_indices: bool,
    /// Colour the geometry marks by group membership.
    show_geometry_groups: bool,
    /// What the field overlay draws, if anything.
    field_display: field::FieldDisplay,
    field_map: field::FieldColorMap,
    field_opacity: f32,
    /// Session-local transparency preview background.
    background_mode: ViewerBackgroundMode,
    /// The selection the last overlay evaluation was requested for.
    ///
    /// Overlay targets are collected while the request is assembled, so a
    /// selection change has to post a new one — and `observe_global` fires on
    /// every `set_global`, including the re-publish a click on an already
    /// selected node performs, so the request is posted on a real change only.
    requested_selection: Option<(CanvasSelection, super::LayerSelection)>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
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
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(instance, &focus_handle, window, cx);

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
            this.pointer_hint = if matches!(
                state.active,
                ravel_ui::ToolKind::Pen | ravel_ui::ToolKind::Rect | ravel_ui::ToolKind::Ellipse
            ) {
                ViewerPointerHint::Drawing
            } else {
                ViewerPointerHint::Empty
            };
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
            if this.handle_drag.as_ref().is_some_and(|drag| {
                this.project(cx).is_none_or(|project| {
                    !drag.press_edit.target_exists(project.read(cx).document())
                })
            }) {
                this.handle_drag = None;
            }
            // A node parameter is manipulable only while its node is the
            // selected one, so a selection that moved elsewhere has to revert
            // the preview rather than leave it to be committed against a node
            // nobody is looking at — the shell manipulator's rule, applied to
            // the node side of `OverlayEdit`.
            let selection = cx
                .try_global::<CanvasSelection>()
                .cloned()
                .unwrap_or_default();
            if this.handle_drag.as_ref().is_some_and(|drag| {
                drag.press_edit
                    .node_target()
                    .is_some_and(|(network, node)| {
                        selection.path.as_ref() != Some(&network)
                            || !selection.nodes.contains(&node)
                    })
            }) {
                this.cancel_handle_drag(cx);
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
            this.request_overlay_eval(cx);
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
            // A shell drag belongs to the selection the same way: the
            // manipulator only exists while its layer is the one selected
            // layer, so losing that selection has to revert the preview
            // instead of committing it to a layer nobody is looking at.
            if this.handle_drag.as_ref().is_some_and(|drag| {
                drag.press_edit.layer_target().is_some_and(|(comp, layer)| {
                    selection.comp() != Some(comp) || !selection.contains(layer)
                })
            }) {
                this.cancel_handle_drag(cx);
            }
            this.request_overlay_eval(cx);
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
            //
            // Since the conversion moved to the worker the image is shared
            // (the global carries it and every Viewer instance takes the same
            // `Arc`), so a drop can free an entry another instance still
            // draws. That costs a re-upload, not a missing frame:
            // `Window::paint_image` inserts on a cache miss. Dropping is
            // still what bounds VRAM, and the atlas remove is idempotent.
            if let Some(old) = std::mem::replace(&mut this.image, content.image) {
                cx.defer(move |cx| cx.drop_image(old, None));
            }
            if let Some(old) = std::mem::replace(&mut this.gpu_frame, content.gpu_frame) {
                // Hold the outgoing lease one turn past the swap so the frame
                // GPUI is still painting is not returned to the pool underneath
                // it.
                //
                // **This is a delay, not a synchronisation.** `defer` runs when
                // the app finishes the current update; Metal may not have
                // finished the command buffer that samples the texture. The
                // window is one turn wide rather than zero, which is enough for
                // the interactive rates this path is for and is why the picture
                // holds in practice — but a stall between the two timelines can
                // still let the pool hand this texture to the next frame while
                // the old one is on screen. Closing it needs the renderer's
                // completion signal (a `MTLSharedEvent` or GPUI's
                // command-buffer callback), which is `ZC-4`'s subject; the
                // lease must not be released on a timer until then.
                cx.defer(move |_cx| drop(old));
            }
            cx.notify();
        });

        // Release the last frame's atlas entry when the panel goes away.
        cx.on_release(|this: &mut Self, cx| {
            if let Some(old) = this.image.take() {
                cx.drop_image(old, None);
            }
            this.gpu_frame.take();
        })
        .detach();

        let initial = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
        let content = viewer_content(initial);

        Self {
            image: content.image,
            gpu_frame: content.gpu_frame,
            error: content.error,
            composition_resolution: content.composition_resolution,
            viewport: ViewerViewport::default(),
            viewport_origin: Rc::new(Cell::new((0.0, 0.0))),
            viewport_size: Rc::new(Cell::new((0.0, 0.0))),
            pan_drag: None,
            move_drag: None,
            shape_drag: None,
            pen_session: None,
            handle_drag: None,
            snap_guides: SnapGuides::default(),
            pointer_hint: ViewerPointerHint::default(),
            show_grid: false,
            show_safe_areas: false,
            show_geometry_bounds: true,
            show_geometry_points: false,
            show_geometry_paths: false,
            geometry_arrow_attr: None,
            show_geometry_indices: false,
            show_geometry_groups: false,
            field_display: field::FieldDisplay::default(),
            field_map: field::FieldColorMap::default(),
            field_opacity: field::DEFAULT_FIELD_OPACITY,
            background_mode: ViewerBackgroundMode::default(),
            requested_selection: None,
            focus_handle,
            focus_subscriptions,
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
        let eval = EvalContext::new(position.frame, position.fps, resolution);
        let shell = world_matrix(comp, layer, &eval);
        // Network parameters live in layer-local time (REQ-LAYER-006): the
        // drag origins below must sample the same frame the keyframe writes
        // target.
        let local_frame = ravel_ui::keyframes::layer_local_frame(layer, position.frame);
        let overlay_ctx = self.overlay_context(cx);
        let hit = hit_test_shape_nodes(&overlay_ctx, &network, pointer);
        let nodes = selection_after_click(&selection.nodes, hit, event.modifiers.shift);
        // Publish both the durable selection and its Properties projection,
        // including a plain click on an already-selected node. This mirrors
        // the Node Editor and restores node Properties if another panel had
        // temporarily published a different target.
        Self::publish_selection(network.clone(), nodes.clone(), cx);

        if event.modifiers.shift || hit.is_none() || !shell.is_identity() {
            return;
        }
        let Some(graph) = ravel_ui::document::resolve_network(&document, &network) else {
            return;
        };
        let origins: Vec<_> = nodes
            .iter()
            .filter_map(|id| {
                let node = graph.node(*id)?;
                let bounds = geometry::evaluated_bounds(&overlay_ctx, &network, *id)?;
                Some(MoveOrigin {
                    node: *id,
                    center: sample_vec2_param(node, "center", local_frame, &eval)
                        .unwrap_or((bounds.x + bounds.w * 0.5, bounds.y + bounds.h * 0.5)),
                    path_points: path_points(node).map(<[ravel_core::graph::PathPoint]>::to_vec),
                })
            })
            .collect();
        if !origins.is_empty() {
            // The nodes being dragged are what moves, so their union is the
            // rectangle snapping aligns — and the layer holding them is left
            // out of the candidates, since its bbox travels with them.
            let snap = SnapTarget {
                lines: SnapLines::collect(&overlay_ctx, Some(network.comp), &[network.layer]),
                rect: origins
                    .iter()
                    .filter_map(|origin| node_comp_rect(&overlay_ctx, &network, origin.node))
                    .reduce(union_rect),
            };
            // A gesture that has not moved yet has corrected nothing:
            // the previous one's guide must not survive into this frame.
            self.snap_guides = SnapGuides::default();
            self.move_drag = Some(MoveDrag {
                pointer_start: pointer,
                targets: vec![MoveTarget {
                    network,
                    origins,
                    local_frame,
                }],
                original_document: document,
                snap,
                changed: false,
            });
        }
    }

    /// Mouse-down with several layers selected: start moving all of them when
    /// the pointer is inside one of their bboxes (REQ-UI-013).
    ///
    /// Only layers whose compositing chain transform is identity take part — the
    /// drag writes comp-space deltas into layer-local `center`
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
        let overlay_ctx = self.overlay_context(cx);

        let mut hit = false;
        let mut targets = Vec::new();
        let mut moved_rect: Option<CompRect> = None;
        for layer_id in selection.layers() {
            let Some(layer) = comp.get_layer(*layer_id) else {
                continue;
            };
            let Some(rect) = layer_comp_rect(&overlay_ctx, &document, comp_id, *layer_id) else {
                continue;
            };
            let shell = world_matrix(comp, layer, &eval);
            if !shell.is_identity() {
                // A transformed layer is not movable, so pressing inside its
                // bbox must not drag the rest of the selection either: the
                // press has to land on something this gesture can actually move.
                continue;
            }
            let network = NetworkPath::layer(comp_id, *layer_id);
            let local_frame = ravel_ui::keyframes::layer_local_frame(layer, position.frame);
            let origins: Vec<MoveOrigin> = layer_geometry_nodes(&overlay_ctx, &network)
                .into_iter()
                .filter_map(|id| {
                    let node = layer.network.node(id)?;
                    let bounds = geometry::evaluated_bounds(&overlay_ctx, &network, id)?;
                    Some(MoveOrigin {
                        node: id,
                        center: sample_vec2_param(node, "center", local_frame, &eval)
                            .unwrap_or((bounds.x + bounds.w * 0.5, bounds.y + bounds.h * 0.5)),
                        path_points: path_points(node)
                            .map(<[ravel_core::graph::PathPoint]>::to_vec),
                    })
                })
                .collect();
            if origins.is_empty() {
                continue;
            }
            hit |= rect_contains(&rect, pointer);
            moved_rect = Some(match moved_rect {
                Some(union) => union_rect(union, rect),
                None => rect,
            });
            targets.push(MoveTarget {
                network,
                origins,
                local_frame,
            });
        }
        // A click outside every selected layer is not a move: it leaves the
        // selection alone (the panels that own it decide deselection).
        if !hit || targets.is_empty() {
            return;
        }
        // Every layer taking part is excluded from the candidates, so the
        // gesture aligns against the layers it is *not* moving.
        let moving: Vec<LayerId> = targets.iter().map(|target| target.network.layer).collect();
        // A gesture that has not moved yet has corrected nothing: the
        // previous one's guide must not survive into this frame.
        self.snap_guides = SnapGuides::default();
        self.move_drag = Some(MoveDrag {
            pointer_start: pointer,
            targets,
            original_document: document,
            snap: SnapTarget {
                lines: SnapLines::collect(&overlay_ctx, Some(comp_id), &moving),
                rect: moved_rect,
            },
            changed: false,
        });
    }

    fn move_dragged(
        &mut self,
        position: Point<Pixels>,
        modifiers: DragModifiers,
        cx: &mut Context<Self>,
    ) {
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
        // Snapping only corrects this delta; the preview and the commit below
        // are untouched, so the gesture stays one undo step.
        let delta = self.snapped_delta(&drag.snap.lines, drag.snap.rect, delta, modifiers);
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
            cx.notify();
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
            cx.notify();
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
        // The layer being drawn into is excluded: the node this gesture creates
        // enters that layer's bbox, so leaving it in would let the shape snap
        // to itself from the first move onwards.
        let comp = previous_selection
            .path
            .as_ref()
            .map(|path| path.comp)
            .or_else(|| crate::panels::active_composition(cx));
        let drawn_into: Vec<LayerId> = previous_selection
            .path
            .as_ref()
            .map(|path| vec![path.layer])
            .unwrap_or_default();
        let snap_lines = self.snap_lines(comp, &drawn_into, cx);
        // A gesture that has not moved yet has corrected nothing: the
        // previous one's guide must not survive into this frame.
        self.snap_guides = SnapGuides::default();
        self.shape_drag = Some(ShapeDrag {
            kind,
            start: pointer,
            previous_selection,
            original_document,
            snap_lines,
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
        let modifiers = drag_modifiers(&event.modifiers);
        // The moving corner of the drawn rectangle is the pointer itself, so
        // that single point is what snapping aligns: the opposite corner is
        // pinned at the press and never moves.
        //
        // Except under Shift: `drag_geometry` then squares the shape off from
        // the larger of the two deltas, which overwrites whichever axis had
        // just been snapped — the guide would name a line the drawn corner
        // misses. A constrained drag snaps nothing.
        let corner = (!modifiers.shift).then(|| point_rect(pointer));
        let snapped = self.snapped_delta(&drag.snap_lines, corner, (0.0, 0.0), modifiers);
        let pointer = (pointer.0 + snapped.0, pointer.1 + snapped.1);
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
            cx.notify();
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
            cx.notify();
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

    /// Resolve a press against the overlay handles, topmost overlay first.
    /// The only entry point from the pointer to an overlay drag.
    fn overlay_handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        // A pen session owns the pointer until it is finalized.
        if self.pen_session.is_some() {
            return false;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return false;
        };
        let context = self.overlay_context(cx);
        let registry = OverlayRegistry::builtin();
        let Some(handle) = registry.hit_test_draggable(&context, pointer, self.comp_per_pixel())
        else {
            return false;
        };
        let Some(original_document) = context.document.clone() else {
            return false;
        };
        let Some(press_edit) = registry.overlay(handle.overlay).and_then(|overlay| {
            overlay.drag(&handle, (0.0, 0.0), DragModifiers::default(), &context)
        }) else {
            return false;
        };
        // Only the layer shell's grips snap: they move the layer the bboxes are
        // drawn from. A path point, a parameter mark or a motion key is not
        // this unit's subject, and pulling one onto a layer edge would move a
        // value the user is aiming by hand.
        let snap = match handle.id.shell() {
            Some(_) => SnapTarget {
                lines: SnapLines::collect(
                    &context,
                    context.layer_selection.comp(),
                    context.layer_selection.layers(),
                ),
                rect: layer_selection_comp_rects(&context).first().copied(),
            },
            None => SnapTarget::default(),
        };
        // A gesture that has not moved yet has corrected nothing: the
        // previous one's guide must not survive into this frame.
        self.snap_guides = SnapGuides::default();
        self.handle_drag = Some(OverlayHandleDrag {
            handle,
            press_context: context,
            press_edit,
            pointer_start: pointer,
            original_document,
            snap,
            invalidation: InvalidationHint::None,
            changed: false,
        });
        true
    }

    /// Composition pixels per screen pixel, the scale a handle's screen-space
    /// hit radius is measured in.
    fn comp_per_pixel(&self) -> f32 {
        self.comp_hit_radius(1.0).unwrap_or(1.0)
    }

    /// Whether a document-editing gesture is in flight. Panning is not one:
    /// it moves the view, not the picture.
    fn dragging(&self) -> bool {
        self.move_drag.is_some() || self.shape_drag.is_some() || self.handle_drag.is_some()
    }

    /// Correct a gesture's composition-space delta so an edge or the centre of
    /// `rect` lands on a snap candidate, and record the guides that report it.
    ///
    /// The one place a gesture's delta meets the snapping rules: the three drag
    /// paths differ only in what they pass as the moving rectangle.
    fn snapped_delta(
        &mut self,
        lines: &SnapLines,
        rect: Option<CompRect>,
        delta: (f32, f32),
        modifiers: DragModifiers,
    ) -> (f32, f32) {
        let result = self.snap_result(lines, rect, delta, modifiers);
        self.snap_guides = result.guides;
        result.delta
    }

    /// The correction itself, without recording it. The only place the
    /// screen-pixel threshold meets the panel's zoom, so every gesture reaches
    /// the same distance on screen.
    fn snap_result(
        &self,
        lines: &SnapLines,
        rect: Option<CompRect>,
        delta: (f32, f32),
        modifiers: DragModifiers,
    ) -> snap::SnapResult {
        let Some(rect) = rect else {
            return snap::SnapResult::unsnapped(delta);
        };
        snap::snap_delta(
            rect,
            delta,
            lines,
            snap::comp_threshold(self.comp_per_pixel()),
            modifiers,
        )
    }

    /// The snap candidates a gesture over `comp` sees, with the layers it moves
    /// left out.
    fn snap_lines(&self, comp: Option<CompId>, moving: &[LayerId], cx: &App) -> SnapLines {
        SnapLines::collect(&self.overlay_context(cx), comp, moving)
    }

    /// Snapshot the world the overlays are allowed to see. Read-only, and the
    /// same snapshot backs painting, labels and hit-testing.
    fn overlay_context(&self, cx: &App) -> OverlayContext {
        let project = self.project(cx);
        OverlayContext {
            resolution: self.composition_resolution,
            // The very factor `ProjectState::viewer_eval_context` puts in the
            // request, so a sampled field reads the `res.*` the frame under it
            // was evaluated with.
            eval_resolution: self.composition_resolution.map(|resolution| {
                project
                    .as_ref()
                    .map(|project| project.read(cx).viewer_resolution().apply(resolution))
                    .unwrap_or(resolution)
            }),
            playback: cx.try_global::<super::PlaybackPosition>().copied(),
            document: self
                .project(cx)
                .map(|project| project.read(cx).document().clone()),
            selection: cx.try_global::<CanvasSelection>().cloned(),
            layer_selection: super::layer_selection(cx),
            tool: cx.try_global::<ToolState>().map(|state| state.active),
            show_grid: self.show_grid,
            show_safe_areas: self.show_safe_areas,
            show_geometry_bounds: self.show_geometry_bounds,
            show_geometry_points: self.show_geometry_points,
            show_geometry_paths: self.show_geometry_paths,
            geometry_arrow_attr: self.geometry_arrow_attr.clone(),
            show_geometry_indices: self.show_geometry_indices,
            show_geometry_groups: self.show_geometry_groups,
            field_display: self.field_display,
            field_map: self.field_map,
            field_opacity: self.field_opacity,
            error: self.error.clone(),
            // Only while a gesture is live. The field keeps the last
            // correction, and a guide left on screen after the drag ended
            // would claim an alignment nothing is holding.
            snap_guides: if self.dragging() {
                self.snap_guides
            } else {
                SnapGuides::default()
            },
            active_drag: self.handle_drag.as_ref().map(|drag| ActiveDrag {
                handle: drag.handle.id,
                press_document: drag.original_document.clone(),
            }),
            colors: OverlayColors {
                // A bright semantic info color keeps the editable path legible
                // over both dark footage and the black composition background.
                path: cx.theme().colors.info,
                error: cx.theme().colors.danger,
            },
            // Written by `ProjectState` in the same update as `ViewerFrame`,
            // which this panel already observes — so reading it here needs no
            // observer of its own.
            results: cx
                .try_global::<OverlayResults>()
                .cloned()
                .unwrap_or_default(),
            registry: self
                .project(cx)
                .map(|project| project.read(cx).shared_registry()),
        }
    }

    /// Post a viewer evaluation when the selection changed.
    ///
    /// The overlays declare their evaluation targets while
    /// [`ProjectState::build_viewer_request`] assembles the request, and
    /// nothing else re-assembles one when only the selection moved: with the
    /// playhead stopped and the document untouched, selecting another layer,
    /// another network or another field node would leave the new target
    /// unevaluated and the overlay blank until the next frame step.
    ///
    /// Called from the selection observers, never from `render()`, and it
    /// rides the one existing request path rather than opening a second.
    fn request_overlay_eval(&mut self, cx: &mut Context<Self>) {
        let selection = (
            cx.try_global::<CanvasSelection>()
                .cloned()
                .unwrap_or_default(),
            super::layer_selection(cx),
        );
        if self.requested_selection.as_ref() == Some(&selection) {
            return;
        }
        self.requested_selection = Some(selection);
        let Some(project) = self.project(cx) else {
            return;
        };
        // Nothing about the document changed, so the evaluator keeps every
        // cached value and the new target is the only work this adds.
        project.update(cx, |project, cx| {
            project.request_viewer_eval(InvalidationHint::None, cx);
        });
    }

    fn comp_hit_radius(&self, pixels: f32) -> Option<f32> {
        let resolution = self.composition_resolution?;
        let rect = self.viewport.rect(self.viewport_size.get(), resolution);
        (rect.width > 0.0).then_some(pixels * resolution.0 as f32 / rect.width)
    }

    fn pointer_hint_at(&self, position: Point<Pixels>, cx: &App) -> Option<ViewerPointerHint> {
        let pointer = self.comp_position(position)?;
        let tool = cx
            .try_global::<ToolState>()
            .map(|state| state.active)
            .unwrap_or_default();
        let radius = self.comp_hit_radius(8.0).unwrap_or(8.0);

        if tool == ravel_ui::ToolKind::Pen
            && let Some(session) = &self.pen_session
            && let Some(points) = self.session_points(session, cx)
            && pen_close_pointer_hint(&points, pointer, radius).is_some()
        {
            return Some(ViewerPointerHint::PenClose);
        }

        if let Some(handle) = OverlayRegistry::builtin().hit_test(
            &self.overlay_context(cx),
            pointer,
            self.comp_per_pixel(),
        ) {
            return Some(handle.hint);
        }

        if tool == ravel_ui::ToolKind::Select && self.selected_body_contains(pointer, cx) {
            return Some(ViewerPointerHint::MovableBody);
        }

        Some(
            if matches!(
                tool,
                ravel_ui::ToolKind::Pen | ravel_ui::ToolKind::Rect | ravel_ui::ToolKind::Ellipse
            ) {
                ViewerPointerHint::Drawing
            } else {
                ViewerPointerHint::Empty
            },
        )
    }

    fn selected_body_contains(&self, pointer: (f32, f32), cx: &App) -> bool {
        let ctx = self.overlay_context(cx);
        let rects = if ctx.layer_selection.layers().len() >= 2 {
            layer_selection_comp_rects(&ctx)
        } else {
            selection_comp_rects(&ctx)
        };
        selected_body_pointer_hint(&rects, pointer).is_some()
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

    fn handle_dragged(
        &mut self,
        position: Point<Pixels>,
        modifiers: DragModifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        let registry = OverlayRegistry::builtin();
        // Borrow the press-time state rather than cloning it: this runs on
        // every mouse move, and the snapshot carries a document and a point
        // list.
        //
        // Snapping corrects the delta before the edit is derived from it, so
        // the gesture keeps its single preview / single commit shape.
        let (edit, snapped) = {
            let Some(drag) = self.handle_drag.as_ref() else {
                return;
            };
            let raw = (
                pointer.0 - drag.pointer_start.0,
                pointer.1 - drag.pointer_start.1,
            );
            let snapped = match snap_target_for_handle(&drag.handle, drag.snap.rect, modifiers) {
                Some((rect, axes)) => self
                    .snap_result(&drag.snap.lines, Some(rect), raw, modifiers)
                    .restrict(raw, axes),
                None => snap::SnapResult::unsnapped(raw),
            };
            let edit = registry.overlay(drag.handle.overlay).and_then(|overlay| {
                overlay.drag(&drag.handle, snapped.delta, modifiers, &drag.press_context)
            });
            (edit, snapped)
        };
        self.snap_guides = snapped.guides;
        let delta = snapped.delta;
        let Some(edit) = edit else {
            return;
        };
        if self.apply_overlay_edit(&edit, cx)
            && let Some(active) = &mut self.handle_drag
        {
            active.changed = delta != (0.0, 0.0);
            active.invalidation = edit.invalidation();
        }
    }

    /// Preview an overlay edit. The gesture becomes one undo step only when
    /// [`Self::handle_drag_ended`] commits.
    fn apply_overlay_edit(&self, edit: &OverlayEdit, cx: &mut Context<Self>) -> bool {
        let Some(project) = self.project(cx) else {
            return false;
        };
        let mut applied = false;
        project.update(cx, |project, cx| {
            let Some(document) = edit.apply(project.document()) else {
                return;
            };
            project.apply_document(document, edit.invalidation(), cx);
            applied = true;
        });
        if applied {
            cx.notify();
        }
        applied
    }

    fn handle_drag_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.handle_drag.take() else {
            return;
        };
        if drag.changed
            && let Some(project) = self.project(cx)
        {
            project.update(cx, |project, cx| {
                project.commit_document(project.document().clone(), drag.invalidation, cx);
            });
        }
        cx.notify();
    }

    fn cancel_handle_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.handle_drag.take() else {
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
        let background_entity = entity.clone();
        let background_mode = self.background_mode;
        let field_entity = entity.clone();
        let (field_display, field_map, field_opacity) =
            (self.field_display, self.field_map, self.field_opacity);
        let attr_entity = entity.clone();
        let arrow_attr = self.geometry_arrow_attr.clone();
        let (show_indices, show_groups) = (self.show_geometry_indices, self.show_geometry_groups);
        // What the picker can offer comes from the geometry the overlays draw,
        // so an attribute is listed exactly while it exists.
        let arrow_names = overlay::drawn_vector_attributes(&self.overlay_context(cx));
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
                Button::new("viewer-background-mode")
                    .xsmall()
                    .ghost()
                    .label(SharedString::from(t!(background_mode.label_key())))
                    .tooltip(t!("viewer.background_mode"))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for mode in ViewerBackgroundMode::ALL {
                            let entity = background_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(mode.label_key())))
                                    .checked(mode == background_mode)
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                if this.background_mode != mode {
                                                    this.background_mode = mode;
                                                    cx.notify();
                                                }
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    }),
            )
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
            .child(
                Button::new("viewer-geometry-bounds")
                    .xsmall()
                    .ghost()
                    .selected(self.show_geometry_bounds)
                    .icon(Icon::new(RavelIcon::GeometryBounds))
                    .tooltip(t!("viewer.geometry_bounds"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_geometry_bounds = !this.show_geometry_bounds;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("viewer-geometry-points")
                    .xsmall()
                    .ghost()
                    .selected(self.show_geometry_points)
                    .icon(Icon::new(RavelIcon::GeometryPoints))
                    .tooltip(t!("viewer.geometry_points"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_geometry_points = !this.show_geometry_points;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("viewer-geometry-paths")
                    .xsmall()
                    .ghost()
                    .selected(self.show_geometry_paths)
                    .icon(Icon::new(RavelIcon::GeometryPaths))
                    .tooltip(t!("viewer.geometry_paths"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.show_geometry_paths = !this.show_geometry_paths;
                        cx.notify();
                    })),
            )
            .child(
                // One menu for the three attribute visualisations, the way the
                // field menu holds its three: they answer "what about these
                // elements do I want to see", and the arrow picker is a list of
                // whatever the evaluated geometry actually carries rather than
                // a fixed set of reserved names.
                Button::new("viewer-geometry-attrs")
                    .xsmall()
                    .ghost()
                    .selected(
                        self.geometry_arrow_attr.is_some()
                            || self.show_geometry_indices
                            || self.show_geometry_groups,
                    )
                    .icon(Icon::new(RavelIcon::GeometryAttributes))
                    .tooltip(t!("viewer.geometry_attrs"))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        let selected = |name: Option<&str>| match (&arrow_attr, name) {
                            (Some(current), Some(name)) => current.as_ref() == name,
                            (None, None) => true,
                            _ => false,
                        };
                        for name in std::iter::once(None)
                            .chain(arrow_names.iter().map(|name| Some(name.as_str())))
                        {
                            let entity = attr_entity.clone();
                            let value: Option<SharedString> =
                                name.map(|name| SharedString::from(name.to_string()));
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(match name {
                                    Some(name) => {
                                        format!("{}: {name}", t!("viewer.geometry_arrows"))
                                    }
                                    None => t!("viewer.geometry_arrows_off").to_string(),
                                }))
                                .checked(selected(name))
                                .on_click(
                                    move |_, _window, cx| {
                                        let value = value.clone();
                                        entity
                                            .update(cx, |this, cx| {
                                                if this.geometry_arrow_attr != value {
                                                    this.geometry_arrow_attr = value;
                                                    cx.notify();
                                                }
                                            })
                                            .ok();
                                    },
                                ),
                            );
                        }
                        menu = menu.separator();
                        let entity = attr_entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(SharedString::from(t!("viewer.geometry_indices")))
                                .checked(show_indices)
                                .on_click(move |_, _window, cx| {
                                    entity
                                        .update(cx, |this, cx| {
                                            this.show_geometry_indices =
                                                !this.show_geometry_indices;
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        );
                        let entity = attr_entity.clone();
                        menu.item(
                            PopupMenuItem::new(SharedString::from(t!("viewer.geometry_groups")))
                                .checked(show_groups)
                                .on_click(move |_, _window, cx| {
                                    entity
                                        .update(cx, |this, cx| {
                                            this.show_geometry_groups = !this.show_geometry_groups;
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        )
                    }),
            )
            .child(
                // One menu rather than three controls: the display mode, the
                // colour map and the opacity are all "how do I want to look at
                // this field", and only the first of them is ever off.
                Button::new("viewer-field")
                    .xsmall()
                    .ghost()
                    .selected(field_display != field::FieldDisplay::Off)
                    .icon(Icon::new(RavelIcon::FieldOverlay))
                    .tooltip(t!("viewer.field"))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for mode in field::FieldDisplay::ALL {
                            let entity = field_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(mode.label_key())))
                                    .checked(mode == field_display)
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                if this.field_display != mode {
                                                    this.field_display = mode;
                                                    cx.notify();
                                                }
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu = menu.separator();
                        for map in field::FieldColorMap::ALL {
                            let entity = field_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(map.label_key())))
                                    .checked(map == field_map)
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                if this.field_map != map {
                                                    this.field_map = map;
                                                    cx.notify();
                                                }
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu = menu.separator();
                        for step in field::FIELD_OPACITY_STEPS {
                            let entity = field_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(format!(
                                    "{}: {:.0}%",
                                    t!("viewer.field_opacity"),
                                    step * 100.0
                                )))
                                .checked((step - field_opacity).abs() < f32::EPSILON)
                                .on_click(
                                    move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                if this.field_opacity != step {
                                                    this.field_opacity = step;
                                                    cx.notify();
                                                }
                                            })
                                            .ok();
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            )
    }
}

/// Render a screen-space overlay label as an element. GPUI shapes text
/// through elements, so labels take this path instead of the canvas painter.
///
/// `viewport` is the composition rectangle and resolution a composition-anchored
/// label is placed through — the same pair the pointer is resolved with. `None`
/// when there is no composition on screen, and such a label then has nowhere to
/// sit and is dropped.
fn overlay_label_element(
    label: overlay::OverlayLabel,
    viewport: Option<(viewport::Rect, (u32, u32))>,
) -> Option<Div> {
    let text = div()
        .text_xs()
        .text_color(label.color)
        .child(label.text.clone());
    Some(match label.placement {
        LabelPlacement::CanvasCenter => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(text),
        LabelPlacement::CanvasTopLeft => div().absolute().top_2().left_2().child(text),
        LabelPlacement::Comp(comp) => {
            let (rect, resolution) = viewport?;
            let (x, y) = comp_to_screen(comp, rect, resolution.0);
            div().absolute().left(px(x)).top(px(y)).child(text)
        }
    })
}

/// Overlay line color: light gray that stays readable over both the black
/// frame and bright content.
fn overlay_line_color() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.3)
}

const CHECKER_CELL_PX: f32 = 12.0;

fn checkerboard_tiles(
    width: f32,
    height: f32,
    visible: (f32, f32, f32, f32),
) -> Vec<(f32, f32, f32, f32, bool)> {
    let left = visible.0.clamp(0.0, width);
    let top = visible.1.clamp(0.0, height);
    let right = visible.2.clamp(left, width);
    let bottom = visible.3.clamp(top, height);
    let first_column = (left / CHECKER_CELL_PX).floor() as usize;
    let first_row = (top / CHECKER_CELL_PX).floor() as usize;
    let end_column = (right / CHECKER_CELL_PX).ceil() as usize;
    let end_row = (bottom / CHECKER_CELL_PX).ceil() as usize;
    let mut tiles = Vec::with_capacity(
        end_column
            .saturating_sub(first_column)
            .saturating_mul(end_row.saturating_sub(first_row)),
    );
    for row in first_row..end_row {
        for column in first_column..end_column {
            let x = column as f32 * CHECKER_CELL_PX;
            let y = row as f32 * CHECKER_CELL_PX;
            tiles.push((
                x,
                y,
                CHECKER_CELL_PX.min(width - x),
                CHECKER_CELL_PX.min(height - y),
                (row + column) % 2 == 0,
            ));
        }
    }
    tiles
}

fn paint_checkerboard(window: &mut Window, frame: Bounds<Pixels>, clip: Bounds<Pixels>) {
    let width: f32 = frame.size.width.into();
    let height: f32 = frame.size.height.into();
    let frame_x: f32 = frame.origin.x.into();
    let frame_y: f32 = frame.origin.y.into();
    let clip_x: f32 = clip.origin.x.into();
    let clip_y: f32 = clip.origin.y.into();
    let clip_width: f32 = clip.size.width.into();
    let clip_height: f32 = clip.size.height.into();
    let visible = (
        clip_x - frame_x,
        clip_y - frame_y,
        clip_x + clip_width - frame_x,
        clip_y + clip_height - frame_y,
    );
    let colors = [rgb(0x4a4a4a), rgb(0x707070)];
    for (x, y, width, height, light) in checkerboard_tiles(width, height, visible) {
        window.paint_quad(fill(
            Bounds {
                origin: point(frame.origin.x + px(x), frame.origin.y + px(y)),
                size: size(px(width), px(height)),
            },
            colors[usize::from(light)],
        ));
    }
}

/// Paint a worker-owned display texture through the fork's generic surface
/// path. The texture pointer is borrowed only for this call; `gpu_frame` stays
/// in the panel until the scene has consumed it.
#[cfg(target_os = "macos")]
fn paint_gpu_surface(
    frame: &GpuFrameBuffer,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    _cx: &gpui::App,
) -> bool {
    let Some(handles) = window.native_gpu_handles() else {
        return false;
    };
    // GPUI owns this callback through the Metal command buffer, so the pooled
    // texture cannot return to Ravel's pool until the surface has finished
    // sampling it.
    let completion = frame.completion_signal();
    ravel_gpu::interop::with_surface_texture(frame, handles.device(), |texture, width, height| {
        window.paint_surface(
            bounds,
            gpui::SurfaceSource::Texture {
                texture,
                size: size(
                    DevicePixels::from(width as i32),
                    DevicePixels::from(height as i32),
                ),
                completion: Some(completion),
            },
        );
    })
    .is_some()
}

/// The wgpu-backed platforms need no interop at all: GPUI's renderer runs on
/// the device Ravel was handed at startup, so the frame's own texture is
/// already the host's. The completion callback is retained by GPUI's wgpu
/// submission until the renderer has finished sampling the texture, keeping
/// the pooled frame lease alive across the surface draw.
///
/// **The release is late, never early.** GPUI hands the callback to
/// `wgpu::Queue::on_submitted_work_done`, and wgpu only runs such callbacks
/// during a later `submit` / `poll` — which for GPUI means the next frame it
/// draws. So a frame's lease returns to the pool one draw after the GPU
/// actually finished with it. That errs on the safe side of the race `ZC-4`
/// closed, and it cannot stall evaluation: `TexturePool::acquire` allocates
/// when nothing idle matches rather than waiting. The cost is at most one
/// extra pooled texture held while the window sits idle.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
fn paint_gpu_surface(
    frame: &GpuFrameBuffer,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &gpui::App,
) -> bool {
    // A lost device recovers on a later draw; sampling its textures meanwhile
    // is not safe, so this frame falls back instead. `None` means the backend
    // cannot say, which is not the same as "healthy" — treat the unknown as
    // lost and take the CPU road, the way the capability check does.
    if window.gpu_device_lost().unwrap_or(true) {
        return false;
    }
    // **The flag alone is not enough.** Recovery gives the renderer a brand new
    // device and clears the flag, and this frame's texture still belongs to the
    // dead one. Ask whether the renderer is on the device Ravel adopted rather
    // than whether it is unhappy right now.
    if !crate::workspace::host_device_unchanged(window, cx) {
        return false;
    }
    let texture = ravel_gpu::interop::surface_texture_wgpu(frame);
    let size = size(
        DevicePixels::from(frame.width() as i32),
        DevicePixels::from(frame.height() as i32),
    );
    window.paint_surface(bounds, texture, size, Some(frame.completion_signal()));
    true
}

/// Targets without a wgpu-backed GPUI renderer keep the CPU road.
///
/// Ravel enables GPUI's `wgpu` renderer on Windows, so Windows uses the
/// wgpu-backed implementation above and shares the adopted DX12 device. The
/// remaining targets have no compatible surface API and must fall back.
#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn paint_gpu_surface(
    _frame: &GpuFrameBuffer,
    _bounds: Bounds<Pixels>,
    _window: &mut Window,
    _cx: &gpui::App,
) -> bool {
    false
}

struct ViewerContent {
    image: Option<Arc<RenderImage>>,
    gpu_frame: Option<GpuFrameBuffer>,
    error: Option<SharedString>,
    composition_resolution: Option<(u32, u32)>,
}

/// Split a published [`ViewerFrame`] into durable panel content. Black is
/// painted as a quad, so Blank and Error do not allocate composition-sized
/// textures.
fn viewer_content(vf: ViewerFrame) -> ViewerContent {
    match vf {
        ViewerFrame::Frame {
            image,
            composition_resolution,
        } => ViewerContent {
            // Already BGRA and already wrapped — the conversion ran on the
            // evaluation worker (HIGH-08).
            image: Some(image.into_image()),
            gpu_frame: None,
            error: None,
            composition_resolution: Some(composition_resolution),
        },
        ViewerFrame::GpuFrame {
            frame,
            composition_resolution,
        } => ViewerContent {
            image: None,
            gpu_frame: Some(frame),
            error: None,
            composition_resolution: Some(composition_resolution),
        },
        ViewerFrame::Blank {
            composition_resolution,
        } => ViewerContent {
            image: None,
            gpu_frame: None,
            error: None,
            composition_resolution,
        },
        ViewerFrame::Error {
            message,
            composition_resolution,
        } => ViewerContent {
            image: None,
            gpu_frame: None,
            error: Some(message),
            composition_resolution,
        },
    }
}

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
        let gpu_frame = self.gpu_frame.clone();
        let viewport_origin = self.viewport_origin.clone();
        let viewport_size = self.viewport_size.clone();
        let background_mode = self.background_mode;
        let pointer_cursor = self.pointer_hint.cursor();
        let active_drag_cursor = viewer_drag_cursor(
            self.pan_drag.is_some(),
            self.move_drag.is_some(),
            self.shape_drag.is_some(),
            self.pen_session
                .as_ref()
                .is_some_and(|session| session.active_point.is_some()),
            self.handle_drag
                .as_ref()
                .and_then(|drag| drag.handle.id.path_handle_kind()),
            self.handle_drag
                .as_ref()
                .filter(|drag| drag.handle.id.shell().is_some())
                .map(|drag| drag.handle.hint),
        );
        let composition_background = (|| {
            let project = cx.try_global::<ProjectStateHandle>()?.0.upgrade()?;
            let color = project.read(cx).active_composition(cx)?.background_color;
            Some(Hsla::from(gpui::Rgba {
                r: color.r,
                g: color.g,
                b: color.b,
                a: color.a,
            }))
        })()
        .unwrap_or_else(|| rgb(0x000000).into());

        // One snapshot feeds paint, labels and hit-testing, so an overlay can
        // never see a different world than the pointer does.
        let overlay_context = self.overlay_context(cx);
        let overlays = OverlayRegistry::builtin();
        let labels = overlays.labels(&overlay_context);

        let content = div().relative().size_full().overflow_hidden().child(
            canvas(
                move |bounds: Bounds<Pixels>, _window, _cx| {
                    viewport_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                    viewport_size.set((bounds.size.width.into(), bounds.size.height.into()));
                },
                move |bounds: Bounds<Pixels>, _, window, cx| {
                    let Some(resolution) = composition_resolution else {
                        return;
                    };
                    let panel_size = (bounds.size.width.into(), bounds.size.height.into());
                    let rect = viewport.rect(panel_size, resolution);
                    let frame_bounds = Bounds {
                        origin: point(bounds.origin.x + px(rect.x), bounds.origin.y + px(rect.y)),
                        size: size(px(rect.width), px(rect.height)),
                    };
                    match background_mode {
                        ViewerBackgroundMode::Composition => {
                            window.paint_quad(fill(frame_bounds, composition_background));
                        }
                        ViewerBackgroundMode::Checkerboard => {
                            paint_checkerboard(window, frame_bounds, bounds);
                        }
                        ViewerBackgroundMode::Solid => {
                            window.paint_quad(fill(frame_bounds, rgb(0x000000)));
                        }
                    }
                    if let Some(image) = image.clone()
                        && let Err(err) =
                            window.paint_image(frame_bounds, Corners::default(), image, 0, false)
                    {
                        tracing::error!(%err, "failed to paint viewer image");
                    }
                    // Ask the renderer that lent Ravel its device whether that
                    // device died — on **every** paint, not only when a GPU
                    // frame is in hand. A dead device stops producing frames,
                    // and the update that blanks the viewer takes the last one
                    // with it; hanging the announcement off a frame still being
                    // around is how a loss goes unreported for the rest of the
                    // session.
                    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
                    let host_device_loss = crate::workspace::host_device_loss_detected(window, cx);
                    #[cfg(not(any(
                        target_os = "linux",
                        target_os = "freebsd",
                        target_os = "windows"
                    )))]
                    let host_device_loss = false;
                    let mut surface_lost = false;
                    let mut self_owned_device_loss = false;
                    if let Some(frame) = gpu_frame.as_ref() {
                        self_owned_device_loss = frame.device_state().lost();
                        if !paint_gpu_surface(frame, frame_bounds, window, cx) {
                            // This window cannot sample the worker's texture — a
                            // second window on another device, or a device that
                            // changed under us. Painting nothing would leave the
                            // viewer blank for good, so turn the path off and ask
                            // for a CPU frame; the next update repaints normally.
                            surface_lost = true;
                            tracing::warn!(
                                "viewer GPU surface unavailable; falling back to the CPU frame"
                            );
                        }
                        if self_owned_device_loss {
                            tracing::warn!("viewer GPU device loss detected");
                        }
                    }
                    // A self-owned context reports its loss through the shared
                    // state; the adopted host reports it through the renderer's
                    // own flag. Both observations leave the paint guard a pure
                    // capability check and reach the session through the
                    // existing deferred update — this unit announces the loss,
                    // it does not rebuild anything.
                    if (surface_lost || self_owned_device_loss || host_device_loss)
                        && let Some(project) = cx
                            .try_global::<ProjectStateHandle>()
                            .and_then(|handle| handle.0.upgrade())
                    {
                        cx.defer(move |cx| {
                            project.update(cx, |project, cx| {
                                project.report_gpu_device_loss(host_device_loss, cx);
                                if surface_lost {
                                    project.configure_viewer_surface(false, cx);
                                }
                            });
                        });
                    }
                    let mut painter = OverlayPainter::new(frame_bounds, resolution);
                    overlays.paint(&overlay_context, &mut painter);
                    overlay::paint_primitives(&painter.finish(), window);
                    if let Some(cursor) = active_drag_cursor {
                        window.set_window_cursor_style(cursor);
                    }
                },
            )
            .size_full(),
        );

        // Composition-anchored labels go through the viewport the pointer is
        // resolved with, so an index label sits on the mark it names.
        let label_viewport = self.composition_resolution.map(|resolution| {
            (
                self.viewport.rect(self.viewport_size.get(), resolution),
                resolution,
            )
        });
        let content = if !labels.is_empty() {
            content.children(
                labels
                    .into_iter()
                    .filter_map(|label| overlay_label_element(label, label_viewport)),
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
        let drop_highlight = cx.theme().colors.drop_target;
        let content = div()
            .id("viewer-canvas-area")
            .flex_1()
            .min_h_0()
            .cursor(pointer_cursor)
            // A MediaBin asset dropped on the picture becomes a layer at the
            // playhead: the Viewer shows one instant, so that instant is the
            // only frame a drop here can mean (unit 10).
            .drag_over::<DraggedAsset>(move |style, _drag, _window, _cx| style.bg(drop_highlight))
            .on_drop(cx.listener(|_this, drag: &DraggedAsset, _window, cx| {
                let assets = dropped_asset_ids(drag, cx);
                add_assets_as_layers(&assets, ProjectState::playhead_frame(cx), cx);
                cx.notify();
            }))
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
                    if !this.overlay_handle_mouse_down(event, cx) {
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
                    this.handle_drag_ended(cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                match event.pressed_button {
                    Some(MouseButton::Middle) => {
                        this.cancel_move(cx);
                        this.cancel_shape(cx);
                        this.cancel_handle_drag(cx);
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
                        if this.handle_drag.is_some() {
                            this.handle_dragged(
                                event.position,
                                drag_modifiers(&event.modifiers),
                                cx,
                            );
                        } else if this
                            .pen_session
                            .as_ref()
                            .is_some_and(|s| s.active_point.is_some())
                        {
                            this.pen_dragged(event.position, cx);
                        } else if this.shape_drag.is_some() {
                            this.shape_dragged(event, cx);
                        } else {
                            this.move_dragged(event.position, drag_modifiers(&event.modifiers), cx);
                        }
                    }
                    _ => {
                        this.pan_drag = None;
                        this.pen_point_ended(cx);
                        this.cancel_move(cx);
                        this.cancel_shape(cx);
                        this.cancel_handle_drag(cx);
                        let Some(next) = this.pointer_hint_at(event.position, cx) else {
                            return;
                        };
                        if let Some(next) = viewer_pointer_hint_transition(
                            this.pointer_hint,
                            next,
                            this.pan_drag.is_some()
                                || this.move_drag.is_some()
                                || this.shape_drag.is_some()
                                || this.handle_drag.is_some(),
                        ) {
                            this.pointer_hint = next;
                            cx.notify();
                        }
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
                } else if event.keystroke.key.as_str() == "escape" && this.handle_drag.is_some() {
                    this.cancel_handle_drag(cx);
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

// ---------------------------------------------------------------------------
// Selection bbox overlay (REQ-UI-011 unit 3)
// ---------------------------------------------------------------------------

use ravel_core::composition::Document;
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Graph, Node, ParameterValue, PathPoint};
use ravel_core::types::{FrameRate, Vec2};

#[cfg(test)]
fn sample_float_param(node: &Node, key: &str, frame: u64, ctx: &EvalContext) -> Option<f32> {
    let param = node.parameters.iter().find(|p| p.key == key)?;
    match &param.value {
        ParameterValue::Float(v) => Some(*v),
        ParameterValue::Channel(ch) => Some(ch.evaluate(frame as f64, ctx)),
        _ => None,
    }
}

/// Sample the first two components of a vector parameter (`Channel2` or
/// `Channel3`). Geometric vectors are one parameter, so the canvas gestures
/// read a pair from a single key instead of two `_x` / `_y` Floats.
fn sample_vec2_param(node: &Node, key: &str, frame: u64, ctx: &EvalContext) -> Option<(f32, f32)> {
    let param = node.parameters.iter().find(|p| p.key == key)?;
    let sample =
        |ch: &ravel_core::animation::channel::AnimationChannel| ch.evaluate(frame as f64, ctx);
    match &param.value {
        ParameterValue::Channel2(chs) => Some((sample(&chs[0]), sample(&chs[1]))),
        ParameterValue::Channel3(chs) => Some((sample(&chs[0]), sample(&chs[1]))),
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
pub struct CompRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

struct PathOverlay {
    /// Control points in composition space (the layer shell applied).
    points: Vec<PathPoint>,
    closed: bool,
    /// The shell is the identity, so composition space equals node-local
    /// space and a handle drag translates 1:1 into the `points` parameter.
    shell_identity: bool,
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

/// The panel-local pixel position of a composition point: the inverse of
/// [`screen_to_comp`], and where a composition-anchored overlay label sits.
fn comp_to_screen(comp: (f32, f32), rect: viewport::Rect, comp_width: u32) -> (f32, f32) {
    let zoom = rect.width / comp_width as f32;
    (rect.x + comp.0 * zoom, rect.y + comp.1 * zoom)
}

/// A single composition point as a zero-sized rectangle, which is what a
/// gesture that moves one point (a drawing pointer, a shell grip) hands to
/// [`snap::snap_delta`].
/// The drag modifiers of a pointer event.
///
/// Snapping is suppressed by the platform's primary modifier — Cmd on macOS,
/// Ctrl elsewhere — spelled the way the Node Editor and the Timeline already
/// spell it. Shift and Alt keep the meanings the drawing and shell gestures
/// gave them.
fn drag_modifiers(modifiers: &Modifiers) -> DragModifiers {
    DragModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        primary: modifiers.platform || modifiers.control,
    }
}

fn point_rect(point: (f32, f32)) -> CompRect {
    CompRect {
        x: point.0,
        y: point.1,
        w: 0.0,
        h: 0.0,
    }
}

/// What a shell handle drag moves, for snapping: the rectangle to align, and
/// the axes the grip's edit actually writes.
///
/// The move grip carries the whole layer, so its bbox is what aligns; the
/// scale grips and the anchor marker each move one point, so that point is.
/// Rotation is excluded: an angle has no edge to line up, and pulling the
/// grabbed corner onto a guide would silently quantise the sweep. Handles that
/// are not the layer shell's snap nothing.
fn snap_target_for_handle(
    handle: &OverlayHandle,
    layer_rect: Option<CompRect>,
    modifiers: DragModifiers,
) -> Option<(CompRect, (bool, bool))> {
    let shell = handle.id.shell()?;
    let rect = match shell {
        ShellHandle::Position => layer_rect?,
        ShellHandle::Rotate(_) => return None,
        // Shift scales uniformly: `scale_edits` takes the larger movement and
        // applies it to both axes, which overwrites whatever a snapped axis
        // had landed on. The guide would name a line the grip then misses, so
        // the constrained gesture snaps nothing at all.
        ShellHandle::Scale(_) if modifiers.shift => return None,
        ShellHandle::Scale(_) | ShellHandle::Anchor => point_rect(handle.position),
    };
    Some((rect, shell.driven_axes()))
}

fn rect_contains(rect: &CompRect, point: (f32, f32)) -> bool {
    point.0 >= rect.x
        && point.0 <= rect.x + rect.w
        && point.1 >= rect.y
        && point.1 <= rect.y + rect.h
}

fn selected_body_pointer_hint(
    selected_rects: &[CompRect],
    pointer: (f32, f32),
) -> Option<ViewerPointerHint> {
    selected_rects
        .iter()
        .any(|rect| rect_contains(rect, pointer))
        .then_some(ViewerPointerHint::MovableBody)
}

/// The topmost node of `network` whose evaluated geometry contains `point`.
///
/// Driven by the same evaluated bounds the bbox is drawn from, so what the
/// pointer picks is what the outline promised — a shape node this crate has
/// never heard of is selectable, and a transformed one is picked where it
/// appears rather than where its parameters say.
fn hit_test_shape_nodes(
    ctx: &OverlayContext,
    network: &NetworkPath,
    point: (f32, f32),
) -> Option<NodeId> {
    let document = ctx.document.as_ref()?;
    let graph = ravel_ui::document::resolve_network(document, network)?;
    let shell = layer_shell(ctx, document, network.comp, network.layer)?;
    let mut candidates: Vec<_> = graph.nodes().collect();
    candidates.sort_by_key(|node| std::cmp::Reverse(node.metadata.z));
    candidates.into_iter().find_map(|node| {
        let bounds = geometry::evaluated_bounds(ctx, network, node.id)?;
        rect_contains(&to_comp_space(&shell, bounds), point).then_some(node.id)
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
    let parameter = updated
        .parameters
        .iter_mut()
        .find(|param| param.key == "center")?;
    parameter.value = edited_vector_param(
        &parameter.value,
        &[origin.0 + delta.0, origin.1 + delta.1],
        Some(local_frame),
    )?;
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

fn pen_close_pointer_hint(
    points: &[PathPoint],
    pointer: (f32, f32),
    radius: f32,
) -> Option<ViewerPointerHint> {
    pen_should_close(points, pointer, radius).then_some(ViewerPointerHint::PenClose)
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
    let mut values = vec![("center", ParameterValue::vec2(geo.center.0, geo.center.1))];
    match kind {
        ShapeDrawKind::Rect => {
            values.push(("width", ParameterValue::Float(geo.half.0 * 2.0)));
            values.push(("height", ParameterValue::Float(geo.half.1 * 2.0)));
        }
        ShapeDrawKind::Ellipse => {
            values.push(("radius", ParameterValue::vec2(geo.half.0, geo.half.1)));
        }
    }
    for (key, value) in values {
        if let Some(param) = node.parameters.iter_mut().find(|p| p.key == key) {
            param.value = value;
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

/// The compositing chain transform of `layer`, at the context's frame.
fn layer_shell(
    ctx: &OverlayContext,
    document: &Document,
    comp: CompId,
    layer: LayerId,
) -> Option<Affine> {
    let (resolution, playback) = (ctx.resolution?, ctx.playback?);
    let composition = document.get_composition(comp)?;
    let layer = composition.get_layer(layer)?;
    let eval = EvalContext::new(playback.frame, playback.fps, resolution);
    Some(world_matrix(composition, layer, &eval))
}

fn to_comp_space(shell: &Affine, rect: CompRect) -> CompRect {
    if shell.is_identity() {
        rect
    } else {
        transform_rect(&rect, shell)
    }
}

/// Comp-space bounds of one node's **evaluated** geometry: the extent the
/// evaluator produced, put through the owning layer's compositing chain.
///
/// `None` while the result has not arrived, when the node produced something
/// that is not a geometry, and when the geometry places nothing. Deliberately
/// not backed by a parameter reading: mixing the two would make a bbox jump
/// from an estimate to the truth on the frame the evaluation lands.
fn node_comp_rect(ctx: &OverlayContext, network: &NetworkPath, node: NodeId) -> Option<CompRect> {
    let bounds = geometry::evaluated_bounds(ctx, network, node)?;
    let document = ctx.document.as_ref()?;
    Some(to_comp_space(
        &layer_shell(ctx, document, network.comp, network.layer)?,
        bounds,
    ))
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

/// The geometry nodes of a network whose result nothing else in that network
/// consumes as geometry — what the layer actually draws.
///
/// The last node of a chain, not every node in it: `shape.rect →
/// geometry.transform → rasterize` places one shape, and unioning both the
/// pre- and post-transform extents would outline a rectangle the layer never
/// shows. `rasterize` consumes the geometry but produces a frame, so the
/// transform stays terminal; `geometry.transform` produces geometry, so the
/// rect it consumes drops out.
fn terminal_geometry_nodes(graph: &Graph) -> Vec<NodeId> {
    let geometry = ravel_core::id::DataTypeId::GEOMETRY;
    let produces_geometry = |id: NodeId| {
        graph
            .node(id)
            .is_some_and(|node| node.outputs.iter().any(|port| port.data_type == geometry))
    };
    // Which port an edge leaves by, not merely which node. A multi-output node
    // whose *other* port feeds a geometry operator — a scalar driving a
    // `geometry.transform` parameter, say — still places its own geometry, and
    // ignoring the port index dropped it from the layer's bbox entirely.
    //
    // A port index the node does not declare reads as no port at all, the same
    // boundary `PortRecord::extract` draws: an edge left over from an earlier
    // interface must not resolve to a neighbouring port's type.
    let leaves_a_geometry_port = |edge: &ravel_core::graph::Edge| {
        graph
            .node(edge.source)
            .and_then(|node| node.outputs.get(edge.source_port.0 as usize))
            .is_some_and(|port| port.data_type == geometry)
    };
    graph
        .nodes()
        .filter(|node| produces_geometry(node.id))
        .filter(|node| {
            !graph.edges().any(|edge| {
                edge.source == node.id
                    && leaves_a_geometry_port(edge)
                    && produces_geometry(edge.target)
            })
        })
        .map(|node| node.id)
        .collect()
}

/// [`terminal_geometry_nodes`] of a network named by path.
fn terminal_geometry_nodes_of(ctx: &OverlayContext, network: &NetworkPath) -> Vec<NodeId> {
    ctx.document
        .as_ref()
        .and_then(|document| ravel_ui::document::resolve_network(document, network))
        .map(terminal_geometry_nodes)
        .unwrap_or_default()
}

/// The nodes of a layer's own network that a layer-level move drags: those
/// with an evaluated geometry. Nested subnets are not descended into — their
/// parameters are not addressable from the layer network, so a drag could not
/// write them.
fn layer_geometry_nodes(ctx: &OverlayContext, network: &NetworkPath) -> Vec<NodeId> {
    let Some(document) = ctx.document.as_ref() else {
        return Vec::new();
    };
    let Some(graph) = ravel_ui::document::resolve_network(document, network) else {
        return Vec::new();
    };
    graph
        .nodes()
        .map(|node| node.id)
        .filter(|id| geometry::evaluated_bounds(ctx, network, *id).is_some())
        .collect()
}

/// Comp-space bounds of a whole layer: the union of the extents its terminal
/// geometry nodes evaluated to, put through the layer's compositing chain
/// transform (REQ-UI-013 multi-selection).
///
/// `None` until the evaluation lands, and `None` for a layer that places no
/// geometry at all — a media or effects-only network has nothing to measure,
/// so it gets no bbox rather than a guessed one.
fn layer_comp_rect(
    ctx: &OverlayContext,
    document: &Document,
    comp: CompId,
    layer: LayerId,
) -> Option<CompRect> {
    let network = NetworkPath::layer(comp, layer);
    let graph = ravel_ui::document::resolve_network(document, &network)?;
    let bounds = terminal_geometry_nodes(graph)
        .into_iter()
        .filter_map(|node| geometry::evaluated_bounds(ctx, &network, node))
        .reduce(union_rect)?;
    Some(to_comp_space(
        &layer_shell(ctx, document, comp, layer)?,
        bounds,
    ))
}

/// One bbox per selected layer that has measurable geometry, in selection order.
fn layer_selection_comp_rects(ctx: &OverlayContext) -> Vec<CompRect> {
    let (Some(comp), Some(document)) = (ctx.layer_selection.comp(), ctx.document.as_ref()) else {
        return Vec::new();
    };
    ctx.layer_selection
        .layers()
        .iter()
        .filter_map(|layer| layer_comp_rect(ctx, document, comp, *layer))
        .collect()
}

fn selection_comp_rects(ctx: &OverlayContext) -> Vec<CompRect> {
    let Some(selection) = ctx.selection.as_ref() else {
        return Vec::new();
    };
    let Some(network) = selection.path.as_ref() else {
        return Vec::new();
    };
    let mut ids: Vec<_> = selection.nodes.iter().copied().collect();
    // The set has no order of its own and the rects are compared positionally
    // in tests and unioned in production; sorting keeps both deterministic.
    ids.sort_by_key(|id| id.raw());
    ids.into_iter()
        .filter_map(|id| node_comp_rect(ctx, network, id))
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
        shell_identity: shell.is_identity(),
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

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one for these plain unit tests.
    use core::prelude::v1::test;
    use ravel_core::composition::{Composition, Layer};
    use std::collections::HashMap;

    #[test]
    fn checkerboard_cells_stay_screen_space_sized_across_zoomed_frames() {
        for (width, height) in [(320.0, 180.0), (1280.0, 720.0)] {
            let tiles = checkerboard_tiles(width, height, (0.0, 0.0, width, height));
            assert!(tiles.iter().any(|(_, _, w, h, _)| {
                (*w - CHECKER_CELL_PX).abs() < f32::EPSILON
                    && (*h - CHECKER_CELL_PX).abs() < f32::EPSILON
            }));
            assert!(tiles.iter().all(|(_, _, w, h, _)| {
                *w > 0.0 && *w <= CHECKER_CELL_PX && *h > 0.0 && *h <= CHECKER_CELL_PX
            }));
        }

        let visible = checkerboard_tiles(
            1920.0 * 32.0,
            1080.0 * 32.0,
            (30_000.0, 17_000.0, 31_000.0, 17_800.0),
        );
        assert!(
            visible.len() < 6_000,
            "painting work is bounded by the visible panel, not zoomed frame area"
        );
    }

    fn shape_node(type_key: &str, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(ravel_core::id::NodeId::next(), type_key);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        node
    }

    /// `(x, y)` as the folded vector parameter `key` holds it.
    fn v2(key: &str, x: f32, y: f32) -> (&str, ParameterValue) {
        (key, ParameterValue::vec2(x, y))
    }

    /// A scalar parameter, as a `shape_node` entry.
    fn f(key: &str, value: f32) -> (&str, ParameterValue) {
        (key, ParameterValue::Float(value))
    }

    fn eval_ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    // ---- evaluated geometry -------------------------------------------

    /// A processor for a `type_key` this crate has never heard of. Emits a
    /// square of side `2 * half` centred on `center`, so the bbox it produces
    /// is knowable without the Viewer knowing anything about the node.
    struct UnknownShape {
        center: (f32, f32),
        half: f32,
    }

    impl ravel_core::eval::NodeProcessor for UnknownShape {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
            _params: &ravel_core::eval::ResolvedParams,
            _scope: &mut dyn ravel_core::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
            let (x, y, h) = (self.center.0, self.center.1, self.half);
            Ok(Arc::new(ravel_core::geometry::Geometry::from_points(vec![
                ravel_core::types::Vec2(x - h, y - h),
                ravel_core::types::Vec2(x + h, y - h),
                ravel_core::types::Vec2(x + h, y + h),
                ravel_core::types::Vec2(x - h, y + h),
            ])))
        }
    }

    /// Register the CPU processors these tests evaluate with.
    ///
    /// Explicit rather than `ravel_nodes::register_all_processors`, which
    /// wants a GPU context: everything the geometry overlay reads is produced
    /// on the CPU, so a headless test can drive the real processors.
    fn register_geometry_processors(evaluator: &mut ravel_core::eval::Evaluator, graph: &Graph) {
        for node in graph.nodes() {
            let processor: Arc<dyn ravel_core::eval::NodeProcessor> = match node.type_key.as_str() {
                "shape.rect" => Arc::new(ravel_nodes::shape::RectProcessor::from_node(node)),
                "shape.ellipse" => Arc::new(ravel_nodes::shape::EllipseProcessor::from_node(node)),
                "geometry.transform" => {
                    Arc::new(ravel_nodes::geometry::GeometryTransformProcessor::from_node(node))
                }
                "scatter.grid" => Arc::new(ravel_nodes::scatter::GridProcessor::from_node(node)),
                "shape.custom_path" => {
                    Arc::new(ravel_nodes::shape::CustomPathProcessor::from_node(node))
                }
                "test.unknown_shape" => Arc::new(UnknownShape {
                    center: sample_vec2_param(node, "center", 0, &eval_ctx()).unwrap_or((0.0, 0.0)),
                    half: sample_float_param(node, "half", 0, &eval_ctx()).unwrap_or(1.0),
                }),
                _ => continue,
            };
            evaluator.register(node.id, processor);
        }
    }

    /// Evaluate every node of `graph` in `network`'s scope and index the
    /// results the way the request → publish path does.
    fn evaluated_results(graph: &Graph, network: &NetworkPath) -> overlay::OverlayResults {
        let mut evaluator = ravel_core::eval::Evaluator::new();
        register_geometry_processors(&mut evaluator, graph);
        let ctx = eval_ctx();
        let path = network.segments();
        let mut values = HashMap::new();
        for node in graph.nodes() {
            if let Ok(value) = evaluator.evaluate_at(&path, graph, node.id, &ctx) {
                values.insert((path.clone(), node.id), value);
            }
        }
        overlay::OverlayResults::new(values)
    }

    /// Publish the overlay snapshot a real evaluation would have produced for
    /// every layer network of the project.
    ///
    /// The panel tests run with the background worker disabled, so nothing
    /// else fills it — and since unit 3 the bbox, the click test and the layer
    /// drag all read it rather than the node parameters.
    fn publish_geometry_results(project: &Entity<ProjectState>, cx: &mut TestAppContext) {
        let document = project.read_with(cx, |project, _| project.document().clone());
        let mut values = HashMap::new();
        for comp in document.compositions.values() {
            for layer in &comp.layers {
                let network = NetworkPath::layer(comp.id, layer.id);
                values.extend(evaluated_results(&layer.network, &network).values);
            }
        }
        cx.update(|cx| cx.set_global(overlay::OverlayResults::new(values)));
    }

    /// A one-layer document holding `network`, plus the overlay context an
    /// overlay would see with `selected` selected inside it.
    fn geometry_context(network: Graph, selected: &[NodeId]) -> (OverlayContext, NetworkPath) {
        use ravel_core::id::LayerId;
        let layer = Layer::new(LayerId::next(), "shapes", network.clone()).with_time(0, 0, 300);
        let comp = comp_with_layers(vec![layer.clone()]);
        let path = NetworkPath::layer(comp.id, layer.id);
        let document = Document::default().with_composition(comp);
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            document: Some(document),
            selection: Some(CanvasSelection {
                path: Some(path.clone()),
                nodes: selected.iter().copied().collect(),
            }),
            show_geometry_bounds: true,
            results: evaluated_results(&network, &path),
            ..OverlayContext::default()
        };
        (ctx, path)
    }

    /// Completion criterion: a shape node the Viewer knows nothing about
    /// still gets a bbox, because the bbox comes from the value the evaluator
    /// produced rather than from a `type_key` table.
    #[test]
    fn a_node_type_the_viewer_does_not_know_still_gets_a_bbox() {
        let node = shape_node(
            "test.unknown_shape",
            &[v2("center", 100.0, 50.0), f("half", 40.0)],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let (ctx, network) = geometry_context(graph, &[id]);

        let rect = node_comp_rect(&ctx, &network, id).expect("bbox from the evaluated geometry");
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (60.0, 10.0, 80.0, 80.0));
    }

    /// Completion criterion: a shape put through `geometry.transform`
    /// outlines where the transform put it, not where its own parameters say.
    #[test]
    fn a_transformed_shape_outlines_after_the_transform() {
        use ravel_core::id::{EdgeId, InputPortIndex, OutputPortIndex};

        let rect = shape_node(
            "shape.rect",
            &[
                v2("center", 0.0, 0.0),
                f("width", 100.0),
                f("height", 100.0),
            ],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let rect_id = rect.id;
        let transform = shape_node(
            "geometry.transform",
            &[("translate", ParameterValue::vec3(200.0, 30.0, 0.0))],
        )
        .with_input("geometry", &[ravel_core::id::DataTypeId::GEOMETRY])
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let transform_id = transform.id;
        let graph = Graph::new()
            .add_node(rect)
            .unwrap()
            .add_node(transform)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                rect_id,
                OutputPortIndex(0),
                transform_id,
                InputPortIndex(0),
            )
            .unwrap();
        let (ctx, network) = geometry_context(graph, &[transform_id]);

        let source = node_comp_rect(&ctx, &network, rect_id).expect("the rect evaluated");
        let moved = node_comp_rect(&ctx, &network, transform_id).expect("the transform evaluated");
        assert_eq!((source.x, source.y), (-50.0, -50.0));
        assert_eq!(
            (moved.x, moved.y),
            (150.0, -20.0),
            "the bbox did not follow the transform"
        );
        assert_eq!((moved.w, moved.h), (source.w, source.h));
    }

    /// Completion criterion: every instance a `scatter.*` places is drawn as a
    /// point, and the bbox spans all of them — the instance domain is where a
    /// scatter's copies live, and a points-only reading would miss them.
    #[test]
    fn every_scatter_instance_is_drawn_as_a_point() {
        use ravel_core::id::{EdgeId, InputPortIndex, OutputPortIndex};

        let source = shape_node(
            "shape.rect",
            &[v2("center", 0.0, 0.0), f("width", 10.0), f("height", 10.0)],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let source_id = source.id;
        let scatter = shape_node(
            "scatter.grid",
            &[
                ("count_x", ParameterValue::Int(3)),
                ("count_y", ParameterValue::Int(2)),
                v2("spacing", 100.0, 50.0),
                v2("center", 0.0, 0.0),
                ("center_input", ParameterValue::Bool(true)),
            ],
        )
        .with_input("geometry", &[ravel_core::id::DataTypeId::GEOMETRY])
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let scatter_id = scatter.id;
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(scatter)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                scatter_id,
                InputPortIndex(0),
            )
            .unwrap();
        let (ctx, network) = geometry_context(graph, &[scatter_id]);

        let value =
            geometry::evaluated_geometry(&ctx, &network, scatter_id).expect("scatter evaluated");
        let scattered = geometry::as_geometry(&value).expect("a geometry");
        assert_eq!(
            scattered.instance_count(),
            6,
            "the grid scatter placed a different number of instances than this test assumes"
        );
        let points: Vec<_> = geometry::geometry_marks(scattered)
            .into_iter()
            .map(|mark| mark.position)
            .collect();
        for instance in 0..scattered.instance_count() {
            let position = scattered
                .positions(ravel_core::geometry::Domain::Instance)
                .and_then(|p| p.ok())
                .and_then(|p| p.get3(instance))
                .expect("every instance carries P");
            assert!(
                points
                    .iter()
                    .any(|p| (p.0 - position.0).abs() < 1e-4 && (p.1 - position.1).abs() < 1e-4),
                "instance {instance} at {position:?} was not drawn"
            );
        }
        let rect =
            node_comp_rect(&ctx, &network, scatter_id).expect("bbox spans the placed instances");
        assert!(
            rect.w >= 200.0 && rect.h >= 50.0,
            "the bbox missed the instance domain: {rect:?}"
        );
    }

    /// The migration rule: no result, no drawing. A guessed rectangle that
    /// jumps to the truth a frame later is worse than one that arrives late.
    #[test]
    fn nothing_is_outlined_before_the_result_arrives() {
        let node = shape_node(
            "shape.rect",
            &[v2("center", 0.0, 0.0), f("width", 10.0), f("height", 10.0)],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let (mut ctx, network) = geometry_context(graph, &[id]);
        assert!(node_comp_rect(&ctx, &network, id).is_some());

        ctx.results = overlay::OverlayResults::default();
        assert_eq!(node_comp_rect(&ctx, &network, id), None);
        assert!(selection_comp_rects(&ctx).is_empty());
        assert_eq!(hit_test_shape_nodes(&ctx, &network, (0.0, 0.0)), None);
    }

    /// A shape node the tests can build without knowing a processor: a square
    /// of side `size` at `center`, declaring a geometry output.
    fn square_node(center: (f32, f32), size: f32) -> Node {
        shape_node(
            "shape.rect",
            &[
                v2("center", center.0, center.1),
                f("width", size),
                f("height", size),
            ],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY)
    }

    #[test]
    fn hit_test_uses_frontmost_shape_and_reports_misses() {
        let mut back = square_node((50.0, 50.0), 40.0);
        back.metadata.z = 2;
        let back_id = back.id;
        let mut front = square_node((50.0, 50.0), 40.0);
        front.metadata.z = 8;
        let front_id = front.id;
        let graph = Graph::new()
            .add_node(back)
            .unwrap()
            .add_node(front)
            .unwrap();
        let (ctx, network) = geometry_context(graph, &[]);

        assert_eq!(
            hit_test_shape_nodes(&ctx, &network, (50.0, 50.0)),
            Some(front_id)
        );
        assert_eq!(hit_test_shape_nodes(&ctx, &network, (200.0, 200.0)), None);
        assert_ne!(front_id, back_id);
    }

    /// The click test reads the same evaluated bounds the outline is drawn
    /// from, put through the same shell — so what the pointer picks is what
    /// the outline promised.
    #[test]
    fn hit_test_applies_shell_transform() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::id::LayerId;

        let node = square_node((20.0, 20.0), 20.0);
        let id = node.id;
        let network = Graph::new().add_node(node).unwrap();
        let mut layer = Layer::new(LayerId::next(), "shapes", network.clone()).with_time(0, 0, 300);
        layer.transform.position = [
            AnimationChannel::constant(100.0),
            AnimationChannel::constant(50.0),
        ];
        let comp = comp_with_layers(vec![layer.clone()]);
        let path = NetworkPath::layer(comp.id, layer.id);
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            results: evaluated_results(&network, &path),
            document: Some(Document::default().with_composition(comp)),
            ..OverlayContext::default()
        };

        assert_eq!(hit_test_shape_nodes(&ctx, &path, (120.0, 70.0)), Some(id));
        assert_eq!(hit_test_shape_nodes(&ctx, &path, (20.0, 20.0)), None);
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
                v2("center", 10.0, 20.0),
                f("width", 40.0),
                f("height", 30.0),
            ],
        );
        let moved = moved_shape_node(&node, (10.0, 20.0), None, (4.5, -2.0), 7).unwrap();
        assert_eq!(
            sample_vec2_param(&moved, "center", 7, &eval_ctx()),
            Some((14.5, 18.0))
        );
    }

    #[test]
    fn zero_delta_restores_the_origin() {
        let node = shape_node(
            "shape.rect",
            &[
                v2("center", 10.0, 20.0),
                f("width", 40.0),
                f("height", 30.0),
            ],
        );
        let moved = moved_shape_node(&node, (10.0, 20.0), None, (0.0, 0.0), 0).unwrap();
        assert_eq!(
            sample_vec2_param(&moved, "center", 0, &eval_ctx()),
            Some((10.0, 20.0))
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
            .add_node(square_node((0.0, 0.0), 20.0))
            .unwrap();
        let child = Layer::new(LayerId::next(), "child", network.clone())
            .with_time(0, 0, 300)
            .with_parent(parent.id);

        for muted in [false, true] {
            parent.muted = muted;
            let comp = comp_with_layers(vec![parent.clone(), child.clone()]);
            let comp_id = comp.id;
            let path = NetworkPath::layer(comp_id, child.id);
            let m = world_matrix(&comp, &child, &eval_ctx());
            assert_eq!(
                (m.0[2], m.0[5]),
                (100.0, 50.0),
                "the parent transform applies regardless of mute (muted = {muted})"
            );
            let document = Document::default().with_composition(comp);
            let ctx = OverlayContext {
                resolution: Some((1920, 1080)),
                playback: Some(super::super::PlaybackPosition {
                    frame: 0,
                    fps: FrameRate::new(30, 1),
                }),
                results: evaluated_results(&network, &path),
                document: Some(document.clone()),
                ..OverlayContext::default()
            };
            let rect = layer_comp_rect(&ctx, &document, comp_id, child.id).unwrap();
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

    /// A layer's bbox is the union of the extents its **terminal** geometry
    /// nodes evaluated to, put through the layer's shell transform (REQ-UI-013
    /// multi-selection).
    #[test]
    fn layer_bbox_unions_terminal_geometry_and_follows_the_shell() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::id::LayerId;

        let left = square_node((0.0, 0.0), 100.0);
        let right = shape_node(
            "shape.ellipse",
            &[v2("center", 200.0, 0.0), v2("radius", 50.0, 10.0)],
        )
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let network = Graph::new()
            .add_node(left)
            .unwrap()
            .add_node(right)
            .unwrap();
        let mut layer = Layer::new(LayerId::next(), "shapes", network.clone()).with_time(0, 0, 300);

        let rect_of = |layer: &Layer| {
            let comp = comp_with_layers(vec![layer.clone()]);
            let comp_id = comp.id;
            let path = NetworkPath::layer(comp_id, layer.id);
            let document = Document::default().with_composition(comp);
            let ctx = OverlayContext {
                resolution: Some((1920, 1080)),
                playback: Some(super::super::PlaybackPosition {
                    frame: 0,
                    fps: FrameRate::new(30, 1),
                }),
                results: evaluated_results(&layer.network, &path),
                document: Some(document.clone()),
                ..OverlayContext::default()
            };
            layer_comp_rect(&ctx, &document, comp_id, layer.id)
        };

        let rect = rect_of(&layer).unwrap();
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
        let moved = rect_of(&layer).unwrap();
        assert_eq!((moved.x, moved.y), (-40.0, -30.0));
        assert_eq!((moved.w, moved.h), (rect.w, rect.h));
        assert_eq!(
            selected_body_pointer_hint(&[moved], (255.0, 25.0)),
            Some(ViewerPointerHint::MovableBody),
            "the pointer boundary follows the transformed bbox"
        );
        assert_eq!(selected_body_pointer_hint(&[moved], (-45.0, 25.0)), None);

        // A layer that places no geometry gets no bbox rather than a guessed
        // one — a media or effects-only network has nothing to measure.
        let empty = Layer::new(LayerId::next(), "null", Graph::new()).with_time(0, 0, 300);
        assert!(rect_of(&empty).is_none());
    }

    /// A chain is measured at its end: unioning both the pre- and
    /// post-transform extents would outline a rectangle the layer never shows.
    #[test]
    fn a_transform_chain_contributes_only_its_result_to_the_layer_bbox() {
        use ravel_core::id::{EdgeId, InputPortIndex, LayerId, OutputPortIndex};

        let rect = square_node((0.0, 0.0), 100.0);
        let rect_id = rect.id;
        let transform = shape_node(
            "geometry.transform",
            &[("translate", ParameterValue::vec3(400.0, 0.0, 0.0))],
        )
        .with_input("geometry", &[ravel_core::id::DataTypeId::GEOMETRY])
        .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
        let transform_id = transform.id;
        let network = Graph::new()
            .add_node(rect)
            .unwrap()
            .add_node(transform)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                rect_id,
                OutputPortIndex(0),
                transform_id,
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(
            terminal_geometry_nodes(&network),
            vec![transform_id],
            "the source rect is consumed by another geometry node"
        );

        let layer = Layer::new(LayerId::next(), "chain", network.clone()).with_time(0, 0, 300);
        let comp = comp_with_layers(vec![layer.clone()]);
        let comp_id = comp.id;
        let path = NetworkPath::layer(comp_id, layer.id);
        let document = Document::default().with_composition(comp);
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            results: evaluated_results(&network, &path),
            document: Some(document.clone()),
            ..OverlayContext::default()
        };
        let rect = layer_comp_rect(&ctx, &document, comp_id, layer.id).unwrap();
        assert_eq!(
            (rect.x, rect.w),
            (350.0, 100.0),
            "the bbox spanned the chain instead of its result: {rect:?}"
        );
    }

    /// A multi-output node whose non-geometry port feeds a geometry operator
    /// still places its own geometry: it is the *port* the edge leaves by that
    /// says whether the geometry was consumed, not the node.
    #[test]
    fn a_multi_output_node_stays_terminal_when_only_its_other_port_is_wired() {
        use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};

        // Port 0 geometry, port 1 a scalar that drives the transform.
        let source = shape_node("test.shape_and_scalar", &[])
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_output("amount", DataTypeId::SCALAR);
        let source_id = source.id;
        let transform = shape_node("geometry.transform", &[])
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_output("geometry", DataTypeId::GEOMETRY);
        let transform_id = transform.id;
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(transform)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                // The **scalar** port, not the geometry one.
                OutputPortIndex(1),
                transform_id,
                InputPortIndex(1),
            )
            .unwrap();

        let mut terminal = terminal_geometry_nodes(&graph);
        terminal.sort_by_key(|id| id.raw());
        let mut expected = vec![source_id, transform_id];
        expected.sort_by_key(|id| id.raw());
        assert_eq!(
            terminal, expected,
            "the source's geometry is unconsumed, so it is terminal too"
        );

        // Wiring the geometry port as well does drop it.
        let wired = graph
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                transform_id,
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(terminal_geometry_nodes(&wired), vec![transform_id]);
    }

    /// The selection's rects come out in selection order, skipping layers with
    /// nothing to measure.
    #[test]
    fn layer_selection_rects_skip_unmeasurable_layers() {
        use ravel_core::id::LayerId;

        let network = Graph::new()
            .add_node(square_node((10.0, 10.0), 20.0))
            .unwrap();
        let shapes = Layer::new(LayerId::next(), "shapes", network.clone()).with_time(0, 0, 300);
        let null = Layer::new(LayerId::next(), "null", Graph::new()).with_time(0, 0, 300);
        let comp = comp_with_layers(vec![shapes.clone(), null.clone()]);
        let comp_id = comp.id;
        let path = NetworkPath::layer(comp_id, shapes.id);
        let document = Document::default().with_composition(comp);
        let layer_selection = crate::panels::LayerSelection::of(comp_id, vec![null.id, shapes.id]);
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            results: evaluated_results(&network, &path),
            document: Some(document),
            layer_selection,
            ..OverlayContext::default()
        };

        let rects = layer_selection_comp_rects(&ctx);
        assert_eq!(rects.len(), 1, "only the measurable layer draws a bbox");
        assert_eq!((rects[0].x, rects[0].y), (0.0, 0.0));
    }

    #[test]
    fn handle_centers_cover_corners_and_edge_midpoints() {
        let centers = overlay::selection_handle_centers(10.0, 20.0, 100.0, 50.0);
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
        assert_eq!(
            sample_vec2_param(&node, "center", 0, &ctx),
            Some((60.0, 45.0))
        );
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
        assert_eq!(
            sample_vec2_param(&node, "center", 0, &ctx),
            Some((10.0, 20.0))
        );
        assert_eq!(
            sample_vec2_param(&node, "radius", 0, &ctx),
            Some((30.0, 15.0))
        );
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
        // bbox/hit-test integration: the drawn node evaluates to the extent
        // the gesture drew.
        let (overlay_ctx, network) = geometry_context(graph.clone(), &[node_id]);
        let bounds =
            node_comp_rect(&overlay_ctx, &network, node_id).expect("the drawn rect evaluated");
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
        assert_eq!(
            pen_close_pointer_hint(&points, (13.0, 14.0), 5.0),
            Some(ViewerPointerHint::PenClose)
        );
        assert_eq!(pen_close_pointer_hint(&points, (16.0, 10.0), 5.0), None);
    }

    /// A custom path outlines the curve the evaluator produced, tangents
    /// included. The parameter-derived bbox this replaces used the control
    /// points alone and therefore under-covered a curve that bulges past
    /// them — the outline no longer contains the drawn shape only because it
    /// no longer guesses at it.
    #[test]
    fn a_custom_path_outlines_the_evaluated_curve() {
        let node = custom_path_node(
            registry()
                .create_node("shape.custom_path", NodeId::next())
                .unwrap(),
            vec![
                PathPoint {
                    p: Vec2(10.0, 20.0),
                    in_tan: Vec2(-40.0, -40.0),
                    out_tan: Vec2(40.0, 40.0),
                },
                corner_path_point((50.0, 80.0)),
            ],
            false,
        );
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let (ctx, network) = geometry_context(graph, &[id]);
        let bounds = node_comp_rect(&ctx, &network, id).expect("the path evaluated");
        assert!(
            bounds.x <= 10.0 && bounds.y <= 20.0,
            "the outline does not contain the first control point: {bounds:?}"
        );
        assert!(
            bounds.x + bounds.w >= 50.0 && bounds.y + bounds.h >= 80.0,
            "the outline does not contain the last control point: {bounds:?}"
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
    }

    #[test]
    fn selected_body_hint_only_covers_selected_bounds() {
        let selected = [CompRect {
            x: 10.0,
            y: 20.0,
            w: 40.0,
            h: 30.0,
        }];
        assert_eq!(
            selected_body_pointer_hint(&selected, (25.0, 35.0)),
            Some(ViewerPointerHint::MovableBody)
        );
        assert_eq!(selected_body_pointer_hint(&selected, (60.0, 35.0)), None);
        assert_eq!(
            selected_body_pointer_hint(&[], (25.0, 35.0)),
            None,
            "an unselected shape contributes no hover target"
        );
    }

    #[test]
    fn viewer_pointer_hint_notifies_only_on_idle_changes() {
        assert_eq!(
            viewer_pointer_hint_transition(
                ViewerPointerHint::Empty,
                ViewerPointerHint::Drawing,
                false,
            ),
            Some(ViewerPointerHint::Drawing)
        );
        assert_eq!(
            viewer_pointer_hint_transition(
                ViewerPointerHint::Drawing,
                ViewerPointerHint::Drawing,
                false,
            ),
            None
        );
        assert_eq!(
            viewer_pointer_hint_transition(
                ViewerPointerHint::Empty,
                ViewerPointerHint::Drawing,
                true,
            ),
            None
        );
        assert_eq!(ViewerPointerHint::Drawing.cursor(), CursorStyle::Crosshair);
        assert_eq!(
            ViewerPointerHint::MovableBody.cursor(),
            CursorStyle::OpenHand
        );
        assert_eq!(
            viewer_drag_cursor(false, true, false, false, None, None),
            Some(CursorStyle::ClosedHand)
        );
        assert_eq!(
            viewer_drag_cursor(
                false,
                false,
                false,
                false,
                Some(PathHandleKind::OutTangent),
                None
            ),
            Some(CursorStyle::Crosshair)
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

        let rect = |center: (f32, f32)| Graph::new().add_node(square_node(center, 100.0)).unwrap();
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

        let window = cx.add_window(|window, cx| {
            ViewerPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        window
            .update(cx, |panel, _window, _cx| {
                panel.composition_resolution = Some((1920, 1080));
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
            })
            .unwrap();
        publish_geometry_results(&project, cx);
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
            sample_vec2_param(&node, "center", 0, &eval_ctx()).unwrap()
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
                panel.move_dragged(point(px(40.0), px(25.0)), DragModifiers::default(), cx);
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
                panel.move_dragged(point(px(40.0), px(25.0)), DragModifiers::default(), cx);
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

    // -----------------------------------------------------------------------
    // Drag snapping (SNAP-1)
    // -----------------------------------------------------------------------

    /// Two layers with measurable geometry, and the overlay context that sees
    /// them: the fixture the snap candidates are enumerated from.
    fn snap_context() -> (OverlayContext, CompId, Vec<LayerId>) {
        use ravel_core::id::LayerId;

        let layers: Vec<Layer> = [(0.0, 0.0), (400.0, 200.0)]
            .into_iter()
            .enumerate()
            .map(|(index, center)| {
                let graph = Graph::new().add_node(square_node(center, 100.0)).unwrap();
                Layer::new(LayerId::next(), format!("L{index}"), graph).with_time(0, 0, 300)
            })
            .collect();
        let comp = comp_with_layers(layers);
        let ids: Vec<LayerId> = comp.layers.iter().map(|layer| layer.id).collect();
        let mut values = HashMap::new();
        for layer in &comp.layers {
            let network = NetworkPath::layer(comp.id, layer.id);
            values.extend(evaluated_results(&layer.network, &network).values);
        }
        let comp_id = comp.id;
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            document: Some(Document::default().with_composition(comp)),
            results: overlay::OverlayResults::new(values),
            ..OverlayContext::default()
        };
        (ctx, comp_id, ids)
    }

    /// The candidates: the composition frame first, then every layer's edges
    /// and centre — except the ones the gesture is moving, whose own edges
    /// travel with it.
    #[test]
    fn snap_candidates_cover_the_frame_and_the_other_layers() {
        let (ctx, comp, layers) = snap_context();

        let all = SnapLines::collect(&ctx, Some(comp), &[]);
        assert_eq!(
            &all.x[..3],
            &[0.0, 960.0, 1920.0],
            "the composition frame is enumerated first, so it wins ties"
        );
        assert_eq!(&all.y[..3], &[0.0, 540.0, 1080.0]);
        // The first layer is a 100-square at the origin, the second at
        // (400, 200): both edges and the centre, per axis.
        assert!(all.x.contains(&-50.0) && all.x.contains(&50.0));
        assert!(all.x.contains(&350.0) && all.x.contains(&400.0) && all.x.contains(&450.0));
        assert!(all.y.contains(&150.0) && all.y.contains(&200.0) && all.y.contains(&250.0));

        let without_first = SnapLines::collect(&ctx, Some(comp), &layers[..1]);
        assert!(
            !without_first.x.contains(&-50.0),
            "a moving layer contributes no candidate of its own"
        );
        assert!(
            without_first.x.contains(&350.0),
            "the layers it is aligned against are still there"
        );

        // No composition named: only what the frame itself provides.
        let frame_only = SnapLines::collect(&ctx, None, &[]);
        assert_eq!(frame_only.x, vec![0.0, 960.0, 1920.0]);
    }

    /// The safe areas are candidates exactly while they are drawn, and from the
    /// same fractions [`overlay::SafeAreaOverlay`] draws them with.
    #[test]
    fn safe_areas_are_candidates_only_while_they_are_shown() {
        let (mut ctx, comp, _) = snap_context();
        assert!(
            !SnapLines::collect(&ctx, Some(comp), &[]).x.contains(&96.0),
            "hidden safe areas pull nothing"
        );

        ctx.show_safe_areas = true;
        let shown = SnapLines::collect(&ctx, Some(comp), &[]);
        for fraction in overlay::SAFE_AREA_FRACTIONS {
            let inset = 1920.0 * (1.0 - fraction) * 0.5;
            assert!(shown.x.contains(&inset) && shown.x.contains(&(1920.0 - inset)));
        }
    }

    /// The completion criterion, on the real gesture: a layer drag lands on the
    /// composition centre, the guide reports which line it landed on, and the
    /// whole gesture is still one undo step.
    #[gpui::test]
    fn a_snapped_layer_drag_stays_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let before: Vec<(f32, f32)> = layers
            .iter()
            .map(|layer| rect_center(&project, comp_id, *layer, cx))
            .collect();

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                // The two selected layers span x −50..150, so their centre sits
                // at 50; a 905 drag puts it 5 short of the composition centre.
                panel.move_dragged(point(px(905.0), px(0.0)), DragModifiers::default(), cx);
                assert_eq!(
                    panel.snap_guides.x,
                    Some(960.0),
                    "the guide names the line the delta landed on"
                );
                panel.move_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        for (layer, origin) in layers.iter().zip(&before) {
            assert_eq!(
                rect_center(&project, comp_id, *layer, cx),
                (origin.0 + 910.0, origin.1),
                "the drag was pulled the last 5 units onto the composition centre"
            );
        }

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        for (layer, origin) in layers.iter().zip(&before) {
            assert_eq!(
                rect_center(&project, comp_id, *layer, cx),
                *origin,
                "one undo restores the snapped gesture"
            );
        }
        project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layer_count(),
                2,
                "the undo step above was the drag, not the layer creation"
            );
        });
    }

    /// The suppression key reaches the gesture: the drag lands where the
    /// pointer put it, and reports no guide.
    #[gpui::test]
    fn the_primary_modifier_turns_snapping_off_for_a_layer_drag(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let before = rect_center(&project, comp_id, layers[0], cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                panel.move_dragged(
                    point(px(905.0), px(0.0)),
                    DragModifiers {
                        primary: true,
                        ..DragModifiers::default()
                    },
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            rect_center(&project, comp_id, layers[0], cx),
            (before.0 + 905.0, before.1),
            "the primary modifier kept the pointer's own delta"
        );
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.snap_guides.is_empty(), "and drew no guide");
            })
            .unwrap();
    }

    /// Cmd / Ctrl is the suppression key, so the two modifiers that mean
    /// something else to a gesture keep snapping alive. Shift means nothing to
    /// a layer move, and Alt means nothing to it either.
    #[gpui::test]
    fn shift_and_alt_keep_snapping_on_for_a_layer_drag(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let before = rect_center(&project, comp_id, layers[0], cx);

        for modifiers in [
            DragModifiers {
                shift: true,
                ..DragModifiers::default()
            },
            DragModifiers {
                alt: true,
                ..DragModifiers::default()
            },
        ] {
            window
                .update(cx, |panel, _window, cx| {
                    panel.layer_move_mouse_down((0.0, 0.0), cx);
                    panel.move_dragged(point(px(905.0), px(0.0)), modifiers, cx);
                    assert_eq!(
                        panel.snap_guides.x,
                        Some(960.0),
                        "{modifiers:?} constrains nothing here, so the pull stays"
                    );
                    panel.cancel_move(cx);
                })
                .unwrap();
            cx.run_until_parked();
            publish_geometry_results(&project, cx);
            assert_eq!(
                rect_center(&project, comp_id, layers[0], cx),
                before,
                "the cancel put the layer back for the next round"
            );
        }
    }

    /// Shift squares a drawn shape off from the larger of the two deltas, which
    /// would overwrite a snapped axis — so a constrained drawing drag snaps
    /// nothing rather than drawing a guide it then misses.
    #[gpui::test]
    fn shift_turns_snapping_off_while_drawing(cx: &mut TestAppContext) {
        let (window, _project, comp_id, layer) = shell_setup(cx);
        let network = NetworkPath::layer(comp_id, layer);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(network),
                nodes: HashSet::new(),
            });
            cx.set_global(ToolState {
                active: ravel_ui::ToolKind::Rect,
                ..ToolState::default()
            });
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.shape_mouse_down(&press_at(panel, (500.0, 500.0)), cx);
                let event = MouseMoveEvent {
                    // Five units short of the composition centre: inside the
                    // reach, and ignored because Shift is down.
                    position: window_point(panel, (955.0, 600.0)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                };
                panel.shape_dragged(&event, cx);
                assert!(
                    panel.snap_guides.is_empty(),
                    "the constraint decides the corner, so nothing was aligned"
                );
                let geo = panel
                    .shape_drag
                    .as_ref()
                    .and_then(|drag| drag.created.as_ref())
                    .expect("the drag created a shape")
                    .geo;
                // Squared off from the larger delta (455 across, 100 down).
                assert!(
                    (geo.half.0 - geo.half.1).abs() < 1e-3,
                    "Shift still squares the shape off: {geo:?}"
                );
            })
            .unwrap();
    }

    /// Shift applies one ratio to both axes of a shell scale, which would move
    /// a snapped grip off the line it landed on. The move grip constrains
    /// nothing, so it keeps snapping.
    #[gpui::test]
    fn shift_turns_snapping_off_for_a_scale_grip_only(cx: &mut TestAppContext) {
        let (window, project, _comp_id, _layer) = shell_setup(cx);
        let shift = DragModifiers {
            shift: true,
            ..DragModifiers::default()
        };

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                let to = window_point(panel, (955.0, 210.0));
                panel.handle_dragged(to, shift, cx);
                assert!(
                    panel.snap_guides.is_empty(),
                    "a uniform scale overwrites whichever axis was pulled"
                );
                panel.cancel_handle_drag(cx);
            })
            .unwrap();
        cx.run_until_parked();
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                // The move grip, at the bbox centre: Shift does nothing there.
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, (100.0, 200.0)), cx));
                let to = window_point(panel, (955.0, 200.0));
                panel.handle_dragged(to, shift, cx);
                assert_eq!(
                    panel.snap_guides.x,
                    Some(960.0),
                    "the move grip constrains nothing, so the pull stays"
                );
                panel.cancel_handle_drag(cx);
            })
            .unwrap();
    }

    /// The guide is a report of a correction in flight, so it reaches the
    /// overlays only while the gesture is live.
    #[gpui::test]
    fn the_snap_guide_does_not_outlive_the_gesture(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _layers) = multi_layer_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                panel.move_dragged(point(px(905.0), px(0.0)), DragModifiers::default(), cx);
                assert_eq!(panel.overlay_context(cx).snap_guides.x, Some(960.0));
                panel.move_ended(cx);
                assert!(
                    panel.overlay_context(cx).snap_guides.is_empty(),
                    "the gesture ended, so nothing is holding the alignment"
                );
            })
            .unwrap();
    }

    /// With the playhead stopped and the document untouched, changing the
    /// selection has to post a new viewer evaluation: the overlays declare
    /// their targets while the request is assembled, so without this the new
    /// selection's geometry or field is never evaluated and the overlay stays
    /// blank until the next frame step.
    ///
    /// The signal is the overlay snapshot. These tests run without an
    /// evaluation worker, and the no-worker branch of `request_viewer_eval` is
    /// the only thing on this path that replaces the snapshot — so a cleared
    /// snapshot means a request went out, and a surviving one means none did.
    #[gpui::test]
    fn changing_the_selection_while_stopped_posts_a_viewer_evaluation(cx: &mut TestAppContext) {
        let (_window, project, comp_id, layers) = multi_layer_setup(cx);
        let network = NetworkPath::layer(comp_id, layers[0]);
        let node = project.read_with(cx, |project, _| {
            ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the layer network")
                .nodes()
                .next()
                .expect("a node to select")
                .id
        });
        let snapshot_present = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                cx.try_global::<overlay::OverlayResults>()
                    .is_some_and(|results| !results.values.is_empty())
            })
        };
        let select = |nodes: HashSet<NodeId>, cx: &mut TestAppContext| {
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(network.clone()),
                    nodes,
                });
            });
            cx.run_until_parked();
        };

        publish_geometry_results(&project, cx);
        assert!(snapshot_present(cx), "the fixture publishes a snapshot");

        // A real change: the request goes out.
        select(HashSet::from([node]), cx);
        assert!(
            !snapshot_present(cx),
            "changing the selection posted no evaluation, so the overlay would \
             stay blank until the next frame step"
        );

        // Re-publishing the *same* selection — what a click on an already
        // selected node does — must not post another request.
        publish_geometry_results(&project, cx);
        select(HashSet::from([node]), cx);
        assert!(
            snapshot_present(cx),
            "an unchanged selection re-posted the evaluation"
        );

        // Clearing it is a change too: the overlay has to stop drawing the
        // node that is no longer selected.
        select(HashSet::new(), cx);
        assert!(!snapshot_present(cx));
    }

    /// The layer selection drives the layer-level bboxes, so it posts the
    /// request on the same rule.
    #[gpui::test]
    fn changing_the_layer_selection_while_stopped_posts_a_viewer_evaluation(
        cx: &mut TestAppContext,
    ) {
        let (_window, project, _comp_id, layers) = multi_layer_setup(cx);
        publish_geometry_results(&project, cx);

        cx.update(|cx| crate::panels::set_layer_selection(vec![layers[1]], cx));
        cx.run_until_parked();

        let present = cx.update(|cx| {
            cx.try_global::<overlay::OverlayResults>()
                .is_some_and(|results| !results.values.is_empty())
        });
        assert!(
            !present,
            "changing the layer selection posted no evaluation"
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
        // The commit's re-request cleared the snapshot (no worker in tests).
        publish_geometry_results(&project, cx);

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
                    "center",
                    ParameterValue::Channel2([
                        AnimationChannel::keyframes(curve),
                        AnimationChannel::constant(100.0),
                    ]),
                )
                .with_param("width", ParameterValue::Float(100.0))
                .with_param("height", ParameterValue::Float(100.0))
                .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY);
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

        let window = cx.add_window(|window, cx| {
            ViewerPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        publish_geometry_results(&project, cx);
        window
            .update(cx, |panel, _window, cx| {
                panel.composition_resolution = Some((1920, 1080));
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
                // (100, 100) is the shared rect center.
                panel.layer_move_mouse_down((100.0, 100.0), cx);
                panel.move_dragged(point(px(150.0), px(100.0)), DragModifiers::default(), cx);
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
                    .find(|param| param.key == "center")
                    .unwrap()
                    .clone();
                match param.value {
                    // Only the X component was keyframed; the drag must key
                    // that component and leave Y a constant.
                    ParameterValue::Channel2(chs) => match chs[0].source.clone() {
                        ChannelSource::Keyframes(curve) => {
                            assert!(
                                matches!(chs[1].source, ChannelSource::Constant(_)),
                                "the constant component stays constant"
                            );
                            curve
                        }
                        other => panic!("center lost its keyframes: {other:?}"),
                    },
                    other => panic!("center lost its channels: {other:?}"),
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

    // -----------------------------------------------------------------------
    // Shell manipulator gestures (REQ-UI-011 unit 7)
    // -----------------------------------------------------------------------

    /// One selected layer with a 40x20 rect centered at (100, 200), on a 1:1
    /// viewport: the shell bbox is (80, 190, 40, 20) and its south-east grip
    /// sits at window pixel (120, 210).
    fn shell_setup(
        cx: &mut TestAppContext,
    ) -> (
        WindowHandle<ViewerPanel>,
        Entity<ProjectState>,
        ravel_core::id::CompId,
        ravel_core::id::LayerId,
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
            // The manipulator only answers the pointer under Select.
            cx.set_global(ToolState::default());
        });

        let (comp_id, layer) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let layer = LayerId::next();
            let network = Graph::new()
                .add_node(
                    shape_node(
                        "shape.rect",
                        &[
                            v2("center", 100.0, 200.0),
                            f("width", 40.0),
                            f("height", 20.0),
                        ],
                    )
                    .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY),
                )
                .unwrap();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                Layer::new(layer, "L", network).with_time(0, 0, 300),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (comp_id, layer)
        });
        cx.update(|cx| crate::panels::set_layer_selection(vec![layer], cx));

        let window = cx.add_window(|window, cx| {
            ViewerPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        window
            .update(cx, |panel, _window, _cx| {
                panel.composition_resolution = Some((1920, 1080));
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
            })
            .unwrap();
        publish_geometry_results(&project, cx);
        (window, project, comp_id, layer)
    }

    /// The window position of a composition point, read from the panel's
    /// *current* viewport. The fixture's 1:1 viewport only survives until the
    /// canvas lays out for real, so a test that hardcodes window pixels
    /// silently starts pressing somewhere else.
    fn window_point(panel: &ViewerPanel, comp: (f32, f32)) -> Point<Pixels> {
        let resolution = panel.composition_resolution.expect("no composition");
        let rect = panel.viewport.rect(panel.viewport_size.get(), resolution);
        let origin = panel.viewport_origin.get();
        let (x, y) = comp_to_screen(comp, rect, resolution.0);
        point(px(x + origin.0), px(y + origin.1))
    }

    /// A left press on the composition point `comp`.
    fn press_at(panel: &ViewerPanel, comp: (f32, f32)) -> MouseDownEvent {
        MouseDownEvent {
            button: MouseButton::Left,
            position: window_point(panel, comp),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }
    }

    /// The composition point of the south-east scale grip for the fixture's
    /// (80, 190, 40, 20) bbox.
    const SE_GRIP: (f32, f32) = (120.0, 210.0);

    fn shell_scale(
        project: &Entity<ProjectState>,
        comp_id: ravel_core::id::CompId,
        layer: ravel_core::id::LayerId,
        cx: &mut TestAppContext,
    ) -> (f32, f32) {
        project.read_with(cx, |project, _| {
            let transform = &project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer)
                .unwrap()
                .transform;
            (
                transform.scale[0].evaluate(0.0, &eval_ctx()),
                transform.scale[1].evaluate(0.0, &eval_ctx()),
            )
        })
    }

    /// A whole shell drag is one undo step, however many previews it published
    /// on the way. Three moves and a single `undo` must land back at 100%.
    #[gpui::test]
    fn a_shell_handle_drag_is_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        assert_eq!(shell_scale(&project, comp_id, layer, cx), (1.0, 1.0));

        window
            .update(cx, |panel, _window, cx| {
                assert!(
                    panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx),
                    "the south-east grip took the press"
                );
                for x in [130.0, 150.0, 160.0] {
                    let to = window_point(panel, (x, 215.0));
                    panel.handle_dragged(to, DragModifiers::default(), cx);
                }
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        let scaled = shell_scale(&project, comp_id, layer, cx);
        assert!(
            (scaled.0 - 2.0).abs() < 1e-3 && (scaled.1 - 1.25).abs() < 1e-3,
            "the last preview is what the gesture committed: {scaled:?}"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            shell_scale(&project, comp_id, layer, cx),
            (1.0, 1.0),
            "one undo covers the whole gesture, not just the last preview"
        );
        // That single undo did not eat the layer: the next step back is its
        // creation, so the gesture really was one step.
        project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layer_count(),
                1
            );
        });
    }

    /// The drawing tools snap the corner they are dragging, so a shape drawn
    /// near the composition centre lands on it exactly.
    #[gpui::test]
    fn a_drawn_shape_snaps_its_moving_corner(cx: &mut TestAppContext) {
        let (window, _project, comp_id, layer) = shell_setup(cx);
        let network = NetworkPath::layer(comp_id, layer);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(network),
                nodes: HashSet::new(),
            });
            cx.set_global(ToolState {
                active: ravel_ui::ToolKind::Rect,
                ..ToolState::default()
            });
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.shape_mouse_down(&press_at(panel, (500.0, 500.0)), cx);
                let event = MouseMoveEvent {
                    // Five units short of the composition's horizontal centre.
                    position: window_point(panel, (955.0, 600.0)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                };
                panel.shape_dragged(&event, cx);
                assert_eq!(panel.snap_guides.x, Some(960.0));
                let geo = panel
                    .shape_drag
                    .as_ref()
                    .and_then(|drag| drag.created.as_ref())
                    .expect("the drag created a shape")
                    .geo;
                // The corner is at 960, not at the pointer's 955: centre
                // (500 + 960) / 2 and half extent (960 − 500) / 2.
                assert!(
                    (geo.center.0 - 730.0).abs() < 1e-3 && (geo.half.0 - 230.0).abs() < 1e-3,
                    "the drawn corner was not pulled onto 960: {geo:?}"
                );
                assert!(
                    (geo.center.1 - 550.0).abs() < 1e-3 && (geo.half.1 - 50.0).abs() < 1e-3,
                    "the unsnapped axis kept the pointer's own extent: {geo:?}"
                );
            })
            .unwrap();
    }

    /// A shell scale grip snaps too: the grabbed grip is the point that lands
    /// on the candidate, so the layer resizes to meet the line exactly.
    #[gpui::test]
    fn a_shell_scale_grip_snaps_the_grabbed_point(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                // Five units short of the composition's horizontal centre.
                let to = window_point(panel, (955.0, 210.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
                assert_eq!(panel.snap_guides.x, Some(960.0));
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        // The bbox's fixed corner is x = 80 and the grip started at 120, so a
        // grip landing on 960 is a factor of (960 − 80) / (120 − 80).
        let scaled = shell_scale(&project, comp_id, layer, cx);
        assert!(
            (scaled.0 - 22.0).abs() < 1e-3,
            "the grip was pulled onto the composition centre: {scaled:?}"
        );
    }

    /// The move grip carries the whole layer, so it is the layer's bounding box
    /// — not the grabbed point — that lands on the candidate.
    #[gpui::test]
    fn the_shell_move_grip_snaps_the_layer_bbox(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                // The move grip sits at the bbox centre, (100, 200).
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, (100.0, 200.0)), cx));
                assert_eq!(
                    panel
                        .handle_drag
                        .as_ref()
                        .and_then(|drag| drag.handle.id.shell()),
                    Some(overlay::ShellHandle::Position)
                );
                // 855 puts the bbox centre at 955, five short of 960.
                let to = window_point(panel, (955.0, 200.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
                assert_eq!(panel.snap_guides.x, Some(960.0));
                assert_eq!(panel.snap_guides.y, None, "the vertical axis is clear");
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        let position = project.read_with(cx, |project, _| {
            let transform = &project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer)
                .unwrap()
                .transform;
            (
                transform.position[0].evaluate(0.0, &eval_ctx()),
                transform.position[1].evaluate(0.0, &eval_ctx()),
            )
        });
        assert!(
            (position.0 - 860.0).abs() < 1e-3 && position.1.abs() < 1e-3,
            "the layer travelled the extra five units onto the centre: {position:?}"
        );
    }

    /// An edge grip drives one axis; `scale_edits` throws the other one away.
    /// Snapping the discarded axis would draw a guide for an alignment the edit
    /// cannot make, and mark the gesture changed for a document that did not
    /// change — a no-op undo step.
    #[gpui::test]
    fn an_edge_grip_snaps_only_the_axis_it_writes(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                // The action-safe rectangle puts a candidate at x = 96, four
                // units from the grip below.
                panel.show_safe_areas = true;
                // The north edge grip, at the middle of the bbox's top edge.
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, (100.0, 190.0)), cx));
                assert_eq!(
                    panel
                        .handle_drag
                        .as_ref()
                        .and_then(|drag| drag.handle.id.shell()),
                    Some(overlay::ShellHandle::Scale(1)),
                    "the press landed on the north edge grip"
                );
                // The pointer does not move at all.
                let held = window_point(panel, (100.0, 190.0));
                panel.handle_dragged(held, DragModifiers::default(), cx);
                assert!(
                    panel.snap_guides.is_empty(),
                    "this grip writes Y only, so an X candidate is not an alignment it can make"
                );
                assert!(
                    !panel.handle_drag.as_ref().is_some_and(|drag| drag.changed),
                    "nothing moved, so there is nothing to commit"
                );
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        // `changed` is what decides the commit, so the assertion above is the
        // undo step; this is the value it would have committed.
        assert_eq!(
            shell_scale(&project, comp_id, layer, cx),
            (1.0, 1.0),
            "the pull reached neither axis of the edit"
        );
    }

    /// The completion criterion, on the real gesture: the pull covers the same
    /// screen distance at every zoom, which is a different composition distance
    /// each time.
    #[gpui::test]
    fn the_snap_threshold_is_the_same_screen_distance_at_every_zoom(cx: &mut TestAppContext) {
        let (window, project, _comp_id, _layer) = shell_setup(cx);

        for (percent, comp_per_px) in [(50.0f32, 2.0f32), (200.0, 0.5)] {
            // The cancel below posts a fresh evaluation, so the bounds the
            // manipulator reads have to be republished for the next round.
            publish_geometry_results(&project, cx);
            window
                .update(cx, |panel, _window, cx| {
                    panel.set_zoom_percent(percent);
                    // The south-east grip: one point, so the distance measured
                    // is the pointer's own rather than an edge of a bbox.
                    assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                    // Six screen pixels short of the composition centre: inside
                    // the eight-pixel reach at both zooms, and twelve or three
                    // composition units depending on which one.
                    let near = window_point(panel, (960.0 - 6.0 * comp_per_px, SE_GRIP.1));
                    panel.handle_dragged(near, DragModifiers::default(), cx);
                    assert_eq!(
                        panel.snap_guides.x,
                        Some(960.0),
                        "six screen pixels pull at {percent}%"
                    );
                    // Twelve screen pixels: out of reach at both zooms.
                    let far = window_point(panel, (960.0 - 12.0 * comp_per_px, SE_GRIP.1));
                    panel.handle_dragged(far, DragModifiers::default(), cx);
                    assert_eq!(
                        panel.snap_guides.x, None,
                        "twelve screen pixels do not pull at {percent}%"
                    );
                    panel.cancel_handle_drag(cx);
                })
                .unwrap();
            cx.run_until_parked();
        }
    }

    /// A press starts a gesture that has corrected nothing yet, so the guide of
    /// the previous one must not be on screen when the first frame renders.
    #[gpui::test]
    fn a_new_gesture_starts_with_no_guide(cx: &mut TestAppContext) {
        let (window, project, _comp_id, _layers) = multi_layer_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((0.0, 0.0), cx);
                panel.move_dragged(point(px(905.0), px(0.0)), DragModifiers::default(), cx);
                panel.move_ended(cx);
                assert_eq!(panel.snap_guides.x, Some(960.0), "the gesture did snap");
            })
            .unwrap();
        cx.run_until_parked();
        // The layers moved, so their bounds have to be republished before the
        // next press can land inside one of them.
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.layer_move_mouse_down((910.0, 0.0), cx);
                assert!(
                    panel.move_drag.is_some(),
                    "the press started a second gesture"
                );
                assert!(
                    panel.overlay_context(cx).snap_guides.is_empty(),
                    "a press before its first move has no correction to report"
                );
            })
            .unwrap();
    }

    /// Rotation has no edge to line up, so its grip snaps nothing — and says so
    /// by reporting no guide.
    #[gpui::test]
    fn a_shell_rotation_drag_does_not_snap(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                // Twelve units diagonally outside the corner grip: past the 8px
                // scale radius, inside the 18px rotation ring.
                let press = press_at(panel, (SE_GRIP.0 + 9.0, SE_GRIP.1 + 9.0));
                assert!(panel.overlay_handle_mouse_down(&press, cx));
                assert_eq!(
                    panel
                        .handle_drag
                        .as_ref()
                        .and_then(|drag| drag.handle.id.shell()),
                    Some(overlay::ShellHandle::Rotate(7)),
                    "the press landed in the rotation ring"
                );
                // The press sits 9 units off the grip it grabbed, so this is
                // the sweep that puts the grip's own anchor exactly on the
                // composition centre — a zero distance any snapping candidate
                // would win.
                let to = window_point(panel, (969.0, 549.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
                assert!(
                    panel.snap_guides.is_empty(),
                    "a sweep onto the composition centre is not an alignment"
                );
            })
            .unwrap();
    }

    /// Escape during a shell drag restores the document the gesture started
    /// from and leaves no undo step behind.
    #[gpui::test]
    fn escape_reverts_a_shell_handle_drag(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        let snapshot = project.read_with(cx, |project, _| project.document().clone());

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                let to = window_point(panel, (160.0, 215.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
                assert_ne!(shell_scale_of(panel, cx, comp_id, layer), (1.0, 1.0));
                panel.cancel_handle_drag(cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "Escape restores the document the press captured"
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
            "the cancelled gesture left no step of its own in the history"
        );
    }

    /// The overlay hit test runs before `select_mouse_down` and
    /// `shape_mouse_down`, so a press on a shell grip under a drawing or
    /// navigation tool has to fall through to that tool instead of starting a
    /// transform.
    #[gpui::test]
    fn only_the_select_tool_grabs_a_shell_grip(cx: &mut TestAppContext) {
        // `_project` keeps the entity alive: `ProjectStateHandle` is weak, and
        // dropping it here would empty every overlay's document instead of
        // testing the tool gate.
        let (window, _project, ..) = shell_setup(cx);

        for tool in [
            ravel_ui::ToolKind::Rect,
            ravel_ui::ToolKind::Ellipse,
            ravel_ui::ToolKind::Pen,
            ravel_ui::ToolKind::Hand,
            ravel_ui::ToolKind::Zoom,
        ] {
            cx.update(|cx| {
                cx.set_global(ToolState {
                    active: tool,
                    ..Default::default()
                })
            });
            window
                .update(cx, |panel, _window, cx| {
                    assert!(
                        !panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx),
                        "{tool:?} must keep the press it is waiting for"
                    );
                    assert!(panel.handle_drag.is_none(), "{tool:?} started a shell drag");
                    // The cursor must not promise a transform either.
                    assert_ne!(
                        panel.pointer_hint_at(window_point(panel, SE_GRIP), cx),
                        Some(ViewerPointerHint::ResizeUpLeftDownRight),
                        "{tool:?} still advertises the scale grip"
                    );
                })
                .unwrap();
        }

        cx.update(|cx| cx.set_global(ToolState::default()));
        window
            .update(cx, |panel, _window, cx| {
                assert!(
                    panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx),
                    "Select owns the grip"
                );
                panel.cancel_handle_drag(cx);
            })
            .unwrap();
    }

    /// The manipulator exists only while its layer is the selected one, so a
    /// selection that moves elsewhere mid-drag reverts the preview instead of
    /// leaving it to be committed against a layer nobody is looking at.
    #[gpui::test]
    fn changing_the_layer_selection_reverts_a_shell_drag(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        let snapshot = project.read_with(cx, |project, _| project.document().clone());

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                let to = window_point(panel, (160.0, 215.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
                assert_ne!(shell_scale_of(panel, cx, comp_id, layer), (1.0, 1.0));
            })
            .unwrap();

        // Another panel selects something else while the button is still down.
        cx.update(|cx| crate::panels::set_layer_selection(Vec::new(), cx));
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.handle_drag.is_none(), "the gesture was released");
            })
            .unwrap();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "the preview was reverted, not left in the document"
        );

        // A mouse-up after the selection moved must not commit anything.
        window
            .update(cx, |panel, _window, cx| panel.handle_drag_ended(cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "the release found nothing to commit"
        );
    }

    // -----------------------------------------------------------------------
    // Motion path gestures (unit 9)
    // -----------------------------------------------------------------------

    /// The shell fixture with its `position` keyed from (100, 100) at frame 0 to
    /// (400, 300) at frame 60, so the motion path has a trajectory and two
    /// grabbable keys.
    fn motion_setup(
        cx: &mut TestAppContext,
    ) -> (
        WindowHandle<ViewerPanel>,
        Entity<ProjectState>,
        ravel_core::id::CompId,
        ravel_core::id::LayerId,
    ) {
        use ravel_core::animation::channel::AnimationChannel;

        let (window, project, comp_id, layer) = shell_setup(cx);
        project.update(cx, |project, cx| {
            let keyed = |from: f32, to: f32| {
                let mut curve = ravel_core::animation::KeyframeCurve::new();
                curve.insert(0, from, ravel_core::animation::Interpolation::Linear);
                curve.insert(60, to, ravel_core::animation::Interpolation::Linear);
                AnimationChannel::keyframes(curve)
            };
            let document =
                ravel_ui::document::update_layer(project.document(), comp_id, layer, |layer| {
                    layer.transform.position = [keyed(100.0, 400.0), keyed(100.0, 300.0)];
                })
                .unwrap();
            project.commit_document(document, InvalidationHint::None, cx);
        });
        (window, project, comp_id, layer)
    }

    /// The `position` keys of a layer, as `(frame, x, y)`.
    fn position_keys(
        project: &Entity<ProjectState>,
        comp: ravel_core::id::CompId,
        layer: ravel_core::id::LayerId,
        cx: &mut TestAppContext,
    ) -> Vec<(u64, f32, f32)> {
        use ravel_core::animation::channel::ChannelSource;

        project.read_with(cx, |project, _| {
            let position = &project
                .document()
                .get_composition(comp)
                .unwrap()
                .get_layer(layer)
                .unwrap()
                .transform
                .position;
            let curve = |channel: &ravel_core::animation::channel::AnimationChannel| match &channel
                .source
            {
                ChannelSource::Keyframes(curve) => curve.clone(),
                other => panic!("the channel was flattened to {other:?}"),
            };
            let (x, y) = (curve(&position[0]), curve(&position[1]));
            assert_eq!(x.len(), y.len(), "the components hold different key counts");
            x.keyframes()
                .iter()
                .zip(y.keyframes())
                .map(|(x, y)| {
                    assert_eq!(x.frame, y.frame, "the components keyed different frames");
                    (x.frame, x.value, y.value)
                })
                .collect()
        })
    }

    /// A whole key drag is one undo step, it writes both components at the
    /// grabbed key's own frame, and it leaves the other key alone.
    #[gpui::test]
    fn a_motion_key_drag_is_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = motion_setup(cx);
        assert_eq!(
            position_keys(&project, comp_id, layer, cx),
            vec![(0, 100.0, 100.0), (60, 400.0, 300.0)]
        );

        window
            .update(cx, |panel, _window, cx| {
                assert!(
                    panel.overlay_handle_mouse_down(&press_at(panel, (400.0, 300.0)), cx),
                    "the second key took the press"
                );
                assert_eq!(
                    panel.handle_drag.as_ref().map(|drag| drag.handle.id),
                    Some(overlay::OverlayHandleId::MotionKey(60)),
                    "another overlay answered a press meant for the key"
                );
                for x in [410.0, 425.0, 430.0] {
                    let to = window_point(panel, (x, 310.0));
                    panel.handle_dragged(to, DragModifiers::default(), cx);
                }
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            position_keys(&project, comp_id, layer, cx),
            vec![(0, 100.0, 100.0), (60, 430.0, 310.0)],
            "the gesture committed its last preview onto the grabbed key only"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            position_keys(&project, comp_id, layer, cx),
            vec![(0, 100.0, 100.0), (60, 400.0, 300.0)],
            "one undo covers the whole gesture, not just the last preview"
        );
    }

    /// The gesture belongs to the selection: a selection that moves elsewhere
    /// mid-drag reverts the preview rather than leaving it to be committed
    /// against a layer nobody is looking at.
    #[gpui::test]
    fn changing_the_layer_selection_reverts_a_motion_key_drag(cx: &mut TestAppContext) {
        let (window, project, ..) = motion_setup(cx);
        let snapshot = project.read_with(cx, |project, _| project.document().clone());

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, (400.0, 300.0)), cx));
                let to = window_point(panel, (430.0, 310.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
            })
            .unwrap();
        cx.update(|cx| crate::panels::set_layer_selection(Vec::new(), cx));
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.handle_drag.is_none(), "the gesture was released");
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "the preview was reverted, not left in the document"
        );
    }

    // -----------------------------------------------------------------------
    // Parameter manipulator gestures (REQ-UI-011 unit 5)
    // -----------------------------------------------------------------------

    /// The shell fixture with its rect node selected in the node editor, so
    /// the parameter manipulator has a node to put handles on. Its `center`
    /// handle sits at composition (100, 200) — the same point as the shell's
    /// move grip, which is what makes this also a test of the priority order.
    fn param_setup(
        cx: &mut TestAppContext,
    ) -> (
        WindowHandle<ViewerPanel>,
        Entity<ProjectState>,
        NetworkPath,
        NodeId,
    ) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        let network = NetworkPath::layer(comp_id, layer);
        let node = project.read_with(cx, |project, _| {
            ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the layer network")
                .node_ids()
                .next()
                .expect("the rect node")
        });
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(network.clone()),
                nodes: HashSet::from([node]),
            })
        });
        (window, project, network, node)
    }

    /// The `center` of `node` as the live document holds it, at frame 0.
    fn node_center(
        project: &Entity<ProjectState>,
        network: &NetworkPath,
        node: NodeId,
        cx: &mut TestAppContext,
    ) -> (f32, f32) {
        project.read_with(cx, |project, _| {
            let graph = ravel_ui::document::resolve_network(project.document(), network).unwrap();
            sample_vec2_param(graph.node(node).unwrap(), "center", 0, &eval_ctx()).unwrap()
        })
    }

    /// A whole parameter drag is one undo step, and the press it starts from
    /// is the node's handle rather than the shell grip drawn at the same point.
    #[gpui::test]
    fn a_param_handle_drag_is_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, network, node) = param_setup(cx);
        assert_eq!(node_center(&project, &network, node, cx), (100.0, 200.0));

        window
            .update(cx, |panel, _window, cx| {
                assert!(
                    panel.overlay_handle_mouse_down(&press_at(panel, (100.0, 200.0)), cx),
                    "the centre handle took the press"
                );
                assert_eq!(
                    panel.handle_drag.as_ref().map(|drag| drag.handle.id),
                    Some(overlay::OverlayHandleId::Param(0)),
                    "the shell's move grip answered a press meant for the node"
                );
                for x in [130.0, 150.0, 160.0] {
                    let to = window_point(panel, (x, 215.0));
                    panel.handle_dragged(to, DragModifiers::default(), cx);
                }
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            node_center(&project, &network, node, cx),
            (160.0, 215.0),
            "the last preview is what the gesture committed"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            node_center(&project, &network, node, cx),
            (100.0, 200.0),
            "one undo covers the whole gesture, not just the last preview"
        );
        // That single undo did not eat the layer: the next step back is its
        // creation, so the gesture really was one step.
        project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(network.comp)
                    .unwrap()
                    .layer_count(),
                1
            );
        });
    }

    /// The manipulator exists only while its node is the selected one, so a
    /// selection that moves elsewhere mid-drag reverts the preview instead of
    /// committing it against a node nobody is looking at.
    #[gpui::test]
    fn changing_the_node_selection_reverts_a_param_drag(cx: &mut TestAppContext) {
        let (window, project, network, node) = param_setup(cx);
        let snapshot = project.read_with(cx, |project, _| project.document().clone());

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, (100.0, 200.0)), cx));
                let to = window_point(panel, (160.0, 215.0));
                panel.handle_dragged(to, DragModifiers::default(), cx);
            })
            .unwrap();
        assert_ne!(node_center(&project, &network, node, cx), (100.0, 200.0));

        // Another panel selects something else while the button is still down.
        cx.update(|cx| cx.set_global(CanvasSelection::default()));
        cx.run_until_parked();

        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.handle_drag.is_none(), "the gesture was released");
            })
            .unwrap();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "the preview was reverted, not left in the document"
        );

        // A mouse-up after the selection moved must not commit anything.
        window
            .update(cx, |panel, _window, cx| panel.handle_drag_ended(cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            project.read_with(cx, |project, _| project.document().clone()),
            snapshot,
            "the release found nothing to commit"
        );
    }

    /// The scale of `layer` as the panel currently sees it, for assertions
    /// made from inside a `window.update`.
    fn shell_scale_of(
        panel: &ViewerPanel,
        cx: &mut Context<ViewerPanel>,
        comp_id: ravel_core::id::CompId,
        layer: ravel_core::id::LayerId,
    ) -> (f32, f32) {
        let project = panel.project(cx).unwrap();
        let document = project.read(cx).document().clone();
        let transform = &document
            .get_composition(comp_id)
            .unwrap()
            .get_layer(layer)
            .unwrap()
            .transform;
        (
            transform.scale[0].evaluate(0.0, &eval_ctx()),
            transform.scale[1].evaluate(0.0, &eval_ctx()),
        )
    }
}

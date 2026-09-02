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
pub mod guides;
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
use ravel_core::color::DisplayChannel;
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::composition::{Guide, GuideAxis};
use ravel_core::id::{CompId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameBuffer;
use ravel_gpu::GpuFrameBuffer;
use ravel_ui::document::NetworkPath;
use ravel_ui::panels::viewer::{PixelReadoutFormat, ViewerResolution, display_channel_label_key};
use viewport::ViewerViewport;

use super::param_edit::edited_vector_param;
use overlay::{
    ActiveDrag, BoxSelect, BoxSelectScope, DragModifiers, EvalResults, LabelPlacement,
    OverlayColors, OverlayContext, OverlayEdit, OverlayHandle, OverlayPainter, OverlayRegistry,
    ShellHandle,
};
use snap::{SnapGuides, SnapLines};

pub const KEY_CONTEXT: &str = "Viewer";

#[derive(Clone, Copy)]
struct PanDrag {
    pointer_start: (f32, f32),
    offset_start: (f32, f32),
}

/// Zoom is exponential in pointer travel: `current * exp(-dy * RATE)` for a
/// scroll of `dy` panel pixels. The Zoom tool's click names its own travel
/// ([`ZOOM_CLICK_TRAVEL`]) and multiplies through this same function, so the
/// tool lands on the wheel's ladder instead of introducing a second zoom scale.
const ZOOM_RATE_PER_PIXEL: f32 = 0.002;

/// The scroll travel one Zoom-tool click stands for: ten wheel notches of the
/// `px(20.0)` line height, i.e. `exp(0.4)` ≈ 1.49x in, 0.67x out.
const ZOOM_CLICK_TRAVEL: f32 = 200.0;

/// A press that travels less than this on *either* axis is a click, not a
/// rectangle. Without the floor a hand tremor becomes a two-pixel rectangle
/// blown up to fill the panel — a jump to `MAX_ZOOM` nobody asked for.
const ZOOM_RECT_MIN_PIXELS: f32 = 8.0;

fn zoom_factor(dy: f32) -> f32 {
    (-dy * ZOOM_RATE_PER_PIXEL).exp()
}

/// A Zoom-tool press. Which of the two gestures it is — a click zoom or a
/// rectangle zoom — is only known when the button comes back up, so the press
/// records and the release decides.
#[derive(Clone, Copy)]
struct ZoomDrag {
    /// Panel-local pixels, where the press landed. Also the click's anchor:
    /// the user aimed there, not wherever the pointer drifted to.
    start: (f32, f32),
    current: (f32, f32),
    /// `Alt` was held on the press, so a click zooms out.
    zoom_out: bool,
}

impl ZoomDrag {
    /// The dragged rectangle in panel-local pixels, or `None` while the
    /// gesture is still a click (see [`ZOOM_RECT_MIN_PIXELS`]).
    fn rect(self) -> Option<viewport::Rect> {
        let width = (self.current.0 - self.start.0).abs();
        let height = (self.current.1 - self.start.1).abs();
        (width >= ZOOM_RECT_MIN_PIXELS && height >= ZOOM_RECT_MIN_PIXELS).then_some(
            viewport::Rect {
                x: self.start.0.min(self.current.0),
                y: self.start.1.min(self.current.1),
                width,
                height,
            },
        )
    }
}

/// What a box-selection drag picks from, and what was already selected when it
/// started.
///
/// The two halves travel together because they have to match: a Shift drag
/// publishes the union of the sweep and the selection as it stood at the press,
/// and a union across selection models is not a thing. Capturing the start is
/// the whole point — `LOW-APP-03` is the Node Editor publishing the box's
/// contents alone and dropping what Shift promised to keep.
#[derive(Clone)]
enum BoxSelectTarget {
    Nodes {
        network: NetworkPath,
        initial: HashSet<NodeId>,
    },
    Layers {
        comp: CompId,
        initial: Vec<LayerId>,
    },
}

impl BoxSelectTarget {
    fn scope(&self) -> BoxSelectScope {
        match self {
            Self::Nodes { network, .. } => BoxSelectScope::Nodes(network.clone()),
            Self::Layers { comp, .. } => BoxSelectScope::Layers(*comp),
        }
    }

    /// The composition the candidates live in. A drag cannot outlive it being
    /// the one on screen.
    fn comp(&self) -> CompId {
        match self {
            Self::Nodes { network, .. } => network.comp,
            Self::Layers { comp, .. } => *comp,
        }
    }
}

/// A Select-tool press on empty space, sweeping a rectangle.
///
/// Nothing is decided until the release: the contents are recomputed there,
/// from the evaluation results as they stand at that moment, because the
/// candidate bboxes the drag asked for arrive asynchronously and the first
/// frames of the gesture can legitimately see none of them.
#[derive(Clone)]
struct BoxSelectGesture {
    /// Composition-space press point.
    start: (f32, f32),
    current: (f32, f32),
    /// Shift was held on the press, so the sweep adds to `target`'s capture.
    shift: bool,
    target: BoxSelectTarget,
}

impl BoxSelectGesture {
    /// The gesture as the overlays see it.
    fn live(&self) -> BoxSelect {
        BoxSelect {
            scope: self.target.scope(),
            rect: Some(box_rect(self.start, self.current)),
        }
    }
}

/// The active canvas tool. One reader for the Global, so every gesture asks
/// the same question the same way.
fn active_tool(cx: &App) -> ravel_ui::ToolKind {
    cx.try_global::<ToolState>()
        .map(|state| state.active)
        .unwrap_or_default()
}

/// The cursor a tool promises where nothing under the pointer claims its own —
/// the single place a tool becomes a hint, so the toolbar switch and the
/// pointer hit test can never disagree.
fn tool_pointer_hint(tool: ravel_ui::ToolKind) -> ViewerPointerHint {
    match tool {
        ravel_ui::ToolKind::Select => ViewerPointerHint::Empty,
        ravel_ui::ToolKind::Pen
        | ravel_ui::ToolKind::Rect
        | ravel_ui::ToolKind::Ellipse
        | ravel_ui::ToolKind::Polygon
        | ravel_ui::ToolKind::Star => ViewerPointerHint::Drawing,
        ravel_ui::ToolKind::Hand => ViewerPointerHint::Hand,
        ravel_ui::ToolKind::Zoom => ViewerPointerHint::Zoom,
    }
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
    Polygon,
    Star,
}

impl ShapeDrawKind {
    /// Exhaustive on purpose: a tool added without an answer here would
    /// silently draw nothing.
    fn from_tool(tool: ravel_ui::ToolKind) -> Option<Self> {
        match tool {
            ravel_ui::ToolKind::Rect => Some(Self::Rect),
            ravel_ui::ToolKind::Ellipse => Some(Self::Ellipse),
            ravel_ui::ToolKind::Polygon => Some(Self::Polygon),
            ravel_ui::ToolKind::Star => Some(Self::Star),
            ravel_ui::ToolKind::Select
            | ravel_ui::ToolKind::Pen
            | ravel_ui::ToolKind::Hand
            | ravel_ui::ToolKind::Zoom => None,
        }
    }

    fn type_key(self) -> &'static str {
        match self {
            Self::Rect => "shape.rect",
            Self::Ellipse => "shape.ellipse",
            Self::Polygon => "shape.polygon",
            Self::Star => "shape.star",
        }
    }

    /// Whether the shape is radially symmetric, and so drawn from its centre
    /// out rather than corner to corner (`TOOLX-4`).
    fn is_radial(self) -> bool {
        match self {
            Self::Rect | Self::Ellipse => false,
            Self::Polygon | Self::Star => true,
        }
    }
}

/// Drag-derived shape extents in comp space: `center` plus the half extents
/// (rect half width/height, ellipse radii, and for a radial shape the outer
/// radius in both components).
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

/// Shape-tool drag. The node is created on the first mouse move, not on
/// mouse-down, so a plain click leaves the document (and the selection)
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
    /// The Hand tool: the picture can be dragged from anywhere.
    Hand,
    /// The Zoom tool: click or drag out a rectangle.
    Zoom,
}

impl ViewerPointerHint {
    fn cursor(self) -> CursorStyle {
        match self {
            Self::Empty => CursorStyle::Arrow,
            // The Zoom tool aims at a point, like the drawing tools do.
            // GPUI-CE has no magnifier cursor and no custom bitmaps.
            Self::Drawing | Self::PathTangent | Self::Zoom => CursorStyle::Crosshair,
            // GPUI-CE has no generic `Move` cursor. OpenHand communicates the
            // same grab-to-move affordance and matches the Node Editor. The
            // Hand tool is the literal case: the press closes it into
            // `ClosedHand` through `viewer_drag_cursor`.
            Self::MovableBody | Self::Hand => CursorStyle::OpenHand,
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

/// The cursor a guide promises: it moves across itself and nowhere else, so the
/// glyph names the one axis the drag writes.
fn guide_hint(axis: GuideAxis) -> ViewerPointerHint {
    match axis {
        GuideAxis::Vertical => ViewerPointerHint::ResizeLeftRight,
        GuideAxis::Horizontal => ViewerPointerHint::ResizeUpDown,
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
    held_hint: Option<ViewerPointerHint>,
) -> Option<CursorStyle> {
    // A shell grip and a guide both keep the cursor the pointer showed before
    // the press: the gesture is the one the hover promised, so changing the
    // glyph mid-drag would only unsay it.
    if let Some(hint) = held_hint {
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

/// The preview resolution factor as the toolbar names it.
///
/// While the effective factor differs from the selected one — which only
/// `VRES-4`'s adaptive downgrade can cause — **both** are shown, so a coarse
/// preview is never read as the factor the user asked for. When they agree,
/// only one is shown: a permanent `1/2 → 1/2` would train the user to ignore
/// exactly the signal that matters.
///
/// The pair is one locale key with placeholders rather than a `format!` of
/// translated fragments, because the order of the two is a language's choice
/// (`docs/dev/add-locale.md`). Public for the same reason
/// `properties::read_only_value` is: the lib unit tests run with an empty
/// i18n store, so the coverage lives in the `localized_display_text` binary.
pub fn resolution_label(selected: ViewerResolution, effective: ViewerResolution) -> String {
    if effective == selected {
        return t!(selected.label_key());
    }
    t!("viewer.resolution_effective")
        .replace("{selected}", &t!(selected.label_key()))
        .replace("{effective}", &t!(effective.label_key()))
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
    /// through [`snap_target_for_handle`].
    snap: SnapTarget,
    /// Invalidation the applied edits ask for, committed with the gesture.
    invalidation: InvalidationHint,
    changed: bool,
}

/// A guide being dragged out of a ruler or moved.
///
/// Creating and moving are the same gesture: a guide dragged out of a ruler is
/// inserted into the preview document at press time, so from the first move on
/// there is only ever "the guide at this index is being placed".
#[derive(Clone)]
struct GuideDrag {
    comp: CompId,
    /// Index into `Composition::guides`.
    index: usize,
    axis: GuideAxis,
    /// The guide's position when the gesture pressed.
    origin: f32,
    pointer_start: (f32, f32),
    /// Candidates, with this guide left out — a line is always within zero of
    /// itself, so leaving it in would pin the drag to its start.
    lines: SnapLines,
    original_document: Document,
    /// Whether this gesture created the guide. A new guide released back over
    /// the ruler leaves the document exactly as it was rather than committing
    /// an undo step for a guide the user put back.
    created: bool,
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
    zoom_drag: Option<ZoomDrag>,
    move_drag: Option<MoveDrag>,
    box_select: Option<BoxSelectGesture>,
    /// This panel's identity, for the one piece of shared state a gesture of
    /// its own writes: the box-selection scope
    /// ([`Self::withdraw_box_select_candidates`]).
    instance: ravel_ui::layout::PanelInstanceId,
    shape_drag: Option<ShapeDrag>,
    pen_session: Option<PenSession>,
    handle_drag: Option<OverlayHandleDrag>,
    guide_drag: Option<GuideDrag>,
    /// The lines the drag in flight is snapped to. Read back only while a
    /// gesture is live (see [`Self::overlay_context`]), so a guide cannot
    /// outlive the correction it reports.
    snap_guides: SnapGuides,
    pointer_hint: ViewerPointerHint,
    /// The evaluated frame behind the picture, held only while the pixel
    /// readout is on (`INSP-3`). The readout indexes this; nothing evaluates
    /// or reads back when the pointer moves.
    linear: Option<Arc<FrameBuffer>>,
    /// Composition-space pointer position, tracked only while the readout is
    /// on. Off, the field stays `None` and a pointer move notifies nothing.
    readout_pointer: Option<(f32, f32)>,
    /// Proportional (3x3) grid overlay toggle.
    show_grid: bool,
    /// Action-safe (90%) / title-safe (80%) overlay toggle.
    show_safe_areas: bool,
    /// Ruler strips along the panel's top and left edges. Off by default, like
    /// every other piece of measuring chrome — and the only place a guide is
    /// dragged out of, so turning them on is what makes guides reachable.
    show_rulers: bool,
    /// Whether the composition's guides are drawn and snapped to. Session
    /// state, not document state: it is a view of the guides, the same class as
    /// the grid and safe-area toggles.
    show_guides: bool,
    /// Whether guides refuse to be dragged. Locking says "do not move these",
    /// so a locked guide is still drawn and still snapped to — it just cannot
    /// be created, moved or deleted with the pointer.
    guides_locked: bool,
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
    /// Repaints when the pixel readout's scale changes (`INSP-3`): the
    /// Global is the only thing that moves, so nothing else would. Held for
    /// its lifetime, like the neighbours — dropping it unsubscribes.
    #[allow(dead_code)]
    readout_format_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    layer_selection_sub: Subscription,
}

impl ViewerPanel {
    /// **Deliberately outside the panel visibility gate** (`MED-UI-02`,
    /// `docs/implementation/panel-visibility-plan.md`).
    ///
    /// The other document-mirroring panels delay their sync while their tab is
    /// behind another one, because a rebuilt row model nobody can see is pure
    /// cost. This panel is the exception: what it does on a notification is
    /// post evaluation requests, and those are what keep playback fed and the
    /// frame cache warm. Stopping them while the tab is in the background is
    /// not a saving, it is a feature that stops — the playhead would run
    /// against an empty cache and the first frame after a tab switch would
    /// have to be evaluated from scratch.
    ///
    /// The scopes (Waveform / Vectorscope / Histogram) are outside it for the
    /// opposite reason: they do not mirror the document at all yet. When one of
    /// them grows a mirror, it belongs on the gate.
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(instance, &focus_handle, window, cx);

        // The readout's scale is a Global written by the command
        // (`INSP-3`), and nothing else repaints for it: with the pointer
        // standing still, the chord would change the numbers only at the next
        // frame or the next mouse move.
        let readout_format_sub =
            cx.observe_global::<super::ViewerReadoutFormat>(|_this, cx| cx.notify());

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
            // The box selection follows the same rule: a deliberate switch away
            // from Select ends the sweep — leaving it live would keep declaring
            // candidate evaluation for a gesture no tool can finish — while the
            // `h` hold is transient navigation that gives the tool back.
            if this.box_select.is_some()
                && state.active != ravel_ui::ToolKind::Select
                && !state.hand_hold
            {
                this.cancel_box_select(cx);
            }
            this.pointer_hint = tool_pointer_hint(state.active);
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
            // A composition switch resets the layer selection
            // (`set_active_composition`), so this observer is where a Viewer
            // hears about it. A sweep whose candidates belong to a composition
            // that is no longer on screen has to end: the release already
            // refuses to publish into it, but the declaration would keep
            // evaluating a network nobody is looking at.
            //
            // The node scope needs nothing further. A layer deleted under a
            // live sweep leaves its network unresolvable, and the release then
            // finds no bboxes and publishes the empty selection a click on
            // nothing publishes — the same outcome, by the same rule.
            if this
                .box_select
                .as_ref()
                .is_some_and(|drag| Some(drag.target.comp()) != this.active_comp(cx))
            {
                this.cancel_box_select(cx);
            }
            this.request_overlay_eval(cx);
            cx.notify();
        });

        let viewer_sub = cx.observe_global::<ViewerFrame>(|this: &mut Self, cx| {
            let vf = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
            let content = viewer_content(vf);
            this.error = content.error;
            this.composition_resolution = content.composition_resolution;
            this.linear = content.linear;
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
            // A panel dropped mid-drag would otherwise leave the candidate
            // scope standing, and every later request would keep evaluating
            // geometry for a gesture nobody is making.
            if this.box_select.take().is_some() {
                this.withdraw_box_select_candidates(cx);
            }
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
            zoom_drag: None,
            move_drag: None,
            box_select: None,
            instance,
            shape_drag: None,
            pen_session: None,
            handle_drag: None,
            guide_drag: None,
            snap_guides: SnapGuides::default(),
            pointer_hint: ViewerPointerHint::default(),
            linear: content.linear,
            readout_pointer: None,
            show_grid: false,
            show_safe_areas: false,
            show_rulers: false,
            show_guides: true,
            guides_locked: false,
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
            readout_format_sub,
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

    /// Follow the pointer for the pixel readout (`INSP-3`).
    ///
    /// Only while the readout is on: with it off the field stays `None`, so a
    /// pointer move stores nothing and notifies nothing. Nothing here asks for
    /// an evaluation or a readback either — the values come from the frame
    /// this panel already holds, which is the whole reason the linear frame
    /// travels with the picture.
    ///
    /// The position is kept **unclamped** in composition space: a point beside
    /// the composition is a real place the pointer can be, and the readout has
    /// to answer "no value here" for it. A point outside the canvas area is
    /// not — a zoomed-in composition extends under the toolbar, and reporting
    /// pixels the panel is not drawing there would be reporting a place the
    /// user cannot see.
    fn track_readout_pointer(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let enabled = self
            .project(cx)
            .is_some_and(|project| project.read(cx).pixel_readout());
        let local = self.local_position(position);
        let (width, height) = self.viewport_size.get();
        let on_canvas = local.0 >= 0.0 && local.1 >= 0.0 && local.0 < width && local.1 < height;
        let next = (enabled && on_canvas)
            .then(|| self.comp_position(position))
            .flatten();
        if self.readout_pointer != next {
            self.readout_pointer = next;
            cx.notify();
        }
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

    /// What the left button means, decided in one place from the active tool.
    ///
    /// Hand and Zoom own the pointer outright, so a press under them reaches
    /// neither an overlay handle nor a guide, a selection, a shape drag or the
    /// pen. The `match` is exhaustive on purpose: another tool cannot be
    /// added without answering here.
    ///
    /// The Pen's point insertion / removal comes before the overlay handles:
    /// under that tool a press on an existing anchor *removes* it, so the
    /// handle drag the same anchor offers must not answer first.
    fn left_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        match active_tool(cx) {
            ravel_ui::ToolKind::Hand => self.pan_mouse_down(event, cx),
            ravel_ui::ToolKind::Zoom => self.zoom_mouse_down(event, cx),
            ravel_ui::ToolKind::Select
            | ravel_ui::ToolKind::Pen
            | ravel_ui::ToolKind::Rect
            | ravel_ui::ToolKind::Ellipse
            | ravel_ui::ToolKind::Polygon
            | ravel_ui::ToolKind::Star => {
                if !self.path_point_edit_mouse_down(event, cx)
                    && !self.overlay_handle_mouse_down(event, cx)
                    && !self.guide_mouse_down(event, cx)
                {
                    self.select_mouse_down(event, cx);
                    self.shape_mouse_down(event, cx);
                    self.pen_mouse_down(event, cx);
                }
            }
        }
    }

    /// Follow the pointer with whichever left-button gesture is live.
    ///
    /// The navigation drags come first: `pan_drag` and `zoom_drag` are only
    /// ever set by the middle button or by Hand / Zoom, so while one of them
    /// owns the pointer nothing else may start.
    fn left_dragged(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.pan_dragged(event.position, cx) || self.zoom_dragged(event.position, cx) {
            return;
        }
        if self.guide_drag.is_some() {
            self.guide_dragged(event.position, drag_modifiers(&event.modifiers), cx);
        } else if self.handle_drag.is_some() {
            self.handle_dragged(event.position, drag_modifiers(&event.modifiers), cx);
        } else if self
            .pen_session
            .as_ref()
            .is_some_and(|session| session.active_point.is_some())
        {
            self.pen_dragged(event.position, cx);
        } else if self.shape_drag.is_some() {
            self.shape_dragged(event, cx);
        } else if self.box_select.is_some() {
            self.box_select_dragged(event.position, cx);
        } else {
            self.move_dragged(event.position, drag_modifiers(&event.modifiers), cx);
        }
    }

    /// Begin a pan. The Hand tool's left press and the middle button — which
    /// pans under any tool — are the same gesture, so the temporary hand
    /// (`h` held), the toolbar hand and the middle button share this entry
    /// point and the one [`PanDrag`] state.
    fn pan_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.pen_point_ended(cx);
        let Some(resolution) = self.composition_resolution else {
            return;
        };
        let pointer_start = self.local_position(event.position);
        let offset_start = self
            .viewport
            .begin_pan(self.viewport_size.get(), resolution);
        self.pan_drag = Some(PanDrag {
            pointer_start,
            offset_start,
        });
        cx.notify();
    }

    /// Follow the pointer while panning. Reports whether the pan owns the
    /// drag, so the left button's gesture chain stops before the move tool.
    fn pan_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.pan_drag else {
            return false;
        };
        let pointer = self.local_position(position);
        self.viewport.set_offset((
            drag.offset_start.0 + pointer.0 - drag.pointer_start.0,
            drag.offset_start.1 + pointer.1 - drag.pointer_start.1,
        ));
        cx.notify();
        true
    }

    fn pan_ended(&mut self, cx: &mut Context<Self>) {
        if self.pan_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Zoom tool press: record it. Nothing zooms yet — the release decides
    /// whether this was a click or a rectangle.
    fn zoom_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.pen_point_ended(cx);
        let start = self.local_position(event.position);
        self.zoom_drag = Some(ZoomDrag {
            start,
            current: start,
            zoom_out: event.modifiers.alt,
        });
        cx.notify();
    }

    /// Size the zoom rectangle. Reports whether the zoom owns the drag.
    fn zoom_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let current = self.local_position(position);
        let Some(drag) = self.zoom_drag.as_mut() else {
            return false;
        };
        drag.current = current;
        cx.notify();
        true
    }

    /// The release decides which zoom the press was: the dragged rectangle if
    /// there is one, otherwise a click at the press point — `Alt` held on the
    /// press zooms out. Both go through the viewport's anchored zoom on the
    /// wheel's own multiplier ladder ([`zoom_factor`]).
    fn zoom_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.zoom_drag.take() else {
            return;
        };
        cx.notify();
        let Some(resolution) = self.composition_resolution else {
            return;
        };
        let panel = self.viewport_size.get();
        if let Some(rect) = drag.rect() {
            self.viewport.zoom_to_rect(rect, panel, resolution);
            return;
        }
        let travel = if drag.zoom_out {
            ZOOM_CLICK_TRAVEL
        } else {
            -ZOOM_CLICK_TRAVEL
        };
        let requested = self.viewport.zoom(panel, resolution) * zoom_factor(travel);
        self.viewport
            .zoom_toward(requested, drag.start, panel, resolution);
    }

    fn select_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if active_tool(cx) != ravel_ui::ToolKind::Select {
            return;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return;
        };
        // Several selected layers: no network is open, so there is nothing to
        // pick inside one — the gesture moves the selected layers instead.
        if super::layer_selection(cx).layers().len() >= 2 {
            self.layer_move_mouse_down(pointer, cx);
            // The press landed outside every selected layer, so there is
            // nothing to move: the drag sweeps a rectangle over the
            // composition's layers instead.
            if self.move_drag.is_none() {
                self.begin_layer_box_select(pointer, event.modifiers.shift, cx);
            }
            return;
        }
        let Some(selection) = cx.try_global::<CanvasSelection>().cloned() else {
            return;
        };
        let Some(network) = selection.path.clone() else {
            // No network is open, so the layers are what a rectangle picks.
            self.begin_layer_box_select(pointer, event.modifiers.shift, cx);
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

        if hit.is_none() {
            // Empty space inside the open network: sweep a rectangle over its
            // nodes. `selection.nodes` is the set as it stood *before* the
            // publish above, which is what a Shift sweep has to keep — the
            // click it published for a press on nothing is an empty selection.
            self.begin_box_select(
                pointer,
                BoxSelectTarget::Nodes {
                    network,
                    initial: selection.nodes.clone(),
                },
                event.modifiers.shift,
                cx,
            );
            return;
        }
        if event.modifiers.shift || !shell.is_identity() {
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
                lines: SnapLines::collect(&overlay_ctx, Some(network.comp), &[network.layer], None),
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
                lines: SnapLines::collect(&overlay_ctx, Some(comp_id), &moving, None),
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

    /// Start a layer-scope box selection: no network is open (or none can be
    /// picked inside), so the composition's layers are the candidates.
    fn begin_layer_box_select(&mut self, pointer: (f32, f32), shift: bool, cx: &mut Context<Self>) {
        let Some(comp) = self.active_comp(cx) else {
            return;
        };
        let selection = super::layer_selection(cx);
        // A selection stamped with another composition cannot be added to.
        let initial = if selection.comp() == Some(comp) {
            selection.layers().to_vec()
        } else {
            Vec::new()
        };
        // The press is a click on empty space, and a click on empty space
        // deselects — what `selection_after_click` does for the node scope,
        // done here for the layer scope so a zero-distance drag means the same
        // thing in both. The capture above is deliberately *before* this: a
        // Shift sweep publishes the union with what was selected when the press
        // landed, which is the `LOW-APP-03` trap.
        //
        // `set_layer_selection` drops a Properties target left pointing at the
        // cleared layers, so nothing else has to be published here.
        if !selection.is_empty() {
            super::set_layer_selection(Vec::new(), cx);
        }
        self.begin_box_select(
            pointer,
            BoxSelectTarget::Layers { comp, initial },
            shift,
            cx,
        );
    }

    /// Record the press and ask for the candidate bboxes.
    ///
    /// This gesture posts its own request here and **never again**: the
    /// candidate set is fixed for the whole drag, so
    /// [`OverlayRegistry::eval_targets`] keeps returning the same list and a
    /// request per pointer move would buy nothing but work. That is the
    /// promise — not that the press enqueues exactly one request in total,
    /// which the paragraph below qualifies. The
    /// hint is `None` because the document did not change: every value the
    /// composition already evaluated stays cached and the candidates are the
    /// only work this adds.
    ///
    /// The press also publishes a selection, whose observer posts a request of
    /// its own — but **only when the selection actually changed**
    /// ([`Self::request_overlay_eval`] returns early otherwise), and a press on
    /// empty space with nothing selected changes nothing. That is the box
    /// selection's main case, so this request cannot be left to the observer.
    /// When both do fire, the second is free: the worker is latest-wins, so two
    /// requests carrying the same hint coalesce into one evaluation.
    fn begin_box_select(
        &mut self,
        pointer: (f32, f32),
        target: BoxSelectTarget,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        // This panel becomes the owner, taking the declaration over from
        // another instance if one is still holding it: the pointer is one, so a
        // drag standing elsewhere has lost its release and is stale. Refusing
        // to start would strand this Viewer until the stale one is nudged;
        // taking over is self-healing — the stale gesture is dropped by its own
        // panel on its next idle pointer move, and the owner check below stops
        // it withdrawing what this drag declared.
        cx.set_global(overlay::BoxSelectDrag(Some(overlay::LiveBoxSelect {
            panel: self.instance,
            scope: target.scope(),
        })));
        self.box_select = Some(BoxSelectGesture {
            start: pointer,
            current: pointer,
            shift,
            target,
        });
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.request_viewer_eval(InvalidationHint::None, cx);
            });
        }
        cx.notify();
    }

    /// Size the marquee. Nothing is selected yet: the release decides.
    fn box_select_dragged(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        let Some(drag) = self.box_select.as_mut() else {
            return;
        };
        drag.current = pointer;
        cx.notify();
    }

    /// The release publishes what the rectangle caught.
    ///
    /// Recomputed here rather than accumulated during the drag: the candidate
    /// bboxes arrive asynchronously, so the frames the moves saw may have had
    /// fewer of them than this frame does.
    fn box_select_ended(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.box_select.take() else {
            return;
        };
        self.withdraw_box_select_candidates(cx);
        cx.notify();
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        // A press that never travelled is a click, and the press already
        // published what a click means. Publishing a union here would put the
        // selection the click cleared straight back — but a click that grabbed
        // nothing is the one chance to look in the other layers.
        if pointer == drag.start {
            self.resolve_hit_fallback(pointer, &drag.target, cx);
            return;
        }
        let rect = box_rect(drag.start, pointer);
        let ctx = self.overlay_context(cx);
        match &drag.target {
            BoxSelectTarget::Nodes { network, initial } => {
                let inside = nodes_in_box(&ctx, network, rect);
                Self::publish_selection(
                    network.clone(),
                    nodes_after_box(initial, inside, drag.shift),
                    cx,
                );
            }
            BoxSelectTarget::Layers { comp, initial } => {
                // `set_layer_selection` stamps the *active* composition, so a
                // switch mid-drag would file these ids under a composition
                // they do not belong to.
                if self.active_comp(cx) != Some(*comp) {
                    return;
                }
                let inside = layers_in_box(&ctx, *comp, rect);
                super::set_layer_selection(layers_after_box(initial, &inside, drag.shift), cx);
                super::publish_layer_properties_target(cx);
            }
        }
    }

    /// A click that grabbed nothing where it landed: pick the topmost shape
    /// node under it from the composition's **other** layers (REQ-UI-011's
    /// v1.5 fallback).
    ///
    /// Only reached from a zero-distance release of a box selection, which is
    /// the one press that found nothing to grab — so the fallback is scoped to
    /// "the active layer missed" without a condition of its own. The candidate
    /// bboxes were declared by that same press ([`Self::begin_box_select`]),
    /// which is why this can measure layers no selection ever asked for.
    ///
    /// Selecting a node moves the layer selection onto its layer, the way the
    /// Outliner's node rows do: the network the selection names is the one the
    /// next press hit-tests, and leaving several layers selected would send it
    /// straight back into the multi-layer gesture.
    ///
    /// **The results arrive asynchronously**, so a click released before the
    /// evaluation lands falls back to nothing. Same known limitation as the
    /// box selection's first frames.
    fn resolve_hit_fallback(
        &mut self,
        pointer: (f32, f32),
        target: &BoxSelectTarget,
        cx: &mut Context<Self>,
    ) {
        let (comp, active) = match target {
            // The open network was already tested by the press. Its layer is
            // skipped rather than tested again: "another layer" is what the
            // requirement asks for, and re-testing a layer whose subnet is
            // open would silently close it.
            BoxSelectTarget::Nodes { network, .. } => (network.comp, Some(network.layer)),
            // No network was open, so no layer was active and every one of
            // them is a candidate.
            BoxSelectTarget::Layers { comp, .. } => (*comp, None),
        };
        // `set_layer_selection` stamps the *active* composition, so a switch
        // mid-gesture would file this layer under one it does not belong to.
        if self.active_comp(cx) != Some(comp) {
            return;
        }
        let ctx = self.overlay_context(cx);
        let Some((network, node)) = hit_test_other_layers(&ctx, comp, active, pointer) else {
            return;
        };
        super::set_layer_selection(vec![network.layer], cx);
        Self::publish_selection(network, HashSet::from([node]), cx);
        cx.notify();
    }

    /// Drop the gesture without publishing anything (Escape, a deliberate tool
    /// switch, a composition switch, or the pointer turning up under another
    /// button).
    fn cancel_box_select(&mut self, cx: &mut Context<Self>) {
        if self.box_select.take().is_some() {
            self.withdraw_box_select_candidates(cx);
            cx.notify();
        }
    }

    /// Stop declaring this panel's box-select candidates — **only if this panel
    /// still owns the declaration**.
    ///
    /// Several Viewers may be open (REQ-UI-005) and they share one Global, so an
    /// unconditional clear would let one instance ending its gesture stop the
    /// evaluation another instance's live drag is waiting for. That happens for
    /// real: a release this window never received leaves a gesture standing
    /// here while the next press starts one, and takes the ownership, in
    /// another Viewer.
    fn withdraw_box_select_candidates(&self, cx: &mut App) {
        let owned = cx
            .try_global::<overlay::BoxSelectDrag>()
            .and_then(|drag| drag.0.as_ref())
            .is_some_and(|live| live.panel == self.instance);
        if owned {
            cx.set_global(overlay::BoxSelectDrag(None));
        }
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

    /// Shape-tool mouse-down: record the pending drag. Nothing is created yet
    /// — a click without a drag must not touch the document.
    fn shape_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(kind) = ShapeDrawKind::from_tool(active_tool(cx)) else {
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
        let snap_lines = self.snap_lines(comp, &drawn_into, None, cx);
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
        //
        // A radial shape snaps nothing for the same reason: its centre is
        // pinned at the press and no edge of it follows the pointer, so a
        // correction there would only lie about the radius the drag asked for.
        let corner = (!modifiers.shift && !drag.kind.is_radial()).then(|| point_rect(pointer));
        let snapped = self.snapped_delta(&drag.snap_lines, corner, (0.0, 0.0), modifiers);
        let pointer = (pointer.0 + snapped.0, pointer.1 + snapped.1);
        let geo = if drag.kind.is_radial() {
            radial_drag_geometry(drag.start, pointer)
        } else {
            drag_geometry(
                drag.start,
                pointer,
                event.modifiers.shift,
                event.modifiers.alt,
            )
        };
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
                    None,
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
        self.move_drag.is_some()
            || self.shape_drag.is_some()
            || self.handle_drag.is_some()
            || self.guide_drag.is_some()
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

    /// The snap candidates a gesture over `comp` sees, with the layers and the
    /// guide it moves left out.
    fn snap_lines(
        &self,
        comp: Option<CompId>,
        moving: &[LayerId],
        moving_guide: Option<usize>,
        cx: &App,
    ) -> SnapLines {
        SnapLines::collect(&self.overlay_context(cx), comp, moving, moving_guide)
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
                    .map(|project| {
                        project
                            .read(cx)
                            .effective_viewer_resolution()
                            .apply(resolution)
                    })
                    .unwrap_or(resolution)
            }),
            comp: self.active_comp(cx),
            playback: cx.try_global::<super::PlaybackPosition>().copied(),
            document: self
                .project(cx)
                .map(|project| project.read(cx).document().clone()),
            selection: cx.try_global::<CanvasSelection>().cloned(),
            layer_selection: super::layer_selection(cx),
            tool: cx.try_global::<ToolState>().map(|state| state.active),
            show_grid: self.show_grid,
            show_safe_areas: self.show_safe_areas,
            show_guides: self.show_guides,
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
            // From the panel's own gesture, not from the Global that carries
            // the scope to the request path: with two Viewer instances open,
            // the marquee belongs to the one the pointer is in.
            box_select: self.box_select.as_ref().map(BoxSelectGesture::live),
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
            results: cx.try_global::<EvalResults>().cloned().unwrap_or_default(),
            registry: self
                .project(cx)
                .map(|project| project.read(cx).shared_registry()),
            // All three or nothing (`INSP-3`): the readout is on, a pointer
            // has been seen, and the frame on screen brought its evaluated
            // source along. A frame published while the readout was off has no
            // `linear`, so the readout stays quiet for the one frame between
            // switching it on and the re-finalized frame arriving.
            // Both written outside this panel and read without an observer of
            // their own (`INSP-4`): the status line only changes on frames the
            // viewer is already repainting for.
            playback_status: super::playback_status(cx),
            // The selection and the factor in force, read from the one place
            // that owns both (`ProjectState`). Their *difference* is what the
            // status line reports, so they travel as a pair.
            preview_factors: project.as_ref().map(|project| {
                let project = project.read(cx);
                (
                    project.viewer_resolution(),
                    project.effective_viewer_resolution(),
                )
            }),
            cached_frames: super::cache_band(cx),
            pixel_readout: self
                .readout_pointer
                .zip(self.linear.clone())
                .map(|(pointer, frame)| overlay::PixelReadout {
                    pointer,
                    frame,
                    format: cx
                        .try_global::<super::ViewerReadoutFormat>()
                        .copied()
                        .unwrap_or_default()
                        .0,
                }),
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
        let tool = active_tool(cx);
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

        // Only where a press would actually do something: hidden or locked
        // guides promise nothing, and neither does a ruler strip that cannot
        // drag one out.
        if tool == ravel_ui::ToolKind::Select && self.show_guides && !self.guides_locked {
            if self.show_rulers
                && let Some(axis) =
                    guides::ruler_axis(self.local_position(position), self.viewport_size.get())
            {
                return Some(guide_hint(axis));
            }
            if let Some(axis) = self.guide_axis_at(pointer, cx) {
                return Some(guide_hint(axis));
            }
        }

        if tool == ravel_ui::ToolKind::Select && self.selected_body_contains(pointer, cx) {
            return Some(ViewerPointerHint::MovableBody);
        }

        Some(tool_pointer_hint(tool))
    }

    /// The composition rectangle in composition units — the very extent
    /// [`OverlayPainter`] is handed, so the length of a drawn guide and the
    /// length of its hit region come from one value.
    fn comp_extent(&self) -> Option<(f32, f32)> {
        self.composition_resolution
            .map(|(width, height)| (width as f32, height as f32))
    }

    /// Which way the guide under `pointer` runs, if one is in reach. The same
    /// hit test the press uses, so the cursor promises exactly what a press
    /// would grab.
    fn guide_axis_at(&self, pointer: (f32, f32), cx: &App) -> Option<GuideAxis> {
        let comp = self.active_comp(cx)?;
        let project = self.project(cx)?;
        let document = project.read(cx).document();
        let composition = document.get_composition(comp)?;
        let threshold = guides::GUIDE_HIT_PX * self.comp_per_pixel();
        let index = guides::guide_at(&composition.guides, pointer, threshold, self.comp_extent()?)?;
        Some(composition.guides[index].axis)
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

    /// Pen-tool press on the selected path: a press on one of its points
    /// removes that point, a press on one of its segments inserts one there.
    /// Reports whether the press was taken.
    ///
    /// The Pen tool alone, so the Select tool's handle drags are untouched —
    /// a click that moved a point by zero must not delete it. This is the
    /// pen's own vocabulary (a press on the path edits the path, a press
    /// beside it starts a new one), and it is why the press is answered here
    /// rather than through an overlay handle: removal has no drag.
    ///
    /// A click, not a gesture: there is nothing to preview, so the edit is
    /// committed at once and is one undo step by construction.
    ///
    /// Priority under the pointer: **a tangent handle, then the removal, then
    /// the insertion.** A tangent's mark is drawn from the anchor, so a short
    /// arm shares the anchor's grab radius and the press has to go to the arm
    /// — a point with an arm shorter than the radius is therefore not
    /// removable until the arm is pulled out, which is the cost of two marks
    /// inside one radius. A point with no arms at all (the pen's plain click)
    /// draws no tangent handle and stays removable.
    fn path_point_edit_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        // A live pen session owns the pointer: its clicks extend the path it
        // is drawing rather than edit the points already placed.
        if active_tool(cx) != ravel_ui::ToolKind::Pen || self.pen_session.is_some() {
            return false;
        }
        let (Some(pointer), Some(resolution)) = (
            self.comp_position(event.position),
            self.composition_resolution,
        ) else {
            return false;
        };
        let (Some(position), Some(selection)) = (
            cx.try_global::<super::PlaybackPosition>().copied(),
            cx.try_global::<CanvasSelection>().cloned(),
        ) else {
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
        let document = project.read(cx).document().clone();
        let Some(path) = selected_path_overlay(
            &selection,
            &document,
            position.frame,
            position.fps,
            resolution,
        ) else {
            return false;
        };
        // The edit writes node-local coordinates, so the comp-space pointer is
        // only the same place under an identity shell — the restriction the
        // path handles already carry (REQ-UI-011 leaves the transformed case
        // to v2).
        if !path.shell_identity {
            return false;
        }
        // A **tangent** handle answers before the removal does. Its mark is
        // drawn from the anchor, so a short arm sits inside the very radius
        // the removal reaches with, and a press meant to bend the curve would
        // delete the point instead — which is what a point the pen placed by
        // clicking (no drag, so no arm at all until one is pulled out) is made
        // of. The anchor's own handle deliberately does *not* win: a press on
        // a point is a removal under the Pen. Asked through the registry's
        // hit test, the same one the press below and the cursor already use,
        // so there is no second notion of what the pointer is over.
        let context = self.overlay_context(cx);
        if matches!(
            OverlayRegistry::builtin()
                .hit_test_draggable(&context, pointer, self.comp_per_pixel())
                .and_then(|handle| handle.id.path_handle_kind()),
            Some(PathHandleKind::InTangent | PathHandleKind::OutTangent)
        ) {
            return false;
        }
        let radius = self
            .comp_hit_radius(overlay::PathEditOverlay::HIT_RADIUS_PX)
            .unwrap_or(overlay::PathEditOverlay::HIT_RADIUS_PX);
        let points = match path_point_at(&path.points, pointer, radius) {
            Some(index) => match path_without_point(&path.points, index) {
                Some(points) => points,
                // The press was on the path, so it is taken either way: a
                // refusal is this gesture's answer, not a pass to the next
                // one. Falling through would hand the press to the anchor's
                // own handle — the same 8px reach found it — and turn "this
                // path cannot lose a point" into a silent move under the Pen.
                None => return true,
            },
            None => match path_with_inserted_point(&path.points, path.closed, pointer, radius) {
                Some(points) => points,
                // Not on the path at all: the pen's own gestures get the press.
                None => return false,
            },
        };
        if self.apply_path_points(&network, node, points, path.closed, cx)
            && let Some(project) = self.project(cx)
        {
            project.update(cx, |project, cx| {
                project.commit_document(
                    project.document().clone(),
                    InvalidationHint::Params(vec![node]),
                    cx,
                );
            });
        }
        true
    }

    fn pen_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if active_tool(cx) != ravel_ui::ToolKind::Pen {
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

    /// The composition the Viewer is showing.
    fn active_comp(&self, cx: &App) -> Option<CompId> {
        self.project(cx)?
            .read(cx)
            .active_composition(cx)
            .map(|c| c.id)
    }

    /// Press on a ruler strip (drag out a new guide) or on a guide (move it).
    ///
    /// Returns whether the press was taken. Guides are a `Select`-tool gesture:
    /// the ruler strips lie over the canvas area, and a drawing tool's press
    /// there belongs to the drawing tool.
    ///
    /// This is a panel branch rather than an [`OverlayHandle`] because neither
    /// half fits one: a ruler is panel chrome outside the composition rectangle
    /// the overlay painter knows, and a guide is a line, which the registry's
    /// radius-around-a-point hit test cannot express.
    fn guide_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        if !self.show_guides || self.guides_locked || self.pen_session.is_some() {
            return false;
        }
        if active_tool(cx) != ravel_ui::ToolKind::Select {
            return false;
        }
        let Some(pointer) = self.comp_position(event.position) else {
            return false;
        };
        let Some(comp_id) = self.active_comp(cx) else {
            return false;
        };
        let Some(project) = self.project(cx) else {
            return false;
        };
        let document = project.read(cx).document().clone();
        let Some(composition) = document.get_composition(comp_id) else {
            return false;
        };
        let existing = composition.guides.len();

        // The ruler wins over the guides under it: it is drawn on top, and it
        // is the only way to make a new one.
        let ruler = self.show_rulers.then(|| {
            guides::ruler_axis(
                self.local_position(event.position),
                self.viewport_size.get(),
            )
        });
        let (index, axis, origin, created) = match ruler.flatten() {
            Some(axis) => {
                let origin = match axis {
                    GuideAxis::Vertical => pointer.0,
                    GuideAxis::Horizontal => pointer.1,
                };
                (existing, axis, origin, true)
            }
            None => {
                let threshold = guides::GUIDE_HIT_PX * self.comp_per_pixel();
                let Some(extent) = self.comp_extent() else {
                    return false;
                };
                let Some(index) = guides::guide_at(&composition.guides, pointer, threshold, extent)
                else {
                    return false;
                };
                let guide = composition.guides[index];
                (index, guide.axis, guide.position, false)
            }
        };

        // A gesture that has not moved yet has corrected nothing: the previous
        // one's guide must not survive into this frame.
        self.snap_guides = SnapGuides::default();
        self.guide_drag = Some(GuideDrag {
            comp: comp_id,
            index,
            axis,
            origin,
            pointer_start: pointer,
            lines: self.snap_lines(Some(comp_id), &[], Some(index), cx),
            original_document: document,
            created,
            changed: false,
        });
        if created && let Some(drag) = self.guide_drag.clone() {
            self.write_guide(
                &drag,
                Some(Guide {
                    axis,
                    position: origin,
                }),
                cx,
            );
        }
        true
    }

    fn guide_dragged(
        &mut self,
        position: Point<Pixels>,
        modifiers: DragModifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.guide_drag.clone() else {
            return;
        };
        let Some(pointer) = self.comp_position(position) else {
            return;
        };
        let raw = (
            pointer.0 - drag.pointer_start.0,
            pointer.1 - drag.pointer_start.1,
        );
        // A guide moves across itself and nowhere else, so the correction on the
        // other axis is discarded — and with it the guide line that would
        // otherwise name an alignment this gesture cannot make.
        let (rect, axes) = match drag.axis {
            GuideAxis::Vertical => (point_rect((drag.origin, 0.0)), (true, false)),
            GuideAxis::Horizontal => (point_rect((0.0, drag.origin)), (false, true)),
        };
        let snapped = self
            .snap_result(&drag.lines, Some(rect), raw, modifiers)
            .restrict(raw, axes);
        self.snap_guides = snapped.guides;
        let delta = match drag.axis {
            GuideAxis::Vertical => snapped.delta.0,
            GuideAxis::Horizontal => snapped.delta.1,
        };
        let position = drag.origin + delta;
        if !self.write_guide(
            &drag,
            Some(Guide {
                axis: drag.axis,
                position,
            }),
            cx,
        ) {
            return;
        }
        if let Some(active) = &mut self.guide_drag {
            active.changed = position != drag.origin;
        }
    }

    /// Release: a guide dropped back over a ruler is deleted, and one dropped on
    /// the picture stays. Either way the whole gesture is one undo step, and a
    /// gesture that changed nothing commits none.
    fn guide_drag_ended(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.guide_drag.take() else {
            return;
        };
        let over_ruler = self.show_rulers
            && guides::ruler_axis(self.local_position(position), self.viewport_size.get())
                .is_some();
        if over_ruler {
            // A guide that never existed outside this gesture leaves nothing
            // behind: restore rather than commit a create-then-delete pair.
            if drag.created {
                self.restore_document(drag.original_document, cx);
                return;
            }
            if !self.write_guide(&drag, None, cx) {
                return;
            }
        } else if !drag.created && !drag.changed {
            // A guide put back where it started is not an undo step. The
            // preview already matches the committed snapshot, but applying it
            // marked the store dirty, so drop that — committing it would push
            // an identical version, and leaving it would cost the next undo on
            // nothing. Reverting keeps whatever another panel committed
            // meanwhile, which rolling back to the press-time snapshot would
            // discard.
            if let Some(project) = self.project(cx) {
                project.update(cx, |project, cx| {
                    project.revert_document(cx);
                });
            }
            cx.notify();
            return;
        }
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.commit_document(project.document().clone(), InvalidationHint::None, cx);
            });
        }
        cx.notify();
    }

    fn cancel_guide_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.guide_drag.take() else {
            return;
        };
        self.restore_document(drag.original_document, cx);
    }

    /// Write the dragged guide into the preview document, or remove it when
    /// `guide` is `None`. Guides drive no evaluation, so the hint is `None`.
    fn write_guide(
        &mut self,
        drag: &GuideDrag,
        guide: Option<Guide>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(project) = self.project(cx) else {
            return false;
        };
        let mut applied = false;
        project.update(cx, |project, cx| {
            let Some(document) = ravel_ui::document::update_composition(
                project.document(),
                drag.comp,
                |mut comp| {
                    match guide {
                        Some(guide) if drag.index < comp.guides.len() => {
                            comp.guides[drag.index] = guide;
                        }
                        Some(guide) => comp.guides.push(guide),
                        None if drag.index < comp.guides.len() => {
                            comp.guides.remove(drag.index);
                        }
                        None => {}
                    }
                    comp
                },
            ) else {
                return;
            };
            project.apply_document(document, InvalidationHint::None, cx);
            applied = true;
        });
        if applied {
            cx.notify();
        }
        applied
    }

    fn restore_document(&mut self, snapshot: Document, cx: &mut Context<Self>) {
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.restore_document_snapshot(snapshot, cx);
            });
        }
        cx.notify();
    }

    /// Drop every guide of the composition on screen, as one undo step.
    ///
    /// Refused while the guides are locked: clearing is a deletion, and the
    /// lock forbids deletions however they are reached — the menu is not a way
    /// around the pointer's refusal.
    fn clear_guides(&mut self, cx: &mut Context<Self>) {
        if self.guides_locked {
            return;
        }
        let Some(comp) = self.active_comp(cx) else {
            return;
        };
        let Some(project) = self.project(cx) else {
            return;
        };
        project.update(cx, |project, cx| {
            // Nothing to clear is not an undo step.
            if project
                .document()
                .get_composition(comp)
                .is_none_or(|composition| composition.guides.is_empty())
            {
                return;
            }
            let Some(document) =
                ravel_ui::document::update_composition(project.document(), comp, |mut comp| {
                    comp.guides.clear();
                    comp
                })
            else {
                return;
            };
            project.commit_document(document, InvalidationHint::None, cx);
        });
        cx.notify();
    }

    fn tool_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let active = cx
            .try_global::<ToolState>()
            .map(|s| s.active)
            .unwrap_or_default();

        const TOOLS: [ravel_ui::ToolKind; 8] = [
            ravel_ui::ToolKind::Select,
            ravel_ui::ToolKind::Pen,
            ravel_ui::ToolKind::Rect,
            ravel_ui::ToolKind::Ellipse,
            ravel_ui::ToolKind::Polygon,
            ravel_ui::ToolKind::Star,
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

    /// Point [`ProjectState`] at a preview resolution factor.
    ///
    /// The factor lives there and nowhere else: it is what the evaluation
    /// request is built from, so a panel-local copy would be a second source
    /// of truth for the same setting. The notify is this panel's own — it does
    /// not observe the project entity, and the toolbar label reads the factor.
    fn set_preview_resolution(&mut self, resolution: ViewerResolution, cx: &mut Context<Self>) {
        let Some(project) = self.project(cx) else {
            return;
        };
        project.update(cx, |project, cx| {
            project.set_viewer_resolution(resolution, cx);
        });
        cx.notify();
    }

    /// Show one channel of the composite on its own (`INSP-2`).
    ///
    /// Through `ProjectState` like the preview factor, and for the same
    /// reason: the mode has to reach the display transform on the evaluation
    /// worker, and the project owns the cell that gets it there. The same
    /// call is what `CommandId::ViewerChannel*` reaches.
    fn set_display_channel(&mut self, channel: DisplayChannel, cx: &mut Context<Self>) {
        let Some(project) = self.project(cx) else {
            return;
        };
        project.update(cx, |project, cx| {
            project.set_display_channel(channel, cx);
        });
        cx.notify();
    }

    /// Switch the pixel value readout on or off (`INSP-3`).
    ///
    /// Through `ProjectState` like the display channel above, and for the same
    /// reason: the flag has to reach the display transform on the evaluation
    /// worker, and the project owns the cell that gets it there. The same call
    /// is what `CommandId::ViewerPixelReadout` reaches.
    fn set_pixel_readout(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(project) = self.project(cx) else {
            return;
        };
        project.update(cx, |project, cx| {
            project.set_pixel_readout(on, cx);
        });
        // Off, the last position would otherwise still be sitting here when
        // the readout is switched on again, before the pointer has moved.
        if !on {
            self.readout_pointer = None;
        }
        cx.notify();
    }

    /// AE-style bottom toolbar: zoom readout with preset menu, Fit, 100%,
    /// the preview resolution factor, and the grid / safe-area overlay
    /// toggles.
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let zoom_label = SharedString::from(format!("{:.0}%", self.zoom_percent()));
        let entity = cx.entity().downgrade();
        let background_entity = entity.clone();
        let resolution_entity = entity.clone();
        // Selected and effective are read together from one borrow: showing a
        // selection from before an adaptive downgrade next to the factor after
        // it would report a difference that never existed.
        let (selected_resolution, effective_resolution) = self
            .project(cx)
            .map(|project| {
                let project = project.read(cx);
                (
                    project.viewer_resolution(),
                    project.effective_viewer_resolution(),
                )
            })
            .unwrap_or_default();
        let background_mode = self.background_mode;
        let channel_entity = entity.clone();
        let display_channel = self
            .project(cx)
            .map(|project| project.read(cx).display_channel())
            .unwrap_or_default();
        let readout_entity = entity.clone();
        let (pixel_readout, readout_format) = (
            self.project(cx)
                .is_some_and(|project| project.read(cx).pixel_readout()),
            cx.try_global::<super::ViewerReadoutFormat>()
                .copied()
                .unwrap_or_default()
                .0,
        );
        let field_entity = entity.clone();
        let (field_display, field_map, field_opacity) =
            (self.field_display, self.field_map, self.field_opacity);
        let guide_entity = entity.clone();
        let (show_rulers, show_guides, guides_locked) =
            (self.show_rulers, self.show_guides, self.guides_locked);
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
            .child(
                // Beside the zoom readout, not with the toggles on the right:
                // both are labelled scale readouts with a preset menu (display
                // scale and evaluation scale), while everything past the
                // spacer is an icon toggle for an overlay. `ui/viewer.md`
                // places the zoom controls in this toolbar and does not name a
                // slot for this one.
                Button::new("viewer-preview-resolution")
                    .xsmall()
                    .ghost()
                    .label(SharedString::from(resolution_label(
                        selected_resolution,
                        effective_resolution,
                    )))
                    .tooltip(t!("viewer.resolution"))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for factor in ViewerResolution::ALL {
                            let entity = resolution_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(factor.label_key())))
                                    // The tick follows the *selection*: it is
                                    // what the menu sets, and an adaptive
                                    // downgrade is not the user's choice.
                                    .checked(factor == selected_resolution)
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                this.set_preview_resolution(factor, cx);
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    }),
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
                // Beside the background mode, not with the icon toggles: both
                // are display options that change how the frame is shown
                // rather than something drawn over it, and both need a name
                // on screen because "which mode am I in" is the question the
                // channel views exist to answer.
                Button::new("viewer-display-channel")
                    .xsmall()
                    .ghost()
                    .label(SharedString::from(t!(display_channel_label_key(
                        display_channel
                    ))))
                    .tooltip(t!("viewer.channel"))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for channel in DisplayChannel::ALL {
                            let entity = channel_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(
                                    display_channel_label_key(channel)
                                )))
                                .checked(channel == display_channel)
                                .on_click(
                                    move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                this.set_display_channel(channel, cx);
                                            })
                                            .ok();
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            )
            .child(
                // Beside the channel picker: both answer "what am I being
                // shown", and the readout's own menu carries the only other
                // decision it has — which scale the numbers are on.
                Button::new("viewer-pixel-readout")
                    .xsmall()
                    .ghost()
                    .selected(pixel_readout)
                    .label(SharedString::from(t!("viewer.pixel_readout")))
                    .tooltip(t!("viewer.pixel_readout"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        let on = this
                            .project(cx)
                            .is_some_and(|project| project.read(cx).pixel_readout());
                        this.set_pixel_readout(!on, cx);
                    }))
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for format in PixelReadoutFormat::ALL {
                            let entity = readout_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(format.label_key())))
                                    .checked(format == readout_format)
                                    .on_click(move |_, _window, cx| {
                                        cx.set_global(super::ViewerReadoutFormat(format));
                                        entity.update(cx, |_this, cx| cx.notify()).ok();
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
                // One menu rather than four buttons: rulers, guides, the lock
                // and "clear" all answer "what do I want the guides to do", and
                // only the first two are ever visible state.
                Button::new("viewer-rulers")
                    .xsmall()
                    .ghost()
                    .selected(self.show_rulers)
                    .icon(Icon::new(RavelIcon::Rulers))
                    .tooltip(t!("viewer.rulers"))
                    .dropdown_menu(move |menu, _window, _cx| {
                        let mut menu = menu;
                        for (key, checked, apply) in [
                            (
                                "viewer.rulers",
                                show_rulers,
                                (|this: &mut ViewerPanel| this.show_rulers = !this.show_rulers)
                                    as fn(&mut ViewerPanel),
                            ),
                            ("viewer.guides", show_guides, |this| {
                                this.show_guides = !this.show_guides
                            }),
                            ("viewer.guides_lock", guides_locked, |this| {
                                this.guides_locked = !this.guides_locked
                            }),
                        ] {
                            let entity = guide_entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(t!(key)))
                                    .checked(checked)
                                    .on_click(move |_, _window, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                apply(this);
                                                cx.notify();
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        let entity = guide_entity.clone();
                        menu.separator().item(
                            PopupMenuItem::new(SharedString::from(t!("viewer.guides_clear")))
                                // Clearing is a deletion, so the lock greys it
                                // out; `clear_guides` refuses it anyway.
                                .disabled(guides_locked)
                                .on_click(move |_, _window, cx| {
                                    entity.update(cx, |this, cx| this.clear_guides(cx)).ok();
                                }),
                        )
                    }),
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
///
/// `mono` is the monospace family every label but the centered one is set in.
/// Passed rather than read here because this is not a `Render` method and the
/// family is the theme's ([`crate::fonts::mono_font`], so the Japanese
/// fallback comes with it — the status line is translated).
fn overlay_label_element(
    label: overlay::OverlayLabel,
    viewport: Option<(viewport::Rect, (u32, u32))>,
    mono: &Font,
) -> Option<Div> {
    let text = div().text_xs().text_color(label.color);
    // Every label but one is a readout whose digits change under the pointer
    // or the playhead, and proportional digits make the corner jitter while
    // they do — the same reason the Timeline's ruler labels are monospaced.
    // The centered label is the evaluation error, which is a sentence.
    let text = if matches!(label.placement, LabelPlacement::CanvasCenter) {
        text
    } else {
        text.font(mono.clone())
    };
    let text = text.child(label.text.clone());
    Some(match label.placement {
        LabelPlacement::CanvasCenter => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(text),
        LabelPlacement::CanvasTopLeft => div().absolute().top_2().left_2().child(text),
        LabelPlacement::CanvasBottomLeft => div().absolute().bottom_2().left_2().child(text),
        LabelPlacement::CanvasTopRight => div().absolute().top_2().right_2().child(text),
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
    /// The evaluated frame behind the picture, while the pixel readout is on
    /// (`INSP-3`). Blank and error frames have none — there is no composite to
    /// report values from.
    linear: Option<Arc<FrameBuffer>>,
}

/// Split a published [`ViewerFrame`] into durable panel content. Black is
/// painted as a quad, so Blank and Error do not allocate composition-sized
/// textures.
fn viewer_content(vf: ViewerFrame) -> ViewerContent {
    match vf {
        ViewerFrame::Frame {
            image,
            composition_resolution,
            linear,
        } => ViewerContent {
            // Already BGRA and already wrapped — the conversion ran on the
            // evaluation worker (HIGH-08).
            image: Some(image.into_image()),
            gpu_frame: None,
            error: None,
            composition_resolution: Some(composition_resolution),
            linear,
        },
        ViewerFrame::GpuFrame {
            frame,
            composition_resolution,
            linear,
        } => ViewerContent {
            image: None,
            gpu_frame: Some(frame),
            error: None,
            composition_resolution: Some(composition_resolution),
            linear,
        },
        ViewerFrame::Blank {
            composition_resolution,
        } => ViewerContent {
            image: None,
            gpu_frame: None,
            error: None,
            composition_resolution,
            linear: None,
        },
        ViewerFrame::Error {
            message,
            composition_resolution,
        } => ViewerContent {
            image: None,
            gpu_frame: None,
            error: Some(message),
            composition_resolution,
            linear: None,
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
        // The rectangle the Zoom drag has swept so far, in panel-local pixels.
        // Without the band the gesture is invisible until the view jumps.
        let zoom_marquee = self.zoom_drag.and_then(|drag| drag.rect());
        let zoom_marquee_color = cx.theme().colors.primary;
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
                .map(|drag| drag.handle.hint)
                .or_else(|| self.guide_drag.as_ref().map(|drag| guide_hint(drag.axis))),
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

        // The ruler is the one mark that is not an overlay: it is pinned to the
        // panel's edges, which the composition rectangle leaves entirely as
        // soon as the view is zoomed in.
        let rulers = self.show_rulers.then(|| {
            (
                cx.theme().colors.secondary,
                cx.theme().colors.muted_foreground,
            )
        });
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
                    // Over the overlays: the strips are chrome the guides are
                    // dragged out of, and a mark that slid under the picture
                    // would be a target the pointer cannot reach.
                    if let Some((background, tick)) = rulers {
                        overlay::paint_primitives(
                            &guides::ruler_primitives(
                                bounds,
                                frame_bounds,
                                resolution,
                                background,
                                tick,
                            ),
                            window,
                        );
                    }
                    if let Some(marquee) = zoom_marquee {
                        window.paint_quad(
                            outline(
                                Bounds {
                                    origin: point(
                                        bounds.origin.x + px(marquee.x),
                                        bounds.origin.y + px(marquee.y),
                                    ),
                                    size: size(px(marquee.width), px(marquee.height)),
                                },
                                zoom_marquee_color,
                                BorderStyle::default(),
                            )
                            .border_widths(px(1.0)),
                        );
                    }
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
            let mono = crate::fonts::mono_font(cx);
            content.children(
                labels
                    .into_iter()
                    .filter_map(|label| overlay_label_element(label, label_viewport, &mono)),
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
                    this.pan_mouse_down(event, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.left_mouse_down(event, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.pan_ended(cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.pan_ended(cx);
                    this.zoom_ended(cx);
                    this.move_ended(cx);
                    this.box_select_ended(event.position, cx);
                    this.shape_ended(cx);
                    this.pen_point_ended(cx);
                    this.handle_drag_ended(cx);
                    this.guide_drag_ended(event.position, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                // Before the gesture match, not inside one of its arms: the
                // readout follows the pointer whatever the pointer is doing,
                // and several arms below return early.
                this.track_readout_pointer(event.position, cx);
                match event.pressed_button {
                    Some(MouseButton::Middle) => {
                        this.cancel_move(cx);
                        this.cancel_box_select(cx);
                        this.cancel_shape(cx);
                        this.cancel_handle_drag(cx);
                        this.cancel_guide_drag(cx);
                        this.pan_dragged(event.position, cx);
                    }
                    Some(MouseButton::Left) => this.left_dragged(event, cx),
                    _ => {
                        // Repaint on the way out: the Zoom marquee is painted
                        // from `zoom_drag`, and the hint block below returns
                        // early whenever the pointer is off the composition or
                        // the hint is unchanged. A release the window never
                        // saw would otherwise leave the rectangle on screen
                        // until something else repainted the panel. Both are
                        // taken before the `||` so neither short-circuits.
                        let had_pan = this.pan_drag.take().is_some();
                        let had_zoom = this.zoom_drag.take().is_some();
                        if had_pan || had_zoom {
                            cx.notify();
                        }
                        this.pen_point_ended(cx);
                        this.cancel_move(cx);
                        this.cancel_box_select(cx);
                        this.cancel_shape(cx);
                        this.cancel_handle_drag(cx);
                        this.cancel_guide_drag(cx);
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
                let requested = current * zoom_factor(dy);
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
                } else if event.keystroke.key.as_str() == "escape" && this.guide_drag.is_some() {
                    this.cancel_guide_drag(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "escape" && this.box_select.is_some() {
                    this.cancel_box_select(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key.as_str() == "escape" && this.zoom_drag.is_some() {
                    this.zoom_drag = None;
                    cx.notify();
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

/// A single composition point as a zero-sized rectangle, which is what a
/// gesture that moves one point (a drawing pointer, a shell grip) hands to
/// [`snap::snap_delta`].
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

/// The rectangle a drag between two composition points swept, normalized so
/// the extents are never negative — which is what makes a sweep to the left or
/// upwards the same rectangle as one to the right or downwards.
fn box_rect(start: (f32, f32), current: (f32, f32)) -> CompRect {
    CompRect {
        x: start.0.min(current.0),
        y: start.1.min(current.1),
        w: (current.0 - start.0).abs(),
        h: (current.1 - start.1).abs(),
    }
}

/// Whether two closed composition rectangles meet.
///
/// **Intersection, not containment**: a box selection catches what it touches,
/// which is what makes a shape larger than the swept rectangle selectable at
/// all. Closed on the boundary, the rule [`rect_contains`] already follows, so
/// a rectangle whose edge lands exactly on a bbox's edge catches it — and a
/// zero-extent bbox (a single placed point) is catchable, which a strict
/// comparison would make impossible.
///
/// **Both rectangles are assumed normalized** (`w`, `h` >= 0), which is what
/// every caller has: the swept rectangle comes from [`box_rect`], and a bbox
/// comes from `geometry_bounds` / [`transform_rect`], both of which build their
/// extents from a min/max pass. Normalizing here would write the opposite
/// premise into the code — that a rectangle with negative extents can arrive —
/// and no such rectangle exists.
fn rects_overlap(a: &CompRect, b: &CompRect) -> bool {
    a.x <= b.x + b.w && b.x <= a.x + a.w && a.y <= b.y + b.h && b.y <= a.y + a.h
}

/// Every node of `network` whose evaluated bbox meets `rect`.
///
/// The same bounds the outline is drawn from and the click test picks by
/// ([`node_comp_rect`]), so a sweep catches what the user can see. A node
/// whose result has not arrived has no bbox and is not caught — the mechanism's
/// "no result, no guessing" rule, which is why the release recomputes.
fn nodes_in_box(ctx: &OverlayContext, network: &NetworkPath, rect: CompRect) -> HashSet<NodeId> {
    let Some(document) = ctx.document.as_ref() else {
        return HashSet::new();
    };
    let Some(graph) = ravel_ui::document::resolve_network(document, network) else {
        return HashSet::new();
    };
    graph
        .nodes()
        .filter(|node| {
            node_comp_rect(ctx, network, node.id).is_some_and(|bbox| rects_overlap(&bbox, &rect))
        })
        .map(|node| node.id)
        .collect()
}

/// Every layer of `comp` whose evaluated bbox meets `rect`, in layer order.
fn layers_in_box(ctx: &OverlayContext, comp: CompId, rect: CompRect) -> Vec<LayerId> {
    let Some(document) = ctx.document.as_ref() else {
        return Vec::new();
    };
    let Some(composition) = document.get_composition(comp) else {
        return Vec::new();
    };
    composition
        .layers
        .iter()
        .filter(|layer| {
            layer_comp_rect(ctx, document, comp, layer.id)
                .is_some_and(|bbox| rects_overlap(&bbox, &rect))
        })
        .map(|layer| layer.id)
        .collect()
}

/// The node selection a released box publishes: the sweep, or its union with
/// the press-time selection when Shift started the drag.
///
/// The union is the whole point of the Shift form. The Node Editor requires
/// Shift to start its band and then publishes the band's contents alone
/// (`LOW-APP-03`), which silently drops everything the user had selected; this
/// is the same feature without that.
fn nodes_after_box(
    initial: &HashSet<NodeId>,
    inside: HashSet<NodeId>,
    shift: bool,
) -> HashSet<NodeId> {
    if shift {
        initial.union(&inside).copied().collect()
    } else {
        inside
    }
}

/// [`nodes_after_box`] for layers, which keep click order: the press-time
/// selection first, then whatever the sweep added.
fn layers_after_box(initial: &[LayerId], inside: &[LayerId], shift: bool) -> Vec<LayerId> {
    if !shift {
        return inside.to_vec();
    }
    let mut layers = initial.to_vec();
    for id in inside {
        if !layers.contains(id) {
            layers.push(*id);
        }
    }
    layers
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

/// The topmost shape node under `point` in any layer of `comp` other than
/// `active` (REQ-UI-011's v1.5 fallback).
///
/// Searched top-down, because that is the order the layers are composited in
/// (`Composition::layers` runs bottom-to-top) — so the node this picks is the
/// one drawn over the others, the rule [`hit_test_shape_nodes`] already
/// follows inside a network with `metadata.z`.
///
/// Each layer is tested through its own shell, so a rotated or scaled layer is
/// picked where it appears. What happens *after* the pick is still restricted:
/// the move and draw gestures refuse a non-identity shell (REQ-UI-011 v2), so
/// such a layer becomes selectable here without becoming editable.
///
/// A layer whose geometry has not been evaluated has no bbox and is skipped —
/// the mechanism's "no result, no guessing" rule. A layer that does not
/// composite is skipped too: [`Composition::composites`] is the compositor's
/// own rule (muted out, and non-soloed out while anything is soloed), asked
/// rather than restated, so this can never pick a shape that is not on screen.
fn hit_test_other_layers(
    ctx: &OverlayContext,
    comp: CompId,
    active: Option<LayerId>,
    point: (f32, f32),
) -> Option<(NetworkPath, NodeId)> {
    let document = ctx.document.as_ref()?;
    let composition = document.get_composition(comp)?;
    composition
        .layers
        .iter()
        .rev()
        .filter(|layer| Some(layer.id) != active && composition.composites(layer))
        .find_map(|layer| {
            let network = NetworkPath::layer(comp, layer.id);
            let node = hit_test_shape_nodes(ctx, &network, point)?;
            Some((network, node))
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

/// How far a point's two tangents may miss being reflections of each other
/// and still count as one smooth handle, as a fraction of the longer arm.
///
/// Relative rather than absolute because these are f32 composition units: at
/// the ~10^4 magnitudes a 4K composition reaches, one ulp is already ~10^-3,
/// so a fixed epsilon would call a large symmetric handle asymmetric. A
/// thousandth of a percent of the arm is far below the width of the line the
/// handle is drawn as, and far above the rounding a save / load round trip can
/// introduce.
const TANGENT_SYMMETRY_TOLERANCE: f32 = 1e-4;

/// Whether a point's tangents form one smooth handle: two arms pointing
/// opposite ways.
///
/// Read off the values, with no flag stored anywhere — a point that *is*
/// symmetric behaves as smooth and a point that is not behaves as split, so
/// there is nothing to persist, migrate, or keep in sync with the geometry
/// (the decision `viewer-tool-extensions-plan.md` records).
///
/// A point with no tangents at all (a corner the pen placed) is not smooth: it
/// has no arms to mirror, and the overlay draws neither handle for it.
fn tangents_are_symmetric(point: &PathPoint) -> bool {
    let origin = (0.0, 0.0);
    let sum = (
        point.in_tan.0 + point.out_tan.0,
        point.in_tan.1 + point.out_tan.1,
    );
    let reach = distance_squared((point.in_tan.0, point.in_tan.1), origin)
        .max(distance_squared((point.out_tan.0, point.out_tan.1), origin))
        .sqrt();
    reach > 0.0 && distance_squared(sum, origin).sqrt() <= reach * TANGENT_SYMMETRY_TOLERANCE
}

/// The path with one handle of `index` moved by `delta`.
///
/// A tangent of a smooth point carries its opposite arm with it, mirrored, so
/// the curve stays smooth through the anchor; `separate` (the `Alt` modifier)
/// moves the grabbed arm alone. A point whose arms are already not
/// reflections is *already* split, so it moves one arm whatever `separate`
/// says — which is what makes `Alt` a one-way operation rather than a mode:
/// the first `Alt` drag breaks the symmetry, and every later drag of that
/// point is independent because the values say so.
///
/// The **delta** is mirrored, not the value: a point that was only nearly
/// symmetric keeps its own arms rather than being silently squared up, and a
/// drag back to the press point restores exactly what was there — which is
/// what lets a zero-delta release skip both the commit and the revert.
fn edited_path_points(
    original: &[PathPoint],
    index: usize,
    handle: PathHandleKind,
    delta: (f32, f32),
    separate: bool,
) -> Vec<PathPoint> {
    let mut points = original.to_vec();
    let Some(point) = points.get_mut(index) else {
        return points;
    };
    // Judged on the press-time values (`original`), which is also the frame
    // the delta is measured from: a drag cannot argue itself out of the
    // symmetry it started with, half way through.
    let mirror = !separate && tangents_are_symmetric(point);
    let opposite = (-delta.0, -delta.1);
    match handle {
        PathHandleKind::Point => {
            offset_vec2(&mut point.p, delta);
        }
        PathHandleKind::InTangent => {
            offset_vec2(&mut point.in_tan, delta);
            if mirror {
                offset_vec2(&mut point.out_tan, opposite);
            }
        }
        PathHandleKind::OutTangent => {
            offset_vec2(&mut point.out_tan, delta);
            if mirror {
                offset_vec2(&mut point.in_tan, opposite);
            }
        }
    }
    points
}

// ---------------------------------------------------------------------------
// Path point insertion and removal (REQ-UI-011 v1.5)
// ---------------------------------------------------------------------------

/// Samples per segment the segment hit test walks.
///
/// The closest point on a cubic is a fifth-degree root problem, and solving it
/// would buy nothing here: the insertion **splits** the curve at the parameter
/// this returns, so the shape is preserved exactly whatever `t` comes out —
/// `t` only decides where along the curve the new point lands. 32 steps put
/// that within a fraction of the handle drawn on it.
const PATH_SEGMENT_SAMPLES: usize = 32;

/// Which point of a path a press within `radius` grabs: the nearest one, so
/// two points closer together than the radius still resolve to the one under
/// the pointer.
fn path_point_at(points: &[PathPoint], pointer: (f32, f32), radius: f32) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| (index, distance_squared((point.p.0, point.p.1), pointer)))
        .filter(|(_, distance)| *distance <= radius * radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Anchor index pairs of every segment of a path, in drawing order. A closed
/// path has one more: the one back to the start.
fn path_segment_indices(len: usize, closed: bool) -> Vec<(usize, usize)> {
    if len < 2 {
        return Vec::new();
    }
    let mut segments: Vec<_> = (0..len - 1).map(|index| (index, index + 1)).collect();
    if closed {
        segments.push((len - 1, 0));
    }
    segments
}

/// One cubic segment of a path: the two anchors and, between them, the two
/// control points their tangents place.
type PathSegment = [(f32, f32); 4];

/// The cubic between two anchors of a path.
fn path_segment(points: &[PathPoint], from: usize, to: usize) -> PathSegment {
    let (a, b) = (&points[from], &points[to]);
    [
        (a.p.0, a.p.1),
        (a.p.0 + a.out_tan.0, a.p.1 + a.out_tan.1),
        (b.p.0 + b.in_tan.0, b.p.1 + b.in_tan.1),
        (b.p.0, b.p.1),
    ]
}

fn lerp_point(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// de Casteljau's construction at `t`: the two cubics the segment splits into.
///
/// A split, not a re-fit. The two halves trace exactly the curve the original
/// traced — which is what makes inserting a point leave the shape alone.
/// Deriving the new tangents from the neighbouring anchors instead (the
/// obvious shortcut) moves the curve.
///
/// The construction's last point is also the curve's value at `t`, which is
/// how the hit test below evaluates it: one formula, so the point a press
/// lands on and the point it inserts cannot disagree.
fn split_cubic(segment: &PathSegment, t: f32) -> (PathSegment, PathSegment) {
    let a = lerp_point(segment[0], segment[1], t);
    let b = lerp_point(segment[1], segment[2], t);
    let c = lerp_point(segment[2], segment[3], t);
    let d = lerp_point(a, b, t);
    let e = lerp_point(b, c, t);
    let f = lerp_point(d, e, t);
    ([segment[0], a, d, f], [f, e, c, segment[3]])
}

/// The point on a path segment at `t`.
fn cubic_at(segment: &PathSegment, t: f32) -> (f32, f32) {
    split_cubic(segment, t).0[3]
}

/// The segment a press within `radius` landed on, and how far along it.
fn path_segment_at(
    points: &[PathPoint],
    closed: bool,
    pointer: (f32, f32),
    radius: f32,
) -> Option<(usize, usize, f32)> {
    let mut best: Option<((usize, usize, f32), f32)> = None;
    for (from, to) in path_segment_indices(points.len(), closed) {
        let segment = path_segment(points, from, to);
        for step in 0..=PATH_SEGMENT_SAMPLES {
            let t = step as f32 / PATH_SEGMENT_SAMPLES as f32;
            let distance = distance_squared(cubic_at(&segment, t), pointer);
            if distance <= radius * radius && best.is_none_or(|(_, closest)| distance < closest) {
                best = Some(((from, to, t), distance));
            }
        }
    }
    best.map(|(hit, _)| hit)
}

/// The path with a point inserted where a press within `radius` landed on it,
/// or `None` when the press was not on the path.
///
/// The curve is unchanged: both neighbours' facing tangents are rewritten to
/// the halves [`split_cubic`] produced, so the two segments together trace
/// what the one segment traced.
fn path_with_inserted_point(
    points: &[PathPoint],
    closed: bool,
    pointer: (f32, f32),
    radius: f32,
) -> Option<Vec<PathPoint>> {
    let (from, to, t) = path_segment_at(points, closed, pointer, radius)?;
    let (left, right) = split_cubic(&path_segment(points, from, to), t);
    let anchor = left[3];
    let mut points = points.to_vec();
    points[from].out_tan = Vec2(left[1].0 - left[0].0, left[1].1 - left[0].1);
    points[to].in_tan = Vec2(right[2].0 - right[3].0, right[2].1 - right[3].1);
    // `from + 1` is the end of the list for the closing segment, which is
    // exactly where a point between the last anchor and the first belongs.
    points.insert(
        from + 1,
        PathPoint {
            p: Vec2(anchor.0, anchor.1),
            in_tan: Vec2(left[2].0 - anchor.0, left[2].1 - anchor.1),
            out_tan: Vec2(right[1].0 - anchor.0, right[1].1 - anchor.1),
        },
    );
    Some(points)
}

/// The path with the point at `index` removed, or `None` when the path cannot
/// spare it.
///
/// The neighbours keep their own tangents untouched. The curve across the gap
/// changes — it has to, one of its ends is gone — but nothing else about the
/// shape does, so re-inserting a point restores the neighbourhood instead of
/// leaving the user to re-sculpt it.
///
/// Two points are the least a path can be (the pen discards a one-point
/// session for the same reason), so a path down to two refuses.
fn path_without_point(points: &[PathPoint], index: usize) -> Option<Vec<PathPoint>> {
    if points.len() <= 2 || index >= points.len() {
        return None;
    }
    let mut points = points.to_vec();
    points.remove(index);
    Some(points)
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

/// Map a comp-space drag to the extents of a radially symmetric shape: the
/// press point is the centre and the drag distance is the outer radius
/// (`TOOLX-4`).
///
/// Not [`drag_geometry`]: a polygon or a star drawn corner to corner would move
/// its own centre on every pointer move, so the shape would jump around under
/// the cursor instead of growing out of the press. Both components carry the
/// same radius, which is what makes the degenerate check below apply unchanged.
///
/// Shift and Alt carry no meaning here — the shape is already centred on the
/// press, and constraining the *angle* means dragging a rotation parameter,
/// which this unit does not do.
fn radial_drag_geometry(start: (f32, f32), current: (f32, f32)) -> DragGeometry {
    let radius = (current.0 - start.0).hypot(current.1 - start.1);
    DragGeometry {
        center: start,
        half: (radius, radius),
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
        // Only the outer radius: the side / point count and the star's inner
        // radius keep the registry defaults and are adjusted in Properties
        // after the shape is committed (`TOOLX-4`).
        ShapeDrawKind::Polygon => {
            values.push(("radius", ParameterValue::Float(geo.half.0)));
        }
        ShapeDrawKind::Star => {
            values.push(("outer_radius", ParameterValue::Float(geo.half.0)));
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
    fn evaluated_results(graph: &Graph, network: &NetworkPath) -> overlay::EvalResults {
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
        overlay::EvalResults::new(values)
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
        cx.update(|cx| cx.set_global(overlay::EvalResults::new(values)));
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

        ctx.results = overlay::EvalResults::default();
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

    /// A radial shape is drawn from its centre: the press point *is* the
    /// centre and the drag distance is the outer radius, in every direction.
    #[test]
    fn radial_drag_geometry_puts_the_outer_radius_at_the_drag_distance() {
        // 3-4-5: a distance that is not either delta, so a stray `abs()` on
        // one axis cannot pass.
        for to in [
            (330.0, 240.0),
            (270.0, 240.0),
            (330.0, 160.0),
            (270.0, 160.0),
        ] {
            let geo = radial_drag_geometry((300.0, 200.0), to);
            assert_eq!(geo.center, (300.0, 200.0), "the press point is the centre");
            assert!(
                (geo.half.0 - 50.0).abs() < 1e-3 && (geo.half.1 - 50.0).abs() < 1e-3,
                "the drag distance is the outer radius: {geo:?}"
            );
        }

        // And it is *not* the corner-to-corner box the rect tools use, whose
        // centre drifts with the pointer.
        let corner = drag_geometry((300.0, 200.0), (330.0, 240.0), false, false);
        assert_ne!(corner.center, (300.0, 200.0));
        assert_ne!(corner.half, (50.0, 50.0));
    }

    /// Both components carry the same radius, so the existing degenerate check
    /// answers "zero distance" without a rule of its own.
    #[test]
    fn a_radial_drag_is_degenerate_only_at_zero_distance() {
        assert!(drag_geometry_degenerate(radial_drag_geometry(
            (10.0, 10.0),
            (10.0, 10.0)
        )));
        // Along one axis only: a box drag would call this degenerate, a radial
        // one must not.
        assert!(!drag_geometry_degenerate(radial_drag_geometry(
            (10.0, 10.0),
            (50.0, 10.0)
        )));
        assert!(!drag_geometry_degenerate(radial_drag_geometry(
            (10.0, 10.0),
            (10.0, 11.0)
        )));
    }

    /// The polygon takes the radius and nothing else: the side count is the
    /// registry default until Properties changes it (`TOOLX-4`).
    #[test]
    fn drawn_polygon_takes_the_radius_and_keeps_the_side_count() {
        let registry = registry();
        let fresh = registry
            .create_node("shape.polygon", NodeId::next())
            .unwrap();
        let node = drawn_shape_node(
            fresh.clone(),
            ShapeDrawKind::Polygon,
            radial_drag_geometry((300.0, 200.0), (330.0, 240.0)),
        );
        let ctx = eval_ctx();
        assert_eq!(
            sample_vec2_param(&node, "center", 0, &ctx),
            Some((300.0, 200.0))
        );
        assert_eq!(sample_float_param(&node, "radius", 0, &ctx), Some(50.0));
        assert_eq!(
            param_value(&node, "sides"),
            param_value(&fresh, "sides"),
            "the drag must not decide the side count"
        );
    }

    /// The star takes the *outer* radius only: the inner radius and the point
    /// count stay at the registry defaults.
    #[test]
    fn drawn_star_takes_the_outer_radius_and_keeps_the_other_defaults() {
        let registry = registry();
        let fresh = registry.create_node("shape.star", NodeId::next()).unwrap();
        let node = drawn_shape_node(
            fresh.clone(),
            ShapeDrawKind::Star,
            radial_drag_geometry((300.0, 200.0), (330.0, 240.0)),
        );
        let ctx = eval_ctx();
        assert_eq!(
            sample_vec2_param(&node, "center", 0, &ctx),
            Some((300.0, 200.0))
        );
        assert_eq!(
            sample_float_param(&node, "outer_radius", 0, &ctx),
            Some(50.0)
        );
        for key in ["inner_radius", "points"] {
            assert_eq!(
                param_value(&node, key),
                param_value(&fresh, key),
                "the drag must not decide {key}"
            );
        }
    }

    /// The stored value of one parameter, for "the drag left this alone"
    /// assertions that must not hardcode the registry default.
    fn param_value(node: &Node, key: &str) -> Option<ParameterValue> {
        node.parameters
            .iter()
            .find(|param| param.key == key)
            .map(|param| param.value.clone())
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

    /// The radial tools travel the same creation path as the box ones: the
    /// Shape template's rect placeholder is dropped and the drawn node takes
    /// the freed rasterize geometry input.
    #[test]
    fn a_drawn_radial_shape_replaces_the_template_placeholder() {
        let registry = registry();
        for (kind, type_key, radius_key) in [
            (ShapeDrawKind::Polygon, "shape.polygon", "radius"),
            (ShapeDrawKind::Star, "shape.star", "outer_radius"),
        ] {
            let (doc, _path) = doc_with_network(Graph::new());
            let comp = doc.root_comp.unwrap();
            let geo = radial_drag_geometry((100.0, 100.0), (130.0, 140.0));
            let (doc, path, node_id) =
                create_layer_with_drawn_shape(&doc, comp, &registry, kind, geo).unwrap();
            let graph = ravel_ui::document::resolve_network(&doc, &path).unwrap();

            assert!(graph.nodes().all(|n| n.type_key != "shape.rect"));
            assert_eq!(graph.nodes().count(), 4, "{type_key}: rect out, shape in");
            let node = graph.node(node_id).unwrap();
            assert_eq!(node.type_key, type_key);
            assert_eq!(
                sample_float_param(node, radius_key, 0, &eval_ctx()),
                Some(50.0)
            );
            let rasterize_id = graph
                .nodes()
                .find(|n| n.type_key == "rasterize")
                .unwrap()
                .id;
            let outgoing: Vec<_> = graph.edges().filter(|e| e.source == node_id).collect();
            assert_eq!(outgoing.len(), 1, "{type_key}: one auto-wired edge");
            assert_eq!(outgoing[0].target, rasterize_id);
            assert_eq!(outgoing[0].target_port, InputPortIndex(0));
            assert_eq!(doc.validate(), Ok(()));
        }
    }

    /// And they collapse into one undo step the same way — template layer,
    /// node and wiring all unwind together.
    #[test]
    fn radial_shape_creation_is_one_undo_step() {
        use ravel_ui::document::DocumentStore;
        let registry = registry();
        for kind in [ShapeDrawKind::Polygon, ShapeDrawKind::Star] {
            let (doc, _path) = doc_with_network(Graph::new());
            let original_layers = ravel_ui::document::root_composition(&doc)
                .unwrap()
                .layer_count();
            let mut store = DocumentStore::new(doc);
            let comp = store.document().root_comp.unwrap();
            let geo = radial_drag_geometry((100.0, 100.0), (130.0, 140.0));
            let (doc, _path, _node) =
                create_layer_with_drawn_shape(store.document(), comp, &registry, kind, geo)
                    .unwrap();
            store.apply(doc.clone());
            store.commit(doc);

            let layers = |store: &DocumentStore| {
                ravel_ui::document::root_composition(store.document())
                    .unwrap()
                    .layer_count()
            };
            assert_eq!(layers(&store), original_layers + 1);
            assert!(store.undo());
            assert_eq!(
                layers(&store),
                original_layers,
                "{kind:?}: one undo removes it all"
            );
            assert!(!store.can_undo(), "{kind:?}: no intermediate steps remain");
        }
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

    /// Completion criterion: a point whose arms are **already** not
    /// reflections moves one arm alone, with no modifier — the behaviour that
    /// was there before the smooth handle existed, kept for every point the
    /// user has already split.
    #[test]
    fn path_handle_editing_moves_only_the_requested_vector() {
        let original = vec![PathPoint {
            p: Vec2(10.0, 20.0),
            in_tan: Vec2(-3.0, 4.0),
            out_tan: Vec2(5.0, -6.0),
        }];
        assert!(
            !tangents_are_symmetric(&original[0]),
            "the fixture is the already-split case"
        );
        let edited =
            edited_path_points(&original, 0, PathHandleKind::OutTangent, (2.0, 3.0), false);
        assert_eq!(edited[0].p, original[0].p);
        assert_eq!(edited[0].in_tan, original[0].in_tan);
        assert_eq!(edited[0].out_tan, Vec2(7.0, -3.0));

        let moved_point =
            edited_path_points(&original, 0, PathHandleKind::Point, (2.0, 3.0), false);
        assert_eq!(moved_point[0].p, Vec2(12.0, 23.0));
        assert_eq!(moved_point[0].in_tan, original[0].in_tan);
        assert_eq!(moved_point[0].out_tan, original[0].out_tan);
    }

    /// Completion criterion: dragging one tangent of a smooth point carries
    /// the other one with it, mirrored — and `Alt` does not.
    #[test]
    fn a_smooth_points_tangents_move_together_until_alt_splits_them() {
        let smooth = vec![smooth_path_point((10.0, 20.0), (30.0, 20.0))];
        assert!(tangents_are_symmetric(&smooth[0]));

        for (handle, expected_in, expected_out) in [
            (
                PathHandleKind::OutTangent,
                Vec2(-22.0, -3.0),
                Vec2(22.0, 3.0),
            ),
            (
                PathHandleKind::InTangent,
                Vec2(-18.0, 3.0),
                Vec2(18.0, -3.0),
            ),
        ] {
            let mirrored = edited_path_points(&smooth, 0, handle, (2.0, 3.0), false);
            assert_eq!(
                (mirrored[0].in_tan, mirrored[0].out_tan),
                (expected_in, expected_out),
                "the opposite arm follows the grabbed one, reflected"
            );
            assert_eq!(mirrored[0].p, smooth[0].p, "the anchor stays put");
            assert!(
                tangents_are_symmetric(&mirrored[0]),
                "and the point is still smooth"
            );
        }

        // Completion criterion: after an Alt drag the opposite tangent has not
        // moved, and the point is split from then on.
        let split = edited_path_points(&smooth, 0, PathHandleKind::OutTangent, (2.0, 3.0), true);
        assert_eq!(
            split[0].in_tan, smooth[0].in_tan,
            "Alt leaves the opposite arm exactly where it was"
        );
        assert_eq!(split[0].out_tan, Vec2(22.0, 3.0));
        assert!(!tangents_are_symmetric(&split[0]));

        let again = edited_path_points(&split, 0, PathHandleKind::OutTangent, (1.0, 1.0), false);
        assert_eq!(
            again[0].in_tan, split[0].in_tan,
            "a split point stays split without the modifier: no flag was needed to remember"
        );

        // A drag back to the press point restores the arms bit for bit, which
        // is what lets a zero-delta release skip the commit and the revert.
        assert_eq!(
            edited_path_points(&smooth, 0, PathHandleKind::OutTangent, (0.0, 0.0), false),
            smooth
        );
    }

    /// The symmetry test tolerates the rounding f32 composition units carry,
    /// and nothing more: a corner point has no arms to mirror, and a bend the
    /// user can see is not "nearly symmetric".
    #[test]
    fn tangent_symmetry_is_read_off_the_values() {
        let point = |in_tan: (f32, f32), out_tan: (f32, f32)| PathPoint {
            p: Vec2(0.0, 0.0),
            in_tan: Vec2(in_tan.0, in_tan.1),
            out_tan: Vec2(out_tan.0, out_tan.1),
        };
        assert!(tangents_are_symmetric(&point((-10.0, -5.0), (10.0, 5.0))));
        assert!(
            tangents_are_symmetric(&point((-8000.0, 0.0), (7999.9995, 0.0))),
            "one ulp at 4K magnitudes is still one smooth handle"
        );
        assert!(
            !tangents_are_symmetric(&point((-10.0, 0.0), (10.0, 0.1))),
            "a visible bend is a split point"
        );
        assert!(
            !tangents_are_symmetric(&point((-10.0, 0.0), (5.0, 0.0))),
            "arms of different lengths are not reflections"
        );
        assert!(
            !tangents_are_symmetric(&corner_path_point((10.0, 20.0))),
            "a corner has no arms at all"
        );
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
            results: overlay::EvalResults::new(values),
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

        let all = SnapLines::collect(&ctx, Some(comp), &[], None);
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

        let without_first = SnapLines::collect(&ctx, Some(comp), &layers[..1], None);
        assert!(
            !without_first.x.contains(&-50.0),
            "a moving layer contributes no candidate of its own"
        );
        assert!(
            without_first.x.contains(&350.0),
            "the layers it is aligned against are still there"
        );

        // No composition named: only what the frame itself provides.
        let frame_only = SnapLines::collect(&ctx, None, &[], None);
        assert_eq!(frame_only.x, vec![0.0, 960.0, 1920.0]);
    }

    /// The safe areas are candidates exactly while they are drawn, and from the
    /// same fractions [`overlay::SafeAreaOverlay`] draws them with.
    #[test]
    fn safe_areas_are_candidates_only_while_they_are_shown() {
        let (mut ctx, comp, _) = snap_context();
        assert!(
            !SnapLines::collect(&ctx, Some(comp), &[], None)
                .x
                .contains(&96.0),
            "hidden safe areas pull nothing"
        );

        ctx.show_safe_areas = true;
        let shown = SnapLines::collect(&ctx, Some(comp), &[], None);
        for fraction in overlay::SAFE_AREA_FRACTIONS {
            let inset = 1920.0 * (1.0 - fraction) * 0.5;
            assert!(shown.x.contains(&inset) && shown.x.contains(&(1920.0 - inset)));
        }
    }

    // -----------------------------------------------------------------------
    // Rulers and user guides (SNAP-2)
    // -----------------------------------------------------------------------

    /// The same context with these guides on its composition.
    fn with_guides(ctx: &OverlayContext, comp: CompId, guides: Vec<Guide>) -> OverlayContext {
        let document = ravel_ui::document::update_composition(
            ctx.document.as_ref().expect("a document"),
            comp,
            |mut composition| {
                composition.guides = guides;
                composition
            },
        )
        .expect("the fixture composition");
        OverlayContext {
            document: Some(document),
            show_guides: true,
            ..ctx.clone()
        }
    }

    /// A guide is a candidate exactly while the guides are shown, on the axis it
    /// runs along, and ahead of the layers so a deliberate mark beats an
    /// accidental edge.
    #[test]
    fn guides_are_candidates_only_while_they_are_shown() {
        let (ctx, comp, _) = snap_context();
        let placed = vec![Guide::vertical(700.0), Guide::horizontal(300.0)];

        let hidden = OverlayContext {
            show_guides: false,
            ..with_guides(&ctx, comp, placed.clone())
        };
        let hidden = SnapLines::collect(&hidden, Some(comp), &[], None);
        assert!(
            !hidden.x.contains(&700.0) && !hidden.y.contains(&300.0),
            "a hidden guide pulls nothing"
        );

        let shown = SnapLines::collect(&with_guides(&ctx, comp, placed), Some(comp), &[], None);
        assert!(shown.x.contains(&700.0), "the vertical guide is on x");
        assert!(shown.y.contains(&300.0), "the horizontal guide is on y");
        assert!(
            shown.x.iter().position(|line| *line == 700.0)
                < shown.x.iter().position(|line| *line == 350.0),
            "guides are enumerated before the layer edges, so they win ties"
        );
    }

    /// The guide a gesture is moving is left out of its own candidates: a line
    /// is always within zero of itself, so leaving it in would pin the drag to
    /// its start.
    #[test]
    fn a_moving_guide_is_not_its_own_candidate() {
        let (ctx, comp, _) = snap_context();
        let ctx = with_guides(
            &ctx,
            comp,
            vec![Guide::vertical(700.0), Guide::vertical(710.0)],
        );

        let moving = SnapLines::collect(&ctx, Some(comp), &[], Some(0));
        assert!(!moving.x.contains(&700.0), "the dragged guide is left out");
        assert!(
            moving.x.contains(&710.0),
            "the guide it is aligned against is still there"
        );
        assert!(
            SnapLines::collect(&ctx, Some(comp), &[], None)
                .x
                .contains(&700.0)
        );
    }

    /// A guide pulls under exactly the rule every other candidate does: the
    /// screen-pixel threshold converted through the zoom, and nothing at all
    /// while the primary modifier is held.
    #[test]
    fn a_guide_pulls_under_the_same_threshold_as_every_other_candidate() {
        let (ctx, comp, _) = snap_context();
        let ctx = with_guides(&ctx, comp, vec![Guide::vertical(700.0)]);
        let lines = SnapLines::collect(&ctx, Some(comp), &[], None);
        let point = CompRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        let held = DragModifiers {
            primary: true,
            ..DragModifiers::default()
        };

        // 1:1 — six units is six pixels, inside the eight-pixel reach.
        let near = snap::snap_delta(
            point,
            (694.0, 0.0),
            &lines,
            snap::comp_threshold(1.0),
            Default::default(),
        );
        assert_eq!(near.delta, (700.0, 0.0));
        assert_eq!(near.guides.x, Some(700.0));
        // The same gap at 4x zoom is 24 pixels: out of reach.
        assert_eq!(
            snap::snap_delta(
                point,
                (694.0, 0.0),
                &lines,
                snap::comp_threshold(0.25),
                Default::default()
            )
            .delta,
            (694.0, 0.0)
        );
        // And the suppression key applies to a guide like to anything else.
        assert_eq!(
            snap::snap_delta(point, (694.0, 0.0), &lines, snap::comp_threshold(1.0), held).delta,
            (694.0, 0.0)
        );
    }

    /// The composition's guides, in order.
    fn guides_of(
        project: &Entity<ProjectState>,
        comp: ravel_core::id::CompId,
        cx: &mut TestAppContext,
    ) -> Vec<Guide> {
        project.read_with(cx, |project, _| {
            project
                .document()
                .get_composition(comp)
                .expect("the composition")
                .guides
                .clone()
        })
    }

    fn set_guides(
        project: &Entity<ProjectState>,
        comp: ravel_core::id::CompId,
        guides: Vec<Guide>,
        cx: &mut TestAppContext,
    ) {
        project.update(cx, |project, cx| {
            let document =
                ravel_ui::document::update_composition(project.document(), comp, |mut c| {
                    c.guides = guides;
                    c
                })
                .expect("the composition");
            project.commit_document(document, InvalidationHint::None, cx);
        });
    }

    /// Dragging out of a ruler creates a guide where the pointer let go, and the
    /// whole gesture is one undo step.
    #[gpui::test]
    fn a_guide_dragged_out_of_the_ruler_is_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.show_rulers = true;
                // The top strip: composition y 8 is inside the 16px band.
                let press = press_at(panel, (600.0, 8.0));
                assert!(panel.guide_mouse_down(&press, cx), "the ruler took it");
                let release = window_point(panel, (600.0, 300.0));
                panel.guide_dragged(release, DragModifiers::default(), cx);
                panel.guide_drag_ended(release, cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::horizontal(300.0)],
            "the top ruler drags out a horizontal guide at the pointer"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "one undo took the whole gesture"
        );
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::horizontal(300.0)],
            "and one redo puts the created guide back"
        );
    }

    /// A guide moves across itself and nowhere else. The delta on the other
    /// axis is discarded, and so is the guide line that would otherwise name an
    /// alignment this gesture cannot make.
    #[gpui::test]
    fn a_guide_drag_writes_only_the_axis_it_runs_across(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(&project, comp_id, vec![Guide::vertical(500.0)], cx);

        window
            .update(cx, |panel, _window, cx| {
                let press = press_at(panel, (500.0, 400.0));
                assert!(panel.guide_mouse_down(&press, cx), "the guide took it");
                // y lands exactly on the composition's horizontal centre line,
                // which a vertical guide has no way of reaching.
                let to = window_point(panel, (620.0, 940.0));
                panel.guide_dragged(to, DragModifiers::default(), cx);
                assert!(
                    panel.snap_guides.y.is_none(),
                    "no guide on the axis the gesture cannot write"
                );
                panel.guide_drag_ended(to, cx);
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(620.0)],
            "only the guide's own axis moved"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(500.0)],
            "one undo returns the guide to where the drag picked it up"
        );
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(620.0)],
            "and one redo replays the move"
        );
    }

    /// Dropping a guide back on the ruler deletes it, and a guide that never
    /// left the ruler leaves no undo step behind.
    #[gpui::test]
    fn a_guide_dropped_back_on_the_ruler_is_deleted(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(&project, comp_id, vec![Guide::horizontal(300.0)], cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.show_rulers = true;
                // Created and released without leaving the strip.
                let press = press_at(panel, (600.0, 4.0));
                assert!(panel.guide_mouse_down(&press, cx));
                let release = window_point(panel, (600.0, 6.0));
                panel.guide_dragged(release, DragModifiers::default(), cx);
                panel.guide_drag_ended(release, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::horizontal(300.0)],
            "a guide put straight back leaves the document as it was"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "and adds no undo step: the next undo is the one before it"
        );
        project.update(cx, |project, cx| assert!(project.redo(cx)));

        window
            .update(cx, |panel, _window, cx| {
                let press = press_at(panel, (600.0, 300.0));
                assert!(panel.guide_mouse_down(&press, cx));
                let release = window_point(panel, (600.0, 6.0));
                panel.guide_dragged(release, DragModifiers::default(), cx);
                panel.guide_drag_ended(release, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "an existing guide dropped on the ruler is deleted"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::horizontal(300.0)],
            "and the deletion is one undo step"
        );
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "and one redo deletes it again"
        );
    }

    /// A guide pressed and released where it stood costs nothing: the very next
    /// undo is the step before the gesture, not a wasted press on an identical
    /// document.
    #[gpui::test]
    fn a_guide_released_where_it_started_is_not_an_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(&project, comp_id, vec![Guide::vertical(500.0)], cx);

        window
            .update(cx, |panel, _window, cx| {
                let press = press_at(panel, (500.0, 400.0));
                assert!(panel.guide_mouse_down(&press, cx));
                let same = window_point(panel, (500.0, 400.0));
                panel.guide_dragged(same, DragModifiers::default(), cx);
                panel.guide_drag_ended(same, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(500.0)],
            "the guide stayed where it was"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "one undo reached the state before the guide was placed"
        );
    }

    /// Locking says "do not move these": the pointer is refused and promises
    /// nothing, while the line stays drawn and stays a snap candidate.
    #[gpui::test]
    fn a_locked_guide_refuses_the_pointer_and_still_snaps(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(&project, comp_id, vec![Guide::vertical(500.0)], cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.show_rulers = true;
                panel.guides_locked = true;
                let press = press_at(panel, (500.0, 400.0));
                assert!(!panel.guide_mouse_down(&press, cx), "a locked guide holds");
                let ruler = press_at(panel, (600.0, 8.0));
                assert!(!panel.guide_mouse_down(&ruler, cx), "and so does the ruler");
                assert!(panel.guide_drag.is_none());
                assert_eq!(
                    panel.pointer_hint_at(window_point(panel, (500.0, 400.0)), cx),
                    Some(ViewerPointerHint::Empty),
                    "a mark that cannot be grabbed promises no cursor"
                );

                // The line is still drawn and still pulls.
                let ctx = panel.overlay_context(cx);
                assert!(overlay::ViewerOverlay::is_active(
                    &guides::GuideOverlay,
                    &ctx
                ));
                assert!(
                    SnapLines::collect(&ctx, Some(comp_id), &[], None)
                        .x
                        .contains(&500.0),
                    "locking withdraws no candidate"
                );

                panel.guides_locked = false;
                assert_eq!(
                    panel.pointer_hint_at(window_point(panel, (500.0, 400.0)), cx),
                    Some(ViewerPointerHint::ResizeLeftRight),
                    "unlocked, the cursor names the axis the drag writes"
                );
            })
            .unwrap();
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(500.0)]
        );
    }

    /// "Clear guides" is a deletion, and locking forbids deletions: it leaves
    /// the guides alone and pushes no undo step.
    #[gpui::test]
    fn clearing_the_guides_is_refused_while_they_are_locked(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(
            &project,
            comp_id,
            vec![Guide::vertical(500.0), Guide::horizontal(300.0)],
            cx,
        );
        let placed = guides_of(&project, comp_id, cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.guides_locked = true;
                panel.clear_guides(cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            guides_of(&project, comp_id, cx),
            placed,
            "a locked guide is not deleted"
        );
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "and no undo step was pushed: the next undo is the one that placed them"
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.guides_locked = false;
                panel.clear_guides(cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            guides_of(&project, comp_id, cx).is_empty(),
            "unlocked, clearing works"
        );
    }

    /// A guide is drawn across the composition frame only, so it is grabbable
    /// there only: the letterbox around the picture holds no line to pick up.
    #[gpui::test]
    fn a_guide_is_not_grabbable_outside_the_composition_frame(cx: &mut TestAppContext) {
        let (window, project, comp_id, _layer) = shell_setup(cx);
        set_guides(&project, comp_id, vec![Guide::vertical(500.0)], cx);

        window
            .update(cx, |panel, _window, cx| {
                // On the line, inside the 1920x1080 frame.
                assert_eq!(
                    panel.guide_axis_at((500.0, 400.0), cx),
                    Some(GuideAxis::Vertical)
                );
                // On the same line, below the frame: nothing is drawn here.
                assert_eq!(panel.guide_axis_at((500.0, 1200.0), cx), None);
                assert_eq!(panel.guide_axis_at((500.0, -200.0), cx), None);

                let outside = press_at(panel, (500.0, 1200.0));
                assert!(
                    !panel.guide_mouse_down(&outside, cx),
                    "and the press is not taken there"
                );
                assert!(panel.guide_drag.is_none());
            })
            .unwrap();
        assert_eq!(
            guides_of(&project, comp_id, cx),
            vec![Guide::vertical(500.0)]
        );
    }

    /// The ruler is a `Select` gesture: a drawing tool's press over the strip
    /// belongs to the drawing tool.
    #[gpui::test]
    fn another_tool_keeps_its_press_over_the_ruler(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _layer) = shell_setup(cx);
        cx.update(|cx| {
            cx.set_global(ToolState {
                active: ravel_ui::ToolKind::Rect,
                ..ToolState::default()
            })
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.show_rulers = true;
                let press = press_at(panel, (600.0, 8.0));
                assert!(!panel.guide_mouse_down(&press, cx));
                assert!(panel.guide_drag.is_none());
            })
            .unwrap();
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

    /// The gestures that give Alt a meaning of their own keep snapping while it
    /// is held: drawing from the centre still lands its corner on a candidate.
    #[gpui::test]
    fn alt_and_snapping_coexist_while_drawing_from_the_centre(cx: &mut TestAppContext) {
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
                    position: window_point(panel, (955.0, 600.0)),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers {
                        alt: true,
                        ..Modifiers::default()
                    },
                };
                panel.shape_dragged(&event, cx);
                assert_eq!(panel.snap_guides.x, Some(960.0), "Alt does not suppress");
                let geo = panel
                    .shape_drag
                    .as_ref()
                    .and_then(|drag| drag.created.as_ref())
                    .expect("the drag created a shape")
                    .geo;
                assert!(
                    (geo.center.0 - 500.0).abs() < 1e-3,
                    "and Alt still draws from the press point: {geo:?}"
                );
                assert!(
                    (geo.half.0 - 460.0).abs() < 1e-3,
                    "the half extent reaches the snapped 960, not the pointer's 955: {geo:?}"
                );
            })
            .unwrap();
    }

    /// The same for the shell: Alt scales about the anchor and the grabbed grip
    /// is still pulled onto the candidate.
    #[gpui::test]
    fn alt_and_snapping_coexist_while_scaling_about_the_anchor(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.overlay_handle_mouse_down(&press_at(panel, SE_GRIP), cx));
                let to = window_point(panel, (955.0, 210.0));
                panel.handle_dragged(
                    to,
                    DragModifiers {
                        alt: true,
                        ..DragModifiers::default()
                    },
                    cx,
                );
                assert_eq!(panel.snap_guides.x, Some(960.0), "Alt does not suppress");
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();

        // Alt pins the anchor at the layer's origin, so a grip landing on 960
        // is a factor of 960 / 120 rather than the 22 the opposite corner gives.
        let scaled = shell_scale(&project, comp_id, layer, cx);
        assert!(
            (scaled.0 - 8.0).abs() < 1e-3,
            "the anchor stayed the fixed point: {scaled:?}"
        );
    }

    /// The suppression key is the platform's primary modifier — Cmd on macOS,
    /// Ctrl elsewhere — and Shift and Alt reach the gesture untouched.
    #[test]
    fn drag_modifiers_take_snapping_off_the_platform_primary_key() {
        let held = |modifiers: Modifiers| drag_modifiers(&modifiers);

        let platform = held(Modifiers {
            platform: true,
            ..Modifiers::default()
        });
        assert!(platform.primary, "Cmd suppresses the pull");
        assert!(!platform.shift && !platform.alt);

        assert!(
            held(Modifiers {
                control: true,
                ..Modifiers::default()
            })
            .primary,
            "and so does Ctrl, which is the same key off macOS"
        );

        // The two that already mean something to a gesture are not it.
        let alt = held(Modifiers {
            alt: true,
            ..Modifiers::default()
        });
        assert!(alt.alt && !alt.primary, "Alt draws from the centre");
        let shift = held(Modifiers {
            shift: true,
            ..Modifiers::default()
        });
        assert!(shift.shift && !shift.primary, "Shift constrains");
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
                cx.try_global::<overlay::EvalResults>()
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
            cx.try_global::<overlay::EvalResults>()
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

    /// End to end for the radial tools: the press fixes the centre and the
    /// pointer's distance from it becomes the outer radius on the created
    /// node — and no snap correction gets between the two.
    #[gpui::test]
    fn a_radial_drag_writes_the_drag_distance_as_the_outer_radius(cx: &mut TestAppContext) {
        for (tool, kind, radius_key) in [
            (
                ravel_ui::ToolKind::Polygon,
                ShapeDrawKind::Polygon,
                "radius",
            ),
            (
                ravel_ui::ToolKind::Star,
                ShapeDrawKind::Star,
                "outer_radius",
            ),
        ] {
            let (window, project, comp_id, layer) = shell_setup(cx);
            let network = NetworkPath::layer(comp_id, layer);
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(network.clone()),
                    nodes: HashSet::new(),
                });
                cx.set_global(ToolState {
                    active: tool,
                    ..ToolState::default()
                });
            });

            let node = window
                .update(cx, |panel, _window, cx| {
                    panel.shape_mouse_down(&press_at(panel, (655.0, 500.0)), cx);
                    let event = MouseMoveEvent {
                        // A 300 / 400 delta, so the distance (500) is neither
                        // delta nor their sum. The x also lands five units
                        // short of the composition centre at 960 — a candidate
                        // a box drag would be pulled onto, which would make
                        // the radius 503.2 instead.
                        position: window_point(panel, (955.0, 900.0)),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    };
                    panel.shape_dragged(&event, cx);
                    assert_eq!(
                        panel.snap_guides.x, None,
                        "{tool:?}: a radial drag has no edge to snap"
                    );
                    let created = panel
                        .shape_drag
                        .as_ref()
                        .and_then(|drag| drag.created.as_ref())
                        .expect("the drag created a shape");
                    let geo = created.geo;
                    assert!(
                        (geo.center.0 - 655.0).abs() < 1e-3 && (geo.center.1 - 500.0).abs() < 1e-3,
                        "{tool:?}: the press point stayed the centre: {geo:?}"
                    );
                    assert!(
                        (geo.half.0 - 500.0).abs() < 1e-3,
                        "{tool:?}: the radius is the drag distance, not the delta: {geo:?}"
                    );
                    let node = created.node;
                    panel.shape_ended(cx);
                    assert!(panel.shape_drag.is_none(), "{tool:?}: the release ended it");
                    node
                })
                .unwrap();

            // On the committed document, not just the drag state.
            project.read_with(cx, |project, _| {
                let graph = ravel_ui::document::resolve_network(project.document(), &network)
                    .expect("the layer network survived the commit");
                let node = graph.node(node).expect("the drawn node was committed");
                assert_eq!(node.type_key, kind.type_key());
                assert_eq!(
                    sample_float_param(node, radius_key, 0, &eval_ctx()),
                    Some(500.0),
                    "{tool:?}: the committed radius is the drag distance"
                );
            });
        }
    }

    /// A press that never moves creates nothing: no node, no layer, and the
    /// selection is the one the press found — the same rule the box tools keep.
    #[gpui::test]
    fn a_radial_press_without_a_move_leaves_the_document_alone(cx: &mut TestAppContext) {
        for tool in [ravel_ui::ToolKind::Polygon, ravel_ui::ToolKind::Star] {
            let (window, project, comp_id, layer) = shell_setup(cx);
            let network = NetworkPath::layer(comp_id, layer);
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(network.clone()),
                    nodes: HashSet::new(),
                });
                cx.set_global(ToolState {
                    active: tool,
                    ..ToolState::default()
                });
            });
            let before = project.read_with(cx, |project, _| project.document().clone());

            window
                .update(cx, |panel, _window, cx| {
                    panel.shape_mouse_down(&press_at(panel, (500.0, 500.0)), cx);
                    assert!(panel.shape_drag.is_some(), "{tool:?}: the press is pending");
                    panel.shape_ended(cx);
                })
                .unwrap();

            project.read_with(cx, |project, _| {
                let nodes = |doc: &Document| {
                    ravel_ui::document::resolve_network(doc, &network)
                        .map(|graph| graph.nodes().count())
                };
                assert_eq!(
                    nodes(project.document()),
                    nodes(&before),
                    "{tool:?}: a click created a node"
                );
                assert_eq!(
                    project
                        .document()
                        .get_composition(comp_id)
                        .unwrap()
                        .layer_count(),
                    before.get_composition(comp_id).unwrap().layer_count(),
                    "{tool:?}: a click created a layer"
                );
            });
        }
    }

    /// A drag that returns to the press point releases at zero radius, which
    /// commits nothing: the invisible shape and any layer created for it are
    /// rolled back.
    #[gpui::test]
    fn a_zero_distance_radial_release_commits_nothing(cx: &mut TestAppContext) {
        for tool in [ravel_ui::ToolKind::Polygon, ravel_ui::ToolKind::Star] {
            let (window, project, comp_id, layer) = shell_setup(cx);
            let network = NetworkPath::layer(comp_id, layer);
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(network.clone()),
                    nodes: HashSet::new(),
                });
                cx.set_global(ToolState {
                    active: tool,
                    ..ToolState::default()
                });
            });
            let before = project.read_with(cx, |project, _| {
                ravel_ui::document::resolve_network(project.document(), &network)
                    .map(|graph| graph.nodes().count())
            });

            window
                .update(cx, |panel, _window, cx| {
                    panel.shape_mouse_down(&press_at(panel, (500.0, 500.0)), cx);
                    // A real move, so the node is created …
                    let moved = MouseMoveEvent {
                        position: window_point(panel, (600.0, 500.0)),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    };
                    panel.shape_dragged(&moved, cx);
                    assert!(
                        panel
                            .shape_drag
                            .as_ref()
                            .is_some_and(|drag| drag.created.is_some()),
                        "{tool:?}: the first move creates the node"
                    );
                    // … then back onto the press point, so the radius is zero.
                    let back = MouseMoveEvent {
                        position: window_point(panel, (500.0, 500.0)),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    };
                    panel.shape_dragged(&back, cx);
                    panel.shape_ended(cx);
                    assert!(panel.shape_drag.is_none(), "{tool:?}: the gesture is over");
                })
                .unwrap();

            project.read_with(cx, |project, _| {
                assert_eq!(
                    ravel_ui::document::resolve_network(project.document(), &network)
                        .map(|graph| graph.nodes().count()),
                    before,
                    "{tool:?}: a zero-radius release left a node behind"
                );
                assert_eq!(
                    project
                        .document()
                        .get_composition(comp_id)
                        .unwrap()
                        .layer_count(),
                    1,
                    "{tool:?}: a zero-radius release left a layer behind"
                );
            });
        }
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
            ravel_ui::ToolKind::Polygon,
            ravel_ui::ToolKind::Star,
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

    /// **Moving the pointer must not evaluate anything** (`INSP-3`).
    ///
    /// The readout indexes the frame the panel already holds, so a pointer
    /// move costs a lookup and a `format!`. The observation point is the one
    /// `the_display_channel_never_changes_the_evaluation_request` uses — the
    /// count of viewer evaluation requests — because that is what an overlay
    /// declaring an evaluation target, or a readout implemented by asking the
    /// worker, would move.
    ///
    /// Switching the readout on is the one thing here that *does* cost an
    /// evaluation, and exactly one: the finished frames already in the cache
    /// carry no float source, so they have to be finalized again.
    #[gpui::test]
    fn moving_the_pointer_never_asks_for_an_evaluation(cx: &mut TestAppContext) {
        let (window, project, _comp, _layer) = shell_setup(cx);

        let before = project.update(cx, |project, _| project.viewer_eval_requests());
        project.update(cx, |project, cx| project.set_pixel_readout(true, cx));
        let after_switch = project.update(cx, |project, _| project.viewer_eval_requests());
        assert_eq!(
            after_switch,
            before + 1,
            "switching the readout on must cost exactly one re-finalize"
        );
        // Already on: nothing to redo.
        project.update(cx, |project, cx| project.set_pixel_readout(true, cx));
        assert_eq!(
            project.update(cx, |project, _| project.viewer_eval_requests()),
            after_switch,
            "switching the readout to the state it is already in re-evaluated"
        );

        window
            .update(cx, |panel, _window, cx| {
                // A frame arrived while the readout was on, so the panel holds
                // its float source.
                panel.linear = Some(Arc::new(FrameBuffer::from_f32(
                    960,
                    540,
                    vec![0.5; 960 * 540 * 4],
                )));
                for x in 0..64 {
                    let comp = (x as f32 * 30.0, x as f32 * 16.0);
                    panel.track_readout_pointer(window_point(panel, comp), cx);
                }
                assert!(
                    panel.readout_pointer.is_some(),
                    "the pointer was never tracked, so this proves nothing"
                );
                // The overlay is being fed at the last position, i.e. the
                // moves above exercised the path that would have evaluated.
                assert!(
                    panel.overlay_context(cx).pixel_readout.is_some(),
                    "the readout was never fed, so this proves nothing"
                );
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            project.update(cx, |project, _| project.viewer_eval_requests()),
            after_switch,
            "moving the pointer asked for an evaluation"
        );
    }

    /// With the readout off, a pointer move stores nothing — so it also
    /// notifies nothing, and the panel does not re-render per mouse move for a
    /// feature nobody switched on.
    #[gpui::test]
    fn the_pointer_is_not_tracked_while_the_readout_is_off(cx: &mut TestAppContext) {
        let (window, project, _comp, _layer) = shell_setup(cx);
        assert!(!project.update(cx, |project, _| project.pixel_readout()));

        window
            .update(cx, |panel, _window, cx| {
                panel.track_readout_pointer(window_point(panel, (100.0, 100.0)), cx);
                assert_eq!(panel.readout_pointer, None);
                assert!(
                    panel.overlay_context(cx).pixel_readout.is_none(),
                    "the readout overlay was fed with the readout switched off"
                );
            })
            .unwrap();
    }

    /// A pointer outside the canvas area has no readout, even when the
    /// composition is zoomed far enough in to reach under the toolbar: the
    /// panel is not drawing those pixels, so it must not report them.
    #[gpui::test]
    fn a_pointer_off_the_canvas_area_is_not_tracked(cx: &mut TestAppContext) {
        let (window, project, _comp, _layer) = shell_setup(cx);
        project.update(cx, |project, cx| project.set_pixel_readout(true, cx));

        window
            .update(cx, |panel, _window, cx| {
                panel.track_readout_pointer(window_point(panel, (100.0, 100.0)), cx);
                assert!(panel.readout_pointer.is_some());
                // Below the canvas: the fixture's canvas is 1920x1080 at the
                // window origin, so this is under it.
                panel.track_readout_pointer(point(px(100.0), px(2000.0)), cx);
                assert_eq!(panel.readout_pointer, None);
            })
            .unwrap();
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

    // -----------------------------------------------------------------------
    // Hand / Zoom tools (`TOOLX-1`)
    // -----------------------------------------------------------------------

    const FIXTURE_RES: (u32, u32) = (1920, 1080);

    /// The window position of a panel-local pixel, read from the panel's
    /// *current* viewport origin. The fixture's canvas moves as soon as the
    /// window lays out for real, so a hardcoded window pixel drifts.
    fn local_point(panel: &ViewerPanel, local: (f32, f32)) -> Point<Pixels> {
        let origin = panel.viewport_origin.get();
        point(px(local.0 + origin.0), px(local.1 + origin.1))
    }

    fn press_local(panel: &ViewerPanel, local: (f32, f32), modifiers: Modifiers) -> MouseDownEvent {
        MouseDownEvent {
            button: MouseButton::Left,
            position: local_point(panel, local),
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
    }

    fn drag_local(panel: &ViewerPanel, local: (f32, f32)) -> MouseMoveEvent {
        MouseMoveEvent {
            position: local_point(panel, local),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }
    }

    fn tool_state(active: ravel_ui::ToolKind) -> ToolState {
        ToolState {
            active,
            ..ToolState::default()
        }
    }

    /// Hand's left drag pans, and the temporary hand (`h` held) goes down the
    /// same path as the toolbar hand — one `PanDrag`, one offset.
    #[gpui::test]
    fn the_hand_tool_pans_with_the_left_button(cx: &mut TestAppContext) {
        for hand_hold in [false, true] {
            // `_project` keeps the entity alive: `ProjectStateHandle` is weak.
            let (window, _project, ..) = shell_setup(cx);
            cx.update(|cx| {
                cx.set_global(ToolState {
                    active: ravel_ui::ToolKind::Hand,
                    hand_hold,
                    previous: ravel_ui::ToolKind::Select,
                })
            });

            window
                .update(cx, |panel, _window, cx| {
                    let panel_size = panel.viewport_size.get();
                    let before = panel.viewport.rect(panel_size, FIXTURE_RES);
                    let press = press_local(panel, (400.0, 300.0), Modifiers::default());
                    panel.left_mouse_down(&press, cx);
                    assert!(panel.pan_drag.is_some(), "hand_hold = {hand_hold}");
                    let moved = drag_local(panel, (460.0, 280.0));
                    panel.left_dragged(&moved, cx);

                    let after = panel.viewport.rect(panel_size, FIXTURE_RES);
                    assert_eq!(
                        (after.x - before.x, after.y - before.y),
                        (60.0, -20.0),
                        "the picture follows the pointer (hand_hold = {hand_hold})"
                    );

                    panel.pan_ended(cx);
                    assert!(panel.pan_drag.is_none());
                })
                .unwrap();
        }
    }

    /// The middle button pans under *every* tool. `pan_mouse_down` is the one
    /// entry point the middle-button listener calls unconditionally, so it
    /// must not consult the active tool.
    #[gpui::test]
    fn the_middle_button_pans_under_every_tool(cx: &mut TestAppContext) {
        for tool in [
            ravel_ui::ToolKind::Select,
            ravel_ui::ToolKind::Pen,
            ravel_ui::ToolKind::Rect,
            ravel_ui::ToolKind::Ellipse,
            ravel_ui::ToolKind::Polygon,
            ravel_ui::ToolKind::Star,
            ravel_ui::ToolKind::Hand,
            ravel_ui::ToolKind::Zoom,
        ] {
            let (window, _project, ..) = shell_setup(cx);
            cx.update(|cx| cx.set_global(tool_state(tool)));

            window
                .update(cx, |panel, _window, cx| {
                    let panel_size = panel.viewport_size.get();
                    let before = panel.viewport.rect(panel_size, FIXTURE_RES);
                    let press = MouseDownEvent {
                        button: MouseButton::Middle,
                        position: local_point(panel, (400.0, 300.0)),
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    panel.pan_mouse_down(&press, cx);
                    let moved = local_point(panel, (370.0, 310.0));
                    assert!(panel.pan_dragged(moved, cx));

                    let after = panel.viewport.rect(panel_size, FIXTURE_RES);
                    assert_eq!(
                        (after.x - before.x, after.y - before.y),
                        (-30.0, 10.0),
                        "the middle button pans under {tool:?}"
                    );
                })
                .unwrap();
        }
    }

    /// The temporary hand still hands the tool back. This unit adds the pan the
    /// hold performs; the transition itself must be untouched.
    #[gpui::test]
    fn the_hand_hold_returns_the_previous_tool(cx: &mut TestAppContext) {
        let (window, _project, ..) = shell_setup(cx);
        cx.update(|cx| cx.set_global(tool_state(ravel_ui::ToolKind::Rect)));
        window
            .update(cx, |panel, window, cx| {
                panel.focus_handle.focus(window, cx);
            })
            .unwrap();

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, _cx| window.refresh());
        visual.run_until_parked();

        let h = || Keystroke {
            modifiers: Modifiers::default(),
            key: "h".to_string(),
            key_char: Some("h".to_string()),
        };

        visual.simulate_event(KeyDownEvent {
            keystroke: h(),
            is_held: false,
            prefer_character_input: false,
        });
        let held = visual.update(|_window, cx| cx.global::<ToolState>().clone());
        assert_eq!(held.active, ravel_ui::ToolKind::Hand);
        assert!(held.hand_hold);
        assert_eq!(held.previous, ravel_ui::ToolKind::Rect);

        visual.simulate_event(KeyUpEvent { keystroke: h() });
        let released = visual.update(|_window, cx| cx.global::<ToolState>().clone());
        assert_eq!(
            released.active,
            ravel_ui::ToolKind::Rect,
            "releasing `h` gives the tool back"
        );
        assert!(!released.hand_hold);
    }

    /// A Zoom click magnifies around the pointer — the same anchor rule the
    /// scroll wheel uses — by one step of the wheel's own multiplier ladder.
    /// `Alt` on the press turns the same click into the reciprocal.
    #[gpui::test]
    fn the_zoom_tool_click_magnifies_around_the_pointer(cx: &mut TestAppContext) {
        for alt in [false, true] {
            let (window, _project, ..) = shell_setup(cx);
            cx.update(|cx| cx.set_global(tool_state(ravel_ui::ToolKind::Zoom)));

            window
                .update(cx, |panel, _window, cx| {
                    let panel_size = panel.viewport_size.get();
                    let pointer = local_point(panel, (400.0, 300.0));
                    let before_comp = panel.comp_position(pointer).expect("inside the comp");
                    let before_zoom = panel.viewport.zoom(panel_size, FIXTURE_RES);

                    let press = press_local(
                        panel,
                        (400.0, 300.0),
                        Modifiers {
                            alt,
                            ..Modifiers::default()
                        },
                    );
                    panel.left_mouse_down(&press, cx);
                    assert!(panel.zoom_drag.is_some(), "the press only records");
                    assert_eq!(
                        panel.viewport.zoom(panel_size, FIXTURE_RES),
                        before_zoom,
                        "nothing zooms until the release"
                    );
                    panel.zoom_ended(cx);

                    let after_zoom = panel.viewport.zoom(panel_size, FIXTURE_RES);
                    let expected = if alt {
                        zoom_factor(ZOOM_CLICK_TRAVEL)
                    } else {
                        zoom_factor(-ZOOM_CLICK_TRAVEL)
                    };
                    assert!(
                        (after_zoom / before_zoom - expected).abs() < 1e-4,
                        "alt = {alt}: {after_zoom} / {before_zoom} is not {expected}"
                    );
                    if alt {
                        assert!(after_zoom < before_zoom, "Alt zooms out");
                    } else {
                        assert!(after_zoom > before_zoom, "a plain click zooms in");
                    }

                    let after_comp = panel.comp_position(pointer).expect("inside the comp");
                    assert!(
                        (after_comp.0 - before_comp.0).abs() < 0.01
                            && (after_comp.1 - before_comp.1).abs() < 0.01,
                        "the same composition pixel stays under the pointer: \
                         {before_comp:?} became {after_comp:?}"
                    );
                })
                .unwrap();
        }
    }

    /// A drag frames the rectangle it swept; a press that barely moved is a
    /// click, not a rectangle two pixels tall blown up to fill the panel.
    #[gpui::test]
    fn a_zoom_drag_frames_the_rectangle_and_a_tremor_is_a_click(cx: &mut TestAppContext) {
        let (window, _project, ..) = shell_setup(cx);
        cx.update(|cx| cx.set_global(tool_state(ravel_ui::ToolKind::Zoom)));

        window
            .update(cx, |panel, _window, cx| {
                let panel_size = panel.viewport_size.get();
                let before = panel.viewport.rect(panel_size, FIXTURE_RES);
                let drag = (500.0, 300.0);
                let press = press_local(panel, (400.0, 300.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                let moved = drag_local(panel, (400.0 + drag.0, 300.0 + drag.1));
                panel.left_dragged(&moved, cx);
                panel.zoom_ended(cx);

                let after = panel.viewport.rect(panel_size, FIXTURE_RES);
                assert!(
                    (after.width / after.height - before.width / before.height).abs() < 1e-3,
                    "the rectangle zoom kept the composition's aspect ratio: \
                     {before:?} became {after:?}"
                );

                // One scale for both axes, so the framed rectangle grows by the
                // same factor the picture did. It has to end up inside the
                // panel and flush against it on the limiting axis: contained,
                // not cropped, and not left short of a fit either.
                let scale = after.width / before.width;
                let framed = (drag.0 * scale, drag.1 * scale);
                assert!(
                    framed.0 <= panel_size.0 + 0.01 && framed.1 <= panel_size.1 + 0.01,
                    "the rectangle is contained: {framed:?} in {panel_size:?}"
                );
                assert!(
                    (framed.0 - panel_size.0).abs() < 0.01
                        || (framed.1 - panel_size.1).abs() < 0.01,
                    "the rectangle fills the panel on one axis: \
                     {framed:?} in {panel_size:?}"
                );

                // And its centre is the panel's centre.
                let centre = (
                    (after.x + (400.0 - before.x) / before.width * after.width) + framed.0 * 0.5,
                    (after.y + (300.0 - before.y) / before.height * after.height) + framed.1 * 0.5,
                );
                assert!(
                    (centre.0 - panel_size.0 * 0.5).abs() < 0.01
                        && (centre.1 - panel_size.1 * 0.5).abs() < 0.01,
                    "the framed centre landed at {centre:?}, not the panel centre"
                );
            })
            .unwrap();

        let (window, _project, ..) = shell_setup(cx);
        cx.update(|cx| cx.set_global(tool_state(ravel_ui::ToolKind::Zoom)));
        window
            .update(cx, |panel, _window, cx| {
                let panel_size = panel.viewport_size.get();
                let before = panel.viewport.zoom(panel_size, FIXTURE_RES);
                let press = press_local(panel, (400.0, 300.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                let moved = drag_local(panel, (404.0, 302.0));
                panel.left_dragged(&moved, cx);
                panel.zoom_ended(cx);

                let after = panel.viewport.zoom(panel_size, FIXTURE_RES);
                assert!(
                    (after / before - zoom_factor(-ZOOM_CLICK_TRAVEL)).abs() < 1e-4,
                    "a four-pixel drag is a click zoom, got {after} from {before}"
                );
            })
            .unwrap();
    }

    /// Hand and Zoom own the press outright: it reaches no overlay handle, no
    /// selection and no shape drag. The `Select` and `Rect` rows are the
    /// control — they prove the same press does start those gestures.
    #[gpui::test]
    fn the_navigation_tools_take_the_press_from_every_editing_gesture(cx: &mut TestAppContext) {
        for tool in [
            ravel_ui::ToolKind::Select,
            ravel_ui::ToolKind::Rect,
            ravel_ui::ToolKind::Hand,
            ravel_ui::ToolKind::Zoom,
        ] {
            let (window, _project, ..) = shell_setup(cx);
            cx.update(|cx| cx.set_global(tool_state(tool)));

            window
                .update(cx, |panel, _window, cx| {
                    let press = press_at(panel, SE_GRIP);
                    panel.left_mouse_down(&press, cx);
                    let navigating =
                        matches!(tool, ravel_ui::ToolKind::Hand | ravel_ui::ToolKind::Zoom);
                    assert_eq!(
                        panel.handle_drag.is_some(),
                        tool == ravel_ui::ToolKind::Select,
                        "{tool:?} and the shell scale grip"
                    );
                    assert_eq!(
                        panel.shape_drag.is_some(),
                        tool == ravel_ui::ToolKind::Rect,
                        "{tool:?} and the shape drag"
                    );
                    assert!(panel.move_drag.is_none());
                    assert!(panel.pen_session.is_none());
                    assert!(panel.guide_drag.is_none());
                    assert_eq!(
                        panel.pan_drag.is_some(),
                        tool == ravel_ui::ToolKind::Hand,
                        "{tool:?} and the pan"
                    );
                    assert_eq!(
                        panel.zoom_drag.is_some(),
                        tool == ravel_ui::ToolKind::Zoom,
                        "{tool:?} and the zoom"
                    );
                    assert!(
                        !navigating || panel.handle_drag.is_none(),
                        "a navigation tool never grabs a handle"
                    );
                    // And the cursor says so: every handle-bearing overlay
                    // gates itself on `Select`, so the hint over the grip is
                    // the tool's own — the press and the promise agree.
                    if navigating {
                        assert_eq!(
                            panel.pointer_hint_at(window_point(panel, SE_GRIP), cx),
                            Some(tool_pointer_hint(tool)),
                            "{tool:?} must promise its own cursor over the shell grip"
                        );
                    }
                })
                .unwrap();
        }
    }

    /// The cursor a tool promises. `done/pointer-feedback-plan.md` held these
    /// two back until the gestures existed; they exist now.
    #[test]
    fn the_navigation_tools_promise_their_own_cursors() {
        assert_eq!(
            tool_pointer_hint(ravel_ui::ToolKind::Hand).cursor(),
            CursorStyle::OpenHand
        );
        assert_eq!(
            tool_pointer_hint(ravel_ui::ToolKind::Zoom).cursor(),
            CursorStyle::Crosshair
        );
        // A press closes the hand, whichever gesture opened the pan.
        assert_eq!(
            viewer_drag_cursor(true, false, false, false, None, None),
            Some(CursorStyle::ClosedHand)
        );
        // And the editing tools keep the cursors they had.
        assert_eq!(
            tool_pointer_hint(ravel_ui::ToolKind::Select).cursor(),
            CursorStyle::Arrow
        );
        assert_eq!(
            tool_pointer_hint(ravel_ui::ToolKind::Rect).cursor(),
            CursorStyle::Crosshair
        );
    }

    /// The click / rectangle split, and the rectangle normalized whichever way
    /// the drag was swept.
    #[test]
    fn a_zoom_press_becomes_a_rectangle_only_once_both_axes_clear_the_floor() {
        let click = ZoomDrag {
            start: (10.0, 10.0),
            current: (14.0, 40.0),
            zoom_out: false,
        };
        assert!(
            click.rect().is_none(),
            "one axis under the floor is still a click"
        );

        let swept_up_left = ZoomDrag {
            start: (40.0, 40.0),
            current: (10.0, 10.0),
            zoom_out: false,
        };
        assert_eq!(
            swept_up_left.rect(),
            Some(viewport::Rect {
                x: 10.0,
                y: 10.0,
                width: 30.0,
                height: 30.0,
            })
        );
    }

    // -----------------------------------------------------------------------
    // Box selection (`TOOLX-2`)
    // -----------------------------------------------------------------------

    /// Completion criterion: the rectangle picks by **intersection**, and the
    /// boundary is closed — rectangles that merely touch count as meeting.
    #[test]
    fn a_box_catches_what_it_touches_boundary_included() {
        let bbox = CompRect {
            x: 50.0,
            y: 50.0,
            w: 100.0,
            h: 100.0,
        };
        let sweep = |x: f32, y: f32, w: f32, h: f32| rects_overlap(&bbox, &CompRect { x, y, w, h });

        // Crossing, and fully inside — neither is containment of the bbox.
        assert!(sweep(100.0, 100.0, 200.0, 200.0));
        assert!(sweep(60.0, 60.0, 10.0, 10.0));
        // Larger than the bbox on every side.
        assert!(sweep(0.0, 0.0, 300.0, 300.0));
        // Edge-to-edge on each side: touching is meeting.
        assert!(sweep(0.0, 50.0, 50.0, 10.0), "left edge touches");
        assert!(sweep(150.0, 50.0, 50.0, 10.0), "right edge touches");
        assert!(sweep(50.0, 0.0, 10.0, 50.0), "top edge touches");
        assert!(sweep(50.0, 150.0, 10.0, 50.0), "bottom edge touches");
        // Corner-to-corner, the tightest touch there is.
        assert!(sweep(0.0, 0.0, 50.0, 50.0), "corners touch");
        // One unit past each edge: no longer meeting.
        assert!(!sweep(-50.0, 50.0, 99.0, 10.0));
        assert!(!sweep(151.0, 50.0, 50.0, 10.0));
        assert!(!sweep(50.0, -50.0, 10.0, 99.0));
        assert!(!sweep(50.0, 151.0, 10.0, 50.0));
        // A zero-extent sweep is still a rectangle, and a zero-extent bbox
        // (one placed point) is still catchable.
        assert!(sweep(100.0, 100.0, 0.0, 0.0));
        assert!(rects_overlap(
            &CompRect {
                x: 60.0,
                y: 60.0,
                w: 0.0,
                h: 0.0
            },
            &bbox
        ));
    }

    /// A swept rectangle is the same rectangle whichever way it was swept.
    #[test]
    fn a_box_is_normalized_in_both_directions() {
        let forward = box_rect((10.0, 20.0), (40.0, 60.0));
        let backward = box_rect((40.0, 60.0), (10.0, 20.0));
        assert_eq!(forward, backward);
        assert_eq!(
            (forward.x, forward.y, forward.w, forward.h),
            (10.0, 20.0, 30.0, 40.0)
        );
    }

    /// Completion criterion (`LOW-APP-03`): Shift keeps the selection the drag
    /// started from and adds the sweep to it. Without Shift the sweep replaces.
    #[test]
    fn a_shift_box_publishes_the_union_and_a_plain_box_replaces() {
        let (first, second, third) = (NodeId::next(), NodeId::next(), NodeId::next());
        let initial = HashSet::from([first, second]);

        assert_eq!(
            nodes_after_box(&initial, HashSet::from([third]), true),
            HashSet::from([first, second, third]),
            "Shift adds the sweep to the press-time selection"
        );
        assert_eq!(
            nodes_after_box(&initial, HashSet::new(), true),
            initial,
            "a Shift sweep that caught nothing keeps everything"
        );
        assert_eq!(
            nodes_after_box(&initial, HashSet::from([third]), false),
            HashSet::from([third]),
            "without Shift the sweep is the whole selection"
        );
        assert!(nodes_after_box(&initial, HashSet::new(), false).is_empty());

        let layers = vec![LayerId::next(), LayerId::next()];
        let swept = vec![layers[1], LayerId::next()];
        assert_eq!(
            layers_after_box(&layers, &swept, true),
            vec![layers[0], layers[1], swept[1]],
            "Shift keeps click order and adds each layer once"
        );
        assert_eq!(layers_after_box(&layers, &swept, false), swept);
        assert_eq!(layers_after_box(&layers, &[], true), layers);
        assert!(layers_after_box(&layers, &[], false).is_empty());
    }

    /// Completion criterion: the candidate bboxes are declared **only while
    /// the drag is live**. Nothing is selected in this context, so no other
    /// overlay asks for anything — which is exactly the state a box drag has
    /// to work from, and why declaring them permanently would evaluate every
    /// network of the composition on every frame.
    #[test]
    fn candidate_targets_are_declared_only_while_the_box_drag_lives() {
        let node = square_node((50.0, 50.0), 40.0);
        let (mut ctx, network) = geometry_context(Graph::new().add_node(node).unwrap(), &[]);
        let registry = OverlayRegistry::builtin();
        assert!(
            registry.eval_targets(&ctx).is_empty(),
            "with nothing selected and no drag, nobody asks for geometry"
        );

        let sweep = Some(CompRect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        });
        ctx.box_select = Some(BoxSelect {
            scope: BoxSelectScope::Nodes(network.clone()),
            rect: sweep,
        });
        assert_eq!(
            registry.eval_targets(&ctx),
            geometry::geometry_targets(ctx.document.as_ref().unwrap(), &network),
            "the open network's geometry nodes are the candidates"
        );

        // No network open: every layer of the composition is a candidate.
        ctx.box_select = Some(BoxSelect {
            scope: BoxSelectScope::Layers(network.comp),
            rect: sweep,
        });
        assert_eq!(
            registry.eval_targets(&ctx),
            geometry::geometry_targets(ctx.document.as_ref().unwrap(), &network),
            "the fixture's one layer is the whole candidate set"
        );

        ctx.box_select = None;
        assert!(
            registry.eval_targets(&ctx).is_empty(),
            "the release stops the declaration"
        );
    }

    /// Completion criterion: the marquee is drawn through the overlay
    /// mechanism, so it is registered in the builtin registry and paints as
    /// composition-space primitives.
    #[test]
    fn the_marquee_is_painted_by_the_registered_overlay() {
        let node = square_node((50.0, 50.0), 40.0);
        let (mut ctx, network) = geometry_context(Graph::new().add_node(node).unwrap(), &[]);
        let rect = CompRect {
            x: 100.0,
            y: 200.0,
            w: 300.0,
            h: 400.0,
        };
        ctx.box_select = Some(BoxSelect {
            scope: BoxSelectScope::Nodes(network),
            rect: Some(rect),
        });

        let frame = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1920.0), px(1080.0)),
        };
        let mut painter = OverlayPainter::new(frame, (1920, 1080));
        OverlayRegistry::builtin().paint(&ctx, &mut painter);
        let outline: Vec<_> = painter
            .finish()
            .into_iter()
            .filter_map(|primitive| match primitive {
                overlay::OverlayPrimitive::Quad { bounds, .. } => Some(bounds),
                overlay::OverlayPrimitive::Stroke { .. } => None,
            })
            .collect();
        assert_eq!(outline.len(), 4, "a 1px outline is four quads: {outline:?}");
        assert!(
            outline
                .iter()
                .any(|bounds| bounds.origin == point(px(rect.x), px(rect.y))
                    && bounds.size.width == px(rect.w)),
            "the top edge spans the swept rectangle: {outline:?}"
        );
    }

    /// A press at a composition point, with modifiers.
    fn press_comp(panel: &ViewerPanel, comp: (f32, f32), modifiers: Modifiers) -> MouseDownEvent {
        MouseDownEvent {
            button: MouseButton::Left,
            position: window_point(panel, comp),
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    /// A left-button move to a composition point.
    fn move_comp(panel: &ViewerPanel, comp: (f32, f32)) -> MouseMoveEvent {
        MouseMoveEvent {
            position: window_point(panel, comp),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }
    }

    fn selected_nodes(cx: &mut TestAppContext) -> HashSet<NodeId> {
        cx.update(|cx| {
            cx.try_global::<CanvasSelection>()
                .cloned()
                .unwrap_or_default()
                .nodes
        })
    }

    /// With a network open, a drag from empty space sweeps its **nodes**: the
    /// press declares the candidates, the release publishes what the rectangle
    /// caught.
    ///
    /// The fixture's one node is a 40x20 rect at (100, 200), so its bbox is
    /// (80, 190)-(120, 210).
    #[gpui::test]
    fn a_box_over_an_open_network_selects_the_nodes_it_caught(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        let network = NetworkPath::layer(comp_id, layer);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(network.clone()),
                nodes: HashSet::new(),
            })
        });
        let node = project.read_with(cx, |project, _| {
            project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer)
                .unwrap()
                .network
                .nodes()
                .next()
                .unwrap()
                .id
        });

        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (400.0, 400.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(
                    panel.box_select.is_some(),
                    "a press on empty space starts a sweep"
                );
                assert!(panel.move_drag.is_none(), "nothing was under the pointer");
                assert!(
                    !OverlayRegistry::builtin()
                        .eval_targets(&panel.overlay_context(cx))
                        .is_empty(),
                    "the live drag declares the candidate bboxes"
                );
                assert!(
                    overlay::box_select_candidates(cx).is_some(),
                    "and the request path can see the scope"
                );
            })
            .unwrap();
        // The press posted a request, and with no worker that clears the
        // snapshot: republish what an evaluation would have delivered.
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.left_dragged(&move_comp(panel, (60.0, 180.0)), cx);
                panel.box_select_ended(window_point(panel, (60.0, 180.0)), cx);
                assert!(panel.box_select.is_none(), "the release ends the gesture");
                assert!(
                    panel.overlay_context(cx).box_select.is_none(),
                    "the marquee stops being drawn"
                );
                assert!(
                    overlay::box_select_candidates(cx).is_none(),
                    "and the candidates stop being declared"
                );
            })
            .unwrap();
        assert_eq!(
            selected_nodes(cx),
            HashSet::from([node]),
            "the swept node is selected"
        );
    }

    /// A sweep that catches nothing selects nothing — and a Shift sweep that
    /// catches nothing keeps what was selected when it started
    /// (`LOW-APP-03`), even though the press itself published the deselection
    /// a click on empty space means.
    #[gpui::test]
    fn a_shift_box_keeps_the_selection_the_press_cleared(cx: &mut TestAppContext) {
        for (modifiers, expect_kept) in [(Modifiers::default(), false), (shift(), true)] {
            let (window, project, comp_id, layer) = shell_setup(cx);
            let network = NetworkPath::layer(comp_id, layer);
            let node = project.read_with(cx, |project, _| {
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layer)
                    .unwrap()
                    .network
                    .nodes()
                    .next()
                    .unwrap()
                    .id
            });
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(network.clone()),
                    nodes: HashSet::from([node]),
                })
            });

            window
                .update(cx, |panel, _window, cx| {
                    let press = press_comp(panel, (400.0, 400.0), modifiers);
                    panel.left_mouse_down(&press, cx);
                    assert!(panel.box_select.is_some());
                })
                .unwrap();
            assert!(
                selected_nodes(cx).is_empty(),
                "the press published the click's deselection"
            );
            publish_geometry_results(&project, cx);

            window
                .update(cx, |panel, _window, cx| {
                    // Nowhere near the fixture's (80, 190)-(120, 210) bbox.
                    panel.left_dragged(&move_comp(panel, (600.0, 600.0)), cx);
                    panel.box_select_ended(window_point(panel, (600.0, 600.0)), cx);
                })
                .unwrap();

            let expected = if expect_kept {
                HashSet::from([node])
            } else {
                HashSet::new()
            };
            assert_eq!(selected_nodes(cx), expected, "shift = {}", modifiers.shift);
        }
    }

    /// Completion criterion: a press that never travelled is a click, and the
    /// click's deselection stands. Publishing the union on a zero-distance
    /// Shift press would put the cleared selection straight back.
    #[gpui::test]
    fn a_zero_distance_box_leaves_the_click_deselection_alone(cx: &mut TestAppContext) {
        for modifiers in [Modifiers::default(), shift()] {
            let (window, project, comp_id, layer) = shell_setup(cx);
            let node = project.read_with(cx, |project, _| {
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layer)
                    .unwrap()
                    .network
                    .nodes()
                    .next()
                    .unwrap()
                    .id
            });
            cx.update(|cx| {
                cx.set_global(CanvasSelection {
                    path: Some(NetworkPath::layer(comp_id, layer)),
                    nodes: HashSet::from([node]),
                })
            });
            publish_geometry_results(&project, cx);

            window
                .update(cx, |panel, _window, cx| {
                    let press = press_comp(panel, (400.0, 400.0), modifiers);
                    panel.left_mouse_down(&press, cx);
                    panel.box_select_ended(window_point(panel, (400.0, 400.0)), cx);
                    assert!(panel.box_select.is_none());
                })
                .unwrap();
            assert!(
                selected_nodes(cx).is_empty(),
                "a click deselects, whatever Shift a drag would have meant (shift = {})",
                modifiers.shift
            );
        }
    }

    /// A press *on* a shape keeps the existing move gesture: the rectangle is
    /// what empty space means, not what the Select tool always means.
    #[gpui::test]
    fn a_press_on_a_shape_moves_it_instead_of_sweeping(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        cx.update(|cx| {
            // The shell manipulator's move grip sits on the bbox centre, and it
            // answers the press before any tool does. This test is about the
            // node gesture, so the layer selection that summons the manipulator
            // is cleared and the network stays open.
            crate::panels::clear_layer_selection(cx);
            cx.set_global(CanvasSelection {
                path: Some(NetworkPath::layer(comp_id, layer)),
                nodes: HashSet::new(),
            })
        });
        // Both selection writes above posted a request, and with no worker that
        // clears the snapshot the click test reads its bboxes from.
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                // The centre of the fixture's rect node.
                let press = press_comp(panel, (100.0, 200.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(
                    panel.move_drag.is_some(),
                    "a press on the shape starts the move gesture"
                );
                assert!(panel.box_select.is_none(), "and never a box selection");
            })
            .unwrap();
    }

    /// With no network open the sweep picks **layers**, and Shift adds them to
    /// the selection the press started from.
    ///
    /// `multi_layer_setup`'s two layers hold one 100x100 rect each, centred at
    /// (0, 0) and (100, 0): bboxes (-50, -50)-(50, 50) and (50, -50)-(150, 50).
    /// The sweep below starts past the first one's right edge, so it catches
    /// the second layer only.
    #[gpui::test]
    fn a_box_with_no_network_open_selects_layers(cx: &mut TestAppContext) {
        for (modifiers, expected) in [(Modifiers::default(), 1), (shift(), 2)] {
            let (window, project, _comp_id, layers) = multi_layer_setup(cx);
            cx.update(|cx| {
                crate::panels::set_layer_selection(vec![layers[0]], cx);
                cx.set_global(CanvasSelection::default());
            });

            window
                .update(cx, |panel, _window, cx| {
                    let press = press_comp(panel, (60.0, -40.0), modifiers);
                    panel.left_mouse_down(&press, cx);
                    assert!(panel.box_select.is_some(), "no network is open");
                })
                .unwrap();
            publish_geometry_results(&project, cx);

            window
                .update(cx, |panel, _window, cx| {
                    panel.left_dragged(&move_comp(panel, (140.0, 40.0)), cx);
                    panel.box_select_ended(window_point(panel, (140.0, 40.0)), cx);
                })
                .unwrap();

            let selected = cx.update(|cx| crate::panels::layer_selection(cx).layers().to_vec());
            assert_eq!(
                selected.len(),
                expected,
                "shift = {}, selected = {selected:?}",
                modifiers.shift
            );
            assert!(selected.contains(&layers[1]), "the swept layer is selected");
            assert_eq!(
                selected.contains(&layers[0]),
                modifiers.shift,
                "the press-time selection survives exactly when Shift asked it to"
            );
        }
    }

    /// A second Viewer on the same project, with its own instance id — the
    /// state REQ-UI-005 allows, and what the box-select Global has to survive.
    fn extra_viewer(cx: &mut TestAppContext, instance: u64) -> WindowHandle<ViewerPanel> {
        let window = cx.add_window(|window, cx| {
            ViewerPanel::new(ravel_ui::layout::PanelInstanceId(instance), window, cx)
        });
        window
            .update(cx, |panel, _window, _cx| {
                panel.composition_resolution = Some(FIXTURE_RES);
                panel.viewport_origin.set((0.0, 0.0));
                panel.viewport_size.set((1920.0, 1080.0));
            })
            .unwrap();
        window
    }

    fn live_box_select_owner(cx: &mut TestAppContext) -> Option<ravel_ui::layout::PanelInstanceId> {
        cx.update(|cx| {
            cx.try_global::<overlay::BoxSelectDrag>()
                .and_then(|drag| drag.0.as_ref())
                .map(|live| live.panel)
        })
    }

    /// One Viewer ending its gesture must not withdraw another Viewer's
    /// candidate declaration: the Global is shared, the panels are not.
    ///
    /// The path is real — a release this window never received leaves a gesture
    /// standing while the next press starts one in another Viewer — so the
    /// second press takes the ownership and the first panel's release, arriving
    /// late, has to be a no-op on the shared state.
    #[gpui::test]
    fn a_release_in_one_viewer_leaves_another_viewers_sweep_alone(cx: &mut TestAppContext) {
        let (first, _project, comp_id, layer) = shell_setup(cx);
        let second = extra_viewer(cx, 1);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(NetworkPath::layer(comp_id, layer)),
                nodes: HashSet::new(),
            })
        });

        first
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (400.0, 400.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_some());
            })
            .unwrap();
        assert_eq!(
            live_box_select_owner(cx),
            Some(ravel_ui::layout::PanelInstanceId(0))
        );

        second
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (500.0, 500.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_some());
            })
            .unwrap();
        assert_eq!(
            live_box_select_owner(cx),
            Some(ravel_ui::layout::PanelInstanceId(1)),
            "the new press takes the declaration over from the stale one"
        );

        // The first panel's release, arriving after the take-over.
        first
            .update(cx, |panel, _window, cx| {
                panel.box_select_ended(window_point(panel, (400.0, 400.0)), cx);
                assert!(panel.box_select.is_none());
            })
            .unwrap();
        assert_eq!(
            live_box_select_owner(cx),
            Some(ravel_ui::layout::PanelInstanceId(1)),
            "and does not withdraw the other Viewer's candidates"
        );
        second
            .update(cx, |panel, _window, cx| {
                assert!(panel.box_select.is_some(), "the live sweep is untouched");
                assert!(
                    !OverlayRegistry::builtin()
                        .eval_targets(&panel.overlay_context(cx))
                        .is_empty()
                );
            })
            .unwrap();
    }

    /// The sweep follows the pen session's rule: a deliberate tool switch ends
    /// it, and the `h` hold — transient navigation that gives the tool back —
    /// keeps it.
    #[gpui::test]
    fn a_tool_switch_ends_the_sweep_but_the_hand_hold_keeps_it(cx: &mut TestAppContext) {
        let (window, _project, comp_id, layer) = shell_setup(cx);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(NetworkPath::layer(comp_id, layer)),
                nodes: HashSet::new(),
            })
        });
        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (400.0, 400.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_some());
            })
            .unwrap();

        cx.update(|cx| {
            cx.set_global(ToolState {
                active: ravel_ui::ToolKind::Hand,
                hand_hold: true,
                previous: ravel_ui::ToolKind::Select,
            })
        });
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(
                    panel.box_select.is_some(),
                    "the `h` hold is navigation, not a tool change"
                );
            })
            .unwrap();
        assert!(live_box_select_owner(cx).is_some());

        cx.update(|cx| cx.set_global(tool_state(ravel_ui::ToolKind::Pen)));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.box_select.is_none(), "a real switch ends the sweep");
            })
            .unwrap();
        assert_eq!(
            live_box_select_owner(cx),
            None,
            "and withdraws the candidates"
        );
    }

    /// A composition switch ends the sweep. The release already refuses to
    /// publish into a composition that left the screen, but the declaration
    /// would keep evaluating a network nobody is looking at.
    #[gpui::test]
    fn a_composition_switch_ends_the_sweep(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        cx.update(|cx| {
            cx.set_global(CanvasSelection {
                path: Some(NetworkPath::layer(comp_id, layer)),
                nodes: HashSet::new(),
            })
        });
        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (400.0, 400.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_some());
            })
            .unwrap();

        project.update(cx, |project, cx| project.set_active_composition(None, cx));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.box_select.is_none());
            })
            .unwrap();
        assert_eq!(live_box_select_owner(cx), None);
    }

    /// Completion criterion, layer scope: a zero-distance drag is a click, and
    /// a click on empty space deselects. The press publishes it, exactly as the
    /// node path does through `selection_after_click`.
    #[gpui::test]
    fn a_zero_distance_layer_box_clears_the_layer_selection(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, layers) = multi_layer_setup(cx);
        assert_eq!(
            cx.update(|cx| crate::panels::layer_selection(cx).layers().len()),
            2
        );

        window
            .update(cx, |panel, _window, cx| {
                // Well outside both layer bboxes (-50..50 and 50..150 in x).
                let press = press_comp(panel, (600.0, 600.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_some(), "empty space sweeps");
                assert!(panel.move_drag.is_none());
            })
            .unwrap();
        assert!(
            cx.update(|cx| crate::panels::layer_selection(cx).is_empty()),
            "the press published the click's deselection"
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.box_select_ended(window_point(panel, (600.0, 600.0)), cx);
            })
            .unwrap();
        assert!(
            cx.update(|cx| crate::panels::layer_selection(cx).is_empty()),
            "and the release leaves the deselection alone"
        );
        assert_eq!(layers.len(), 2, "both layers still exist");
    }

    // -----------------------------------------------------------------------
    // TOOLX-3: the hit-target fallback
    // -----------------------------------------------------------------------

    /// A composition holding one 40x40 square per `centers` entry, one layer
    /// each in the given order — which is bottom-to-top, the order
    /// `Composition::layers` is stored in — with every layer's geometry
    /// evaluated the way the request → publish path delivers it.
    fn stacked_layers_context(centers: &[(f32, f32)]) -> (OverlayContext, CompId, Vec<Layer>) {
        use ravel_core::id::LayerId;

        let layers: Vec<Layer> = centers
            .iter()
            .enumerate()
            .map(|(index, center)| {
                let network = Graph::new().add_node(square_node(*center, 40.0)).unwrap();
                Layer::new(LayerId::next(), format!("L{index}"), network).with_time(0, 0, 300)
            })
            .collect();
        let comp = comp_with_layers(layers.clone());
        let mut values = HashMap::new();
        for layer in &layers {
            let path = NetworkPath::layer(comp.id, layer.id);
            values.extend(evaluated_results(&layer.network, &path).values);
        }
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(super::super::PlaybackPosition {
                frame: 0,
                fps: FrameRate::new(30, 1),
            }),
            results: overlay::EvalResults::new(values),
            document: Some(Document::default().with_composition(comp.clone())),
            ..OverlayContext::default()
        };
        (ctx, comp.id, layers)
    }

    fn only_node(layer: &Layer) -> NodeId {
        layer.network.nodes().next().expect("one node").id
    }

    /// The fallback searches the layers top-down and skips the one the press
    /// already tested.
    #[test]
    fn the_fallback_picks_the_topmost_other_layer() {
        // Three stacked squares, all covering the origin.
        let (ctx, comp, layers) = stacked_layers_context(&[(0.0, 0.0), (0.0, 0.0), (0.0, 0.0)]);

        assert_eq!(
            hit_test_other_layers(&ctx, comp, None, (0.0, 0.0)),
            Some((
                NetworkPath::layer(comp, layers[2].id),
                only_node(&layers[2])
            )),
            "the topmost layer is the one drawn over the others"
        );
        assert_eq!(
            hit_test_other_layers(&ctx, comp, Some(layers[2].id), (0.0, 0.0)),
            Some((
                NetworkPath::layer(comp, layers[1].id),
                only_node(&layers[1])
            )),
            "the layer the press already tested is skipped"
        );
        assert_eq!(
            hit_test_other_layers(&ctx, comp, None, (500.0, 500.0)),
            None,
            "nothing is under the pointer"
        );
    }

    /// Completion criterion: the fallback's candidates are declared by the
    /// press that missed — so a node-scope sweep asks for every layer of the
    /// composition, not only the open network's, and stops asking on release.
    #[test]
    fn a_node_scope_sweep_declares_every_layers_candidates() {
        let (mut ctx, comp, layers) = stacked_layers_context(&[(0.0, 0.0), (100.0, 0.0)]);
        let open = NetworkPath::layer(comp, layers[0].id);
        let other = NetworkPath::layer(comp, layers[1].id);
        ctx.box_select = Some(BoxSelect {
            scope: BoxSelectScope::Nodes(open.clone()),
            rect: None,
        });

        let targets = OverlayRegistry::builtin().eval_targets(&ctx);
        let document = ctx.document.clone().expect("a document");
        for (network, why) in [
            (&open, "the open network's own candidates"),
            (&other, "and the layer the release may fall back to"),
        ] {
            let expected = geometry::geometry_targets(&document, network);
            assert!(!expected.is_empty(), "{why}: the fixture measures nothing");
            for target in expected {
                assert!(targets.contains(&target), "{why}");
            }
        }

        ctx.box_select = None;
        assert!(
            OverlayRegistry::builtin().eval_targets(&ctx).is_empty(),
            "the declaration lives only as long as the gesture"
        );
    }

    /// Completion criterion: a press that hits something inside the open
    /// network never looks at the other layers — no sweep starts, so nothing
    /// declares their candidates and nothing can fall back to them.
    ///
    /// `multi_layer_setup`'s bboxes are (-50, -50)-(50, 50) and
    /// (50, -50)-(150, 50), so the column x = 50 is held by both: the pointer
    /// is over the open network's node *and* over the layer above it.
    #[gpui::test]
    fn a_hit_in_the_open_network_never_looks_at_the_other_layers(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let network = NetworkPath::layer(comp_id, layers[0]);
        let node = project.read_with(cx, |project, _| {
            only_node(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layers[0])
                    .unwrap(),
            )
        });
        cx.update(|cx| {
            // The manipulator's grips answer the pointer before any tool does,
            // and one of them sits exactly on the shared column below.
            crate::panels::clear_layer_selection(cx);
            cx.set_global(CanvasSelection {
                path: Some(network.clone()),
                nodes: HashSet::new(),
            });
        });
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (50.0, 0.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.box_select.is_none(), "a hit starts no sweep");
                assert!(panel.move_drag.is_some(), "it moves what it hit");
                assert!(
                    overlay::box_select_candidates(cx).is_none(),
                    "so nothing declares the other layers' bboxes"
                );
            })
            .unwrap();
        assert_eq!(
            selected_nodes(cx),
            HashSet::from([node]),
            "the open network's node wins over the layer stacked above it"
        );
        assert!(
            cx.update(|cx| crate::panels::layer_selection(cx).is_empty()),
            "and no other layer was selected"
        );
    }

    /// A click that grabbed nothing selects the shape it landed on in another
    /// layer, and the layer selection follows it.
    ///
    /// This is the `TOOLX-2` side effect: with several layers selected, a
    /// press inside a layer whose shell is not the identity could move
    /// nothing, so it counted as empty space and deselected everything. The
    /// fallback turns it into a pick.
    #[gpui::test]
    fn a_click_inside_a_transformed_layer_selects_its_shape(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::AnimationChannel;

        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        project.update(cx, |project, cx| {
            let doc =
                ravel_ui::document::update_layer(project.document(), comp_id, layers[1], |layer| {
                    layer.transform.rotation = AnimationChannel::constant(45.0)
                })
                .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();
        let node = project.read_with(cx, |project, _| {
            only_node(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layers[1])
                    .unwrap(),
            )
        });
        // The commit's re-request cleared the snapshot (no worker in tests).
        publish_geometry_results(&project, cx);

        // Inside the rotated layer's bbox and outside the other one's.
        let hit = (140.0, 0.0);
        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, hit, Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(
                    panel.move_drag.is_none(),
                    "a transformed layer is still not movable"
                );
                assert!(
                    panel.box_select.is_some(),
                    "the press found nothing to grab"
                );
            })
            .unwrap();
        assert!(
            cx.update(|cx| crate::panels::layer_selection(cx).is_empty()),
            "the press published the click's deselection"
        );
        publish_geometry_results(&project, cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.box_select_ended(window_point(panel, hit), cx);
            })
            .unwrap();

        assert_eq!(
            selected_nodes(cx),
            HashSet::from([node]),
            "the release fell back to the shape under the click"
        );
        assert_eq!(
            cx.update(|cx| crate::panels::layer_selection(cx).layers().to_vec()),
            vec![layers[1]],
            "and the layer selection followed it, so the next press picks inside it"
        );
        assert_eq!(
            cx.update(|cx| cx
                .try_global::<CanvasSelection>()
                .and_then(|selection| selection.path.clone())),
            Some(NetworkPath::layer(comp_id, layers[1])),
            "the found layer's network is the one that opened"
        );
    }

    // -----------------------------------------------------------------------
    // TOOLX-3: path point insertion and removal
    // -----------------------------------------------------------------------

    /// A curve with tangents on both ends: the shape a naive re-fit would
    /// flatten, so it is the fixture the insertion has to preserve.
    fn curved_path() -> Vec<PathPoint> {
        vec![
            PathPoint {
                p: Vec2(100.0, 100.0),
                in_tan: Vec2(-30.0, 0.0),
                out_tan: Vec2(60.0, -40.0),
            },
            PathPoint {
                p: Vec2(300.0, 200.0),
                in_tan: Vec2(-50.0, -70.0),
                out_tan: Vec2(20.0, 10.0),
            },
        ]
    }

    fn assert_on_curve(actual: (f32, f32), expected: (f32, f32), what: &str) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-3 && (actual.1 - expected.1).abs() < 1e-3,
            "{what}: {actual:?} is not {expected:?}"
        );
    }

    /// Completion criterion: inserting a point does not change the shape of
    /// the path.
    ///
    /// Proved parametrically rather than by eye: splitting at `t` makes the
    /// first new segment trace the original's `[0, t]` and the second its
    /// `[t, 1]`, so every sample of the two halves has a matching sample on
    /// the curve that was there before.
    #[test]
    fn inserting_a_point_does_not_move_the_curve() {
        let original = curved_path();
        let segment = path_segment(&original, 0, 1);
        // Exactly on the curve, half way along it — which is one of the hit
        // test's samples, so the split lands at t = 0.5.
        let split = 0.5;
        let pointer = cubic_at(&segment, split);

        let inserted =
            path_with_inserted_point(&original, false, pointer, 1.0).expect("a press on the curve");
        assert_eq!(inserted.len(), 3, "one point was added");
        assert_on_curve(
            (inserted[1].p.0, inserted[1].p.1),
            pointer,
            "the new anchor sits where the press landed",
        );
        assert_eq!(
            (inserted[0].p, inserted[2].p),
            (original[0].p, original[1].p),
            "the existing anchors did not move"
        );
        assert_eq!(
            (inserted[0].in_tan, inserted[2].out_tan),
            (original[0].in_tan, original[1].out_tan),
            "and neither did the tangents facing away from the split"
        );

        let halves = [
            (path_segment(&inserted, 0, 1), 0.0, split),
            (path_segment(&inserted, 1, 2), split, 1.0),
        ];
        for (half, start, end) in halves {
            for step in 0..=20 {
                let u = step as f32 / 20.0;
                assert_on_curve(
                    cubic_at(&half, u),
                    cubic_at(&segment, start + u * (end - start)),
                    "the split traces the original curve",
                );
            }
        }
    }

    /// Completion criterion: removing a point leaves the tangents of the
    /// points beside it alone, and a path that is only two points refuses.
    #[test]
    fn removing_a_point_keeps_the_neighbours_tangents() {
        let mut points = curved_path();
        points.insert(
            1,
            PathPoint {
                p: Vec2(200.0, 150.0),
                in_tan: Vec2(-11.0, -12.0),
                out_tan: Vec2(13.0, 14.0),
            },
        );

        let after = path_without_point(&points, 1).expect("a three-point path can spare one");
        assert_eq!(
            after,
            vec![points[0], points[2]],
            "the neighbours survive untouched, tangents included"
        );
        assert_eq!(
            path_without_point(&after, 0),
            None,
            "two points are the least a path can be"
        );
        assert_eq!(
            path_without_point(&points, 3),
            None,
            "and an index past the end removes nothing"
        );
    }

    /// The insertion goes where the closing segment is: between the last
    /// anchor and the first, which is the end of the list.
    #[test]
    fn a_press_on_the_closing_segment_inserts_at_the_end() {
        let points = vec![
            corner_path_point((0.0, 0.0)),
            corner_path_point((100.0, 0.0)),
            corner_path_point((100.0, 100.0)),
        ];
        let pointer = (50.0, 50.0);

        assert_eq!(
            path_with_inserted_point(&points, false, pointer, 4.0),
            None,
            "an open path has no segment back to the start"
        );
        let inserted = path_with_inserted_point(&points, true, pointer, 4.0)
            .expect("the closing segment runs under the pointer");
        assert_eq!(inserted.len(), 4);
        assert_on_curve(
            (inserted[3].p.0, inserted[3].p.1),
            pointer,
            "the new point closes the path",
        );
    }

    /// A press within reach of both a point and the segment through it picks
    /// the point: removal is what the pointer is over.
    #[test]
    fn a_press_picks_the_nearest_point_within_reach() {
        let points = vec![
            corner_path_point((0.0, 0.0)),
            corner_path_point((10.0, 0.0)),
            corner_path_point((100.0, 0.0)),
        ];
        assert_eq!(path_point_at(&points, (6.0, 0.0), 8.0), Some(1));
        assert_eq!(path_point_at(&points, (4.0, 0.0), 8.0), Some(0));
        assert_eq!(path_point_at(&points, (50.0, 0.0), 8.0), None);
    }

    /// `shell_setup` with the layer's network replaced by one
    /// `shape.custom_path` node holding `points`, selected, under the Pen.
    fn pen_path_setup(
        cx: &mut TestAppContext,
        points: Vec<PathPoint>,
    ) -> (
        WindowHandle<ViewerPanel>,
        Entity<ProjectState>,
        NetworkPath,
        NodeId,
    ) {
        let (window, project, comp_id, layer) = shell_setup(cx);
        let network = NetworkPath::layer(comp_id, layer);
        let node = custom_path_node(
            registry()
                .create_node("shape.custom_path", NodeId::next())
                .unwrap(),
            points,
            false,
        );
        let id = node.id;
        project.update(cx, |project, cx| {
            let graph = Graph::new().add_node(node).unwrap();
            let doc =
                ravel_ui::document::replace_network(project.document(), &network, graph).unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.update(|cx| {
            cx.set_global(tool_state(ravel_ui::ToolKind::Pen));
            cx.set_global(CanvasSelection {
                path: Some(network.clone()),
                nodes: HashSet::from([id]),
            });
        });
        (window, project, network, id)
    }

    fn committed_path(
        project: &Entity<ProjectState>,
        network: &NetworkPath,
        node: NodeId,
        cx: &mut TestAppContext,
    ) -> Vec<PathPoint> {
        project.read_with(cx, |project, _| {
            let graph = ravel_ui::document::resolve_network(project.document(), network)
                .expect("the network");
            path_points(graph.node(node).expect("the path node"))
                .expect("a points parameter")
                .to_vec()
        })
    }

    fn network_node_count(
        project: &Entity<ProjectState>,
        network: &NetworkPath,
        cx: &mut TestAppContext,
    ) -> usize {
        project.read_with(cx, |project, _| {
            ravel_ui::document::resolve_network(project.document(), network)
                .expect("the network")
                .nodes()
                .count()
        })
    }

    /// Completion criterion: the Pen inserts a point where it presses on the
    /// path, and the whole edit is one undo step.
    #[gpui::test]
    fn the_pen_inserts_a_point_on_the_segment_in_one_undo_step(cx: &mut TestAppContext) {
        let original = curved_path();
        let (window, project, network, node) = pen_path_setup(cx, original.clone());
        let pointer = cubic_at(&path_segment(&original, 0, 1), 0.5);

        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, pointer, Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(
                    panel.pen_session.is_none(),
                    "a press on the path edits it instead of starting a new one"
                );
            })
            .unwrap();
        cx.run_until_parked();

        let inserted = committed_path(&project, &network, node, cx);
        assert_eq!(inserted.len(), 3, "the press inserted a point");
        assert_eq!(
            network_node_count(&project, &network, cx),
            1,
            "and created no second path node"
        );
        assert!(
            !project.update(cx, |project, cx| project.revert_document(cx)),
            "the click left no uncommitted preview behind — it is a committed \
             step, so a cancel belonging to some other gesture cannot throw it \
             away, and an unrelated commit cannot absorb it"
        );
        assert_eq!(
            committed_path(&project, &network, node, cx).len(),
            3,
            "the inserted point survives the cancel of nothing"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx),
            original,
            "one undo takes the whole insertion back"
        );
    }

    /// A tangent handle answers the press before the removal does.
    ///
    /// `PathEditOverlay` draws a tangent's mark at `anchor + tangent`, so an
    /// arm shorter than the 8px grab radius puts two marks inside one radius.
    /// The press has to reach the arm — otherwise every attempt to bend the
    /// curve at such a point would delete the point instead.
    #[gpui::test]
    fn a_press_on_a_tangent_bends_the_curve_instead_of_removing_the_point(cx: &mut TestAppContext) {
        let original = vec![
            // A 3-unit arm: its handle sits well inside the anchor's radius.
            PathPoint {
                p: Vec2(100.0, 100.0),
                in_tan: Vec2(-3.0, 0.0),
                out_tan: Vec2(3.0, 0.0),
            },
            corner_path_point((300.0, 100.0)),
            corner_path_point((300.0, 300.0)),
        ];
        let (window, project, network, node) = pen_path_setup(cx, original.clone());

        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (103.0, 100.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(
                    panel.handle_drag.is_some(),
                    "the tangent's own handle took the press"
                );
                // Which arm is a property of the registry's hit test (the
                // first handle in range wins, not the nearest), and both arms
                // of this point are in range. Either is a bend, which is what
                // this press must not turn into a removal.
                assert!(
                    matches!(
                        panel
                            .handle_drag
                            .as_ref()
                            .and_then(|drag| drag.handle.id.path_handle_kind()),
                        Some(PathHandleKind::InTangent | PathHandleKind::OutTangent)
                    ),
                    "and what it grabbed is an arm, not the anchor"
                );
                panel.handle_drag_ended(cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx),
            original,
            "the point the arm belongs to is still there, untouched"
        );

        // The corner point has no arms, so it draws no tangent handle: the
        // press on it is the removal it has always been.
        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (300.0, 100.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx),
            vec![original[0], original[2]],
            "an anchor press still removes the point"
        );
    }

    /// The fallback picks out of what is **on screen**: a muted layer, and a
    /// layer left out by another layer's solo, are not candidates.
    ///
    /// The rule is the compositor's own (`Composition::composites`), so this
    /// pins the agreement rather than a second copy of it.
    #[gpui::test]
    fn the_fallback_skips_layers_that_do_not_composite(cx: &mut TestAppContext) {
        let (window, project, comp_id, layers) = multi_layer_setup(cx);
        let node = project.read_with(cx, |project, _| {
            only_node(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(layers[1])
                    .unwrap(),
            )
        });
        // No network open, so a press on empty space sweeps layers — and a
        // release without travel falls back. (140, 0) is inside the second
        // layer's bbox only.
        let hit = (140.0, 0.0);
        let click = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                crate::panels::clear_layer_selection(cx);
                cx.set_global(CanvasSelection::default());
            });
            window
                .update(cx, |panel, _window, cx| {
                    let press = press_comp(panel, hit, Modifiers::default());
                    panel.left_mouse_down(&press, cx);
                    assert!(panel.box_select.is_some(), "the press grabbed nothing");
                })
                .unwrap();
            // The press posted a request, and with no worker that clears the
            // snapshot the fallback reads its bboxes from.
            publish_geometry_results(&project, cx);
            window
                .update(cx, |panel, _window, cx| {
                    panel.box_select_ended(window_point(panel, hit), cx);
                })
                .unwrap();
            selected_nodes(cx)
        };

        assert_eq!(
            click(cx),
            HashSet::from([node]),
            "the visible layer's shape is picked"
        );

        for (label, mutate) in [
            (
                "muted",
                (|layer: &mut Layer| layer.muted = true) as fn(&mut Layer),
            ),
            ("left out by another layer's solo", |layer: &mut Layer| {
                layer.solo = false
            }),
        ] {
            let (window_layer, other) = (layers[1], layers[0]);
            project.update(cx, |project, cx| {
                let mut doc = ravel_ui::document::update_layer(
                    project.document(),
                    comp_id,
                    window_layer,
                    |layer| {
                        layer.muted = false;
                        layer.solo = false;
                        mutate(layer);
                    },
                )
                .unwrap();
                // The solo case needs someone else soloed; the mute case must
                // not have one.
                doc = ravel_ui::document::update_layer(&doc, comp_id, other, |layer| {
                    layer.solo = label.starts_with("left out");
                })
                .unwrap();
                project.commit_document(doc, InvalidationHint::Structural, cx);
            });
            cx.run_until_parked();
            assert!(
                click(cx).is_empty(),
                "a layer {label} is not on screen, so it cannot be picked"
            );
        }
    }

    /// Completion criterion: the Pen removes the point it presses on, and the
    /// press is still taken when the path cannot spare one — a refusal must
    /// not fall through to "start a new path here".
    #[gpui::test]
    fn the_pen_removes_the_point_it_presses(cx: &mut TestAppContext) {
        let mut original = curved_path();
        original.insert(
            1,
            PathPoint {
                p: Vec2(200.0, 150.0),
                in_tan: Vec2(-11.0, -12.0),
                out_tan: Vec2(13.0, 14.0),
            },
        );
        let (window, project, network, node) = pen_path_setup(cx, original.clone());

        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (200.0, 150.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx),
            vec![original[0], original[2]],
            "the pressed point is gone and its neighbours are untouched"
        );

        // The path is down to two points: the next press on one of them is
        // refused, and swallowed.
        window
            .update(cx, |panel, _window, cx| {
                let press = press_comp(panel, (100.0, 100.0), Modifiers::default());
                panel.left_mouse_down(&press, cx);
                assert!(panel.pen_session.is_none(), "and starts no new path");
                assert!(
                    panel.handle_drag.is_none(),
                    "the refusal consumes the press: it must not fall through                      to the anchor's own handle drag"
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx).len(),
            2,
            "a two-point path keeps both"
        );
        assert_eq!(
            network_node_count(&project, &network, cx),
            1,
            "and the refused press created nothing"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            committed_path(&project, &network, node, cx),
            original,
            "one undo takes the whole removal back"
        );
    }
}

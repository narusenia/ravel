// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Viewer overlay mechanism.
//!
//! Every layer the Viewer draws on top of the composition image is a
//! [`ViewerOverlay`]: it declares when it is visible, what it draws, which
//! handles it exposes to the pointer, and how dragging a handle changes the
//! [`Document`]. The registry owns the paint order and the hit-test priority
//! so adding an overlay never means editing `render()` and the input handlers
//! in three places.
//!
//! Three properties the mechanism has to keep:
//!
//! - **Coordinates live in one place.** Overlays speak composition space;
//!   [`OverlayPainter`] converts to screen pixels. Zoom-invariant marks
//!   (handles, rules) go through the screen-space entry points, which anchor
//!   at a composition point but size themselves in screen pixels.
//! - **Painting and hit-testing come from the same overlay.** `paint` and
//!   `handles` are neighbours, so a handle cannot drift away from the mark
//!   drawn under it.
//! - **`render()` stays pure.** Overlays are plain data transforms over an
//!   [`OverlayContext`] snapshot; they never touch globals, focus, or the
//!   command path.
//!
//! Output has two channels. [`ViewerOverlay::paint`] produces
//! [`OverlayPrimitive`]s that the canvas element flushes with real frame
//! bounds at paint time; [`ViewerOverlay::labels`] produces
//! [`OverlayLabel`]s that the panel renders as elements, because GPUI shapes
//! text through elements rather than through the canvas painter.

use gpui::{Bounds, Global, Hsla, Pixels, Point, SharedString, Window, fill, point, px, size};
use ravel_core::composition::Document;
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::eval::{EvalContext, PathSegment};
use ravel_core::geometry::{Domain, Geometry};
use ravel_core::graph::{ParameterValue, PathPoint};
use ravel_core::id::{CompId, LayerId, NodeId, OutputPortIndex};
use ravel_core::registry::{NodeRegistry, ParamRange, ParamRole};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::{NodeData, PortRecord};
use ravel_ui::ToolKind;
use ravel_ui::document::NetworkPath;

use crate::panels::{CanvasSelection, LayerSelection, PlaybackPosition};
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    CompRect, PathHandleKind, ViewerPointerHint, edited_path_points, layer_selection_comp_rects,
    overlay_line_color, path_points, selected_path_overlay, selection_comp_rects,
};

// ===========================================================================
// Identity and ordering
// ===========================================================================

/// Stable identity of an overlay. A newtype over a static string rather than
/// an enum so later units add overlays without editing a central list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OverlayId(pub &'static str);

/// Paint order and hit-test priority. Overlays paint in ascending order and
/// are hit-tested in descending order, so the visually topmost overlay also
/// wins the pointer.
pub mod priority {
    pub const GRID: i32 = 0;
    pub const SAFE_AREAS: i32 = 10;
    /// Under both bboxes: the field is a background wash the outlines and
    /// handles have to stay readable over.
    pub const FIELD: i32 = 15;
    pub const NODE_SELECTION_BBOX: i32 = 20;
    pub const LAYER_SELECTION_BBOX: i32 = 30;
    /// Above both bboxes and below the path handles: a path point drawn on
    /// top of a shell handle is the more specific thing to grab.
    pub const SHELL_MANIPULATOR: i32 = 35;
    /// Above the shell: with a node selected, the parameter under the pointer
    /// is the more specific thing to grab — a node's own centre and the bbox's
    /// move grip land on the same point.
    pub const PARAM_MANIPULATOR: i32 = 37;
    pub const PATH_EDIT: i32 = 40;
    pub const EVAL_ERROR: i32 = 50;
}

// ===========================================================================
// Context
// ===========================================================================

/// Colors an overlay may need that come from the active theme.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OverlayColors {
    /// Editable path stroke and handles.
    pub path: Hsla,
    /// Evaluation error text.
    pub error: Hsla,
}

/// The overlay-target results of the evaluation whose frame the viewer is
/// currently showing.
///
/// Durable state, not an event: it is replaced wholesale, in the same update
/// that publishes [`crate::panels::ViewerFrame`], so the values an overlay
/// reads always belong to the image underneath them. A target that failed,
/// was never requested, or has not been evaluated yet is simply absent —
/// [`OverlayContext::eval_result`] then returns `None` and the overlay draws
/// nothing rather than guessing.
/// Which node, in which network instance, a result belongs to.
///
/// The scope is part of the key because a `NodeId` is not an identity on its
/// own: two layer networks routinely hold the same ids, and a map keyed by id
/// alone would hand one layer's overlay the geometry of another whenever two
/// layers are selected at once.
pub type OverlayResultKey = (Vec<PathSegment>, NodeId);

#[derive(Clone, Default)]
pub struct OverlayResults {
    pub(crate) values: HashMap<OverlayResultKey, Arc<dyn NodeData>>,
}

impl OverlayResults {
    pub(crate) fn new(values: HashMap<OverlayResultKey, Arc<dyn NodeData>>) -> Self {
        Self { values }
    }
}

impl Global for OverlayResults {}

/// Everything the overlays are allowed to see, snapshotted once per render or
/// once per pointer event. Fields are optional exactly where the underlying
/// global may be absent, so an overlay stays inactive instead of guessing.
///
/// `Default` is the "nothing is loaded" snapshot. It exists so the
/// target-discovery context built while assembling an evaluation request
/// (`ProjectState::overlay_context_for_request`) can name only the fields
/// that decide `is_active` / `eval_target`, instead of inventing theme colors
/// and panel toggles it has no access to.
#[derive(Clone, Default)]
pub struct OverlayContext {
    /// Resolution of the composition currently shown; `None` with no output.
    ///
    /// The composition's own, which is the coordinate basis every overlay draws
    /// in — not the resolution the frame under them was evaluated at. That one
    /// is [`eval_resolution`](Self::eval_resolution).
    pub resolution: Option<(u32, u32)>,
    /// The resolution the viewer's evaluation request runs at: the composition
    /// resolution scaled by the preview factor (`ViewerResolution`, `VRES-1`).
    ///
    /// Only sampling reads it, and only because a field can: an
    /// `ExpressionField` sees `res.width` / `res.height` / `res.aspect`, so a
    /// grid sampled at the composition resolution would draw numbers the
    /// composition never rendered — at the default `Half` factor, twice the
    /// width the frame underneath was evaluated with. `None` falls back to
    /// [`resolution`](Self::resolution), which is what a context assembled
    /// without a project has.
    pub eval_resolution: Option<(u32, u32)>,
    pub playback: Option<PlaybackPosition>,
    pub document: Option<Document>,
    pub selection: Option<CanvasSelection>,
    pub layer_selection: LayerSelection,
    pub tool: Option<ToolKind>,
    pub show_grid: bool,
    pub show_safe_areas: bool,
    /// Selection bounding boxes, drawn from evaluated geometry.
    pub show_geometry_bounds: bool,
    /// Point and instance markers of the evaluated geometry.
    pub show_geometry_points: bool,
    /// Path primitives of the evaluated geometry.
    pub show_geometry_paths: bool,
    /// The geometry attribute drawn as arrows, or `None` for none. A name
    /// rather than a mode: the overlay names no attribute of its own, so a
    /// `velocity` a simulation writes and a `N` a 3D node writes take the same
    /// path (`particle-plan.md` adds no drawing code of its own).
    pub geometry_arrow_attr: Option<SharedString>,
    /// Element index labels on the drawn geometry marks.
    pub show_geometry_indices: bool,
    /// Colour the geometry marks by the group (`Bool` attribute) they are in.
    pub show_geometry_groups: bool,
    /// What the field overlay draws, if anything.
    pub field_display: super::field::FieldDisplay,
    pub field_map: super::field::FieldColorMap,
    /// Alpha the field marks are drawn at.
    pub field_opacity: f32,
    /// The latest evaluation error message, if any.
    pub error: Option<SharedString>,
    /// The gesture the pointer currently holds, or `None` when it is idle.
    /// Only the drag HUD reads it.
    pub active_drag: Option<ActiveDrag>,
    pub colors: OverlayColors,
    /// Overlay-target results belonging to the frame currently shown.
    pub results: OverlayResults,
    /// The node templates, for the parameter declarations an overlay reads
    /// (`ParamRole`). Shared rather than cloned: the snapshot is rebuilt on
    /// every pointer move.
    pub registry: Option<Arc<NodeRegistry>>,
}

/// The handle drag in flight, as the overlays see it.
///
/// The press-time document rides along because a HUD reports what the gesture
/// has *done* — a factor, an angle swept — and that is a difference between
/// two documents, not a reading of one. The panel already holds this snapshot
/// for the undo path, so carrying it costs a structurally shared clone.
#[derive(Clone)]
pub struct ActiveDrag {
    pub handle: OverlayHandleId,
    pub press_document: Document,
}

impl OverlayContext {
    /// The three pieces every document-driven overlay needs at once.
    pub fn resolved(&self) -> Option<(&Document, (u32, u32), PlaybackPosition)> {
        Some((self.document.as_ref()?, self.resolution?, self.playback?))
    }

    /// Read a target result without guessing when evaluation has not arrived.
    ///
    /// Evaluation is per node, not per port: a node with several outputs
    /// produces one [`PortRecord`] holding all of them, so the target's
    /// `output` selects from it. Handing the record over whole would give an
    /// overlay asking for port 1 a value of an entirely different type.
    ///
    /// `None` whenever the answer is not knowable — no result, the node is
    /// gone from the network, or the port carries no value — which is the
    /// same "draw nothing" signal as a result that has not arrived.
    pub fn eval_result(&self, target: &OverlayTarget) -> Option<Arc<dyn NodeData>> {
        let value = self
            .results
            .values
            .get(&(target.network.segments(), target.node))?;
        let ports = ravel_ui::document::resolve_network(self.document.as_ref()?, &target.network)?
            .node(target.node)?
            .outputs
            .len();
        PortRecord::extract(value, ports, target.output)
    }
}

/// A node output an overlay needs evaluated before it can draw. Unit 2
/// aggregates these into the multi-target `EvalRequest`; none of the overlays
/// ported in unit 1 declare one, because all five read the `Document`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayTarget {
    pub network: NetworkPath,
    pub node: NodeId,
    pub output: OutputPortIndex,
}

// ===========================================================================
// Painting
// ===========================================================================

/// A screen-space drawing command. Overlays never touch [`Window`]; they emit
/// primitives, which makes the drawn geometry directly assertable in tests.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayPrimitive {
    /// Filled rectangle.
    Quad { bounds: Bounds<Pixels>, color: Hsla },
    /// Polyline stroke. `close` repeats the first vertex at the end.
    Stroke {
        points: Vec<Point<Pixels>>,
        width: Pixels,
        color: Hsla,
        close: bool,
    },
}

/// Converts composition space to screen pixels and records what to draw.
///
/// Composition-space entry points scale with zoom; screen-space entry points
/// anchor at a composition point but keep a constant pixel size, which is how
/// handles and hairlines stay legible at any zoom.
pub struct OverlayPainter {
    frame: Bounds<Pixels>,
    resolution: (u32, u32),
    primitives: Vec<OverlayPrimitive>,
}

impl OverlayPainter {
    /// `frame` is the on-screen rectangle the composition occupies.
    pub fn new(frame: Bounds<Pixels>, resolution: (u32, u32)) -> Self {
        Self {
            frame,
            resolution,
            primitives: Vec::new(),
        }
    }

    pub fn frame(&self) -> Bounds<Pixels> {
        self.frame
    }

    pub fn resolution(&self) -> (u32, u32) {
        self.resolution
    }

    /// Screen pixels per composition pixel, per axis.
    pub fn zoom(&self) -> (f32, f32) {
        (
            f32::from(self.frame.size.width) / self.resolution.0 as f32,
            f32::from(self.frame.size.height) / self.resolution.1 as f32,
        )
    }

    pub fn to_screen(&self, comp: (f32, f32)) -> (f32, f32) {
        let (zoom_x, zoom_y) = self.zoom();
        (
            f32::from(self.frame.origin.x) + comp.0 * zoom_x,
            f32::from(self.frame.origin.y) + comp.1 * zoom_y,
        )
    }

    pub fn finish(self) -> Vec<OverlayPrimitive> {
        self.primitives
    }

    // --- composition space ------------------------------------------------

    /// Fill a composition-space rectangle.
    pub fn fill_comp_rect(&mut self, rect: CompRect, color: Hsla) {
        let bounds = self.comp_rect_bounds(rect);
        self.fill_screen_rect(bounds, color);
    }

    /// Outline a composition-space rectangle with a 1px screen-space hairline.
    pub fn stroke_comp_rect(&mut self, rect: CompRect, color: Hsla) {
        let bounds = self.comp_rect_bounds(rect);
        self.stroke_screen_rect(bounds, color);
    }

    /// Stroke a composition-space polyline with a constant screen-pixel width.
    pub fn stroke_comp_polyline(
        &mut self,
        points: &[(f32, f32)],
        close: bool,
        width_px: f32,
        color: Hsla,
    ) {
        if points.is_empty() {
            return;
        }
        let points = points
            .iter()
            .map(|comp| {
                let screen = self.to_screen(*comp);
                point(px(screen.0), px(screen.1))
            })
            .collect();
        self.primitives.push(OverlayPrimitive::Stroke {
            points,
            width: px(width_px),
            color,
            close,
        });
    }

    /// Full-height vertical rule at composition x, `width_px` pixels wide.
    pub fn comp_vrule(&mut self, comp_x: f32, width_px: f32, color: Hsla) {
        let (screen_x, _) = self.to_screen((comp_x, 0.0));
        let bounds = Bounds {
            origin: point(px(screen_x), self.frame.origin.y),
            size: size(px(width_px), self.frame.size.height),
        };
        self.fill_screen_rect(bounds, color);
    }

    /// Full-width horizontal rule at composition y, `height_px` pixels tall.
    pub fn comp_hrule(&mut self, comp_y: f32, height_px: f32, color: Hsla) {
        let (_, screen_y) = self.to_screen((0.0, comp_y));
        let bounds = Bounds {
            origin: point(self.frame.origin.x, px(screen_y)),
            size: size(self.frame.size.width, px(height_px)),
        };
        self.fill_screen_rect(bounds, color);
    }

    // --- screen space -----------------------------------------------------

    /// A square of constant screen size centered on a composition point.
    pub fn screen_square_at(&mut self, comp: (f32, f32), size_px: f32, color: Hsla) {
        let (screen_x, screen_y) = self.to_screen(comp);
        let half = size_px * 0.5;
        let bounds = Bounds {
            origin: point(px(screen_x - half), px(screen_y - half)),
            size: size(px(size_px), px(size_px)),
        };
        self.fill_screen_rect(bounds, color);
    }

    /// A circle of constant screen size centered on a composition point.
    ///
    /// The segment count is fixed: the rings this draws are a couple of dozen
    /// pixels across, where more vertices are invisible and fewer are a
    /// polygon.
    pub fn screen_ring_at(&mut self, comp: (f32, f32), radius_px: f32, width_px: f32, color: Hsla) {
        const SEGMENTS: usize = 24;
        let (screen_x, screen_y) = self.to_screen(comp);
        let points = (0..SEGMENTS)
            .map(|segment| {
                let angle = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                point(
                    px(screen_x + radius_px * angle.cos()),
                    px(screen_y + radius_px * angle.sin()),
                )
            })
            .collect();
        self.primitives.push(OverlayPrimitive::Stroke {
            points,
            width: px(width_px),
            color,
            close: true,
        });
    }

    pub fn fill_screen_rect(&mut self, bounds: Bounds<Pixels>, color: Hsla) {
        self.primitives
            .push(OverlayPrimitive::Quad { bounds, color });
    }

    /// 1px outline drawn as four quads (`paint_quad` has no stroke mode).
    pub fn stroke_screen_rect(&mut self, rect: Bounds<Pixels>, color: Hsla) {
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
            self.fill_screen_rect(edge, color);
        }
    }

    fn comp_rect_bounds(&self, rect: CompRect) -> Bounds<Pixels> {
        let (zoom_x, zoom_y) = self.zoom();
        let origin = self.to_screen((rect.x, rect.y));
        Bounds {
            origin: point(px(origin.0), px(origin.1)),
            size: size(px(rect.w * zoom_x), px(rect.h * zoom_y)),
        }
    }
}

/// Flush recorded primitives to the window. The only place overlay output
/// reaches GPUI.
pub fn paint_primitives(primitives: &[OverlayPrimitive], window: &mut Window) {
    for primitive in primitives {
        match primitive {
            OverlayPrimitive::Quad { bounds, color } => {
                window.paint_quad(fill(*bounds, *color));
            }
            OverlayPrimitive::Stroke {
                points,
                width,
                color,
                close,
            } => {
                let Some(first) = points.first() else {
                    continue;
                };
                let mut path = gpui::PathBuilder::stroke(*width);
                path.move_to(*first);
                for vertex in &points[1..] {
                    path.line_to(*vertex);
                }
                if *close && points.len() > 1 {
                    path.line_to(*first);
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, *color);
                }
            }
        }
    }
}

// ===========================================================================
// Labels
// ===========================================================================

/// Where a screen-space label sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabelPlacement {
    /// Centered over the whole viewer canvas area.
    CanvasCenter,
    /// Pinned to the canvas area's top-left corner. Where the drag HUD sits:
    /// a fixed corner never lands under the pointer or under the handles the
    /// gesture is moving.
    CanvasTopLeft,
    /// Anchored at a composition point, so the text travels with the element
    /// it annotates under pan and zoom. The panel converts through the same
    /// viewport the pointer is resolved with; a label outside the canvas is
    /// simply off-screen.
    Comp((f32, f32)),
}

/// Screen-space text produced by an overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayLabel {
    pub text: SharedString,
    pub color: Hsla,
    pub placement: LabelPlacement,
}

// ===========================================================================
// Handles and edits
// ===========================================================================

/// Overlay-local identity of a handle, carried back into
/// [`ViewerOverlay::drag`] so the overlay knows what was grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayHandleId {
    /// A control point of an editable path, or one of its tangents.
    PathPoint { index: usize, kind: PathHandleKind },
    /// A grip of the layer shell manipulator.
    Shell(ShellHandle),
    /// A parameter handle of the selected node, indexed into the order
    /// [`ParamManipulator`] resolves its marks in.
    Param(u8),
    #[cfg(test)]
    Test(u8),
}

impl OverlayHandleId {
    /// The control point index and handle kind, when this is a path handle.
    pub fn path_point(self) -> Option<(usize, PathHandleKind)> {
        match self {
            Self::PathPoint { index, kind } => Some((index, kind)),
            _ => None,
        }
    }

    /// The path handle kind, for the cursor mapping during a drag.
    pub fn path_handle_kind(self) -> Option<PathHandleKind> {
        self.path_point().map(|(_, kind)| kind)
    }

    /// The shell grip, when this handle belongs to the shell manipulator.
    pub fn shell(self) -> Option<ShellHandle> {
        match self {
            Self::Shell(handle) => Some(handle),
            _ => None,
        }
    }

    /// The parameter mark index, when this handle belongs to the parameter
    /// manipulator.
    pub fn param(self) -> Option<u8> {
        match self {
            Self::Param(index) => Some(index),
            _ => None,
        }
    }
}

/// Modifier keys sampled at each move of a handle drag.
///
/// The convention is the one the shape-drawing drag (`drag_geometry`) and the
/// Timeline gestures already use: **Shift constrains** (a square there, a
/// locked aspect ratio here) and **Alt changes the fixed reference point**
/// (the drag's origin there, the anchor point here).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DragModifiers {
    pub shift: bool,
    pub alt: bool,
}

/// A grabbable point an overlay exposes to the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayHandle {
    /// Owning overlay, stamped by the registry.
    pub overlay: OverlayId,
    pub id: OverlayHandleId,
    /// Composition-space anchor, the same point `paint` draws the mark at.
    pub position: (f32, f32),
    /// Hit radius in screen pixels, so grabbing is zoom-independent.
    pub hit_radius_px: f32,
    /// Cursor shown while hovering the handle.
    pub hint: ViewerPointerHint,
    /// Whether pressing starts a drag. A handle can be visible and hoverable
    /// while its edit path is not available yet.
    pub draggable: bool,
}

/// Which axis of a two-component shell channel an edit targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    /// Index into the `[AnimationChannel; 2]` a Vec2 shell property uses.
    pub fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
        }
    }
}

/// One animatable channel of a [`ravel_core::composition::LayerTransform`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellChannel {
    AnchorPoint(Axis),
    Position(Axis),
    Scale(Axis),
    Rotation,
}

/// A change a handle drag makes to the [`Document`].
///
/// Both node parameters and layer-shell channels are representable: the shell
/// transform is a field of `Layer`, not a node parameter, so a node-only shape
/// would leave unit 7's manipulator without a way to express its edit. The
/// undo rule is shared and lives with the caller: a gesture previews with
/// `apply_document` and commits one snapshot when it ends.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayEdit {
    /// Replace one parameter of one node. The caller supplies the finished
    /// [`ParameterValue`], so keyframe-preserving writes stay in
    /// `param_edit`.
    NodeParameter {
        network: NetworkPath,
        node: NodeId,
        key: SharedString,
        value: ParameterValue,
    },
    /// Write one shell transform channel at a layer-local frame. `None`
    /// collapses the channel to a constant, matching the Properties scrub.
    LayerTransform {
        comp: CompId,
        layer: LayerId,
        channel: ShellChannel,
        value: f32,
        local_frame: Option<u64>,
    },
    /// Several writes that belong to one gesture step and must land together.
    /// A shell scale about the opposite corner moves `scale` *and* `position`;
    /// an anchor move rewrites all four channels. Applying them one at a time
    /// would publish a document in which the picture has jumped.
    Batch(Vec<OverlayEdit>),
}

impl OverlayEdit {
    /// The new document, or `None` when the target no longer exists.
    pub fn apply(&self, document: &Document) -> Option<Document> {
        match self {
            Self::NodeParameter {
                network,
                node,
                key,
                value,
            } => {
                let graph = ravel_ui::document::resolve_network(document, network)?;
                let current = graph.node(*node)?;
                let mut updated = current.as_ref().clone();
                let parameter = updated
                    .parameters
                    .iter_mut()
                    .find(|parameter| parameter.key == key.as_ref())?;
                parameter.value = value.clone();
                let graph = graph.clone().replace_node(std::sync::Arc::new(updated));
                ravel_ui::document::replace_network(document, network, graph)
            }
            Self::LayerTransform {
                comp,
                layer,
                channel,
                value,
                local_frame,
            } => ravel_ui::document::update_layer(document, *comp, *layer, |layer| {
                let transform = &mut layer.transform;
                let slot = match channel {
                    ShellChannel::AnchorPoint(axis) => &mut transform.anchor_point[axis.index()],
                    ShellChannel::Position(axis) => &mut transform.position[axis.index()],
                    ShellChannel::Scale(axis) => &mut transform.scale[axis.index()],
                    ShellChannel::Rotation => &mut transform.rotation,
                };
                *slot = crate::panels::param_edit::edited_channel(slot, *value, *local_frame);
            }),
            // All or nothing: a half-applied batch is a document nobody asked
            // for.
            Self::Batch(edits) => edits
                .iter()
                .try_fold(document.clone(), |document, edit| edit.apply(&document)),
        }
    }

    /// Whether the edit still has something to write. A gesture whose target
    /// was deleted from another panel has to end instead of resurrecting it.
    pub fn target_exists(&self, document: &Document) -> bool {
        match self {
            Self::NodeParameter {
                network, node, key, ..
            } => ravel_ui::document::resolve_network(document, network)
                .and_then(|graph| graph.node(*node))
                .is_some_and(|node| {
                    node.parameters
                        .iter()
                        .any(|parameter| parameter.key == key.as_ref())
                }),
            Self::LayerTransform { comp, layer, .. } => document
                .get_composition(*comp)
                .and_then(|comp| comp.get_layer(*layer))
                .is_some(),
            Self::Batch(edits) => edits.iter().all(|edit| edit.target_exists(document)),
        }
    }

    /// The layer shell this edit writes, when it writes one. The gesture's
    /// lifetime hangs off it: a selection that no longer holds this layer has
    /// to end the drag rather than keep transforming what is no longer
    /// selected.
    pub fn layer_target(&self) -> Option<(CompId, LayerId)> {
        match self {
            Self::NodeParameter { .. } => None,
            Self::LayerTransform { comp, layer, .. } => Some((*comp, *layer)),
            Self::Batch(edits) => edits.iter().find_map(Self::layer_target),
        }
    }

    /// The node this edit writes, when it writes one. The counterpart of
    /// [`Self::layer_target`], and the gesture's lifetime hangs off it the
    /// same way: a node parameter is only manipulable while that node is the
    /// selected one, so a selection that moved on has to end the drag instead
    /// of writing a node nobody is looking at.
    pub fn node_target(&self) -> Option<(NetworkPath, NodeId)> {
        match self {
            Self::NodeParameter { network, node, .. } => Some((network.clone(), *node)),
            Self::LayerTransform { .. } => None,
            Self::Batch(edits) => edits.iter().find_map(Self::node_target),
        }
    }

    /// How much of the evaluator the edit invalidates.
    pub fn invalidation(&self) -> InvalidationHint {
        match self {
            Self::NodeParameter { node, .. } => InvalidationHint::Params(vec![*node]),
            // The shell compositing chain is recompiled from the layer, so no
            // node registration goes stale.
            Self::LayerTransform { .. } => InvalidationHint::None,
            Self::Batch(edits) => edits.iter().fold(InvalidationHint::None, |hint, edit| {
                hint.merge(edit.invalidation())
            }),
        }
    }
}

// ===========================================================================
// The trait and the registry
// ===========================================================================

/// One layer the Viewer draws over the composition.
pub trait ViewerOverlay {
    fn id(&self) -> OverlayId;

    /// Paint order and hit-test priority; see [`priority`].
    fn priority(&self) -> i32;

    /// Visibility condition. Nothing else on the overlay is called when false.
    fn is_active(&self, ctx: &OverlayContext) -> bool;

    /// The node outputs this overlay needs evaluated, if any (unit 2).
    ///
    /// A list rather than one target because a bbox is per selected node and
    /// a selection is a set: one target per overlay would have forced a
    /// registry entry per selected element.
    fn eval_targets(&self, _ctx: &OverlayContext) -> Vec<OverlayTarget> {
        Vec::new()
    }

    /// Draw in composition or screen space through the painter.
    fn paint(&self, _ctx: &OverlayContext, _painter: &mut OverlayPainter) {}

    /// Screen-space text, rendered by the panel as elements.
    fn labels(&self, _ctx: &OverlayContext) -> Vec<OverlayLabel> {
        Vec::new()
    }

    /// Grabbable points, in the order the pointer should resolve them.
    fn handles(&self, _ctx: &OverlayContext) -> Vec<OverlayHandle> {
        Vec::new()
    }

    /// Translate a handle drag into a document change. `delta` is the
    /// composition-space offset from the press position, `modifiers` are the
    /// keys held at this move, and `ctx` is the context captured at press
    /// time, so repeated calls during one gesture stay absolute instead of
    /// compounding.
    fn drag(
        &self,
        _handle: &OverlayHandle,
        _delta: (f32, f32),
        _modifiers: DragModifiers,
        _ctx: &OverlayContext,
    ) -> Option<OverlayEdit> {
        None
    }
}

/// The ordered set of overlays. Owns paint order and hit-test priority so no
/// caller has to reproduce either.
pub struct OverlayRegistry {
    overlays: Vec<Box<dyn ViewerOverlay>>,
}

impl OverlayRegistry {
    /// The overlays the Viewer ships with: the five kinds the panel drew
    /// before the registry existed — with the selection bbox registered once
    /// per scope so the node and layer variants order independently — plus the
    /// layer shell manipulator.
    pub fn builtin() -> Self {
        Self::new(vec![
            Box::new(GridOverlay),
            Box::new(SafeAreaOverlay),
            Box::new(GeometryOverlay {
                scope: BboxScope::Node,
            }),
            Box::new(GeometryOverlay {
                scope: BboxScope::Layer,
            }),
            Box::new(super::field::FieldOverlay),
            Box::new(ShellManipulator),
            Box::new(ParamManipulator),
            Box::new(PathEditOverlay),
            Box::new(EvalErrorOverlay),
        ])
    }

    /// Sorts by priority once; every traversal below relies on that order.
    pub fn new(mut overlays: Vec<Box<dyn ViewerOverlay>>) -> Self {
        overlays.sort_by_key(|overlay| overlay.priority());
        Self { overlays }
    }

    /// Active overlays in paint order (ascending priority).
    pub fn active<'a>(
        &'a self,
        ctx: &'a OverlayContext,
    ) -> impl Iterator<Item = &'a dyn ViewerOverlay> {
        self.overlays
            .iter()
            .map(Box::as_ref)
            .filter(move |overlay| overlay.is_active(ctx))
    }

    pub fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        for overlay in self.active(ctx) {
            overlay.paint(ctx, painter);
        }
    }

    pub fn labels(&self, ctx: &OverlayContext) -> Vec<OverlayLabel> {
        self.active(ctx)
            .flat_map(|overlay| overlay.labels(ctx))
            .collect()
    }

    /// Distinct evaluation targets of the active overlays (unit 2 folds these
    /// into one multi-target request).
    pub fn eval_targets(&self, ctx: &OverlayContext) -> Vec<OverlayTarget> {
        let mut targets: Vec<OverlayTarget> = Vec::new();
        for overlay in self.active(ctx) {
            for target in overlay.eval_targets(ctx) {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
        targets
    }

    /// The handle under the pointer, resolved from the topmost overlay down.
    /// `comp_per_px` converts a handle's screen-pixel radius to composition
    /// space.
    pub fn hit_test(
        &self,
        ctx: &OverlayContext,
        pointer: (f32, f32),
        comp_per_px: f32,
    ) -> Option<OverlayHandle> {
        self.hit_test_filtered(ctx, pointer, comp_per_px, false)
    }

    /// Same resolution order, restricted to handles that start a drag.
    pub fn hit_test_draggable(
        &self,
        ctx: &OverlayContext,
        pointer: (f32, f32),
        comp_per_px: f32,
    ) -> Option<OverlayHandle> {
        self.hit_test_filtered(ctx, pointer, comp_per_px, true)
    }

    pub fn overlay(&self, id: OverlayId) -> Option<&dyn ViewerOverlay> {
        self.overlays
            .iter()
            .map(Box::as_ref)
            .find(|overlay| overlay.id() == id)
    }

    fn hit_test_filtered(
        &self,
        ctx: &OverlayContext,
        pointer: (f32, f32),
        comp_per_px: f32,
        draggable_only: bool,
    ) -> Option<OverlayHandle> {
        for overlay in self.overlays.iter().rev() {
            if !overlay.is_active(ctx) {
                continue;
            }
            for mut handle in overlay.handles(ctx) {
                if draggable_only && !handle.draggable {
                    continue;
                }
                let radius = handle.hit_radius_px * comp_per_px;
                let dx = handle.position.0 - pointer.0;
                let dy = handle.position.1 - pointer.1;
                if dx * dx + dy * dy <= radius * radius {
                    handle.overlay = overlay.id();
                    return Some(handle);
                }
            }
        }
        None
    }
}

// ===========================================================================
// The five built-in overlays
// ===========================================================================

/// 3x3 proportional grid over the composition rectangle.
pub struct GridOverlay;

impl GridOverlay {
    pub const ID: OverlayId = OverlayId("viewer.grid");
}

impl ViewerOverlay for GridOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::GRID
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.show_grid && ctx.resolution.is_some()
    }

    fn paint(&self, _ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let (width, height) = painter.resolution();
        let color = overlay_line_color();
        for i in 1..3 {
            let t = i as f32 / 3.0;
            painter.comp_vrule(width as f32 * t, 1.0, color);
            painter.comp_hrule(height as f32 * t, 1.0, color);
        }
    }
}

/// Action-safe (90%) and title-safe (80%) rectangles, centered.
pub struct SafeAreaOverlay;

impl SafeAreaOverlay {
    pub const ID: OverlayId = OverlayId("viewer.safe_areas");
}

impl ViewerOverlay for SafeAreaOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::SAFE_AREAS
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.show_safe_areas && ctx.resolution.is_some()
    }

    fn paint(&self, _ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let (width, height) = painter.resolution();
        let (width, height) = (width as f32, height as f32);
        for fraction in [0.9f32, 0.8] {
            let (w, h) = (width * fraction, height * fraction);
            painter.stroke_comp_rect(
                CompRect {
                    x: (width - w) * 0.5,
                    y: (height - h) * 0.5,
                    w,
                    h,
                },
                overlay_line_color(),
            );
        }
    }
}

/// Whether the bbox outlines the node selection or the layer selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BboxScope {
    Node,
    Layer,
}

/// Accent used by both selection bboxes.
const SELECTION_COLOR: Hsla = Hsla {
    h: 0.58,
    s: 0.7,
    l: 0.6,
    a: 0.9,
};

/// Screen-pixel side length of a selection handle (zoom-independent).
pub const SELECTION_HANDLE_PX: f32 = 7.0;

/// Inner fill of a two-square handle mark.
const HANDLE_FILL: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};

/// The eight handle anchor points of a bbox: four corners and the four edge
/// midpoints. Coordinate-system agnostic.
pub fn selection_handle_centers(x: f32, y: f32, w: f32, h: f32) -> [(f32, f32); 8] {
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

/// A handle mark: an outer square in `color` with a light core, so it reads
/// against both the composition and the outline it sits on.
fn paint_handle_mark(painter: &mut OverlayPainter, center: (f32, f32), size_px: f32, color: Hsla) {
    painter.screen_square_at(center, size_px, color);
    painter.screen_square_at(center, size_px - 2.0, HANDLE_FILL);
}

/// Screen-pixel side length of a geometry point marker.
const GEOMETRY_POINT_PX: f32 = 3.0;

/// Point and path marks: a warmer accent than the bbox, so a dense point cloud
/// stays distinguishable from the outline around it.
const GEOMETRY_MARK_COLOR: Hsla = Hsla {
    h: 0.12,
    s: 0.85,
    l: 0.62,
    a: 0.9,
};

/// Attribute arrows: cool where the point marks are warm, so an arrow reads as
/// a separate thing from the element it leaves.
const ARROW_COLOR: Hsla = Hsla {
    h: 0.45,
    s: 0.85,
    l: 0.62,
    a: 0.95,
};

/// Longest attribute arrow drawn, as a fraction of the composition's shorter
/// side. A cap in composition units rather than in the geometry's own bounds:
/// a single particle has no extent to measure an arrow against, and its
/// velocity still has to be visible.
const ARROW_COMP_FRACTION: f32 = 0.15;

/// The colour of the group named `name`.
///
/// Derived from the name, not from the group's position in a list, so a group
/// keeps its colour when another one appears beside it or when the same group
/// exists on both drawn domains. The hue comes off a small FNV-1a hash: any
/// spread would do, and this one needs no table to maintain.
fn group_color(name: &str) -> Hsla {
    let mut hash: u32 = 2_166_136_261;
    for byte in name.as_bytes() {
        hash = (hash ^ u32::from(*byte)).wrapping_mul(16_777_619);
    }
    Hsla {
        h: (hash % 360) as f32 / 360.0,
        s: 0.85,
        l: 0.62,
        a: 0.9,
    }
}

/// Draws what the evaluator produced for the selection: the bounding box, the
/// point and instance positions, and the path primitives — each behind its own
/// toggle.
///
/// Everything here comes from an evaluated [`ravel_core::geometry::Geometry`],
/// never from a `type_key` reading of the node's parameters. That is what makes
/// a shape node this crate does not know about outline correctly, a
/// `geometry.transform` outline where it actually is, and a `scatter.*` show
/// every copy it places.
///
/// The eight bbox handles stay decorative, and the reason is now specific
/// rather than general: they outline *nodes*, and scaling a node means writing
/// its own size parameters, which is unit 5's `ParamRole` work. The layer shell
/// is grabbable — [`ShellManipulator`] draws the same eight marks around the
/// layer bbox and backs them with [`OverlayHandle`]s.
pub struct GeometryOverlay {
    pub scope: BboxScope,
}

impl GeometryOverlay {
    pub const NODE_ID: OverlayId = OverlayId("viewer.geometry.node");
    pub const LAYER_ID: OverlayId = OverlayId("viewer.geometry.layer");

    /// The networks this scope outlines, in selection order.
    fn networks(&self, ctx: &OverlayContext) -> Vec<NetworkPath> {
        match self.scope {
            BboxScope::Node => ctx
                .selection
                .as_ref()
                .filter(|selection| !selection.nodes.is_empty())
                .and_then(|selection| selection.path.clone())
                .into_iter()
                .collect(),
            // Layer-level bboxes stand in for node bboxes exactly when several
            // layers are selected (REQ-UI-013): no network is open then, so
            // there is no node selection, and what is outlined is what a drag
            // moves.
            BboxScope::Layer => {
                if ctx.layer_selection.layers().len() < 2 {
                    return Vec::new();
                }
                let Some(comp) = ctx.layer_selection.comp() else {
                    return Vec::new();
                };
                ctx.layer_selection
                    .layers()
                    .iter()
                    .map(|layer| NetworkPath::layer(comp, *layer))
                    .collect()
            }
        }
    }

    /// The `(network, node)` pairs whose geometry the point and path marks are
    /// drawn from.
    ///
    /// A node selection draws the nodes the user selected. A layer selection
    /// has no node selection to draw, so it falls back to what the layers
    /// terminally place — the same nodes their bbox is unioned from.
    fn drawn_nodes(&self, ctx: &OverlayContext) -> Vec<(NetworkPath, NodeId)> {
        match self.scope {
            BboxScope::Node => {
                let Some(selection) = ctx.selection.as_ref() else {
                    return Vec::new();
                };
                let Some(network) = selection.path.clone() else {
                    return Vec::new();
                };
                let mut nodes: Vec<_> = selection.nodes.iter().copied().collect();
                nodes.sort_by_key(|id| id.raw());
                nodes
                    .into_iter()
                    .map(|node| (network.clone(), node))
                    .collect()
            }
            BboxScope::Layer => self
                .networks(ctx)
                .into_iter()
                .flat_map(|network| {
                    super::terminal_geometry_nodes_of(ctx, &network)
                        .into_iter()
                        .map(move |node| (network.clone(), node))
                })
                .collect(),
        }
    }

    /// Run `draw` over every drawn geometry with the compositing matrix that
    /// places it on the canvas.
    ///
    /// One traversal for `paint` and `labels` alike: the index labels have to
    /// land on the marks the point pass drew, and two walks of "which geometry,
    /// placed how" is how they would stop agreeing.
    fn for_each_geometry(&self, ctx: &OverlayContext, mut draw: impl FnMut(&Affine, &Geometry)) {
        let Some(document) = ctx.document.as_ref() else {
            return;
        };
        for (network, node) in self.drawn_nodes(ctx) {
            let Some(shell) = super::layer_shell(ctx, document, network.comp, network.layer) else {
                continue;
            };
            let Some(value) = super::geometry::evaluated_geometry(ctx, &network, node) else {
                continue;
            };
            let Some(geometry) = super::geometry::as_geometry(&value) else {
                continue;
            };
            draw(&shell, geometry);
        }
    }

    fn rects(&self, ctx: &OverlayContext) -> Vec<CompRect> {
        if ctx.resolved().is_none() {
            return Vec::new();
        }
        match self.scope {
            BboxScope::Node => selection_comp_rects(ctx),
            BboxScope::Layer => {
                if ctx.layer_selection.layers().len() < 2 {
                    return Vec::new();
                }
                layer_selection_comp_rects(ctx)
            }
        }
    }
}

impl ViewerOverlay for GeometryOverlay {
    fn id(&self) -> OverlayId {
        match self.scope {
            BboxScope::Node => Self::NODE_ID,
            BboxScope::Layer => Self::LAYER_ID,
        }
    }

    fn priority(&self) -> i32 {
        match self.scope {
            BboxScope::Node => priority::NODE_SELECTION_BBOX,
            BboxScope::Layer => priority::LAYER_SELECTION_BBOX,
        }
    }

    /// Active on the *selection*, not on the results.
    ///
    /// Deciding it from `rects()` would deadlock the mechanism: an inactive
    /// overlay is never asked for its targets, so the evaluation it needs to
    /// become active would never be requested. `paint` draws nothing until the
    /// results land, which is where "no guessing" is enforced.
    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.resolution.is_some() && !self.networks(ctx).is_empty()
    }

    /// Every geometry node of the outlined networks, not only the selected
    /// ones: the Viewer's click test picks a node by its evaluated bounds, so
    /// an unselected shape needs its geometry to be selectable at all.
    fn eval_targets(&self, ctx: &OverlayContext) -> Vec<OverlayTarget> {
        let Some(document) = ctx.document.as_ref() else {
            return Vec::new();
        };
        self.networks(ctx)
            .iter()
            .flat_map(|network| super::geometry::geometry_targets(document, network))
            .collect()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        if ctx.show_geometry_bounds {
            for rect in self.rects(ctx) {
                painter.stroke_comp_rect(rect, SELECTION_COLOR);
                if self.scope != BboxScope::Node {
                    continue;
                }
                for center in selection_handle_centers(rect.x, rect.y, rect.w, rect.h) {
                    paint_handle_mark(painter, center, SELECTION_HANDLE_PX, SELECTION_COLOR);
                }
            }
        }
        let arrows = ctx.geometry_arrow_attr.clone();
        if !ctx.show_geometry_points && !ctx.show_geometry_paths && arrows.is_none() {
            return;
        }
        // The arrow cap is in composition units, so it does not change with
        // zoom: an arrow is a reading of the attribute, and a value that
        // stretched further the closer you looked would not be one.
        let (width, height) = painter.resolution();
        let reach = width.min(height) as f32 * ARROW_COMP_FRACTION;
        self.for_each_geometry(ctx, |shell, geometry| {
            let place = |point: (f32, f32)| shell.apply(point.0, point.1);
            if ctx.show_geometry_paths {
                for (points, closed) in super::geometry::geometry_paths(geometry) {
                    let points: Vec<_> = points.into_iter().map(place).collect();
                    painter.stroke_comp_polyline(&points, closed, 1.0, GEOMETRY_MARK_COLOR);
                }
            }
            if !ctx.show_geometry_points && arrows.is_none() {
                return;
            }
            let marks = super::geometry::geometry_marks(geometry);
            if ctx.show_geometry_points {
                for domain in [Domain::Point, Domain::Instance] {
                    // Resolved once per domain rather than once per mark: the
                    // group columns are the same for every element of it.
                    let groups = if ctx.show_geometry_groups {
                        super::geometry::group_columns(geometry, domain)
                    } else {
                        Vec::new()
                    };
                    for mark in marks.iter().filter(|mark| mark.domain == domain) {
                        let color = super::geometry::mark_group(&groups, mark.index)
                            .map_or(GEOMETRY_MARK_COLOR, group_color);
                        painter.screen_square_at(place(mark.position), GEOMETRY_POINT_PX, color);
                    }
                }
            }
            if let Some(name) = arrows.as_ref() {
                for (tail, tip) in
                    super::geometry::attribute_arrows(geometry, &marks, name.as_ref(), reach)
                {
                    // Both ends through the shell, so an arrow on a rotated or
                    // scaled layer points where the geometry under it does.
                    painter.stroke_comp_polyline(
                        &[place(tail), place(tip)],
                        false,
                        1.0,
                        ARROW_COLOR,
                    );
                    painter.screen_square_at(place(tip), GEOMETRY_POINT_PX, ARROW_COLOR);
                }
            }
        });
    }

    /// The element index of each drawn mark, anchored in composition space.
    ///
    /// Thinned to [`MAX_DRAWN_LABELS`](super::geometry::MAX_DRAWN_LABELS): a
    /// number per element of a scatter is an unreadable block of text and one
    /// GPUI element each.
    fn labels(&self, ctx: &OverlayContext) -> Vec<OverlayLabel> {
        if !ctx.show_geometry_indices {
            return Vec::new();
        }
        let mut labels = Vec::new();
        self.for_each_geometry(ctx, |shell, geometry| {
            for mark in super::geometry::label_marks(&super::geometry::geometry_marks(geometry)) {
                labels.push(OverlayLabel {
                    text: SharedString::from(mark.index.to_string()),
                    color: GEOMETRY_MARK_COLOR,
                    placement: LabelPlacement::Comp(shell.apply(mark.position.0, mark.position.1)),
                });
            }
        });
        labels
    }
}

/// Every planar-vector attribute the geometry overlays currently draw, for the
/// toolbar's arrow picker.
///
/// Read off the evaluated geometry rather than from a fixed list of reserved
/// names, so a `velocity` a simulation writes and an attribute a user invented
/// are equally pickable.
pub fn drawn_vector_attributes(ctx: &OverlayContext) -> Vec<String> {
    let mut names = Vec::new();
    for scope in [BboxScope::Node, BboxScope::Layer] {
        GeometryOverlay { scope }.for_each_geometry(ctx, |_, geometry| {
            names.extend(super::geometry::vector_attribute_names(geometry));
        });
    }
    names.sort();
    names.dedup();
    names
}

/// The editable path of a selected `shape.custom_path` node: the flattened
/// curve, its tangent arms, and one handle per control point and tangent.
pub struct PathEditOverlay;

impl PathEditOverlay {
    pub const ID: OverlayId = OverlayId("viewer.path_edit");
    /// Screen-pixel grab radius of a path handle.
    const HIT_RADIUS_PX: f32 = 8.0;

    fn overlay(&self, ctx: &OverlayContext) -> Option<super::PathOverlay> {
        if !matches!(ctx.tool, Some(ToolKind::Select | ToolKind::Pen)) {
            return None;
        }
        let (document, resolution, playback) = ctx.resolved()?;
        selected_path_overlay(
            ctx.selection.as_ref()?,
            document,
            playback.frame,
            playback.fps,
            resolution,
        )
    }
}

impl ViewerOverlay for PathEditOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::PATH_EDIT
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        self.overlay(ctx).is_some()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let Some(overlay) = self.overlay(ctx) else {
            return;
        };
        let color = ctx.colors.path;
        let anchors: Vec<_> = overlay.points.iter().map(|point| point.p).collect();
        let incoming: Vec<_> = overlay.points.iter().map(|point| point.in_tan).collect();
        let outgoing: Vec<_> = overlay.points.iter().map(|point| point.out_tan).collect();
        let polyline = ravel_nodes::flatten::flatten_path(
            &anchors,
            Some(&incoming),
            Some(&outgoing),
            overlay.closed,
        );
        let polyline: Vec<(f32, f32)> = polyline.iter().map(|v| (v.0, v.1)).collect();
        painter.stroke_comp_polyline(&polyline, overlay.closed, 3.0, color);

        for control in &overlay.points {
            let anchor = (control.p.0, control.p.1);
            for tangent in [control.in_tan, control.out_tan] {
                if tangent == ravel_core::types::Vec2(0.0, 0.0) {
                    continue;
                }
                let handle = (control.p.0 + tangent.0, control.p.1 + tangent.1);
                painter.stroke_comp_polyline(&[anchor, handle], false, 1.0, color);
                painter.screen_square_at(handle, 5.0, color);
            }
            painter.screen_square_at(anchor, 7.0, color);
        }
    }

    fn handles(&self, ctx: &OverlayContext) -> Vec<OverlayHandle> {
        let Some(overlay) = self.overlay(ctx) else {
            return Vec::new();
        };
        // A non-identity shell paints in composition space but cannot be
        // edited: the drag math writes node-local coordinates. Unit 3 moves
        // both onto evaluated geometry and removes the restriction.
        let draggable = overlay.shell_identity;
        let mut handles = Vec::new();
        for (index, control) in overlay.points.iter().enumerate() {
            for (kind, tangent) in [
                (PathHandleKind::InTangent, control.in_tan),
                (PathHandleKind::OutTangent, control.out_tan),
            ] {
                if tangent == ravel_core::types::Vec2(0.0, 0.0) {
                    continue;
                }
                handles.push(OverlayHandle {
                    overlay: Self::ID,
                    id: OverlayHandleId::PathPoint { index, kind },
                    position: (control.p.0 + tangent.0, control.p.1 + tangent.1),
                    hit_radius_px: Self::HIT_RADIUS_PX,
                    hint: ViewerPointerHint::PathTangent,
                    draggable,
                });
            }
            handles.push(OverlayHandle {
                overlay: Self::ID,
                id: OverlayHandleId::PathPoint {
                    index,
                    kind: PathHandleKind::Point,
                },
                position: (control.p.0, control.p.1),
                hit_radius_px: Self::HIT_RADIUS_PX,
                hint: ViewerPointerHint::PathAnchor,
                draggable,
            });
        }
        handles
    }

    fn drag(
        &self,
        handle: &OverlayHandle,
        delta: (f32, f32),
        _modifiers: DragModifiers,
        ctx: &OverlayContext,
    ) -> Option<OverlayEdit> {
        let (index, kind) = handle.id.path_point()?;
        let (document, _, _) = ctx.resolved()?;
        let network = ctx.selection.as_ref()?.path.clone()?;
        let selected: Vec<_> = ctx.selection.as_ref()?.nodes.iter().copied().collect();
        let [node] = selected.as_slice() else {
            return None;
        };
        let graph = ravel_ui::document::resolve_network(document, &network)?;
        let original = path_points(graph.node(*node)?)?;
        let points: Vec<PathPoint> = edited_path_points(original, index, kind, delta);
        Some(OverlayEdit::NodeParameter {
            network,
            node: *node,
            key: "points".into(),
            value: ParameterValue::PathPoints(points),
        })
    }
}

// ===========================================================================
// The layer shell manipulator
// ===========================================================================

/// One grip of [`ShellManipulator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellHandle {
    /// A scale grip, indexed into [`selection_handle_centers`]' order.
    Scale(u8),
    /// The rotation ring around the corner grip with this index.
    Rotate(u8),
    /// The anchor marker.
    Anchor,
    /// The move grip at the middle of the bbox.
    Position,
}

impl ShellHandle {
    /// Which axes a scale grip drives: corners both, edge midpoints one.
    fn scale_axes(index: u8) -> (bool, bool) {
        match index {
            1 | 6 => (false, true),
            3 | 4 => (true, false),
            _ => (true, true),
        }
    }

    /// The cursor a scale grip promises, from the diagonal it sits on.
    fn scale_hint(index: u8) -> ViewerPointerHint {
        match index {
            0 | 7 => ViewerPointerHint::ResizeUpLeftDownRight,
            2 | 5 => ViewerPointerHint::ResizeUpRightDownLeft,
            1 | 6 => ViewerPointerHint::ResizeUpDown,
            _ => ViewerPointerHint::ResizeLeftRight,
        }
    }
}

/// Anchor marker colour: warm, so it never reads as one of the blue scale
/// handles.
const ANCHOR_COLOR: Hsla = Hsla {
    h: 0.09,
    s: 0.9,
    l: 0.6,
    a: 0.95,
};

/// The line from a child's anchor to its parent's.
const PARENT_LINK_COLOR: Hsla = Hsla {
    h: 0.09,
    s: 0.5,
    l: 0.6,
    a: 0.55,
};

/// Screen-pixel side length of the anchor marker.
const ANCHOR_MARKER_PX: f32 = 11.0;

/// The rotation ring: the selection accent held back so the ring reads as a
/// zone around the corner rather than as another grip.
const ROTATE_RING_COLOR: Hsla = Hsla {
    h: 0.58,
    s: 0.7,
    l: 0.6,
    a: 0.4,
};

/// An angle in radians folded into `(−π, π]`.
fn wrap_angle(radians: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let wrapped = radians.rem_euclid(TAU);
    if wrapped > PI { wrapped - TAU } else { wrapped }
}

/// `numerator / denominator`, or `1.0` when the denominator is too small to
/// carry a ratio — a grip that already sits on its fixed point cannot say
/// anything about scale, so it must leave the channel where it is.
fn scale_ratio(numerator: f32, denominator: f32) -> f32 {
    if denominator.abs() < 1e-4 {
        1.0
    } else {
        numerator / denominator
    }
}

/// The shell transform of one layer, resolved at the current frame, plus the
/// matrices the grips need. Every field is read-only; the edits are derived.
struct ShellState {
    comp: CompId,
    layer: LayerId,
    /// World-space AABB of the layer's drawn geometry — the same rectangle
    /// the selection bbox outlines.
    rect: CompRect,
    /// The parent chain's matrix; identity when the layer has no parent.
    /// `position` lives in this space.
    parent: Affine,
    /// `parent · layer`: the matrix taking layer-local points to the canvas.
    world: Affine,
    anchor: (f32, f32),
    position: (f32, f32),
    scale: (f32, f32),
    /// Degrees, the unit the channel stores.
    rotation: f32,
    /// The layer-local frame every write targets (REQ-LAYER-006).
    local_frame: u64,
    /// Where the anchor lands on the canvas: `parent · position`.
    anchor_world: (f32, f32),
    /// The parent layer's own anchor, for the link line.
    parent_anchor_world: Option<(f32, f32)>,
}

impl ShellState {
    fn resolve(ctx: &OverlayContext) -> Option<Self> {
        Self::resolve_in(ctx, ctx.document.as_ref()?)
    }

    /// The same resolution against a document the caller supplies, so the HUD
    /// can read the shell as it stood when the gesture pressed.
    fn resolve_in(ctx: &OverlayContext, document: &Document) -> Option<Self> {
        let (resolution, playback) = (ctx.resolution?, ctx.playback?);
        let comp_id = ctx.layer_selection.comp()?;
        // Exactly one layer: two or more have no single shell to manipulate,
        // and the multi-layer bbox already says so by drawing no handles.
        let [layer_id] = ctx.layer_selection.layers() else {
            return None;
        };
        let comp = document.get_composition(comp_id)?;
        let layer = comp.get_layer(*layer_id)?;
        let eval = EvalContext::new(playback.frame, playback.fps, resolution);
        let rect = super::layer_comp_rect(ctx, document, comp_id, *layer_id)?;

        let anchor_of = |layer: &ravel_core::composition::Layer| {
            let lf = layer.local_frame_continuous(eval.sample_frame());
            (
                layer.transform.anchor_point[0].evaluate(lf, &eval),
                layer.transform.anchor_point[1].evaluate(lf, &eval),
            )
        };
        let lf = layer.local_frame_continuous(eval.sample_frame());
        let transform = &layer.transform;
        let position = (
            transform.position[0].evaluate(lf, &eval),
            transform.position[1].evaluate(lf, &eval),
        );
        let parent_layer = layer.parent.and_then(|id| comp.get_layer(id));
        let parent = parent_layer
            .map(|parent| world_matrix(comp, parent, &eval))
            .unwrap_or(Affine::IDENTITY);
        Some(Self {
            comp: comp_id,
            layer: *layer_id,
            rect,
            parent,
            world: world_matrix(comp, layer, &eval),
            anchor: anchor_of(layer),
            position,
            scale: (
                transform.scale[0].evaluate(lf, &eval),
                transform.scale[1].evaluate(lf, &eval),
            ),
            rotation: transform.rotation.evaluate(lf, &eval),
            local_frame: ravel_ui::keyframes::layer_local_frame(layer, playback.frame),
            anchor_world: parent.apply(position.0, position.1),
            parent_anchor_world: parent_layer.map(|parent| {
                let anchor = anchor_of(parent);
                world_matrix(comp, parent, &eval).apply(anchor.0, anchor.1)
            }),
        })
    }

    fn centers(&self) -> [(f32, f32); 8] {
        selection_handle_centers(self.rect.x, self.rect.y, self.rect.w, self.rect.h)
    }

    fn rect_center(&self) -> (f32, f32) {
        (
            self.rect.x + self.rect.w * 0.5,
            self.rect.y + self.rect.h * 0.5,
        )
    }

    /// One channel write at the layer's own local frame. `Some(frame)` is what
    /// keeps a keyframed channel keyframed: [`param_edit::edited_channel`]
    /// only collapses a curve when the frame is `None`.
    ///
    /// [`param_edit::edited_channel`]: crate::panels::param_edit::edited_channel
    fn write(&self, channel: ShellChannel, value: f32) -> OverlayEdit {
        OverlayEdit::LayerTransform {
            comp: self.comp,
            layer: self.layer,
            channel,
            value,
            local_frame: Some(self.local_frame),
        }
    }

    /// Scale so the grabbed grip follows the pointer while a fixed point stays
    /// put: the opposite grip by default, the anchor under Alt.
    fn scale_edits(
        &self,
        index: u8,
        grabbed: (f32, f32),
        target: (f32, f32),
        modifiers: DragModifiers,
    ) -> Option<Vec<OverlayEdit>> {
        let inverse = self.world.inverse()?;
        let local = |point: (f32, f32)| inverse.apply(point.0, point.1);
        // Ratios are taken in layer-local space, so the current scale, the
        // rotation and the whole parent chain are already divided out and the
        // result is the factor to multiply onto `scale`.
        let fixed = if modifiers.alt {
            self.anchor
        } else {
            local(self.centers()[7 - index as usize])
        };
        let grabbed = local(grabbed);
        let moved = local(target);
        let ratio_x = scale_ratio(moved.0 - fixed.0, grabbed.0 - fixed.0);
        let ratio_y = scale_ratio(moved.1 - fixed.1, grabbed.1 - fixed.1);
        let axes = ShellHandle::scale_axes(index);
        let (factor_x, factor_y) = if modifiers.shift {
            // The larger movement decides both axes — the rule `drag_geometry`
            // uses to square off a shape drag.
            let uniform = match axes {
                (true, true) if (moved.1 - grabbed.1).abs() > (moved.0 - grabbed.0).abs() => {
                    ratio_y
                }
                (false, true) => ratio_y,
                _ => ratio_x,
            };
            (uniform, uniform)
        } else {
            (
                if axes.0 { ratio_x } else { 1.0 },
                if axes.1 { ratio_y } else { 1.0 },
            )
        };
        let scaled = (self.scale.0 * factor_x, self.scale.1 * factor_y);
        let mut edits = vec![
            self.write(ShellChannel::Scale(Axis::X), scaled.0),
            self.write(ShellChannel::Scale(Axis::Y), scaled.1),
        ];
        if !modifiers.alt {
            // The layer's matrix pins the anchor, not the opposite grip, so
            // scaling about anything else drags the content along unless
            // `position` absorbs the difference:
            //   L(q) = R·S·(q − a) + p, and holding L(fixed) fixed gives
            //   p' = p + R·(S − S')·(fixed − a).
            let (sin, cos) = self.rotation.to_radians().sin_cos();
            let dx = (self.scale.0 - scaled.0) * (fixed.0 - self.anchor.0);
            let dy = (self.scale.1 - scaled.1) * (fixed.1 - self.anchor.1);
            edits.push(self.write(
                ShellChannel::Position(Axis::X),
                self.position.0 + cos * dx - sin * dy,
            ));
            edits.push(self.write(
                ShellChannel::Position(Axis::Y),
                self.position.1 + sin * dx + cos * dy,
            ));
        }
        Some(edits)
    }

    /// Rotation measured in the parent's space, where the layer turns around
    /// `position`: the angle the pointer sweeps there *is* the delta in
    /// degrees, whatever the parent chain does.
    fn rotate_edits(&self, grabbed: (f32, f32), target: (f32, f32)) -> Option<Vec<OverlayEdit>> {
        let inverse = self.parent.inverse()?;
        let arm = |point: (f32, f32)| {
            let point = inverse.apply(point.0, point.1);
            (point.0 - self.position.0, point.1 - self.position.1)
        };
        let from = arm(grabbed);
        let to = arm(target);
        if from.0.hypot(from.1) < 1e-3 || to.0.hypot(to.1) < 1e-3 {
            return None;
        }
        // `atan2` is discontinuous across the negative x axis, so the raw
        // difference of two arms straddling it reads as most of a turn the
        // wrong way (+2° arrives as −358°). Fold it back into (−π, π]: a
        // pointer drag is always the short way round.
        let delta = wrap_angle(to.1.atan2(to.0) - from.1.atan2(from.0));
        Some(vec![self.write(
            ShellChannel::Rotation,
            self.rotation + delta.to_degrees(),
        )])
    }

    /// Move the anchor without moving the picture.
    ///
    /// The anchor becomes the layer-local point under the pointer and
    /// `position` becomes the same point in the parent's space. That pair is
    /// exactly the correction that leaves `L = T(p)·R·S·T(−a)` unchanged:
    /// writing the anchor alone would shift the content by `R·S·(a' − a)`.
    fn anchor_edits(&self, target: (f32, f32)) -> Option<Vec<OverlayEdit>> {
        let world = self.world.inverse()?;
        let parent = self.parent.inverse()?;
        let anchor = world.apply(target.0, target.1);
        let position = parent.apply(target.0, target.1);
        Some(vec![
            self.write(ShellChannel::AnchorPoint(Axis::X), anchor.0),
            self.write(ShellChannel::AnchorPoint(Axis::Y), anchor.1),
            self.write(ShellChannel::Position(Axis::X), position.0),
            self.write(ShellChannel::Position(Axis::Y), position.1),
        ])
    }

    fn position_edits(&self, grabbed: (f32, f32), target: (f32, f32)) -> Option<Vec<OverlayEdit>> {
        let inverse = self.parent.inverse()?;
        let from = inverse.apply(grabbed.0, grabbed.1);
        let to = inverse.apply(target.0, target.1);
        Some(vec![
            self.write(
                ShellChannel::Position(Axis::X),
                self.position.0 + to.0 - from.0,
            ),
            self.write(
                ShellChannel::Position(Axis::Y),
                self.position.1 + to.1 - from.1,
            ),
        ])
    }

    /// What the drag HUD shows for the grip being held: how far the gesture
    /// has got, read as the difference between the previewed document (`self`)
    /// and the one the press captured (`press`).
    ///
    /// Scale reports the factor this drag applied and rotation the angle it
    /// swept, because "200%" tells you nothing while you are dragging a layer
    /// that was already at 200%. The two positional channels report
    /// coordinates instead — the plan's "位置なら座標" — since where the
    /// anchor or the layer now sits is the thing being aimed.
    fn hud(&self, press: &ShellState, handle: ShellHandle) -> String {
        match handle {
            ShellHandle::Scale(_) => format!(
                "{:.1}% × {:.1}%",
                scale_ratio(self.scale.0, press.scale.0) * 100.0,
                scale_ratio(self.scale.1, press.scale.1) * 100.0
            ),
            ShellHandle::Rotate(_) => format!("{:+.1}°", self.rotation - press.rotation),
            ShellHandle::Anchor => format!("({:.1}, {:.1})", self.anchor.0, self.anchor.1),
            ShellHandle::Position => format!("({:.1}, {:.1})", self.position.0, self.position.1),
        }
    }
}

/// The manipulator for a single selected layer's shell transform: scale on the
/// eight bbox grips, rotation in the ring just outside each corner, the anchor
/// marker, and a move grip at the middle.
///
/// It stands down unless exactly one layer is selected — with two or more
/// there is no single shell to write, and with none there is nothing to
/// outline.
///
/// The parent link line lives here too. Choosing the parent is `SHELL-5`'s
/// Properties dropdown; this overlay only shows the relationship, as a line
/// from the child's anchor to the parent's.
pub struct ShellManipulator;

impl ShellManipulator {
    pub const ID: OverlayId = OverlayId("viewer.shell_manipulator");
    /// Screen-pixel grab radius of a grip.
    const HIT_RADIUS_PX: f32 = 8.0;
    /// Rotation is grabbed in the ring *around* a corner grip: the same anchor
    /// point with a wider radius, listed after every scale grip so the inner
    /// disc still scales and only the surrounding ring turns. Keeping both on
    /// one point is what makes the rotation zone zoom-invariant without the
    /// overlay having to know the zoom.
    const ROTATE_RADIUS_PX: f32 = 18.0;
    /// The bbox grips that carry a rotation ring, in
    /// [`selection_handle_centers`]' order: the four corners. One list, read
    /// by both `paint` and `handles`, so the drawn ring cannot drift away from
    /// the zone that answers the pointer.
    const ROTATE_CORNERS: [u8; 4] = [0, 2, 5, 7];
}

impl ViewerOverlay for ShellManipulator {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::SHELL_MANIPULATOR
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        // Only the Select tool. The overlay hit test runs before
        // `select_mouse_down` / `shape_mouse_down`, so a manipulator that
        // stayed live under Rect / Ellipse / Hand / Zoom would answer the
        // press those tools are waiting for and start a transform instead of
        // a shape or a pan.
        ctx.tool == Some(ToolKind::Select) && ShellState::resolve(ctx).is_some()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let Some(state) = ShellState::resolve(ctx) else {
            return;
        };
        painter.stroke_comp_rect(state.rect, SELECTION_COLOR);
        let centers = state.centers();
        // The rotation zone is drawn before the grips so the grip's mark stays
        // on top of the ring that surrounds it. A cursor promises what works,
        // and an undrawn hit radius promises nothing.
        for corner in Self::ROTATE_CORNERS {
            painter.screen_ring_at(
                centers[corner as usize],
                Self::ROTATE_RADIUS_PX,
                1.0,
                ROTATE_RING_COLOR,
            );
        }
        for center in centers {
            paint_handle_mark(painter, center, SELECTION_HANDLE_PX, SELECTION_COLOR);
        }
        painter.screen_square_at(
            state.rect_center(),
            SELECTION_HANDLE_PX - 2.0,
            SELECTION_COLOR,
        );
        // Drawn before the marker so the line ends under it rather than over.
        if let Some(parent) = state.parent_anchor_world {
            painter.stroke_comp_polyline(
                &[state.anchor_world, parent],
                false,
                1.0,
                PARENT_LINK_COLOR,
            );
        }
        paint_handle_mark(painter, state.anchor_world, ANCHOR_MARKER_PX, ANCHOR_COLOR);
    }

    fn labels(&self, ctx: &OverlayContext) -> Vec<OverlayLabel> {
        let Some(drag) = ctx.active_drag.as_ref() else {
            return Vec::new();
        };
        let Some(handle) = drag.handle.shell() else {
            return Vec::new();
        };
        let (Some(state), Some(press)) = (
            ShellState::resolve(ctx),
            ShellState::resolve_in(ctx, &drag.press_document),
        ) else {
            return Vec::new();
        };
        vec![OverlayLabel {
            text: SharedString::from(state.hud(&press, handle)),
            color: SELECTION_COLOR,
            placement: LabelPlacement::CanvasTopLeft,
        }]
    }

    fn handles(&self, ctx: &OverlayContext) -> Vec<OverlayHandle> {
        let Some(state) = ShellState::resolve(ctx) else {
            return Vec::new();
        };
        let grip = |id, position, hit_radius_px, hint| OverlayHandle {
            overlay: Self::ID,
            id: OverlayHandleId::Shell(id),
            position,
            hit_radius_px,
            hint,
            draggable: true,
        };
        let mut handles = vec![
            // First, so an anchor parked on a bbox grip still wins the press:
            // there is no other way to grab it, while every scale grip has
            // seven siblings.
            grip(
                ShellHandle::Anchor,
                state.anchor_world,
                Self::HIT_RADIUS_PX,
                ViewerPointerHint::ShellAnchor,
            ),
            grip(
                ShellHandle::Position,
                state.rect_center(),
                Self::HIT_RADIUS_PX,
                ViewerPointerHint::MovableBody,
            ),
        ];
        for (index, center) in state.centers().into_iter().enumerate() {
            let index = index as u8;
            handles.push(grip(
                ShellHandle::Scale(index),
                center,
                Self::HIT_RADIUS_PX,
                ShellHandle::scale_hint(index),
            ));
        }
        for index in Self::ROTATE_CORNERS {
            handles.push(grip(
                ShellHandle::Rotate(index),
                state.centers()[index as usize],
                Self::ROTATE_RADIUS_PX,
                ViewerPointerHint::Rotate,
            ));
        }
        handles
    }

    fn drag(
        &self,
        handle: &OverlayHandle,
        delta: (f32, f32),
        modifiers: DragModifiers,
        ctx: &OverlayContext,
    ) -> Option<OverlayEdit> {
        let state = ShellState::resolve(ctx)?;
        // The grip's own mark is treated as the grabbed point, so the gesture
        // stays absolute in `delta` instead of accumulating pointer offsets.
        let target = (handle.position.0 + delta.0, handle.position.1 + delta.1);
        let edits = match handle.id.shell()? {
            ShellHandle::Scale(index) => {
                state.scale_edits(index, handle.position, target, modifiers)?
            }
            ShellHandle::Rotate(_) => state.rotate_edits(handle.position, target)?,
            ShellHandle::Anchor => state.anchor_edits(target)?,
            ShellHandle::Position => state.position_edits(handle.position, target)?,
        };
        Some(OverlayEdit::Batch(edits))
    }
}

// ===========================================================================
// The node parameter manipulator
// ===========================================================================

/// Screen-pixel side length of a parameter mark. Larger than
/// [`SELECTION_HANDLE_PX`] so it reads as its own grip where it lands on a
/// bbox handle, and the hit radius below matches it.
const PARAM_MARK_PX: f32 = 9.0;

/// One manipulable parameter of the selected node, resolved at the current
/// frame.
#[derive(Clone, Debug, PartialEq)]
struct ParamMark {
    key: String,
    role: ParamRole,
    /// The layer-local point the mark sits at.
    local: (f32, f32),
    /// The same point on the canvas: `world · local`.
    world: (f32, f32),
    /// The parameter's declared clamp boundary, applied to every component a
    /// drag writes. Without it a gesture can push a value where no other
    /// editor can: a radius past zero and out the other side.
    range: Option<ParamRange>,
}

/// The selected node's role-carrying parameters, resolved once and read by
/// `paint`, `handles` and `drag` alike — so a drawn mark, the point that
/// answers the pointer, and the value a drag writes cannot drift apart.
struct ParamState {
    network: NetworkPath,
    node: Arc<ravel_core::graph::Node>,
    /// The layer's compositing matrix: parameters are written in the node's
    /// own space, and the handles have to sit on the picture.
    world: Affine,
    /// The layer-local frame every write targets (REQ-LAYER-006).
    local_frame: u64,
    /// The layer-local point [`ParamRole::Size`] is measured from.
    anchor: (f32, f32),
    marks: Vec<ParamMark>,
}

impl ParamState {
    fn resolve(ctx: &OverlayContext) -> Option<Self> {
        let (document, resolution, playback) = ctx.resolved()?;
        let registry = ctx.registry.as_ref()?;
        let selection = ctx.selection.as_ref()?;
        let network = selection.path.clone()?;
        // Exactly one node: two selected nodes have no single parameter set to
        // put handles on, and the marks of both at once would overlap without
        // saying which is which.
        let mut selected = selection.nodes.iter().copied();
        let node_id = selected.next()?;
        if selected.next().is_some() {
            return None;
        }
        let comp = document.get_composition(network.comp)?;
        let layer = comp.get_layer(network.layer)?;
        let graph = ravel_ui::document::resolve_network(document, &network)?;
        let node = graph.node(node_id)?.clone();
        let template = registry.get(&node.type_key)?;
        let eval = EvalContext::new(playback.frame, playback.fps, resolution);
        // Network parameters live in layer-local time (REQ-LAYER-006).
        let local_frame = ravel_ui::keyframes::layer_local_frame(layer, playback.frame);
        let world = world_matrix(comp, layer, &eval);

        // A parameter fed by a connected port is not the node's to write: the
        // stored value is overridden at evaluation, so a handle on it would
        // move nothing and still pile up undo steps. The Properties panel
        // stops the same edits from the same list.
        let driven = crate::panels::node_editor::driven_params(graph, &node, registry);

        // Declaration order, so the marks keep a stable index across a
        // gesture: the handle id carries nothing else.
        let declared: Vec<(String, ParamRole, (f32, f32))> = node
            .parameters
            .iter()
            .filter_map(|parameter| {
                let role = template.param_role(&parameter.key)?;
                if driven.iter().any(|driven| driven.key == parameter.key) {
                    return None;
                }
                let value = super::sample_vec2_param(&node, &parameter.key, local_frame, &eval)?;
                Some((parameter.key.clone(), role, value))
            })
            .collect();
        let anchor = declared
            .iter()
            .find(|(_, role, _)| *role == ParamRole::Position)
            .map_or((0.0, 0.0), |(_, _, value)| *value);
        let marks: Vec<ParamMark> = declared
            .iter()
            .map(|(key, role, value)| {
                let local = match role {
                    ParamRole::Position => *value,
                    ParamRole::Size => (anchor.0 + value.0, anchor.1 + value.1),
                };
                ParamMark {
                    key: key.clone(),
                    role: *role,
                    local,
                    world: world.apply(local.0, local.1),
                    range: template.param_range(key).cloned(),
                }
            })
            .collect();
        if marks.is_empty() {
            return None;
        }
        Some(Self {
            network,
            node,
            world,
            local_frame,
            anchor,
            marks,
        })
    }

    /// The value `mark` takes when its handle is dragged to the canvas point
    /// `target`, in the node's own space.
    fn value_at(&self, mark: &ParamMark, target: (f32, f32)) -> Option<(f32, f32)> {
        let local = self.world.inverse()?.apply(target.0, target.1);
        let (x, y) = match mark.role {
            ParamRole::Position => local,
            ParamRole::Size => (local.0 - self.anchor.0, local.1 - self.anchor.1),
        };
        // The declared hard boundary, the one every other editor clamps to.
        Some(match &mark.range {
            Some(range) => (range.clamp(x), range.clamp(y)),
            None => (x, y),
        })
    }
}

/// Handles for the parameters the selected node declares a [`ParamRole`] for.
///
/// The declaration is the whole mechanism: this overlay names no type key and
/// no parameter spelling, so a node becomes manipulable by declaring a role in
/// the registry and nothing here changes.
///
/// Writes go through [`param_edit::edited_vector_param`], which is what keeps
/// an animated parameter animated: a drag inserts a key at the layer-local
/// frame instead of collapsing the curve, the same rule the Properties scrub
/// follows.
///
/// [`param_edit::edited_vector_param`]: crate::panels::param_edit::edited_vector_param
pub struct ParamManipulator;

impl ParamManipulator {
    pub const ID: OverlayId = OverlayId("viewer.param_manipulator");
    /// Screen-pixel grab radius, the same measure the mark is drawn with so a
    /// grabbable point is always a visible one.
    const HIT_RADIUS_PX: f32 = PARAM_MARK_PX;
}

impl ViewerOverlay for ParamManipulator {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::PARAM_MANIPULATOR
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        // Only the Select tool, for the reason [`ShellManipulator`] states:
        // the overlay hit test runs before `select_mouse_down` /
        // `shape_mouse_down`, so a live manipulator under Rect / Ellipse /
        // Hand / Zoom would answer the press those tools are waiting for.
        ctx.tool == Some(ToolKind::Select) && ParamState::resolve(ctx).is_some()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let Some(state) = ParamState::resolve(ctx) else {
            return;
        };
        let color = ctx.colors.path;
        let anchor_world = state.world.apply(state.anchor.0, state.anchor.1);
        for mark in &state.marks {
            // A size is an offset, so the tether says what it is measured
            // from; without it the mark reads as a second position.
            if mark.role == ParamRole::Size {
                painter.stroke_comp_polyline(&[anchor_world, mark.world], false, 1.0, color);
            }
            paint_handle_mark(painter, mark.world, PARAM_MARK_PX, color);
        }
    }

    fn handles(&self, ctx: &OverlayContext) -> Vec<OverlayHandle> {
        let Some(state) = ParamState::resolve(ctx) else {
            return Vec::new();
        };
        state
            .marks
            .iter()
            .enumerate()
            .map(|(index, mark)| OverlayHandle {
                overlay: Self::ID,
                id: OverlayHandleId::Param(index as u8),
                position: mark.world,
                hit_radius_px: Self::HIT_RADIUS_PX,
                hint: match mark.role {
                    ParamRole::Size => ViewerPointerHint::ResizeUpLeftDownRight,
                    _ => ViewerPointerHint::MovableBody,
                },
                draggable: true,
            })
            .collect()
    }

    fn drag(
        &self,
        handle: &OverlayHandle,
        delta: (f32, f32),
        _modifiers: DragModifiers,
        ctx: &OverlayContext,
    ) -> Option<OverlayEdit> {
        let state = ParamState::resolve(ctx)?;
        let mark = state.marks.get(handle.id.param()? as usize)?;
        // The mark's own point is the grabbed one, so repeated calls during a
        // gesture stay absolute instead of compounding onto their preview.
        let target = (handle.position.0 + delta.0, handle.position.1 + delta.1);
        let (x, y) = state.value_at(mark, target)?;
        let existing = &state
            .node
            .parameters
            .iter()
            .find(|parameter| parameter.key == mark.key)?
            .value;
        // Two components for a two-dimensional canvas: a `Channel3` keeps its
        // Z, and every component keeps its curve.
        let value = crate::panels::param_edit::edited_vector_param(
            existing,
            &[x, y],
            Some(state.local_frame),
        )?;
        Some(OverlayEdit::NodeParameter {
            network: state.network.clone(),
            node: state.node.id,
            key: mark.key.clone().into(),
            value,
        })
    }
}

/// The latest evaluation error, centered over the canvas.
///
/// Text is the one thing the canvas painter cannot express, so this overlay
/// emits a [`OverlayLabel`] instead of primitives. Keeping it in the registry
/// is what lets unit 2 see that it needs no evaluation and unit 7 reuse the
/// same channel for the drag HUD.
pub struct EvalErrorOverlay;

impl EvalErrorOverlay {
    pub const ID: OverlayId = OverlayId("viewer.eval_error");
}

impl ViewerOverlay for EvalErrorOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::EVAL_ERROR
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        ctx.error.is_some()
    }

    fn labels(&self, ctx: &OverlayContext) -> Vec<OverlayLabel> {
        let Some(message) = ctx.error.as_ref() else {
            return Vec::new();
        };
        let label = ravel_i18n::t!("viewer.eval_error");
        vec![OverlayLabel {
            text: SharedString::from(format!("{label}: {message}")),
            color: ctx.colors.error,
            placement: LabelPlacement::CanvasCenter,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::KeyframeCurve;
    use ravel_core::animation::channel::ChannelSource;
    use ravel_core::composition::{Composition, Layer};
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::NodeId;
    use ravel_core::types::{FrameRate, Vec2};

    /// A frame placed away from the panel origin and zoomed to 0.5, so a bug
    /// that forgets either the offset or the scale shows up.
    fn painter() -> OverlayPainter {
        OverlayPainter::new(
            Bounds {
                origin: point(px(100.0), px(50.0)),
                size: size(px(960.0), px(540.0)),
            },
            (1920, 1080),
        )
    }

    fn colors() -> OverlayColors {
        OverlayColors {
            path: gpui::hsla(0.5, 0.5, 0.5, 1.0),
            error: gpui::hsla(0.0, 1.0, 0.5, 1.0),
        }
    }

    fn base_context() -> OverlayContext {
        OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(PlaybackPosition::default()),
            document: None,
            selection: None,
            layer_selection: LayerSelection::default(),
            tool: Some(ToolKind::Select),
            eval_resolution: Some((1920, 1080)),
            show_grid: false,
            show_safe_areas: false,
            show_geometry_bounds: true,
            show_geometry_points: false,
            show_geometry_paths: false,
            geometry_arrow_attr: None,
            show_geometry_indices: false,
            show_geometry_groups: false,
            field_display: crate::panels::viewer::field::FieldDisplay::default(),
            field_map: crate::panels::viewer::field::FieldColorMap::default(),
            field_opacity: crate::panels::viewer::field::DEFAULT_FIELD_OPACITY,
            error: None,
            active_drag: None,
            colors: colors(),
            results: OverlayResults::default(),
            // Absent by default so every overlay that does not read the
            // templates is tested against the same snapshot it always was;
            // `param_context` supplies the real one.
            registry: None,
        }
    }

    fn quads(primitives: &[OverlayPrimitive]) -> Vec<(Bounds<Pixels>, Hsla)> {
        primitives
            .iter()
            .filter_map(|primitive| match primitive {
                OverlayPrimitive::Quad { bounds, color } => Some((*bounds, *color)),
                OverlayPrimitive::Stroke { .. } => None,
            })
            .collect()
    }

    fn strokes(primitives: &[OverlayPrimitive]) -> Vec<&OverlayPrimitive> {
        primitives
            .iter()
            .filter(|primitive| matches!(primitive, OverlayPrimitive::Stroke { .. }))
            .collect()
    }

    fn close_to(actual: Bounds<Pixels>, expected: Bounds<Pixels>) {
        let pairs = [
            (actual.origin.x, expected.origin.x),
            (actual.origin.y, expected.origin.y),
            (actual.size.width, expected.size.width),
            (actual.size.height, expected.size.height),
        ];
        for (a, b) in pairs {
            assert!(
                (f32::from(a) - f32::from(b)).abs() < 1e-3,
                "{actual:?} != {expected:?}"
            );
        }
    }

    /// The pre-registry outline: four one-pixel quads around a screen rect.
    fn legacy_outline(rect: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
        let line = px(1.0);
        vec![
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
        ]
    }

    // -----------------------------------------------------------------------
    // Documents to drive the document-backed overlays
    // -----------------------------------------------------------------------

    fn rect_node(center: (f32, f32)) -> Node {
        Node::new(NodeId::next(), "shape.rect")
            .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY)
            .with_param("center", ParameterValue::vec2(center.0, center.1))
            .with_param("width", ParameterValue::Float(40.0))
            .with_param("height", ParameterValue::Float(20.0))
    }

    /// The geometry a test node stands for.
    ///
    /// The overlays read evaluated values, so a document alone no longer
    /// decides what they draw. These tests are about the overlays; the real
    /// processors are driven through the real evaluator in the parent
    /// module's tests, which is where "the bbox follows the evaluation" is
    /// pinned. Here the stub only has to be the shape the assertions assume.
    fn stub_geometry(node: &Node) -> Option<ravel_core::geometry::Geometry> {
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        match node.type_key.as_str() {
            "shape.rect" => {
                let (cx, cy) = crate::panels::viewer::sample_vec2_param(node, "center", 0, &ctx)?;
                let (hw, hh) = (
                    crate::panels::viewer::sample_float_param(node, "width", 0, &ctx)? * 0.5,
                    crate::panels::viewer::sample_float_param(node, "height", 0, &ctx)? * 0.5,
                );
                Some(ravel_core::geometry::Geometry::from_points(vec![
                    Vec2(cx - hw, cy - hh),
                    Vec2(cx + hw, cy - hh),
                    Vec2(cx + hw, cy + hh),
                    Vec2(cx - hw, cy + hh),
                ]))
            }
            "shape.custom_path" => {
                let points = crate::panels::viewer::path_points(node)?;
                Some(ravel_core::geometry::Geometry::from_points(
                    points
                        .iter()
                        .map(|point| Vec2(point.p.0, point.p.1))
                        .collect(),
                ))
            }
            _ => None,
        }
    }

    /// The published snapshot a document's own layer networks would produce.
    fn stub_results(document: &Document) -> OverlayResults {
        let mut values: HashMap<OverlayResultKey, Arc<dyn NodeData>> = HashMap::new();
        for comp in document.compositions.values() {
            for layer in &comp.layers {
                let path = NetworkPath::layer(comp.id, layer.id).segments();
                for node in layer.network.nodes() {
                    if let Some(geometry) = stub_geometry(node) {
                        values.insert((path.clone(), node.id), Arc::new(geometry));
                    }
                }
            }
        }
        OverlayResults::new(values)
    }

    fn path_node(points: Vec<PathPoint>) -> Node {
        Node::new(NodeId::next(), "shape.custom_path")
            .with_output("geometry", ravel_core::id::DataTypeId::GEOMETRY)
            .with_param("points", ParameterValue::PathPoints(points))
            .with_param("closed", ParameterValue::Bool(false))
    }

    /// One composition holding one layer whose network is `node`, plus the
    /// selection that points at it.
    fn doc_with_node(node: Node) -> (OverlayContext, NodeId, CompId, LayerId) {
        let node_id = node.id;
        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let graph = Graph::new().add_node(node).unwrap();
        let comp = Composition::new(comp_id, "Comp", (1920, 1080), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(layer_id, "Layer", graph).with_time(0, 0, 300));
        let document = Document::default().with_composition(comp);
        let mut ctx = base_context();
        ctx.results = stub_results(&document);
        ctx.document = Some(document);
        ctx.selection = Some(CanvasSelection {
            path: Some(NetworkPath::layer(comp_id, layer_id)),
            nodes: std::collections::HashSet::from([node_id]),
        });
        (ctx, node_id, comp_id, layer_id)
    }

    /// The fixture's node standing for `geometry`, so a test can drive a
    /// geometry the stub generator does not produce — one carrying the
    /// attribute columns unit 8 draws.
    fn ctx_with_geometry(
        geometry: ravel_core::geometry::Geometry,
    ) -> (OverlayContext, CompId, LayerId) {
        let (mut ctx, node, comp, layer) = doc_with_node(rect_node((100.0, 200.0)));
        ctx.results = OverlayResults::new(HashMap::from([(
            (NetworkPath::layer(comp, layer).segments(), node),
            Arc::new(geometry) as Arc<dyn NodeData>,
        )]));
        // The bbox is another overlay's business; these tests read the marks.
        ctx.show_geometry_bounds = false;
        (ctx, comp, layer)
    }

    /// Two points carrying a `velocity` column and two groups.
    fn attributed_points() -> ravel_core::geometry::Geometry {
        use ravel_core::geometry::AttributeArray;
        let mut geometry =
            ravel_core::geometry::Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        geometry
            .points_mut()
            .insert(
                "velocity",
                AttributeArray::Vec2(vec![Vec2(3.0, 4.0), Vec2(-6.0, 8.0)]),
            )
            .unwrap();
        geometry
            .points_mut()
            .insert("head", AttributeArray::Bool(vec![true, false]))
            .unwrap();
        geometry
            .points_mut()
            .insert("tail", AttributeArray::Bool(vec![false, true]))
            .unwrap();
        geometry
    }

    /// The polyline vertices of every stroke, back in composition space — the
    /// coordinates the assertions are written in. Reads the zoom and offset off
    /// the same [`painter`] fixture the primitives were drawn with.
    fn stroke_polylines(primitives: &[OverlayPrimitive]) -> Vec<Vec<(f32, f32)>> {
        let painter = painter();
        let (zoom_x, zoom_y) = painter.zoom();
        let origin = painter.frame().origin;
        primitives
            .iter()
            .filter_map(|primitive| match primitive {
                OverlayPrimitive::Stroke { points, .. } => Some(
                    points
                        .iter()
                        .map(|p| {
                            (
                                (f32::from(p.x) - f32::from(origin.x)) / zoom_x,
                                (f32::from(p.y) - f32::from(origin.y)) / zoom_y,
                            )
                        })
                        .collect(),
                ),
                OverlayPrimitive::Quad { .. } => None,
            })
            .collect()
    }

    fn close_point(actual: (f32, f32), expected: (f32, f32), what: &str) {
        assert!(
            (actual.0 - expected.0).abs() < 0.05 && (actual.1 - expected.1).abs() < 0.05,
            "{what}: {actual:?} != {expected:?}"
        );
    }

    /// What `overlay` paints onto the shared [`painter`] fixture.
    fn painted(overlay: &dyn ViewerOverlay, ctx: &OverlayContext) -> Vec<OverlayPrimitive> {
        let mut painter = painter();
        overlay.paint(ctx, &mut painter);
        painter.finish()
    }

    // -----------------------------------------------------------------------
    // Unit 8: the geometry attributes drawn in place
    // -----------------------------------------------------------------------

    /// Completion criteria: the picked `Vec2` attribute is drawn as arrows with
    /// its own direction and length, an attribute the geometry does not carry
    /// draws nothing, and the display is a toggle.
    #[test]
    fn attribute_arrows_are_drawn_for_the_picked_attribute_only() {
        let (mut ctx, ..) = ctx_with_geometry(attributed_points());
        let overlay = GeometryOverlay {
            scope: BboxScope::Node,
        };
        assert!(overlay.is_active(&ctx));

        // Off by default: the toggle is what turns the arrows on.
        assert!(
            painted(&overlay, &ctx).is_empty(),
            "something was drawn with every attribute toggle off"
        );

        ctx.geometry_arrow_attr = Some("velocity".into());
        let arrows = stroke_polylines(&painted(&overlay, &ctx));
        assert_eq!(arrows.len(), 2, "one arrow per element: {arrows:?}");
        close_point(arrows[0][0], (0.0, 0.0), "first tail");
        close_point(arrows[0][1], (3.0, 4.0), "first tip");
        close_point(arrows[1][0], (10.0, 0.0), "second tail");
        close_point(arrows[1][1], (4.0, 8.0), "second tip");

        // An attribute this geometry does not carry draws nothing at all.
        ctx.geometry_arrow_attr = Some("force".into());
        assert!(
            painted(&overlay, &ctx).is_empty(),
            "an absent attribute still drew something"
        );
    }

    /// Both ends of an arrow go through the layer's compositing matrix, so an
    /// arrow on a rotated, non-uniformly scaled layer points where the geometry
    /// under it does.
    #[test]
    fn a_transformed_shell_carries_both_ends_of_an_arrow() {
        let (ctx, comp, layer) = ctx_with_geometry(attributed_points());
        let mut ctx = with_own_transform(&ctx, comp, layer);
        ctx.geometry_arrow_attr = Some("velocity".into());
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);

        let arrows = stroke_polylines(&painted(
            &GeometryOverlay {
                scope: BboxScope::Node,
            },
            &ctx,
        ));
        assert_eq!(arrows.len(), 2);
        close_point(arrows[0][0], world.apply(0.0, 0.0), "placed tail");
        close_point(arrows[0][1], world.apply(3.0, 4.0), "placed tip");
    }

    /// Completion criteria: the index labels are a toggle, they sit on the
    /// marks they name, and past the cap they are thinned.
    #[test]
    fn index_labels_ride_the_toggle_and_land_on_their_marks() {
        let (base, comp, layer) = ctx_with_geometry(attributed_points());
        let overlay = GeometryOverlay {
            scope: BboxScope::Node,
        };
        assert!(overlay.labels(&base).is_empty(), "labels drew while off");

        // A layer with a transform of its own, so a label that skipped the
        // compositing matrix lands somewhere else than the mark it names.
        let mut ctx = with_own_transform(&base, comp, layer);
        ctx.show_geometry_indices = true;
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);
        let labels = overlay.labels(&ctx);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].text.as_ref(), "0");
        assert_eq!(labels[1].text.as_ref(), "1");
        assert_eq!(
            labels[0].placement,
            LabelPlacement::Comp(world.apply(0.0, 0.0))
        );
        assert_eq!(
            labels[1].placement,
            LabelPlacement::Comp(world.apply(10.0, 0.0))
        );

        // Past the cap the labels thin out rather than pile up.
        let cloud = ravel_core::geometry::Geometry::from_points(
            (0..crate::panels::viewer::geometry::MAX_DRAWN_LABELS * 4)
                .map(|i| Vec2(i as f32, 0.0))
                .collect::<Vec<_>>(),
        );
        let (mut ctx, ..) = ctx_with_geometry(cloud);
        ctx.show_geometry_indices = true;
        let labels = overlay.labels(&ctx);
        assert!(
            labels.len() <= crate::panels::viewer::geometry::MAX_DRAWN_LABELS,
            "{} labels for one geometry",
            labels.len()
        );
    }

    /// Group colouring is a toggle, and a group's colour comes from its name so
    /// two groups never read as one.
    #[test]
    fn group_colours_ride_the_toggle() {
        let (mut ctx, ..) = ctx_with_geometry(attributed_points());
        ctx.show_geometry_points = true;
        let overlay = GeometryOverlay {
            scope: BboxScope::Node,
        };

        let plain: Vec<_> = quads(&painted(&overlay, &ctx))
            .into_iter()
            .map(|(_, color)| color)
            .collect();
        assert_eq!(plain.len(), 2);
        assert!(
            plain.iter().all(|color| *color == GEOMETRY_MARK_COLOR),
            "the marks were coloured with the toggle off"
        );

        ctx.show_geometry_groups = true;
        let grouped: Vec<_> = quads(&painted(&overlay, &ctx))
            .into_iter()
            .map(|(_, color)| color)
            .collect();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0], group_color("head"));
        assert_eq!(grouped[1], group_color("tail"));
        assert_ne!(grouped[0], grouped[1], "both groups read as one colour");
        assert!(grouped.iter().all(|color| *color != GEOMETRY_MARK_COLOR));
    }

    // -----------------------------------------------------------------------
    // Unit 1: the five ported overlays draw what they drew before
    // -----------------------------------------------------------------------

    #[test]
    fn grid_draws_screen_space_hairlines_at_the_frame_thirds() {
        let mut ctx = base_context();
        ctx.show_grid = true;
        assert!(GridOverlay.is_active(&ctx));

        let mut painter = painter();
        let frame = painter.frame();
        GridOverlay.paint(&ctx, &mut painter);
        let quads = quads(&painter.finish());
        assert_eq!(quads.len(), 4);

        // The pre-registry formula: a fraction of the frame rectangle, one
        // screen pixel wide, spanning the whole frame on the other axis.
        for (index, i) in (1..3).enumerate() {
            let t = i as f32 / 3.0;
            close_to(
                quads[index * 2].0,
                Bounds {
                    origin: point(frame.origin.x + frame.size.width * t, frame.origin.y),
                    size: size(px(1.0), frame.size.height),
                },
            );
            close_to(
                quads[index * 2 + 1].0,
                Bounds {
                    origin: point(frame.origin.x, frame.origin.y + frame.size.height * t),
                    size: size(frame.size.width, px(1.0)),
                },
            );
        }
        assert!(
            quads
                .iter()
                .all(|(_, color)| *color == overlay_line_color())
        );
    }

    #[test]
    fn safe_areas_outline_the_ninety_and_eighty_percent_rectangles() {
        let mut ctx = base_context();
        ctx.show_safe_areas = true;
        assert!(SafeAreaOverlay.is_active(&ctx));

        let mut painter = painter();
        let frame = painter.frame();
        SafeAreaOverlay.paint(&ctx, &mut painter);
        let quads = quads(&painter.finish());
        assert_eq!(quads.len(), 8, "two outlines of four edges each");

        for (index, fraction) in [0.9f32, 0.8].into_iter().enumerate() {
            let width = frame.size.width * fraction;
            let height = frame.size.height * fraction;
            let expected = legacy_outline(Bounds {
                origin: point(
                    frame.origin.x + (frame.size.width - width) * 0.5,
                    frame.origin.y + (frame.size.height - height) * 0.5,
                ),
                size: size(width, height),
            });
            for (edge, expected) in expected.into_iter().enumerate() {
                close_to(quads[index * 4 + edge].0, expected);
            }
        }
    }

    #[test]
    fn node_selection_bbox_outlines_the_shape_and_keeps_its_eight_handles() {
        let (ctx, ..) = doc_with_node(rect_node((100.0, 200.0)));
        let overlay = GeometryOverlay {
            scope: BboxScope::Node,
        };
        assert!(overlay.is_active(&ctx));

        let mut painter = painter();
        let frame = painter.frame();
        overlay.paint(&ctx, &mut painter);
        let quads = quads(&painter.finish());
        assert_eq!(
            quads.len(),
            4 + 8 * 2,
            "outline plus eight two-quad handles"
        );

        // The shape is 40x20 centered at (100, 200) in composition space.
        let (zoom_x, zoom_y) = (
            f32::from(frame.size.width) / 1920.0,
            f32::from(frame.size.height) / 1080.0,
        );
        let screen = Bounds {
            origin: point(
                frame.origin.x + px(80.0 * zoom_x),
                frame.origin.y + px(190.0 * zoom_y),
            ),
            size: size(px(40.0 * zoom_x), px(20.0 * zoom_y)),
        };
        for (edge, expected) in legacy_outline(screen).into_iter().enumerate() {
            close_to(quads[edge].0, expected);
        }

        let centers = selection_handle_centers(
            f32::from(screen.origin.x),
            f32::from(screen.origin.y),
            f32::from(screen.size.width),
            f32::from(screen.size.height),
        );
        for (index, center) in centers.into_iter().enumerate() {
            let outer = quads[4 + index * 2].0;
            close_to(
                outer,
                Bounds {
                    origin: point(
                        px(center.0 - SELECTION_HANDLE_PX * 0.5),
                        px(center.1 - SELECTION_HANDLE_PX * 0.5),
                    ),
                    size: size(px(SELECTION_HANDLE_PX), px(SELECTION_HANDLE_PX)),
                },
            );
            let inner = quads[4 + index * 2 + 1].0;
            close_to(
                inner,
                Bounds {
                    origin: point(
                        px(center.0 - SELECTION_HANDLE_PX * 0.5 + 1.0),
                        px(center.1 - SELECTION_HANDLE_PX * 0.5 + 1.0),
                    ),
                    size: size(px(SELECTION_HANDLE_PX - 2.0), px(SELECTION_HANDLE_PX - 2.0)),
                },
            );
        }
    }

    #[test]
    fn layer_selection_bbox_needs_two_layers_and_draws_no_handles() {
        let comp_id = CompId::next();
        let layers: Vec<LayerId> = (0..2).map(|_| LayerId::next()).collect();
        let mut comp = Composition::new(comp_id, "Comp", (1920, 1080), FrameRate::new(30, 1), 300);
        for (index, id) in layers.iter().enumerate() {
            let graph = Graph::new()
                .add_node(rect_node((100.0 * index as f32, 200.0)))
                .unwrap();
            comp = comp.add_layer(Layer::new(*id, "Layer", graph).with_time(0, 0, 300));
        }
        let mut ctx = base_context();
        let document = Document::default().with_composition(comp);
        ctx.results = stub_results(&document);
        ctx.document = Some(document);

        let overlay = GeometryOverlay {
            scope: BboxScope::Layer,
        };
        ctx.layer_selection = LayerSelection::of(comp_id, vec![layers[0]]);
        assert!(
            !overlay.is_active(&ctx),
            "a single layer is outlined by the node-level bbox"
        );

        ctx.layer_selection = LayerSelection::of(comp_id, layers);
        assert!(overlay.is_active(&ctx));
        let mut painter = painter();
        overlay.paint(&ctx, &mut painter);
        assert_eq!(
            quads(&painter.finish()).len(),
            8,
            "two outlines, no handles: there is no layer-level scale gesture"
        );
    }

    #[test]
    fn path_overlay_draws_the_curve_and_the_handles_that_grab_it() {
        let points = vec![
            PathPoint {
                p: Vec2(100.0, 100.0),
                in_tan: Vec2(-10.0, 0.0),
                out_tan: Vec2(10.0, 0.0),
            },
            PathPoint {
                p: Vec2(300.0, 400.0),
                in_tan: Vec2(0.0, 0.0),
                out_tan: Vec2(0.0, 0.0),
            },
        ];
        let (ctx, ..) = doc_with_node(path_node(points));
        assert!(PathEditOverlay.is_active(&ctx));

        let projector = painter();
        let mut painter = painter();
        PathEditOverlay.paint(&ctx, &mut painter);
        let primitives = painter.finish();
        let strokes = strokes(&primitives);
        assert_eq!(
            strokes.len(),
            3,
            "the flattened curve plus one arm per non-zero tangent"
        );
        assert_eq!(
            quads(&primitives).len(),
            4,
            "two tangent handles and two anchors"
        );

        // Every handle sits on a mark the same paint pass drew.
        let handles = PathEditOverlay.handles(&ctx);
        let drawn: Vec<(f32, f32)> = quads(&primitives)
            .into_iter()
            .map(|(bounds, _)| {
                (
                    f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5,
                    f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5,
                )
            })
            .collect();
        for handle in &handles {
            let expected = projector.to_screen(handle.position);
            assert!(
                drawn
                    .iter()
                    .any(|mark| (mark.0 - expected.0).abs() < 1e-3
                        && (mark.1 - expected.1).abs() < 1e-3),
                "handle {handle:?} has no mark under it"
            );
        }
    }

    #[test]
    fn eval_error_becomes_one_centered_label_and_paints_nothing() {
        let mut ctx = base_context();
        assert!(!EvalErrorOverlay.is_active(&ctx));

        ctx.error = Some(SharedString::from("boom"));
        assert!(EvalErrorOverlay.is_active(&ctx));
        let labels = EvalErrorOverlay.labels(&ctx);
        assert_eq!(labels.len(), 1);
        assert!(labels[0].text.ends_with("boom"));
        assert_eq!(labels[0].placement, LabelPlacement::CanvasCenter);
        assert_eq!(labels[0].color, ctx.colors.error);

        let mut painter = painter();
        EvalErrorOverlay.paint(&ctx, &mut painter);
        assert!(
            painter.finish().is_empty(),
            "text is not a canvas primitive"
        );
    }

    #[test]
    fn inactive_overlays_contribute_nothing() {
        let ctx = base_context();
        let registry = OverlayRegistry::builtin();
        assert_eq!(registry.active(&ctx).count(), 0);
        let mut painter = painter();
        registry.paint(&ctx, &mut painter);
        assert!(painter.finish().is_empty());
        assert!(registry.labels(&ctx).is_empty());
        assert!(registry.eval_targets(&ctx).is_empty());
    }

    struct ResultProbe {
        target: OverlayTarget,
    }

    impl ViewerOverlay for ResultProbe {
        fn id(&self) -> OverlayId {
            OverlayId("test.result")
        }

        fn priority(&self) -> i32 {
            0
        }

        fn is_active(&self, _ctx: &OverlayContext) -> bool {
            true
        }

        fn eval_targets(&self, _ctx: &OverlayContext) -> Vec<OverlayTarget> {
            vec![self.target.clone()]
        }

        fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
            if ctx.eval_result(&self.target).is_some() {
                painter.fill_screen_rect(painter.frame(), ctx.colors.path);
            }
        }
    }

    /// A context whose document holds one layer network containing `node`,
    /// plus the path naming that network.
    fn ctx_with_network_node(node: Node) -> (OverlayContext, NetworkPath) {
        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let graph = Graph::new().add_node(node).unwrap();
        let comp = Composition::new(comp_id, "Comp", (1920, 1080), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(layer_id, "Layer", graph).with_time(0, 0, 300));
        let mut ctx = base_context();
        ctx.document = Some(Document::default().with_composition(comp));
        (ctx, NetworkPath::layer(comp_id, layer_id))
    }

    fn scalar(value: f32) -> Arc<dyn NodeData> {
        Arc::new(ravel_core::types::Scalar(value))
    }

    /// The published snapshot for one `(network, node)` pair, keyed the way
    /// the evaluation worker tags a scoped result.
    fn results_for(
        network: &NetworkPath,
        node: NodeId,
        value: Arc<dyn NodeData>,
    ) -> OverlayResults {
        OverlayResults::new(HashMap::from([((network.segments(), node), value)]))
    }

    #[test]
    fn an_overlay_without_a_current_result_paints_nothing() {
        let node = Node::new(NodeId::next(), "test.single")
            .with_output("out", ravel_core::id::DataTypeId::GEOMETRY);
        let node_id = node.id;
        let (base, network) = ctx_with_network_node(node);
        let target = OverlayTarget {
            network,
            node: node_id,
            output: OutputPortIndex(0),
        };
        let registry = OverlayRegistry::new(vec![Box::new(ResultProbe {
            target: target.clone(),
        })]);

        let mut previous = base;
        previous.results = results_for(&target.network, node_id, scalar(1.0));
        let mut previous_painter = painter();
        registry.paint(&previous, &mut previous_painter);
        assert!(!previous_painter.finish().is_empty());

        // The snapshot is replaced wholesale, so a target that did not come
        // back has no entry at all. Another target's result is present to
        // pin that the lookup is by identity: any value will not do.
        let mut pending = previous;
        pending.results = results_for(&target.network, NodeId::new(999_001), scalar(1.0));
        let mut painter = painter();
        registry.paint(&pending, &mut painter);
        assert!(painter.finish().is_empty());
    }

    /// Evaluation is per node, so a multi-output node's result arrives as one
    /// `PortRecord`. The target's port has to select from it — handing the
    /// record over whole gives the overlay a value of the wrong type.
    #[test]
    fn a_multi_output_target_reads_its_own_port() {
        let node = Node::new(NodeId::next(), "test.multi")
            .with_output("a", ravel_core::id::DataTypeId::GEOMETRY)
            .with_output("b", ravel_core::id::DataTypeId::GEOMETRY);
        let node_id = node.id;
        let (mut ctx, network) = ctx_with_network_node(node);
        let record: Arc<dyn NodeData> = Arc::new(ravel_core::types::PortRecord(vec![
            scalar(10.0),
            scalar(20.0),
        ]));
        ctx.results = results_for(&network, node_id, record);

        let read = |port: u32| {
            ctx.eval_result(&OverlayTarget {
                network: network.clone(),
                node: node_id,
                output: OutputPortIndex(port),
            })
            .and_then(|value| {
                value
                    .downcast_ref::<ravel_core::types::Scalar>()
                    .map(|s| s.0)
            })
        };

        assert_eq!(read(0), Some(10.0));
        assert_eq!(read(1), Some(20.0));
        // A port the node does not declare has no value to draw from.
        assert_eq!(read(2), None);
    }

    /// A node that is no longer in the network cannot have its port count
    /// resolved, so there is nothing to draw — not a guess from the record.
    #[test]
    fn a_result_for_a_node_outside_the_network_is_not_readable() {
        let node = Node::new(NodeId::next(), "test.single")
            .with_output("out", ravel_core::id::DataTypeId::GEOMETRY);
        let (mut ctx, network) = ctx_with_network_node(node);
        let stranger = NodeId::new(999_002);
        ctx.results = results_for(&network, stranger, scalar(1.0));

        assert!(
            ctx.eval_result(&OverlayTarget {
                network,
                node: stranger,
                output: OutputPortIndex(0),
            })
            .is_none()
        );
    }

    /// Two layer networks routinely hold the same `NodeId`. The snapshot is
    /// keyed by scope *and* id, so one layer's result can never be drawn as
    /// another's — the failure a map keyed by id alone would produce the
    /// moment two layers are selected together.
    #[test]
    fn a_result_from_another_network_is_never_read_as_this_ones() {
        let node = Node::new(NodeId::next(), "test.single")
            .with_output("out", ravel_core::id::DataTypeId::GEOMETRY);
        let node_id = node.id;
        let (mut ctx, network) = ctx_with_network_node(node);
        let elsewhere = NetworkPath::layer(network.comp, LayerId::new(network.layer.raw() + 1));
        assert_ne!(elsewhere.segments(), network.segments());
        ctx.results = results_for(&elsewhere, node_id, scalar(1.0));

        let target = OverlayTarget {
            network,
            node: node_id,
            output: OutputPortIndex(0),
        };
        assert!(
            ctx.eval_result(&target).is_none(),
            "a value evaluated in another network was served to this one"
        );
    }

    // -----------------------------------------------------------------------
    // Unit 1: one priority-ordered hit-test path
    // -----------------------------------------------------------------------

    struct Probe {
        id: OverlayId,
        priority: i32,
        handle: u8,
        draggable: bool,
    }

    impl ViewerOverlay for Probe {
        fn id(&self) -> OverlayId {
            self.id
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        fn is_active(&self, _ctx: &OverlayContext) -> bool {
            true
        }

        fn handles(&self, _ctx: &OverlayContext) -> Vec<OverlayHandle> {
            vec![OverlayHandle {
                overlay: self.id,
                id: OverlayHandleId::Test(self.handle),
                position: (100.0, 100.0),
                hit_radius_px: 8.0,
                hint: ViewerPointerHint::PathAnchor,
                draggable: self.draggable,
            }]
        }
    }

    #[test]
    fn overlapping_handles_resolve_to_the_topmost_overlay() {
        const LOW: OverlayId = OverlayId("test.low");
        const HIGH: OverlayId = OverlayId("test.high");
        let ctx = base_context();
        // Registered low-first and high-first: the answer must not depend on
        // the order the overlays were handed to the registry.
        for overlays in [[(LOW, 1, 1u8), (HIGH, 2, 2u8)], [(HIGH, 2, 2), (LOW, 1, 1)]] {
            let registry = OverlayRegistry::new(
                overlays
                    .into_iter()
                    .map(|(id, priority, handle)| {
                        Box::new(Probe {
                            id,
                            priority,
                            handle,
                            draggable: true,
                        }) as Box<dyn ViewerOverlay>
                    })
                    .collect(),
            );
            let hit = registry.hit_test(&ctx, (100.0, 100.0), 1.0).unwrap();
            assert_eq!(hit.overlay, HIGH);
            assert_eq!(hit.id, OverlayHandleId::Test(2));
        }
    }

    #[test]
    fn a_non_draggable_handle_still_hovers_but_never_starts_a_drag() {
        let ctx = base_context();
        let registry = OverlayRegistry::new(vec![Box::new(Probe {
            id: OverlayId("test.decorative"),
            priority: 1,
            handle: 1,
            draggable: false,
        })]);
        assert!(registry.hit_test(&ctx, (100.0, 100.0), 1.0).is_some());
        assert!(
            registry
                .hit_test_draggable(&ctx, (100.0, 100.0), 1.0)
                .is_none()
        );
    }

    #[test]
    fn the_hit_radius_is_measured_in_screen_pixels() {
        let ctx = base_context();
        let registry = OverlayRegistry::new(vec![Box::new(Probe {
            id: OverlayId("test.radius"),
            priority: 1,
            handle: 1,
            draggable: true,
        })]);
        // 8px of grab at 1:1 does not reach 20 composition pixels away, but
        // the same 8px does once the view is zoomed out four times.
        assert!(registry.hit_test(&ctx, (120.0, 100.0), 1.0).is_none());
        assert!(registry.hit_test(&ctx, (120.0, 100.0), 4.0).is_some());
    }

    #[test]
    fn zero_tangents_do_not_mask_their_control_point() {
        let (ctx, ..) = doc_with_node(path_node(vec![PathPoint {
            p: Vec2(100.0, 200.0),
            in_tan: Vec2(0.0, 0.0),
            out_tan: Vec2(0.0, 0.0),
        }]));
        let handles = PathEditOverlay.handles(&ctx);
        assert_eq!(handles.len(), 1);
        assert_eq!(
            handles[0].id,
            OverlayHandleId::PathPoint {
                index: 0,
                kind: PathHandleKind::Point
            }
        );
        assert_eq!(handles[0].hint, ViewerPointerHint::PathAnchor);
    }

    #[test]
    fn a_transformed_shell_paints_path_handles_without_making_them_editable() {
        let (mut ctx, _, comp_id, layer_id) = doc_with_node(path_node(vec![PathPoint {
            p: Vec2(100.0, 200.0),
            in_tan: Vec2(0.0, 0.0),
            out_tan: Vec2(0.0, 0.0),
        }]));
        assert!(PathEditOverlay.handles(&ctx)[0].draggable);

        ctx.document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp_id,
            layer_id,
            |layer| {
                layer.transform.rotation =
                    ravel_core::animation::channel::AnimationChannel::constant(45.0);
            },
        );
        let handles = PathEditOverlay.handles(&ctx);
        assert_eq!(handles.len(), 1);
        assert!(
            !handles[0].draggable,
            "the drag writes node-local coordinates, so a rotated shell is read-only"
        );
    }

    // -----------------------------------------------------------------------
    // Unit 1: screen-space marks are zoom-independent
    // -----------------------------------------------------------------------

    #[test]
    fn screen_space_marks_keep_their_pixel_size_across_zoom() {
        let sizes = |zoom: f32| {
            let mut painter = OverlayPainter::new(
                Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(1920.0 * zoom), px(1080.0 * zoom)),
                },
                (1920, 1080),
            );
            painter.screen_square_at((100.0, 200.0), 7.0, colors().path);
            painter.stroke_comp_rect(
                CompRect {
                    x: 100.0,
                    y: 200.0,
                    w: 40.0,
                    h: 20.0,
                },
                colors().path,
            );
            let quads = quads(&painter.finish());
            (quads[0].0, quads[1].0)
        };

        let (square_1x, top_edge_1x) = sizes(1.0);
        let (square_4x, top_edge_4x) = sizes(4.0);

        assert_eq!(square_1x.size, square_4x.size, "handles do not scale");
        assert_eq!(f32::from(square_1x.size.width), 7.0);
        assert_eq!(
            f32::from(top_edge_1x.size.height),
            f32::from(top_edge_4x.size.height),
            "hairlines stay one pixel"
        );
        assert!(
            f32::from(top_edge_4x.size.width) - f32::from(top_edge_1x.size.width) > 100.0,
            "but the rectangle they outline does scale"
        );
        // Both stay anchored to the same composition point.
        assert_eq!(
            f32::from(square_1x.origin.x) + 3.5,
            f32::from(top_edge_1x.origin.x)
        );
        assert_eq!(
            f32::from(square_4x.origin.x) + 3.5,
            f32::from(top_edge_4x.origin.x)
        );
    }

    // -----------------------------------------------------------------------
    // Unit 1: OverlayEdit reaches both node parameters and shell channels
    // -----------------------------------------------------------------------

    #[test]
    fn an_overlay_edit_writes_a_node_parameter() {
        let (ctx, node, comp_id, layer_id) = doc_with_node(rect_node((100.0, 200.0)));
        let document = ctx.document.clone().unwrap();
        let edit = OverlayEdit::NodeParameter {
            network: NetworkPath::layer(comp_id, layer_id),
            node,
            key: "center".into(),
            value: ParameterValue::vec2(5.0, 6.0),
        };
        assert!(edit.target_exists(&document));
        assert_eq!(edit.invalidation(), InvalidationHint::Params(vec![node]));

        let updated = edit.apply(&document).unwrap();
        let graph =
            ravel_ui::document::resolve_network(&updated, &NetworkPath::layer(comp_id, layer_id))
                .unwrap();
        let parameter = graph
            .node(node)
            .unwrap()
            .parameters
            .iter()
            .find(|parameter| parameter.key == "center")
            .unwrap();
        assert_eq!(parameter.value, ParameterValue::vec2(5.0, 6.0));
    }

    #[test]
    fn an_overlay_edit_writes_a_layer_shell_channel() {
        let (ctx, _, comp_id, layer_id) = doc_with_node(rect_node((100.0, 200.0)));
        let document = ctx.document.clone().unwrap();
        let eval = ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));

        for (channel, value) in [
            (ShellChannel::Position(Axis::X), 12.0f32),
            (ShellChannel::Position(Axis::Y), 34.0),
            (ShellChannel::AnchorPoint(Axis::X), 5.0),
            (ShellChannel::Scale(Axis::Y), 2.0),
            (ShellChannel::Rotation, 45.0),
        ] {
            let edit = OverlayEdit::LayerTransform {
                comp: comp_id,
                layer: layer_id,
                channel,
                value,
                local_frame: None,
            };
            assert!(edit.target_exists(&document));
            assert_eq!(edit.invalidation(), InvalidationHint::None);

            let updated = edit.apply(&document).unwrap();
            let transform = &updated
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer_id)
                .unwrap()
                .transform;
            let written = match channel {
                ShellChannel::Position(axis) => &transform.position[axis.index()],
                ShellChannel::AnchorPoint(axis) => &transform.anchor_point[axis.index()],
                ShellChannel::Scale(axis) => &transform.scale[axis.index()],
                ShellChannel::Rotation => &transform.rotation,
            };
            assert_eq!(written.evaluate(0.0, &eval), value);
        }
    }

    #[test]
    fn a_shell_edit_keys_a_keyframed_channel_instead_of_flattening_it() {
        let (ctx, _, comp_id, layer_id) = doc_with_node(rect_node((100.0, 200.0)));
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, ravel_core::animation::Interpolation::Linear);
        curve.insert(10, 100.0, ravel_core::animation::Interpolation::Linear);
        let document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp_id,
            layer_id,
            |layer| {
                layer.transform.position[0] =
                    ravel_core::animation::channel::AnimationChannel::keyframes(curve.clone());
            },
        )
        .unwrap();

        let updated = OverlayEdit::LayerTransform {
            comp: comp_id,
            layer: layer_id,
            channel: ShellChannel::Position(Axis::X),
            value: 42.0,
            local_frame: Some(5),
        }
        .apply(&document)
        .unwrap();

        let channel = updated
            .get_composition(comp_id)
            .unwrap()
            .get_layer(layer_id)
            .unwrap()
            .transform
            .position[0]
            .clone();
        let ChannelSource::Keyframes(curve) = &channel.source else {
            panic!("the drag flattened an animated channel");
        };
        let eval = ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        assert_eq!(curve.keyframes().len(), 3);
        assert_eq!(channel.evaluate(5.0, &eval), 42.0);
        assert_eq!(channel.evaluate(0.0, &eval), 0.0);
    }

    #[test]
    fn an_edit_whose_target_was_deleted_neither_applies_nor_claims_to_exist() {
        let (ctx, node, comp_id, layer_id) = doc_with_node(rect_node((100.0, 200.0)));
        let document = ctx.document.clone().unwrap();
        let missing_node = OverlayEdit::NodeParameter {
            network: NetworkPath::layer(comp_id, layer_id),
            node: NodeId::next(),
            key: "center".into(),
            value: ParameterValue::vec2(5.0, 6.0),
        };
        assert!(!missing_node.target_exists(&document));
        assert!(missing_node.apply(&document).is_none());

        let missing_key = OverlayEdit::NodeParameter {
            network: NetworkPath::layer(comp_id, layer_id),
            node,
            key: "no_such_parameter".into(),
            value: ParameterValue::Float(1.0),
        };
        assert!(!missing_key.target_exists(&document));
        assert!(missing_key.apply(&document).is_none());

        let missing_layer = OverlayEdit::LayerTransform {
            comp: comp_id,
            layer: LayerId::next(),
            channel: ShellChannel::Rotation,
            value: 1.0,
            local_frame: None,
        };
        assert!(!missing_layer.target_exists(&document));
        assert!(missing_layer.apply(&document).is_none());
    }

    #[test]
    fn dragging_a_path_handle_produces_a_node_parameter_edit() {
        let (ctx, node, comp_id, layer_id) = doc_with_node(path_node(vec![
            PathPoint {
                p: Vec2(100.0, 200.0),
                in_tan: Vec2(-10.0, 0.0),
                out_tan: Vec2(10.0, 0.0),
            },
            PathPoint {
                p: Vec2(300.0, 400.0),
                in_tan: Vec2(0.0, 0.0),
                out_tan: Vec2(0.0, 0.0),
            },
        ]));
        let handles = PathEditOverlay.handles(&ctx);
        let anchor = handles
            .iter()
            .find(|handle| {
                handle.id
                    == OverlayHandleId::PathPoint {
                        index: 0,
                        kind: PathHandleKind::Point,
                    }
            })
            .unwrap();

        let edit = PathEditOverlay
            .drag(anchor, (7.0, -3.0), DragModifiers::default(), &ctx)
            .unwrap();
        let OverlayEdit::NodeParameter {
            node: edited, key, ..
        } = &edit
        else {
            panic!("a path drag writes a node parameter");
        };
        assert_eq!(*edited, node);
        assert_eq!(key.as_ref(), "points");

        let updated = edit.apply(ctx.document.as_ref().unwrap()).unwrap();
        let graph =
            ravel_ui::document::resolve_network(&updated, &NetworkPath::layer(comp_id, layer_id))
                .unwrap();
        let points = path_points(graph.node(node).unwrap()).unwrap();
        assert_eq!(points[0].p, Vec2(107.0, 197.0));
        assert_eq!(
            points[0].in_tan,
            Vec2(-10.0, 0.0),
            "moving an anchor keeps its tangent offsets"
        );
        assert_eq!(points[1].p, Vec2(300.0, 400.0));

        // Repeating the gesture from the same press context stays absolute
        // instead of compounding onto its own preview.
        let again = PathEditOverlay
            .drag(anchor, (7.0, -3.0), DragModifiers::default(), &ctx)
            .unwrap();
        assert_eq!(again, edit);
    }

    // -----------------------------------------------------------------------
    // Unit 7: the layer shell manipulator
    // -----------------------------------------------------------------------

    /// One selected layer holding a 40x20 rect centered at (100, 200), so the
    /// shell bbox is (80, 190, 40, 20) and its handle centers are round
    /// numbers.
    fn shell_context() -> (OverlayContext, CompId, LayerId) {
        let (mut ctx, _, comp_id, layer_id) = doc_with_node(rect_node((100.0, 200.0)));
        ctx.layer_selection = LayerSelection::of(comp_id, vec![layer_id]);
        (ctx, comp_id, layer_id)
    }

    fn eval() -> ravel_core::eval::EvalContext {
        ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    /// Anchor, position, scale and rotation of a layer's shell at frame 0.
    #[allow(clippy::type_complexity)]
    fn shell_values(
        document: &Document,
        comp: CompId,
        layer: LayerId,
    ) -> ((f32, f32), (f32, f32), (f32, f32), f32) {
        let eval = eval();
        let transform = &document
            .get_composition(comp)
            .unwrap()
            .get_layer(layer)
            .unwrap()
            .transform;
        let at = |channel: &ravel_core::animation::channel::AnimationChannel| {
            channel.evaluate(0.0, &eval)
        };
        (
            (
                at(&transform.anchor_point[0]),
                at(&transform.anchor_point[1]),
            ),
            (at(&transform.position[0]), at(&transform.position[1])),
            (at(&transform.scale[0]), at(&transform.scale[1])),
            at(&transform.rotation),
        )
    }

    fn shell_handle(ctx: &OverlayContext, id: ShellHandle) -> OverlayHandle {
        ShellManipulator
            .handles(ctx)
            .into_iter()
            .find(|handle| handle.id == OverlayHandleId::Shell(id))
            .unwrap_or_else(|| panic!("no {id:?} handle"))
    }

    /// Run one drag of `id` and return the document it produces.
    fn drag_shell(
        ctx: &OverlayContext,
        id: ShellHandle,
        delta: (f32, f32),
        modifiers: DragModifiers,
    ) -> Document {
        let handle = shell_handle(ctx, id);
        ShellManipulator
            .drag(&handle, delta, modifiers, ctx)
            .expect("the grip produced no edit")
            .apply(ctx.document.as_ref().unwrap())
            .expect("the edit did not apply")
    }

    fn close(actual: f32, expected: f32, what: &str) {
        close_within(actual, expected, 1e-3, what);
    }

    /// `close` with an explicit tolerance: coordinates that have been through a
    /// scaled parent chain and back carry more float error than a raw channel.
    fn close_within(actual: f32, expected: f32, tolerance: f32, what: &str) {
        assert!(
            (actual - expected).abs() < tolerance,
            "{what}: {actual} != {expected}"
        );
    }

    /// The same context reading a different document.
    fn with_document(ctx: &OverlayContext, document: Document) -> OverlayContext {
        let mut ctx = ctx.clone();
        ctx.document = Some(document);
        ctx
    }

    /// Give the fixture's layer a transform of its own, so `world` and
    /// `parent` stop being the same map. Without this a manipulator that
    /// confuses layer-local space with parent space still passes every
    /// parented test.
    fn with_own_transform(ctx: &OverlayContext, comp: CompId, layer: LayerId) -> OverlayContext {
        use ravel_core::animation::channel::AnimationChannel;

        let document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp,
            layer,
            |layer| {
                layer.transform.anchor_point = [
                    AnimationChannel::constant(10.0),
                    AnimationChannel::constant(20.0),
                ];
                layer.transform.position = [
                    AnimationChannel::constant(50.0),
                    AnimationChannel::constant(60.0),
                ];
                layer.transform.scale = [
                    AnimationChannel::constant(2.0),
                    AnimationChannel::constant(3.0),
                ];
                layer.transform.rotation = AnimationChannel::constant(30.0);
            },
        )
        .unwrap();
        with_document(ctx, document)
    }

    /// The fixture's layer parented to a fresh layer carrying `position`,
    /// `scale` and `rotation` — the knobs that decide whether the manipulator
    /// really goes through the parent chain or just happens to agree with it.
    fn parented_context(
        ctx: &OverlayContext,
        comp: CompId,
        layer: LayerId,
        position: (f32, f32),
        scale: (f32, f32),
        rotation: f32,
    ) -> OverlayContext {
        use ravel_core::animation::channel::AnimationChannel;

        let parent_id = LayerId::next();
        let mut parent = Layer::new(parent_id, "Parent", Graph::new()).with_time(0, 0, 300);
        parent.transform.position = [
            AnimationChannel::constant(position.0),
            AnimationChannel::constant(position.1),
        ];
        parent.transform.scale = [
            AnimationChannel::constant(scale.0),
            AnimationChannel::constant(scale.1),
        ];
        parent.transform.rotation = AnimationChannel::constant(rotation);
        let document =
            ravel_ui::document::add_layer(ctx.document.as_ref().unwrap(), comp, parent).unwrap();
        let document = ravel_ui::document::update_layer(&document, comp, layer, |layer| {
            layer.parent = Some(parent_id)
        })
        .unwrap();
        with_document(ctx, document)
    }

    fn world_of(document: &Document, comp: CompId, layer: LayerId) -> Affine {
        let composition = document.get_composition(comp).unwrap();
        world_matrix(composition, composition.get_layer(layer).unwrap(), &eval())
    }

    #[test]
    fn a_corner_grip_scales_the_shell_and_keeps_the_opposite_corner_still() {
        let (ctx, comp, layer) = shell_context();
        // The south-east grip (index 7) of the (80, 190, 40, 20) bbox, pulled
        // 40 out on x and 5 on y: x doubles, y grows by a quarter.
        let updated = drag_shell(
            &ctx,
            ShellHandle::Scale(7),
            (40.0, 5.0),
            DragModifiers::default(),
        );
        let (_, position, scale, _) = shell_values(&updated, comp, layer);
        close(scale.0, 2.0, "scale x");
        close(scale.1, 1.25, "scale y");

        // The grabbed corner ends under the pointer and the opposite corner
        // has not moved — which is what `position` was rewritten for.
        let world = world_of(&updated, comp, layer);
        let grabbed = world.apply(120.0, 210.0);
        close(grabbed.0, 160.0, "grabbed corner x");
        close(grabbed.1, 215.0, "grabbed corner y");
        let fixed = world.apply(80.0, 190.0);
        close(fixed.0, 80.0, "fixed corner x");
        close(fixed.1, 190.0, "fixed corner y");
        assert_ne!(position, (0.0, 0.0), "the compensation wrote position");
    }

    #[test]
    fn shift_locks_the_aspect_ratio_to_the_larger_movement() {
        let (ctx, comp, layer) = shell_context();
        let free = drag_shell(
            &ctx,
            ShellHandle::Scale(7),
            (40.0, 5.0),
            DragModifiers::default(),
        );
        let (.., free_scale, _) = shell_values(&free, comp, layer);
        assert_ne!(free_scale.0, free_scale.1, "without Shift the axes differ");

        let locked = drag_shell(
            &ctx,
            ShellHandle::Scale(7),
            (40.0, 5.0),
            DragModifiers {
                shift: true,
                alt: false,
            },
        );
        let (.., locked_scale, _) = shell_values(&locked, comp, layer);
        close(locked_scale.0, 2.0, "scale x");
        close(
            locked_scale.1,
            2.0,
            "Shift takes the larger movement (x) for both axes",
        );
    }

    #[test]
    fn an_edge_grip_scales_one_axis_only() {
        let (ctx, comp, layer) = shell_context();
        // Index 4 is the east edge midpoint: a big vertical movement must not
        // reach the y scale.
        let updated = drag_shell(
            &ctx,
            ShellHandle::Scale(4),
            (40.0, 50.0),
            DragModifiers::default(),
        );
        let (.., scale, _) = shell_values(&updated, comp, layer);
        close(scale.0, 2.0, "scale x");
        close(scale.1, 1.0, "scale y is untouched by an edge grip");
    }

    #[test]
    fn alt_scales_about_the_anchor_and_leaves_position_alone() {
        let (ctx, comp, layer) = shell_context();
        let handle = shell_handle(&ctx, ShellHandle::Scale(7));
        let edit = ShellManipulator
            .drag(
                &handle,
                (40.0, 5.0),
                DragModifiers {
                    shift: false,
                    alt: true,
                },
                &ctx,
            )
            .unwrap();
        let OverlayEdit::Batch(edits) = &edit else {
            panic!("a shell drag batches its writes");
        };
        assert_eq!(
            edits.len(),
            2,
            "the anchor is already the fixed point, so nothing corrects position"
        );

        let updated = edit.apply(ctx.document.as_ref().unwrap()).unwrap();
        let (anchor, position, scale, _) = shell_values(&updated, comp, layer);
        assert_eq!(position, (0.0, 0.0));
        // Grabbed (120, 210) relative to the anchor (0, 0) reaches (160, 215).
        close(scale.0, 160.0 / 120.0, "scale x about the anchor");
        close(scale.1, 215.0 / 210.0, "scale y about the anchor");
        assert_eq!(anchor, (0.0, 0.0), "Alt scales about the anchor, not it");
    }

    #[test]
    fn the_ring_outside_a_corner_rotates_the_shell() {
        let (ctx, comp, layer) = shell_context();
        // The grip at (120, 210) swung a quarter turn about the anchor (0, 0):
        // (x, y) -> (-y, x) is +90 degrees in the matrix's convention.
        let updated = drag_shell(
            &ctx,
            ShellHandle::Rotate(7),
            (-210.0 - 120.0, 120.0 - 210.0),
            DragModifiers::default(),
        );
        let (.., rotation) = shell_values(&updated, comp, layer);
        close(rotation, 90.0, "rotation");

        // Sign check that does not restate the formula: the layer really does
        // end up where the pointer put the grip.
        let landed = world_of(&updated, comp, layer).apply(120.0, 210.0);
        close(landed.0, -210.0, "grip x after the turn");
        close(landed.1, 120.0, "grip y after the turn");
    }

    #[test]
    fn moving_the_anchor_leaves_the_picture_where_it_was() {
        let (ctx, comp, layer) = shell_context();
        // A shell with every channel non-trivial: an uncompensated anchor move
        // would shift the content by R·S·(a' − a), which needs all three.
        let mut ctx = ctx;
        ctx.document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp,
            layer,
            |layer| {
                use ravel_core::animation::channel::AnimationChannel;
                layer.transform.anchor_point = [
                    AnimationChannel::constant(10.0),
                    AnimationChannel::constant(20.0),
                ];
                layer.transform.position = [
                    AnimationChannel::constant(50.0),
                    AnimationChannel::constant(60.0),
                ];
                layer.transform.scale = [
                    AnimationChannel::constant(2.0),
                    AnimationChannel::constant(3.0),
                ];
                layer.transform.rotation = AnimationChannel::constant(30.0);
            },
        );
        let before = world_of(ctx.document.as_ref().unwrap(), comp, layer);
        let (anchor_before, ..) = shell_values(ctx.document.as_ref().unwrap(), comp, layer);

        let updated = drag_shell(
            &ctx,
            ShellHandle::Anchor,
            (30.0, -15.0),
            DragModifiers::default(),
        );
        let (anchor_after, position_after, ..) = shell_values(&updated, comp, layer);
        assert_ne!(anchor_after, anchor_before, "the anchor did move");
        // The marker followed the pointer: it sits at the parent-space
        // position, and the parent chain here is the identity.
        close(
            position_after.0,
            50.0 + 30.0,
            "position x follows the marker",
        );
        close(
            position_after.1,
            60.0 - 15.0,
            "position y follows the marker",
        );

        let after = world_of(&updated, comp, layer);
        for (index, (a, b)) in before.0.iter().zip(after.0).enumerate() {
            assert!(
                (a - b).abs() < 1e-3,
                "the world matrix moved at {index}: {before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn a_keyframed_scale_channel_gains_a_key_instead_of_being_flattened() {
        use ravel_core::animation::Interpolation;

        let (mut ctx, comp, layer) = shell_context();
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 1.0, Interpolation::Linear);
        curve.insert(10, 4.0, Interpolation::Linear);
        ctx.document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp,
            layer,
            |layer| {
                layer.transform.scale[0] =
                    ravel_core::animation::channel::AnimationChannel::keyframes(curve.clone());
            },
        );

        let updated = drag_shell(
            &ctx,
            ShellHandle::Scale(7),
            (40.0, 5.0),
            DragModifiers::default(),
        );
        let channel = updated
            .get_composition(comp)
            .unwrap()
            .get_layer(layer)
            .unwrap()
            .transform
            .scale[0]
            .clone();
        let ChannelSource::Keyframes(curve) = &channel.source else {
            panic!("the drag flattened an animated scale channel");
        };
        assert_eq!(curve.keyframes().len(), 2, "frame 0's key was updated");
        close(
            channel.evaluate(0.0, &eval()),
            2.0,
            "the key at the playhead",
        );
        close(
            channel.evaluate(10.0, &eval()),
            4.0,
            "the later key is untouched",
        );
    }

    #[test]
    fn a_parented_layer_is_manipulated_through_its_parents_transform() {
        let (ctx, comp, layer) = shell_context();
        // A parent that both moves and scales: identity-blind code passes a
        // translation-only parent.
        let ctx = parented_context(&ctx, comp, layer, (100.0, 50.0), (2.0, 2.0), 0.0);

        // The bbox, the grips and the anchor marker all sit where the parent
        // chain puts the content: (x, y) -> (2x + 100, 2y + 50).
        let handle = shell_handle(&ctx, ShellHandle::Scale(7));
        close(handle.position.0, 2.0 * 120.0 + 100.0, "grip x");
        close(handle.position.1, 2.0 * 210.0 + 50.0, "grip y");
        let anchor = shell_handle(&ctx, ShellHandle::Anchor);
        close(anchor.position.0, 100.0, "anchor marker x");
        close(anchor.position.1, 50.0, "anchor marker y");

        // A move grip dragged 40 canvas pixels writes 20 into `position`,
        // because `position` lives in the parent's space.
        let updated = drag_shell(
            &ctx,
            ShellHandle::Position,
            (40.0, 20.0),
            DragModifiers::default(),
        );
        let (_, position, ..) = shell_values(&updated, comp, layer);
        close(position.0, 20.0, "position x is parent-space");
        close(position.1, 10.0, "position y is parent-space");
    }

    /// A rotated, non-uniformly scaled parent is where a manipulator that
    /// merely *offsets* by the parent instead of inverting it stops agreeing
    /// with the picture. The contract is stated in world coordinates: the
    /// fixed corner does not move and the grabbed corner ends under the
    /// pointer, whatever the chain does in between.
    #[test]
    fn a_corner_scale_holds_its_fixed_point_under_a_rotated_non_uniform_parent() {
        let (ctx, comp, layer) = shell_context();
        let ctx = with_own_transform(&ctx, comp, layer);
        let ctx = parented_context(&ctx, comp, layer, (70.0, -40.0), (2.0, 3.0), 30.0);
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);
        let inverse = world.inverse().unwrap();

        let grabbed = shell_handle(&ctx, ShellHandle::Scale(7)).position;
        let fixed = shell_handle(&ctx, ShellHandle::Scale(0)).position;
        let delta = (33.0, -17.0);
        let updated = drag_shell(&ctx, ShellHandle::Scale(7), delta, DragModifiers::default());
        let after = world_of(&updated, comp, layer);

        // Both corners are layer-local points; where they land afterwards is
        // the whole contract.
        let local = |point: (f32, f32)| inverse.apply(point.0, point.1);
        let landed = |point: (f32, f32)| {
            let local = local(point);
            after.apply(local.0, local.1)
        };
        let held = landed(fixed);
        close_within(held.0, fixed.0, 1e-2, "fixed corner x");
        close_within(held.1, fixed.1, 1e-2, "fixed corner y");
        let pulled = landed(grabbed);
        close_within(pulled.0, grabbed.0 + delta.0, 1e-2, "grabbed corner x");
        close_within(pulled.1, grabbed.1 + delta.1, 1e-2, "grabbed corner y");
    }

    /// Rotation under a rotated parent: the layer turns about its anchor, so
    /// the grabbed grip keeps its distance from the anchor and swings onto the
    /// pointer's bearing. Measuring the sweep in comp space instead of parent
    /// space gets the angle wrong the moment the parent is turned.
    #[test]
    fn a_rotation_under_a_rotated_parent_swings_onto_the_pointers_bearing() {
        let (ctx, comp, layer) = shell_context();
        let ctx = parented_context(&ctx, comp, layer, (10.0, 20.0), (1.5, 1.5), 40.0);
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);

        let anchor = shell_handle(&ctx, ShellHandle::Anchor).position;
        let grabbed = shell_handle(&ctx, ShellHandle::Rotate(7)).position;
        let delta = (60.0, 25.0);
        let target = (grabbed.0 + delta.0, grabbed.1 + delta.1);
        let updated = drag_shell(
            &ctx,
            ShellHandle::Rotate(7),
            delta,
            DragModifiers::default(),
        );
        let after = world_of(&updated, comp, layer);

        let local = world.inverse().unwrap().apply(grabbed.0, grabbed.1);
        let landed = after.apply(local.0, local.1);
        let arm = |point: (f32, f32)| (point.0 - anchor.0, point.1 - anchor.1);
        let (before_arm, after_arm, target_arm) = (arm(grabbed), arm(landed), arm(target));
        close_within(
            after_arm.0.hypot(after_arm.1),
            before_arm.0.hypot(before_arm.1),
            1e-2,
            "a rotation keeps the grip's distance from the anchor",
        );
        close_within(
            wrap_angle(after_arm.1.atan2(after_arm.0) - target_arm.1.atan2(target_arm.0)),
            0.0,
            1e-3,
            "the grip ends on the pointer's bearing",
        );

        // Rotation alone: the anchor marker has not moved.
        let anchor_after =
            shell_handle(&with_document(&ctx, updated), ShellHandle::Anchor).position;
        close_within(anchor_after.0, anchor.0, 1e-2, "anchor x");
        close_within(anchor_after.1, anchor.1, 1e-2, "anchor y");
    }

    /// The anchor correction has to survive a chain that rotates *and* scales
    /// unevenly: `a' = W⁻¹(pointer)` and `p' = P⁻¹(pointer)` only cancel if
    /// both inverses are the real ones.
    #[test]
    fn an_anchor_move_under_a_rotated_non_uniform_parent_leaves_the_picture_alone() {
        let (ctx, comp, layer) = shell_context();
        // The child carries its own non-trivial transform too, so the
        // correction cannot be right by symmetry.
        let ctx = with_own_transform(&ctx, comp, layer);
        let ctx = parented_context(&ctx, comp, layer, (70.0, -40.0), (2.0, 3.0), 25.0);

        let before = world_of(ctx.document.as_ref().unwrap(), comp, layer);
        let (anchor_before, ..) = shell_values(ctx.document.as_ref().unwrap(), comp, layer);
        let marker = shell_handle(&ctx, ShellHandle::Anchor).position;
        let delta = (25.0, -13.0);
        let updated = drag_shell(&ctx, ShellHandle::Anchor, delta, DragModifiers::default());

        let (anchor_after, ..) = shell_values(&updated, comp, layer);
        assert_ne!(anchor_after, anchor_before, "the anchor did move");
        let after = world_of(&updated, comp, layer);
        for (index, (a, b)) in before.0.iter().zip(after.0).enumerate() {
            close_within(
                b,
                *a,
                1e-2,
                &format!("world matrix component {index} moved: {before:?} -> {after:?}"),
            );
        }

        // And the marker went where the pointer put it.
        let marker_after =
            shell_handle(&with_document(&ctx, updated), ShellHandle::Anchor).position;
        close_within(marker_after.0, marker.0 + delta.0, 1e-2, "anchor marker x");
        close_within(marker_after.1, marker.1 + delta.1, 1e-2, "anchor marker y");
    }

    /// The rotation zone has to be visible, not just reachable: a cursor is a
    /// promise, and unit 7 only earns the `Resize*` / rotate cursors once the
    /// marks under them show what is grabbable. The drawn ring is asserted
    /// against the *handle's own* hit radius, so the picture cannot drift away
    /// from the zone that answers the pointer.
    #[test]
    fn every_rotation_grip_draws_its_ring_at_the_radius_it_grabs() {
        let (ctx, ..) = shell_context();

        // Centre and pixel extent of every closed stroke the overlay paints.
        let rings = |zoom: f32| {
            let mut painter = OverlayPainter::new(
                Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(1920.0 * zoom), px(1080.0 * zoom)),
                },
                (1920, 1080),
            );
            ShellManipulator.paint(&ctx, &mut painter);
            painter
                .finish()
                .into_iter()
                .filter_map(|primitive| match primitive {
                    OverlayPrimitive::Stroke {
                        points,
                        close: true,
                        ..
                    } => {
                        let bound = |values: Vec<f32>| {
                            let low = values.iter().copied().fold(f32::INFINITY, f32::min);
                            let high = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                            ((low + high) * 0.5, high - low)
                        };
                        let (cx, width) = bound(points.iter().map(|p| f32::from(p.x)).collect());
                        let (cy, height) = bound(points.iter().map(|p| f32::from(p.y)).collect());
                        Some(((cx, cy), (width, height)))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let rotate: Vec<OverlayHandle> = ShellManipulator
            .handles(&ctx)
            .into_iter()
            .filter(|handle| matches!(handle.id.shell(), Some(ShellHandle::Rotate(_))))
            .collect();
        assert_eq!(rotate.len(), 4, "one rotation grip per corner");

        let projector = OverlayPainter::new(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(1920.0), px(1080.0)),
            },
            (1920, 1080),
        );
        let at_1x = rings(1.0);
        assert_eq!(at_1x.len(), rotate.len(), "one ring per rotation grip");
        for handle in &rotate {
            let expected = projector.to_screen(handle.position);
            let ring = at_1x
                .iter()
                .find(|(center, _)| {
                    (center.0 - expected.0).abs() < 1e-3 && (center.1 - expected.1).abs() < 1e-3
                })
                .unwrap_or_else(|| panic!("no ring drawn on {handle:?}"));
            close(ring.1.0, handle.hit_radius_px * 2.0, "ring width");
            close(ring.1.1, handle.hit_radius_px * 2.0, "ring height");
        }

        // Zoomed four times the rings keep their pixel size, like every other
        // screen-space mark.
        let at_4x = rings(4.0);
        assert_eq!(at_4x.len(), at_1x.len());
        for (one, four) in at_1x.iter().zip(&at_4x) {
            close(four.1.0, one.1.0, "ring width across zoom");
            close(four.1.1, one.1.1, "ring height across zoom");
        }
    }

    /// `atan2` jumps by a full turn across the negative x axis. A drag that
    /// straddles it is still a small turn, and the sign has to survive.
    #[test]
    fn a_rotation_drag_across_the_angle_boundary_turns_the_short_way() {
        use ravel_core::animation::channel::AnimationChannel;

        let (mut ctx, comp, layer) = shell_context();
        // Park the anchor so the grip's arm points along −x: the branch cut of
        // `atan2` runs straight through this gesture.
        ctx.document = ravel_ui::document::update_layer(
            ctx.document.as_ref().unwrap(),
            comp,
            layer,
            |layer| {
                layer.transform.anchor_point = [
                    AnimationChannel::constant(400.0),
                    AnimationChannel::constant(209.0),
                ];
            },
        );
        let grip = shell_handle(&ctx, ShellHandle::Rotate(7)).position;
        close(grip.0, -280.0, "grip x");
        close(grip.1, 1.0, "grip y");

        // Two pixels down: a fraction of a degree the positive way round. An
        // unwrapped difference reads the same drag as about −359.6°.
        let updated = drag_shell(
            &ctx,
            ShellHandle::Rotate(7),
            (0.0, -2.0),
            DragModifiers::default(),
        );
        let (.., rotation) = shell_values(&updated, comp, layer);
        assert!(
            rotation > 0.0 && rotation < 1.0,
            "expected a fraction of a degree the short way, got {rotation}"
        );
    }

    /// `Batch` claims its writes land together. A later edit that cannot apply
    /// therefore has to discard the earlier ones rather than publish a
    /// half-transformed layer.
    #[test]
    fn a_batch_with_one_impossible_edit_applies_none_of_it() {
        let (ctx, comp, layer) = shell_context();
        let document = ctx.document.clone().unwrap();
        let before = document.clone();
        let turn = OverlayEdit::LayerTransform {
            comp,
            layer,
            channel: ShellChannel::Rotation,
            value: 45.0,
            local_frame: None,
        };
        let missing = OverlayEdit::LayerTransform {
            comp,
            layer: LayerId::next(),
            channel: ShellChannel::Scale(Axis::X),
            value: 9.0,
            local_frame: None,
        };
        assert!(turn.apply(&document).is_some(), "the good edit does apply");

        for batch in [
            OverlayEdit::Batch(vec![turn.clone(), missing.clone()]),
            OverlayEdit::Batch(vec![missing, turn]),
        ] {
            assert!(!batch.target_exists(&document));
            assert!(
                batch.apply(&document).is_none(),
                "a batch that cannot finish must not produce a document"
            );
            assert_eq!(document, before, "the source document was left untouched");
        }
    }

    #[test]
    fn the_rotation_ring_only_wins_outside_the_scale_grip() {
        let (ctx, ..) = shell_context();
        let registry = OverlayRegistry::new(vec![Box::new(ShellManipulator)]);
        let at = |point: (f32, f32)| registry.hit_test_draggable(&ctx, point, 1.0).map(|h| h.id);
        assert_eq!(
            at((120.0, 210.0)),
            Some(OverlayHandleId::Shell(ShellHandle::Scale(7))),
            "the inner disc scales"
        );
        assert_eq!(
            at((132.0, 210.0)),
            Some(OverlayHandleId::Shell(ShellHandle::Rotate(7))),
            "the ring around it rotates"
        );
        assert_eq!(at((300.0, 300.0)), None);
    }

    /// The HUD reports how far the gesture has got, not where the channel
    /// happens to sit: a layer already at 200% scaled by half must read 50%,
    /// and one already turned 30° nudged by 15° must read +15°.
    #[test]
    fn the_drag_hud_reports_the_gestures_delta_not_the_absolute_channel() {
        use ravel_core::animation::channel::AnimationChannel;

        let (mut ctx, comp, layer) = shell_context();
        assert!(
            ShellManipulator.labels(&ctx).is_empty(),
            "no HUD while the pointer is idle"
        );

        let with = |ctx: &OverlayContext, scale: (f32, f32), rotation: f32| {
            ravel_ui::document::update_layer(ctx.document.as_ref().unwrap(), comp, layer, |layer| {
                layer.transform.scale = [
                    AnimationChannel::constant(scale.0),
                    AnimationChannel::constant(scale.1),
                ];
                layer.transform.rotation = AnimationChannel::constant(rotation);
            })
            .unwrap()
        };
        let pressed = with(&ctx, (2.0, 4.0), 30.0);
        ctx.document = Some(with(&ctx, (1.0, 1.0), 45.0));
        ctx.active_drag = Some(ActiveDrag {
            handle: OverlayHandleId::Shell(ShellHandle::Rotate(0)),
            press_document: pressed.clone(),
        });

        let labels = ShellManipulator.labels(&ctx);
        assert_eq!(labels.len(), 1);
        assert_eq!(
            labels[0].text.as_ref(),
            "+15.0°",
            "the angle swept, not the angle reached"
        );
        assert_eq!(labels[0].placement, LabelPlacement::CanvasTopLeft);

        ctx.active_drag = Some(ActiveDrag {
            handle: OverlayHandleId::Shell(ShellHandle::Scale(0)),
            press_document: pressed,
        });
        assert_eq!(
            ShellManipulator.labels(&ctx)[0].text.as_ref(),
            "50.0% × 25.0%",
            "the factor this drag applied, not the scale reached"
        );
    }

    #[test]
    fn the_manipulator_needs_exactly_one_selected_layer() {
        let (ctx, comp, layer) = shell_context();
        assert!(ShellManipulator.is_active(&ctx));

        let mut none = ctx.clone();
        none.layer_selection = LayerSelection::default();
        assert!(!ShellManipulator.is_active(&none));
        assert!(ShellManipulator.handles(&none).is_empty());

        let mut two = ctx;
        two.layer_selection = LayerSelection::of(comp, vec![layer, LayerId::next()]);
        assert!(
            !ShellManipulator.is_active(&two),
            "two layers have no single shell to write"
        );
    }

    // -----------------------------------------------------------------------
    // Unit 5: the node parameter manipulator
    // -----------------------------------------------------------------------

    fn builtin_registry() -> Arc<NodeRegistry> {
        let mut registry = NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        Arc::new(registry)
    }

    /// The selected-node fixture with the templates the manipulator reads.
    fn param_context(node: Node) -> (OverlayContext, NodeId, CompId, LayerId) {
        let (mut ctx, node, comp, layer) = doc_with_node(node);
        ctx.registry = Some(builtin_registry());
        (ctx, node, comp, layer)
    }

    fn ellipse_node(center: (f32, f32), radius: (f32, f32)) -> Node {
        Node::new(NodeId::next(), "shape.ellipse")
            .with_param("center", ParameterValue::vec2(center.0, center.1))
            .with_param("radius", ParameterValue::vec2(radius.0, radius.1))
            .with_param("segments", ParameterValue::Int(32))
    }

    /// The handle the manipulator exposes for parameter `key`.
    fn param_handle(ctx: &OverlayContext, key: &str) -> OverlayHandle {
        let state = ParamState::resolve(ctx).expect("no manipulable parameter");
        let index = state
            .marks
            .iter()
            .position(|mark| mark.key == key)
            .unwrap_or_else(|| panic!("no {key} handle"));
        ParamManipulator
            .handles(ctx)
            .into_iter()
            .find(|handle| handle.id == OverlayHandleId::Param(index as u8))
            .expect("the mark has no handle")
    }

    /// The value of a vector parameter of `node`, sampled at frame 0.
    fn vec_param(document: &Document, network: &NetworkPath, node: NodeId, key: &str) -> Vec<f32> {
        let graph = ravel_ui::document::resolve_network(document, network).unwrap();
        let value = &graph
            .node(node)
            .unwrap()
            .parameters
            .iter()
            .find(|parameter| parameter.key == key)
            .unwrap()
            .value;
        let channels = match value {
            ParameterValue::Channel2(channels) => channels.to_vec(),
            ParameterValue::Channel3(channels) => channels.to_vec(),
            other => panic!("{key} is {other:?}"),
        };
        channels
            .iter()
            .map(|channel| channel.evaluate(0.0, &eval()))
            .collect()
    }

    /// Dragging the declared `center` moves the shape, and repeating the same
    /// gesture from the same press context stays absolute.
    #[test]
    fn dragging_the_declared_centre_moves_the_shape() {
        let (ctx, node, comp, layer) = param_context(rect_node((100.0, 200.0)));
        let network = NetworkPath::layer(comp, layer);
        let handle = param_handle(&ctx, "center");
        assert_eq!(handle.position, (100.0, 200.0));

        let edit = ParamManipulator
            .drag(&handle, (30.0, -10.0), DragModifiers::default(), &ctx)
            .expect("the centre handle produced no edit");
        let updated = edit.apply(ctx.document.as_ref().unwrap()).unwrap();
        assert_eq!(
            vec_param(&updated, &network, node, "center"),
            vec![130.0, 190.0]
        );

        let again = ParamManipulator
            .drag(&handle, (30.0, -10.0), DragModifiers::default(), &ctx)
            .unwrap();
        assert_eq!(again, edit, "the gesture compounded onto its own preview");
    }

    /// A `Size` role is an offset from the node's position, so its handle sits
    /// at the rim and a drag writes the radius, not the point under the
    /// pointer.
    #[test]
    fn a_size_handle_writes_the_offset_from_the_position() {
        let (ctx, node, comp, layer) = param_context(ellipse_node((100.0, 200.0), (50.0, 50.0)));
        let network = NetworkPath::layer(comp, layer);
        let handle = param_handle(&ctx, "radius");
        assert_eq!(
            handle.position,
            (150.0, 250.0),
            "the radius handle sits on the rim, not at the origin"
        );

        let updated = ParamManipulator
            .drag(&handle, (10.0, -20.0), DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            vec_param(&updated, &network, node, "radius"),
            vec![60.0, 30.0]
        );
        assert_eq!(
            vec_param(&updated, &network, node, "center"),
            vec![100.0, 200.0],
            "a size drag left the position alone"
        );
    }

    /// An animated parameter stays animated: the drag keys the layer-local
    /// frame instead of collapsing the curve, and the components it does not
    /// touch keep their own shape.
    #[test]
    fn dragging_a_keyframed_centre_keys_it_instead_of_flattening_it() {
        let (mut ctx, node, comp, layer) = param_context(rect_node((100.0, 200.0)));
        let network = NetworkPath::layer(comp, layer);
        ctx.playback = Some(PlaybackPosition {
            frame: 5,
            ..PlaybackPosition::default()
        });
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, ravel_core::animation::Interpolation::Linear);
        curve.insert(10, 100.0, ravel_core::animation::Interpolation::Linear);
        let graph = ravel_ui::document::resolve_network(ctx.document.as_ref().unwrap(), &network)
            .unwrap()
            .clone();
        let mut animated = graph.node(node).unwrap().as_ref().clone();
        animated.parameters.iter_mut().for_each(|parameter| {
            if parameter.key == "center" {
                parameter.value = ParameterValue::Channel2([
                    ravel_core::animation::channel::AnimationChannel::keyframes(curve.clone()),
                    ravel_core::animation::channel::AnimationChannel::constant(200.0),
                ]);
            }
        });
        let document = ravel_ui::document::replace_network(
            ctx.document.as_ref().unwrap(),
            &network,
            graph.replace_node(Arc::new(animated)),
        )
        .unwrap();
        ctx.document = Some(document);

        // At frame 5 the curve reads 50, so that is where the handle sits.
        let handle = param_handle(&ctx, "center");
        assert_eq!(handle.position, (50.0, 200.0));
        let updated = ParamManipulator
            .drag(&handle, (10.0, 5.0), DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();

        let graph = ravel_ui::document::resolve_network(&updated, &network).unwrap();
        let ParameterValue::Channel2(channels) = &graph
            .node(node)
            .unwrap()
            .parameters
            .iter()
            .find(|parameter| parameter.key == "center")
            .unwrap()
            .value
        else {
            panic!("the drag retyped the parameter");
        };
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("the drag flattened an animated component");
        };
        assert_eq!(curve.keyframes().len(), 3);
        assert_eq!(channels[0].evaluate(5.0, &eval()), 60.0);
        assert_eq!(
            channels[0].evaluate(0.0, &eval()),
            0.0,
            "the drag rewrote the whole curve instead of keying one frame"
        );
        assert!(
            matches!(channels[1].source, ChannelSource::Constant(205.0)),
            "the constant component stayed constant"
        );
    }

    /// A layer whose shell scales, rotates and offsets its network still shows
    /// its handles on the picture, and a drag lands the parameter where the
    /// pointer is — both directions of the same matrix.
    #[test]
    fn a_transformed_shell_keeps_the_handles_on_the_shape() {
        let (plain, node, comp, layer) = param_context(rect_node((100.0, 200.0)));
        let ctx = with_own_transform(&plain, comp, layer);
        let network = NetworkPath::layer(comp, layer);
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);

        let handle = param_handle(&ctx, "center");
        let expected = world.apply(100.0, 200.0);
        close(handle.position.0, expected.0, "handle x");
        close(handle.position.1, expected.1, "handle y");
        assert!(
            (handle.position.0 - 100.0).abs() > 1.0 || (handle.position.1 - 200.0).abs() > 1.0,
            "the shell transform was ignored: the handle stayed in node space"
        );

        // Dragging to a canvas point writes the node-space point under it.
        let delta = (40.0, -25.0);
        let updated = ParamManipulator
            .drag(&handle, delta, DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();
        let target = (handle.position.0 + delta.0, handle.position.1 + delta.1);
        let local = world.inverse().unwrap().apply(target.0, target.1);
        let written = vec_param(&updated, &network, node, "center");
        close_within(written[0], local.0, 1e-2, "written x");
        close_within(written[1], local.1, 1e-2, "written y");
        assert!(
            (written[0] - target.0).abs() > 1.0 || (written[1] - target.1).abs() > 1.0,
            "the drag wrote canvas coordinates into a node-space parameter"
        );
    }

    /// The overlay hit test runs before the drawing and navigation tools see
    /// the press, so the manipulator has to stand down unless Select is
    /// active.
    #[test]
    fn only_the_select_tool_offers_parameter_handles() {
        let (ctx, ..) = param_context(rect_node((100.0, 200.0)));
        assert!(ParamManipulator.is_active(&ctx));

        for tool in [
            ToolKind::Rect,
            ToolKind::Ellipse,
            ToolKind::Pen,
            ToolKind::Hand,
            ToolKind::Zoom,
        ] {
            let mut other = ctx.clone();
            other.tool = Some(tool);
            assert!(
                !ParamManipulator.is_active(&other),
                "{tool:?} lost its press to the parameter manipulator"
            );
        }
    }

    /// Without a role there is no handle, and without a single selected node
    /// there is no parameter set to put one on.
    #[test]
    fn the_manipulator_needs_one_selected_node_that_declares_a_role() {
        let (ctx, node, ..) = param_context(rect_node((100.0, 200.0)));
        assert!(ParamManipulator.is_active(&ctx));

        let mut two = ctx.clone();
        two.selection.as_mut().unwrap().nodes.insert(NodeId::next());
        assert!(
            !ParamManipulator.is_active(&two),
            "two nodes have no single parameter set"
        );

        let mut none = ctx.clone();
        none.selection.as_mut().unwrap().nodes.clear();
        assert!(!ParamManipulator.is_active(&none));

        // `shape.custom_path` declares no role: its points are the path
        // overlay's business.
        let (path_ctx, ..) = param_context(path_node(vec![PathPoint {
            p: Vec2(10.0, 10.0),
            in_tan: Vec2(0.0, 0.0),
            out_tan: Vec2(0.0, 0.0),
        }]));
        assert!(!ParamManipulator.is_active(&path_ctx));

        // And without the templates there is nothing to read a role from.
        let mut unregistered = ctx;
        unregistered.registry = None;
        assert!(!ParamManipulator.is_active(&unregistered));
        let _ = node;
    }

    /// A parameter fed by a connected port belongs to its source, not to the
    /// pointer: writing the stored value would change nothing on screen and
    /// still cost an undo step. The sibling parameter keeps its handle.
    #[test]
    fn a_parameter_driven_by_a_connection_gets_no_handle() {
        use ravel_core::id::{DataTypeId, EdgeId, OutputPortIndex};

        let (ctx, node, comp, layer) = param_context(ellipse_node((100.0, 200.0), (50.0, 50.0)));
        let network = NetworkPath::layer(comp, layer);
        assert_eq!(
            ParamManipulator.handles(&ctx).len(),
            2,
            "the fixture starts with a centre and a radius handle"
        );

        let source = Node::new(NodeId::next(), "constant").with_output("out", DataTypeId::VEC2);
        let source_id = source.id;
        let graph = ravel_ui::document::resolve_network(ctx.document.as_ref().unwrap(), &network)
            .unwrap()
            .clone()
            .add_node(source)
            .unwrap()
            .expose_param_port(node, "center")
            .unwrap();
        let port = graph
            .node(node)
            .unwrap()
            .param_port_index("center")
            .unwrap();
        let graph = graph
            .add_edge(EdgeId::next(), source_id, OutputPortIndex(0), node, port)
            .unwrap();
        let ctx = with_document(
            &ctx,
            ravel_ui::document::replace_network(ctx.document.as_ref().unwrap(), &network, graph)
                .unwrap(),
        );

        let state = ParamState::resolve(&ctx).expect("the radius is still manipulable");
        assert_eq!(
            state
                .marks
                .iter()
                .map(|mark| mark.key.as_str())
                .collect::<Vec<_>>(),
            vec!["radius"],
            "the connected centre still offered a handle"
        );
        assert_eq!(ParamManipulator.handles(&ctx).len(), 1);
        assert_eq!(
            painted_marks(&ctx).len(),
            1,
            "a mark was drawn where nothing can be grabbed"
        );
    }

    /// A drag stops at the parameter's declared hard boundary — the one every
    /// other editor clamps to — instead of pushing a radius through zero.
    #[test]
    fn a_drag_stops_at_the_declared_hard_range() {
        let (ctx, node, comp, layer) = param_context(ellipse_node((100.0, 200.0), (50.0, 50.0)));
        let network = NetworkPath::layer(comp, layer);
        let handle = param_handle(&ctx, "radius");

        // Far past the centre: the raw offset would be (-150, -80).
        let updated = ParamManipulator
            .drag(&handle, (-200.0, -130.0), DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            vec_param(&updated, &network, node, "radius"),
            vec![0.0, 0.0],
            "the radius went negative"
        );
    }

    /// A gesture is two-dimensional and the parameter need not be: the
    /// components the canvas cannot address keep their value.
    #[test]
    fn a_drag_leaves_the_third_component_alone() {
        use ravel_core::registry::{NodeCategory, NodeTemplate};

        let mut registry = NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        registry.register(
            NodeTemplate::new("test.vec3", "Vec3", NodeCategory::Geometry)
                .with_param(ravel_core::graph::Parameter {
                    key: "center".into(),
                    value: ParameterValue::vec3(10.0, 20.0, 30.0),
                })
                .with_param_role("center", ParamRole::Position),
        );
        let node = Node::new(NodeId::next(), "test.vec3")
            .with_param("center", ParameterValue::vec3(10.0, 20.0, 30.0));
        let (mut ctx, node, comp, layer) = doc_with_node(node);
        ctx.registry = Some(Arc::new(registry));
        let network = NetworkPath::layer(comp, layer);

        let handle = param_handle(&ctx, "center");
        assert_eq!(handle.position, (10.0, 20.0));
        let updated = ParamManipulator
            .drag(&handle, (5.0, -5.0), DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            vec_param(&updated, &network, node, "center"),
            vec![15.0, 15.0, 30.0],
            "the drag wrote a component the canvas cannot address"
        );
    }

    /// Network parameters live in layer-local time (REQ-LAYER-006): a layer
    /// placed later on the timeline, trimmed to start inside its own footage,
    /// is keyed at the frame *it* is showing, not at the playhead.
    #[test]
    fn a_drag_keys_the_layers_own_local_frame() {
        let (mut ctx, node, comp, layer) = param_context(rect_node((100.0, 200.0)));
        let network = NetworkPath::layer(comp, layer);
        // Local frame 12 while the playhead sits at 20: neither offset alone
        // reproduces it, so a drag that reuses the comp frame lands elsewhere.
        ctx.document = Some(
            ravel_ui::document::update_layer(
                ctx.document.as_ref().unwrap(),
                comp,
                layer,
                |layer| {
                    layer.start_frame = 12;
                    layer.in_frame = 4;
                },
            )
            .unwrap(),
        );
        ctx.playback = Some(PlaybackPosition {
            frame: 20,
            ..PlaybackPosition::default()
        });
        ctx.document = Some(keyframed_center(
            ctx.document.as_ref().unwrap(),
            &network,
            node,
        ));

        let handle = param_handle(&ctx, "center");
        let updated = ParamManipulator
            .drag(&handle, (7.0, 0.0), DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();

        let ChannelSource::Keyframes(curve) = &centre_channels(&updated, &network, node)[0].source
        else {
            panic!("the drag flattened an animated component");
        };
        let frames: Vec<u64> = curve.keyframes().iter().map(|key| key.frame).collect();
        assert_eq!(
            frames,
            vec![0, 12, 30],
            "the key landed on the playhead instead of the layer's own frame"
        );
    }

    /// A layer inside a rotated, non-uniformly scaled parent: the handle sits
    /// where the picture is, the value written is the node-space point under
    /// the pointer, and an animated component survives both trips.
    #[test]
    fn a_parented_layer_writes_through_the_whole_chain() {
        let (plain, node, comp, layer) = param_context(rect_node((100.0, 200.0)));
        let ctx = with_own_transform(&plain, comp, layer);
        let ctx = parented_context(&ctx, comp, layer, (70.0, -40.0), (2.0, 0.5), 40.0);
        let network = NetworkPath::layer(comp, layer);
        let ctx = with_document(
            &ctx,
            keyframed_center(ctx.document.as_ref().unwrap(), &network, node),
        );
        let ctx = {
            let mut ctx = ctx;
            ctx.playback = Some(PlaybackPosition {
                frame: 5,
                ..PlaybackPosition::default()
            });
            ctx
        };
        let world = world_of(ctx.document.as_ref().unwrap(), comp, layer);

        // The animated X reads 50 at frame 5; the handle sits on the picture.
        let handle = param_handle(&ctx, "center");
        let expected = world.apply(50.0, 200.0);
        close_within(handle.position.0, expected.0, 1e-2, "handle x");
        close_within(handle.position.1, expected.1, 1e-2, "handle y");
        let layer_only = {
            let document = ctx.document.as_ref().unwrap();
            let composition = document.get_composition(comp).unwrap();
            let layer = composition.get_layer(layer).unwrap();
            ravel_core::composition::transform::layer_matrix(layer, 5.0, &eval()).apply(50.0, 200.0)
        };
        assert!(
            (handle.position.0 - layer_only.0).abs() > 1.0
                || (handle.position.1 - layer_only.1).abs() > 1.0,
            "the parent chain made no difference: the fixture cannot see it"
        );

        let delta = (35.0, -20.0);
        let updated = ParamManipulator
            .drag(&handle, delta, DragModifiers::default(), &ctx)
            .unwrap()
            .apply(ctx.document.as_ref().unwrap())
            .unwrap();
        let target = (handle.position.0 + delta.0, handle.position.1 + delta.1);
        let local = world.inverse().unwrap().apply(target.0, target.1);
        let channels = centre_channels(&updated, &network, node);
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("the drag flattened an animated component under a parent");
        };
        assert_eq!(curve.keyframes().len(), 3);
        close_within(
            channels[0].evaluate(5.0, &eval()),
            local.0,
            1e-2,
            "written x",
        );
        close_within(
            channels[1].evaluate(5.0, &eval()),
            local.1,
            1e-2,
            "written y",
        );
        close_within(
            channels[0].evaluate(0.0, &eval()),
            0.0,
            1e-3,
            "the untouched key moved",
        );
    }

    /// `center` with a keyframed X (0 → 0, 10 → 100) and a constant Y.
    fn keyframed_center(document: &Document, network: &NetworkPath, node: NodeId) -> Document {
        use ravel_core::animation::channel::AnimationChannel;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, ravel_core::animation::Interpolation::Linear);
        curve.insert(30, 300.0, ravel_core::animation::Interpolation::Linear);
        let graph = ravel_ui::document::resolve_network(document, network)
            .unwrap()
            .clone();
        let mut animated = graph.node(node).unwrap().as_ref().clone();
        for parameter in animated.parameters.iter_mut() {
            if parameter.key == "center" {
                parameter.value = ParameterValue::Channel2([
                    AnimationChannel::keyframes(curve.clone()),
                    AnimationChannel::constant(200.0),
                ]);
            }
        }
        ravel_ui::document::replace_network(
            document,
            network,
            graph.replace_node(Arc::new(animated)),
        )
        .unwrap()
    }

    /// The two channels of a node's `center`.
    fn centre_channels(
        document: &Document,
        network: &NetworkPath,
        node: NodeId,
    ) -> [ravel_core::animation::channel::AnimationChannel; 2] {
        let graph = ravel_ui::document::resolve_network(document, network).unwrap();
        let ParameterValue::Channel2(channels) = &graph
            .node(node)
            .unwrap()
            .parameters
            .iter()
            .find(|parameter| parameter.key == "center")
            .unwrap()
            .value
        else {
            panic!("the drag retyped the parameter");
        };
        channels.clone()
    }

    /// The centres of the marks the manipulator paints, in composition space.
    fn painted_marks(ctx: &OverlayContext) -> Vec<(f32, f32)> {
        let mut painter = painter();
        ParamManipulator.paint(ctx, &mut painter);
        let frame = painter.frame();
        let zoom = painter.zoom();
        quads(&painter.finish())
            .iter()
            // The inner core of each mark is drawn 2px smaller; count outers.
            .filter(|(bounds, _)| (f32::from(bounds.size.width) - PARAM_MARK_PX).abs() < 1e-3)
            .map(|(bounds, _)| {
                (
                    (f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5
                        - f32::from(frame.origin.x))
                        / zoom.0,
                    (f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5
                        - f32::from(frame.origin.y))
                        / zoom.1,
                )
            })
            .collect()
    }

    /// Every drawn mark answers the pointer at the point it is drawn at: the
    /// paint and the handles come from the same resolved marks.
    #[test]
    fn every_parameter_mark_is_grabbable_where_it_is_drawn() {
        let (ctx, ..) = param_context(ellipse_node((100.0, 200.0), (50.0, 50.0)));
        let mut marks = painter();
        ParamManipulator.paint(&ctx, &mut marks);
        let primitives = marks.finish();
        let screen = painter();

        for handle in ParamManipulator.handles(&ctx) {
            let (screen_x, screen_y) = screen.to_screen(handle.position);
            assert!(
                quads(&primitives).iter().any(|(bounds, _)| {
                    let center_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
                    let center_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
                    (center_x - screen_x).abs() < 1e-3 && (center_y - screen_y).abs() < 1e-3
                }),
                "nothing is drawn at {:?}, but the pointer is answered there",
                handle.position
            );
        }
    }
}

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
use ravel_core::graph::{ParameterValue, PathPoint};
use ravel_core::id::{CompId, LayerId, NodeId, OutputPortIndex};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::NodeData;
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
    pub const NODE_SELECTION_BBOX: i32 = 20;
    pub const LAYER_SELECTION_BBOX: i32 = 30;
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
#[derive(Clone, Default)]
pub struct OverlayResults {
    pub(crate) values: HashMap<NodeId, Arc<dyn NodeData>>,
}

impl OverlayResults {
    pub(crate) fn new(values: HashMap<NodeId, Arc<dyn NodeData>>) -> Self {
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
    pub resolution: Option<(u32, u32)>,
    pub playback: Option<PlaybackPosition>,
    pub document: Option<Document>,
    pub selection: Option<CanvasSelection>,
    pub layer_selection: LayerSelection,
    pub tool: Option<ToolKind>,
    pub show_grid: bool,
    pub show_safe_areas: bool,
    /// The latest evaluation error message, if any.
    pub error: Option<SharedString>,
    pub colors: OverlayColors,
    /// Overlay-target results belonging to the frame currently shown.
    pub results: OverlayResults,
}

impl OverlayContext {
    /// The three pieces every document-driven overlay needs at once.
    pub fn resolved(&self) -> Option<(&Document, (u32, u32), PlaybackPosition)> {
        Some((self.document.as_ref()?, self.resolution?, self.playback?))
    }

    /// Read a target result without guessing when evaluation has not arrived.
    pub fn eval_result(&self, target: &OverlayTarget) -> Option<&Arc<dyn NodeData>> {
        self.results.values.get(&target.node)
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

/// Where a screen-space label sits. Unit 7's drag HUD and unit 8's element
/// index labels extend this with composition-anchored placements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelPlacement {
    /// Centered over the whole viewer canvas area.
    CanvasCenter,
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
    #[cfg(test)]
    Test(u8),
}

impl OverlayHandleId {
    /// The control point index and handle kind, when this is a path handle.
    pub fn path_point(self) -> Option<(usize, PathHandleKind)> {
        match self {
            Self::PathPoint { index, kind } => Some((index, kind)),
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    /// The path handle kind, for the cursor mapping during a drag.
    pub fn path_handle_kind(self) -> Option<PathHandleKind> {
        self.path_point().map(|(_, kind)| kind)
    }
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
        }
    }

    /// How much of the evaluator the edit invalidates.
    pub fn invalidation(&self) -> InvalidationHint {
        match self {
            Self::NodeParameter { node, .. } => InvalidationHint::Params(vec![*node]),
            // The shell compositing chain is recompiled from the layer, so no
            // node registration goes stale.
            Self::LayerTransform { .. } => InvalidationHint::None,
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

    /// The node output this overlay needs evaluated, if any (unit 2).
    fn eval_target(&self, _ctx: &OverlayContext) -> Option<OverlayTarget> {
        None
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
    /// composition-space offset from the press position, and `ctx` is the
    /// context captured at press time, so repeated calls during one gesture
    /// stay absolute instead of compounding.
    fn drag(
        &self,
        _handle: &OverlayHandle,
        _delta: (f32, f32),
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
    /// before the registry existed, with the selection bbox registered once per
    /// scope so the node and layer variants order independently.
    pub fn builtin() -> Self {
        Self::new(vec![
            Box::new(GridOverlay),
            Box::new(SafeAreaOverlay),
            Box::new(SelectionBboxOverlay {
                scope: BboxScope::Node,
            }),
            Box::new(SelectionBboxOverlay {
                scope: BboxScope::Layer,
            }),
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
            if let Some(target) = overlay.eval_target(ctx)
                && !targets.contains(&target)
            {
                targets.push(target);
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

/// Outlines the selection, with the eight transform handles for a node
/// selection.
///
/// The handles are still decorative: they expose no [`OverlayHandle`], because
/// no layer- or node-level scale gesture exists yet. Unit 7 gives them one and
/// makes them grabbable through the same overlay.
pub struct SelectionBboxOverlay {
    pub scope: BboxScope,
}

impl SelectionBboxOverlay {
    pub const NODE_ID: OverlayId = OverlayId("viewer.selection_bbox.node");
    pub const LAYER_ID: OverlayId = OverlayId("viewer.selection_bbox.layer");

    fn rects(&self, ctx: &OverlayContext) -> Vec<CompRect> {
        let Some((document, resolution, playback)) = ctx.resolved() else {
            return Vec::new();
        };
        match self.scope {
            BboxScope::Node => {
                let Some(selection) = ctx.selection.as_ref() else {
                    return Vec::new();
                };
                selection_comp_rects(
                    selection,
                    document,
                    playback.frame,
                    playback.fps,
                    resolution,
                )
            }
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
                layer_selection_comp_rects(
                    document,
                    comp,
                    ctx.layer_selection.layers(),
                    playback.frame,
                    playback.fps,
                    resolution,
                )
            }
        }
    }
}

impl ViewerOverlay for SelectionBboxOverlay {
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

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        !self.rects(ctx).is_empty()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        for rect in self.rects(ctx) {
            painter.stroke_comp_rect(rect, SELECTION_COLOR);
            if self.scope != BboxScope::Node {
                continue;
            }
            for center in selection_handle_centers(rect.x, rect.y, rect.w, rect.h) {
                painter.screen_square_at(center, SELECTION_HANDLE_PX, SELECTION_COLOR);
                painter.screen_square_at(
                    center,
                    SELECTION_HANDLE_PX - 2.0,
                    Hsla {
                        h: 0.0,
                        s: 0.0,
                        l: 1.0,
                        a: 1.0,
                    },
                );
            }
        }
    }
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
            show_grid: false,
            show_safe_areas: false,
            error: None,
            colors: colors(),
            results: OverlayResults::default(),
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
            .with_param("center", ParameterValue::vec2(center.0, center.1))
            .with_param("width", ParameterValue::Float(40.0))
            .with_param("height", ParameterValue::Float(20.0))
    }

    fn path_node(points: Vec<PathPoint>) -> Node {
        Node::new(NodeId::next(), "shape.custom_path")
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
        ctx.document = Some(document);
        ctx.selection = Some(CanvasSelection {
            path: Some(NetworkPath::layer(comp_id, layer_id)),
            nodes: std::collections::HashSet::from([node_id]),
        });
        (ctx, node_id, comp_id, layer_id)
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
        let overlay = SelectionBboxOverlay {
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
        ctx.document = Some(Document::default().with_composition(comp));

        let overlay = SelectionBboxOverlay {
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

        fn eval_target(&self, _ctx: &OverlayContext) -> Option<OverlayTarget> {
            Some(self.target.clone())
        }

        fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
            if ctx.eval_result(&self.target).is_some() {
                painter.fill_screen_rect(painter.frame(), ctx.colors.path);
            }
        }
    }

    #[test]
    fn an_overlay_without_a_current_result_paints_nothing() {
        let target = OverlayTarget {
            network: NetworkPath::layer(CompId::new(1), LayerId::new(1)),
            node: NodeId::new(1),
            output: OutputPortIndex(0),
        };
        let registry = OverlayRegistry::new(vec![Box::new(ResultProbe {
            target: target.clone(),
        })]);
        let mut previous = base_context();
        previous
            .results
            .values
            .insert(target.node, Arc::new(ravel_core::types::Scalar(1.0)));
        let mut previous_painter = painter();
        registry.paint(&previous, &mut previous_painter);
        assert!(!previous_painter.finish().is_empty());

        // The snapshot is replaced wholesale, so a target that did not come
        // back has no entry at all. Another target's result is present to
        // pin that the lookup is by node id: any value will not do.
        let mut pending = previous;
        pending.results = OverlayResults::new(HashMap::from([(
            NodeId::new(2),
            Arc::new(ravel_core::types::Scalar(1.0)) as Arc<dyn NodeData>,
        )]));
        let mut painter = painter();
        registry.paint(&pending, &mut painter);
        assert!(painter.finish().is_empty());
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

        let edit = PathEditOverlay.drag(anchor, (7.0, -3.0), &ctx).unwrap();
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
        let again = PathEditOverlay.drag(anchor, (7.0, -3.0), &ctx).unwrap();
        assert_eq!(again, edit);
    }
}

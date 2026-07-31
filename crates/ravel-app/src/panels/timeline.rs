// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style GPUI timeline panel: ruler, layer bars, solo/mute/lock,
//! property tree with keyframe diamonds, playhead.
//!
//! The panel displays and edits the **active composition**
//! (layer-network-model Phase 3, REQ-UI-013): every layer edit — add (menu
//! commands), delete, reorder (header drag), move/trim (bar drag),
//! solo/mute/lock, keyframe add/move/delete on the property tree (Phase 4,
//! REQ-LAYER-004) — goes through the app-wide [`ProjectState`] and lands in
//! the Document-level undo history (REQ-LAYER-009). Selecting a layer feeds
//! the Properties panel and makes its network active in the node editor
//! (REQ-LAYER-011).
//!
//! The composition mirror follows the [`super::ActiveComposition`] global and
//! the layer selection lives in [`super::LayerSelection`] — the panel keeps
//! no selection state of its own, so the Timeline and the Outliner cannot
//! drift apart (REQ-UI-013).

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, ThemeColor,
};
use ravel_core::animation::channel::ChannelSource;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::composition::Layer;
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::{FrameRate, Vec2};
use ravel_i18n::t;
use ravel_ui::document::{
    duplicate_layer as duplicate_layer_document, duplicate_layers, remove_layers, reorder_layer,
    update_layer, update_layers,
};
use ravel_ui::keyframes::{self, PropertyRow, PropertyRowId};
use ravel_ui::panels::layer_selection::{LayerClickMode, layer_selection_after_click};
use ravel_ui::panels::timeline::{
    MAX_PPF, MIN_PPF, PropertyGroup, TimelineChannelRef, TimelinePanel, TimelineViewMode,
};

use crate::assets::RavelIcon;
use crate::project_state::ProjectState;
use crate::widgets::curve_view::{self, CurveValueRange, format_value_label, value_grid_values};
// Exercised only by this module's grid tests, which reach it through
// `use super::*`.
#[cfg(test)]
use crate::widgets::curve_view::nice_value_step;
use crate::widgets::{
    CurveDrag as WidgetCurveDrag, CurveDragAxis, CurveEdit, CurveHit, CurvePoint, CurveSeries,
    CurveSource, CurveTransform, HitPart, begin_drag, curve_editor_canvas_with_x_scale,
    dominant_drag_axis, drag_to_constrained, drag_to_with_tangent_snap, hit_test_with_offsets,
    keyframes_in_rect_with_offsets,
};
use crate::workspace::{
    EditDelete, FrameStepBackward, FrameStepForward, KeyframeInterpolationBezier,
    KeyframeInterpolationLinear, KeyframeInterpolationStep, PlaybackStop, PlaybackToggle,
};
use ravel_ui::command::CommandId;

/// GPUI key context used by shortcuts local to the timeline.
pub const KEY_CONTEXT: &str = "Timeline";

const RULER_HEIGHT: f32 = 24.0;
const TRANSPORT_HEIGHT: f32 = 28.0;
const HEADER_WIDTH: f32 = 200.0;
const LAYER_ROW_HEIGHT: f32 = 28.0;
const PROPERTY_ROW_HEIGHT: f32 = 20.0;
const LAYER_BAR_CORNER_RADIUS: f32 = 4.0;
const LAYER_TEXT_PADDING: f32 = 6.0;
const PLAYHEAD_WIDTH: f32 = 2.0;
const TOGGLE_BUTTON_SIZE: f32 = 16.0;
const DIAMOND_SIZE: f32 = 8.0;
/// Bar-edge grab tolerance in pixels (trim handles).
const TRIM_HANDLE_PX: f64 = 6.0;
/// Keyframe diamond click tolerance in pixels.
const KEYFRAME_HIT_PX: f64 = 5.0;
const CURVE_DEGENERATE_MARGIN: f64 = curve_view::DEGENERATE_MARGIN;
const CURVE_HIT_RADIUS: f64 = 7.0;

#[derive(Clone, Debug)]
struct TimelineCurveData {
    channel: TimelineChannelRef,
    curve: Arc<ravel_core::animation::curve::KeyframeCurve>,
    /// Converts a layer-local key frame to a composition frame.
    frame_offset: i64,
    color: Hsla,
}

/// Zones of a layer bar a drag can grab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarZone {
    Body,
    InEdge,
    OutEdge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PointerHint {
    #[default]
    Arrow,
    Lane,
    BarBody,
    Trim,
    Locked,
    Keyframe,
    GraphAnchor,
    GraphTangent,
}

impl PointerHint {
    fn cursor(self) -> CursorStyle {
        match self {
            Self::Arrow => CursorStyle::Arrow,
            Self::Lane => CursorStyle::Crosshair,
            Self::BarBody => CursorStyle::OpenHand,
            Self::Trim => CursorStyle::ResizeLeftRight,
            Self::Locked => CursorStyle::OperationNotAllowed,
            Self::Keyframe | Self::GraphAnchor => CursorStyle::PointingHand,
            Self::GraphTangent => CursorStyle::Crosshair,
        }
    }
}

fn bar_pointer_hint(zone: Option<BarZone>, locked: bool) -> PointerHint {
    if locked && zone.is_some() {
        return PointerHint::Locked;
    }
    match zone {
        Some(BarZone::Body) => PointerHint::BarBody,
        Some(BarZone::InEdge | BarZone::OutEdge) => PointerHint::Trim,
        None => PointerHint::Lane,
    }
}

fn graph_pointer_hint(hit: Option<CurveHit>) -> PointerHint {
    match hit {
        Some(CurveHit {
            part: HitPart::Keyframe,
            ..
        }) => PointerHint::GraphAnchor,
        Some(CurveHit {
            part: HitPart::TangentIn | HitPart::TangentOut,
            ..
        }) => PointerHint::GraphTangent,
        None => PointerHint::Lane,
    }
}

/// The layer-area row under a content-space y position.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowHit {
    /// A layer bar row.
    LayerBar(LayerId),
    /// A property group row of the layer's property tree.
    PropertyGroup(LayerId, PropertyRowId),
    /// A channel sub-row (the usize is the row's component index).
    Channel(LayerId, PropertyRowId, usize),
}

/// Stable identity of one keyframe diamond in the timeline property tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct KeyframeRef {
    layer: LayerId,
    row: PropertyRowId,
    component: usize,
    frame: u64,
}

#[derive(Clone, Debug)]
struct KeyframeChannelBaseline {
    layer: LayerId,
    row: PropertyRowId,
    component: usize,
    curve: ravel_core::animation::curve::KeyframeCurve,
    origin_frames: Vec<u64>,
}

/// Active drag gesture over the layer area / headers. Live updates go
/// through `ProjectState::apply_document`; the ending mouse-up records one
/// Document undo step for the whole gesture.
#[derive(Clone, Debug)]
enum TimelineDrag {
    None,
    /// Scrub the playhead: after a ruler mousedown the pointer may leave the
    /// ruler and the scrub keeps tracking (same "drag anywhere after
    /// mousedown" contract as `widgets/scrub_input.rs`). No document edits,
    /// so ending or cancelling commits nothing.
    Scrub,
    /// Move the bar along the timeline (start_frame).
    MoveBar {
        layer: LayerId,
        origin_start: i64,
        grab_x: f32,
        changed: bool,
    },
    /// Trim the display interval's in edge (start and in move together, the
    /// out edge stays fixed).
    TrimIn {
        layer: LayerId,
        origin_start: i64,
        origin_in: u64,
        origin_out: u64,
        grab_x: f32,
        changed: bool,
    },
    /// Trim the display interval's out edge.
    TrimOut {
        layer: LayerId,
        origin_in: u64,
        origin_out: u64,
        grab_x: f32,
        changed: bool,
    },
    /// Reorder the layer in the stack (header vertical drag).
    Reorder {
        layer: LayerId,
        changed: bool,
    },
    /// Move selected keyframes along the timeline (layer-local frames).
    MoveKeyframe {
        baselines: Vec<KeyframeChannelBaseline>,
        origin_selection: HashSet<KeyframeRef>,
        pressed: KeyframeRef,
        collapse_on_click: bool,
        current_delta: i64,
        grab_x: f32,
        changed: bool,
    },
    /// Move the selected graph keyframes in frame/value space.
    GraphKeyframes {
        baselines: Vec<KeyframeChannelBaseline>,
        origin_selection: HashSet<KeyframeRef>,
        drag: WidgetCurveDrag,
        transform: CurveTransform,
        graph_origin: (f32, f32),
        pressed_value: f32,
        current_frame_delta: i64,
        current_value_delta: f32,
        changed: bool,
    },
    /// Edit the same Bezier handle across selected immutable baselines.
    GraphTangent {
        baselines: Vec<KeyframeChannelBaseline>,
        drag: WidgetCurveDrag,
        transform: CurveTransform,
        graph_origin: (f32, f32),
        pressed_tangent: Vec2,
        current_delta: Vec2,
        current_coupling: keyframes::TangentCoupling,
        changed: bool,
    },
    /// Select graph keyframe anchors inside a widget-space rectangle.
    GraphRubberBand {
        curves: Vec<TimelineCurveData>,
        transform: CurveTransform,
        graph_origin: (f32, f32),
        start: CurvePoint,
        current: CurvePoint,
        initial_selection: HashSet<KeyframeRef>,
        additive: bool,
        moved: bool,
    },
    /// Select keyframes whose diamond centers fall inside an area-local
    /// rectangle. The starting selection is retained only for Shift-add.
    RubberBand {
        start: (f32, f32),
        current: (f32, f32),
        initial_selection: HashSet<KeyframeRef>,
        additive: bool,
        moved: bool,
    },
}

fn pointer_hint_transition(
    current: PointerHint,
    next: PointerHint,
    dragging: bool,
) -> Option<PointerHint> {
    (!dragging && current != next).then_some(next)
}

fn drag_cursor(drag: &TimelineDrag) -> Option<CursorStyle> {
    match drag {
        TimelineDrag::None => None,
        TimelineDrag::Scrub | TimelineDrag::TrimIn { .. } | TimelineDrag::TrimOut { .. } => {
            Some(CursorStyle::ResizeLeftRight)
        }
        TimelineDrag::MoveBar { .. }
        | TimelineDrag::MoveKeyframe { .. }
        | TimelineDrag::GraphKeyframes { .. } => Some(CursorStyle::ClosedHand),
        TimelineDrag::Reorder { .. } => Some(CursorStyle::ResizeUpDown),
        TimelineDrag::GraphTangent { .. }
        | TimelineDrag::GraphRubberBand { .. }
        | TimelineDrag::RubberBand { .. } => Some(CursorStyle::Crosshair),
    }
}

pub struct TimelineGpuiPanel {
    state: TimelinePanel,
    project: Option<Entity<ProjectState>>,
    audio: Option<Entity<crate::audio::AudioService>>,
    drag: TimelineDrag,
    pointer_hint: PointerHint,
    /// Selected keyframe diamonds. Panel-local state; document sync retains
    /// every live identity and drops only refs whose diamonds disappeared.
    selected_keyframes: HashSet<KeyframeRef>,
    /// Whether the graph view paints time/value grid lines and value labels.
    show_curve_grid: bool,
    /// Visible value range of the graph editor, shared with the Properties
    /// curve editor (`widgets::curve_view`). View state: never in the
    /// Document, so it is outside undo.
    curve_value_range: CurveValueRange,
    /// Last painted width of the ruler/layer area (pixels), captured during
    /// prepaint so follow-playhead scrolling knows the visible range.
    ruler_width: Rc<Cell<f32>>,
    /// Origin x of the ruler area, captured during prepaint so a scrub drag
    /// can map window coordinates to frames from anywhere in the panel.
    ruler_origin_x: Rc<Cell<f32>>,
    /// Origin of the layer bar area, captured during prepaint for
    /// bar hit-testing in panel coordinates.
    area_origin: Rc<Cell<(f32, f32)>>,
    /// Last context-menu invocation in layer-area coordinates. Header
    /// clicks have a negative x but share the layer area's content y.
    last_right_click: Rc<Cell<(f32, f32)>>,
    /// Transient frame editor shown after an explicit timecode click.
    timecode_input: Option<Entity<InputState>>,
    timecode_input_sub: Option<Subscription>,
    /// Normalized logarithmic pixels-per-frame control.
    zoom_slider: Entity<SliderState>,
    #[allow(dead_code)]
    zoom_slider_sub: Subscription,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    /// Keeps [`super::TimelinePanelHandle`] pointing at this instance while it
    /// holds the focus.
    #[allow(dead_code)]
    handle_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    #[allow(dead_code)]
    audio_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    active_comp_sub: Subscription,
    #[allow(dead_code)]
    selection_sub: Subscription,
}

impl TimelineGpuiPanel {
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
                // `ProjectState` also notifies for things this panel does not
                // mirror (a completed save moves the window title). Comparing
                // the mirror epoch keeps the `Composition` deep compare and the
                // repaint off those notifications; the composition-switch
                // observer below calls `sync_from_project` on its own path, so
                // the gate belongs here and not inside it.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.sync_from_project(cx);
            })
        });
        let audio = cx
            .try_global::<crate::audio::AudioServiceHandle>()
            .and_then(|handle| handle.0.upgrade());
        let audio_sub = audio.as_ref().map(|audio| {
            cx.observe(audio, |_this: &mut Self, _audio, cx| {
                cx.notify();
            })
        });

        let mut state = TimelinePanel::new(FrameRate::new(30, 1));
        if let Some(project) = &project {
            let comp = super::active_composition_in(project.read(cx).document(), cx).cloned();
            state.set_composition(comp);
        }

        // A composition switch replaces everything this panel shows; the
        // selection global is written by the Outliner as well as by this
        // panel, so the row highlighting has to repaint from it.
        let active_comp_sub = cx.observe_global::<super::ActiveComposition>(|this, cx| {
            this.sync_from_project(cx);
        });
        let selection_sub = cx.observe_global::<super::LayerSelection>(|_this, cx| {
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);
        let zoom_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(ppf_to_slider(state.pixels_per_frame()))
        });
        let zoom_slider_sub = cx.subscribe(
            &zoom_slider,
            |this: &mut Self, _slider, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.state
                        .set_pixels_per_frame(slider_to_ppf(value.start()));
                    cx.notify();
                }
            },
        );
        // The playback controller drives one Timeline: the instance that was
        // built last, and from then on the one the user focuses.
        cx.set_global(super::TimelinePanelHandle(cx.entity().downgrade()));
        let handle_sub =
            super::track_focused_handle(&focus_handle, window, cx, super::TimelinePanelHandle);
        Self {
            state,
            project,
            audio,
            drag: TimelineDrag::None,
            pointer_hint: PointerHint::default(),
            selected_keyframes: HashSet::new(),
            show_curve_grid: true,
            curve_value_range: CurveValueRange::auto(),
            ruler_width: Rc::new(Cell::new(0.0)),
            ruler_origin_x: Rc::new(Cell::new(0.0)),
            area_origin: Rc::new(Cell::new((0.0, 0.0))),
            last_right_click: Rc::new(Cell::new((0.0, 0.0))),
            timecode_input: None,
            timecode_input_sub: None,
            zoom_slider,
            zoom_slider_sub,
            focus_handle,
            focus_subscriptions,
            handle_sub,
            project_sub,
            audio_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            active_comp_sub,
            selection_sub,
        }
    }

    // ----- document sync -----------------------------------------------------

    fn sync_from_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        // The mirror follows the active composition, not the document root:
        // `None` (composition 0, or an active id this document does not
        // have) empties the panel instead of leaving a stale composition on
        // screen.
        let comp = super::active_composition_in(project.read(cx).document(), cx).cloned();
        if self.state.composition() != comp.as_ref() {
            let old_comp_id = self.state.comp_id();
            let new_comp_id = comp.as_ref().map(|comp| comp.id);
            self.state.set_composition(comp);
            // Drop a keyframe selection whose diamond disappeared (undo or
            // an external edit) — a stale selection would hijack Delete.
            self.selected_keyframes.retain(|keyframe| {
                self.state.layer(keyframe.layer).is_some_and(|layer| {
                    keyframes::has_keyframe_at(
                        layer,
                        &keyframe.row,
                        keyframe.component,
                        keyframe.frame,
                    )
                })
            });
            // Deselect a deleted layer. A changed composition id also
            // clears: a same-numbered LayerId in the new composition is an
            // unrelated layer. Clearing the selection drops a Properties
            // target that pointed at it (`set_layer_selection`); a node
            // target is never stolen. Value freshness itself needs no
            // republish — the Properties panel resolves from the document.
            //
            // The node editor needs no push here: it observes
            // `LayerSelection`, and both a composition switch and the clear
            // below write it (a switch closes the network even with nothing
            // selected, so the Viewer tools and `CanvasSelection` cannot stay
            // pointed at a composition the UI no longer shows).
            // Layers that vanished from the document leave the selection in
            // `ProjectState::document_changed` (it owns that for every
            // workspace); what is left here is the composition switch, where a
            // same-numbered `LayerId` in the new composition is an unrelated
            // layer.
            if !super::layer_selection(cx).is_empty() && new_comp_id != old_comp_id {
                super::clear_layer_selection(cx);
            }
        }
        cx.notify();
    }

    /// The selected layer this single-selection panel follows
    /// (`LayerSelection` is the shared source of truth, REQ-UI-013).
    fn selected_layer(&self, cx: &App) -> Option<LayerId> {
        super::selected_layer(cx)
    }

    /// Publish the layer selection to the Properties panel. Only identities are
    /// published; the panel resolves current values from the document itself.
    fn publish_selected_layer_target(&mut self, cx: &mut Context<Self>) {
        let Some(lid) = self.selected_layer(cx) else {
            return;
        };
        if self.state.layer(lid).is_none() {
            return;
        }
        super::publish_layer_properties_target(cx);
    }

    /// Select a layer (plain click). The node editor opens its network by
    /// observing `LayerSelection` — this panel is one of two writers of that
    /// selection (REQ-UI-013) and pushes at no one.
    pub(crate) fn select_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.select_layer_with_mode(lid, LayerClickMode::Replace, cx);
    }

    /// Select a layer, extending the current selection when a modifier asks for
    /// it: Shift ranges over the stack, the platform modifier toggles
    /// (REQ-UI-013). The arithmetic is headless and shared with the Outliner,
    /// so both panels agree on what a modified click means.
    pub(crate) fn select_layer_with_mode(
        &mut self,
        lid: LayerId,
        mode: LayerClickMode,
        cx: &mut Context<Self>,
    ) {
        let order: Vec<LayerId> = self.state.layers().map(|layer| layer.id).collect();
        let selection = super::layer_selection(cx);
        let layers = layer_selection_after_click(selection.layers(), &order, lid, mode);
        super::set_layer_selection(layers, cx);
        self.publish_selected_layer_target(cx);
        cx.notify();
    }

    /// Select a layer for an operation aimed at the row under the cursor (right
    /// click): an existing selection that already holds the layer is kept, so a
    /// context menu opened on one of several selected layers does not silently
    /// throw the rest of the selection away.
    fn select_layer_for_menu(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        if super::layer_selection(cx).contains(lid) {
            // The selection stands, but the right click still points Properties
            // at it (a right click has always done that).
            self.publish_selected_layer_target(cx);
            cx.notify();
            return;
        }
        self.select_layer(lid, cx);
    }

    /// Clear the layer (and keyframe) selection — empty-area click. A
    /// Properties target showing the deselected layer goes with it; a
    /// node-properties view is never stolen.
    fn deselect_layer(&mut self, cx: &mut Context<Self>) {
        self.selected_keyframes.clear();
        if self.selected_layer(cx).is_none() {
            cx.notify();
            return;
        }
        super::clear_layer_selection(cx);
        cx.notify();
    }

    /// Apply `f` to a layer in the document. `commit` records one undo step.
    fn edit_layer(
        &mut self,
        lid: LayerId,
        hint: InvalidationHint,
        commit: bool,
        f: impl FnOnce(&mut Layer),
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        project.update(cx, |project, cx| {
            let Some(doc) = update_layer(project.document(), comp_id, lid, f) else {
                return;
            };
            if commit {
                project.commit_document(doc, hint, cx);
            } else {
                project.apply_document(doc, hint, cx);
            }
        });
    }

    /// The layers an operation on the row `lid` applies to: the whole selection
    /// when the row is part of it, otherwise just that row (REQ-UI-013 bulk
    /// editing). This is the same rule the Outliner uses, so a toggle or a
    /// delete aimed at one row of a multi-selection is one gesture over the
    /// selection instead of a silent single-row edit.
    fn operation_targets(&self, lid: LayerId, cx: &App) -> Vec<LayerId> {
        let selection = super::layer_selection(cx);
        if selection.contains(lid) {
            selection.layers().to_vec()
        } else {
            vec![lid]
        }
    }

    /// Flip a boolean shell flag on every operation target as one undo step.
    /// The clicked row decides the new value, so a mixed selection ends up
    /// uniform instead of each layer flipping its own way.
    fn toggle_layer_flag(
        &mut self,
        lid: LayerId,
        hint: InvalidationHint,
        read: impl Fn(&Layer) -> bool,
        write: impl Fn(&mut Layer, bool),
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        let targets = self.operation_targets(lid, cx);
        project.update(cx, |project, cx| {
            let Some(clicked) = project
                .document()
                .get_composition(comp_id)
                .and_then(|comp| comp.get_layer(lid))
            else {
                return;
            };
            let value = !read(clicked);
            let Some(doc) = update_layers(project.document(), comp_id, &targets, |layer| {
                write(layer, value)
            }) else {
                return;
            };
            project.commit_document(doc, hint, cx);
        });
    }

    fn toggle_solo(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        // Solo/mute change the compiled merge chain (REQ-LAYER-007).
        self.toggle_layer_flag(
            lid,
            InvalidationHint::Structural,
            |l| l.solo,
            |l, value| l.solo = value,
            cx,
        );
    }

    fn toggle_mute(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.toggle_layer_flag(
            lid,
            InvalidationHint::Structural,
            |l| l.muted,
            |l, value| l.muted = value,
            cx,
        );
    }

    fn toggle_lock(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.toggle_layer_flag(
            lid,
            InvalidationHint::None,
            |l| l.locked,
            |l, value| l.locked = value,
            cx,
        );
    }

    /// Duplicate a layer directly above its source and select the copy.
    /// The graph and shell bindings receive fresh globally unique ids in
    /// the headless document helper.
    fn duplicate_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) -> Option<LayerId> {
        let project = self.project.clone()?;
        let comp_id = self.state.comp_id()?;
        let mut duplicated = None;
        project.update(cx, |project, cx| {
            let source_index = project
                .document()
                .get_composition(comp_id)?
                .layers
                .iter()
                .position(|layer| layer.id == lid)?;
            let doc = duplicate_layer_document(project.document(), comp_id, lid)?;
            duplicated = doc
                .get_composition(comp_id)
                .and_then(|composition| composition.layers.get(source_index + 1))
                .map(|layer| layer.id);
            project.commit_document(doc, InvalidationHint::Structural, cx);
            Some(())
        });
        if let Some(new_layer) = duplicated {
            self.selected_keyframes.clear();
            self.select_layer(new_layer, cx);
        }
        duplicated
    }

    /// Duplicate the operation targets of the row `lid` — the whole selection
    /// when the row is part of it — as one undo step, and select the copies.
    fn duplicate_layers_from_row(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        let targets = self.operation_targets(lid, cx);
        if targets.len() < 2 {
            self.duplicate_layer(lid, cx);
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        let copies = project.update(cx, |project, cx| {
            let (doc, copies) = duplicate_layers(project.document(), comp_id, &targets)?;
            project.commit_document(doc, InvalidationHint::Structural, cx);
            Some(copies)
        });
        if let Some(copies) = copies
            && !copies.is_empty()
        {
            self.selected_keyframes.clear();
            super::set_layer_selection(copies, cx);
            self.publish_selected_layer_target(cx);
            cx.notify();
        }
    }

    /// Delete the operation targets of the row `lid` — the whole selection when
    /// the row is part of it — as one undo step (REQ-LAYER-009). Locked layers
    /// are protected and stay selected; the lock is checked against the document
    /// (the panel mirror may lag one observer flush). Returns whether anything
    /// was deleted.
    fn delete_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) -> bool {
        let Some(project) = self.project.clone() else {
            return false;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return false;
        };
        let targets = self.operation_targets(lid, cx);
        let deleted = project.update(cx, |project, cx| {
            match remove_layers(project.document(), comp_id, &targets) {
                Some(doc) => {
                    project.commit_document(doc, InvalidationHint::Structural, cx);
                    true
                }
                None => false,
            }
        });
        if !deleted {
            return false;
        }
        // The deleted layers leave the selection; a locked layer that survived
        // the delete stays selected, so the user can see what was kept.
        let remaining: Vec<LayerId> = super::layer_selection(cx)
            .layers()
            .iter()
            .copied()
            .filter(|id| self.layer_exists(comp_id, *id, cx))
            .collect();
        if remaining.is_empty() {
            self.deselect_layer(cx);
        } else if remaining.len() != super::layer_selection(cx).layers().len() {
            super::set_layer_selection(remaining, cx);
            self.publish_selected_layer_target(cx);
            cx.notify();
        }
        true
    }

    /// Whether the document still holds `lid` in `comp_id`.
    fn layer_exists(&self, comp_id: CompId, lid: LayerId, cx: &App) -> bool {
        self.project.as_ref().is_some_and(|project| {
            project
                .read(cx)
                .document()
                .get_composition(comp_id)
                .is_some_and(|comp| comp.get_layer(lid).is_some())
        })
    }

    /// Delete the whole layer selection (each owned network goes with its
    /// layer, REQ-LAYER-009) as one undo step. Locked layers are protected.
    fn delete_selected_layers(&mut self, cx: &mut Context<Self>) {
        let Some(lid) = self.selected_layer(cx) else {
            return;
        };
        self.delete_layer(lid, cx);
    }

    /// Remove all selected keyframes as one Document undo step. Locked-layer
    /// refs stay selected; deleted and stale refs are dropped.
    fn delete_selected_keyframes(&mut self, cx: &mut Context<Self>) {
        if self.selected_keyframes.is_empty() {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        let selection = self.selected_keyframes.clone();
        let mut retained = HashSet::new();
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            let mut removed_any = false;
            for keyframe in selection {
                let Some(layer) = doc
                    .get_composition(comp_id)
                    .and_then(|composition| composition.get_layer(keyframe.layer))
                else {
                    continue;
                };
                if !keyframes::has_keyframe_at(
                    layer,
                    &keyframe.row,
                    keyframe.component,
                    keyframe.frame,
                ) {
                    continue;
                }
                if layer.locked {
                    retained.insert(keyframe);
                    continue;
                }
                let mut removed = false;
                if let Some(updated) = update_layer(&doc, comp_id, keyframe.layer, |layer| {
                    removed = keyframes::remove_keyframe(
                        layer,
                        &keyframe.row,
                        keyframe.component,
                        keyframe.frame,
                    );
                }) {
                    doc = updated;
                    removed_any |= removed;
                }
            }
            if removed_any {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
        self.selected_keyframes = retained;
        cx.notify();
    }

    fn delete_keyframe_from_menu(&mut self, clicked: KeyframeRef, cx: &mut Context<Self>) {
        if !self.selected_keyframes.contains(&clicked) {
            self.selected_keyframes.clear();
            self.selected_keyframes.insert(clicked);
        }
        self.delete_selected_keyframes(cx);
    }

    fn selected_interpolation(&self) -> Option<Interpolation> {
        let mut selected = self.selected_keyframes.iter();
        let first = selected.next()?;
        let interpolation = self.keyframe_interpolation(first)?;
        selected
            .all(|keyframe| self.keyframe_interpolation(keyframe) == Some(interpolation))
            .then_some(interpolation)
    }

    fn keyframe_interpolation(&self, keyframe: &KeyframeRef) -> Option<Interpolation> {
        let layer = self.state.layer(keyframe.layer)?;
        let channels = keyframes::row_channels(layer, &keyframe.row)?;
        let channel = channels.get(keyframe.component)?;
        let ChannelSource::Keyframes(curve) = &channel.source else {
            return None;
        };
        curve
            .keyframes()
            .iter()
            .find(|candidate| candidate.frame == keyframe.frame)
            .map(|candidate| candidate.interpolation)
    }

    /// Apply one interpolation mode to the graph/diamond selection as one
    /// Document undo step. Locked and stale references are ignored.
    fn set_selected_keyframe_interpolation(
        &mut self,
        interpolation: Interpolation,
        cx: &mut Context<Self>,
    ) {
        if self.selected_keyframes.is_empty() {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        let selection = self.selected_keyframes.clone();
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            let mut changed = false;
            for keyframe in selection {
                let Some(layer) = doc
                    .get_composition(comp_id)
                    .and_then(|composition| composition.get_layer(keyframe.layer))
                else {
                    continue;
                };
                if layer.locked {
                    continue;
                }
                let current = keyframes::row_channels(layer, &keyframe.row)
                    .and_then(|channels| channels.get(keyframe.component).copied())
                    .and_then(|channel| match &channel.source {
                        ChannelSource::Keyframes(curve) => curve
                            .keyframes()
                            .iter()
                            .find(|candidate| candidate.frame == keyframe.frame)
                            .map(|candidate| candidate.interpolation),
                        _ => None,
                    });
                if current.is_none() || current == Some(interpolation) {
                    continue;
                }
                let mut updated_key = false;
                if let Some(updated) = update_layer(&doc, comp_id, keyframe.layer, |layer| {
                    updated_key = keyframes::set_keyframe_interpolation(
                        layer,
                        &keyframe.row,
                        keyframe.component,
                        keyframe.frame,
                        interpolation,
                    );
                }) && updated_key
                {
                    doc = updated;
                    changed = true;
                }
            }
            if changed {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
        cx.notify();
    }

    fn on_keyframe_bezier(
        &mut self,
        _: &KeyframeInterpolationBezier,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_selected_keyframe_interpolation(Interpolation::Bezier, cx);
    }

    fn on_keyframe_linear(
        &mut self,
        _: &KeyframeInterpolationLinear,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_selected_keyframe_interpolation(Interpolation::Linear, cx);
    }

    fn on_keyframe_step(
        &mut self,
        _: &KeyframeInterpolationStep,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_selected_keyframe_interpolation(Interpolation::Step, cx);
    }

    fn select_graph_hit(
        &mut self,
        curves: &[TimelineCurveData],
        hit: CurveHit,
        additive: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(curve) = curves.get(hit.curve) else {
            return;
        };
        let selected = KeyframeRef {
            layer: curve.channel.layer,
            row: curve.channel.row.clone(),
            component: curve.channel.component,
            frame: hit.frame,
        };
        if additive {
            if !self.selected_keyframes.insert(selected.clone()) {
                self.selected_keyframes.remove(&selected);
            }
        } else if !self.selected_keyframes.contains(&selected) {
            self.selected_keyframes.clear();
            self.selected_keyframes.insert(selected);
        }
        cx.notify();
    }

    fn select_all_displayed_keyframes(&mut self, cx: &mut Context<Self>) {
        let mut selected = HashSet::new();
        for channel in self.state.selected_channels() {
            let Some(layer) = self.state.layer(channel.layer) else {
                continue;
            };
            let Some(channels) = keyframes::row_channels(layer, &channel.row) else {
                continue;
            };
            let Some(channel_value) = channels.get(channel.component) else {
                continue;
            };
            let ChannelSource::Keyframes(curve) = &channel_value.source else {
                continue;
            };
            selected.extend(curve.keyframes().iter().map(|keyframe| KeyframeRef {
                layer: channel.layer,
                row: channel.row.clone(),
                component: channel.component,
                frame: keyframe.frame,
            }));
        }
        self.selected_keyframes = selected;
        cx.notify();
    }

    /// Fit the value axis back onto the data. Unpinning the shared range is
    /// the whole operation: the automatic bounds are derived from every
    /// keyframe, so nothing can stay out of view.
    fn fit_curve_values(&mut self, cx: &mut Context<Self>) {
        self.curve_value_range.fit();
        cx.notify();
    }

    fn toggle_curve_grid(&mut self, cx: &mut Context<Self>) {
        self.show_curve_grid = !self.show_curve_grid;
        cx.notify();
    }

    fn add_layer_from_template(&mut self, template_key: &str, cx: &mut Context<Self>) {
        if let Some(project) = self.project.clone() {
            let layer = project.update(cx, |project, cx| {
                project.add_layer_from_template(template_key, cx)
            });
            if let Some(layer) = layer {
                self.selected_keyframes.clear();
                self.select_layer(layer, cx);
            }
        }
    }

    fn on_delete(&mut self, _: &EditDelete, _window: &mut Window, cx: &mut Context<Self>) {
        // A selected keyframe scopes Delete to that keyframe; otherwise the
        // selected layer is deleted as before.
        let outcome = if !self.selected_keyframes.is_empty() {
            self.delete_selected_keyframes(cx);
            "delete_selected_keyframes"
        } else {
            self.delete_selected_layers(cx);
            "delete_selected_layers"
        };
        let focused_instance = crate::trace::focused_instance(cx);
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::PanelKeyDown,
                command: Some(CommandId::EditDelete),
                focused_instance,
                handler: "TimelineGpuiPanel::on_delete",
                outcome: Some(outcome.to_string()),
            },
        );
        cx.notify();
    }

    // ----- bar drags -----------------------------------------------------------

    /// The layer row and bar zone under an area-local position.
    fn bar_hit(&self, content_x: f64, content_y: f32) -> Option<(LayerId, BarZone)> {
        Self::bar_hit_in(&self.state, content_x, content_y)
    }

    fn bar_hit_in(
        state: &TimelinePanel,
        content_x: f64,
        content_y: f32,
    ) -> Option<(LayerId, BarZone)> {
        let lid = match Self::row_at_content_y_in(state, content_y) {
            Some(RowHit::LayerBar(lid)) => lid,
            _ => return None,
        };
        let layer = state.layer(lid)?;
        let ppf = state.pixels_per_frame();
        let scroll = state.scroll_offset();
        let x0 = (layer.start_frame as f64 - scroll) * ppf;
        let x1 = x0 + layer.duration() as f64 * ppf;
        if (content_x - x0).abs() <= TRIM_HANDLE_PX {
            Some((lid, BarZone::InEdge))
        } else if (content_x - x1).abs() <= TRIM_HANDLE_PX {
            Some((lid, BarZone::OutEdge))
        } else if content_x > x0 && content_x < x1 {
            Some((lid, BarZone::Body))
        } else {
            None
        }
    }

    fn pointer_hint_at(&self, content_x: f64, content_y: f32) -> PointerHint {
        match self.row_at_content_y(content_y) {
            Some(RowHit::LayerBar(layer_id)) => {
                let zone = self.bar_hit(content_x, content_y).map(|(_, zone)| zone);
                let locked = self.state.layer(layer_id).is_none_or(|layer| layer.locked);
                bar_pointer_hint(zone, locked)
            }
            Some(RowHit::Channel(layer, row, component)) => {
                if self
                    .keyframe_at_content_x(layer, &row, component, content_x)
                    .is_some()
                {
                    PointerHint::Keyframe
                } else {
                    PointerHint::Lane
                }
            }
            Some(RowHit::PropertyGroup(..)) | None => PointerHint::Arrow,
        }
    }

    fn update_pointer_hint(&mut self, next: PointerHint, cx: &mut Context<Self>) {
        if let Some(next) = pointer_hint_transition(
            self.pointer_hint,
            next,
            !matches!(self.drag, TimelineDrag::None),
        ) {
            self.pointer_hint = next;
            cx.notify();
        }
    }

    fn frames_delta(&self, from_x: f32, to_x: f32) -> i64 {
        ((to_x - from_x) as f64 / self.state.pixels_per_frame()).round() as i64
    }

    fn keyframe_is_live(&self, keyframe: &KeyframeRef) -> bool {
        self.state.layer(keyframe.layer).is_some_and(|layer| {
            keyframes::has_keyframe_at(layer, &keyframe.row, keyframe.component, keyframe.frame)
        })
    }

    fn move_keyframe_baselines(&self) -> Vec<KeyframeChannelBaseline> {
        let mut baselines: Vec<KeyframeChannelBaseline> = Vec::new();
        for keyframe in &self.selected_keyframes {
            let Some(layer) = self.state.layer(keyframe.layer) else {
                continue;
            };
            if layer.locked || !self.keyframe_is_live(keyframe) {
                continue;
            }
            if let Some(existing) = baselines.iter_mut().find(|baseline| {
                baseline.layer == keyframe.layer
                    && baseline.row == keyframe.row
                    && baseline.component == keyframe.component
            }) {
                existing.origin_frames.push(keyframe.frame);
                continue;
            }
            let Some(curve) = keyframes::row_channels(layer, &keyframe.row)
                .and_then(|channels| channels.get(keyframe.component).cloned())
                .and_then(|channel| match &channel.source {
                    ChannelSource::Keyframes(curve) => Some(curve.clone()),
                    _ => None,
                })
            else {
                continue;
            };
            baselines.push(KeyframeChannelBaseline {
                layer: keyframe.layer,
                row: keyframe.row.clone(),
                component: keyframe.component,
                curve,
                origin_frames: vec![keyframe.frame],
            });
        }
        for baseline in &mut baselines {
            baseline.origin_frames.sort_unstable();
        }
        baselines
    }

    fn apply_keyframe_move_preview(
        &mut self,
        baselines: &[KeyframeChannelBaseline],
        delta: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            for baseline in baselines {
                let Some(updated) = update_layer(&doc, comp_id, baseline.layer, |layer| {
                    keyframes::preview_keyframe_moves(
                        layer,
                        &baseline.row,
                        baseline.component,
                        &baseline.curve,
                        &baseline.origin_frames,
                        delta,
                    );
                }) else {
                    continue;
                };
                doc = updated;
            }
            project.apply_document(doc, InvalidationHint::None, cx);
        });
    }

    fn apply_graph_keyframe_preview(
        &mut self,
        baselines: &[KeyframeChannelBaseline],
        frame_delta: i64,
        value_delta: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            for baseline in baselines {
                let Some(updated) = update_layer(&doc, comp_id, baseline.layer, |layer| {
                    keyframes::preview_keyframe_moves_with_value_delta(
                        layer,
                        &baseline.row,
                        baseline.component,
                        &baseline.curve,
                        &baseline.origin_frames,
                        frame_delta,
                        value_delta,
                    );
                }) else {
                    continue;
                };
                doc = updated;
            }
            project.apply_document(doc, InvalidationHint::None, cx);
        });
    }

    fn apply_graph_tangent_preview(
        &mut self,
        baselines: &[KeyframeChannelBaseline],
        handle: keyframes::TangentHandle,
        delta: Vec2,
        coupling: keyframes::TangentCoupling,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            for baseline in baselines {
                let Some(updated) = update_layer(&doc, comp_id, baseline.layer, |layer| {
                    keyframes::preview_keyframe_tangents_with_delta(
                        layer,
                        &baseline.row,
                        baseline.component,
                        &baseline.curve,
                        keyframes::KeyframeTangentDeltaEdit {
                            frames: &baseline.origin_frames,
                            handle,
                            delta,
                            coupling,
                        },
                    );
                }) else {
                    continue;
                };
                doc = updated;
            }
            project.apply_document(doc, InvalidationHint::None, cx);
        });
    }

    fn begin_graph_drag(
        &mut self,
        curves: &[TimelineCurveData],
        hit: CurveHit,
        pointer: CurvePoint,
        transform: CurveTransform,
        graph_origin: (f32, f32),
    ) {
        let Some(curve) = curves.get(hit.curve) else {
            return;
        };
        let Some(layer) = self.state.layer(curve.channel.layer) else {
            return;
        };
        if layer.locked {
            return;
        }
        let Some(drag) = begin_drag(&curve.curve, hit, pointer) else {
            return;
        };
        match hit.part {
            HitPart::Keyframe => {
                let baselines = self.move_keyframe_baselines();
                if !baselines.iter().any(|baseline| {
                    baseline.layer == curve.channel.layer
                        && baseline.row == curve.channel.row
                        && baseline.component == curve.channel.component
                        && baseline.origin_frames.contains(&hit.frame)
                }) {
                    return;
                }
                let Some(pressed_value) = curve
                    .curve
                    .keyframes()
                    .iter()
                    .find(|keyframe| keyframe.frame == hit.frame)
                    .map(|keyframe| keyframe.value)
                else {
                    return;
                };
                self.drag = TimelineDrag::GraphKeyframes {
                    baselines,
                    origin_selection: self.selected_keyframes.clone(),
                    drag,
                    transform,
                    graph_origin,
                    pressed_value,
                    current_frame_delta: 0,
                    current_value_delta: 0.0,
                    changed: false,
                };
            }
            HitPart::TangentIn | HitPart::TangentOut => {
                let baselines = self.move_keyframe_baselines();
                if !baselines.iter().any(|baseline| {
                    baseline.layer == curve.channel.layer
                        && baseline.row == curve.channel.row
                        && baseline.component == curve.channel.component
                        && baseline.origin_frames.contains(&hit.frame)
                }) {
                    return;
                }
                let Some(keyframe) = curve
                    .curve
                    .keyframes()
                    .iter()
                    .find(|keyframe| keyframe.frame == hit.frame)
                else {
                    return;
                };
                let pressed_tangent = if hit.part == HitPart::TangentIn {
                    keyframe.tangent_in
                } else {
                    keyframe.tangent_out
                };
                self.drag = TimelineDrag::GraphTangent {
                    baselines,
                    drag,
                    transform,
                    graph_origin,
                    pressed_tangent,
                    current_delta: Vec2(0.0, 0.0),
                    current_coupling: keyframes::TangentCoupling::Symmetric,
                    changed: false,
                };
            }
        }
    }

    fn graph_keyframes_in_rect(
        curves: &[TimelineCurveData],
        transform: CurveTransform,
        start: CurvePoint,
        current: CurvePoint,
    ) -> HashSet<KeyframeRef> {
        let sources: Vec<_> = curves
            .iter()
            .map(|curve| CurveSource {
                curve: &curve.curve,
                frame_offset: curve.frame_offset,
            })
            .collect();
        keyframes_in_rect_with_offsets(&sources, transform, start, current)
            .into_iter()
            .filter_map(|hit| {
                let curve = curves.get(hit.curve)?;
                Some(KeyframeRef {
                    layer: curve.channel.layer,
                    row: curve.channel.row.clone(),
                    component: curve.channel.component,
                    frame: hit.frame,
                })
            })
            .collect()
    }

    fn selection_after_move(
        origin_selection: &HashSet<KeyframeRef>,
        baselines: &[KeyframeChannelBaseline],
        delta: i64,
    ) -> HashSet<KeyframeRef> {
        let mut selection = origin_selection.clone();
        for baseline in baselines {
            for frame in &baseline.origin_frames {
                selection.remove(&KeyframeRef {
                    layer: baseline.layer,
                    row: baseline.row.clone(),
                    component: baseline.component,
                    frame: *frame,
                });
            }
        }
        for baseline in baselines {
            for frame in &baseline.origin_frames {
                selection.insert(KeyframeRef {
                    layer: baseline.layer,
                    row: baseline.row.clone(),
                    component: baseline.component,
                    frame: (*frame as i64 + delta) as u64,
                });
            }
        }
        selection
    }

    fn drag_moved(&mut self, x: f32, y: f32, shift: bool, alt: bool, cx: &mut Context<Self>) {
        match self.drag.clone() {
            TimelineDrag::Scrub => {
                let local_x = (x - self.ruler_origin_x.get()).max(0.0) as f64;
                let frame = self.state.x_to_frame(local_x);
                self.scrub_playhead(frame, cx);
            }
            TimelineDrag::MoveBar {
                layer,
                origin_start,
                grab_x,
                ..
            } => {
                let delta = self.frames_delta(grab_x, x);
                let new_start = origin_start + delta;
                self.edit_layer(
                    layer,
                    InvalidationHint::None,
                    false,
                    |l| l.start_frame = new_start,
                    cx,
                );
                self.drag = TimelineDrag::MoveBar {
                    layer,
                    origin_start,
                    grab_x,
                    changed: true,
                };
            }
            TimelineDrag::TrimIn {
                layer,
                origin_start,
                origin_in,
                origin_out,
                grab_x,
                ..
            } => {
                let delta = self.frames_delta(grab_x, x);
                // The out edge stays fixed: start and in move together,
                // clamped into [0, out) (REQ-LAYER-006 display interval).
                let new_in = (origin_in as i64 + delta).clamp(0, origin_out as i64 - 1) as u64;
                let new_start = origin_start + (new_in as i64 - origin_in as i64);
                self.edit_layer(
                    layer,
                    InvalidationHint::None,
                    false,
                    |l| {
                        l.in_frame = new_in;
                        l.start_frame = new_start;
                    },
                    cx,
                );
                self.drag = TimelineDrag::TrimIn {
                    layer,
                    origin_start,
                    origin_in,
                    origin_out,
                    grab_x,
                    changed: true,
                };
            }
            TimelineDrag::TrimOut {
                layer,
                origin_in,
                origin_out,
                grab_x,
                ..
            } => {
                let delta = self.frames_delta(grab_x, x);
                let new_out = (origin_out as i64 + delta).max(origin_in as i64 + 1) as u64;
                self.edit_layer(
                    layer,
                    InvalidationHint::None,
                    false,
                    |l| l.out_frame = new_out,
                    cx,
                );
                self.drag = TimelineDrag::TrimOut {
                    layer,
                    origin_in,
                    origin_out,
                    grab_x,
                    changed: true,
                };
            }
            TimelineDrag::Reorder { layer, changed } => {
                let origin_y = self.area_origin.get().1;
                let Some(target) = self.layer_at_content_y(y - origin_y) else {
                    return;
                };
                if target == layer {
                    return;
                }
                let Some(project) = self.project.clone() else {
                    return;
                };
                let Some(comp_id) = self.state.comp_id() else {
                    return;
                };
                let Some(to_index) = self.state.layers().position(|l| l.id == target) else {
                    return;
                };
                project.update(cx, |project, cx| {
                    if let Some(doc) = reorder_layer(project.document(), comp_id, layer, to_index) {
                        project.apply_document(doc, InvalidationHint::Structural, cx);
                    }
                });
                let _ = changed;
                self.drag = TimelineDrag::Reorder {
                    layer,
                    changed: true,
                };
            }
            TimelineDrag::MoveKeyframe {
                baselines,
                origin_selection,
                pressed,
                collapse_on_click,
                current_delta,
                grab_x,
                ..
            } => {
                let min_origin = baselines
                    .iter()
                    .flat_map(|baseline| baseline.origin_frames.iter())
                    .copied()
                    .min()
                    .unwrap_or(0);
                let delta = self.frames_delta(grab_x, x).max(-(min_origin as i64));
                if delta == current_delta {
                    return;
                }
                self.apply_keyframe_move_preview(&baselines, delta, cx);
                self.selected_keyframes =
                    Self::selection_after_move(&origin_selection, &baselines, delta);
                self.drag = TimelineDrag::MoveKeyframe {
                    baselines,
                    origin_selection,
                    pressed,
                    collapse_on_click,
                    current_delta: delta,
                    grab_x,
                    changed: true,
                };
            }
            TimelineDrag::GraphKeyframes {
                baselines,
                origin_selection,
                drag,
                transform,
                graph_origin,
                pressed_value,
                current_frame_delta,
                current_value_delta,
                changed,
            } => {
                let pointer =
                    CurvePoint::new(f64::from(x - graph_origin.0), f64::from(y - graph_origin.1));
                let axis = if shift {
                    dominant_drag_axis(drag, pointer)
                } else {
                    CurveDragAxis::Free
                };
                let CurveEdit::MoveKeyframe {
                    from_frame,
                    to_frame,
                    value,
                    ..
                } = drag_to_constrained(drag, pointer, transform, axis)
                else {
                    return;
                };
                let min_origin = baselines
                    .iter()
                    .flat_map(|baseline| baseline.origin_frames.iter())
                    .copied()
                    .min()
                    .unwrap_or(0);
                let requested_delta = to_frame as i128 - from_frame as i128;
                let frame_delta =
                    requested_delta.clamp(-(min_origin as i128), i64::MAX as i128) as i64;
                let value_delta = value - pressed_value;
                if frame_delta == current_frame_delta
                    && value_delta.to_bits() == current_value_delta.to_bits()
                {
                    return;
                }
                self.apply_graph_keyframe_preview(&baselines, frame_delta, value_delta, cx);
                self.selected_keyframes =
                    Self::selection_after_move(&origin_selection, &baselines, frame_delta);
                self.drag = TimelineDrag::GraphKeyframes {
                    baselines,
                    origin_selection,
                    drag,
                    transform,
                    graph_origin,
                    pressed_value,
                    current_frame_delta: frame_delta,
                    current_value_delta: value_delta,
                    changed: changed || frame_delta != 0 || value_delta != 0.0,
                };
            }
            TimelineDrag::GraphTangent {
                baselines,
                drag,
                transform,
                graph_origin,
                pressed_tangent,
                current_delta,
                current_coupling,
                changed,
            } => {
                let pointer =
                    CurvePoint::new(f64::from(x - graph_origin.0), f64::from(y - graph_origin.1));
                let CurveEdit::SetTangent { part, tangent, .. } =
                    drag_to_with_tangent_snap(drag, pointer, transform, shift)
                else {
                    return;
                };
                let delta = Vec2(tangent.0 - pressed_tangent.0, tangent.1 - pressed_tangent.1);
                let coupling = if alt {
                    keyframes::TangentCoupling::Separated
                } else {
                    keyframes::TangentCoupling::Symmetric
                };
                if delta == current_delta && coupling == current_coupling {
                    return;
                }
                let handle = match part {
                    HitPart::TangentIn => keyframes::TangentHandle::In,
                    HitPart::TangentOut => keyframes::TangentHandle::Out,
                    HitPart::Keyframe => return,
                };
                self.apply_graph_tangent_preview(&baselines, handle, delta, coupling, cx);
                self.drag = TimelineDrag::GraphTangent {
                    baselines,
                    drag,
                    transform,
                    graph_origin,
                    pressed_tangent,
                    current_delta: delta,
                    current_coupling: coupling,
                    changed: changed || delta != Vec2(0.0, 0.0),
                };
            }
            TimelineDrag::GraphRubberBand {
                curves,
                transform,
                graph_origin,
                start,
                initial_selection,
                additive,
                ..
            } => {
                let current =
                    CurvePoint::new(f64::from(x - graph_origin.0), f64::from(y - graph_origin.1));
                let moved = current != start;
                let mut selection = if additive {
                    initial_selection.clone()
                } else {
                    HashSet::new()
                };
                if moved {
                    selection.extend(Self::graph_keyframes_in_rect(
                        &curves, transform, start, current,
                    ));
                }
                self.selected_keyframes = selection;
                self.drag = TimelineDrag::GraphRubberBand {
                    curves,
                    transform,
                    graph_origin,
                    start,
                    current,
                    initial_selection,
                    additive,
                    moved,
                };
                cx.notify();
            }
            TimelineDrag::RubberBand {
                start,
                initial_selection,
                additive,
                ..
            } => {
                let (origin_x, origin_y) = self.area_origin.get();
                let current = (x - origin_x, y - origin_y);
                let moved = current != start;
                let mut selection = if additive {
                    initial_selection.clone()
                } else {
                    HashSet::new()
                };
                if moved {
                    selection.extend(self.keyframes_in_rect(start, current));
                }
                self.selected_keyframes = selection;
                self.drag = TimelineDrag::RubberBand {
                    start,
                    current,
                    initial_selection,
                    additive,
                    moved,
                };
                cx.notify();
            }
            TimelineDrag::None => {}
        }
    }

    /// Abort the active drag (button state lost mid-gesture): its live
    /// document updates are uncommitted and must not leak into an unrelated
    /// undo step.
    fn cancel_drag(&mut self, cx: &mut Context<Self>) {
        let had_drag = !matches!(self.drag, TimelineDrag::None);
        let changed = match &self.drag {
            TimelineDrag::MoveBar { changed, .. }
            | TimelineDrag::TrimIn { changed, .. }
            | TimelineDrag::TrimOut { changed, .. }
            | TimelineDrag::Reorder { changed, .. }
            | TimelineDrag::MoveKeyframe { changed, .. }
            | TimelineDrag::GraphKeyframes { changed, .. }
            | TimelineDrag::GraphTangent { changed, .. } => *changed,
            TimelineDrag::None
            | TimelineDrag::Scrub
            | TimelineDrag::RubberBand { .. }
            | TimelineDrag::GraphRubberBand { .. } => false,
        };
        self.drag = TimelineDrag::None;
        if had_drag {
            cx.notify();
        }
        if !changed {
            return;
        }
        if let Some(project) = self.project.clone() {
            project.update(cx, |project, cx| {
                project.revert_document(cx);
            });
        }
    }

    fn drag_ended(&mut self, cx: &mut Context<Self>) {
        let collapse_to = match &self.drag {
            TimelineDrag::MoveKeyframe {
                pressed,
                collapse_on_click: true,
                changed: false,
                ..
            } => Some(pressed.clone()),
            _ => None,
        };
        let changed = match &self.drag {
            TimelineDrag::MoveBar { changed, .. }
            | TimelineDrag::TrimIn { changed, .. }
            | TimelineDrag::TrimOut { changed, .. }
            | TimelineDrag::Reorder { changed, .. }
            | TimelineDrag::MoveKeyframe { changed, .. }
            | TimelineDrag::GraphKeyframes { changed, .. }
            | TimelineDrag::GraphTangent { changed, .. } => *changed,
            TimelineDrag::None
            | TimelineDrag::Scrub
            | TimelineDrag::RubberBand { .. }
            | TimelineDrag::GraphRubberBand { .. } => false,
        };
        let structural = matches!(self.drag, TimelineDrag::Reorder { .. });
        self.drag = TimelineDrag::None;
        cx.notify();
        if let Some(pressed) = collapse_to {
            self.selected_keyframes = HashSet::from([pressed]);
        }
        if !changed {
            return;
        }
        // The gesture's live edits become one Document undo step.
        if let Some(project) = self.project.clone() {
            project.update(cx, |project, cx| {
                let doc = project.document().clone();
                let hint = if structural {
                    InvalidationHint::Structural
                } else {
                    InvalidationHint::None
                };
                project.commit_document(doc, hint, cx);
            });
        }
        self.publish_selected_layer_target(cx);
    }

    // ----- keyframe editing ----------------------------------------------------

    /// The layer-local frame of the keyframe diamond nearest to a
    /// content-space x on a channel row, within [`KEYFRAME_HIT_PX`].
    fn keyframe_at_content_x(
        &self,
        lid: LayerId,
        row: &PropertyRowId,
        component: usize,
        content_x: f64,
    ) -> Option<u64> {
        Self::keyframe_at_content_x_in(&self.state, lid, row, component, content_x)
    }

    fn keyframe_at_content_x_in(
        state: &TimelinePanel,
        lid: LayerId,
        row: &PropertyRowId,
        component: usize,
        content_x: f64,
    ) -> Option<u64> {
        let layer = state.layer(lid)?;
        let channels = keyframes::row_channels(layer, row)?;
        let channel = channels.get(component)?;
        let ChannelSource::Keyframes(curve) = &channel.source else {
            return None;
        };
        let ppf = state.pixels_per_frame();
        let scroll = state.scroll_offset();
        curve
            .keyframes()
            .iter()
            .map(|kf| {
                let x = (keyframes::comp_frame_for_key(layer, kf.frame) as f64 - scroll) * ppf;
                (kf.frame, (x - content_x).abs())
            })
            .filter(|(_, distance)| *distance <= KEYFRAME_HIT_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(frame, _)| frame)
    }

    /// Keyframes whose diamond centers lie inside an area-local rectangle.
    fn keyframes_in_rect(&self, start: (f32, f32), end: (f32, f32)) -> HashSet<KeyframeRef> {
        let min_x = start.0.min(end.0) as f64;
        let max_x = start.0.max(end.0) as f64;
        let min_y = start.1.min(end.1);
        let max_y = start.1.max(end.1);
        let ppf = self.state.pixels_per_frame();
        let scroll = self.state.scroll_offset();
        let mut hits = HashSet::new();
        let mut y = 0.0;

        for layer in self.state.layers().rev() {
            y += LAYER_ROW_HEIGHT;
            if !self.state.is_layer_expanded(layer.id) {
                continue;
            }
            for row in keyframes::property_rows(layer) {
                y += PROPERTY_ROW_HEIGHT;
                if !self.state.is_property_expanded(layer.id, &row.id) {
                    continue;
                }
                let channels = keyframes::row_channels(layer, &row.id).unwrap_or_default();
                for (component, channel) in channels.iter().enumerate() {
                    let center_y = y + PROPERTY_ROW_HEIGHT / 2.0;
                    if center_y >= min_y
                        && center_y <= max_y
                        && let ChannelSource::Keyframes(curve) = &channel.source
                    {
                        for keyframe in curve.keyframes() {
                            let center_x = (keyframes::comp_frame_for_key(layer, keyframe.frame)
                                as f64
                                - scroll)
                                * ppf;
                            if center_x >= min_x && center_x <= max_x {
                                hits.insert(KeyframeRef {
                                    layer: layer.id,
                                    row: row.id.clone(),
                                    component,
                                    frame: keyframe.frame,
                                });
                            }
                        }
                    }
                    y += PROPERTY_ROW_HEIGHT;
                }
            }
        }
        hits
    }

    /// Mouse down on a channel sub-row: click an existing diamond to select
    /// it and start a [`TimelineDrag::MoveKeyframe`], double-click empty
    /// space to add a keyframe, plain-click empty space to clear selection.
    #[allow(clippy::too_many_arguments)]
    fn channel_row_mouse_down(
        &mut self,
        lid: LayerId,
        row: PropertyRowId,
        component: usize,
        content_x: f64,
        click_count: usize,
        grab_x: f32,
        grab_y: f32,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        let hit_frame = self.keyframe_at_content_x(lid, &row, component, content_x);
        if click_count == 2 {
            // Double-click on an existing diamond only selects (done by the
            // first click); on empty space it adds a keyframe.
            if hit_frame.is_none() {
                let comp_frame = self.state.x_to_frame(content_x);
                self.add_keyframe_at(lid, row, component, comp_frame, cx);
            }
            return;
        }
        match hit_frame {
            Some(frame) => {
                let hit = KeyframeRef {
                    layer: lid,
                    row: row.clone(),
                    component,
                    frame,
                };
                let composition = self.state.composition().cloned();
                self.selected_keyframes.retain(|keyframe| {
                    composition
                        .as_ref()
                        .and_then(|comp| comp.get_layer(keyframe.layer))
                        .is_some_and(|layer| {
                            keyframes::has_keyframe_at(
                                layer,
                                &keyframe.row,
                                keyframe.component,
                                keyframe.frame,
                            )
                        })
                });
                let was_selected = self.selected_keyframes.contains(&hit);
                if shift {
                    if !self.selected_keyframes.insert(hit.clone()) {
                        self.selected_keyframes.remove(&hit);
                    }
                } else if !was_selected {
                    self.selected_keyframes.clear();
                    self.selected_keyframes.insert(hit.clone());
                }
                let layer = self.state.layer(lid);
                let locked = layer.is_none_or(|l| l.locked);
                if !locked && self.selected_keyframes.contains(&hit) {
                    let baselines = self.move_keyframe_baselines();
                    let origin_selection = self.selected_keyframes.clone();
                    self.drag = TimelineDrag::MoveKeyframe {
                        baselines,
                        origin_selection,
                        pressed: hit,
                        collapse_on_click: !shift,
                        current_delta: 0,
                        grab_x,
                        changed: false,
                    };
                } else {
                    self.drag = TimelineDrag::None;
                }
            }
            None => {
                let initial_selection = self.selected_keyframes.clone();
                if !shift {
                    self.selected_keyframes.clear();
                }
                let (origin_x, origin_y) = self.area_origin.get();
                let start = (grab_x - origin_x, grab_y - origin_y);
                self.drag = TimelineDrag::RubberBand {
                    start,
                    current: start,
                    initial_selection,
                    additive: shift,
                    moved: false,
                };
            }
        }
        cx.notify();
    }

    /// Insert a keyframe at a comp frame on a channel row and commit it as
    /// one Document undo step. The inserted key holds the channel's current
    /// value. No-op for locked layers or rows that do not resolve.
    pub fn add_keyframe_at(
        &mut self,
        lid: LayerId,
        row: PropertyRowId,
        component: usize,
        comp_frame: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        project.update(cx, |project, cx| {
            let locked = project
                .document()
                .get_composition(comp_id)
                .and_then(|c| c.get_layer(lid))
                .is_none_or(|l| l.locked);
            if locked {
                return;
            }
            let mut inserted = false;
            let Some(doc) = update_layer(project.document(), comp_id, lid, |l| {
                let local = keyframes::layer_local_frame(l, comp_frame);
                inserted = keyframes::insert_keyframe(l, &row, component, local);
            }) else {
                return;
            };
            // Only a real insertion earns an undo step (a non-key-editable
            // channel rejects the edit).
            if inserted {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
        cx.notify();
    }

    // ----- keyframe navigator ---------------------------------------------------

    /// Deduplicated, sorted comp frames of every keyframe across the row's
    /// channels (the navigator treats the property row as one lane). Comp
    /// frames are signed: a negative `start_frame` can push keys before 0.
    fn row_keyframe_comp_frames(&self, lid: LayerId, row: &PropertyRowId) -> Vec<i64> {
        let Some(layer) = self.state.layer(lid) else {
            return Vec::new();
        };
        let Some(channels) = keyframes::row_channels(layer, row) else {
            return Vec::new();
        };
        let mut frames: Vec<i64> = channels
            .iter()
            .filter_map(|channel| match &channel.source {
                ChannelSource::Keyframes(curve) => Some(curve.keyframes()),
                _ => None,
            })
            .flatten()
            .map(|kf| keyframes::comp_frame_for_key(layer, kf.frame))
            .collect();
        frames.sort_unstable();
        frames.dedup();
        frames
    }

    /// Navigator ◀: jump to the nearest keyframe strictly before the
    /// playhead. Keys pushed before comp frame 0 are unreachable. No-op
    /// when none exists.
    fn jump_to_prev_keyframe(&mut self, lid: LayerId, row: &PropertyRowId, cx: &mut Context<Self>) {
        let playhead = self.state.playhead() as i64;
        let frame = self
            .row_keyframe_comp_frames(lid, row)
            .into_iter()
            .take_while(|frame| *frame < playhead)
            .filter(|frame| *frame >= 0)
            .last();
        if let Some(frame) = frame {
            self.scrub_playhead(frame as u64, cx);
        }
    }

    /// Navigator ▶: jump to the nearest keyframe strictly after the
    /// playhead. No-op when none exists.
    fn jump_to_next_keyframe(&mut self, lid: LayerId, row: &PropertyRowId, cx: &mut Context<Self>) {
        let playhead = self.state.playhead() as i64;
        let frame = self
            .row_keyframe_comp_frames(lid, row)
            .into_iter()
            .find(|frame| *frame > playhead);
        if let Some(frame) = frame {
            self.scrub_playhead(frame as u64, cx);
        }
    }

    /// Whether every channel of the row holds a key at the playhead — the
    /// navigator diamond's fill state (same all-channels rule as the
    /// Properties panel's ◆ toggle).
    fn row_keyed_at_playhead(&self, lid: LayerId, row: &PropertyRowId) -> bool {
        let Some(layer) = self.state.layer(lid) else {
            return false;
        };
        let Some(channels) = keyframes::row_channels(layer, row) else {
            return false;
        };
        if channels.is_empty() {
            return false;
        }
        let local = keyframes::layer_local_frame(layer, self.state.playhead());
        (0..channels.len())
            .all(|component| keyframes::has_keyframe_at(layer, row, component, local))
    }

    /// Navigator ◆: toggle keys at the playhead across the row's channels
    /// as one Document undo step. Fully keyed rows lose their keys at the
    /// frame; otherwise the missing keys are inserted. Locked layers are
    /// protected (checked against the document).
    fn toggle_row_keyframe(&mut self, lid: LayerId, row: &PropertyRowId, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp_id) = self.state.comp_id() else {
            return;
        };
        let comp_frame = self.state.playhead();
        project.update(cx, |project, cx| {
            let Some(layer) = project
                .document()
                .get_composition(comp_id)
                .and_then(|c| c.get_layer(lid))
            else {
                return;
            };
            if layer.locked {
                return;
            }
            let Some(channels) = keyframes::row_channels(layer, row) else {
                return;
            };
            let components = channels.len();
            if components == 0 {
                return;
            }
            let local = keyframes::layer_local_frame(layer, comp_frame);
            let fully_keyed = (0..components)
                .all(|component| keyframes::has_keyframe_at(layer, row, component, local));
            let mut changed = false;
            let Some(doc) = update_layer(project.document(), comp_id, lid, |l| {
                for component in 0..components {
                    if fully_keyed {
                        changed |= keyframes::remove_keyframe(l, row, component, local);
                    } else if !keyframes::has_keyframe_at(l, row, component, local) {
                        changed |= keyframes::insert_keyframe(l, row, component, local);
                    }
                }
            }) else {
                return;
            };
            if changed {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
        cx.notify();
    }

    // ----- playback glue -------------------------------------------------------

    fn begin_timecode_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.timecode_input.is_some() {
            return;
        }
        let frame = self.state.playhead().to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(frame));
        let input_sub =
            cx.subscribe(
                &input,
                |this: &mut Self, _input, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => this.commit_timecode_edit(cx),
                    InputEvent::Blur => this.cancel_timecode_edit(cx),
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.timecode_input = Some(input);
        self.timecode_input_sub = Some(input_sub);
        cx.notify();
    }

    fn commit_timecode_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.timecode_input.take() else {
            return;
        };
        self.timecode_input_sub = None;
        let value = input.read(cx).value().to_string();
        if let Some(frame) = parse_frame_entry(
            &value,
            self.state.frame_rate(),
            self.state.duration_frames(),
        ) {
            self.scrub_playhead(frame, cx);
        }
        cx.notify();
    }

    fn cancel_timecode_edit(&mut self, cx: &mut Context<Self>) {
        self.timecode_input = None;
        self.timecode_input_sub = None;
        cx.notify();
    }

    fn sync_zoom_slider(&self, window: &mut Window, cx: &mut Context<Self>) {
        let value = ppf_to_slider(self.state.pixels_per_frame());
        self.zoom_slider
            .update(cx, |slider, cx| slider.set_value(value, window, cx));
    }

    fn fit_timeline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ppf = fit_pixels_per_frame(self.ruler_width.get() as f64, self.state.duration_frames());
        self.state.set_pixels_per_frame(ppf);
        self.state.set_scroll_offset(0.0);
        self.sync_zoom_slider(window, cx);
        cx.notify();
    }

    fn build_transport_toolbar(&self, is_playing: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let playhead = self.state.playhead();
        let fps_value = self.state.frame_rate();
        let fps = format_fps(fps_value);
        let duration_frames = self.state.duration_frames();
        let graph_mode = self.state.view_mode() == TimelineViewMode::Graph;
        let interpolation = self.selected_interpolation();
        let can_edit_interpolation = !self.selected_keyframes.is_empty();

        let graph_controls = if graph_mode {
            div()
                .flex()
                .items_center()
                .gap_1()
                .ml_2()
                .pl_2()
                .border_l_1()
                .border_color(colors.border)
                .child(
                    Button::new("curve-grid")
                        .xsmall()
                        .ghost()
                        .selected(self.show_curve_grid)
                        .icon(Icon::new(RavelIcon::GridOverlay))
                        .tooltip(t!("timeline.graph.grid"))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.toggle_curve_grid(cx);
                        })),
                )
                .child(
                    Button::new("curve-fit-values")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(RavelIcon::TimelineFit))
                        .tooltip(t!("timeline.graph.fit_values"))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.fit_curve_values(cx);
                        })),
                )
                .child(
                    Button::new("curve-bezier")
                        .xsmall()
                        .ghost()
                        .selected(interpolation == Some(Interpolation::Bezier))
                        .disabled(!can_edit_interpolation)
                        .icon(Icon::new(RavelIcon::InterpolationBezier))
                        .tooltip(t!("timeline.interpolation.bezier"))
                        .on_click(|_event, window, cx| {
                            window.dispatch_action(Box::new(KeyframeInterpolationBezier), cx);
                        }),
                )
                .child(
                    Button::new("curve-linear")
                        .xsmall()
                        .ghost()
                        .selected(interpolation == Some(Interpolation::Linear))
                        .disabled(!can_edit_interpolation)
                        .icon(Icon::new(RavelIcon::InterpolationLinear))
                        .tooltip(t!("timeline.interpolation.linear"))
                        .on_click(|_event, window, cx| {
                            window.dispatch_action(Box::new(KeyframeInterpolationLinear), cx);
                        }),
                )
                .child(
                    Button::new("curve-step")
                        .xsmall()
                        .ghost()
                        .selected(interpolation == Some(Interpolation::Step))
                        .disabled(!can_edit_interpolation)
                        .icon(Icon::new(RavelIcon::InterpolationStep))
                        .tooltip(t!("timeline.interpolation.step"))
                        .on_click(|_event, window, cx| {
                            window.dispatch_action(Box::new(KeyframeInterpolationStep), cx);
                        }),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let timecode = if let Some(input) = &self.timecode_input {
            div()
                .w(px(92.0))
                .h(px(22.0))
                .child(Input::new(input).small())
                .into_any_element()
        } else {
            div()
                .id("timeline-timecode")
                .w(px(92.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .px_1()
                .rounded(px(2.0))
                .cursor_pointer()
                .text_xs()
                .text_color(colors.foreground)
                .hover(|this| this.bg(colors.muted))
                .child(SharedString::from(format_timecode(playhead, fps_value)))
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.begin_timecode_edit(window, cx);
                }))
                .into_any_element()
        };

        div()
            .id("timeline-transport-toolbar")
            .h(px(TRANSPORT_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .bg(colors.tab_bar)
            .border_b_1()
            .border_color(colors.border)
            .child(timecode)
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(format!("{playhead}f"))),
            )
            .child(
                div()
                    .ml_2()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(format!(
                        "{fps} fps · {duration_frames}f"
                    ))),
            )
            .child(graph_controls)
            .child(div().flex_1())
            .child(
                Button::new("timeline-to-start")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::SkipBack))
                    .tooltip(t!("timeline.transport.to_start"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.scrub_playhead(0, cx);
                    })),
            )
            .child(
                Button::new("timeline-step-back")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::StepBack))
                    .tooltip(t!("timeline.transport.step_back"))
                    .on_click(|_event, window, cx| {
                        window.dispatch_action(Box::new(FrameStepBackward), cx);
                    }),
            )
            .child(
                Button::new("timeline-play-pause")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(if is_playing {
                        RavelIcon::Pause
                    } else {
                        RavelIcon::Play
                    }))
                    .tooltip(if is_playing {
                        t!("timeline.transport.pause")
                    } else {
                        t!("timeline.transport.play")
                    })
                    .on_click(|_event, window, cx| {
                        window.dispatch_action(Box::new(PlaybackToggle), cx);
                    }),
            )
            .child(
                Button::new("timeline-stop")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::Stop))
                    .tooltip(t!("timeline.transport.stop"))
                    .on_click(|_event, window, cx| {
                        window.dispatch_action(Box::new(PlaybackStop), cx);
                    }),
            )
            .child(
                Button::new("timeline-step-forward")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::StepForward))
                    .tooltip(t!("timeline.transport.step_forward"))
                    .on_click(|_event, window, cx| {
                        window.dispatch_action(Box::new(FrameStepForward), cx);
                    }),
            )
            .child(
                Button::new("timeline-to-end")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::SkipForward))
                    .tooltip(t!("timeline.transport.to_end"))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        let end = this.state.duration_frames().saturating_sub(1);
                        this.scrub_playhead(end, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .w(px(104.0))
                    .px_1()
                    .child(Slider::new(&self.zoom_slider)),
            )
            .child(
                Button::new("timeline-fit")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(RavelIcon::TimelineFit))
                    .tooltip(t!("timeline.transport.fit"))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.fit_timeline(window, cx);
                    })),
            )
    }

    /// Moves the playhead (playback controller entry point). When
    /// follow-playhead is enabled, pages the visible range along with it.
    /// The controller records the shared `PlaybackPosition` on the same
    /// path, which the Properties panel observes — no republish needed here.
    pub fn set_playhead(&mut self, frame: u64) {
        self.state.set_playhead(frame);
        self.state
            .scroll_to_follow_playhead(self.ruler_width.get() as f64);
    }

    /// Ruler scrub: moves the local playhead and seeks the playback clock so
    /// playback and frame steps resume from the scrubbed position.
    fn scrub_playhead(&mut self, frame: u64, cx: &mut Context<Self>) {
        let Some((fps, duration_frames)) = self.composition_params() else {
            return;
        };
        let frame = frame.min(duration_frames.saturating_sub(1));
        self.state.set_playhead(frame);
        let controller = cx
            .try_global::<crate::playback::PlaybackControllerHandle>()
            .and_then(|handle| handle.0.upgrade());
        if let Some(controller) = controller {
            // This panel is on the entity update stack, so the controller
            // gets the composition parameters as arguments; it must not
            // read the timeline entity back.
            controller.update(cx, |controller, cx| {
                controller.seek_from_timeline(frame, fps, duration_frames, cx);
            });
        }
        cx.notify();
    }

    /// The frame currently under the playhead.
    pub fn playhead(&self) -> u64 {
        self.state.playhead()
    }

    /// Frame rate and duration of the displayed composition, for the
    /// playback clock. `None` while no composition is active — the
    /// transport then has nothing to run over.
    pub fn composition_params(&self) -> Option<(FrameRate, u64)> {
        self.state
            .composition()
            .map(|comp| (comp.frame_rate, comp.duration_frames))
    }

    fn build_ruler(&self, theme_colors: &ThemeColor) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;
        let ruler_width = self.ruler_width.clone();
        let ruler_origin_x = self.ruler_origin_x.clone();

        canvas(
            move |bounds, _window, _cx| {
                ruler_origin_x.set(bounds.origin.x.into());
                ruler_width.set(bounds.size.width.into());
                state
            },
            move |bounds, state, window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let fr = state.frame_rate();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.tab_bar));

                let border_bounds = Bounds::new(
                    point(
                        bounds.origin.x,
                        bounds.origin.y + bounds.size.height - px(1.0),
                    ),
                    size(bounds.size.width, px(1.0)),
                );
                window.paint_quad(fill(border_bounds, colors.border));

                let (minor_interval, major_interval) = tick_intervals(ppf, fr);
                if minor_interval == 0 || major_interval == 0 {
                    return;
                }

                let first_frame = scroll.floor().max(0.0) as u64;
                let visible_frames = (area_width as f64 / ppf).ceil() as u64 + 1;
                let last_frame = first_frame + visible_frames;
                let start = (first_frame / minor_interval) * minor_interval;

                for frame in (start..=last_frame).step_by(minor_interval as usize) {
                    let x_px = (frame as f64 - scroll) * ppf;
                    if x_px < 0.0 {
                        continue;
                    }
                    let x = bounds.origin.x + px(x_px as f32);
                    let is_major = frame % major_interval == 0;

                    let tick_h = if is_major {
                        bounds.size.height * 0.6
                    } else {
                        bounds.size.height * 0.3
                    };

                    let tick_bounds = Bounds::new(
                        point(x, bounds.origin.y + bounds.size.height - tick_h),
                        size(px(1.0), tick_h),
                    );
                    let tick_color = if is_major {
                        Hsla {
                            a: 0.6,
                            ..colors.foreground
                        }
                    } else {
                        Hsla {
                            a: 0.2,
                            ..colors.foreground
                        }
                    };
                    window.paint_quad(fill(tick_bounds, tick_color));

                    if is_major && ppf > 0.5 {
                        let label = format_frame_label(frame, fr);
                        let text: SharedString = label.into();
                        let text_len = text.len();
                        let font = Font {
                            family: SharedString::from("sans-serif"),
                            ..Default::default()
                        };
                        let shaped = window.text_system().shape_line(
                            text,
                            px(10.0),
                            &[TextRun {
                                len: text_len,
                                font,
                                color: colors.muted_foreground,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        );
                        let text_origin = point(x + px(3.0), bounds.origin.y + px(2.0));
                        shaped
                            .paint(
                                text_origin,
                                bounds.size.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                }
            },
        )
        .h(px(RULER_HEIGHT))
        .w_full()
        .cursor(CursorStyle::ResizeLeftRight)
    }

    /// The layer-area row under a content-space y: layer bar, property
    /// group, or channel sub-row, following the same layout as the painter
    /// (top layer first, property rows only while expanded).
    fn row_at_content_y(&self, content_y: f32) -> Option<RowHit> {
        Self::row_at_content_y_in(&self.state, content_y)
    }

    fn row_at_content_y_in(state: &TimelinePanel, content_y: f32) -> Option<RowHit> {
        let mut y = 0.0f32;
        for layer in state.layers().rev() {
            if content_y >= y && content_y < y + LAYER_ROW_HEIGHT {
                return Some(RowHit::LayerBar(layer.id));
            }
            y += LAYER_ROW_HEIGHT;
            if state.is_layer_expanded(layer.id) {
                for row in keyframes::property_rows(layer) {
                    if content_y >= y && content_y < y + PROPERTY_ROW_HEIGHT {
                        return Some(RowHit::PropertyGroup(layer.id, row.id));
                    }
                    y += PROPERTY_ROW_HEIGHT;
                    if state.is_property_expanded(layer.id, &row.id) {
                        for component in 0..row.channel_names.len() {
                            if content_y >= y && content_y < y + PROPERTY_ROW_HEIGHT {
                                return Some(RowHit::Channel(layer.id, row.id, component));
                            }
                            y += PROPERTY_ROW_HEIGHT;
                        }
                    }
                }
            }
        }
        None
    }

    fn layer_at_content_y(&self, content_y: f32) -> Option<ravel_core::id::LayerId> {
        match self.row_at_content_y(content_y) {
            Some(RowHit::LayerBar(lid)) => Some(lid),
            _ => None,
        }
    }

    fn total_layer_height(&self) -> f32 {
        let mut h = 0.0f32;
        for layer in self.state.layers() {
            h += LAYER_ROW_HEIGHT;
            if self.state.is_layer_expanded(layer.id) {
                for row in keyframes::property_rows(layer) {
                    h += PROPERTY_ROW_HEIGHT;
                    if self.state.is_property_expanded(layer.id, &row.id) {
                        h += row.channel_names.len() as f32 * PROPERTY_ROW_HEIGHT;
                    }
                }
            }
        }
        h
    }

    fn build_layer_area(
        &self,
        theme_colors: &ThemeColor,
        area_origin: Rc<Cell<(f32, f32)>>,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;
        // Bars outline every selected layer (REQ-UI-013 multi-selection).
        let selected_layers: Vec<LayerId> = super::layer_selection(cx).layers().to_vec();
        let selected_keyframes = self.selected_keyframes.clone();
        let preparing_layers: HashSet<LayerId> = self
            .audio
            .as_ref()
            .map(|audio| {
                state
                    .layers()
                    .filter(|layer| audio.read(cx).is_layer_preparing(layer.id))
                    .map(|layer| layer.id)
                    .collect()
            })
            .unwrap_or_default();
        let preparing_label = t!("audio.preparing");
        let rubber_band = match &self.drag {
            TimelineDrag::RubberBand {
                start,
                current,
                moved: true,
                ..
            } => Some((*start, *current)),
            _ => None,
        };
        let content_height = self.total_layer_height();
        let cursor = self.pointer_hint.cursor();
        let active_drag_cursor = drag_cursor(&self.drag);

        canvas(
            move |bounds, _window, _cx| {
                area_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                (
                    state,
                    selected_layers,
                    selected_keyframes,
                    rubber_band,
                    preparing_layers,
                    preparing_label,
                )
            },
            move |bounds,
                  (
                state,
                selected_layers,
                selected_keyframes,
                rubber_band,
                preparing_layers,
                preparing_label,
            ),
                  window,
                  cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.background));

                let mut y = bounds.origin.y;
                for layer in state.layers().rev() {
                    // Layer bar row
                    let lane_border = Bounds::new(
                        point(bounds.origin.x, y + px(LAYER_ROW_HEIGHT) - px(1.0)),
                        size(bounds.size.width, px(1.0)),
                    );
                    window.paint_quad(fill(lane_border, colors.border));

                    let bar_x = (layer.start_frame as f64 - scroll) * ppf;
                    let bar_w = layer.duration() as f64 * ppf;

                    if bar_x + bar_w >= 0.0 && bar_x < area_width as f64 {
                        let x = bounds.origin.x + px(bar_x.max(0.0) as f32);
                        let visible_w = if bar_x < 0.0 { bar_w + bar_x } else { bar_w };
                        let w = px(visible_w.min(area_width as f64 - bar_x.max(0.0)) as f32);

                        let bar_color = layer_color(layer, &colors);
                        let bar_bounds =
                            Bounds::new(point(x, y + px(2.0)), size(w, px(LAYER_ROW_HEIGHT - 4.0)));
                        window.paint_quad(
                            fill(bar_bounds, bar_color).corner_radii(px(LAYER_BAR_CORNER_RADIUS)),
                        );

                        if selected_layers.contains(&layer.id) {
                            window.paint_quad(
                                outline(bar_bounds, colors.foreground, BorderStyle::default())
                                    .corner_radii(px(LAYER_BAR_CORNER_RADIUS))
                                    .border_widths(px(2.0)),
                            );
                        }

                        if bar_w > 40.0 {
                            let bar_top = y + px(2.0);
                            let bar_h = LAYER_ROW_HEIGHT - 4.0;
                            let label = if preparing_layers.contains(&layer.id) {
                                format!("{} · {preparing_label}", layer.name)
                            } else {
                                layer.name.clone()
                            };
                            paint_bar_label(
                                &label,
                                x + px(LAYER_TEXT_PADDING),
                                bar_top + px((bar_h - 11.0) / 2.0 - 1.0),
                                px(bar_h),
                                &colors,
                                window,
                                cx,
                            );
                        }
                    }

                    if layer.muted {
                        let mute_bounds = Bounds::new(
                            point(bounds.origin.x, y),
                            size(bounds.size.width, px(LAYER_ROW_HEIGHT)),
                        );
                        window.paint_quad(fill(
                            mute_bounds,
                            Hsla {
                                a: 0.5,
                                ..colors.background
                            },
                        ));
                    }

                    y += px(LAYER_ROW_HEIGHT);

                    // Property rows (always present when layer is expanded)
                    if state.is_layer_expanded(layer.id) {
                        for row in keyframes::property_rows(layer) {
                            let prop_border = Bounds::new(
                                point(bounds.origin.x, y + px(PROPERTY_ROW_HEIGHT) - px(1.0)),
                                size(bounds.size.width, px(1.0)),
                            );
                            window.paint_quad(fill(
                                prop_border,
                                Hsla {
                                    a: 0.3,
                                    ..colors.border
                                },
                            ));

                            y += px(PROPERTY_ROW_HEIGHT);

                            // Channel sub-rows with keyframe diamonds
                            if state.is_property_expanded(layer.id, &row.id) {
                                let channels =
                                    keyframes::row_channels(layer, &row.id).unwrap_or_default();
                                for (component, channel) in channels.iter().enumerate() {
                                    // Channel row border
                                    let ch_border = Bounds::new(
                                        point(
                                            bounds.origin.x,
                                            y + px(PROPERTY_ROW_HEIGHT) - px(1.0),
                                        ),
                                        size(bounds.size.width, px(1.0)),
                                    );
                                    window.paint_quad(fill(
                                        ch_border,
                                        Hsla {
                                            a: 0.15,
                                            ..colors.border
                                        },
                                    ));

                                    if let ChannelSource::Keyframes(curve) = &channel.source {
                                        for kf in curve.keyframes() {
                                            // Keyframe frames are layer-local;
                                            // the diamond sits at the comp
                                            // frame (in_frame offset included).
                                            let kf_x =
                                                (keyframes::comp_frame_for_key(layer, kf.frame)
                                                    as f64
                                                    - scroll)
                                                    * ppf;
                                            if kf_x >= 0.0 && kf_x < area_width as f64 {
                                                let is_selected =
                                                    selected_keyframes.contains(&KeyframeRef {
                                                        layer: layer.id,
                                                        row: row.id.clone(),
                                                        component,
                                                        frame: kf.frame,
                                                    });
                                                paint_diamond(
                                                    bounds.origin.x + px(kf_x as f32),
                                                    y + px(PROPERTY_ROW_HEIGHT / 2.0),
                                                    if is_selected {
                                                        colors.foreground
                                                    } else {
                                                        colors.primary
                                                    },
                                                    window,
                                                );
                                            }
                                        }
                                    }

                                    y += px(PROPERTY_ROW_HEIGHT);
                                }
                            }
                        }
                    }
                }

                // Playhead
                let playhead_x = (state.playhead() as f64 - scroll) * ppf;
                if playhead_x >= 0.0 && (playhead_x as f32) < area_width {
                    let ph_bounds = Bounds::new(
                        point(
                            bounds.origin.x + px(playhead_x as f32 - PLAYHEAD_WIDTH / 2.0),
                            bounds.origin.y,
                        ),
                        size(px(PLAYHEAD_WIDTH), bounds.size.height),
                    );
                    window.paint_quad(fill(ph_bounds, colors.primary));
                }

                if let Some((start, current)) = rubber_band {
                    let area_height: f32 = bounds.size.height.into();
                    let left = start.0.min(current.0).clamp(0.0, area_width);
                    let right = start.0.max(current.0).clamp(0.0, area_width);
                    let top = start.1.min(current.1).clamp(0.0, area_height);
                    let bottom = start.1.max(current.1).clamp(0.0, area_height);
                    let band_bounds = Bounds::new(
                        point(bounds.origin.x + px(left), bounds.origin.y + px(top)),
                        size(px(right - left), px(bottom - top)),
                    );
                    window.paint_quad(fill(
                        band_bounds,
                        Hsla {
                            a: 0.18,
                            ..colors.primary
                        },
                    ));
                    window.paint_quad(
                        outline(band_bounds, colors.primary, BorderStyle::default())
                            .border_widths(px(1.0)),
                    );
                }
                if let Some(cursor) = active_drag_cursor {
                    window.set_window_cursor_style(cursor);
                }
            },
        )
        .flex_grow()
        .h(px(content_height))
        .cursor(cursor)
    }

    /// Timeline adapter around the axis-agnostic curve editor widget.
    fn build_curve_editor_shell(
        &self,
        theme_colors: &ThemeColor,
        area_origin: Rc<Cell<(f32, f32)>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;
        let content_height = self.total_layer_height().max(LAYER_ROW_HEIGHT);
        let resolved = selected_timeline_curves(&self.state, theme_colors);
        let has_live_curves = !resolved.is_empty();
        let auto_value_bounds = curve_value_bounds(&resolved)
            .unwrap_or((-CURVE_DEGENERATE_MARGIN, CURVE_DEGENERATE_MARGIN));
        let drag_value_bounds = match &self.drag {
            TimelineDrag::GraphKeyframes { transform, .. }
            | TimelineDrag::GraphTangent { transform, .. }
            | TimelineDrag::GraphRubberBand { transform, .. } => {
                Some((transform.data_min.y, transform.data_max.y))
            }
            _ => None,
        };
        let value_bounds =
            drag_value_bounds.unwrap_or_else(|| self.curve_value_range.resolved(auto_value_bounds));
        let graph_size = Rc::new(Cell::new((0.0_f32, 0.0_f32)));
        let grid = curve_grid_canvas(
            self.state.clone(),
            value_bounds,
            colors,
            self.show_curve_grid && has_live_curves,
        );
        let curve_canvas = if has_live_curves {
            let series = resolved
                .iter()
                .map(|item| {
                    let selected_frames = self
                        .selected_keyframes
                        .iter()
                        .filter(|selected| {
                            selected.layer == item.channel.layer
                                && selected.row == item.channel.row
                                && selected.component == item.channel.component
                        })
                        .map(|selected| selected.frame)
                        .collect();
                    CurveSeries {
                        curve: item.curve.clone(),
                        color: item.color,
                        frame_offset: item.frame_offset,
                        selected_frames: Arc::new(selected_frames),
                    }
                })
                .collect();
            let transparent = Hsla {
                a: 0.0,
                ..colors.background
            };
            curve_editor_canvas_with_x_scale(
                self.state.scroll_offset(),
                self.state.pixels_per_frame(),
                value_bounds.0,
                value_bounds.1,
                series,
                transparent,
                colors.muted_foreground,
            )
            .into_any_element()
        } else {
            div().size_full().into_any_element()
        };

        let hit_curves = resolved.clone();
        let hover_curves = resolved.clone();
        let left_origin = area_origin.clone();
        let left_size = graph_size.clone();
        let hover_origin = area_origin.clone();
        let hover_size = graph_size.clone();
        let right_origin = area_origin.clone();
        let right_size = graph_size.clone();
        let last_right_click = self.last_right_click.clone();
        let graph_rubber_band = match &self.drag {
            TimelineDrag::GraphRubberBand {
                start,
                current,
                moved: true,
                ..
            } => Some((*start, *current)),
            _ => None,
        };
        let cursor = self.pointer_hint.cursor();
        let active_drag_cursor = drag_cursor(&self.drag);

        let host = div()
            .id("timeline-curve-editor-host")
            .relative()
            .flex_grow()
            .h(px(content_height))
            .overflow_hidden()
            .bg(colors.background)
            .child(div().absolute().inset_0().child(grid))
            .child(div().absolute().inset_0().child(curve_canvas))
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        area_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                        graph_size.set((bounds.size.width.into(), bounds.size.height.into()));
                        state
                    },
                    move |bounds, state, window, _cx| {
                        let playhead_x = state.frame_to_x(state.playhead() as i64);
                        let area_width: f32 = bounds.size.width.into();
                        if playhead_x >= 0.0 && playhead_x < area_width as f64 {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(
                                        bounds.origin.x
                                            + px(playhead_x as f32 - PLAYHEAD_WIDTH / 2.0),
                                        bounds.origin.y,
                                    ),
                                    size(px(PLAYHEAD_WIDTH), bounds.size.height),
                                ),
                                colors.primary,
                            ));
                        }
                        if let Some((start, current)) = graph_rubber_band {
                            let area_height: f32 = bounds.size.height.into();
                            let left = start.x.min(current.x).clamp(0.0, area_width as f64) as f32;
                            let right = start.x.max(current.x).clamp(0.0, area_width as f64) as f32;
                            let top = start.y.min(current.y).clamp(0.0, area_height as f64) as f32;
                            let bottom =
                                start.y.max(current.y).clamp(0.0, area_height as f64) as f32;
                            let band_bounds = Bounds::new(
                                point(bounds.origin.x + px(left), bounds.origin.y + px(top)),
                                size(px(right - left), px(bottom - top)),
                            );
                            window.paint_quad(fill(
                                band_bounds,
                                Hsla {
                                    a: 0.18,
                                    ..colors.primary
                                },
                            ));
                            window.paint_quad(
                                outline(band_bounds, colors.primary, BorderStyle::default())
                                    .border_widths(px(1.0)),
                            );
                        }
                        if let Some(cursor) = active_drag_cursor {
                            window.set_window_cursor_style(cursor);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .cursor(cursor)
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                    let (origin_x, origin_y) = hover_origin.get();
                    let (width, height) = hover_size.get();
                    let pointer = CurvePoint::new(
                        f64::from(event.position.x) - origin_x as f64,
                        f64::from(event.position.y) - origin_y as f64,
                    );
                    let hint = graph_pointer_hint(graph_hit_at(
                        &hover_curves,
                        this.state.scroll_offset(),
                        this.state.pixels_per_frame(),
                        value_bounds,
                        (width, height),
                        pointer,
                    ));
                    this.update_pointer_hint(hint, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (origin_x, origin_y) = left_origin.get();
                    let (width, height) = left_size.get();
                    let pointer = CurvePoint::new(
                        f64::from(event.position.x) - origin_x as f64,
                        f64::from(event.position.y) - origin_y as f64,
                    );
                    let hit = graph_hit_at(
                        &hit_curves,
                        this.state.scroll_offset(),
                        this.state.pixels_per_frame(),
                        value_bounds,
                        (width, height),
                        pointer,
                    );
                    if let Some(hit) = hit {
                        this.select_graph_hit(&hit_curves, hit, event.modifiers.shift, cx);
                        if let Some(transform) = graph_transform(
                            this.state.scroll_offset(),
                            this.state.pixels_per_frame(),
                            value_bounds,
                            (width, height),
                        ) {
                            this.begin_graph_drag(
                                &hit_curves,
                                hit,
                                pointer,
                                transform,
                                (origin_x, origin_y),
                            );
                        }
                    } else if event.click_count == 2 {
                        if let Some(curve) = hit_curves.first() {
                            let comp_frame = this.state.x_to_frame(pointer.x);
                            this.add_keyframe_at(
                                curve.channel.layer,
                                curve.channel.row.clone(),
                                curve.channel.component,
                                comp_frame,
                                cx,
                            );
                        }
                    } else if let Some(transform) = graph_transform(
                        this.state.scroll_offset(),
                        this.state.pixels_per_frame(),
                        value_bounds,
                        (width, height),
                    ) {
                        let initial_selection = this.selected_keyframes.clone();
                        if !event.modifiers.shift {
                            this.selected_keyframes.clear();
                        }
                        this.drag = TimelineDrag::GraphRubberBand {
                            curves: hit_curves.clone(),
                            transform,
                            graph_origin: (origin_x, origin_y),
                            start: pointer,
                            current: pointer,
                            initial_selection,
                            additive: event.modifiers.shift,
                            moved: false,
                        };
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (origin_x, origin_y) = right_origin.get();
                    let (width, height) = right_size.get();
                    let pointer = CurvePoint::new(
                        f64::from(event.position.x) - origin_x as f64,
                        f64::from(event.position.y) - origin_y as f64,
                    );
                    last_right_click.set((pointer.x as f32, pointer.y as f32));
                    if let Some(hit) = graph_hit_at(
                        &resolved,
                        this.state.scroll_offset(),
                        this.state.pixels_per_frame(),
                        value_bounds,
                        (width, height),
                        pointer,
                    ) {
                        this.select_graph_hit(&resolved, hit, false, cx);
                    }
                }),
            );

        if has_live_curves {
            host
        } else {
            host.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("timeline.graph.empty"))),
            )
        }
    }

    fn build_layer_headers(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        // Every selected layer is highlighted, not just the primary one
        // (REQ-UI-013 multi-selection).
        let selection = super::layer_selection(cx);

        let mut headers = div()
            .id("layer-headers")
            .w(px(HEADER_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.list);

        // Collect layer data to avoid borrow issues
        let layers: Vec<_> = self
            .state
            .layers()
            .rev()
            .map(|l| (l.id, l.name.clone(), l.solo, l.muted, l.locked))
            .collect();
        let expanded_layers: Vec<_> = layers
            .iter()
            .map(|(id, ..)| self.state.is_layer_expanded(*id))
            .collect();
        let layer_rows: Vec<Vec<PropertyRow>> = layers
            .iter()
            .map(|(id, ..)| {
                self.state
                    .layer(*id)
                    .map(keyframes::property_rows)
                    .unwrap_or_default()
            })
            .collect();

        for (i, (layer_id, name, solo, muted, locked)) in layers.iter().enumerate() {
            let is_selected = selection.contains(*layer_id);
            let bg = if is_selected {
                theme.colors.list_active
            } else {
                theme.colors.list
            };
            let lid = *layer_id;
            let is_expanded = expanded_layers[i];

            let expand_arrow = if is_expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            };

            headers = headers.child(
                div()
                    .id(SharedString::from(format!("lh-{}", lid)))
                    .h(px(LAYER_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .px_1()
                    .gap_1()
                    .bg(bg)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _win, cx| {
                            let mode = LayerClickMode::from_modifiers(
                                ev.modifiers.shift,
                                ev.modifiers.platform,
                            );
                            this.select_layer_with_mode(lid, mode, cx);
                            // Header drag reorders the stack; committed on
                            // mouse-up. A modified click is building a
                            // selection, not moving a layer.
                            if !mode.is_additive() {
                                this.drag = TimelineDrag::Reorder {
                                    layer: lid,
                                    changed: false,
                                };
                            }
                        }),
                    )
                    // Expand arrow
                    .child(
                        div()
                            .id(SharedString::from(format!("exp-{}", lid)))
                            .cursor_pointer()
                            .child(
                                Icon::new(expand_arrow)
                                    .size_3()
                                    .text_color(theme.colors.muted_foreground),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _win, cx| {
                                    // Expanding a row is not selecting it (and
                                    // must not start a reorder drag).
                                    cx.stop_propagation();
                                    this.state.toggle_layer_expanded(lid);
                                    cx.notify();
                                }),
                            ),
                    )
                    // Layer name. `min_w_0` allows the shrink that `truncate`
                    // needs, so a long name ellipsizes on one line instead of
                    // wrapping past the fixed row height and pushing the
                    // S/M/L toggles out of view.
                    .child(
                        div()
                            .flex_grow()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme.colors.foreground)
                            .child(SharedString::from(name.clone())),
                    )
                    // S/M/L toggle buttons
                    .child(
                        make_toggle(
                            format!("s-{lid}"),
                            "S",
                            *solo,
                            SharedString::from(t!("timeline.toggle.solo")),
                            &theme.colors,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                // The flag toggle applies to the whole selection
                                // when this row is part of it, so the row's own
                                // click must not collapse the selection first.
                                cx.stop_propagation();
                                this.toggle_solo(lid, cx);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        make_toggle(
                            format!("m-{lid}"),
                            "M",
                            *muted,
                            SharedString::from(t!("timeline.toggle.mute")),
                            &theme.colors,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                // The flag toggle applies to the whole selection
                                // when this row is part of it, so the row's own
                                // click must not collapse the selection first.
                                cx.stop_propagation();
                                this.toggle_mute(lid, cx);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        make_toggle(
                            format!("l-{lid}"),
                            "L",
                            *locked,
                            SharedString::from(t!("timeline.toggle.lock")),
                            &theme.colors,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                // The flag toggle applies to the whole selection
                                // when this row is part of it, so the row's own
                                // click must not collapse the selection first.
                                cx.stop_propagation();
                                this.toggle_lock(lid, cx);
                                cx.notify();
                            }),
                        ),
                    ),
            );

            // Property expansion sub-rows
            if is_expanded {
                for (j, row) in layer_rows[i].iter().enumerate() {
                    let is_prop_expanded = self.state.is_property_expanded(lid, &row.id);
                    let arrow = if is_prop_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    };
                    // Shell group labels come from the locale; network rows
                    // carry a data-derived label ("node · key", or the bare
                    // key for the In node's custom parameters).
                    let label: SharedString = match &row.id {
                        PropertyRowId::Shell(group) => shell_group_label(*group),
                        PropertyRowId::Network { .. } => {
                            SharedString::from(row.label.clone().unwrap_or_default())
                        }
                    };
                    let row_id = row.id.clone();
                    let keyed = self.row_keyed_at_playhead(lid, &row.id);
                    let (diamond_icon, diamond_color) = if keyed {
                        (RavelIcon::DiamondFilled, theme.colors.primary)
                    } else {
                        (
                            RavelIcon::Diamond,
                            Hsla {
                                a: 0.5,
                                ..theme.colors.muted_foreground
                            },
                        )
                    };
                    let prev_row = row.id.clone();
                    let toggle_row = row.id.clone();
                    let next_row = row.id.clone();

                    headers = headers.child(
                        div()
                            .id(SharedString::from(format!("prop-{lid}-{j}")))
                            .h(px(PROPERTY_ROW_HEIGHT))
                            .flex()
                            .items_center()
                            .pl(px(20.0))
                            .bg(theme.colors.list)
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _win, cx| {
                                    this.state.toggle_property_expanded(lid, row_id.clone());
                                    cx.notify();
                                }),
                            )
                            .child(
                                div().mr_1().child(
                                    Icon::new(arrow)
                                        .size_3()
                                        .text_color(theme.colors.muted_foreground),
                                ),
                            )
                            // Keyframe navigator: ◀ jump back, ◆ toggle at
                            // the playhead, ▶ jump forward. The buttons stop
                            // propagation so the row's expand toggle stays
                            // untouched.
                            .child(
                                nav_button(
                                    format!("kf-prev-{lid}-{j}"),
                                    Icon::new(IconName::ChevronLeft)
                                        .size_3()
                                        .text_color(theme.colors.muted_foreground),
                                    SharedString::from(t!("timeline.navigator.prev")),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _win, cx| {
                                        cx.stop_propagation();
                                        this.jump_to_prev_keyframe(lid, &prev_row, cx);
                                    }),
                                ),
                            )
                            .child(
                                nav_button(
                                    format!("kf-toggle-{lid}-{j}"),
                                    Icon::new(diamond_icon).size_3().text_color(diamond_color),
                                    SharedString::from(t!("timeline.navigator.toggle")),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _win, cx| {
                                        cx.stop_propagation();
                                        this.toggle_row_keyframe(lid, &toggle_row, cx);
                                    }),
                                ),
                            )
                            .child(
                                nav_button(
                                    format!("kf-next-{lid}-{j}"),
                                    Icon::new(IconName::ChevronRight)
                                        .size_3()
                                        .text_color(theme.colors.muted_foreground),
                                    SharedString::from(t!("timeline.navigator.next")),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, _win, cx| {
                                        cx.stop_propagation();
                                        this.jump_to_next_keyframe(lid, &next_row, cx);
                                    }),
                                ),
                            )
                            // A network row's label is data-derived
                            // ("node · key") and can outrun the header width;
                            // ellipsize it rather than wrap past the row.
                            .child(
                                div()
                                    .ml_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.colors.muted_foreground)
                                    .child(label),
                            ),
                    );

                    if is_prop_expanded {
                        for (ci, ch_name) in row.channel_names.iter().enumerate() {
                            let channel = TimelineChannelRef {
                                layer: lid,
                                row: row.id.clone(),
                                component: ci,
                            };
                            let is_selected = self.state.is_channel_selected(&channel);
                            headers = headers.child(
                                div()
                                    .id(SharedString::from(format!("ch-{lid}-{j}-{ci}")))
                                    .h(px(PROPERTY_ROW_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .pl(px(36.0))
                                    .bg(if is_selected {
                                        theme.colors.list_active
                                    } else {
                                        theme.colors.list
                                    })
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, ev: &MouseDownEvent, _win, cx| {
                                            this.state.select_channel(
                                                channel.clone(),
                                                ev.modifiers.shift,
                                            );
                                            cx.notify();
                                        }),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(Hsla {
                                                a: 0.6,
                                                ..theme.colors.muted_foreground
                                            })
                                            .child(SharedString::from(ch_name.clone())),
                                    ),
                            );
                        }
                    }
                }
            }
        }

        headers
    }
}

impl Focusable for TimelineGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TimelineGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Composition 0 (REQ-UI-013): there is no ruler, no stack and no
        // transport range to draw, so the panel says so instead of showing
        // an empty timeline that looks broken.
        if self.state.comp_id().is_none() {
            return div()
                .id("timeline-root")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .border_t_1()
                .border_color(theme.colors.border)
                .text_xs()
                .text_color(theme.colors.muted_foreground)
                .track_focus(&self.focus_handle)
                .key_context(KEY_CONTEXT)
                .child(SharedString::from(t!("timeline.empty.no_composition")))
                .into_any_element();
        }
        let content_height = self.total_layer_height();
        let is_playing = cx
            .try_global::<crate::playback::PlaybackControllerHandle>()
            .and_then(|handle| handle.0.upgrade())
            .is_some_and(|controller| controller.read(cx).transport().is_playing());
        let transport_toolbar = self.build_transport_toolbar(is_playing, cx);
        let ruler = self.build_ruler(&theme.colors);
        let view_mode = self.state.view_mode();
        let right_pane = match view_mode {
            TimelineViewMode::Bars => self
                .build_layer_area(&theme.colors, self.area_origin.clone(), cx)
                .into_any_element(),
            TimelineViewMode::Graph => self
                .build_curve_editor_shell(&theme.colors, self.area_origin.clone(), cx)
                .into_any_element(),
        };
        let layer_headers = self.build_layer_headers(cx);
        let entity = cx.entity().clone();
        let menu_state = self.state.clone();
        let menu_selection = self.selected_keyframes.clone();
        let last_right_click = self.last_right_click.clone();
        let menu_area_origin = self.area_origin.clone();

        div()
            .id("timeline-root")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.colors.border)
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_keyframe_bezier))
            .on_action(cx.listener(Self::on_keyframe_linear))
            .on_action(cx.listener(Self::on_keyframe_step))
            .on_action(
                cx.listener(|this, _: &gpui_component::input::Escape, _window, cx| {
                    if this.timecode_input.is_some() {
                        this.cancel_timecode_edit(cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if matches!(this.drag, TimelineDrag::None) {
                    if this.state.view_mode() == TimelineViewMode::Bars {
                        let x: f32 = event.position.x.into();
                        let y: f32 = event.position.y.into();
                        let (origin_x, origin_y) = this.area_origin.get();
                        let hint = if x >= origin_x && y >= origin_y {
                            this.pointer_hint_at((x - origin_x) as f64, y - origin_y)
                        } else {
                            PointerHint::Arrow
                        };
                        this.update_pointer_hint(hint, cx);
                    }
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left) {
                    this.cancel_drag(cx);
                    return;
                }
                let x: f32 = event.position.x.into();
                let y: f32 = event.position.y.into();
                this.drag_moved(x, y, event.modifiers.shift, event.modifiers.alt, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.drag_ended(cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                if event.modifiers.platform || event.modifiers.control {
                    let dy: f32 = delta.y.into();
                    let factor = if dy > 0.0 { 1.2 } else { 1.0 / 1.2 };
                    let cursor_x: f32 = event.position.x.into();
                    this.state
                        .zoom_at(cursor_x as f64 - HEADER_WIDTH as f64, factor);
                    this.sync_zoom_slider(window, cx);
                } else {
                    let dx: f32 = delta.x.into();
                    let frame_delta = dx as f64 / this.state.pixels_per_frame();
                    let new_offset = this.state.scroll_offset() - frame_delta;
                    this.state.set_scroll_offset(new_offset);
                }
                cx.notify();
            }))
            .child(transport_toolbar)
            .child(
                div()
                    .id("ruler-row")
                    .flex()
                    .flex_row()
                    .h(px(RULER_HEIGHT))
                    .child(
                        div()
                            .w(px(HEADER_WIDTH))
                            .h(px(RULER_HEIGHT))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_end()
                            .px_1()
                            .bg(theme.colors.tab_bar)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(
                                Button::new("timeline-bar-view")
                                    .xsmall()
                                    .ghost()
                                    .selected(view_mode == TimelineViewMode::Bars)
                                    .icon(Icon::new(RavelIcon::TimelineBars))
                                    .tooltip(t!("timeline.toggle.bar_view"))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.state.set_view_mode(TimelineViewMode::Bars);
                                        this.pointer_hint = PointerHint::Lane;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("timeline-graph-view")
                                    .xsmall()
                                    .ghost()
                                    .selected(view_mode == TimelineViewMode::Graph)
                                    .icon(Icon::new(RavelIcon::CurveEditor))
                                    .tooltip(t!("timeline.toggle.graph_view"))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.state.set_view_mode(TimelineViewMode::Graph);
                                        this.pointer_hint = PointerHint::Lane;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                make_toggle(
                                    "follow-playhead".to_string(),
                                    "F",
                                    self.state.follow_playhead(),
                                    SharedString::from(t!("timeline.toggle.follow_playhead")),
                                    &theme.colors,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _ev, _win, cx| {
                                        this.state.toggle_follow_playhead();
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                    .child(
                        // The scrub mousedown lives on the ruler area only;
                        // on the whole row it would also fire for
                        // header-corner clicks (timecode, follow toggle) and
                        // yank the playhead to the first visible frame. The
                        // started drag then tracks on `timeline-root`, so
                        // the pointer may leave the ruler mid-scrub.
                        div()
                            .id("ruler-scrub")
                            .flex_grow()
                            .h_full()
                            .child(ruler)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                    let click_x: f32 = event.position.x.into();
                                    let local_x =
                                        (click_x - this.ruler_origin_x.get()).max(0.0) as f64;
                                    let frame = this.state.x_to_frame(local_x);
                                    this.scrub_playhead(frame, cx);
                                    this.drag = TimelineDrag::Scrub;
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("layer-scroll-area")
                    .flex_grow()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .h_full()
                            .min_h(px(content_height))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener({
                                    let area_origin = menu_area_origin;
                                    move |this, event: &MouseDownEvent, _window, cx| {
                                        if this.state.view_mode() == TimelineViewMode::Graph {
                                            return;
                                        }
                                        let click_x: f32 = event.position.x.into();
                                        let click_y: f32 = event.position.y.into();
                                        let (origin_x, origin_y) = area_origin.get();
                                        let local = (click_x - origin_x, click_y - origin_y);
                                        this.last_right_click.set(local);
                                        let layer = if local.0 < 0.0 {
                                            this.layer_at_content_y(local.1)
                                        } else {
                                            this.bar_hit(local.0 as f64, local.1)
                                                .map(|(layer, _)| layer)
                                        };
                                        if let Some(layer) = layer {
                                            this.selected_keyframes.clear();
                                            this.select_layer_for_menu(layer, cx);
                                        }
                                    }
                                }),
                            )
                            .context_menu({
                                let entity = entity.clone();
                                move |menu, window, cx| {
                                    let (content_x, content_y) = last_right_click.get();
                                    let row_hit = (view_mode == TimelineViewMode::Bars)
                                        .then(|| {
                                            TimelineGpuiPanel::row_at_content_y_in(
                                                &menu_state,
                                                content_y,
                                            )
                                        })
                                        .flatten();
                                    let layer_hit = if view_mode == TimelineViewMode::Graph {
                                        None
                                    } else if content_x < 0.0 {
                                        match &row_hit {
                                            Some(RowHit::LayerBar(layer)) => Some(*layer),
                                            _ => None,
                                        }
                                    } else {
                                        TimelineGpuiPanel::bar_hit_in(
                                            &menu_state,
                                            content_x as f64,
                                            content_y,
                                        )
                                        .map(|(layer, _)| layer)
                                    };
                                    let mut menu = menu;

                                    if let Some(layer_id) = layer_hit {
                                        if let Some(layer) = menu_state.layer(layer_id) {
                                            let duplicate_entity = entity.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.duplicate_layer"
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    duplicate_entity.update(cx, |this, cx| {
                                                        this.duplicate_layers_from_row(
                                                            layer_id, cx,
                                                        );
                                                    });
                                                }),
                                            );

                                            let delete_entity = entity.clone();
                                            // Delete takes the whole selection
                                            // when this row is part of it, so it
                                            // is only unavailable when every
                                            // target is locked.
                                            let all_locked = entity
                                                .read(cx)
                                                .operation_targets(layer_id, cx)
                                                .iter()
                                                .all(|target| {
                                                    menu_state
                                                        .layer(*target)
                                                        .is_none_or(|layer| layer.locked)
                                                });
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.delete_layer"
                                                ))
                                                .disabled(all_locked)
                                                .on_click(move |_, _window, cx| {
                                                    delete_entity.update(cx, |this, cx| {
                                                        this.delete_layer(layer_id, cx);
                                                    });
                                                }),
                                            );

                                            let solo_entity = entity.clone();
                                            let mute_entity = entity.clone();
                                            let lock_entity = entity.clone();
                                            menu = menu
                                                .separator()
                                                .item(
                                                    PopupMenuItem::new(t!("timeline.menu.solo"))
                                                        .checked(layer.solo)
                                                        .on_click(move |_, _window, cx| {
                                                            solo_entity.update(cx, |this, cx| {
                                                                this.toggle_solo(layer_id, cx);
                                                            });
                                                        }),
                                                )
                                                .item(
                                                    PopupMenuItem::new(t!("timeline.menu.mute"))
                                                        .checked(layer.muted)
                                                        .on_click(move |_, _window, cx| {
                                                            mute_entity.update(cx, |this, cx| {
                                                                this.toggle_mute(layer_id, cx);
                                                            });
                                                        }),
                                                )
                                                .item(
                                                    PopupMenuItem::new(t!("timeline.menu.lock"))
                                                        .checked(layer.locked)
                                                        .on_click(move |_, _window, cx| {
                                                            lock_entity.update(cx, |this, cx| {
                                                                this.toggle_lock(layer_id, cx);
                                                            });
                                                        }),
                                                );
                                        }
                                    } else if let Some(RowHit::Channel(layer, row, component)) =
                                        row_hit
                                    {
                                        if let Some(frame) =
                                            TimelineGpuiPanel::keyframe_at_content_x_in(
                                                &menu_state,
                                                layer,
                                                &row,
                                                component,
                                                content_x as f64,
                                            )
                                        {
                                            let clicked = KeyframeRef {
                                                layer,
                                                row,
                                                component,
                                                frame,
                                            };
                                            let delete_entity = entity.clone();
                                            let delete_selection =
                                                menu_selection.contains(&clicked);
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.delete_keyframe"
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    delete_entity.update(cx, |this, cx| {
                                                        if delete_selection {
                                                            this.delete_selected_keyframes(cx);
                                                        } else {
                                                            this.delete_keyframe_from_menu(
                                                                clicked.clone(),
                                                                cx,
                                                            );
                                                        }
                                                    });
                                                }),
                                            );
                                        } else {
                                            let add_entity = entity.clone();
                                            let comp_frame =
                                                menu_state.x_to_frame(content_x as f64);
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.add_keyframe"
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    add_entity.update(cx, |this, cx| {
                                                        this.add_keyframe_at(
                                                            layer,
                                                            row.clone(),
                                                            component,
                                                            comp_frame,
                                                            cx,
                                                        );
                                                    });
                                                }),
                                            );
                                        }
                                    }

                                    if view_mode == TimelineViewMode::Graph {
                                        let live_selection =
                                            entity.read(cx).selected_keyframes.clone();
                                        let live_interpolation =
                                            entity.read(cx).selected_interpolation();
                                        if !live_selection.is_empty() {
                                            menu = menu
                                                .item(
                                                    PopupMenuItem::new(t!(
                                                        "timeline.menu.delete_selected_keyframes"
                                                    ))
                                                    .on_click(|_, window, cx| {
                                                        window.dispatch_action(
                                                            Box::new(EditDelete),
                                                            cx,
                                                        );
                                                    }),
                                                )
                                                .submenu(
                                                    t!("timeline.menu.interpolation"),
                                                    window,
                                                    cx,
                                                    move |sub, _window, _cx| {
                                                        sub.item(
                                                            PopupMenuItem::new(t!(
                                                                "timeline.interpolation.bezier"
                                                            ))
                                                            .checked(
                                                                live_interpolation
                                                                    == Some(Interpolation::Bezier),
                                                            )
                                                            .on_click(|_, window, cx| {
                                                                window.dispatch_action(
                                                                    Box::new(
                                                                        KeyframeInterpolationBezier,
                                                                    ),
                                                                    cx,
                                                                );
                                                            }),
                                                        )
                                                        .item(
                                                            PopupMenuItem::new(t!(
                                                                "timeline.interpolation.linear"
                                                            ))
                                                            .checked(
                                                                live_interpolation
                                                                    == Some(Interpolation::Linear),
                                                            )
                                                            .on_click(|_, window, cx| {
                                                                window.dispatch_action(
                                                                    Box::new(
                                                                        KeyframeInterpolationLinear,
                                                                    ),
                                                                    cx,
                                                                );
                                                            }),
                                                        )
                                                        .item(
                                                            PopupMenuItem::new(t!(
                                                                "timeline.interpolation.step"
                                                            ))
                                                            .checked(
                                                                live_interpolation
                                                                    == Some(Interpolation::Step),
                                                            )
                                                            .on_click(|_, window, cx| {
                                                                window.dispatch_action(
                                                                    Box::new(
                                                                        KeyframeInterpolationStep,
                                                                    ),
                                                                    cx,
                                                                );
                                                            }),
                                                        )
                                                    },
                                                );
                                        }

                                        if let Some(channel) =
                                            menu_state.selected_channels().first().cloned()
                                        {
                                            let add_entity = entity.clone();
                                            let comp_frame =
                                                menu_state.x_to_frame(content_x as f64);
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.add_keyframe"
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    add_entity.update(cx, |this, cx| {
                                                        this.add_keyframe_at(
                                                            channel.layer,
                                                            channel.row.clone(),
                                                            channel.component,
                                                            comp_frame,
                                                            cx,
                                                        );
                                                    });
                                                }),
                                            );
                                        }

                                        let select_entity = entity.clone();
                                        let fit_entity = entity.clone();
                                        let grid_entity = entity.clone();
                                        let grid_visible = entity.read(cx).show_curve_grid;
                                        menu = menu
                                            .item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.select_all_keyframes"
                                                ))
                                                .disabled(menu_state.selected_channels().is_empty())
                                                .on_click(move |_, _window, cx| {
                                                    select_entity.update(cx, |this, cx| {
                                                        this.select_all_displayed_keyframes(cx);
                                                    });
                                                }),
                                            )
                                            .separator()
                                            .item(
                                                PopupMenuItem::new(t!("timeline.graph.fit_values"))
                                                    .on_click(move |_, _window, cx| {
                                                        fit_entity.update(cx, |this, cx| {
                                                            this.fit_curve_values(cx);
                                                        });
                                                    }),
                                            )
                                            .item(
                                                PopupMenuItem::new(t!("timeline.graph.grid"))
                                                    .checked(grid_visible)
                                                    .on_click(move |_, _window, cx| {
                                                        grid_entity.update(cx, |this, cx| {
                                                            this.toggle_curve_grid(cx);
                                                        });
                                                    }),
                                            );
                                    }

                                    let add_layer_entity = entity.clone();
                                    menu.separator().submenu(
                                        t!("timeline.menu.add_layer"),
                                        window,
                                        cx,
                                        move |sub, _window, _cx| {
                                            [
                                                CommandId::LayerAddSolid,
                                                CommandId::LayerAddShape,
                                                CommandId::LayerAddVideo,
                                                CommandId::LayerAddAudio,
                                                CommandId::LayerAddNull,
                                            ]
                                            .into_iter()
                                            .fold(
                                                sub,
                                                |sub, command| {
                                                    let entity = add_layer_entity.clone();
                                                    let template_key = command
                                                        .layer_template_key()
                                                        .expect("builtin layer command");
                                                    sub.item(
                                                        PopupMenuItem::new(t!(command.label_key()))
                                                            .on_click(move |_, _window, cx| {
                                                                entity.update(cx, |this, cx| {
                                                                    this.add_layer_from_template(
                                                                        template_key,
                                                                        cx,
                                                                    );
                                                                });
                                                            }),
                                                    )
                                                },
                                            )
                                        },
                                    )
                                }
                            })
                            .child(layer_headers.min_h(px(content_height)))
                            .child(
                                div()
                                    .id("layer-area-click")
                                    .flex_grow()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let area_origin = self.area_origin.clone();
                                            move |this, event: &MouseDownEvent, _win, cx| {
                                                if this.state.view_mode() == TimelineViewMode::Graph
                                                {
                                                    return;
                                                }
                                                let click_x: f32 = event.position.x.into();
                                                let click_y: f32 = event.position.y.into();
                                                let (origin_x, origin_y) = area_origin.get();
                                                let content_x = (click_x - origin_x) as f64;
                                                let content_y = click_y - origin_y;
                                                match this.row_at_content_y(content_y) {
                                                    Some(RowHit::LayerBar(lid)) => {
                                                        // Bar clicks leave
                                                        // keyframe editing: drop
                                                        // the selection so Delete
                                                        // keeps targeting layers.
                                                        this.selected_keyframes.clear();
                                                        let mode = LayerClickMode::from_modifiers(
                                                            event.modifiers.shift,
                                                            event.modifiers.platform,
                                                        );
                                                        this.select_layer_with_mode(lid, mode, cx);
                                                        // A modified click builds
                                                        // a selection; it must not
                                                        // also move or trim the
                                                        // bar it landed on.
                                                        if mode.is_additive() {
                                                            return;
                                                        }
                                                        let locked = this
                                                            .state
                                                            .layer(lid)
                                                            .is_none_or(|l| l.locked);
                                                        if locked {
                                                            return;
                                                        }
                                                        let Some((lid, zone)) =
                                                            this.bar_hit(content_x, content_y)
                                                        else {
                                                            return;
                                                        };
                                                        let Some(layer) = this.state.layer(lid)
                                                        else {
                                                            return;
                                                        };
                                                        this.drag = match zone {
                                                            BarZone::Body => {
                                                                TimelineDrag::MoveBar {
                                                                    layer: lid,
                                                                    origin_start: layer.start_frame,
                                                                    grab_x: click_x,
                                                                    changed: false,
                                                                }
                                                            }
                                                            BarZone::InEdge => {
                                                                TimelineDrag::TrimIn {
                                                                    layer: lid,
                                                                    origin_start: layer.start_frame,
                                                                    origin_in: layer.in_frame,
                                                                    origin_out: layer.out_frame,
                                                                    grab_x: click_x,
                                                                    changed: false,
                                                                }
                                                            }
                                                            BarZone::OutEdge => {
                                                                TimelineDrag::TrimOut {
                                                                    layer: lid,
                                                                    origin_in: layer.in_frame,
                                                                    origin_out: layer.out_frame,
                                                                    grab_x: click_x,
                                                                    changed: false,
                                                                }
                                                            }
                                                        };
                                                    }
                                                    Some(RowHit::PropertyGroup(lid, row)) => {
                                                        this.state
                                                            .toggle_property_expanded(lid, row);
                                                        cx.notify();
                                                    }
                                                    Some(RowHit::Channel(lid, row, component)) => {
                                                        this.channel_row_mouse_down(
                                                            lid,
                                                            row,
                                                            component,
                                                            content_x,
                                                            event.click_count,
                                                            click_x,
                                                            click_y,
                                                            event.modifiers.shift,
                                                            cx,
                                                        );
                                                    }
                                                    None => this.deselect_layer(cx),
                                                }
                                            }
                                        }),
                                    )
                                    .child(right_pane),
                            ),
                    ),
            )
            .into_any_element()
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// A 14px icon button for the per-row keyframe navigator.
fn nav_button(id: String, icon: Icon, tooltip: SharedString) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .w(px(14.0))
        .h(px(PROPERTY_ROW_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(icon)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

fn graph_hit_at(
    curves: &[TimelineCurveData],
    scroll: f64,
    pixels_per_frame: f64,
    value_bounds: (f64, f64),
    size: (f32, f32),
    pointer: CurvePoint,
) -> Option<CurveHit> {
    if curves.is_empty() || size.0 <= 0.0 || size.1 <= 0.0 || pixels_per_frame <= 0.0 {
        return None;
    }
    let transform = graph_transform(scroll, pixels_per_frame, value_bounds, size)?;
    let sources: Vec<_> = curves
        .iter()
        .map(|curve| CurveSource {
            curve: &curve.curve,
            frame_offset: curve.frame_offset,
        })
        .collect();
    hit_test_with_offsets(&sources, transform, pointer, CURVE_HIT_RADIUS)
}

fn graph_transform(
    scroll: f64,
    pixels_per_frame: f64,
    value_bounds: (f64, f64),
    size: (f32, f32),
) -> Option<CurveTransform> {
    if size.0 <= 0.0 || size.1 <= 0.0 || pixels_per_frame <= 0.0 {
        return None;
    }
    Some(CurveTransform::new(
        CurvePoint::new(scroll, value_bounds.0),
        CurvePoint::new(scroll + size.0 as f64 / pixels_per_frame, value_bounds.1),
        CurvePoint::new(size.0 as f64, size.1 as f64),
    ))
}

fn curve_grid_canvas(
    state: TimelinePanel,
    value_bounds: (f64, f64),
    colors: ThemeColor,
    visible: bool,
) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, cx| {
            if !visible {
                return;
            }
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            if width <= 0.0 || height <= 0.0 {
                return;
            }

            let ppf = state.pixels_per_frame();
            let scroll = state.scroll_offset();
            let (minor_frames, major_frames) = tick_intervals(ppf, state.frame_rate());
            if minor_frames > 0 && major_frames > 0 {
                let first = scroll.floor().max(0.0) as u64;
                let last = first.saturating_add((width as f64 / ppf).ceil() as u64 + 1);
                let start = (first / minor_frames) * minor_frames;
                for frame in (start..=last).step_by(minor_frames as usize) {
                    let x = (frame as f64 - scroll) * ppf;
                    if x < 0.0 || x > width as f64 {
                        continue;
                    }
                    let major = frame % major_frames == 0;
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.origin.x + px(x as f32), bounds.origin.y),
                            size(px(1.0), bounds.size.height),
                        ),
                        Hsla {
                            a: if major { 0.18 } else { 0.07 },
                            ..colors.foreground
                        },
                    ));
                }
            }

            for value in value_grid_values(value_bounds.0, value_bounds.1, height as f64) {
                let normalized = (value_bounds.1 - value) / (value_bounds.1 - value_bounds.0);
                let y = bounds.origin.y + px((normalized * height as f64) as f32);
                let is_zero = value.abs() < f64::EPSILON;
                window.paint_quad(fill(
                    Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, px(1.0))),
                    Hsla {
                        a: if is_zero { 0.32 } else { 0.12 },
                        ..colors.foreground
                    },
                ));

                let label = SharedString::from(format_value_label(value));
                let label_len = label.len();
                let label_width = px(48.0);
                let label_height = px(14.0);
                let label_y = (y - px(7.0)).max(bounds.origin.y);
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x + px(2.0), label_y),
                        size(label_width, label_height),
                    ),
                    Hsla {
                        a: 0.82,
                        ..colors.background
                    },
                ));
                let shaped = window.text_system().shape_line(
                    label,
                    px(10.0),
                    &[TextRun {
                        len: label_len,
                        font: Font {
                            family: SharedString::from("sans-serif"),
                            ..Default::default()
                        },
                        color: colors.muted_foreground,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    Some(label_width),
                );
                let _ = shaped.paint(
                    point(bounds.origin.x + px(5.0), label_y),
                    label_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        },
    )
    .size_full()
}

fn selected_timeline_curves(state: &TimelinePanel, colors: &ThemeColor) -> Vec<TimelineCurveData> {
    let mut series = Vec::new();
    for selected in state.selected_channels() {
        let Some(layer) = state.layer(selected.layer) else {
            continue;
        };
        let Some(channels) = keyframes::row_channels(layer, &selected.row) else {
            continue;
        };
        let Some(channel) = channels.get(selected.component) else {
            continue;
        };
        let ChannelSource::Keyframes(curve) = &channel.source else {
            continue;
        };
        if curve.is_empty() {
            continue;
        }
        let color = match series.len() % 5 {
            0 => colors.chart_1,
            1 => colors.chart_2,
            2 => colors.chart_3,
            3 => colors.chart_4,
            _ => colors.chart_5,
        };
        series.push(TimelineCurveData {
            channel: selected.clone(),
            curve: Arc::new(curve.clone()),
            frame_offset: layer
                .start_frame
                .saturating_sub(i64::try_from(layer.in_frame).unwrap_or(i64::MAX)),
            color,
        });
    }
    series
}

/// Conservative auto-fit bounds for all displayed curves. A Bézier segment
/// lies inside the convex hull of its four control values, so including the
/// active tangent control values guarantees that evaluated curves are not
/// clipped without sampling unbounded frame ranges on the UI thread.
fn curve_value_bounds(series: &[TimelineCurveData]) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut include = |value: f64| {
        if value.is_finite() {
            min = min.min(value);
            max = max.max(value);
        }
    };

    for item in series {
        let keys = item.curve.keyframes();
        for key in keys {
            include(key.value as f64);
        }
        for pair in keys.windows(2) {
            let [left, right] = pair else {
                continue;
            };
            if left.interpolation == ravel_core::animation::Interpolation::Bezier {
                include((left.value + left.tangent_out.1) as f64);
                include((right.value + right.tangent_in.1) as f64);
            }
        }
    }

    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    Some(curve_view::padded_bounds(min, max))
}

fn ppf_to_slider(ppf: f64) -> f32 {
    ((ppf.clamp(MIN_PPF, MAX_PPF) / MIN_PPF).ln() / (MAX_PPF / MIN_PPF).ln()) as f32
}

fn slider_to_ppf(value: f32) -> f64 {
    MIN_PPF * (MAX_PPF / MIN_PPF).powf(value.clamp(0.0, 1.0) as f64)
}

fn fit_pixels_per_frame(ruler_width: f64, duration_frames: u64) -> f64 {
    (ruler_width.max(0.0) / duration_frames.max(1) as f64).clamp(MIN_PPF, MAX_PPF)
}

fn format_fps(frame_rate: FrameRate) -> String {
    let fps = frame_rate.as_f64();
    if (fps - fps.round()).abs() < 0.000_5 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.3}")
    }
}

fn parse_frame_entry(input: &str, frame_rate: FrameRate, duration_frames: u64) -> Option<u64> {
    let input = input.trim();
    let max_frame = duration_frames.saturating_sub(1);
    if !input.contains(':') {
        let frame = input.parse::<i128>().ok()?;
        return Some(frame.clamp(0, max_frame as i128) as u64);
    }

    let parts: Vec<_> = input.split(':').collect();
    let nominal = frame_rate.as_f64().round().max(1.0) as u64;
    let (hours, minutes, seconds, frames) = match parts.as_slice() {
        [minutes, seconds, frames] => (
            0,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
            frames.parse::<u64>().ok()?,
        ),
        [hours, minutes, seconds, frames] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
            frames.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    if seconds >= 60 || frames >= nominal || (parts.len() == 4 && minutes >= 60) {
        return None;
    }
    let total_seconds = hours
        .checked_mul(60)?
        .checked_add(minutes)?
        .checked_mul(60)?
        .checked_add(seconds)?;
    let frame = total_seconds.checked_mul(nominal)?.checked_add(frames)?;
    Some(frame.min(max_frame))
}

fn make_toggle(
    id: String,
    label: &str,
    active: bool,
    tooltip: SharedString,
    colors: &ThemeColor,
) -> Stateful<Div> {
    let text_color = if active {
        colors.primary
    } else {
        Hsla {
            a: 0.4,
            ..colors.muted_foreground
        }
    };
    div()
        .id(SharedString::from(id))
        .w(px(TOGGLE_BUTTON_SIZE))
        .h(px(TOGGLE_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(text_color)
        .cursor_pointer()
        .child(SharedString::from(label))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

fn paint_bar_label(
    text: &str,
    x: Pixels,
    y: Pixels,
    max_h: Pixels,
    colors: &ThemeColor,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = text.into();
    let text_len = text.len();
    let font = Font {
        family: SharedString::from("sans-serif"),
        ..Default::default()
    };
    let shaped = window.text_system().shape_line(
        text,
        px(11.0),
        &[TextRun {
            len: text_len,
            font,
            color: colors.accent_foreground,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    shaped
        .paint(point(x, y), max_h, TextAlign::Left, None, window, cx)
        .ok();
}

/// Paints a keyframe marker as a real diamond (rotated square), matching
/// the lucide diamond icon used by the Properties keyframe toggle.
fn paint_diamond(cx_pos: Pixels, cy: Pixels, color: Hsla, window: &mut Window) {
    let half = px(DIAMOND_SIZE / 2.0);
    let mut builder = PathBuilder::fill();
    builder.move_to(point(cx_pos, cy - half));
    builder.line_to(point(cx_pos + half, cy));
    builder.line_to(point(cx_pos, cy + half));
    builder.line_to(point(cx_pos - half, cy));
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Localized label of a shell property group. `AnchorPoint` is not part of
/// `keyframes::SHELL_GROUPS`, so it never reaches the tree.
fn shell_group_label(group: PropertyGroup) -> SharedString {
    match group {
        PropertyGroup::Position => SharedString::from(t!("timeline.property.position")),
        PropertyGroup::Scale => SharedString::from(t!("timeline.property.scale")),
        PropertyGroup::Rotation => SharedString::from(t!("timeline.property.rotation")),
        PropertyGroup::Opacity => SharedString::from(t!("timeline.property.opacity")),
        PropertyGroup::AudioGain => SharedString::from(t!("timeline.property.gain")),
        PropertyGroup::AnchorPoint => SharedString::default(),
    }
}

fn layer_color(layer: &Layer, colors: &ThemeColor) -> Hsla {
    // Layer "kinds" are creation templates; at runtime a layer is its
    // network. Layers without a frame output (null layers) render muted.
    if layer.has_frame_output() {
        Hsla {
            a: 0.8,
            ..colors.accent
        }
    } else {
        Hsla {
            a: 0.3,
            ..colors.muted_foreground
        }
    }
}

fn tick_intervals(ppf: f64, fr: FrameRate) -> (u64, u64) {
    let fps = fr.as_f64();
    if ppf >= 10.0 {
        (1, 5.max(fps as u64))
    } else if ppf >= 4.0 {
        (5.max(fps as u64 / 6), fps.ceil() as u64)
    } else if ppf >= 1.0 {
        (fps.ceil() as u64, (fps * 10.0).ceil() as u64)
    } else {
        ((fps * 10.0).ceil() as u64, (fps * 60.0).ceil() as u64)
    }
}

/// Fixed-layout `HH:MM:SS:FF` timecode for the header readout.
fn format_timecode(frame: u64, fr: FrameRate) -> String {
    // Non-drop-frame timecode over the nominal integer rate: every second
    // holds exactly `nominal` frames, so the readout is continuous and
    // monotonic. Mixing wall-clock seconds with a frame modulo would jump
    // backwards around minute boundaries at fractional rates like 23.976
    // (nominal timecode intentionally drifts from wall time there).
    let nominal = fr.as_f64().round().max(1.0) as u64;
    let total_seconds = frame / nominal;
    let frames = frame % nominal;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}:{frames:02}")
}

fn format_frame_label(frame: u64, fr: FrameRate) -> String {
    let fps = fr.as_f64();
    let total_seconds = frame as f64 / fps;
    let minutes = (total_seconds / 60.0).floor() as u64;
    let seconds = (total_seconds % 60.0).floor() as u64;
    let remaining_frames = frame % fps.ceil() as u64;
    if minutes > 0 {
        format!("{minutes}:{seconds:02}:{remaining_frames:02}")
    } else {
        format!("{seconds}:{remaining_frames:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one so `#[gpui::test]` and `#[test]` resolve to the
    // real ones.
    use core::prelude::v1::test;
    use gpui::TestAppContext;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::graph::Graph;
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::network as net;
    use ravel_ui::document::NetworkPath;

    #[test]
    fn pointer_hint_changes_only_at_boundaries_and_never_during_drag() {
        assert_eq!(
            pointer_hint_transition(PointerHint::Arrow, PointerHint::BarBody, false),
            Some(PointerHint::BarBody)
        );
        assert_eq!(
            pointer_hint_transition(PointerHint::BarBody, PointerHint::BarBody, false),
            None
        );
        assert_eq!(
            pointer_hint_transition(PointerHint::BarBody, PointerHint::Trim, true),
            None
        );
    }

    #[test]
    fn bar_zones_and_locked_bars_map_to_expected_cursors() {
        assert_eq!(
            bar_pointer_hint(Some(BarZone::Body), false),
            PointerHint::BarBody
        );
        assert_eq!(
            bar_pointer_hint(Some(BarZone::InEdge), false),
            PointerHint::Trim
        );
        assert_eq!(
            bar_pointer_hint(Some(BarZone::OutEdge), false),
            PointerHint::Trim
        );
        assert_eq!(
            bar_pointer_hint(Some(BarZone::Body), true),
            PointerHint::Locked
        );
        assert_eq!(
            PointerHint::Locked.cursor(),
            CursorStyle::OperationNotAllowed
        );
    }

    #[test]
    fn drag_cursors_remain_bound_to_the_active_gesture() {
        assert_eq!(
            drag_cursor(&TimelineDrag::Scrub),
            Some(CursorStyle::ResizeLeftRight)
        );
        assert_eq!(
            drag_cursor(&TimelineDrag::RubberBand {
                start: (0.0, 0.0),
                current: (0.0, 0.0),
                initial_selection: HashSet::new(),
                additive: false,
                moved: false,
            }),
            Some(CursorStyle::Crosshair)
        );
    }

    #[test]
    fn timecode_is_fixed_layout_at_integer_rates() {
        let fr = FrameRate::new(30, 1);
        assert_eq!(format_timecode(0, fr), "00:00:00:00");
        assert_eq!(format_timecode(29, fr), "00:00:00:29");
        assert_eq!(format_timecode(90, fr), "00:00:03:00");
        assert_eq!(format_timecode(30 * 61 + 5, fr), "00:01:01:05");
        assert_eq!(format_timecode(30 * 3_661 + 5, fr), "01:01:01:05");
    }

    #[test]
    fn timecode_stays_continuous_at_fractional_rates() {
        // 23.976 fps → nominal 24; the old wall-clock/ceil mix rendered
        // 0:59:22 → 1:00:23 → 1:00:00 across this boundary.
        let fr = FrameRate::new(24000, 1001);
        assert_eq!(format_timecode(1438, fr), "00:00:59:22");
        assert_eq!(format_timecode(1439, fr), "00:00:59:23");
        assert_eq!(format_timecode(1440, fr), "00:01:00:00");
    }

    #[test]
    fn frame_entry_parses_frames_and_both_timecode_formats() {
        let fr = FrameRate::new(30, 1);
        assert_eq!(parse_frame_entry("42", fr, 10_000), Some(42));
        assert_eq!(parse_frame_entry("2:03:04", fr, 10_000), Some(3_694));
        assert_eq!(parse_frame_entry("1:02:03:04", fr, 200_000), Some(111_694));
        assert_eq!(parse_frame_entry("1:60:00:00", fr, 200_000), None);
        assert_eq!(parse_frame_entry("0:00:00:30", fr, 200_000), None);
        assert_eq!(
            parse_frame_entry("0:00:01:00", FrameRate::new(24_000, 1_001), 100),
            Some(24)
        );
    }

    #[test]
    fn frame_entry_clamps_to_composition_bounds() {
        let fr = FrameRate::new(30, 1);
        assert_eq!(parse_frame_entry("-12", fr, 300), Some(0));
        assert_eq!(parse_frame_entry("999", fr, 300), Some(299));
        assert_eq!(parse_frame_entry("1:00:00", fr, 300), Some(299));
        assert_eq!(parse_frame_entry("12", fr, 0), Some(0));
    }

    #[test]
    fn fit_pixels_per_frame_clamps_to_zoom_range() {
        assert_eq!(fit_pixels_per_frame(1_000.0, 100), 10.0);
        assert_eq!(fit_pixels_per_frame(1.0, 1_000), MIN_PPF);
        assert_eq!(fit_pixels_per_frame(10_000.0, 10), MAX_PPF);
        assert_eq!(fit_pixels_per_frame(500.0, 0), MAX_PPF);
    }

    #[test]
    fn logarithmic_zoom_slider_mapping_roundtrips() {
        assert!((slider_to_ppf(0.0) - MIN_PPF).abs() < f64::EPSILON);
        assert!((slider_to_ppf(1.0) - MAX_PPF).abs() < 1e-9);
        for ppf in [MIN_PPF, 0.5, 4.0, 12.0, MAX_PPF] {
            assert!((slider_to_ppf(ppf_to_slider(ppf)) - ppf).abs() < 1e-5);
        }
    }

    #[test]
    fn selected_curves_resolve_live_channels_with_signed_comp_offset() {
        let layer_id = LayerId::new(7);
        let mut curve = KeyframeCurve::new();
        curve.insert(5, 1.0, Interpolation::Linear);
        curve.insert(15, 3.0, Interpolation::Linear);
        let mut layer = Layer::new(layer_id, "Animated", Graph::new()).with_time(-5, 10, 100);
        layer.opacity = AnimationChannel::keyframes(curve.clone());
        let composition = ravel_core::composition::Composition::new(
            CompId::new(1),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            120,
        )
        .add_layer(layer);
        let mut state = TimelinePanel::with_composition(composition);
        state.select_channel(
            TimelineChannelRef {
                layer: layer_id,
                row: PropertyRowId::Shell(PropertyGroup::Rotation),
                component: 0,
            },
            false,
        );
        state.select_channel(
            TimelineChannelRef {
                layer: layer_id,
                row: PropertyRowId::Shell(PropertyGroup::Opacity),
                component: 0,
            },
            true,
        );

        let colors = ThemeColor::default();
        let resolved = selected_timeline_curves(&state, &colors);
        assert_eq!(resolved.len(), 1, "constant selected channels are skipped");
        assert_eq!(resolved[0].frame_offset, -15);
        assert_eq!(resolved[0].curve.as_ref(), &curve);
        assert_eq!(resolved[0].color, colors.chart_1);
    }

    #[test]
    fn curve_value_fit_includes_bezier_controls_and_expands_flat_values() {
        let mut bezier = KeyframeCurve::new();
        bezier.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(0, 0.0, Interpolation::Bezier)
                .with_tangents(
                    ravel_core::types::Vec2(0.0, 0.0),
                    ravel_core::types::Vec2(3.0, 10.0),
                ),
        );
        bezier.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(10, 2.0, Interpolation::Linear)
                .with_tangents(
                    ravel_core::types::Vec2(-3.0, -7.0),
                    ravel_core::types::Vec2(0.0, 0.0),
                ),
        );
        let colors = ThemeColor::default();
        let channel = TimelineChannelRef {
            layer: LayerId::new(1),
            row: PropertyRowId::Shell(PropertyGroup::Opacity),
            component: 0,
        };
        let fitted = curve_value_bounds(&[TimelineCurveData {
            channel: channel.clone(),
            curve: Arc::new(bezier),
            frame_offset: 0,
            color: colors.chart_1,
        }])
        .unwrap();
        assert!((fitted.0 - -6.2).abs() < 1e-9);
        assert!((fitted.1 - 11.2).abs() < 1e-9);

        let mut flat = KeyframeCurve::new();
        flat.insert(0, 2.0, Interpolation::Linear);
        let flat = curve_value_bounds(&[TimelineCurveData {
            channel,
            curve: Arc::new(flat),
            frame_offset: 0,
            color: colors.chart_1,
        }])
        .unwrap();
        assert!(flat.0 < 2.0 && flat.1 > 2.0);
        assert!(flat.1 - flat.0 >= CURVE_DEGENERATE_MARGIN * 2.0);
    }

    #[test]
    fn value_grid_uses_nice_steps_and_includes_zero() {
        assert_eq!(value_grid_values(-1.0, 1.0, 96.0), vec![-1.0, 0.0, 1.0]);
        assert_eq!(nice_value_step(0.24), 0.5);
        assert_eq!(nice_value_step(24.0), 50.0);
        assert!(value_grid_values(1.0, 1.0, 100.0).is_empty());
    }

    #[test]
    fn graph_hit_respects_series_frame_offset() {
        let channel = TimelineChannelRef {
            layer: LayerId::new(7),
            row: PropertyRowId::Shell(PropertyGroup::Opacity),
            component: 0,
        };
        let mut curve = KeyframeCurve::new();
        curve.insert(5, 1.0, Interpolation::Linear);
        let hit = graph_hit_at(
            &[TimelineCurveData {
                channel,
                curve: Arc::new(curve),
                frame_offset: 10,
                color: ThemeColor::default().chart_1,
            }],
            0.0,
            10.0,
            (0.0, 2.0),
            (200.0, 100.0),
            CurvePoint::new(150.0, 50.0),
        )
        .expect("offset keyframe should be hit");
        assert_eq!(hit.frame, 5);
    }

    // ----- document-driven behavior -----------------------------------------

    fn stub_network() -> Graph {
        let out = ravel_core::graph::Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        Graph::new().add_node(out).unwrap()
    }

    /// Builds a ProjectState (eval disabled) with two layers in the root
    /// comp and a timeline panel synced to it.
    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<TimelineGpuiPanel>,
        Entity<ProjectState>,
        CompId,
        LayerId,
        LayerId,
    ) {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(gpui_component::init);

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ))
        });

        let (comp_id, a, b) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.expect("root comp");
            let a = LayerId::next();
            let b = LayerId::next();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                Layer::new(a, "A", stub_network()).with_time(0, 0, 100),
            )
            .unwrap();
            let doc = ravel_ui::document::add_layer(
                &doc,
                comp_id,
                Layer::new(b, "B", stub_network()).with_time(50, 0, 100),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            (comp_id, a, b)
        });

        let window = cx.add_window(|window, cx| {
            TimelineGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        (window, project, comp_id, a, b)
    }

    fn layer(
        project: &Entity<ProjectState>,
        comp: CompId,
        lid: LayerId,
        cx: &mut TestAppContext,
    ) -> Layer {
        project.read_with(cx, |project, _| {
            project
                .document()
                .get_composition(comp)
                .unwrap()
                .get_layer(lid)
                .unwrap()
                .clone()
        })
    }

    /// A document change caused by someone else (e.g. a node parameter
    /// edit) must not overwrite a node-properties target with this panel's
    /// selected layer (regression: node scrub flipped Properties to the
    /// layer view).
    #[gpui::test]
    fn document_sync_does_not_steal_the_node_properties_target(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        // Select a layer, then let the node editor take over the target.
        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
            })
            .unwrap();
        let node_target = super::super::PropertiesTarget::Nodes {
            network: NetworkPath::layer(comp_id, a),
            ids: vec![NodeId::next()],
        };
        cx.update(|cx| {
            cx.set_global(super::super::SelectedPropertiesTarget(node_target));
        });

        // An unrelated document edit flows through the observer.
        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::update_layer(project.document(), comp_id, a, |l| {
                l.start_frame = 42;
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(
                matches!(target.0, super::super::PropertiesTarget::Nodes { .. }),
                "node target must survive a timeline document sync"
            );
        });
    }

    /// Switching the active composition replaces what the panel shows. A
    /// layer of the new composition that reuses the old selection's
    /// `LayerId` is an unrelated layer: the selection must clear instead of
    /// surviving with a Properties target stuck at the old composition id.
    #[gpui::test]
    fn composition_switch_clears_the_selection_even_when_the_layer_id_recurs(
        cx: &mut TestAppContext,
    ) {
        let (window, project, comp_id, a, _b) = setup(cx);
        let editor = cx.add_window(|window, cx| {
            crate::panels::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });

        window
            .update(cx, |panel, _window, cx| panel.select_layer(a, cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(matches!(
                target.0,
                super::super::PropertiesTarget::Layer { comp_id: c, layer_id }
                    if c == comp_id && layer_id == a
            ));
        });

        // Add a second composition that reuses LayerId `a` and make it the
        // document root. Rewriting `root_comp` is a document edit, not a UI
        // switch — the panel must stay on the active composition.
        let new_comp_id = project.update(cx, |project, cx| {
            let new_comp_id = CompId::next();
            let comp = ravel_core::composition::Composition::new(
                new_comp_id,
                "Other",
                (1920, 1080),
                FrameRate::new(30, 1),
                300,
            )
            .add_layer(Layer::new(a, "unrelated", stub_network()).with_time(0, 0, 100));
            let mut doc = project.document().clone();
            doc.compositions
                .insert(new_comp_id, std::sync::Arc::new(comp));
            doc.root_comp = Some(new_comp_id);
            project.commit_document(doc, InvalidationHint::Structural, cx);
            new_comp_id
        });
        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(
                    panel.state.comp_id(),
                    Some(comp_id),
                    "a root_comp edit must not move the UI"
                );
                assert_eq!(panel.selected_layer(cx), Some(a));
            })
            .unwrap();

        // The node editor is open on the previous composition's layer.
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, a)));
            })
            .unwrap();

        project.update(cx, |project, cx| {
            project.set_active_composition(Some(new_comp_id), cx);
        });
        cx.run_until_parked();

        // The old network must not stay open: the Viewer tools and
        // `CanvasSelection` would keep targeting a composition the UI no
        // longer shows.
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), None);
            })
            .unwrap();
        cx.update(|cx| {
            assert!(
                cx.global::<super::super::CanvasSelection>()
                    .nodes
                    .is_empty(),
                "node selection must not survive a composition switch"
            );
        });

        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(panel.state.comp_id(), Some(new_comp_id));
                assert_eq!(
                    panel.selected_layer(cx),
                    None,
                    "selection must not survive a composition switch"
                );
            })
            .unwrap();
        cx.update(|cx| {
            let selection = super::super::layer_selection(cx);
            assert_eq!(
                selection.comp(),
                Some(new_comp_id),
                "LayerSelection.comp must track ActiveComposition"
            );
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(
                matches!(target.0, super::super::PropertiesTarget::Empty),
                "the Properties target must clear instead of pointing at the old composition"
            );
        });
    }

    /// Composition 0: an active composition that no longer resolves empties
    /// the panel instead of leaving stale layers on screen, and nothing
    /// panics.
    #[gpui::test]
    fn no_active_composition_renders_an_empty_panel(cx: &mut TestAppContext) {
        let (window, project, _comp_id, a, _b) = setup(cx);
        window
            .update(cx, |panel, _window, cx| panel.select_layer(a, cx))
            .unwrap();

        project.update(cx, |project, cx| {
            project.set_active_composition(None, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(panel.state.comp_id(), None);
                assert_eq!(panel.state.layers().count(), 0);
                assert_eq!(panel.state.duration_frames(), 0);
                assert_eq!(panel.selected_layer(cx), None);
                assert_eq!(panel.composition_params(), None);
                // Transport and edit entry points stay inert instead of
                // targeting a composition that is not there.
                panel.scrub_playhead(10, cx);
                assert_eq!(panel.playhead(), 0);
                panel.delete_selected_layers(cx);
            })
            .unwrap();
        cx.update(|cx| {
            let selection = super::super::layer_selection(cx);
            assert_eq!(selection.comp(), None);
            assert!(selection.is_empty());
        });
    }

    /// The panel mirrors the active composition instead of a panel-local
    /// demo composition.
    #[gpui::test]
    fn panel_displays_the_active_composition(cx: &mut TestAppContext) {
        let (window, _project, comp_id, a, b) = setup(cx);
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.state.comp_id(), Some(comp_id));
                let ids: Vec<LayerId> = panel.state.layers().map(|l| l.id).collect();
                assert_eq!(ids, vec![a, b]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn fit_timeline_resets_scroll_and_syncs_zoom_slider(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _a, _b) = setup(cx);

        window
            .update(cx, |panel, window, cx| {
                panel.ruler_width.set(600.0);
                panel.state.set_scroll_offset(42.0);
                panel.fit_timeline(window, cx);

                assert_eq!(panel.state.scroll_offset(), 0.0);
                assert!((panel.state.pixels_per_frame() - 2.0).abs() < f64::EPSILON);
                let slider = panel.zoom_slider.read(cx).value().start();
                assert!((slider - ppf_to_slider(2.0)).abs() < f32::EPSILON);
            })
            .unwrap();
    }

    /// A bar-drag gesture (live moves + mouse-up) lands in the document and
    /// rolls back with one Document undo step.
    #[gpui::test]
    fn bar_move_commits_one_document_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.drag = TimelineDrag::MoveBar {
                    layer: a,
                    origin_start: 0,
                    grab_x: 0.0,
                    changed: false,
                };
                // Two live moves (4 px/frame default zoom): +5 then +10.
                panel.drag_moved(20.0, 0.0, false, false, cx);
                panel.drag_moved(40.0, 0.0, false, false, cx);
                panel.drag_ended(cx);
            })
            .unwrap();
        assert_eq!(layer(&project, comp_id, a, cx).start_frame, 10);

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert_eq!(layer(&project, comp_id, a, cx).start_frame, 0);
        // The panel resynced through its observer.
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.state.layer(a).unwrap().start_frame, 0);
            })
            .unwrap();
    }

    /// Trimming the in edge keeps the out edge fixed and clamps into the
    /// display interval.
    #[gpui::test]
    fn trim_in_moves_start_with_in_frame(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.drag = TimelineDrag::TrimIn {
                    layer: a,
                    origin_start: 0,
                    origin_in: 0,
                    origin_out: 100,
                    grab_x: 0.0,
                    changed: false,
                };
                panel.drag_moved(40.0, 0.0, false, false, cx); // +10 frames
                panel.drag_ended(cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert_eq!((l.start_frame, l.in_frame, l.out_frame), (10, 10, 100));
        // end_frame unchanged: 10 + (100 - 10) = 100.
        assert_eq!(l.end_frame(), 100);
    }

    /// Deleting the selected layer removes it (and its network) from the
    /// document; undo restores it (REQ-LAYER-009).
    #[gpui::test]
    fn delete_selected_layers_roundtrips_through_undo(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.delete_selected_layers(cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            assert!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(a)
                    .is_none()
            );
        });

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        assert_eq!(layer(&project, comp_id, a, cx).name, "A");
    }

    /// The context-menu duplication handler inserts above the source,
    /// selects the copy, and records one structural undo step.
    #[gpui::test]
    fn duplicate_layer_handler_selects_copy_and_undoes(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);

        let copy = window
            .update(cx, |panel, _window, cx| panel.duplicate_layer(a, cx))
            .unwrap()
            .expect("duplicate");
        project.read_with(cx, |project, _| {
            let composition = project.document().get_composition(comp_id).unwrap();
            let ids: Vec<_> = composition.layers.iter().map(|layer| layer.id).collect();
            assert_eq!(ids, vec![a, copy, b]);
            assert_eq!(composition.get_layer(copy).unwrap().name, "A copy");
        });
        window
            .update(cx, |panel, _window, cx| {
                assert_eq!(panel.selected_layer(cx), Some(copy));
            })
            .unwrap();

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let composition = project.document().get_composition(comp_id).unwrap();
            assert_eq!(composition.layers.len(), 2);
            assert!(composition.get_layer(copy).is_none());
        });
    }

    /// The direct layer deletion handler used by the context menu commits a
    /// single structural undo step.
    #[gpui::test]
    fn delete_layer_handler_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| panel.delete_layer(a, cx))
            .unwrap();
        project.read_with(cx, |project, _| {
            assert!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .get_layer(a)
                    .is_none()
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert_eq!(layer(&project, comp_id, a, cx).name, "A");
    }

    /// The context-menu Solo handler toggles the shell flag and its one
    /// document commit is reversible in one undo.
    #[gpui::test]
    fn solo_layer_handler_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| panel.toggle_solo(a, cx))
            .unwrap();
        assert!(layer(&project, comp_id, a, cx).solo);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        assert!(!layer(&project, comp_id, a, cx).solo);
    }

    /// Locked layers are protected from deletion and bar drags.
    #[gpui::test]
    fn locked_layer_is_not_deleted(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_lock(a, cx);
                panel.select_layer(a, cx);
                panel.delete_selected_layers(cx);
            })
            .unwrap();
        assert!(layer(&project, comp_id, a, cx).locked);
    }

    /// Reordering via header drag persists to the document.
    #[gpui::test]
    fn header_drag_reorders_the_stack(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.drag = TimelineDrag::Reorder {
                    layer: a,
                    changed: false,
                };
                // Row 0 (top) is layer B: dragging A onto it moves A to B's
                // stack index.
                let origin_y = panel.area_origin.get().1;
                panel.drag_moved(0.0, origin_y + LAYER_ROW_HEIGHT / 2.0, false, false, cx);
                panel.drag_ended(cx);
            })
            .unwrap();
        project.read_with(cx, |project, _| {
            let ids: Vec<LayerId> = project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .layers
                .iter()
                .map(|l| l.id)
                .collect();
            assert_eq!(ids, vec![b, a]);
        });
    }

    /// Commit a keyframed position-X channel (keys at layer-local frames 0
    /// and 10) to the layer.
    fn add_position_x_keys(
        project: &Entity<ProjectState>,
        comp: CompId,
        lid: LayerId,
        cx: &mut TestAppContext,
    ) {
        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::update_layer(project.document(), comp, lid, |l| {
                let mut curve = KeyframeCurve::new();
                curve.insert(0, 0.0, Interpolation::Linear);
                curve.insert(10, 100.0, Interpolation::Linear);
                l.transform.position[0] = AnimationChannel::keyframes(curve);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
    }

    #[gpui::test]
    fn interpolation_action_commits_selection_as_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer_id, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, layer_id, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.sync_from_project(cx);
                panel.selected_keyframes = HashSet::from([
                    keyframe_ref(layer_id, &row, 0, 0),
                    keyframe_ref(layer_id, &row, 0, 10),
                ]);
                panel.set_selected_keyframe_interpolation(Interpolation::Bezier, cx);
            })
            .unwrap();

        project.read_with(cx, |project, _| {
            let layer = project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer_id)
                .unwrap();
            let channels = keyframes::row_channels(layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            assert!(
                curve
                    .keyframes()
                    .iter()
                    .all(|keyframe| keyframe.interpolation == Interpolation::Bezier)
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        project.read_with(cx, |project, _| {
            let layer = project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer_id)
                .unwrap();
            let channels = keyframes::row_channels(layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            assert!(
                curve
                    .keyframes()
                    .iter()
                    .all(|keyframe| keyframe.interpolation == Interpolation::Linear)
            );
        });
    }

    #[gpui::test]
    fn graph_keyframe_drag_moves_time_and_value_in_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer_id, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, layer_id, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        let channel = TimelineChannelRef {
            layer: layer_id,
            row: row.clone(),
            component: 0,
        };

        window
            .update(cx, |panel, _window, cx| {
                panel.sync_from_project(cx);
                panel.state.select_channel(channel, false);
                panel.selected_keyframes = HashSet::from([keyframe_ref(layer_id, &row, 0, 0)]);
                let curves = selected_timeline_curves(&panel.state, &ThemeColor::default());
                let transform = CurveTransform::new(
                    CurvePoint::new(0.0, 0.0),
                    CurvePoint::new(20.0, 100.0),
                    CurvePoint::new(200.0, 100.0),
                );
                panel.begin_graph_drag(
                    &curves,
                    CurveHit {
                        curve: 0,
                        frame: 0,
                        part: HitPart::Keyframe,
                    },
                    CurvePoint::new(0.0, 100.0),
                    transform,
                    (0.0, 0.0),
                );
                panel.drag_moved(50.0, 50.0, false, false, cx);
                panel.drag_ended(cx);
            })
            .unwrap();

        let moved = layer(&project, comp_id, layer_id, cx);
        let channels = keyframes::row_channels(&moved, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].frame, 5);
        assert_eq!(curve.keyframes()[0].value, 50.0);

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let restored = layer(&project, comp_id, layer_id, cx);
        let channels = keyframes::row_channels(&restored, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].frame, 0);
        assert_eq!(curve.keyframes()[0].value, 0.0);
    }

    #[gpui::test]
    fn graph_rubber_band_replaces_and_shift_adds_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer_id, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, layer_id, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        let first = keyframe_ref(layer_id, &row, 0, 0);
        let second = keyframe_ref(layer_id, &row, 0, 10);

        window
            .update(cx, |panel, _window, cx| {
                panel.sync_from_project(cx);
                panel.state.select_channel(
                    TimelineChannelRef {
                        layer: layer_id,
                        row: row.clone(),
                        component: 0,
                    },
                    false,
                );
                let curves = selected_timeline_curves(&panel.state, &ThemeColor::default());
                let transform = CurveTransform::new(
                    CurvePoint::new(0.0, 0.0),
                    CurvePoint::new(20.0, 100.0),
                    CurvePoint::new(200.0, 100.0),
                );
                let start = CurvePoint::new(-5.0, 90.0);
                panel.drag = TimelineDrag::GraphRubberBand {
                    curves: curves.clone(),
                    transform,
                    graph_origin: (0.0, 0.0),
                    start,
                    current: start,
                    initial_selection: HashSet::new(),
                    additive: false,
                    moved: false,
                };
                panel.drag_moved(5.0, 105.0, false, false, cx);
                assert_eq!(panel.selected_keyframes, HashSet::from([first.clone()]));
                panel.drag_ended(cx);

                panel.selected_keyframes = HashSet::from([second.clone()]);
                panel.drag = TimelineDrag::GraphRubberBand {
                    curves,
                    transform,
                    graph_origin: (0.0, 0.0),
                    start,
                    current: start,
                    initial_selection: HashSet::from([second.clone()]),
                    additive: true,
                    moved: false,
                };
                panel.drag_moved(5.0, 105.0, true, false, cx);
                assert_eq!(panel.selected_keyframes, HashSet::from([first, second]));
                panel.drag_ended(cx);
            })
            .unwrap();
    }

    #[gpui::test]
    fn multi_graph_handle_drag_separates_and_undoes(cx: &mut TestAppContext) {
        let (window, project, comp_id, layer_id, _b) = setup(cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        project.update(cx, |project, cx| {
            let doc =
                ravel_ui::document::update_layer(project.document(), comp_id, layer_id, |layer| {
                    let mut curve = KeyframeCurve::new();
                    curve.insert_keyframe(
                        ravel_core::animation::curve::Keyframe::new(0, 0.0, Interpolation::Bezier)
                            .with_tangents(Vec2(-2.0, -10.0), Vec2(4.0, 20.0)),
                    );
                    curve.insert_keyframe(
                        ravel_core::animation::curve::Keyframe::new(
                            10,
                            50.0,
                            Interpolation::Bezier,
                        )
                        .with_tangents(Vec2(-4.0, -20.0), Vec2(3.0, 10.0)),
                    );
                    curve.insert(20, 100.0, Interpolation::Linear);
                    layer.transform.position[0] = AnimationChannel::keyframes(curve);
                })
                .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        let channel = TimelineChannelRef {
            layer: layer_id,
            row: row.clone(),
            component: 0,
        };

        window
            .update(cx, |panel, _window, cx| {
                panel.sync_from_project(cx);
                panel.state.select_channel(channel, false);
                panel.selected_keyframes = HashSet::from([
                    keyframe_ref(layer_id, &row, 0, 0),
                    keyframe_ref(layer_id, &row, 0, 10),
                ]);
                let curves = selected_timeline_curves(&panel.state, &ThemeColor::default());
                let transform = CurveTransform::new(
                    CurvePoint::new(0.0, 0.0),
                    CurvePoint::new(20.0, 100.0),
                    CurvePoint::new(200.0, 100.0),
                );
                panel.begin_graph_drag(
                    &curves,
                    CurveHit {
                        curve: 0,
                        frame: 0,
                        part: HitPart::TangentOut,
                    },
                    CurvePoint::new(40.0, 80.0),
                    transform,
                    (0.0, 0.0),
                );
                panel.drag_moved(50.0, 70.0, false, true, cx);
                panel.drag_ended(cx);
            })
            .unwrap();

        let edited = layer(&project, comp_id, layer_id, cx);
        let channels = keyframes::row_channels(&edited, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(5.0, 30.0));
        assert_eq!(curve.keyframes()[0].tangent_in, Vec2(-2.0, -10.0));
        assert_eq!(curve.keyframes()[1].tangent_out, Vec2(4.0, 20.0));
        assert_eq!(curve.keyframes()[1].tangent_in, Vec2(-4.0, -20.0));

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let restored = layer(&project, comp_id, layer_id, cx);
        let channels = keyframes::row_channels(&restored, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(4.0, 20.0));
        assert_eq!(curve.keyframes()[1].tangent_out, Vec2(3.0, 10.0));
    }

    fn keyframe_ref(
        layer: LayerId,
        row: &PropertyRowId,
        component: usize,
        frame: u64,
    ) -> KeyframeRef {
        KeyframeRef {
            layer,
            row: row.clone(),
            component,
            frame,
        }
    }

    #[test]
    fn selection_after_move_preserves_overlapping_destinations() {
        let layer = LayerId::next();
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        let origin_selection = HashSet::from([
            keyframe_ref(layer, &row, 0, 0),
            keyframe_ref(layer, &row, 0, 10),
        ]);
        let baselines = vec![KeyframeChannelBaseline {
            layer,
            row: row.clone(),
            component: 0,
            curve: KeyframeCurve::new(),
            origin_frames: vec![0, 10],
        }];

        assert_eq!(
            TimelineGpuiPanel::selection_after_move(&origin_selection, &baselines, 10),
            HashSet::from([
                keyframe_ref(layer, &row, 0, 10),
                keyframe_ref(layer, &row, 0, 20),
            ])
        );
    }

    #[gpui::test]
    fn shift_click_toggles_keyframe_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                let (origin_x, origin_y) = panel.area_origin.get();
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    0.0,
                    1,
                    origin_x,
                    origin_y,
                    false,
                    cx,
                );
                panel.drag_ended(cx);
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    40.0,
                    1,
                    origin_x + 40.0,
                    origin_y,
                    true,
                    cx,
                );
                panel.drag_ended(cx);
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10),])
                );

                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    0.0,
                    1,
                    origin_x,
                    origin_y,
                    true,
                    cx,
                );
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 10)])
                );

                // A plain press on an unselected diamond still replaces the
                // selection immediately.
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    0.0,
                    1,
                    origin_x,
                    origin_y,
                    false,
                    cx,
                );
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 0)])
                );
                panel.drag_ended(cx);
            })
            .unwrap();
    }

    #[gpui::test]
    fn rubber_band_selects_keyframe_centers(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.state.toggle_layer_expanded(a);
                panel.state.toggle_property_expanded(a, row.clone());
                let (origin_x, origin_y) = panel.area_origin.get();
                // Layer B occupies y 0..28; layer A's Position-X channel is
                // centered at area-local y 86. Start at empty frame 15 and
                // drag left across the keys at frames 0 and 10.
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    60.0,
                    1,
                    origin_x + 60.0,
                    origin_y + 86.0,
                    false,
                    cx,
                );
                panel.drag_moved(origin_x - 1.0, origin_y + 90.0, false, false, cx);
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10),])
                );
                panel.drag_ended(cx);
            })
            .unwrap();
    }

    #[gpui::test]
    fn shift_empty_channel_click_keeps_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.state.toggle_layer_expanded(a);
                panel.state.toggle_property_expanded(a, row.clone());
                let selected = keyframe_ref(a, &row, 0, 0);
                panel.selected_keyframes.insert(selected.clone());
                let (origin_x, origin_y) = panel.area_origin.get();
                panel.channel_row_mouse_down(
                    a,
                    row,
                    0,
                    60.0,
                    1,
                    origin_x + 60.0,
                    origin_y + 86.0,
                    true,
                    cx,
                );
                assert_eq!(panel.selected_keyframes, HashSet::from([selected.clone()]));
                panel.drag_ended(cx);
                assert_eq!(panel.selected_keyframes, HashSet::from([selected]));
            })
            .unwrap();
    }

    #[gpui::test]
    fn plain_click_single_select_regression(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.selected_keyframes =
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10)]);
                let (origin_x, origin_y) = panel.area_origin.get();
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    40.0,
                    1,
                    origin_x + 40.0,
                    origin_y,
                    false,
                    cx,
                );
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10),])
                );
                assert!(matches!(panel.drag, TimelineDrag::MoveKeyframe { .. }));
                panel.drag_ended(cx);
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 10)])
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn plain_drag_on_selected_member_moves_full_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.selected_keyframes =
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10)]);
                let (origin_x, origin_y) = panel.area_origin.get();
                panel.channel_row_mouse_down(
                    a,
                    row.clone(),
                    0,
                    0.0,
                    1,
                    origin_x,
                    origin_y,
                    false,
                    cx,
                );
                assert_eq!(panel.selected_keyframes.len(), 2);
                panel.drag_moved(origin_x + 20.0, origin_y, false, false, cx);
                panel.drag_ended(cx);
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 5), keyframe_ref(a, &row, 0, 15),])
                );
            })
            .unwrap();

        let layer = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&layer, &row, 0, 5));
        assert!(keyframes::has_keyframe_at(&layer, &row, 0, 15));
        assert!(!keyframes::has_keyframe_at(&layer, &row, 0, 0));
        assert!(!keyframes::has_keyframe_at(&layer, &row, 0, 10));
    }

    /// A keyframe move drag (live moves + mouse-up) moves the key in layer
    /// time and rolls back with one Document undo step (REQ-LAYER-004).
    #[gpui::test]
    fn batch_move_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, a, |layer| {
                keyframes::insert_keyframe(layer, &row, 0, 20);
                keyframes::set_channel_value(layer, &row, 0, 20, 200.0);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });

        window
            .update(cx, |panel, _window, cx| {
                panel.selected_keyframes =
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10)]);
                panel.drag = TimelineDrag::MoveKeyframe {
                    baselines: panel.move_keyframe_baselines(),
                    origin_selection: panel.selected_keyframes.clone(),
                    pressed: keyframe_ref(a, &row, 0, 0),
                    collapse_on_click: false,
                    current_delta: 0,
                    grab_x: 0.0,
                    changed: false,
                };
                // The first preview collides with the unselected frame-20
                // key; the second must rebuild from the baseline and restore
                // it instead of preserving the transient merge.
                panel.drag_moved(40.0, 0.0, false, false, cx); // +10 frames
                panel.drag_moved(20.0, 0.0, false, false, cx); // +5 frames
                panel.drag_ended(cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 5));
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 15));
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 20));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 0));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 10));
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 5), keyframe_ref(a, &row, 0, 15),])
                );
            })
            .unwrap();

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 0));
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 10));
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 20));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 5));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 15));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.selected_keyframes.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn document_sync_drops_only_stale_keyframe_refs(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, _cx| {
                panel.selected_keyframes =
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10)]);
            })
            .unwrap();
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, a, |layer| {
                keyframes::remove_keyframe(layer, &row, 0, 0);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(
                    panel.selected_keyframes,
                    HashSet::from([keyframe_ref(a, &row, 0, 10)])
                );
            })
            .unwrap();
    }

    /// A keyframe added at a comp frame lands in the document as one undo
    /// step; undo removes it.
    #[gpui::test]
    fn add_keyframe_at_commits_and_undoes(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.add_keyframe_at(a, row.clone(), 0, 12, cx);
            })
            .unwrap();
        // start 0 / in 0: comp frame 12 is layer-local frame 12.
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 12));

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        let l = layer(&project, comp_id, a, cx);
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 12));
    }

    /// Batch Delete removes every selected keyframe while preserving the
    /// layer, and one undo restores the entire selection's edit.
    #[gpui::test]
    fn batch_delete_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, window, cx| {
                panel.select_layer(a, cx);
                panel.selected_keyframes =
                    HashSet::from([keyframe_ref(a, &row, 0, 0), keyframe_ref(a, &row, 0, 10)]);
                panel.on_delete(&EditDelete, window, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 0));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 10));
        window
            .update(cx, |panel, _window, _cx| {
                assert!(panel.selected_keyframes.is_empty());
            })
            .unwrap();

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 0));
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 10));
    }

    /// Diamonds and their hit test use the comp frame (`local - in + start`):
    /// a layer trimmed to in=5 starting at 10 shows its local-0 key at comp
    /// frame 5 — not at frame 10 (the old `key + start` bug).
    #[gpui::test]
    fn keyframe_hit_test_uses_comp_frame_with_in_offset(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        project.update(cx, |project, cx| {
            let doc = ravel_ui::document::update_layer(project.document(), comp_id, a, |l| {
                l.start_frame = 10;
                l.in_frame = 5;
                l.out_frame = 105;
                let mut curve = KeyframeCurve::new();
                curve.insert(0, 0.0, Interpolation::Linear);
                l.transform.position[0] = AnimationChannel::keyframes(curve);
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        window
            .update(cx, |panel, _window, _cx| {
                // Default zoom (4 px/frame), no scroll: comp frame 5 → x 20.
                assert_eq!(panel.keyframe_at_content_x(a, &row, 0, 20.0), Some(0));
                // The buggy `local + start` placement (comp frame 10 → x 40)
                // must not hit.
                assert_eq!(panel.keyframe_at_content_x(a, &row, 0, 40.0), None);
            })
            .unwrap();
    }

    /// A scrub drag keeps tracking through `drag_moved` after the pointer
    /// leaves the ruler (the ruler mousedown arms `TimelineDrag::Scrub`).
    #[gpui::test]
    fn scrub_drag_tracks_outside_the_ruler(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, _a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.drag = TimelineDrag::Scrub;
                // Default zoom (4 px/frame); the ruler origin sits after the
                // 200 px header, so x 240 → frame 10. The y far below the
                // ruler must not matter.
                let origin = panel.ruler_origin_x.get();
                panel.drag_moved(origin + 40.0, 500.0, false, false, cx);
                assert_eq!(panel.playhead(), 10);
                panel.drag_moved(origin + 80.0, -50.0, false, false, cx);
                assert_eq!(panel.playhead(), 20);
                // Ending a scrub commits nothing and clears the drag.
                panel.drag_ended(cx);
                assert!(matches!(panel.drag, TimelineDrag::None));
            })
            .unwrap();
    }

    /// Clicking empty space below the layer rows clears the selection and
    /// the Properties target that was showing it.
    #[gpui::test]
    fn empty_area_click_deselects_the_layer(cx: &mut TestAppContext) {
        let (window, _project, _comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.deselect_layer(cx);
                assert_eq!(panel.selected_layer(cx), None);
            })
            .unwrap();
        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(matches!(target.0, super::super::PropertiesTarget::Empty));
        });
    }

    /// Deselecting must not steal a node-properties target that replaced
    /// the layer view after the selection was made.
    #[gpui::test]
    fn deselect_does_not_steal_the_node_properties_target(cx: &mut TestAppContext) {
        let (window, _project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| panel.select_layer(a, cx))
            .unwrap();
        let node_target = super::super::PropertiesTarget::Nodes {
            network: NetworkPath::layer(comp_id, a),
            ids: vec![NodeId::next()],
        };
        cx.update(|cx| {
            cx.set_global(super::super::SelectedPropertiesTarget(node_target));
        });

        window
            .update(cx, |panel, _window, cx| panel.deselect_layer(cx))
            .unwrap();
        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(
                matches!(target.0, super::super::PropertiesTarget::Nodes { .. }),
                "a node target must survive an empty-area deselect"
            );
        });
    }

    /// Navigator ◀/▶ jump to the nearest keyframe before/after the
    /// playhead, in comp frames (start/in offsets included).
    #[gpui::test]
    fn navigator_jumps_between_keyframes(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx); // local keys at 0, 10
        // Offset the layer: local 0 → comp 5, local 10 → comp 15.
        project.update(cx, |project, cx| {
            let doc = update_layer(project.document(), comp_id, a, |l| {
                l.start_frame = 10;
                l.in_frame = 5;
                l.out_frame = 105;
            })
            .unwrap();
            project.commit_document(doc, InvalidationHint::None, cx);
        });
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.state.set_playhead(9);
                panel.jump_to_prev_keyframe(a, &row, cx);
                assert_eq!(panel.playhead(), 5);
                // Strictly-before: another prev from 5 has nowhere to go.
                panel.jump_to_prev_keyframe(a, &row, cx);
                assert_eq!(panel.playhead(), 5);
                panel.jump_to_next_keyframe(a, &row, cx);
                assert_eq!(panel.playhead(), 15);
                panel.jump_to_next_keyframe(a, &row, cx);
                assert_eq!(panel.playhead(), 15);
            })
            .unwrap();
    }

    /// Navigator ◆ inserts keys on every channel of the row at the playhead
    /// as one undo step; a second toggle removes them again.
    #[gpui::test]
    fn navigator_toggle_round_trips_all_channels(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.state.set_playhead(7);
                panel.toggle_row_keyframe(a, &row, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 7));
        assert!(keyframes::has_keyframe_at(&l, &row, 1, 7));

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_row_keyframe(a, &row, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 7));
        assert!(!keyframes::has_keyframe_at(&l, &row, 1, 7));

        // Each toggle was exactly one undo step.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 7));
        assert!(keyframes::has_keyframe_at(&l, &row, 1, 7));
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let l = layer(&project, comp_id, a, cx);
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 7));
    }

    /// A partially keyed row completes the missing channels instead of
    /// removing the existing key; locked layers reject the toggle.
    #[gpui::test]
    fn navigator_toggle_completes_partial_rows_and_respects_lock(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx); // X keys at 0, 10
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                panel.state.set_playhead(10);
                panel.toggle_row_keyframe(a, &row, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        // X already had a key at 10; Y gained one — nothing was removed.
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 10));
        assert!(keyframes::has_keyframe_at(&l, &row, 1, 10));

        window
            .update(cx, |panel, _window, cx| {
                panel.toggle_lock(a, cx);
                panel.toggle_row_keyframe(a, &row, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(
            keyframes::has_keyframe_at(&l, &row, 0, 10),
            "locked layers must reject the navigator toggle"
        );
    }

    /// Shift ranges over the stack and the platform modifier toggles, both
    /// writing the one shared selection (REQ-UI-013). The anchor stays first,
    /// which is what a following range extends from.
    #[gpui::test]
    fn modified_clicks_range_and_toggle_the_layer_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);
        let c = project.update(cx, |project, cx| {
            let c = LayerId::next();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                Layer::new(c, "C", stub_network()).with_time(0, 0, 100),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            c
        });
        cx.run_until_parked();

        let selection = |cx: &mut TestAppContext| cx.update(|cx| super::super::layer_selection(cx));

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.select_layer_with_mode(c, LayerClickMode::Range, cx);
            })
            .unwrap();
        assert_eq!(
            selection(cx).layers(),
            [a, b, c],
            "a range spans the stack from the anchor to the clicked layer"
        );

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
            })
            .unwrap();
        assert_eq!(selection(cx).layers(), [a, c], "the toggle drops the layer");

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
            })
            .unwrap();
        assert_eq!(
            selection(cx).layers(),
            [b, a, c],
            "re-adding makes the layer the anchor"
        );

        window
            .update(cx, |panel, _window, cx| panel.select_layer(b, cx))
            .unwrap();
        assert_eq!(
            selection(cx).layers(),
            [b],
            "a plain click replaces the whole selection"
        );
    }

    /// Several selected layers publish the read-only multi-layer Properties
    /// subject, and the node editor closes: a single-layer editor has no view of
    /// a multi-layer selection (REQ-UI-013). Nothing may be left pointing into
    /// the closed network — the Viewer bbox reads `CanvasSelection`.
    #[gpui::test]
    fn a_multi_layer_selection_closes_the_network_and_shows_the_layers_target(
        cx: &mut TestAppContext,
    ) {
        let (window, project, comp_id, a, b) = setup(cx);
        let editor = cx.add_window(|window, cx| {
            crate::panels::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });
        let node = layer(&project, comp_id, a, cx)
            .network
            .nodes()
            .next()
            .expect("a node")
            .id;

        window
            .update(cx, |panel, _window, cx| panel.select_layer(a, cx))
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, a)));
                // A node of that network is selected, as after a canvas click.
                cx.set_global(super::super::CanvasSelection {
                    path: Some(NetworkPath::layer(comp_id, a)),
                    nodes: HashSet::from([node]),
                });
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
            })
            .unwrap();
        cx.run_until_parked();

        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(
                    editor.context(),
                    None,
                    "two selected layers close the network"
                );
            })
            .unwrap();
        cx.update(|cx| {
            let selection = cx.global::<super::super::CanvasSelection>();
            assert!(
                selection.nodes.is_empty() && selection.path.is_none(),
                "no stale node selection may drive the Viewer bbox"
            );
            assert!(
                matches!(
                    &cx.global::<super::super::SelectedPropertiesTarget>().0,
                    super::super::PropertiesTarget::Layers { comp_id: c, layer_ids }
                        if *c == comp_id && layer_ids == &vec![b, a]
                ),
                "Properties inspects the whole selection, in selection order"
            );
        });

        // Back to one layer: the editor reopens that layer's network.
        window
            .update(cx, |panel, _window, cx| panel.select_layer(b, cx))
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, b)));
            })
            .unwrap();
        cx.update(|cx| {
            assert!(matches!(
                cx.global::<super::super::SelectedPropertiesTarget>().0,
                super::super::PropertiesTarget::Layer { layer_id, .. } if layer_id == b
            ));
        });
    }

    /// A delete aimed at a row of the selection removes the whole selection in
    /// one undo step; a row outside it takes only itself, leaving the selection
    /// alone (REQ-UI-013 bulk editing).
    #[gpui::test]
    fn deleting_a_selected_row_deletes_the_whole_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);
        let c = project.update(cx, |project, cx| {
            let c = LayerId::next();
            let doc = ravel_ui::document::add_layer(
                project.document(),
                comp_id,
                Layer::new(c, "C", stub_network()).with_time(0, 0, 100),
            )
            .unwrap();
            project.commit_document(doc, InvalidationHint::Structural, cx);
            c
        });
        cx.run_until_parked();
        let layers = |cx: &mut TestAppContext| {
            project.read_with(cx, |project, _| {
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layers
                    .iter()
                    .map(|layer| layer.id)
                    .collect::<Vec<_>>()
            })
        };

        // A row outside the selection: only that row goes.
        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                assert!(panel.delete_layer(c, cx));
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(layers(cx), vec![a, b]);
        cx.update(|cx| {
            assert_eq!(
                super::super::layer_selection(cx).layers(),
                [a],
                "deleting an unselected row leaves the selection alone"
            );
        });

        // A row inside the selection: the whole selection goes, in one step.
        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
                assert!(panel.delete_layer(a, cx));
            })
            .unwrap();
        cx.run_until_parked();
        assert!(layers(cx).is_empty());
        cx.update(|cx| assert!(super::super::layer_selection(cx).is_empty()));

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert_eq!(
            layers(cx),
            vec![a, b],
            "one undo brings back every deleted layer"
        );
    }

    /// A locked layer survives a bulk delete and stays selected.
    #[gpui::test]
    fn a_bulk_delete_protects_locked_layers(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(b, cx);
                panel.toggle_lock(b, cx);
                panel.select_layer(a, cx);
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
                assert!(panel.delete_layer(a, cx));
            })
            .unwrap();
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            let layers: Vec<LayerId> = project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .layers
                .iter()
                .map(|layer| layer.id)
                .collect();
            assert_eq!(layers, vec![b], "the locked layer is protected");
        });
        cx.update(|cx| {
            assert_eq!(
                super::super::layer_selection(cx).layers(),
                [b],
                "what survived stays selected"
            );
        });
    }

    /// A flag toggle aimed at a row of the selection applies to every selected
    /// layer, uniformly and in one undo step.
    #[gpui::test]
    fn toggling_a_flag_on_a_selected_row_applies_to_the_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);
        let muted = |cx: &mut TestAppContext, id: LayerId| layer(&project, comp_id, id, cx).muted;

        // `b` starts muted, so the clicked row decides: both end up muted.
        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(b, cx);
                panel.toggle_mute(b, cx);
                panel.select_layer(a, cx);
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
                panel.toggle_mute(a, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            muted(cx, a) && muted(cx, b),
            "the clicked row sets the value"
        );

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        assert!(
            !muted(cx, a) && muted(cx, b),
            "one undo restores both layers to the pre-toggle state"
        );
    }

    /// Duplicating a row of the selection duplicates every selected layer in one
    /// undo step and selects the copies.
    #[gpui::test]
    fn duplicating_a_selected_row_duplicates_the_selection(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.select_layer_with_mode(b, LayerClickMode::Toggle, cx);
                panel.duplicate_layers_from_row(a, cx);
            })
            .unwrap();
        cx.run_until_parked();

        let layers: Vec<LayerId> = project.read_with(cx, |project, _| {
            project
                .document()
                .get_composition(comp_id)
                .unwrap()
                .layers
                .iter()
                .map(|layer| layer.id)
                .collect()
        });
        assert_eq!(layers.len(), 4, "each source gained a copy: {layers:?}");
        cx.update(|cx| {
            let selection = super::super::layer_selection(cx);
            assert_eq!(selection.layers().len(), 2);
            assert!(
                selection.layers().iter().all(|id| *id != a && *id != b),
                "the copies are selected, not the sources"
            );
        });

        project.update(cx, |project, cx| assert!(project.undo(cx)));
        cx.run_until_parked();
        project.read_with(cx, |project, _| {
            assert_eq!(
                project
                    .document()
                    .get_composition(comp_id)
                    .unwrap()
                    .layers
                    .len(),
                2,
                "one undo removes every copy"
            );
        });
    }

    /// Selecting and deselecting a Timeline layer drives the Node Editor,
    /// while Properties remains targeted at the layer after selection.
    #[gpui::test]
    fn layer_selection_drives_node_editor_and_properties(cx: &mut TestAppContext) {
        let (window, _project, comp_id, _a, b) = setup(cx);

        let editor = cx.add_window(|window, cx| {
            crate::panels::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });
        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(b, cx);
            })
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, b)));
            })
            .unwrap();
        cx.update(|cx| {
            assert!(matches!(
                cx.global::<super::super::SelectedPropertiesTarget>().0,
                super::super::PropertiesTarget::Layer { comp_id: selected_comp, layer_id }
                    if selected_comp == comp_id && layer_id == b
            ));
        });

        window
            .update(cx, |panel, _window, cx| panel.deselect_layer(cx))
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn duplicate_and_delete_keep_node_editor_in_sync(cx: &mut TestAppContext) {
        let (window, _project, comp_id, a, _b) = setup(cx);
        let editor = cx.add_window(|window, cx| {
            crate::panels::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });

        let copy = window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.duplicate_layer(a, cx).expect("duplicate")
            })
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, copy)));
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| panel.delete_selected_layers(cx))
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), None)
            })
            .unwrap();
    }

    #[gpui::test]
    fn added_template_layer_becomes_active_network(cx: &mut TestAppContext) {
        let (window, _project, comp_id, _a, _b) = setup(cx);
        let editor = cx.add_window(|window, cx| {
            crate::panels::node_editor::NodeEditorPanel::new(
                ravel_ui::layout::PanelInstanceId(0),
                window,
                cx,
            )
        });

        let selected = window
            .update(cx, |panel, _window, cx| {
                panel.add_layer_from_template("shape", cx);
                panel.selected_layer(cx).expect("selected new layer")
            })
            .unwrap();
        cx.run_until_parked();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(
                    editor.context(),
                    Some(&NetworkPath::layer(comp_id, selected))
                );
            })
            .unwrap();
    }

    /// The graph editor's value range is the shared `widgets::curve_view`
    /// mechanism, not a Timeline-local one: pinning holds, and Fit puts the
    /// axis back on the data. The Properties curve editor drives the same
    /// type, so both panels zoom and fit identically.
    #[gpui::test]
    fn the_graph_value_range_is_the_shared_view_state(cx: &mut TestAppContext) {
        let (window, _project, _comp, _a, _b) = setup(cx);
        window
            .update(cx, |panel, _window, cx| {
                assert!(panel.curve_value_range.is_auto());
                assert_eq!(panel.curve_value_range.resolved((-2.0, 2.0)), (-2.0, 2.0));

                assert!(panel.curve_value_range.zoom((-2.0, 2.0), 0.5, 0.5));
                assert!(!panel.curve_value_range.is_auto());
                assert_eq!(panel.curve_value_range.resolved((-2.0, 2.0)), (-1.0, 1.0));

                panel.fit_curve_values(cx);
                assert!(
                    panel.curve_value_range.is_auto(),
                    "Fit follows the data again"
                );
                assert_eq!(panel.curve_value_range.resolved((-9.0, 9.0)), (-9.0, 9.0));
            })
            .unwrap();
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style GPUI timeline panel: ruler, layer bars, solo/mute/lock,
//! property tree with keyframe diamonds, playhead.
//!
//! The panel displays and edits the **document's root composition**
//! (layer-network-model Phase 3): every layer edit — add (menu commands),
//! delete, reorder (header drag), move/trim (bar drag), solo/mute/lock,
//! keyframe add/move/delete on the property tree (Phase 4, REQ-LAYER-004) —
//! goes through the app-wide [`ProjectState`] and lands in the
//! Document-level undo history (REQ-LAYER-009). Selecting a layer feeds the
//! Properties panel; double-clicking a layer opens its network in the node
//! editor without stealing that editor's context on mere selection
//! (REQ-LAYER-011).

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{Panel, PanelEvent};
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
use ravel_core::id::LayerId;
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::document::{
    NetworkPath, duplicate_layer as duplicate_layer_document, remove_layer, reorder_layer,
    root_composition, update_layer,
};
use ravel_ui::keyframes::{self, PropertyRow, PropertyRowId};
use ravel_ui::panels::timeline::{
    MAX_PPF, MIN_PPF, PropertyGroup, TimelineChannelRef, TimelinePanel, TimelineViewMode,
};

use crate::assets::RavelIcon;
use crate::project_state::ProjectState;
use crate::widgets::{
    CurveHit, CurvePoint, CurveSeries, CurveSource, CurveTransform,
    curve_editor_canvas_with_x_scale, hit_test_with_offsets,
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
const CURVE_VALUE_MARGIN_RATIO: f64 = 0.08;
const CURVE_DEGENERATE_MARGIN: f64 = 0.5;
const CURVE_HIT_RADIUS: f64 = 7.0;
const CURVE_VALUE_GRID_TARGET_PX: f64 = 48.0;

#[derive(Clone)]
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

pub struct TimelineGpuiPanel {
    state: TimelinePanel,
    project: Option<Entity<ProjectState>>,
    drag: TimelineDrag,
    /// Selected keyframe diamonds. Panel-local state; document sync retains
    /// every live identity and drops only refs whose diamonds disappeared.
    selected_keyframes: HashSet<KeyframeRef>,
    /// Whether the graph view paints time/value grid lines and value labels.
    show_curve_grid: bool,
    /// Explicit vertical graph range. `None` tracks the current curves.
    curve_value_range: Option<(f64, f64)>,
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
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
}

impl TimelineGpuiPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, _project, cx| {
                this.sync_from_project(cx);
            })
        });

        let mut state = TimelinePanel::new(FrameRate::new(30, 1));
        if let Some(project) = &project
            && let Some(comp) = root_composition(project.read(cx).document())
        {
            state.set_composition(comp.clone());
        }

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(
            ravel_ui::panel::PanelKind::Timeline,
            &focus_handle,
            window,
            cx,
        );
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
        cx.set_global(super::TimelinePanelHandle(cx.entity().downgrade()));
        Self {
            state,
            project,
            drag: TimelineDrag::None,
            selected_keyframes: HashSet::new(),
            show_curve_grid: true,
            curve_value_range: None,
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
            focused_sub,
            project_sub,
        }
    }

    // ----- document sync -----------------------------------------------------

    fn sync_from_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(comp) = root_composition(project.read(cx).document()).cloned() else {
            return;
        };
        if *self.state.composition() != comp {
            let old_comp_id = self.state.composition().id;
            let new_comp_id = comp.id;
            // Capture before the swap: was the Properties panel showing our
            // (comp, layer)? Afterwards `showing_selected_layer` compares
            // against the new composition id.
            let was_showing = self.showing_selected_layer(cx);
            self.state.set_composition(comp);
            // Drop a keyframe selection whose diamond disappeared (undo or
            // an external edit) — a stale selection would hijack Delete.
            self.selected_keyframes.retain(|keyframe| {
                self.state
                    .composition()
                    .get_layer(keyframe.layer)
                    .is_some_and(|layer| {
                        keyframes::has_keyframe_at(
                            layer,
                            &keyframe.row,
                            keyframe.component,
                            keyframe.frame,
                        )
                    })
            });
            // Deselect a deleted layer — and clear the Properties target
            // when it was showing it. A changed composition id (project
            // switch / load) also clears: a same-numbered LayerId in the
            // new composition is an unrelated layer, and a surviving
            // selection would leave the Properties target unresolvable at
            // its old comp id. Value freshness itself needs no republish:
            // the panel resolves from the document directly (and a
            // node-properties target must never be stolen).
            let selected = self.state.selected_layer();
            if let Some(selected) = selected
                && (new_comp_id != old_comp_id
                    || self.state.composition().get_layer(selected).is_none())
            {
                self.state.select_layer(None);
                if was_showing {
                    cx.set_global(super::SelectedPropertiesTarget(
                        super::PropertiesTarget::Empty,
                    ));
                }
            }
        }
        cx.notify();
    }

    /// Whether the Properties panel is currently showing this panel's
    /// selected layer in this panel's composition (only then may this panel
    /// re-publish or clear the target — a node-properties view, or a layer
    /// of a different composition, must not be stolen).
    fn showing_selected_layer(&self, cx: &App) -> bool {
        let selected = self.state.selected_layer();
        let own_comp = self.state.composition().id;
        cx.try_global::<super::SelectedPropertiesTarget>()
            .is_some_and(|target| {
                matches!(
                    &target.0,
                    super::PropertiesTarget::Layer { comp_id, layer_id }
                        if *comp_id == own_comp && Some(*layer_id) == selected
                )
            })
    }

    /// Publish the selected layer to the Properties panel. Only the layer's
    /// identity is published; the panel resolves current values from the
    /// document itself.
    fn publish_selected_layer_target(&mut self, cx: &mut Context<Self>) {
        let Some(lid) = self.state.selected_layer() else {
            return;
        };
        if self.state.composition().get_layer(lid).is_none() {
            return;
        }
        let comp_id = self.state.composition().id;
        cx.set_global(super::SelectedPropertiesTarget(
            super::PropertiesTarget::Layer {
                comp_id,
                layer_id: lid,
            },
        ));
    }

    /// Select a layer (single click). Never touches the node editor's
    /// context (REQ-LAYER-011).
    fn select_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.state.select_layer(Some(lid));
        self.publish_selected_layer_target(cx);
        cx.notify();
    }

    /// Clear the layer (and keyframe) selection — empty-area click. The
    /// Properties target is cleared only when it was showing this panel's
    /// selected layer; a node-properties view must not be stolen.
    fn deselect_layer(&mut self, cx: &mut Context<Self>) {
        self.selected_keyframes.clear();
        if self.state.selected_layer().is_none() {
            cx.notify();
            return;
        }
        let was_showing = self.showing_selected_layer(cx);
        self.state.select_layer(None);
        if was_showing {
            cx.set_global(super::SelectedPropertiesTarget(
                super::PropertiesTarget::Empty,
            ));
        }
        cx.notify();
    }

    /// Open the layer's network in the node editor (double-click /
    /// open-network, REQ-LAYER-011).
    pub fn open_layer_network(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        let comp_id = self.state.composition().id;
        let editor = cx
            .try_global::<super::NodeEditorHandle>()
            .and_then(|handle| handle.0.upgrade());
        if let Some(editor) = editor {
            editor.update(cx, |editor, cx| {
                editor.open_network(NetworkPath::layer(comp_id, lid), cx);
            });
        }
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
        let comp_id = self.state.composition().id;
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

    fn toggle_solo(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        // Solo/mute change the compiled merge chain (REQ-LAYER-007).
        self.edit_layer(
            lid,
            InvalidationHint::Structural,
            true,
            |l| l.solo = !l.solo,
            cx,
        );
    }

    fn toggle_mute(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.edit_layer(
            lid,
            InvalidationHint::Structural,
            true,
            |l| l.muted = !l.muted,
            cx,
        );
    }

    fn toggle_lock(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        self.edit_layer(
            lid,
            InvalidationHint::None,
            true,
            |l| l.locked = !l.locked,
            cx,
        );
    }

    /// Duplicate a layer directly above its source and select the copy.
    /// The graph and shell bindings receive fresh globally unique ids in
    /// the headless document helper.
    fn duplicate_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) -> Option<LayerId> {
        let project = self.project.clone()?;
        let comp_id = self.state.composition().id;
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

    /// Delete one named layer. Locked layers are protected even when the
    /// panel's composition mirror has not yet observed the latest document.
    fn delete_layer(&mut self, lid: LayerId, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let comp_id = self.state.composition().id;
        project.update(cx, |project, cx| {
            let locked = project
                .document()
                .get_composition(comp_id)
                .and_then(|c| c.get_layer(lid))
                .is_none_or(|l| l.locked);
            if locked {
                return;
            }
            if let Some(doc) = remove_layer(project.document(), comp_id, lid) {
                project.commit_document(doc, InvalidationHint::Structural, cx);
            }
        });
    }

    /// Delete the selected layer (its owned network goes with it,
    /// REQ-LAYER-009). Locked layers are protected. The lock is checked
    /// against the document (the panel mirror may lag one observer flush).
    fn delete_selected_layer(&mut self, cx: &mut Context<Self>) {
        let Some(lid) = self.state.selected_layer() else {
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
        let comp_id = self.state.composition().id;
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
        let layer = self.state.composition().get_layer(keyframe.layer)?;
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
        let comp_id = self.state.composition().id;
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
            let Some(layer) = self.state.composition().get_layer(channel.layer) else {
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

    fn fit_curve_values(&mut self, cx: &mut Context<Self>) {
        self.curve_value_range = None;
        cx.notify();
    }

    fn toggle_curve_grid(&mut self, cx: &mut Context<Self>) {
        self.show_curve_grid = !self.show_curve_grid;
        cx.notify();
    }

    fn add_layer_from_template(&mut self, template_key: &str, cx: &mut Context<Self>) {
        if let Some(project) = self.project.clone() {
            project.update(cx, |project, cx| {
                project.add_layer_from_template(template_key, cx);
            });
        }
    }

    fn on_delete(&mut self, _: &EditDelete, _window: &mut Window, cx: &mut Context<Self>) {
        // A selected keyframe scopes Delete to that keyframe; otherwise the
        // selected layer is deleted as before.
        let outcome = if !self.selected_keyframes.is_empty() {
            self.delete_selected_keyframes(cx);
            "delete_selected_keyframes"
        } else {
            self.delete_selected_layer(cx);
            "delete_selected_layer"
        };
        let focused_panel = crate::trace::focused_panel(cx);
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::PanelKeyDown,
                command: Some(CommandId::EditDelete),
                focused_panel,
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
        let layer = state.composition().get_layer(lid)?;
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

    fn frames_delta(&self, from_x: f32, to_x: f32) -> i64 {
        ((to_x - from_x) as f64 / self.state.pixels_per_frame()).round() as i64
    }

    fn keyframe_is_live(&self, keyframe: &KeyframeRef) -> bool {
        self.state
            .composition()
            .get_layer(keyframe.layer)
            .is_some_and(|layer| {
                keyframes::has_keyframe_at(layer, &keyframe.row, keyframe.component, keyframe.frame)
            })
    }

    fn move_keyframe_baselines(&self) -> Vec<KeyframeChannelBaseline> {
        let mut baselines: Vec<KeyframeChannelBaseline> = Vec::new();
        for keyframe in &self.selected_keyframes {
            let Some(layer) = self.state.composition().get_layer(keyframe.layer) else {
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
        let comp_id = self.state.composition().id;
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

    fn drag_moved(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
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
                let comp_id = self.state.composition().id;
                let Some(to_index) = self
                    .state
                    .composition()
                    .layers
                    .iter()
                    .position(|l| l.id == target)
                else {
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
        let changed = match &self.drag {
            TimelineDrag::MoveBar { changed, .. }
            | TimelineDrag::TrimIn { changed, .. }
            | TimelineDrag::TrimOut { changed, .. }
            | TimelineDrag::Reorder { changed, .. }
            | TimelineDrag::MoveKeyframe { changed, .. } => *changed,
            TimelineDrag::None | TimelineDrag::Scrub | TimelineDrag::RubberBand { .. } => false,
        };
        self.drag = TimelineDrag::None;
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
            | TimelineDrag::MoveKeyframe { changed, .. } => *changed,
            TimelineDrag::None | TimelineDrag::Scrub | TimelineDrag::RubberBand { .. } => false,
        };
        let structural = matches!(self.drag, TimelineDrag::Reorder { .. });
        self.drag = TimelineDrag::None;
        if let Some(pressed) = collapse_to {
            self.selected_keyframes = HashSet::from([pressed]);
            cx.notify();
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
        let layer = state.composition().get_layer(lid)?;
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

        for layer in self.state.composition().layers.iter().rev() {
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
                let composition = self.state.composition().clone();
                self.selected_keyframes.retain(|keyframe| {
                    composition.get_layer(keyframe.layer).is_some_and(|layer| {
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
                let layer = self.state.composition().get_layer(lid);
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
        let comp_id = self.state.composition().id;
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
        let Some(layer) = self.state.composition().get_layer(lid) else {
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
        let Some(layer) = self.state.composition().get_layer(lid) else {
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
        let comp_id = self.state.composition().id;
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
        let composition = self.state.composition();
        if let Some(frame) =
            parse_frame_entry(&value, composition.frame_rate, composition.duration_frames)
        {
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
        let ppf = fit_pixels_per_frame(
            self.ruler_width.get() as f64,
            self.state.composition().duration_frames,
        );
        self.state.set_pixels_per_frame(ppf);
        self.state.set_scroll_offset(0.0);
        self.sync_zoom_slider(window, cx);
        cx.notify();
    }

    fn build_transport_toolbar(&self, is_playing: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let composition = self.state.composition();
        let playhead = self.state.playhead();
        let fps = format_fps(composition.frame_rate);
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
                        .label(t!("timeline.interpolation.bezier"))
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
                        .label(t!("timeline.interpolation.linear"))
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
                        .label(t!("timeline.interpolation.step"))
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
                .child(SharedString::from(format_timecode(
                    playhead,
                    composition.frame_rate,
                )))
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
                        "{fps} fps · {}f",
                        composition.duration_frames
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
                        let end = this.state.composition().duration_frames.saturating_sub(1);
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
        let (fps, duration_frames) = self.composition_params();
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
    /// playback clock.
    pub fn composition_params(&self) -> (FrameRate, u64) {
        let comp = self.state.composition();
        (comp.frame_rate, comp.duration_frames)
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
                let fr = state.composition().frame_rate;
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
    }

    /// The layer-area row under a content-space y: layer bar, property
    /// group, or channel sub-row, following the same layout as the painter
    /// (top layer first, property rows only while expanded).
    fn row_at_content_y(&self, content_y: f32) -> Option<RowHit> {
        Self::row_at_content_y_in(&self.state, content_y)
    }

    fn row_at_content_y_in(state: &TimelinePanel, content_y: f32) -> Option<RowHit> {
        let mut y = 0.0f32;
        for layer in state.composition().layers.iter().rev() {
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
        for layer in self.state.composition().layers.iter() {
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
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;
        let selected_layer = self.state.selected_layer();
        let selected_keyframes = self.selected_keyframes.clone();
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

        canvas(
            move |bounds, _window, _cx| {
                area_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                (state, selected_layer, selected_keyframes, rubber_band)
            },
            move |bounds, (state, selected_layer, selected_keyframes, rubber_band), window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.background));

                let mut y = bounds.origin.y;
                for layer in state.composition().layers.iter().rev() {
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

                        if selected_layer == Some(layer.id) {
                            window.paint_quad(
                                outline(bar_bounds, colors.foreground, BorderStyle::default())
                                    .corner_radii(px(LAYER_BAR_CORNER_RADIUS))
                                    .border_widths(px(2.0)),
                            );
                        }

                        if bar_w > 40.0 {
                            let bar_top = y + px(2.0);
                            let bar_h = LAYER_ROW_HEIGHT - 4.0;
                            paint_bar_label(
                                &layer.name,
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
            },
        )
        .flex_grow()
        .h(px(content_height))
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
        let value_bounds = self.curve_value_range.unwrap_or(auto_value_bounds);
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
        let left_origin = area_origin.clone();
        let left_size = graph_size.clone();
        let right_origin = area_origin.clone();
        let right_size = graph_size.clone();
        let last_right_click = self.last_right_click.clone();

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
                    },
                )
                .absolute()
                .inset_0(),
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
                    } else if !event.modifiers.shift {
                        this.selected_keyframes.clear();
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
        let selected = self.state.selected_layer();

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
            .composition()
            .layers
            .iter()
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
                    .composition()
                    .get_layer(*id)
                    .map(keyframes::property_rows)
                    .unwrap_or_default()
            })
            .collect();

        for (i, (layer_id, name, solo, muted, locked)) in layers.iter().enumerate() {
            let is_selected = selected == Some(*layer_id);
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
                            if ev.click_count == 2 {
                                // Double-click opens the layer's network
                                // (REQ-LAYER-011).
                                this.drag = TimelineDrag::None;
                                this.open_layer_network(lid, cx);
                                return;
                            }
                            this.select_layer(lid, cx);
                            // Header drag reorders the stack; committed on
                            // mouse-up.
                            this.drag = TimelineDrag::Reorder {
                                layer: lid,
                                changed: false,
                            };
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
                                    this.state.toggle_layer_expanded(lid);
                                    cx.notify();
                                }),
                            ),
                    )
                    // Layer name
                    .child(
                        div()
                            .flex_grow()
                            .text_sm()
                            .text_color(theme.colors.foreground)
                            .overflow_x_hidden()
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
                            .child(
                                div()
                                    .ml_1()
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

impl Panel for TimelineGpuiPanel {
    fn panel_name(&self) -> &'static str {
        "timeline"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(ravel_ui::panel::PanelKind::Timeline, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        super::tab_title(
            Some(ravel_ui::panel::PanelKind::Timeline),
            SharedString::from(t!("panel.timeline")),
            color,
        )
    }
}

impl EventEmitter<PanelEvent> for TimelineGpuiPanel {}

impl Focusable for TimelineGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TimelineGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
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
                .build_layer_area(&theme.colors, self.area_origin.clone())
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
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left) {
                    this.cancel_drag(cx);
                    return;
                }
                let x: f32 = event.position.x.into();
                let y: f32 = event.position.y.into();
                this.drag_moved(x, y, cx);
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
                                            this.select_layer(layer, cx);
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
                                        if let Some(layer) =
                                            menu_state.composition().get_layer(layer_id)
                                        {
                                            let duplicate_entity = entity.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.duplicate_layer"
                                                ))
                                                .on_click(move |_, _window, cx| {
                                                    duplicate_entity.update(cx, |this, cx| {
                                                        this.duplicate_layer(layer_id, cx);
                                                    });
                                                }),
                                            );

                                            let delete_entity = entity.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(t!(
                                                    "timeline.menu.delete_layer"
                                                ))
                                                .disabled(layer.locked)
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
                                                        if event.click_count == 2 {
                                                            this.drag = TimelineDrag::None;
                                                            this.open_layer_network(lid, cx);
                                                            return;
                                                        }
                                                        // Bar clicks leave
                                                        // keyframe editing: drop
                                                        // the selection so Delete
                                                        // keeps targeting layers.
                                                        this.selected_keyframes.clear();
                                                        this.select_layer(lid, cx);
                                                        let locked = this
                                                            .state
                                                            .composition()
                                                            .get_layer(lid)
                                                            .is_none_or(|l| l.locked);
                                                        if locked {
                                                            return;
                                                        }
                                                        let Some((lid, zone)) =
                                                            this.bar_hit(content_x, content_y)
                                                        else {
                                                            return;
                                                        };
                                                        let Some(layer) =
                                                            this.state.composition().get_layer(lid)
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
    let transform = CurveTransform::new(
        CurvePoint::new(scroll, value_bounds.0),
        CurvePoint::new(scroll + size.0 as f64 / pixels_per_frame, value_bounds.1),
        CurvePoint::new(size.0 as f64, size.1 as f64),
    );
    let sources: Vec<_> = curves
        .iter()
        .map(|curve| CurveSource {
            curve: &curve.curve,
            frame_offset: curve.frame_offset,
        })
        .collect();
    hit_test_with_offsets(&sources, transform, pointer, CURVE_HIT_RADIUS)
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
            let (minor_frames, major_frames) = tick_intervals(ppf, state.composition().frame_rate);
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

fn value_grid_values(min: f64, max: f64, height: f64) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || max <= min || height <= 0.0 {
        return Vec::new();
    }
    let target_lines = (height / CURVE_VALUE_GRID_TARGET_PX).max(1.0);
    let step = nice_value_step((max - min) / target_lines);
    if !step.is_finite() || step <= 0.0 {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut value = (min / step).ceil() * step;
    while value <= max && values.len() < 128 {
        values.push(if value.abs() < step * 1.0e-9 {
            0.0
        } else {
            value
        });
        value += step;
    }
    values
}

fn nice_value_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10.0_f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

fn format_value_label(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000.0 || (abs > 0.0 && abs < 0.01) {
        format!("{value:.1e}")
    } else if abs >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn selected_timeline_curves(state: &TimelinePanel, colors: &ThemeColor) -> Vec<TimelineCurveData> {
    let mut series = Vec::new();
    for selected in state.selected_channels() {
        let Some(layer) = state.composition().get_layer(selected.layer) else {
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
    let span = max - min;
    let margin = if span <= f64::EPSILON {
        CURVE_DEGENERATE_MARGIN.max(min.abs() * CURVE_VALUE_MARGIN_RATIO)
    } else {
        span * CURVE_VALUE_MARGIN_RATIO
    };
    Some((min - margin, max + margin))
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
    use ravel_core::id::{CompId, DataTypeId, NodeId};
    use ravel_core::network as net;

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

        let window = cx.add_window(TimelineGpuiPanel::new);
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

    /// A project switch replaces the root composition wholesale. A layer of
    /// the new composition that reuses the old selection's `LayerId` is an
    /// unrelated layer: the selection must clear instead of surviving with
    /// a Properties target stuck at the old composition id.
    #[gpui::test]
    fn project_switch_clears_the_selection_even_when_the_layer_id_recurs(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| panel.select_layer(a, cx))
            .unwrap();
        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(matches!(
                target.0,
                super::super::PropertiesTarget::Layer { comp_id: c, layer_id }
                    if c == comp_id && layer_id == a
            ));
        });

        // Switch to a different root comp that reuses LayerId `a`.
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
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.state.composition().id, new_comp_id);
                assert_eq!(
                    panel.state.selected_layer(),
                    None,
                    "selection must not survive a project switch"
                );
            })
            .unwrap();
        cx.update(|cx| {
            let target = cx.global::<super::super::SelectedPropertiesTarget>();
            assert!(
                matches!(target.0, super::super::PropertiesTarget::Empty),
                "the Properties target must clear instead of pointing at the old composition"
            );
        });
    }

    /// The panel mirrors the document's root composition instead of a
    /// panel-local demo composition.
    #[gpui::test]
    fn panel_displays_the_document_composition(cx: &mut TestAppContext) {
        let (window, _project, comp_id, a, b) = setup(cx);
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.state.composition().id, comp_id);
                let ids: Vec<LayerId> = panel
                    .state
                    .composition()
                    .layers
                    .iter()
                    .map(|l| l.id)
                    .collect();
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
                panel.drag_moved(20.0, 0.0, cx);
                panel.drag_moved(40.0, 0.0, cx);
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
                assert_eq!(
                    panel.state.composition().get_layer(a).unwrap().start_frame,
                    0
                );
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
                panel.drag_moved(40.0, 0.0, cx); // +10 frames
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
    fn delete_selected_layer_roundtrips_through_undo(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(a, cx);
                panel.delete_selected_layer(cx);
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
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.state.selected_layer(), Some(copy));
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
                panel.delete_selected_layer(cx);
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
                panel.drag_moved(0.0, origin_y + LAYER_ROW_HEIGHT / 2.0, cx);
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
                panel.drag_moved(origin_x - 1.0, origin_y + 90.0, cx);
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
                panel.drag_moved(origin_x + 20.0, origin_y, cx);
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
                panel.drag_moved(40.0, 0.0, cx); // +10 frames
                panel.drag_moved(20.0, 0.0, cx); // +5 frames
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
                panel.drag_moved(origin + 40.0, 500.0, cx);
                assert_eq!(panel.playhead(), 10);
                panel.drag_moved(origin + 80.0, -50.0, cx);
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
                assert_eq!(panel.state.selected_layer(), None);
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

    /// Selecting a layer in the timeline never force-switches the node
    /// editor's context (REQ-LAYER-011); only the explicit open does.
    #[gpui::test]
    fn selection_does_not_steal_the_node_editor_context(cx: &mut TestAppContext) {
        let (window, _project, comp_id, a, b) = setup(cx);

        let editor = cx.add_window(crate::panels::node_editor::NodeEditorPanel::new);
        editor
            .update(cx, |editor, _window, cx| {
                editor.open_network(NetworkPath::layer(comp_id, a), cx);
            })
            .unwrap();

        window
            .update(cx, |panel, _window, cx| {
                panel.select_layer(b, cx);
            })
            .unwrap();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, a)));
            })
            .unwrap();

        // The explicit open switches it.
        window
            .update(cx, |panel, _window, cx| {
                panel.open_layer_network(b, cx);
            })
            .unwrap();
        editor
            .update(cx, |editor, _window, _cx| {
                assert_eq!(editor.context(), Some(&NetworkPath::layer(comp_id, b)));
            })
            .unwrap();
    }
}

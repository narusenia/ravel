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
use std::rc::Rc;

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, ThemeColor};
use ravel_core::animation::channel::ChannelSource;
use ravel_core::composition::Layer;
use ravel_core::id::LayerId;
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::document::{
    NetworkPath, remove_layer, reorder_layer, root_composition, update_layer,
};
use ravel_ui::keyframes::{self, PropertyRow, PropertyRowId};
use ravel_ui::panels::timeline::{PropertyGroup, TimelinePanel};

use crate::project_state::ProjectState;
use crate::workspace::EditDelete;
use ravel_ui::command::CommandId;

/// GPUI key context used by shortcuts local to the timeline.
pub const KEY_CONTEXT: &str = "Timeline";

const RULER_HEIGHT: f32 = 24.0;
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
    /// Move a keyframe along the timeline (layer-local frames).
    MoveKeyframe {
        layer: LayerId,
        row: PropertyRowId,
        component: usize,
        /// Layer-local frame the keyframe started the gesture at.
        origin_frame: u64,
        /// Layer-local frame the keyframe currently sits at.
        current_frame: u64,
        /// The channel's curve when the gesture started; every live preview
        /// derives from it so transient collisions never merge keys.
        baseline: ravel_core::animation::curve::KeyframeCurve,
        grab_x: f32,
        changed: bool,
    },
}

pub struct TimelineGpuiPanel {
    state: TimelinePanel,
    project: Option<Entity<ProjectState>>,
    drag: TimelineDrag,
    /// The selected keyframe diamond (layer, row, component, layer-local
    /// frame). Panel-local state; cleared when a document change removes
    /// the keyframe it points at.
    selected_keyframe: Option<(LayerId, PropertyRowId, usize, u64)>,
    /// Last painted width of the ruler/layer area (pixels), captured during
    /// prepaint so follow-playhead scrolling knows the visible range.
    ruler_width: Rc<Cell<f32>>,
    /// Origin x of the ruler area, captured during prepaint so a scrub drag
    /// can map window coordinates to frames from anywhere in the panel.
    ruler_origin_x: Rc<Cell<f32>>,
    /// Origin of the layer bar area, captured during prepaint for
    /// bar hit-testing in panel coordinates.
    area_origin: Rc<Cell<(f32, f32)>>,
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
        cx.set_global(super::TimelinePanelHandle(cx.entity().downgrade()));
        Self {
            state,
            project,
            drag: TimelineDrag::None,
            selected_keyframe: None,
            ruler_width: Rc::new(Cell::new(0.0)),
            ruler_origin_x: Rc::new(Cell::new(0.0)),
            area_origin: Rc::new(Cell::new((0.0, 0.0))),
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
            if let Some((lid, row, component, frame)) = self.selected_keyframe.clone() {
                let alive = self
                    .state
                    .composition()
                    .get_layer(lid)
                    .is_some_and(|l| keyframes::has_keyframe_at(l, &row, component, frame));
                if !alive {
                    self.selected_keyframe = None;
                }
            }
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
        self.selected_keyframe = None;
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

    /// Delete the selected layer (its owned network goes with it,
    /// REQ-LAYER-009). Locked layers are protected. The lock is checked
    /// against the document (the panel mirror may lag one observer flush).
    fn delete_selected_layer(&mut self, cx: &mut Context<Self>) {
        let Some(lid) = self.state.selected_layer() else {
            return;
        };
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

    /// Remove the selected keyframe as one Document undo step and clear the
    /// selection. Locked layers are protected (checked against the document,
    /// like [`Self::delete_selected_layer`]); a selection whose keyframe no
    /// longer resolves is dropped without touching the document.
    fn delete_selected_keyframe(&mut self, cx: &mut Context<Self>) {
        let Some((lid, row, component, frame)) = self.selected_keyframe.clone() else {
            return;
        };
        let Some(project) = self.project.clone() else {
            return;
        };
        let comp_id = self.state.composition().id;
        let mut keep_selection = false;
        project.update(cx, |project, cx| {
            let Some(layer) = project
                .document()
                .get_composition(comp_id)
                .and_then(|c| c.get_layer(lid))
            else {
                return; // The layer is gone: drop the stale selection.
            };
            if layer.locked {
                // Nothing happens; the selection stays, mirroring the
                // locked-layer delete behavior.
                keep_selection = true;
                return;
            }
            if !keyframes::has_keyframe_at(layer, &row, component, frame) {
                return; // Stale selection: drop it without an edit.
            }
            let mut removed = false;
            let doc = update_layer(project.document(), comp_id, lid, |l| {
                removed = keyframes::remove_keyframe(l, &row, component, frame);
            });
            if removed && let Some(doc) = doc {
                project.commit_document(doc, InvalidationHint::None, cx);
            }
        });
        if !keep_selection {
            self.selected_keyframe = None;
        }
        cx.notify();
    }

    fn on_delete(&mut self, _: &EditDelete, _window: &mut Window, cx: &mut Context<Self>) {
        // A selected keyframe scopes Delete to that keyframe; otherwise the
        // selected layer is deleted as before.
        let outcome = if self.selected_keyframe.is_some() {
            self.delete_selected_keyframe(cx);
            "delete_selected_keyframe"
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
        let lid = self.layer_at_content_y(content_y)?;
        let layer = self.state.composition().get_layer(lid)?;
        let ppf = self.state.pixels_per_frame();
        let scroll = self.state.scroll_offset();
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
                layer,
                row,
                component,
                origin_frame,
                current_frame,
                baseline,
                grab_x,
                ..
            } => {
                let delta = self.frames_delta(grab_x, x);
                let new_frame = (origin_frame as i64 + delta).max(0) as u64;
                if new_frame == current_frame {
                    return;
                }
                // Every preview derives from the gesture's baseline curve:
                // passing over an occupied frame does not permanently merge
                // the two keys — only the committed end position overwrites.
                self.edit_layer(
                    layer,
                    InvalidationHint::None,
                    false,
                    |l| {
                        keyframes::preview_keyframe_move(
                            l,
                            &row,
                            component,
                            &baseline,
                            origin_frame,
                            new_frame,
                        );
                    },
                    cx,
                );
                self.selected_keyframe = Some((layer, row.clone(), component, new_frame));
                self.drag = TimelineDrag::MoveKeyframe {
                    layer,
                    row,
                    component,
                    origin_frame,
                    current_frame: new_frame,
                    baseline,
                    grab_x,
                    changed: true,
                };
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
            TimelineDrag::None | TimelineDrag::Scrub => false,
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
        let changed = match &self.drag {
            TimelineDrag::MoveBar { changed, .. }
            | TimelineDrag::TrimIn { changed, .. }
            | TimelineDrag::TrimOut { changed, .. }
            | TimelineDrag::Reorder { changed, .. }
            | TimelineDrag::MoveKeyframe { changed, .. } => *changed,
            TimelineDrag::None | TimelineDrag::Scrub => false,
        };
        let structural = matches!(self.drag, TimelineDrag::Reorder { .. });
        self.drag = TimelineDrag::None;
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
        let layer = self.state.composition().get_layer(lid)?;
        let channels = keyframes::row_channels(layer, row)?;
        let channel = channels.get(component)?;
        let ChannelSource::Keyframes(curve) = &channel.source else {
            return None;
        };
        let ppf = self.state.pixels_per_frame();
        let scroll = self.state.scroll_offset();
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

    /// Mouse down on a channel sub-row: click an existing diamond to select
    /// it and start a [`TimelineDrag::MoveKeyframe`], double-click empty
    /// space to add a keyframe, click empty space to clear the selection.
    #[allow(clippy::too_many_arguments)]
    fn channel_row_mouse_down(
        &mut self,
        lid: LayerId,
        row: PropertyRowId,
        component: usize,
        content_x: f64,
        click_count: usize,
        grab_x: f32,
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
                self.selected_keyframe = Some((lid, row.clone(), component, frame));
                let layer = self.state.composition().get_layer(lid);
                let locked = layer.is_none_or(|l| l.locked);
                // Capture the gesture's baseline curve; every live preview
                // derives from it (see preview_keyframe_move).
                let baseline = layer.and_then(|l| {
                    keyframes::row_channels(l, &row)
                        .and_then(|channels| channels.get(component).cloned())
                        .and_then(|channel| match &channel.source {
                            ChannelSource::Keyframes(curve) => Some(curve.clone()),
                            _ => None,
                        })
                });
                if !locked && let Some(baseline) = baseline {
                    self.drag = TimelineDrag::MoveKeyframe {
                        layer: lid,
                        row,
                        component,
                        origin_frame: frame,
                        current_frame: frame,
                        baseline,
                        grab_x,
                        changed: false,
                    };
                }
            }
            None => self.selected_keyframe = None,
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

    // ----- playback glue -------------------------------------------------------

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
        let mut y = 0.0f32;
        for layer in self.state.composition().layers.iter().rev() {
            if content_y >= y && content_y < y + LAYER_ROW_HEIGHT {
                return Some(RowHit::LayerBar(layer.id));
            }
            y += LAYER_ROW_HEIGHT;
            if self.state.is_layer_expanded(layer.id) {
                for row in keyframes::property_rows(layer) {
                    if content_y >= y && content_y < y + PROPERTY_ROW_HEIGHT {
                        return Some(RowHit::PropertyGroup(layer.id, row.id));
                    }
                    y += PROPERTY_ROW_HEIGHT;
                    if self.state.is_property_expanded(layer.id, &row.id) {
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
        let selected_keyframe = self.selected_keyframe.clone();
        let content_height = self.total_layer_height();

        canvas(
            move |bounds, _window, _cx| {
                area_origin.set((bounds.origin.x.into(), bounds.origin.y.into()));
                (state, selected_layer, selected_keyframe)
            },
            move |bounds, (state, selected_layer, selected_keyframe), window, cx| {
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
                                                let is_selected = selected_keyframe
                                                    .as_ref()
                                                    .is_some_and(|(lid, row_id, comp, frame)| {
                                                        *lid == layer.id
                                                            && *row_id == row.id
                                                            && *comp == component
                                                            && *frame == kf.frame
                                                    });
                                                paint_diamond(
                                                    bounds.origin.x + px(kf_x as f32),
                                                    y + px(PROPERTY_ROW_HEIGHT / 2.0),
                                                    if is_selected {
                                                        colors.foreground
                                                    } else {
                                                        colors.accent
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
            },
        )
        .flex_grow()
        .h(px(content_height))
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
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.colors.muted_foreground)
                                    .child(label),
                            ),
                    );

                    if is_prop_expanded {
                        for (ci, ch_name) in row.channel_names.iter().enumerate() {
                            headers = headers.child(
                                div()
                                    .id(SharedString::from(format!("ch-{lid}-{j}-{ci}")))
                                    .h(px(PROPERTY_ROW_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .pl(px(36.0))
                                    .bg(theme.colors.list)
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
        let ruler = self.build_ruler(&theme.colors);
        let layer_area = self.build_layer_area(&theme.colors, self.area_origin.clone());
        let layer_headers = self.build_layer_headers(cx);

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
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                if event.modifiers.platform || event.modifiers.control {
                    let dy: f32 = delta.y.into();
                    let factor = if dy > 0.0 { 1.2 } else { 1.0 / 1.2 };
                    let cursor_x: f32 = event.position.x.into();
                    this.state
                        .zoom_at(cursor_x as f64 - HEADER_WIDTH as f64, factor);
                } else {
                    let dx: f32 = delta.x.into();
                    let frame_delta = dx as f64 / this.state.pixels_per_frame();
                    let new_offset = this.state.scroll_offset() - frame_delta;
                    this.state.set_scroll_offset(new_offset);
                }
                cx.notify();
            }))
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
                            .justify_between()
                            .px_1()
                            .bg(theme.colors.tab_bar)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(div().text_xs().text_color(theme.colors.foreground).child(
                                SharedString::from(format_timecode(
                                    self.state.playhead(),
                                    self.state.composition().frame_rate,
                                )),
                            ))
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
                            .min_h(px(content_height))
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
                                                        this.selected_keyframe = None;
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
                                                            cx,
                                                        );
                                                    }
                                                    None => this.deselect_layer(cx),
                                                }
                                            }
                                        }),
                                    )
                                    .child(layer_area),
                            ),
                    ),
            )
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn make_toggle(
    id: String,
    label: &str,
    active: bool,
    tooltip: SharedString,
    colors: &ThemeColor,
) -> Stateful<Div> {
    let text_color = if active {
        colors.accent
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

/// Fixed-layout `M:SS:FF` timecode for the header readout (unlike the ruler
/// labels, minutes are always shown so the text width stays stable).
fn format_timecode(frame: u64, fr: FrameRate) -> String {
    // Non-drop-frame timecode over the nominal integer rate: every second
    // holds exactly `nominal` frames, so the readout is continuous and
    // monotonic. Mixing wall-clock seconds with a frame modulo would jump
    // backwards around minute boundaries at fractional rates like 23.976
    // (nominal timecode intentionally drifts from wall time there).
    let nominal = fr.as_f64().round().max(1.0) as u64;
    let total_seconds = frame / nominal;
    let frames = frame % nominal;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes}:{seconds:02}:{frames:02}")
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
        assert_eq!(format_timecode(0, fr), "0:00:00");
        assert_eq!(format_timecode(29, fr), "0:00:29");
        assert_eq!(format_timecode(90, fr), "0:03:00");
        assert_eq!(format_timecode(30 * 61 + 5, fr), "1:01:05");
    }

    #[test]
    fn timecode_stays_continuous_at_fractional_rates() {
        // 23.976 fps → nominal 24; the old wall-clock/ceil mix rendered
        // 0:59:22 → 1:00:23 → 1:00:00 across this boundary.
        let fr = FrameRate::new(24000, 1001);
        assert_eq!(format_timecode(1438, fr), "0:59:22");
        assert_eq!(format_timecode(1439, fr), "0:59:23");
        assert_eq!(format_timecode(1440, fr), "1:00:00");
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

    /// A keyframe move drag (live moves + mouse-up) moves the key in layer
    /// time and rolls back with one Document undo step (REQ-LAYER-004).
    #[gpui::test]
    fn keyframe_move_drag_commits_one_undo_step(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, _window, cx| {
                let mut baseline = KeyframeCurve::new();
                baseline.insert(0, 0.0, Interpolation::Linear);
                baseline.insert(10, 100.0, Interpolation::Linear);
                panel.drag = TimelineDrag::MoveKeyframe {
                    layer: a,
                    row: row.clone(),
                    component: 0,
                    origin_frame: 10,
                    current_frame: 10,
                    baseline,
                    grab_x: 0.0,
                    changed: false,
                };
                // Two live moves (4 px/frame default zoom): +5 then +10.
                panel.drag_moved(20.0, 0.0, cx);
                panel.drag_moved(40.0, 0.0, cx);
                panel.drag_ended(cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 20));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 10));
        // The selection tracked the moved diamond.
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.selected_keyframe, Some((a, row.clone(), 0, 20)));
            })
            .unwrap();

        project.update(cx, |project, cx| {
            assert!(project.undo(cx));
        });
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 10));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 20));
        // The undo removed the selected diamond: the panel drops the stale
        // selection through its document observer.
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.selected_keyframe, None);
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

    /// Delete with a keyframe selection removes only that keyframe (the
    /// layer survives); with no selection it deletes the layer as before.
    #[gpui::test]
    fn delete_scopes_to_the_selected_keyframe(cx: &mut TestAppContext) {
        let (window, project, comp_id, a, _b) = setup(cx);
        add_position_x_keys(&project, comp_id, a, cx);
        let row = PropertyRowId::Shell(PropertyGroup::Position);

        window
            .update(cx, |panel, window, cx| {
                panel.select_layer(a, cx);
                panel.selected_keyframe = Some((a, row.clone(), 0, 10));
                panel.on_delete(&EditDelete, window, cx);
            })
            .unwrap();
        let l = layer(&project, comp_id, a, cx);
        assert!(keyframes::has_keyframe_at(&l, &row, 0, 0));
        assert!(!keyframes::has_keyframe_at(&l, &row, 0, 10));
        window
            .update(cx, |panel, _window, _cx| {
                assert_eq!(panel.selected_keyframe, None);
            })
            .unwrap();

        // No keyframe selection anymore: Delete removes the layer again.
        window
            .update(cx, |panel, window, cx| {
                panel.on_delete(&EditDelete, window, cx);
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

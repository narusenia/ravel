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
use ravel_core::id::NodeId;
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

#[derive(Clone, Copy)]
struct MoveOrigin {
    node: NodeId,
    center: (f32, f32),
}

#[derive(Clone)]
struct MoveDrag {
    network: NetworkPath,
    pointer_start: (f32, f32),
    origins: Vec<MoveOrigin>,
    local_frame: u64,
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
}

impl ViewerPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(PanelKind::Viewer, &focus_handle, window, cx);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| cx.notify());
        let tool_sub = cx.observe_global::<ToolState>(|_this, cx| cx.notify());
        let selection_sub = cx.observe_global::<CanvasSelection>(|_this, cx| cx.notify());

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
            show_grid: false,
            show_safe_areas: false,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            viewer_sub,
            tool_sub,
            selection_sub,
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
        let shell = layer_chain_comp_transform(comp, layer, position.frame, &eval);
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

        if event.modifiers.shift || hit.is_none() || !is_identity_transform(&shell) {
            return;
        }
        let origins: Vec<_> = nodes
            .iter()
            .filter_map(|id| {
                let node = graph.node(*id)?;
                shape_node_bounds(node, local_frame, &eval)?;
                Some(MoveOrigin {
                    node: *id,
                    center: (
                        sample_float_param(node, "center_x", local_frame, &eval)?,
                        sample_float_param(node, "center_y", local_frame, &eval)?,
                    ),
                })
            })
            .collect();
        if !origins.is_empty() {
            self.move_drag = Some(MoveDrag {
                network,
                pointer_start: pointer,
                origins,
                local_frame,
                changed: false,
            });
        }
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
        let ids: Vec<_> = drag.origins.iter().map(|origin| origin.node).collect();
        let mut applied = false;
        project.update(cx, |project, cx| {
            let document = project.document();
            let Some(mut graph) =
                ravel_ui::document::resolve_network(document, &drag.network).cloned()
            else {
                return;
            };
            for origin in &drag.origins {
                let Some(node) = graph.node(origin.node) else {
                    continue;
                };
                let Some(updated) = moved_shape_node(node, origin.center, delta, drag.local_frame)
                else {
                    continue;
                };
                graph = graph.replace_node(Arc::new(updated));
                applied = true;
            }
            let Some(document) =
                ravel_ui::document::replace_network(project.document(), &drag.network, graph)
            else {
                return;
            };
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
        let ids = drag.origins.iter().map(|origin| origin.node).collect();
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
        let changed = self.move_drag.take().is_some_and(|drag| drag.changed);
        if !changed {
            return;
        }
        if let Some(project) = self.project(cx) {
            project.update(cx, |project, cx| {
                project.revert_document(cx);
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
                    paint_selection_bbox(window, frame_bounds, resolution, &bbox_rects);
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
                    this.select_mouse_down(event, cx);
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
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                match event.pressed_button {
                    Some(MouseButton::Middle) => {
                        this.cancel_move(cx);
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
                        this.move_dragged(event.position, cx);
                    }
                    _ => {
                        this.pan_drag = None;
                        this.cancel_move(cx);
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
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::types::FrameRate;

fn sample_float_param(node: &Node, key: &str, frame: u64, ctx: &EvalContext) -> Option<f32> {
    let param = node.parameters.iter().find(|p| p.key == key)?;
    match &param.value {
        ParameterValue::Float(v) => Some(*v),
        ParameterValue::Channel(ch) => Some(ch.evaluate(frame, ctx)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CompRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
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

fn is_identity_transform(transform: &[f32; 6]) -> bool {
    transform
        .iter()
        .zip([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
        .all(|(actual, expected)| (actual - expected).abs() < 1e-6)
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
    shell: &[f32; 6],
) -> Option<NodeId> {
    let mut candidates: Vec<_> = graph.nodes().collect();
    candidates.sort_by_key(|node| std::cmp::Reverse(node.metadata.z));
    candidates.into_iter().find_map(|node| {
        let bounds = shape_node_bounds(node, frame, ctx)?;
        let bounds = if is_identity_transform(shell) {
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
    delta: (f32, f32),
    local_frame: u64,
) -> Option<Node> {
    let mut updated = node.clone();
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

/// Parameter-derived AABB of a shape node (half extents around the center).
/// Polygon and star use the (outer) radius as a square bound — a conservative
/// AABB that never under-covers the actual vertices.
fn shape_node_bounds(node: &Node, frame: u64, ctx: &EvalContext) -> Option<CompRect> {
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

fn layer_comp_transform(layer: &Layer, frame: u64, ctx: &EvalContext) -> [f32; 6] {
    let t = &layer.transform;
    let lf = ravel_ui::keyframes::layer_local_frame(layer, frame);
    let ax = t.anchor_point[0].evaluate(lf, ctx);
    let ay = t.anchor_point[1].evaluate(lf, ctx);
    let pos_x = t.position[0].evaluate(lf, ctx);
    let pos_y = t.position[1].evaluate(lf, ctx);
    let sx = t.scale[0].evaluate(lf, ctx);
    let sy = t.scale[1].evaluate(lf, ctx);
    let rot = t.rotation.evaluate(lf, ctx).to_radians();
    let (sin, cos) = rot.sin_cos();
    [
        cos * sx,
        -sin * sy,
        pos_x - (cos * sx * ax - sin * sy * ay),
        sin * sx,
        cos * sy,
        pos_y - (sin * sx * ax + cos * sy * ay),
    ]
}

/// Row-major 2x3 affine composition: apply `child`, then `parent`.
fn mat2x3_mul(parent: &[f32; 6], child: &[f32; 6]) -> [f32; 6] {
    [
        parent[0] * child[0] + parent[1] * child[3],
        parent[0] * child[1] + parent[1] * child[4],
        parent[0] * child[2] + parent[1] * child[5] + parent[2],
        parent[3] * child[0] + parent[4] * child[3],
        parent[3] * child[1] + parent[4] * child[4],
        parent[3] * child[2] + parent[4] * child[5] + parent[5],
    ]
}

/// The layer's shell transform composed with its parent chain, mirroring the
/// compiled `parent_transform` edges (composition/compile.rs): a parent
/// contributes only while it survives solo/mute filtering, and each layer's
/// channels sample its own local frame. The `seen` set guards against parent
/// cycles in unvalidated documents.
fn layer_chain_comp_transform(
    comp: &Composition,
    layer: &Layer,
    frame: u64,
    ctx: &EvalContext,
) -> [f32; 6] {
    let any_solo = comp.layers.iter().any(|l| l.solo);
    let is_active = |l: &Layer| !l.muted && (!any_solo || l.solo);

    let mut m = layer_comp_transform(layer, frame, ctx);
    let mut seen = HashSet::from([layer.id]);
    let mut current = layer;
    while let Some(parent_id) = current.parent {
        if !seen.insert(parent_id) {
            break;
        }
        let Some(parent) = comp.get_layer(parent_id) else {
            break;
        };
        if !is_active(parent) {
            break;
        }
        m = mat2x3_mul(&layer_comp_transform(parent, frame, ctx), &m);
        current = parent;
    }
    m
}

fn transform_rect(r: &CompRect, m: &[f32; 6]) -> CompRect {
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
        let tx = m[0] * x + m[1] * y + m[2];
        let ty = m[3] * x + m[4] * y + m[5];
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
    let shell = layer_chain_comp_transform(comp, layer, frame, &ctx);
    let is_identity = is_identity_transform(&shell);
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

fn paint_selection_bbox(
    window: &mut Window,
    frame_bounds: Bounds<Pixels>,
    comp_resolution: (u32, u32),
    rects: &[CompRect],
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
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

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
        let translated = [1.0, 0.0, 100.0, 0.0, 1.0, 50.0];

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
        let moved = moved_shape_node(&node, (10.0, 20.0), (4.5, -2.0), 7).unwrap();
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
        let moved = moved_shape_node(&node, (10.0, 20.0), (0.0, 0.0), 0).unwrap();
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

    #[test]
    fn parent_chain_transform_composes_active_parents_only() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::id::LayerId;

        let mut parent = Layer::new(LayerId::next(), "parent", Graph::new());
        parent.transform.position = [
            AnimationChannel::constant(100.0),
            AnimationChannel::constant(50.0),
        ];
        let child = Layer::new(LayerId::next(), "child", Graph::new()).with_parent(parent.id);

        let comp = comp_with_layers(vec![parent.clone(), child.clone()]);
        let m = layer_chain_comp_transform(&comp, &child, 0, &eval_ctx());
        assert_eq!((m[2], m[5]), (100.0, 50.0));
        assert!(!is_identity_transform(&m));

        // A muted parent stops the chain (mirrors compile's active filter).
        parent.muted = true;
        let comp = comp_with_layers(vec![parent, child.clone()]);
        let m = layer_chain_comp_transform(&comp, &child, 0, &eval_ctx());
        assert!(is_identity_transform(&m));
    }

    #[test]
    fn parent_cycles_terminate() {
        use ravel_core::id::LayerId;

        let a_id = LayerId::next();
        let b_id = LayerId::next();
        let a = Layer::new(a_id, "a", Graph::new()).with_parent(b_id);
        let b = Layer::new(b_id, "b", Graph::new()).with_parent(a_id);
        let comp = comp_with_layers(vec![a.clone(), b]);
        let m = layer_chain_comp_transform(&comp, &a, 0, &eval_ctx());
        assert!(is_identity_transform(&m));
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
}

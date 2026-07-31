// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The dock frame: rendering of a [`LayoutNode`] tree plus the interaction
//! events it emits.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, DispatchPhase, EventEmitter,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, SharedString, Size as GpuiSize,
    Styled as _, Subscription, Window, canvas, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, Size};
use ravel_i18n::t;
use ravel_ui::layout::{LayoutNode, Orientation, PanelInstance, PanelInstanceId};

use crate::content::PaneContent;
use crate::layout_math::{
    DropZone, SEPARATOR_PX, SPLITTER_PX, drop_highlight, drop_zone, ratio_from_position,
    splitter_thickness,
};
use crate::path::{NodePath, SplitSide, node_at, tab_drop_changes_layout};

/// How far the pointer must travel with the button held before a press on a
/// tab becomes a drag. Below this a press is just a click that activates the
/// tab.
const DRAG_START_PX: f32 = 4.0;

/// An action offered by an area's overflow menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaAction {
    /// Move the area's active tab into a new area to its right.
    SplitRight,
    /// Move the area's active tab into a new area below it.
    SplitDown,
    /// Create a second instance of the active tab's panel and move it into a
    /// new area to the right. Unlike [`AreaAction::SplitRight`] this also
    /// works for an area holding a single tab.
    DuplicateRight,
    /// Destroy the whole area, discarding every tab in it.
    Close,
}

/// A user interaction with the dock frame.
///
/// ravel-dock never writes the layout model itself: the host subscribes to
/// these events, applies them to its own model state (the helpers in
/// [`crate::path`] cover every kind), and pushes the updated tree back with
/// [`DockRoot::set_layout`].
#[derive(Debug, Clone, PartialEq)]
pub enum DockEvent {
    /// A splitter drag finished. `path` addresses the split whose ratio
    /// changed; `ratio` is the final fraction of the container axis given to
    /// the first child.
    SplitRatioChanged { path: NodePath, ratio: f32 },
    /// A tab was clicked; the host should make it the active tab of its area.
    TabActivated { instance: PanelInstanceId },
    /// A tab was dragged onto an area of this window and released. `anchor` is
    /// a tab of the destination area and `zone` says whether the tab joins
    /// that area or becomes a new area on one of its edges.
    /// [`crate::path::apply_tab_drop`] applies it.
    TabDropped {
        /// The dragged tab.
        instance: PanelInstanceId,
        /// A tab of the area under the pointer.
        anchor: PanelInstanceId,
        /// Where inside that area the pointer was released.
        zone: DropZone,
    },
    /// A tab was dragged out of this window and released.
    ///
    /// ravel-dock does not create windows and does not know about the other
    /// ones, so it reports the release position instead: the host hit-tests
    /// `screen_position` against its own window registry and either moves the
    /// tab into the window found there or detaches it into a new window.
    TabDetachRequested {
        /// The dragged tab.
        instance: PanelInstanceId,
        /// Release position in global (desktop) coordinates, derived from this
        /// window's [`Window::bounds`].
        screen_position: Point<Pixels>,
    },
    /// An item of an area's overflow menu was chosen. `instance` is that
    /// area's active tab. [`crate::path::apply_area_action`] applies it.
    AreaActionRequested {
        /// The active tab of the area whose menu was used.
        instance: PanelInstanceId,
        /// The chosen item.
        action: AreaAction,
    },
}

/// The fields of a [`LayoutNode::Split`], unpacked at the match site so
/// [`DockRoot::render_split`] stays within the argument-count lint.
struct SplitParts<'a> {
    orientation: Orientation,
    ratio: f32,
    first: &'a LayoutNode,
    second: &'a LayoutNode,
}

/// An ongoing splitter drag. The ratio preview is render-local until the
/// drag ends, then it is emitted once as [`DockEvent::SplitRatioChanged`].
#[derive(Debug, Clone)]
struct SplitterDrag {
    /// Path to the split being resized.
    path: NodePath,
    /// Which axis the drag moves along.
    orientation: Orientation,
    /// Split container's axis origin in window coordinates.
    origin: f32,
    /// Split container's axis length.
    len: f32,
    /// Separator thickness in effect for this container, so the drag math and
    /// the rendered separator agree even in a clamped narrow container.
    thickness: f32,
    /// Latest preview ratio; emitted on release.
    ratio: f32,
}

/// An ongoing tab drag. Like [`SplitterDrag`] this is render-local state: the
/// model only learns about it when the drag is released.
#[derive(Debug, Clone)]
struct TabDrag {
    /// The dragged tab.
    instance: PanelInstanceId,
    /// Pointer position when the tab was pressed, in window coordinates.
    press: Point<Pixels>,
    /// `true` once the pointer passed [`DRAG_START_PX`]. Until then the press
    /// is still just a click.
    moved: bool,
    /// `true` when the pointer left this window: releasing there detaches.
    outside: bool,
    /// Drop target resolved from the latest pointer position, or `None` when
    /// the pointer is over no area or over one that the drop would not change.
    target: Option<TabDropTarget>,
}

/// The area and zone a tab drag would land in.
#[derive(Debug, Clone, PartialEq)]
struct TabDropTarget {
    /// Path of the destination area, used to draw its highlight.
    path: NodePath,
    /// First tab of that area; the anchor carried by the emitted event.
    anchor: PanelInstanceId,
    /// Which part of the area the pointer is over.
    zone: DropZone,
}

/// Where one rendered area sits on screen.
///
/// Recorded during prepaint because pointer positions arrive between frames
/// and split/area boxes have no size until GPUI has laid them out. This is a
/// render cache, never model state.
#[derive(Debug, Clone, Copy, Default)]
struct AreaGeometry {
    /// The whole area, tab bar included.
    bounds: Bounds<Pixels>,
    /// Height of the tab bar strip along the top of `bounds`.
    tab_bar_height: Pixels,
}

impl AreaGeometry {
    /// The pane region below the tab bar. Edge drop zones are measured against
    /// this rather than the whole area, so a drop on a 20px tab bar is not
    /// read as a drop on the area's top edge.
    fn content(&self) -> Bounds<Pixels> {
        let height = (self.bounds.size.height - self.tab_bar_height).max(px(0.0));
        Bounds {
            origin: Point {
                x: self.bounds.origin.x,
                y: self.bounds.origin.y + self.tab_bar_height,
            },
            size: GpuiSize {
                width: self.bounds.size.width,
                height,
            },
        }
    }
}

/// Renders one window's [`LayoutNode`] tree: split containers with draggable
/// separators, tab bars for areas, and a placeholder for empty areas.
///
/// The pane contents come from a [`PaneContent`] implementation supplied by
/// the host; interactions leave the view as [`DockEvent`]s.
pub struct DockRoot {
    root: LayoutNode,
    content: Rc<dyn PaneContent>,
    splitter_drag: Option<SplitterDrag>,
    tab_drag: Option<TabDrag>,
    /// Last painted bounds of each split container, keyed by path.
    split_bounds: Rc<RefCell<HashMap<NodePath, Bounds<Pixels>>>>,
    /// Last painted geometry of each area, keyed by path.
    area_bounds: Rc<RefCell<HashMap<NodePath, AreaGeometry>>>,
    _escape: Subscription,
}

impl DockRoot {
    /// Creates a dock frame rendering `root` with contents from `content`.
    pub fn new(root: LayoutNode, content: Rc<dyn PaneContent>, cx: &mut Context<Self>) -> Self {
        // Escape has to abandon a drag wherever focus happens to sit, and
        // `DockRoot` deliberately owns no focus: panels own theirs, and moving
        // focus tracking onto panel instances is a later unit's job. Observing
        // keystrokes reaches the cancel path without the dock taking focus.
        let escape = cx.observe_keystrokes(|this, event, _window, cx| {
            if event.keystroke.key != "escape" || event.keystroke.modifiers.modified() {
                return;
            }
            this.cancel_drags(cx);
        });
        Self {
            root,
            content,
            splitter_drag: None,
            tab_drag: None,
            split_bounds: Rc::new(RefCell::new(HashMap::new())),
            area_bounds: Rc::new(RefCell::new(HashMap::new())),
            _escape: escape,
        }
    }

    /// The currently rendered tree.
    pub fn layout(&self) -> &LayoutNode {
        &self.root
    }

    /// Replaces the rendered tree. Hosts call this after applying a
    /// [`DockEvent`] to their model.
    pub fn set_layout(&mut self, root: LayoutNode, cx: &mut Context<Self>) {
        // Active drags and the geometry cache reference paths in the old tree;
        // drop both so a late event cannot rewrite the new layout and a stale
        // rectangle cannot resolve a drop.
        self.splitter_drag = None;
        self.tab_drag = None;
        self.split_bounds.borrow_mut().clear();
        self.area_bounds.borrow_mut().clear();
        self.root = root;
        cx.notify();
    }

    /// Abandons every in-flight drag without applying it (Escape, or a drag
    /// whose mouse button disappeared).
    fn cancel_drags(&mut self, cx: &mut Context<Self>) {
        let dragging = self.splitter_drag.is_some() || self.tab_drag.is_some();
        self.splitter_drag = None;
        self.tab_drag = None;
        if dragging {
            cx.notify();
        }
    }

    /// Ends the active splitter drag, emitting the final ratio.
    fn finish_splitter_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.splitter_drag.take() {
            cx.emit(DockEvent::SplitRatioChanged {
                path: drag.path,
                ratio: drag.ratio,
            });
            cx.notify();
        }
    }

    /// Ends the active tab drag, emitting the drop or the detach request.
    ///
    /// `position` is where the button came up. That is what decides between a
    /// drop and a detach — the tracked pointer only drives the highlight.
    fn finish_tab_drag(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        if !drag.moved && !passed_drag_threshold(drag.press, position) {
            // The press never became a drag; the tab bar's own click handler
            // already emitted `TabActivated`.
            return;
        }
        if is_outside(window.viewport_size(), position) {
            cx.emit(DockEvent::TabDetachRequested {
                instance: drag.instance,
                screen_position: window.bounds().origin + position,
            });
        } else if let Some(target) = drag.target {
            cx.emit(DockEvent::TabDropped {
                instance: drag.instance,
                anchor: target.anchor,
                zone: target.zone,
            });
        }
        cx.notify();
    }

    /// Tracks the pointer during a splitter drag. The listener sits on the root
    /// element so the drag survives the pointer leaving the separator.
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.splitter_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The button was released without us seeing the up event (for
            // example over another window); settle the preview the user has
            // already been shown.
            self.finish_splitter_drag(cx);
            return;
        }
        self.track_splitter_drag(event.position, cx);
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_splitter_drag(cx);
        self.finish_tab_drag(event.position, window, cx);
    }

    fn track_splitter_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.splitter_drag.as_mut() else {
            return;
        };
        let pointer: f32 = match drag.orientation {
            Orientation::Horizontal => position.x.into(),
            Orientation::Vertical => position.y.into(),
        };
        drag.ratio = ratio_from_position(drag.origin, drag.len, drag.thickness, pointer);
        cx.notify();
    }

    /// Tracks the pointer during a tab drag.
    ///
    /// Unlike the splitter this is driven by a window-level listener (see
    /// [`DockRoot::render_drag_layer`]) rather than an element handler: element
    /// move handlers only fire while the pointer is over the element, and a tab
    /// dragged out of the window has to keep being tracked to become a detach.
    fn track_tab_drag(
        &mut self,
        position: Point<Pixels>,
        pressed: Option<MouseButton>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_drag.is_none() {
            return;
        }
        if pressed != Some(MouseButton::Left) {
            // The button disappeared without an up event we could see. Abandon
            // the drag rather than applying a drop the user never released.
            self.cancel_drags(cx);
            return;
        }
        let Some(drag) = self.tab_drag.as_ref() else {
            return;
        };
        let instance = drag.instance;
        if !drag.moved && !passed_drag_threshold(drag.press, position) {
            return;
        }
        let outside = is_outside(window.viewport_size(), position);
        let target = if outside {
            None
        } else {
            self.resolve_drop_target(position, instance)
        };
        let drag = self.tab_drag.as_mut().expect("checked above");
        // Only a changed highlight (or cursor) needs a frame; plain pointer
        // motion inside one zone must not repaint the whole dock.
        let changed = !drag.moved || drag.outside != outside || drag.target != target;
        drag.moved = true;
        drag.outside = outside;
        drag.target = target;
        if changed {
            cx.notify();
        }
    }

    /// Maps a pointer position onto the area under it and the zone within it.
    /// Returns `None` when no area is there, or when the resulting drop would
    /// leave the tree unchanged.
    fn resolve_drop_target(
        &self,
        position: Point<Pixels>,
        dragged: PanelInstanceId,
    ) -> Option<TabDropTarget> {
        let areas = self.area_bounds.borrow();
        // Areas never overlap, so the first rectangle containing the pointer is
        // the only candidate.
        let (path, geometry) = areas
            .iter()
            .find(|(_, geometry)| geometry.bounds.contains(&position))?;
        let Some(LayoutNode::Area { tabs, .. }) = node_at(&self.root, path) else {
            return None;
        };
        let anchor = tabs.first()?.id;
        let content = geometry.content();
        let zone = if content.size.height > px(0.0) && content.contains(&position) {
            drop_zone(
                content.size.width.into(),
                content.size.height.into(),
                f32::from(position.x - content.origin.x),
                f32::from(position.y - content.origin.y),
            )
        } else {
            // The tab bar strip itself always means "join this area".
            DropZone::Center
        };
        if !tab_drop_changes_layout(&self.root, dragged, anchor, zone) {
            return None;
        }
        Some(TabDropTarget {
            path: path.clone(),
            anchor,
            zone,
        })
    }

    /// The layer that carries the two things a self-managed drag needs from the
    /// paint phase: the window-wide cursor and, for a tab drag, a window-wide
    /// pointer listener.
    ///
    /// Both have to be re-registered every frame. A drag keeps requesting
    /// frames only while its highlight changes, but GPUI keeps the last painted
    /// frame's listeners until the next draw, so the tracking survives the
    /// still periods in between.
    fn render_drag_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cursor = self.drag_cursor();
        let tracking = self.tab_drag.is_some();
        if cursor.is_none() && !tracking {
            return None;
        }
        let dock = cx.weak_entity();
        Some(
            canvas(
                |_bounds, _window, _cx| {},
                move |_bounds, _prepaint, window, _cx| {
                    if let Some(style) = cursor {
                        window.set_window_cursor_style(style);
                    }
                    if !tracking {
                        return;
                    }
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        dock.update(cx, |this, cx| {
                            this.track_tab_drag(event.position, event.pressed_button, window, cx);
                        })
                        .ok();
                    });
                },
            )
            .absolute()
            .size_full()
            .into_any_element(),
        )
    }

    /// The cursor to hold for the whole window while a drag is in flight, or
    /// `None` when no drag is active.
    ///
    /// Follows the pointer-feedback convention: a gesture that carries
    /// something shows `ClosedHand`, and a position where the gesture cannot
    /// be completed shows `OperationNotAllowed`. Leaving the window is a valid
    /// gesture (it detaches), so it keeps `ClosedHand`.
    fn drag_cursor(&self) -> Option<CursorStyle> {
        if let Some(drag) = &self.splitter_drag {
            return Some(match drag.orientation {
                Orientation::Horizontal => CursorStyle::ResizeColumn,
                Orientation::Vertical => CursorStyle::ResizeRow,
            });
        }
        let drag = self.tab_drag.as_ref()?;
        if !drag.moved {
            return None;
        }
        Some(if drag.outside || drag.target.is_some() {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OperationNotAllowed
        })
    }

    fn render_node(
        &mut self,
        node: &LayoutNode,
        path: &NodePath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            LayoutNode::Area { tabs, active } => self.render_area(tabs, *active, path, window, cx),
            LayoutNode::Split {
                orientation,
                ratio,
                first,
                second,
            } => self.render_split(
                SplitParts {
                    orientation: *orientation,
                    ratio: *ratio,
                    first,
                    second,
                },
                path,
                window,
                cx,
            ),
        }
    }

    fn render_area(
        &mut self,
        tabs: &[PanelInstance],
        active: usize,
        path: &NodePath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if tabs.is_empty() {
            return self
                .content
                .empty_state(window, cx)
                .unwrap_or_else(|| default_empty_state(cx));
        }
        let active = active.min(tabs.len() - 1);
        let tab_elements: Vec<Tab> = tabs
            .iter()
            .map(|instance| {
                let title = self.content.tab_title(instance, window, cx);
                let id = instance.id;
                Tab::new()
                    .label(title)
                    // A tab can be picked up and carried, so it advertises the
                    // grab cursor even before the drag starts.
                    .cursor(CursorStyle::OpenHand)
                    // Capture phase: gpui-component's `Tab` stops propagation
                    // on left mouse down (so tab clicks do not drag the title
                    // bar), which would swallow a bubble-phase listener here.
                    .capture_any_mouse_down(cx.listener(
                        move |this, event: &MouseDownEvent, _window, cx| {
                            if event.button == MouseButton::Left {
                                this.begin_tab_drag(id, event, cx);
                            }
                        },
                    ))
            })
            .collect();
        let ids: Vec<PanelInstanceId> = tabs.iter().map(|t| t.id).collect();
        let weak = cx.entity().downgrade();
        let tab_bar = TabBar::new(SharedString::from(format!(
            "dock-tabs-{}",
            path.id_string()
        )))
        // Panel tab bars sit on every area of every window, so their height is
        // pure overhead. The smallest size keeps them at DCC-tool density.
        .with_size(Size::XSmall)
        .selected_index(active)
        .children(tab_elements)
        .suffix(self.render_area_menu(tabs, active, path, cx))
        .on_click(move |ix: &usize, _window, cx| {
            let Some(&instance) = ids.get(*ix) else {
                return;
            };
            weak.update(cx, |_this, cx| {
                cx.emit(DockEvent::TabActivated { instance });
            })
            .ok();
        });

        let content_view = self.content.view(&tabs[active], window, cx);
        let highlight = self.drop_highlight_for(path);
        let colors = cx.theme().colors;
        let geometry = self.area_bounds.clone();
        let area_path = path.clone();
        let tab_bar_path = path.clone();
        let tab_geometry = geometry.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .relative()
            .overflow_hidden()
            .child(bounds_watcher(move |bounds| {
                geometry
                    .borrow_mut()
                    .entry(area_path.clone())
                    .or_default()
                    .bounds = bounds;
            }))
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .relative()
                    .bg(colors.tab_bar)
                    .border_b_1()
                    .border_color(colors.border)
                    .child(bounds_watcher(move |bounds| {
                        tab_geometry
                            .borrow_mut()
                            .entry(tab_bar_path.clone())
                            .or_default()
                            .tab_bar_height = bounds.size.height;
                    }))
                    .child(tab_bar),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(content_view)
                    .children(highlight.map(|zone| render_drop_highlight(zone, cx))),
            )
            .into_any_element()
    }

    /// The overflow menu button placed at the right end of an area's tab bar.
    fn render_area_menu(
        &self,
        tabs: &[PanelInstance],
        active: usize,
        path: &NodePath,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let instance = tabs[active].id;
        // Splitting moves the active tab out of the area, which needs another
        // tab to stay behind. A lone tab has to be duplicated instead.
        let can_split = tabs.len() >= 2;
        let weak = cx.entity().downgrade();
        Button::new(SharedString::from(format!(
            "dock-area-menu-{}",
            path.id_string()
        )))
        .xsmall()
        .ghost()
        .icon(Icon::new(IconName::Ellipsis))
        .dropdown_menu(move |mut menu, _window, _cx| {
            for (action, key) in [
                (AreaAction::SplitRight, "dock.area_menu.split_right"),
                (AreaAction::SplitDown, "dock.area_menu.split_down"),
                (AreaAction::DuplicateRight, "dock.area_menu.duplicate_right"),
                (AreaAction::Close, "dock.area_menu.close"),
            ] {
                if action == AreaAction::Close {
                    menu = menu.separator();
                }
                let weak = weak.clone();
                let disabled =
                    !can_split && matches!(action, AreaAction::SplitRight | AreaAction::SplitDown);
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(t!(key)))
                        .disabled(disabled)
                        .on_click(move |_, _window, cx| {
                            weak.update(cx, |_this, cx| {
                                cx.emit(DockEvent::AreaActionRequested { instance, action });
                            })
                            .ok();
                        }),
                );
            }
            menu
        })
    }

    /// The zone to highlight inside the area at `path`, if a tab drag is
    /// hovering it.
    fn drop_highlight_for(&self, path: &NodePath) -> Option<DropZone> {
        let target = self.tab_drag.as_ref()?.target.as_ref()?;
        (target.path == *path).then_some(target.zone)
    }

    fn render_split(
        &mut self,
        split: SplitParts<'_>,
        path: &NodePath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let SplitParts {
            orientation,
            ratio,
            first,
            second,
        } = split;
        let live_ratio = match &self.splitter_drag {
            Some(drag) if drag.path == *path => drag.ratio,
            _ => ratio,
        };
        let horizontal = matches!(orientation, Orientation::Horizontal);

        // The separator width is clamped against the container, whose pixel
        // span only exists after layout, so the value comes from the last
        // painted frame. A container this narrow cannot be dragged anyway.
        let axis_len = self.split_bounds.borrow().get(path).map(|bounds| {
            if horizontal {
                f32::from(bounds.size.width)
            } else {
                f32::from(bounds.size.height)
            }
        });
        let thickness = match axis_len {
            Some(len) if len > 0.0 => splitter_thickness(len),
            _ => SPLITTER_PX,
        };

        let bounds_cache = self.split_bounds.clone();
        let split_path = path.clone();
        let bounds_recorder = bounds_watcher(move |bounds| {
            bounds_cache.borrow_mut().insert(split_path.clone(), bounds);
        });

        let first_el = self.render_node(first, &path.child(SplitSide::First), window, cx);
        let second_el = self.render_node(second, &path.child(SplitSide::Second), window, cx);
        let splitter = self.render_splitter(path.clone(), orientation, live_ratio, thickness, cx);

        let first_box = div().flex_shrink_0().overflow_hidden().child(first_el);
        let first_box = if horizontal {
            first_box.w(relative(live_ratio)).h_full()
        } else {
            first_box.h(relative(live_ratio)).w_full()
        };
        let second_box = div().flex_1().overflow_hidden().child(second_el);
        let second_box = if horizontal {
            second_box.h_full().min_w_0()
        } else {
            second_box.w_full().min_h_0()
        };

        let container = div().relative().flex().size_full();
        let container = if horizontal {
            container.flex_row()
        } else {
            container.flex_col()
        };
        container
            .child(bounds_recorder)
            .child(first_box)
            .child(splitter)
            .child(second_box)
            .into_any_element()
    }

    fn render_splitter(
        &mut self,
        path: NodePath,
        orientation: Orientation,
        ratio: f32,
        thickness: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let horizontal = matches!(orientation, Orientation::Horizontal);
        let colors = cx.theme().colors;
        let hover_bg = colors.accent.opacity(0.25);
        let bounds_cache = self.split_bounds.clone();

        let hit = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .hover(|style| style.bg(hover_bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    let Some(bounds) = bounds_cache.borrow().get(&path).copied() else {
                        return;
                    };
                    let (origin, len) = if horizontal {
                        (f32::from(bounds.origin.x), f32::from(bounds.size.width))
                    } else {
                        (f32::from(bounds.origin.y), f32::from(bounds.size.height))
                    };
                    if len <= 0.0 {
                        return;
                    }
                    this.splitter_drag = Some(SplitterDrag {
                        path: path.clone(),
                        orientation,
                        origin,
                        len,
                        thickness,
                        ratio,
                    });
                    cx.notify();
                }),
            );
        let hit = if horizontal {
            hit.w(px(thickness)).h_full().cursor_col_resize()
        } else {
            hit.h(px(thickness)).w_full().cursor_row_resize()
        };

        let line = if horizontal {
            div().w(px(SEPARATOR_PX.min(thickness))).h_full()
        } else {
            div().h(px(SEPARATOR_PX.min(thickness))).w_full()
        };
        hit.child(line.bg(colors.border))
    }

    fn begin_tab_drag(
        &mut self,
        instance: PanelInstanceId,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.tab_drag = Some(TabDrag {
            instance,
            press: event.position,
            moved: false,
            outside: false,
            target: None,
        });
        // Nothing looks different yet, but the frame is what installs the
        // window-level pointer listener the drag is tracked by.
        cx.notify();
    }
}

impl EventEmitter<DockEvent> for DockRoot {}

impl Render for DockRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.root.clone();
        let colors = cx.theme().colors;
        div()
            .id("dock-root")
            .relative()
            .size_full()
            .bg(colors.background)
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(self.render_node(&root, &NodePath::root(), window, cx))
            .children(self.render_drag_layer(cx))
    }
}

/// `true` once the pointer has travelled far enough from `press` for the
/// gesture to count as a drag rather than a click.
fn passed_drag_threshold(press: Point<Pixels>, pointer: Point<Pixels>) -> bool {
    let dx = f32::from(pointer.x - press.x);
    let dy = f32::from(pointer.y - press.y);
    dx.abs() >= DRAG_START_PX || dy.abs() >= DRAG_START_PX
}

/// `true` when `position` is outside a window whose drawable area is `viewport`.
fn is_outside(viewport: GpuiSize<Pixels>, position: Point<Pixels>) -> bool {
    position.x < px(0.0)
        || position.y < px(0.0)
        || position.x > viewport.width
        || position.y > viewport.height
}

/// An invisible full-size layer that reports its laid-out bounds every frame.
fn bounds_watcher(record: impl Fn(Bounds<Pixels>) + 'static) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| record(bounds),
        |_bounds, _prepaint, _window, _cx| {},
    )
    .absolute()
    .size_full()
}

/// The translucent band showing where a dropped tab would land.
fn render_drop_highlight(zone: DropZone, cx: &App) -> AnyElement {
    let (left, top, width, height) = drop_highlight(zone);
    let accent = cx.theme().colors.accent;
    div()
        .absolute()
        .left(relative(left))
        .top(relative(top))
        .w(relative(width))
        .h(relative(height))
        .bg(accent.opacity(0.25))
        .border_1()
        .border_color(accent)
        .into_any_element()
}

/// Placeholder for an area without tabs when the [`PaneContent`] does not
/// provide one.
fn default_empty_state(cx: &App) -> AnyElement {
    div()
        .size_full()
        .bg(cx.theme().muted.opacity(0.2))
        .into_any_element()
}

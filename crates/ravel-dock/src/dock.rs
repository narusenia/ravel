// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The dock frame: rendering of a [`LayoutNode`] tree plus the interaction
//! events it emits.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, EventEmitter, InteractiveElement as _, IntoElement,
    MouseButton, MouseMoveEvent, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    Window, canvas, div, px, relative,
};
use gpui_component::ActiveTheme as _;
use gpui_component::tab::TabBar;
use ravel_ui::layout::{LayoutNode, Orientation, PanelInstance, PanelInstanceId};

use crate::content::PaneContent;
use crate::layout_math::{SEPARATOR_PX, SPLITTER_PX};
use crate::path::{NodePath, SplitSide};

/// A user interaction with the dock frame.
///
/// ravel-dock never writes the layout model itself: the host subscribes to
/// these events, applies them to its own model state (the helpers in
/// [`crate::path`] cover both kinds), and pushes the updated tree back with
/// [`DockRoot::set_layout`].
#[derive(Debug, Clone, PartialEq)]
pub enum DockEvent {
    /// A splitter drag finished. `path` addresses the split whose ratio
    /// changed; `ratio` is the final fraction of the container axis given to
    /// the first child.
    SplitRatioChanged { path: NodePath, ratio: f32 },
    /// A tab was clicked; the host should make it the active tab of its area.
    TabActivated { instance: PanelInstanceId },
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
    /// Latest preview ratio; emitted on release.
    ratio: f32,
}

/// Renders one window's [`LayoutNode`] tree: split containers with draggable
/// separators, tab bars for areas, and a placeholder for empty areas.
///
/// The pane contents come from a [`PaneContent`] implementation supplied by
/// the host; interactions leave the view as [`DockEvent`]s.
pub struct DockRoot {
    root: LayoutNode,
    content: Rc<dyn PaneContent>,
    drag: Option<SplitterDrag>,
}

impl DockRoot {
    /// Creates a dock frame rendering `root` with contents from `content`.
    pub fn new(root: LayoutNode, content: Rc<dyn PaneContent>) -> Self {
        Self {
            root,
            content,
            drag: None,
        }
    }

    /// The currently rendered tree.
    pub fn layout(&self) -> &LayoutNode {
        &self.root
    }

    /// Replaces the rendered tree. Hosts call this after applying a
    /// [`DockEvent`] to their model.
    pub fn set_layout(&mut self, root: LayoutNode, cx: &mut Context<Self>) {
        self.root = root;
        cx.notify();
    }

    /// Ends the active splitter drag, emitting the final ratio.
    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag.take() {
            cx.emit(DockEvent::SplitRatioChanged {
                path: drag.path,
                ratio: drag.ratio,
            });
            cx.notify();
        }
    }

    /// Tracks the pointer during a splitter drag. The listener sits on the
    /// root element so the drag survives the pointer leaving the separator.
    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drag.is_none() {
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            let drag = self.drag.as_mut().expect("checked above");
            let pointer: f32 = match drag.orientation {
                Orientation::Horizontal => event.position.x.into(),
                Orientation::Vertical => event.position.y.into(),
            };
            drag.ratio = crate::layout_math::ratio_from_position(
                drag.origin,
                drag.len,
                SPLITTER_PX,
                pointer,
            );
            cx.notify();
        } else {
            // The button was released without us seeing the up event (for
            // example outside the window); settle the drag.
            self.finish_drag(cx);
        }
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
        let titles: Vec<SharedString> = tabs
            .iter()
            .map(|t| self.content.tab_title(t, window, cx))
            .collect();
        let ids: Vec<PanelInstanceId> = tabs.iter().map(|t| t.id).collect();
        let weak = cx.entity().downgrade();
        let tab_bar = TabBar::new(SharedString::from(format!(
            "dock-tabs-{}",
            path.id_string()
        )))
        .selected_index(active)
        .children(titles)
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
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .bg(colors.tab_bar)
                    .border_b_1()
                    .border_color(colors.border)
                    .child(tab_bar),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(content_view),
            )
            .into_any_element()
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
        let live_ratio = match &self.drag {
            Some(drag) if drag.path == *path => drag.ratio,
            _ => ratio,
        };
        let horizontal = matches!(orientation, Orientation::Horizontal);

        // The drag math needs the container's pixel span, which only exists
        // after layout. An invisible canvas records it every frame.
        let bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        let bounds_watcher = {
            let cell = bounds_cell.clone();
            canvas(
                move |bounds, _window, _cx| cell.set(bounds),
                |_bounds, _prepaint, _window, _cx| {},
            )
            .absolute()
            .size_full()
        };

        let first_el = self.render_node(first, &path.child(SplitSide::First), window, cx);
        let second_el = self.render_node(second, &path.child(SplitSide::Second), window, cx);
        let splitter = self.render_splitter(path.clone(), orientation, live_ratio, bounds_cell, cx);

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
            .child(bounds_watcher)
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
        bounds: Rc<Cell<Bounds<Pixels>>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let horizontal = matches!(orientation, Orientation::Horizontal);
        let colors = cx.theme().colors;
        let hover_bg = colors.accent.opacity(0.25);

        let hit = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .hover(|style| style.bg(hover_bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    let bounds = bounds.get();
                    let (origin, len) = if horizontal {
                        (f32::from(bounds.origin.x), f32::from(bounds.size.width))
                    } else {
                        (f32::from(bounds.origin.y), f32::from(bounds.size.height))
                    };
                    if len <= 0.0 {
                        return;
                    }
                    this.drag = Some(SplitterDrag {
                        path: path.clone(),
                        orientation,
                        origin,
                        len,
                        ratio,
                    });
                    cx.notify();
                }),
            );
        let hit = if horizontal {
            hit.w(px(SPLITTER_PX)).h_full().cursor_col_resize()
        } else {
            hit.h(px(SPLITTER_PX)).w_full().cursor_row_resize()
        };

        let line = if horizontal {
            div().w(px(SEPARATOR_PX)).h_full()
        } else {
            div().h(px(SEPARATOR_PX)).w_full()
        };
        hit.child(line.bg(colors.border))
    }
}

impl EventEmitter<DockEvent> for DockRoot {}

impl Render for DockRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.root.clone();
        let colors = cx.theme().colors;
        div()
            .id("dock-root")
            .size_full()
            .bg(colors.background)
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| this.finish_drag(cx)),
            )
            .child(self.render_node(&root, &NodePath::root(), window, cx))
    }
}

/// Placeholder for an area without tabs when the [`PaneContent`] does not
/// provide one.
fn default_empty_state(cx: &App) -> AnyElement {
    div()
        .size_full()
        .bg(cx.theme().muted.opacity(0.2))
        .into_any_element()
}

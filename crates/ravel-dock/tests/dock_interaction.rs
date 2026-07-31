// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tab drag and drop through real GPUI input routing.
//!
//! The geometry (`layout_math`) and the model application (`path`) are covered
//! by unit tests. What only a window can exercise is the middle: pressing a
//! tab, tracking the pointer against the areas GPUI actually laid out, and
//! deciding what to emit on release. These tests drive that with simulated
//! platform events against a deterministically sized window.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyView, App, AppContext as _, Bounds, Context, Entity, IntoElement, MouseButton,
    MouseMoveEvent, ParentElement as _, Pixels, Point, Render, SharedString, Size, Styled as _,
    Subscription, TestApp, TestAppWindow, Window, WindowBounds, WindowOptions, div, point, px,
};
use ravel_dock::{DockEvent, DockRoot, DropZone, PaneContent};
use ravel_ui::layout::{LayoutNode, Orientation, PanelInstance, PanelInstanceId};
use ravel_ui::panel::PanelKind;

/// Window size every test uses, so pixel positions in the assertions are
/// meaningful.
const WINDOW: Size<Pixels> = Size {
    width: px(800.0),
    height: px(600.0),
};

/// Minimal pane content: one cached blank view per instance.
#[derive(Default)]
struct TestContent {
    views: RefCell<Vec<(PanelInstanceId, AnyView)>>,
}

struct BlankPane;

impl Render for BlankPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

impl PaneContent for TestContent {
    fn tab_title(&self, instance: &PanelInstance, _window: &Window, _cx: &App) -> SharedString {
        format!("tab {}", instance.id.0).into()
    }

    fn view(&self, instance: &PanelInstance, _window: &mut Window, cx: &mut App) -> AnyView {
        let mut views = self.views.borrow_mut();
        if let Some((_, view)) = views.iter().find(|(id, _)| *id == instance.id) {
            return view.clone();
        }
        let view = AnyView::from(cx.new(|_cx| BlankPane));
        views.push((instance.id, view.clone()));
        view
    }
}

/// The host around the dock: owns the model side and records what the dock
/// emits, exactly like `examples/gallery` and (after cutover) `ravel-app`.
struct Host {
    dock: Entity<DockRoot>,
    events: Rc<RefCell<Vec<DockEvent>>>,
    _subscription: Subscription,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock.clone())
    }
}

fn inst(id: u64) -> PanelInstance {
    PanelInstance::new(PanelInstanceId(id), PanelKind::Viewer)
}

/// `[Area(0, 1) | Area(2)]`, split down the middle. With an 800px window the
/// left area spans x `0..400` and the right one x `405..800`.
fn two_area_tree() -> LayoutNode {
    LayoutNode::split(
        Orientation::Horizontal,
        0.5,
        LayoutNode::area(vec![inst(0), inst(1)]),
        LayoutNode::area(vec![inst(2)]),
    )
}

/// Opens an 800×600 window hosting `root` and returns it with the event log.
fn open(root: LayoutNode) -> (TestApp, TestAppWindow<Host>, Rc<RefCell<Vec<DockEvent>>>) {
    let mut app = TestApp::new();
    app.update(gpui_component::init);
    let events: Rc<RefCell<Vec<DockEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let mut window = app.open_window_with_options(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: WINDOW,
            })),
            ..Default::default()
        },
        move |_window, cx| {
            let dock = cx.new(|cx| DockRoot::new(root, Rc::new(TestContent::default()), cx));
            let sink = recorded.clone();
            let subscription = cx.subscribe(&dock, move |_this, _dock, event: &DockEvent, _cx| {
                sink.borrow_mut().push(event.clone());
            });
            Host {
                dock,
                events: recorded.clone(),
                _subscription: subscription,
            }
        },
    );
    window.draw();
    (app, window, events)
}

/// Somewhere inside the first tab of the area whose tab bar starts at `left`.
fn first_tab(left: f32) -> Point<Pixels> {
    point(px(left + 4.0), px(8.0))
}

fn drag_to(window: &mut TestAppWindow<Host>, position: Point<Pixels>) {
    window.simulate_event(MouseMoveEvent {
        position,
        modifiers: Default::default(),
        pressed_button: Some(MouseButton::Left),
    });
    window.draw();
}

#[test]
fn dropping_a_tab_in_the_middle_of_another_area_merges_it() {
    let (_app, mut window, events) = open(two_area_tree());

    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, point(px(600.0), px(300.0)));
    window.simulate_mouse_up(point(px(600.0), px(300.0)), MouseButton::Left);

    assert_eq!(
        events.borrow().as_slice(),
        [DockEvent::TabDropped {
            instance: PanelInstanceId(0),
            anchor: PanelInstanceId(2),
            zone: DropZone::Center,
        }]
    );
}

#[test]
fn dropping_a_tab_on_an_area_edge_asks_for_a_split() {
    // The right area spans x 405..800, so its right quarter starts at 701 and
    // its bottom quarter at y 165 + 3/4 of the remaining height.
    let cases = [
        (point(px(760.0), px(300.0)), DropZone::Right),
        (point(px(430.0), px(300.0)), DropZone::Left),
        (point(px(600.0), px(40.0)), DropZone::Top),
        (point(px(600.0), px(570.0)), DropZone::Bottom),
    ];
    for (position, expected) in cases {
        let (_app, mut window, events) = open(two_area_tree());

        window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
        drag_to(&mut window, position);
        window.simulate_mouse_up(position, MouseButton::Left);

        assert_eq!(
            events.borrow().as_slice(),
            [DockEvent::TabDropped {
                instance: PanelInstanceId(0),
                anchor: PanelInstanceId(2),
                zone: expected,
            }],
            "drop at {position:?} should be {expected:?}"
        );
    }
}

#[test]
fn a_drop_on_the_tab_bar_of_another_area_merges_rather_than_splitting() {
    let (_app, mut window, events) = open(two_area_tree());

    // The tab bar is only 20px tall, well inside the top quarter of the area,
    // but dropping on a tab strip has to mean "join this area".
    let on_tab_bar = point(px(600.0), px(8.0));
    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, on_tab_bar);
    window.simulate_mouse_up(on_tab_bar, MouseButton::Left);

    assert_eq!(
        events.borrow().as_slice(),
        [DockEvent::TabDropped {
            instance: PanelInstanceId(0),
            anchor: PanelInstanceId(2),
            zone: DropZone::Center,
        }]
    );
}

#[test]
fn dragging_a_tab_out_of_the_window_requests_a_detach() {
    let (_app, mut window, events) = open(two_area_tree());

    let outside = point(px(900.0), px(300.0));
    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, outside);
    window.simulate_mouse_up(outside, MouseButton::Left);

    let recorded = events.borrow();
    let [DockEvent::TabDetachRequested { instance, .. }] = recorded.as_slice() else {
        panic!("expected exactly one detach request, got {recorded:?}");
    };
    assert_eq!(*instance, PanelInstanceId(0));
}

#[test]
fn pressing_a_tab_without_moving_only_activates_it() {
    let (_app, mut window, events) = open(two_area_tree());

    let tab = first_tab(0.0);
    window.simulate_mouse_down(tab, MouseButton::Left);
    window.simulate_mouse_up(tab, MouseButton::Left);

    assert_eq!(
        events.borrow().as_slice(),
        [DockEvent::TabActivated {
            instance: PanelInstanceId(0)
        }],
        "a press below the drag threshold is still a click"
    );
}

#[test]
fn escape_abandons_a_tab_drag() {
    let (_app, mut window, events) = open(two_area_tree());

    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, point(px(600.0), px(300.0)));
    window.simulate_keystroke("escape");
    window.simulate_mouse_up(point(px(600.0), px(300.0)), MouseButton::Left);

    assert!(
        events.borrow().is_empty(),
        "a cancelled drag must not reach the model: {:?}",
        events.borrow()
    );
}

#[test]
fn losing_the_mouse_button_abandons_a_tab_drag() {
    let (_app, mut window, events) = open(two_area_tree());

    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, point(px(600.0), px(300.0)));
    // A move without the button held means the release happened where we could
    // not see it; the drag must not survive to be applied later.
    window.simulate_mouse_move(point(px(620.0), px(300.0)));
    window.draw();
    window.simulate_mouse_up(point(px(620.0), px(300.0)), MouseButton::Left);

    assert!(
        events.borrow().is_empty(),
        "a drag that lost its button must not reach the model: {:?}",
        events.borrow()
    );
}

#[test]
fn a_drop_that_would_not_change_the_layout_emits_nothing() {
    let (_app, mut window, events) = open(two_area_tree());

    // Tab 0 dragged into the middle of its own area: nothing to apply.
    let own_area = point(px(200.0), px(300.0));
    window.simulate_mouse_down(first_tab(0.0), MouseButton::Left);
    drag_to(&mut window, own_area);
    window.simulate_mouse_up(own_area, MouseButton::Left);

    assert!(
        events.borrow().is_empty(),
        "a no-op drop must not round-trip through the model: {:?}",
        events.borrow()
    );
}

#[test]
fn a_splitter_drag_emits_the_final_ratio_once() {
    let (_app, mut window, events) = open(two_area_tree());

    // The separator of an 800px-wide even split sits at x 400..405.
    let on_splitter = point(px(402.0), px(300.0));
    window.simulate_mouse_down(on_splitter, MouseButton::Left);
    drag_to(&mut window, point(px(300.0), px(300.0)));
    drag_to(&mut window, point(px(200.0), px(300.0)));
    window.simulate_mouse_up(point(px(200.0), px(200.0)), MouseButton::Left);

    let recorded = events.borrow();
    let [DockEvent::SplitRatioChanged { ratio, .. }] = recorded.as_slice() else {
        panic!("expected exactly one ratio change, got {recorded:?}");
    };
    // (200 - splitter/2) / 800, the inverse of the render math.
    assert!(
        (ratio - 0.2469).abs() < 1e-3,
        "unexpected final ratio {ratio}"
    );
}

#[test]
fn escape_abandons_a_splitter_drag() {
    let (_app, mut window, events) = open(two_area_tree());

    window.simulate_mouse_down(point(px(402.0), px(300.0)), MouseButton::Left);
    drag_to(&mut window, point(px(200.0), px(300.0)));
    window.simulate_keystroke("escape");
    window.simulate_mouse_up(point(px(200.0), px(300.0)), MouseButton::Left);

    assert!(
        events.borrow().is_empty(),
        "a cancelled splitter drag must not write a ratio: {:?}",
        events.borrow()
    );
}

#[test]
fn host_state_is_reachable_from_the_dock_entity() {
    // Guards the wiring the other tests rely on: the host really does observe
    // the same dock entity it renders.
    let (_app, window, events) = open(two_area_tree());
    let same = window.read(|host, cx| {
        Rc::ptr_eq(&host.events, &events) && host.dock.read(cx).layout().area_count() == 2
    });
    assert!(same);
}

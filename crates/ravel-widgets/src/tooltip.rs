// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's tooltip.
//!
//! Three parts, and the split is deliberate:
//!
//! - [`install_tooltip_overlay`] gives a window **one** `gpui_base::TooltipOverlay`
//!   — the state machine that owns the show delay, the grace period, the
//!   placement and the window clamping. Every trigger in that window shares it,
//!   which is what makes moving along a toolbar pay [`SHOW_DELAY`] once instead
//!   of once per button;
//! - [`Tooltip`] is the **popup's content**: the text, and the Escape state
//!   machine. The floating surface around it — the raised surface, the 1px
//!   border, the type — is dressed by the overlay's renderer
//!   ([`render_popup`]), so every tooltip in the window reads the tokens
//!   through one path;
//! - [`TooltipExt::ravel_tooltip`] is the **trigger side**: it records its own
//!   bounds during prepaint and asks the window's overlay to show or hide.
//!
//! # Why not GPUI's own `.tooltip()`
//!
//! GPUI's machinery is per element: each element owns its own delay timer, so
//! the pointer moving from one toolbar button to the next pays the delay again
//! at every stop. The shared overlay is the same construction gpui-component
//! uses, and its grace period is the whole point — see [`GRACE_PERIOD`].
//!
//! # Why Ravel installs its own overlay
//!
//! `gpui_component::Root` has one, but `Root::tooltip_overlay` is `pub(crate)`
//! and not every Ravel window has a `Root` at all (`examples/gallery` is a bare
//! GPUI window). A window-keyed table of Ravel's own overlays covers both hosts
//! without touching the fork.
//!
//! # Escape
//!
//! `gpui_base`'s overlay has no key handling, so the popup dismisses *itself*:
//! [`Tooltip::on_key`] is the state machine, and the showing watches the
//! keyboard through `Context::observe_keystrokes` for as long as it exists.
//! It cannot be an element key listener: `Window::on_key_event` attaches to
//! the dispatch node being painted, and the popup is drawn in a `deferred`
//! subtree that is never on the path from the focused element to the root, so
//! nothing it registered would ever be reached. A dismissal folds the
//! window's overlay away; the next hover asks the overlay for a fresh popup,
//! so Escape hides this showing rather than the tooltip forever.
//!
//! **On macOS the keystroke may never arrive at all**, and that is not this
//! module's doing: a bare Escape has no `key_char`, so `gpui_macos` hands it
//! to the window's input context first, and with no text input focused the
//! IME swallows it without calling back into GPUI. Nothing downstream of
//! `Window::dispatch_key_event` runs — element key listeners included, which
//! is why the tooltip behaved the same way before it moved to the shared
//! overlay. The path here is the one that works the moment the keystroke is
//! delivered.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Context, Entity, Global,
    InteractiveElement as _, IntoElement, Keystroke, MouseButton, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled as _, Subscription, WeakEntity, Window,
    WindowId, div, px,
};
use gpui_base::{
    ElementExt, Tooltip as BaseTooltip, TooltipOverlay, TooltipRequest, TooltipTransition,
};

use crate::theme::ActiveTokens as _;

/// How long the pointer must rest on a trigger before its tooltip appears.
///
/// **`gpui-base` owns this number.** It is a private const of the shared
/// `TooltipOverlay`, so Ravel cannot set it; what this const does is *record*
/// the value the overlay actually uses, because 500ms is a decision
/// `docs/implementation/ui-component-layer-plan.md` states and the gallery
/// caption quotes. [`tests::the_delay_is_the_one_the_shared_overlay_pays`]
/// fails if the two ever disagree.
pub const SHOW_DELAY: Duration = Duration::from_millis(500);

/// How long after the pointer leaves a trigger the next one still shows
/// without paying [`SHOW_DELAY`] again.
///
/// This is the number that makes a toolbar usable: the tooltip follows the
/// pointer from button to button instantly, and only a real pause starts the
/// delay over. Owned and recorded exactly like [`SHOW_DELAY`].
pub const GRACE_PERIOD: Duration = Duration::from_millis(300);

/// The gap the popup keeps from the trigger and from the window edge.
pub const WINDOW_MARGIN: Pixels = px(4.0);

/// Vertical padding inside the popup.
const POPUP_PADDING_Y: Pixels = px(2.0);

// ---------------------------------------------------------------------------
// The window's overlay
// ---------------------------------------------------------------------------

/// One window's tooltip machinery.
#[derive(Clone)]
struct WindowTooltip {
    overlay: WeakEntity<TooltipOverlay>,
    /// The trigger whose showing is up, by its bounds.
    ///
    /// A trigger asks the overlay to hide only while this still names *it*.
    /// Without that check, moving the pointer straight from one trigger to the
    /// next takes the new showing down: the two hover callbacks are queued in
    /// the same frame and the *entered* one can run first, so the *left* one's
    /// `request_hide` would arm the grace timer against a tooltip that is
    /// already the new trigger's. The bounds are identity enough — two
    /// triggers that are up at once cannot occupy the same rectangle.
    showing: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// The tooltip machinery of every window that has any.
///
/// Keyed by GPUI's window id rather than reached through the root view,
/// because Ravel's two hosts have nothing in common to downcast to: one is a
/// `gpui_component::Root`, the other a bare example window. Rows hold weak
/// handles so a closed window's overlay is dropped with its host.
#[derive(Default)]
struct TooltipOverlays(HashMap<WindowId, WindowTooltip>);

impl Global for TooltipOverlays {}

/// Give this window the overlay its tooltips will share.
///
/// Call it once while building the window's root view and **render the entity
/// it returns** — the overlay is where the popup is drawn, so a host that
/// registers one without placing it shows nothing. Calling it again replaces
/// the window's overlay, which is what a rebuilt root view wants.
pub fn install_tooltip_overlay(window: &mut Window, cx: &mut App) -> Entity<TooltipOverlay> {
    let overlay = cx.new(|_| TooltipOverlay::new().render_with(render_popup));
    let id = window.window_handle().window_id();
    let overlays = cx.default_global::<TooltipOverlays>();
    // A window that closed took its overlay with it; without this its row
    // would sit in the table for the rest of the session.
    overlays
        .0
        .retain(|_, window| window.overlay.upgrade().is_some());
    overlays.0.insert(
        id,
        WindowTooltip {
            overlay: overlay.downgrade(),
            showing: Rc::new(Cell::new(None)),
        },
    );
    overlay
}

/// The overlay `window`'s tooltips go through, if it has one.
///
/// `None` is an ordinary answer, not a failure: a window built without
/// [`install_tooltip_overlay`] — a test that renders one widget, a tool that
/// opens a window of its own — simply shows no tooltips. Panicking here would
/// take the whole window down on a hover.
pub fn tooltip_overlay(window: &Window, cx: &App) -> Option<Entity<TooltipOverlay>> {
    window_tooltip(window, cx).and_then(|window| window.overlay.upgrade())
}

/// This window's row of the table.
fn window_tooltip(window: &Window, cx: &App) -> Option<WindowTooltip> {
    cx.try_global::<TooltipOverlays>()?
        .0
        .get(&window.window_handle().window_id())
        .cloned()
}

/// Fold this window's tooltip away at once, skipping the grace period.
fn hide_tooltip(window: &Window, cx: &mut App) {
    let Some(this) = window_tooltip(window, cx) else {
        return;
    };
    let Some(overlay) = this.overlay.upgrade() else {
        return;
    };
    this.showing.set(None);
    overlay.update(cx, |overlay, cx| overlay.hide(cx));
}

/// Dress the popup in Ravel's floating surface.
///
/// The whole appearance is here rather than in [`Tooltip`]'s own `render`:
/// the surface belongs to the window's overlay, so a caller who one day hands
/// the overlay a content view of its own gets the same popup. The text style
/// set here does reach that content view — checked in the gallery, because
/// GPUI's inheritance across a view boundary is not obvious.
///
/// The overlay hands over the content view and the transition it is making.
/// The transition is deliberately ignored: UX invariant 11 spends the motion
/// budget on state feedback, and a 20px popup sliding into place under the
/// pointer reads as jitter rather than as height.
fn render_popup(
    content: AnyView,
    _transition: TooltipTransition,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.tokens().clone();
    let colors = theme.colors;

    BaseTooltip::new("ravel-tooltip")
        // The margin is the gap from the trigger: `TooltipPositioner` places
        // the popup flush against it otherwise.
        .m(WINDOW_MARGIN)
        .px(theme.spacing.xs)
        .py(POPUP_PADDING_Y)
        .rounded(theme.radius.radius)
        // A floating surface needs an edge to read against a panel of a
        // similar tone. It gets a border rather than a shadow: a shadow under
        // a 20px popup in a dense tool reads as blur, not as height.
        .border_1()
        .border_color(colors.border)
        .bg(colors.raised_surface())
        .text_size(theme.text.font_size)
        .text_color(colors.foreground)
        .child(content)
        .into_any_element()
}

// ---------------------------------------------------------------------------
// The popup's content
// ---------------------------------------------------------------------------

/// Ravel's tooltip content.
///
/// Built through [`Tooltip::build`], which is what the overlay's request
/// callback hands back.
pub struct Tooltip {
    text: SharedString,
    dismissed: bool,
    /// Watches the keyboard for the Escape that dismisses this showing.
    ///
    /// Held here so it lasts exactly as long as the showing does: the overlay
    /// drops the view when the tooltip folds away, and the subscription goes
    /// with it.
    keystrokes: Option<Subscription>,
}

impl Tooltip {
    /// A tooltip showing `text`.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            dismissed: false,
            keystrokes: None,
        }
    }

    /// Turn this into the view the overlay's request callback returns.
    pub fn build(mut self, _window: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|cx| {
            self.keystrokes = Some(cx.observe_keystrokes(
                |tooltip: &mut Self, event, window, cx| {
                    if !tooltip.on_key(&event.keystroke) {
                        return;
                    }
                    cx.notify();
                    // The content renders nothing once dismissed, but the overlay
                    // still holds it: without this the popup's space would stay
                    // reserved and the grace period would keep it there.
                    hide_tooltip(window, cx);
                },
            ));
            self
        })
        .into()
    }

    /// Whether this showing has been dismissed.
    pub fn is_dismissed(&self) -> bool {
        self.dismissed
    }

    /// Feed a keystroke to the popup; returns whether the popup changed.
    ///
    /// A bare Escape dismisses. Only a bare one: the listener is registered on
    /// the whole window while a tooltip is up, so anything with a modifier
    /// belongs to whatever chord the user is actually typing. And only the
    /// first Escape does anything — a popup that reported a change on every
    /// keystroke would repaint the window on every keystroke.
    pub fn on_key(&mut self, keystroke: &Keystroke) -> bool {
        if keystroke.key != "escape" || keystroke.modifiers.modified() || self.dismissed {
            return false;
        }
        self.dismissed = true;
        true
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.dismissed {
            return div().into_any_element();
        }

        div()
            // Which showing is up is otherwise invisible to a test: the popup
            // is built by the shared overlay, not by the trigger. The closure
            // is never called outside a test build.
            .debug_selector(|| format!("{DEBUG_SELECTOR_PREFIX}{}", self.text))
            .child(self.text.clone())
            .into_any_element()
    }
}

/// What [`Tooltip`]'s debug selector is prefixed with; the showing's text
/// follows.
const DEBUG_SELECTOR_PREFIX: &str = "ravel-tooltip:";

// ---------------------------------------------------------------------------
// The trigger side
// ---------------------------------------------------------------------------

/// Give an element Ravel's tooltip.
///
/// Implemented for every stateful interactive element that can take a child,
/// which is every element GPUI's own `.tooltip()` accepts, so a call site swaps
/// one method for another and nothing about its layout moves: the child added
/// here is an absolutely positioned zero-weight `canvas`.
pub trait TooltipExt: StatefulInteractiveElement + ParentElement + Sized {
    /// Show `text` after [`SHOW_DELAY`] of hovering — or at once, when the
    /// window's tooltip is still inside its [`GRACE_PERIOD`].
    fn ravel_tooltip(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        // The overlay positions the popup against the trigger, so the trigger
        // has to tell it where it ended up. Written during prepaint and read
        // by the handlers registered in the same frame's paint.
        let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        let writer = trigger_bounds.clone();

        // `Stateful` carries GPUI's own `on_prepaint` too; name the one that
        // hands over the resolved bounds.
        ElementExt::on_prepaint(self, move |bounds, _window, _cx| writer.set(bounds))
            .on_hover(move |hovered, window, cx| {
                let Some(this) = window_tooltip(window, cx) else {
                    return;
                };
                let Some(overlay) = this.overlay.upgrade() else {
                    return;
                };
                let bounds = trigger_bounds.get();
                if !*hovered {
                    // Not ours any more: another trigger has taken the
                    // showing over, and hiding now would take *its* tooltip
                    // down. See `WindowTooltip::showing`.
                    if this.showing.get() != Some(bounds) {
                        return;
                    }
                    this.showing.set(None);
                    overlay.update(cx, |overlay, cx| overlay.request_hide(window, cx));
                    return;
                }
                let text = text.clone();
                this.showing.set(Some(bounds));
                overlay.update(cx, |overlay, cx| {
                    let request = TooltipRequest::new(bounds, move |window, cx| {
                        Tooltip::new(text.clone()).build(window, cx)
                    });
                    overlay.request_show(request, window, cx);
                });
            })
            // A tooltip explains a control the user has not used yet; once they
            // press it, it is in the way of the thing it was explaining.
            .on_mouse_down(MouseButton::Left, |_event, window, cx| {
                hide_tooltip(window, cx);
            })
    }
}

impl<E: StatefulInteractiveElement + ParentElement> TooltipExt for E {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};

    fn key(name: &str) -> Keystroke {
        Keystroke::parse(name).expect("the test keystroke parses")
    }

    #[test]
    fn escape_dismisses_the_showing_and_nothing_else_does() {
        let mut tooltip = Tooltip::new("Fit");
        assert!(!tooltip.is_dismissed());

        // Everything a user might type while a tooltip happens to be up must
        // leave it alone; only Escape is a dismissal.
        for other in ["enter", "space", "tab", "a", "cmd-s", "shift-escape"] {
            assert!(
                !tooltip.on_key(&key(other)),
                "{other} dismissed the tooltip"
            );
            assert!(!tooltip.is_dismissed(), "{other} dismissed the tooltip");
        }

        assert!(tooltip.on_key(&key("escape")));
        assert!(tooltip.is_dismissed());
    }

    /// The listener repaints only when the popup changed. Without this, every
    /// keystroke while a tooltip is up would notify the window.
    #[test]
    fn a_second_escape_changes_nothing() {
        let mut tooltip = Tooltip::new("Fit");
        assert!(tooltip.on_key(&key("escape")));
        assert!(!tooltip.on_key(&key("escape")));
        assert!(tooltip.is_dismissed());
    }

    #[test]
    fn the_recorded_numbers_are_the_ones_the_plan_states() {
        assert_eq!(SHOW_DELAY, Duration::from_millis(500));
        assert_eq!(GRACE_PERIOD, Duration::from_millis(300));
        assert_eq!(WINDOW_MARGIN, px(4.0));
    }

    /// Two triggers side by side, sharing the window's overlay — a toolbar in
    /// miniature, which is the case the shared overlay exists for.
    struct Toolbar {
        overlay: Option<Entity<TooltipOverlay>>,
        /// Declares the second trigger before the first.
        ///
        /// GPUI works out which elements changed hover state during paint and
        /// runs the callbacks afterwards, so tree order decides which of
        /// "entered B" and "left A" the overlay hears first. Both orders have
        /// to end with B's tooltip up, and only one of them exercises the
        /// harder path — hence two windows rather than one.
        reversed: bool,
    }

    /// Where the two triggers sit, in window coordinates. Well below the top
    /// so a popup placed above one has room.
    const ROW_TOP: f32 = 200.0;
    const TRIGGER_SIZE: f32 = 40.0;
    const FIRST_LEFT: f32 = 100.0;
    const SECOND_LEFT: f32 = 200.0;

    impl Toolbar {
        fn trigger(id: &'static str, left: f32, text: &'static str) -> impl IntoElement {
            div()
                .id(id)
                .absolute()
                .left(px(left))
                .top(px(ROW_TOP))
                .size(px(TRIGGER_SIZE))
                .ravel_tooltip(text)
        }
    }

    impl Render for Toolbar {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let first = Self::trigger("first", FIRST_LEFT, "First").into_any_element();
            let second = Self::trigger("second", SECOND_LEFT, "Second").into_any_element();
            let order = if self.reversed {
                [second, first]
            } else {
                [first, second]
            };
            div()
                .size_full()
                .children(order)
                .children(self.overlay.clone())
        }
    }

    /// The center of a trigger, for the mouse to be moved onto.
    fn center(left: f32) -> gpui::Point<Pixels> {
        point(
            px(left + TRIGGER_SIZE / 2.0),
            px(ROW_TOP + TRIGGER_SIZE / 2.0),
        )
    }

    /// A point on no trigger at all.
    fn nowhere() -> gpui::Point<Pixels> {
        point(px(600.0), px(600.0))
    }

    /// The debug selectors of the two showings, spelled out because
    /// `debug_bounds` takes a `&'static str`.
    const FIRST_POPUP: &str = "ravel-tooltip:First";
    const SECOND_POPUP: &str = "ravel-tooltip:Second";

    /// Keeps the spelled-out selectors above honest.
    #[test]
    fn the_debug_selector_is_the_prefix_and_the_text() {
        assert_eq!(FIRST_POPUP, format!("{DEBUG_SELECTOR_PREFIX}First"));
        assert_eq!(SECOND_POPUP, format!("{DEBUG_SELECTOR_PREFIX}Second"));
    }

    /// Whether the popup named by `selector` is in the frame the window last
    /// drew. The popup is built by the shared overlay rather than by the
    /// trigger, so the drawn frame is the only place a test can see which
    /// showing is up.
    fn showing(selector: &'static str, cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds(selector).is_some()
    }

    /// A window holding the two triggers and, unless `with_overlay` is false,
    /// the overlay they share.
    fn toolbar(with_overlay: bool, cx: &mut TestAppContext) -> &mut VisualTestContext {
        toolbar_ordered(with_overlay, false, cx)
    }

    /// The same window, with the triggers declared in either tree order.
    fn toolbar_ordered(
        with_overlay: bool,
        reversed: bool,
        cx: &mut TestAppContext,
    ) -> &mut VisualTestContext {
        let (_view, cx) = cx.add_window_view(|window, cx| Toolbar {
            overlay: with_overlay.then(|| install_tooltip_overlay(window, cx)),
            reversed,
        });
        cx
    }

    fn hover(position: gpui::Point<Pixels>, cx: &mut VisualTestContext) {
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.run_until_parked();
    }

    /// Let `duration` pass and draw whatever it changed.
    fn wait(duration: Duration, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    /// The 500ms boundary, from both sides. A tooltip that appeared at 499ms
    /// would mean the delay is no longer the number the plan states; one that
    /// had not appeared at 500ms would mean the overlay never got the request.
    #[gpui::test]
    fn the_delay_is_the_one_the_shared_overlay_pays(cx: &mut TestAppContext) {
        let cx = toolbar(true, cx);
        hover(center(FIRST_LEFT), cx);
        assert!(
            !showing(FIRST_POPUP, cx),
            "the tooltip showed with no delay"
        );

        wait(SHOW_DELAY - Duration::from_millis(1), cx);
        assert!(!showing(FIRST_POPUP, cx), "the tooltip showed 1ms early");

        wait(Duration::from_millis(1), cx);
        assert!(showing(FIRST_POPUP, cx), "the tooltip never showed");
    }

    /// **The regression this unit closes.** Moving along a toolbar inside the
    /// grace period must not pay the delay again: this fails if the triggers
    /// go back to owning a timer each.
    #[gpui::test]
    fn moving_to_the_next_trigger_within_the_grace_period_shows_at_once(cx: &mut TestAppContext) {
        let cx = toolbar(true, cx);
        hover(center(FIRST_LEFT), cx);
        wait(SHOW_DELAY, cx);
        assert!(showing(FIRST_POPUP, cx));

        // Off the first trigger, then most of the way through the grace
        // period: the showing is on its way out but the window is still warm.
        hover(nowhere(), cx);
        wait(GRACE_PERIOD - Duration::from_millis(100), cx);

        // No clock advance at all between the hover and the assertion.
        hover(center(SECOND_LEFT), cx);
        assert!(
            showing(SECOND_POPUP, cx),
            "the second trigger paid the delay again"
        );
    }

    /// Moving **straight** from one trigger to the next — no gap in between,
    /// which is what a toolbar actually feels like. The new showing has to
    /// survive its own grace period: the trigger the pointer left asks the
    /// overlay to hide in the same frame, and that request must not be
    /// allowed to fold the tooltip that is now the next trigger's.
    #[gpui::test]
    fn moving_straight_across_leaves_the_new_showing_alone(cx: &mut TestAppContext) {
        for reversed in [false, true] {
            let cx = toolbar_ordered(true, reversed, cx);
            hover(center(FIRST_LEFT), cx);
            wait(SHOW_DELAY, cx);
            assert!(showing(FIRST_POPUP, cx), "reversed={reversed}");

            hover(center(SECOND_LEFT), cx);
            assert!(
                showing(SECOND_POPUP, cx),
                "the second trigger paid the delay again (reversed={reversed})"
            );
            assert!(!showing(FIRST_POPUP, cx), "reversed={reversed}");

            // Long enough that a grace timer armed by leaving the first
            // trigger would have fired.
            wait(GRACE_PERIOD + Duration::from_millis(100), cx);
            assert!(
                showing(SECOND_POPUP, cx),
                "leaving the first trigger took the second one's tooltip down \
                 (reversed={reversed})"
            );
        }
    }

    /// The other side of the grace period: a real pause is a fresh start, so
    /// the delay is paid again. This fails if the overlay stayed warm forever.
    #[gpui::test]
    fn moving_after_the_grace_period_pays_the_delay_again(cx: &mut TestAppContext) {
        let cx = toolbar(true, cx);
        hover(center(FIRST_LEFT), cx);
        wait(SHOW_DELAY, cx);
        assert!(showing(FIRST_POPUP, cx));

        hover(nowhere(), cx);
        wait(GRACE_PERIOD + Duration::from_millis(100), cx);
        assert!(
            !showing(FIRST_POPUP, cx),
            "the showing outlived the grace period"
        );

        hover(center(SECOND_LEFT), cx);
        assert!(
            !showing(SECOND_POPUP, cx),
            "the delay was skipped after a pause"
        );
        wait(SHOW_DELAY, cx);
        assert!(showing(SECOND_POPUP, cx));
    }

    /// Pressing the control puts the tooltip out of the way at once — and
    /// without leaving the window warm, so the next hover explains itself
    /// again only after a deliberate pause.
    #[gpui::test]
    fn pressing_the_trigger_hides_the_tooltip(cx: &mut TestAppContext) {
        let cx = toolbar(true, cx);
        hover(center(FIRST_LEFT), cx);
        wait(SHOW_DELAY, cx);
        assert!(showing(FIRST_POPUP, cx));

        cx.simulate_mouse_down(center(FIRST_LEFT), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(!showing(FIRST_POPUP, cx), "the press left the tooltip up");
    }

    /// Escape takes down the showing rather than only the content view: with
    /// the popup living in the shared overlay, hiding the content alone would
    /// leave the overlay holding an empty popup.
    #[gpui::test]
    fn escape_takes_down_the_showing(cx: &mut TestAppContext) {
        let cx = toolbar(true, cx);
        hover(center(FIRST_LEFT), cx);
        wait(SHOW_DELAY, cx);
        assert!(showing(FIRST_POPUP, cx));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!showing(FIRST_POPUP, cx), "Escape left the tooltip up");
    }

    /// A window nobody installed an overlay in shows no tooltips, and that is
    /// all it does: a `render` that panicked would take the window with it.
    #[gpui::test]
    fn a_window_without_an_overlay_shows_nothing_and_survives(cx: &mut TestAppContext) {
        let cx = toolbar(false, cx);
        cx.update(|window, cx| assert!(tooltip_overlay(window, cx).is_none()));

        hover(center(FIRST_LEFT), cx);
        wait(SHOW_DELAY + GRACE_PERIOD, cx);
        assert!(!showing(FIRST_POPUP, cx));

        cx.simulate_mouse_down(center(FIRST_LEFT), MouseButton::Left, Modifiers::none());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }

    /// Each window gets its own overlay, so a detached window's tooltips are
    /// not the main window's.
    #[gpui::test]
    fn every_window_gets_its_own_overlay(cx: &mut TestAppContext) {
        let first = {
            let cx = toolbar(true, cx);
            cx.update(|window, cx| tooltip_overlay(window, cx))
        };
        let second = {
            let cx = toolbar(true, cx);
            cx.update(|window, cx| tooltip_overlay(window, cx))
        };
        let (first, second) = (
            first.expect("the first window has an overlay"),
            second.expect("the second window has an overlay"),
        );
        assert_ne!(first.entity_id(), second.entity_id());
    }
}

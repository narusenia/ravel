// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's tooltip.
//!
//! Two halves, and the split is deliberate:
//!
//! - [`Tooltip`] is the **popup itself** — `gpui_base::Tooltip`, which owns the
//!   accessible tooltip role, dressed in Ravel's tokens: the raised surface, a
//!   1px border, no shadow;
//! - [`TooltipExt::ravel_tooltip`] is the **trigger side**, and it rides GPUI's
//!   own tooltip machinery (`gpui/src/elements/div.rs`), which already owns the
//!   show delay, the placement, the window clamping, and dismissal on
//!   mouse-down and on scroll. Ravel supplies the one number GPUI takes as a
//!   parameter, [`SHOW_DELAY`].
//!
//! # Escape
//!
//! GPUI's machinery has no Escape path and `clear_active_tooltip` is
//! `pub(crate)`, so the popup dismisses *itself*: [`Tooltip::on_key`] is the
//! state machine, and a zero-size `canvas` inside the popup registers the key
//! listener during paint (the only phase `Window::on_key_event` accepts). A
//! dismissed popup renders nothing; the next hover builds a fresh view, so
//! Escape hides this showing rather than the tooltip forever.

use std::time::Duration;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, KeyDownEvent, Keystroke,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement, Styled as _,
    Window, canvas, div, px,
};
use gpui_base::Tooltip as BaseTooltip;

use crate::theme::ActiveTokens as _;

/// How long the pointer must rest on a trigger before its tooltip appears.
///
/// The same 500ms `gpui_base::tooltip` and GPUI's own default use; naming it
/// here is what makes it Ravel's number rather than a framework default that
/// changes under us.
pub const SHOW_DELAY: Duration = Duration::from_millis(500);

/// The gap the popup keeps from the trigger and from the window edge.
pub const WINDOW_MARGIN: Pixels = px(4.0);

/// Vertical padding inside the popup.
const POPUP_PADDING_Y: Pixels = px(2.0);

/// Ravel's tooltip popup.
///
/// Built through [`Tooltip::build`], which is what GPUI's `.tooltip()` callback
/// has to hand back.
pub struct Tooltip {
    text: SharedString,
    dismissed: bool,
}

impl Tooltip {
    /// A tooltip showing `text`.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            dismissed: false,
        }
    }

    /// Turn this into the view GPUI's tooltip callback returns.
    pub fn build(self, _window: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| self).into()
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.dismissed {
            return div().into_any_element();
        }

        let theme = cx.tokens().clone();
        let colors = theme.colors;
        let radius = theme.radius.radius;
        let font_size = theme.text.font_size;

        div()
            .child(escape_listener(cx.entity()))
            .child(
                BaseTooltip::new("ravel-tooltip")
                    // The margin is the gap from the trigger: GPUI places the
                    // popup flush against it otherwise.
                    .m(WINDOW_MARGIN)
                    .px(theme.spacing.xs)
                    .py(POPUP_PADDING_Y)
                    .rounded(radius)
                    // A floating surface needs an edge to read against a panel
                    // of a similar tone. It gets a border rather than a
                    // shadow: a shadow under a 20px popup in a dense tool
                    // reads as blur, not as height.
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised_surface())
                    .text_size(font_size)
                    .text_color(colors.foreground)
                    .child(self.text.clone()),
            )
            .into_any_element()
    }
}

/// A zero-size child whose only job is to watch for Escape.
///
/// `Window::on_key_event` asserts it is called during paint, which no `render`
/// body is, so the registration happens in a `canvas`'s paint callback. The
/// element is absolutely positioned and zero-sized so it cannot move the
/// popup's own layout.
fn escape_listener(tooltip: Entity<Tooltip>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _cx| {
            let tooltip = tooltip.clone();
            window.on_key_event::<KeyDownEvent>(move |event, phase, _window, cx| {
                if !phase.bubble() {
                    return;
                }
                tooltip.update(cx, |tooltip, cx| {
                    if tooltip.on_key(&event.keystroke) {
                        cx.notify();
                    }
                });
            });
        },
    )
    .absolute()
    .size_0()
}

/// Give an element Ravel's tooltip.
///
/// Implemented for every stateful interactive element, which is every element
/// GPUI's own `.tooltip()` accepts, so a call site swaps one method for
/// another and nothing about its layout moves.
pub trait TooltipExt: StatefulInteractiveElement + Sized {
    /// Show `text` after [`SHOW_DELAY`] of hovering.
    fn ravel_tooltip(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        self.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            .tooltip_show_delay(SHOW_DELAY)
    }
}

impl<E: StatefulInteractiveElement> TooltipExt for E {}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_show_delay_is_ravels_own_number() {
        assert_eq!(SHOW_DELAY, Duration::from_millis(500));
        assert_eq!(WINDOW_MARGIN, px(4.0));
    }
}

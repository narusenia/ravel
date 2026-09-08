// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Giving a Ravel button a borrowed dropdown menu.
//!
//! Nine buttons in the application open a `gpui_component::PopupMenu`. The
//! menu itself is still borrowed — re-hosting it is a later unit — but its
//! *trigger* is now [`ravel_widgets::Button`], and gpui-component's
//! `DropdownMenu` is implemented for its own `Button` alone. A foreign trait
//! cannot be implemented for a foreign type, so the trigger needs a local one:
//! [`MenuTrigger`].
//!
//! **Why here.** `ravel-dock` is the lowest crate in the graph that may name
//! both `ravel_widgets::Button` and `gpui_component::PopupMenu`; `ravel-app`
//! already depends on it, and `ravel-widgets` must never depend on
//! gpui-component. This module disappears when the menu does.

use gpui::{
    App, Div, InteractiveElement, Interactivity, IntoElement, RenderOnce, Stateful,
    StyleRefinement, Styled, Window, div,
};
use gpui_component::Selectable;
use gpui_component::menu::DropdownMenu;
use ravel_widgets::Button;

/// A Ravel button that can carry a borrowed dropdown menu.
///
/// Everything is delegated: the style refinement and the selected flag are the
/// button's own, and rendering *is* the button. The one thing the wrapper adds
/// is an [`Interactivity`], because `DropdownMenu` reads the trigger's element
/// id out of it to key the popover — the button keeps its own id, and the
/// carrier repeats it so the two agree.
#[derive(IntoElement)]
pub struct MenuTrigger {
    button: Button,
    id_carrier: Stateful<Div>,
}

impl MenuTrigger {
    /// Wrap `button` so it can open a menu.
    pub fn new(button: Button) -> Self {
        let id_carrier = div().id(button.id().clone());
        Self { button, id_carrier }
    }
}

/// Wrap a Ravel button for a dropdown menu.
pub trait MenuButton: Sized {
    /// Give this button a menu. Follow it with `dropdown_menu(..)`.
    fn with_menu(self) -> MenuTrigger;
}

impl MenuButton for Button {
    fn with_menu(self) -> MenuTrigger {
        MenuTrigger::new(self)
    }
}

impl Styled for MenuTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.button.style()
    }
}

impl InteractiveElement for MenuTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.id_carrier.interactivity()
    }
}

impl Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.button = Selectable::selected(self.button, selected);
        self
    }

    fn is_selected(&self) -> bool {
        self.button.is_selected()
    }
}

impl RenderOnce for MenuTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.button
    }
}

impl DropdownMenu for MenuTrigger {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two things `DropdownMenu` reads off a trigger: its element id, so
    /// the popover is keyed to it, and its selected flag, so an open menu
    /// keeps the trigger lit. A carrier with an id of its own would open the
    /// wrong popover on every button.
    #[test]
    fn the_wrapper_carries_the_buttons_id_and_selection() {
        let mut trigger = MenuTrigger::new(Button::new("viewer-grid").selected(true));

        assert_eq!(
            trigger.interactivity().element_id,
            Some("viewer-grid".into()),
            "the carrier does not name the same element as the button"
        );
        assert!(trigger.is_selected());
        assert!(!Selectable::selected(trigger, false).is_selected());
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's button.
//!
//! Behaviour is borrowed and appearance is Ravel's. [`gpui_base::Button`] owns
//! focus, the tab stop, the accessibility role, and — the part that matters
//! most — **one activation handler for pointer, Enter and Space**. That is UX
//! invariant 10 held up by construction rather than by discipline: there is no
//! second key path to rot, and this module adds none. It must never grow an
//! `on_key_down`.
//!
//! What this module decides is the appearance of the four states, and it
//! decides it in one pure function, [`button_layers`], so the rules are
//! readable and testable without a window. `render` only applies what that
//! function returned:
//!
//! - the resting layer is the instance style;
//! - `hover`, `press` and the focus ring are GPUI's own `hover` / `active` /
//!   `focus_visible`, installed **only while the button is enabled**;
//! - `selected` and `disabled` go through `gpui_base`'s `styles()`, whose
//!   `state_style` contract layers them over the instance style in that order,
//!   disabled last.
//!
//! # The focus ring is inside the button
//!
//! A 1px border that is transparent at rest and `primary` under
//! `focus_visible`. Inside, because `ravel-app` and `ravel-dock` carry two
//! dozen `overflow_hidden` ancestors that would clip an outset ring; and as a
//! border that is always present, because a border that appears on focus would
//! change the button's width. `focus_visible` — not `focus` — is what makes the
//! ring keyboard-only: GPUI ANDs it with `window.last_input_was_keyboard()`, so
//! no input-source tracking of our own is needed.
//!
//! # No borders
//!
//! Eight 20px buttons in a toolbar with borders are nine vertical lines. With
//! surfaces alone, only the hovered one is visible.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _, transparent_black,
};
use gpui_base::{Button as BaseButton, StyledExt as _};

use crate::icon::Icon;
use crate::theme::ActiveTokens as _;
use crate::tokens::{self, Colors, Density, PRESS_MIX};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Which surface a button carries at rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ButtonVariant {
    /// No surface until it is hovered. The toolbar default, and by far the
    /// most common: a row of ghost buttons shows one surface at a time.
    #[default]
    Ghost,
    /// A `secondary` surface — a button that must be findable without being
    /// hovered first.
    Solid,
    /// A `primary` surface — the one action a dialog is asking for.
    Primary,
}

/// One layer of a button's appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonFace {
    /// The surface. `transparent_black()` means "no surface", which is a real
    /// value rather than `None` because a style *refinement* can only
    /// overwrite a field, never unset one.
    pub surface: Hsla,
    /// Text and icon colour.
    pub foreground: Hsla,
}

/// Every layer a button installs, in the order they resolve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonLayers {
    /// The instance style.
    pub rest: ButtonFace,
    /// `hover`. `None` while disabled.
    pub hover: Option<ButtonFace>,
    /// `active`. `None` while disabled.
    pub pressed: Option<ButtonFace>,
    /// The keyboard focus ring. `None` while disabled.
    pub focus_ring: Option<Hsla>,
    /// The `selected` semantic layer. `None` unless selected.
    pub selected: Option<ButtonFace>,
    /// The `disabled` semantic layer, resolved last. `None` unless disabled.
    pub disabled: Option<ButtonFace>,
}

/// The appearance of a button in every state it can reach.
///
/// # How the states are derived
///
/// One rule: **hover moves the resting surface one press step toward
/// `foreground`, and press moves one step further.** For a Ghost button, whose
/// resting surface is the window's own background, that first step is the
/// `accent` token rather than a computed mix — that is what `accent` is for,
/// and it is `Colors::hover_surface`. Everything else follows from
/// [`Colors::toward_foreground`], which is why none of it asks which palette
/// is in force.
///
/// Disabled removes the surface and fades the foreground; it never dims the
/// whole element, because a translucent button over the Properties panel's
/// striped rows is harder to read than a faint one.
pub fn button_layers(
    variant: ButtonVariant,
    selected: bool,
    disabled: bool,
    colors: &Colors,
) -> ButtonLayers {
    let (resting_surface, foreground) = match variant {
        ButtonVariant::Ghost => (transparent_black(), colors.foreground),
        ButtonVariant::Solid => (colors.secondary, colors.foreground),
        ButtonVariant::Primary => (colors.primary, colors.readable_on(colors.primary)),
    };
    // A Ghost button has no surface of its own, so its first step is the token
    // that names "one step off the background".
    let hovered_surface = match variant {
        ButtonVariant::Ghost => colors.hover_surface(),
        _ => colors.toward_foreground(resting_surface, PRESS_MIX),
    };
    let pressed_surface = colors.toward_foreground(hovered_surface, PRESS_MIX);

    let face = |surface| ButtonFace {
        surface,
        foreground,
    };

    ButtonLayers {
        rest: face(resting_surface),
        // Neither hover nor press is installed while the button is disabled.
        // Leaving them installed and relying on the `disabled` layer would not
        // work: GPUI resolves `hover` *after* the element style, so a disabled
        // button would still light up under the pointer.
        hover: (!disabled).then(|| face(hovered_surface)),
        pressed: (!disabled).then(|| face(pressed_surface)),
        // Independent of hover by construction: a hovered button and a
        // keyboard-focused button are two different layers, and both can be on.
        focus_ring: (!disabled).then(|| colors.focus_ring()),
        selected: selected.then(|| face(hovered_surface)),
        disabled: disabled.then(|| ButtonFace {
            surface: transparent_black(),
            foreground: colors.disabled_foreground(),
        }),
    }
}

/// The motions a button's state feedback runs on.
///
/// Both tokens are returned, but only the first is wired: GPUI's
/// `StyleTransitions` takes one motion per property and applies it in both
/// directions, so an asymmetric enter/leave is not expressible there.
pub fn feedback_motion(motion: &tokens::Motion) -> (gpui::Motion, gpui::Motion) {
    (
        gpui::Motion::new(motion.feedback_in),
        gpui::Motion::new(motion.feedback_out),
    )
}

/// A Ravel button.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    density: Density,
    variant: ButtonVariant,
    label: Option<SharedString>,
    icon: Option<Icon>,
    children: Vec<AnyElement>,
    selected: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    tab_index: isize,
    tab_stop: bool,
    on_click: Option<ClickHandler>,
    debug_selector: Option<String>,
    style: StyleRefinement,
}

impl Button {
    /// A ghost button on the default density step.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            density: Density::default(),
            variant: ButtonVariant::default(),
            label: None,
            icon: None,
            children: Vec::new(),
            selected: false,
            disabled: false,
            tooltip: None,
            accessibility_label: None,
            focus_handle: None,
            tab_index: 0,
            tab_stop: true,
            on_click: None,
            debug_selector: None,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the button on the compact step (`row.compact` tall).
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Draw the button on `density`.
    pub fn density(mut self, density: Density) -> Self {
        self.density = density;
        self
    }

    /// No surface until hovered. The default.
    pub fn ghost(mut self) -> Self {
        self.variant = ButtonVariant::Ghost;
        self
    }

    /// A `secondary` surface at rest.
    pub fn solid(mut self) -> Self {
        self.variant = ButtonVariant::Solid;
        self
    }

    /// A `primary` surface at rest.
    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    /// The button's text.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The button's icon.
    ///
    /// **The button decides the size**, from its density step — a call site
    /// does not, and any size it set is replaced. That is the whole reason
    /// density exists: 48 of the 51 call sites this replaced spelled their
    /// icon size out by hand, and three of them disagreed.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Show `text` on hover.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// The persistent "this is the active choice" presentation.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Whether the button refuses pointer and keyboard activation.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The label exposed to accessibility clients. Defaults to [`Button::label`].
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Use a caller-owned focus handle.
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }

    /// Where the button sits in the Tab order.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }

    /// Whether Tab reaches the button at all.
    ///
    /// A button that is not a tab stop also gives up Enter and Space, which is
    /// `gpui_base::Button`'s rule and the reason there is one flag rather than
    /// two.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    /// The activation handler, for the pointer, Enter and Space alike.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// A key an integration test can look this button's bounds up under.
    ///
    /// Forwards to GPUI's `InteractiveElement::debug_selector`, which is a
    /// no-op in a release build. It is here rather than left to the call site
    /// because the alternative is wrapping the button in a carrier `div` to
    /// hold the selector, and a wrapper in a dialog footer is a layout change
    /// made for a test's benefit — the thing UX invariant 11 exists to stop.
    /// The closure is called eagerly, exactly as GPUI's own does under
    /// `test-support`; `cfg` cannot help here, because `test-support` is
    /// GPUI's feature and this crate is not the one compiling the test.
    pub fn debug_selector(mut self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    /// The button's element id.
    ///
    /// Exposed for a wrapper that has to name the same element — the menu
    /// trigger in `ravel-dock` is the one caller.
    pub fn id(&self) -> &ElementId {
        &self.id
    }

    /// Whether the button shows nothing but an icon, and is therefore square.
    fn is_icon_only(&self) -> bool {
        self.label.is_none() && self.children.is_empty()
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl gpui_base::Selectable for Button {
    fn selected(self, selected: bool) -> Self {
        Button::selected(self, selected)
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let metrics = self.density.metrics(&theme);
        let layers = button_layers(self.variant, self.selected, self.disabled, &theme.colors);
        let icon_only = self.is_icon_only();
        let (enter, _leave) = feedback_motion(&theme.motion);

        let accessibility_label = self
            .accessibility_label
            .clone()
            .or_else(|| self.label.clone());
        let content = self
            .icon
            .map(|icon| icon.size(metrics.icon).into_any_element());

        BaseButton::new(self.id)
            .h(metrics.height)
            .map(|button| {
                if icon_only {
                    button.w(metrics.height)
                } else {
                    button.px(metrics.padding_x)
                }
            })
            .gap(metrics.gap)
            .rounded(theme.radius.radius)
            // See the module docs: a permanent 1px border that only shows a
            // colour under `focus_visible`, so the ring is inside the bounds
            // and the button never changes size.
            .border_1()
            .border_color(transparent_black())
            .text_size(theme.text.font_size)
            .bg(layers.rest.surface)
            .text_color(layers.rest.foreground)
            // Only the surface and the text animate, and only over the
            // feedback token: UX invariant 11 allows state feedback and
            // nothing else, so no size, position or radius is listed here.
            .transitions(|transitions| {
                transitions
                    .bg(enter.clone())
                    .text_color(enter.clone())
                    .border_color(enter)
            })
            .when_some(layers.hover, |button, face| {
                button.hover(move |style| style.bg(face.surface).text_color(face.foreground))
            })
            .when_some(layers.pressed, |button, face| {
                button.active(move |style| style.bg(face.surface).text_color(face.foreground))
            })
            .when_some(layers.focus_ring, |button, ring| {
                button.focus_visible(move |style| style.border_color(ring))
            })
            .selected(self.selected)
            .disabled(self.disabled)
            .styles(|styles| {
                styles
                    .selected(|style| match layers.selected {
                        Some(face) => style.bg(face.surface).text_color(face.foreground),
                        None => style,
                    })
                    .disabled(|style| match layers.disabled {
                        Some(face) => style.bg(face.surface).text_color(face.foreground),
                        None => style,
                    })
            })
            .tab_index(self.tab_index)
            .tab_stop(self.tab_stop)
            .when_some(accessibility_label, |button, label| {
                button.accessibility_label(label)
            })
            .when_some(self.focus_handle.as_ref(), |button, handle| {
                button.track_focus(handle)
            })
            .when_some(self.on_click, |button, on_click| {
                button.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .when_some(self.debug_selector, |button, selector| {
                button.debug_selector(|| selector)
            })
            .when_some(self.tooltip, |button, text| {
                crate::tooltip::TooltipExt::ravel_tooltip(button, text)
            })
            .when_some(content, |button, icon| button.child(icon))
            .when_some(self.label, |button, label| {
                button.child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
            })
            .children(self.children)
            .refine_style(&self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{Colors, ThemeSpec};
    use std::time::Duration;

    fn layers(variant: ButtonVariant, selected: bool, disabled: bool) -> ButtonLayers {
        button_layers(variant, selected, disabled, &Colors::light())
    }

    /// The invariant the state machine exists for: a control that cannot act
    /// must not react. GPUI resolves `hover` after the element style, so the
    /// only way to hold this is to not install the layer at all.
    #[test]
    fn a_disabled_button_reaches_no_hover_press_or_ring() {
        for variant in [
            ButtonVariant::Ghost,
            ButtonVariant::Solid,
            ButtonVariant::Primary,
        ] {
            for selected in [false, true] {
                let disabled = layers(variant, selected, true);
                assert_eq!(disabled.hover, None, "{variant:?} hover while disabled");
                assert_eq!(disabled.pressed, None, "{variant:?} press while disabled");
                assert_eq!(disabled.focus_ring, None, "{variant:?} ring while disabled");
                assert!(disabled.disabled.is_some(), "{variant:?} lost its layer");
            }
        }
    }

    #[test]
    fn a_disabled_button_loses_its_surface_and_fades_its_text() {
        let colors = Colors::light();
        let face = layers(ButtonVariant::Primary, false, true)
            .disabled
            .expect("the disabled layer");

        // Not an element-wide `opacity()`: the Properties panel's striped rows
        // would show through a translucent button.
        assert_eq!(face.surface, transparent_black());
        assert_eq!(face.foreground, colors.disabled_foreground());
    }

    /// Focus and hover are separate layers, so a button can be both at once
    /// and the ring is legible either way.
    #[test]
    fn the_ring_is_independent_of_hover() {
        let enabled = layers(ButtonVariant::Ghost, false, false);
        assert!(enabled.hover.is_some());
        assert_eq!(enabled.focus_ring, Some(Colors::light().focus_ring()));
        assert_ne!(
            Some(enabled.hover.unwrap().surface),
            enabled.focus_ring,
            "a ring the colour of the hover surface would be invisible on hover"
        );
    }

    #[test]
    fn a_ghost_button_is_exactly_the_two_derived_surfaces() {
        let colors = Colors::light();
        let ghost = layers(ButtonVariant::Ghost, false, false);

        assert_eq!(ghost.rest.surface, transparent_black());
        assert_eq!(ghost.hover.unwrap().surface, colors.hover_surface());
        assert_eq!(ghost.pressed.unwrap().surface, colors.pressed_surface());
    }

    /// The generalisation to a variant that already carries a surface: one
    /// press step for hover, a second for press. A rule that reused the Ghost
    /// surfaces here would turn a primary button grey under the pointer.
    #[test]
    fn a_filled_button_steps_off_its_own_surface() {
        let colors = Colors::light();
        for (variant, resting) in [
            (ButtonVariant::Solid, colors.secondary),
            (ButtonVariant::Primary, colors.primary),
        ] {
            let filled = layers(variant, false, false);
            let hover = filled.hover.unwrap().surface;
            let pressed = filled.pressed.unwrap().surface;

            assert_eq!(filled.rest.surface, resting);
            assert_eq!(hover, colors.toward_foreground(resting, PRESS_MIX));
            assert_eq!(pressed, colors.toward_foreground(hover, PRESS_MIX));
            assert_ne!(hover, resting, "{variant:?} has no visible hover");
            assert_ne!(pressed, hover, "{variant:?} cannot be told apart pressed");
        }
    }

    #[test]
    fn a_primary_buttons_label_is_the_readable_end_of_the_ramp() {
        for colors in [Colors::light(), Colors::dark()] {
            let primary = button_layers(ButtonVariant::Primary, false, false, &colors);
            assert_eq!(primary.rest.foreground, colors.readable_on(colors.primary));
        }
    }

    #[test]
    fn selected_is_a_layer_only_while_selected() {
        assert_eq!(layers(ButtonVariant::Ghost, false, false).selected, None);
        let selected = layers(ButtonVariant::Ghost, true, false)
            .selected
            .expect("the selected layer");
        // A selected ghost button reads like a hovered one, which is what a
        // toggled toolbar button has always looked like here.
        assert_eq!(selected.surface, Colors::light().hover_surface());
    }

    /// Both palettes, one rule: the derivation never asks which mode is on.
    #[test]
    fn the_same_rule_darkens_light_and_lightens_dark() {
        let light = button_layers(ButtonVariant::Ghost, false, false, &Colors::light());
        let dark = button_layers(ButtonVariant::Ghost, false, false, &Colors::dark());

        let lighter = |a: Hsla, b: Hsla| a.color.lightness > b.color.lightness;
        assert!(lighter(
            light.hover.unwrap().surface,
            light.pressed.unwrap().surface
        ));
        assert!(lighter(
            dark.pressed.unwrap().surface,
            dark.hover.unwrap().surface
        ));
    }

    /// The fake clock the completion criterion asks for: `gpui::Motion` is the
    /// same arithmetic `StyleTransitions` runs on, so sampling it is sampling
    /// the button's timeline.
    #[test]
    fn the_feedback_motions_finish_at_the_token_durations() {
        let theme = ThemeSpec::default().resolve();
        let (enter, leave) = feedback_motion(&theme.motion);

        assert_eq!(enter.duration, Duration::from_millis(120));
        assert_eq!(leave.duration, Duration::from_millis(180));

        assert!(enter.sample(Duration::from_millis(119)).is_active);
        assert!(!enter.sample(Duration::from_millis(120)).is_active);
        assert!(leave.sample(Duration::from_millis(179)).is_active);
        assert!(!leave.sample(Duration::from_millis(180)).is_active);

        // And the durations are the theme's, not this module's: a theme that
        // moves them moves the button with it.
        let slow = crate::tokens::ThemeSpec {
            motion: crate::tokens::MotionSpec {
                feedback_in: Some(300),
                feedback_out: Some(400),
            },
            ..crate::tokens::ThemeSpec::default()
        }
        .resolve();
        let (enter, leave) = feedback_motion(&slow.motion);
        assert_eq!(enter.duration, Duration::from_millis(300));
        assert_eq!(leave.duration, Duration::from_millis(400));
    }

    // -----------------------------------------------------------------------
    // What only a window can answer: focus, the Tab order, and which input
    // source the ring follows.
    // -----------------------------------------------------------------------

    use gpui::{
        Context, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
        TestAppContext, VisualTestContext, point, px,
    };
    use std::cell::Cell;

    struct ActivationHarness {
        disabled: bool,
        clicks: Rc<Cell<usize>>,
        keyboard_clicks: Rc<Cell<usize>>,
    }

    impl Render for ActivationHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            let keyboard_clicks = self.keyboard_clicks.clone();
            div().id("harness").tab_group().size(px(100.0)).child(
                Button::new("button")
                    .disabled(self.disabled)
                    .size_full()
                    .on_click(move |event, _, _| {
                        clicks.set(clicks.get() + 1);
                        if matches!(event, ClickEvent::Keyboard(_)) {
                            keyboard_clicks.set(keyboard_clicks.get() + 1);
                        }
                    }),
            )
        }
    }

    fn activation(
        cx: &mut TestAppContext,
        disabled: bool,
    ) -> (&mut VisualTestContext, Rc<Cell<usize>>, Rc<Cell<usize>>) {
        let clicks = Rc::new(Cell::new(0));
        let keyboard_clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            let keyboard_clicks = keyboard_clicks.clone();
            move |_, _| ActivationHarness {
                disabled,
                clicks,
                keyboard_clicks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, clicks, keyboard_clicks)
    }

    fn press(cx: &mut VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).expect("the test keystroke parses");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    /// UX invariant 10, the half that rots silently: Enter and Space must
    /// arrive through **the same handler** a click does. Adding an
    /// `on_key_down` beside `on_click` would make this count 4 instead of 2,
    /// or keep it at 2 while the click path stopped being the one that fired.
    #[gpui::test]
    fn enter_and_space_activate_through_the_click_handler(cx: &mut TestAppContext) {
        let (cx, clicks, keyboard_clicks) = activation(cx, false);

        // Focus it the way a user would, then stop counting that click.
        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        assert_eq!(clicks.get(), 1, "the pointer did not reach the button");
        assert_eq!(keyboard_clicks.get(), 0, "a click is not a keyboard event");
        clicks.set(0);
        cx.update(|window, cx| {
            assert!(
                window.focused(cx).is_some(),
                "the button did not take focus"
            );
            window.draw(cx).clear(cx);
        });

        press(cx, "enter");
        press(cx, "space");

        assert_eq!(clicks.get(), 2, "Enter and Space did not reach on_click");
        assert_eq!(
            keyboard_clicks.get(),
            2,
            "both arrived, but not as keyboard activations"
        );
    }

    /// The rest of invariant 6 and the disabled half of the state machine: a
    /// disabled button is inert to the pointer *and* to the keyboard.
    #[gpui::test]
    fn a_disabled_button_activates_from_neither_input(cx: &mut TestAppContext) {
        let (cx, clicks, _) = activation(cx, true);

        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        cx.update(|window, cx| window.focus_next(cx));
        press(cx, "enter");
        press(cx, "space");

        assert_eq!(clicks.get(), 0);
    }

    /// The ring is `focus_visible`, and GPUI gates that on
    /// `last_input_was_keyboard`. This is the flag the ring hangs off: if it
    /// were `focus` instead, a mouse click would paint a ring.
    #[gpui::test]
    fn the_ring_follows_the_keyboard_and_not_the_pointer(cx: &mut TestAppContext) {
        let (cx, _, _) = activation(cx, false);

        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        cx.update(|window, cx| {
            assert!(window.focused(cx).is_some(), "the click did not focus");
            assert!(
                !window.last_input_was_keyboard(),
                "a click must not arm the focus ring"
            );
        });

        press(cx, "tab");
        cx.update(|window, _| {
            assert!(
                window.last_input_was_keyboard(),
                "Tab must arm the focus ring"
            );
        });
    }

    struct TabHarness {
        handles: Vec<FocusHandle>,
    }

    /// A selected button is still a tab stop, and a disabled one is not.
    /// Getting the first wrong would make every toggled toolbar button
    /// unreachable from the keyboard — UX invariant 10 — and nothing else in
    /// the suite would notice.
    struct StateTabHarness {
        handles: Vec<FocusHandle>,
    }

    impl Render for StateTabHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("state-tabs")
                .tab_group()
                .child(
                    Button::new("plain")
                        .track_focus(&self.handles[0])
                        .size(px(20.0)),
                )
                .child(
                    Button::new("selected")
                        .track_focus(&self.handles[1])
                        .selected(true)
                        .size(px(20.0)),
                )
                .child(
                    Button::new("disabled")
                        .track_focus(&self.handles[2])
                        .disabled(true)
                        .size(px(20.0)),
                )
                .child(
                    Button::new("after")
                        .track_focus(&self.handles[3])
                        .size(px(20.0)),
                )
        }
    }

    #[gpui::test]
    fn selection_keeps_a_tab_stop_and_disabling_removes_one(cx: &mut TestAppContext) {
        let handles = cx.update(|cx| (0..4).map(|_| cx.focus_handle()).collect::<Vec<_>>());
        let (_, cx) = cx.add_window_view({
            let handles = handles.clone();
            move |_, _| StateTabHarness { handles }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let focused = |cx: &mut VisualTestContext| {
            cx.update(|window, _| {
                (0..4)
                    .find(|i| handles[*i].is_focused(window))
                    .expect("nothing is focused")
            })
        };

        cx.update(|window, cx| window.focus_next(cx));
        assert_eq!(focused(cx), 0);

        cx.update(|window, cx| window.focus_next(cx));
        assert_eq!(focused(cx), 1, "a selected button lost its tab stop");

        cx.update(|window, cx| window.focus_next(cx));
        assert_eq!(focused(cx), 3, "Tab stopped on the disabled button");
    }

    impl Render for TabHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            // Declared first, second, third; the middle one opts out of the
            // Tab order. Tab must therefore visit 0 then 2.
            div()
                .id("tabs")
                .tab_group()
                .child(
                    Button::new("first")
                        .track_focus(&self.handles[0])
                        .tab_index(0)
                        .size(px(20.0)),
                )
                .child(
                    Button::new("skipped")
                        .track_focus(&self.handles[1])
                        .tab_index(1)
                        .tab_stop(false)
                        .size(px(20.0)),
                )
                .child(
                    Button::new("last")
                        .track_focus(&self.handles[2])
                        .tab_index(2)
                        .size(px(20.0)),
                )
        }
    }

    #[gpui::test]
    fn tab_visits_the_declared_order_and_skips_a_non_stop(cx: &mut TestAppContext) {
        let handles = cx.update(|cx| (0..3).map(|_| cx.focus_handle()).collect::<Vec<_>>());
        let (_, cx) = cx.add_window_view({
            let handles = handles.clone();
            move |_, _| TabHarness { handles }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let focused = |cx: &mut VisualTestContext, index: usize| {
            cx.update(|window, _| handles[index].is_focused(window))
        };

        cx.update(|window, cx| window.focus_next(cx));
        assert!(focused(cx, 0), "Tab did not reach the first button");

        cx.update(|window, cx| window.focus_next(cx));
        assert!(
            !focused(cx, 1),
            "Tab stopped on the button that declared tab_stop(false)"
        );
        assert!(focused(cx, 2), "Tab did not reach the third button");
    }
}

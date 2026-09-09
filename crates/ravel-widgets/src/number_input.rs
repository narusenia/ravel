// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's spin button: a number field with a step control on each side.
//!
//! [`gpui_base::NumberInput`] owns the whole mechanism — it holds the same
//! [`InputState`] a plain [`Input`](crate::Input) does, and it routes the two
//! buttons *and* the `Increment` / `Decrement` key actions through **one**
//! `on_step` closure. That single path is the property worth having: there is
//! no second way to step a value that could drift from the first, and this
//! module adds none.
//!
//! What Ravel decides is the dressing, and it is the same dressing
//! [`crate::input`] applies: the frame comes from [`crate::input_layers`] and
//! the two step buttons from [`crate::button_layers`], so a spin button and a
//! plain field sit on one border and one set of state colours.
//!
//! # Why the buttons are decorated rather than replaced
//!
//! `NumberInput::decrement_button` hands you the *base* button it already
//! built, wired to `on_step` and marked `focusable(false)`, and asks you to
//! decorate it. Putting a [`crate::Button`] inside instead would nest a second
//! button element in the first: two accessibility roles, two focus decisions,
//! and a click target that belongs to the outer one. So Ravel paints base's
//! button with the layers its own `Button` would have used. `button_layers` is
//! a pure function precisely so a second call site can share the rules rather
//! than copy them.
//!
//! # Why the frame is outside `gpui_base::NumberInput`, not on it
//!
//! Base's element routes whatever you hand `input(..)` into its **middle**
//! slot, between the two buttons. A [`crate::Input`] there would put a second
//! bordered frame inside the first — and a border drawn round the middle
//! third of a spin button says the wrong thing, because focus belongs to the
//! whole control. So the text slot gets the editing engine on its own
//! (`Entity<InputState>`, which is precisely what `Input` wraps) and the frame
//! goes on an element around all three parts. That frame is still Ravel's
//! `Input` frame: it is painted from `input_layers`, the same function, so the
//! two widgets cannot drift apart.
//!
//! # Why this is not `scrub_input`
//!
//! Ravel has two numeric editors and they are not alternatives. This one is a
//! spin button — visible step buttons, used for the dialog fields where a
//! value is typed once and nudged (export ranges, composition size, frame
//! rate, padding). [`crate::scrub_input`] is the After Effects gesture: a
//! label you drag sideways to change the value, used for every parameter row
//! in the Properties panel, where there is no room for buttons and no patience
//! for clicking. Both are kept.

use gpui::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_base::input::InputState;
use gpui_base::{Button as BaseButton, NumberInput as BaseNumberInput, StyledExt as _};

use crate::button::{ButtonLayers, ButtonVariant, button_layers, feedback_motion};
use crate::icon::{Icon, UiIcon};
use crate::input::{frame_is_focused, input_layers};
use crate::theme::ActiveTokens as _;
use crate::tokens::Density;

/// A Ravel number input.
///
/// As with [`Input`](crate::Input) the state is the caller's, and the step
/// size, minimum and maximum live on it (`InputState::step`, `min`, `max`) —
/// they are what stepping *means*, not how it looks.
#[derive(IntoElement)]
pub struct NumberInput {
    state: Entity<InputState>,
    density: Density,
    invalid: bool,
    style: StyleRefinement,
}

impl NumberInput {
    /// Paint `state` as a spin button on the default density step.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            density: Density::default(),
            invalid: false,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the spin button on the compact step (`row.compact` tall).
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Draw the spin button on `density`.
    pub fn density(mut self, density: Density) -> Self {
        self.density = density;
        self
    }

    /// Whether the text currently in the box cannot be used.
    ///
    /// The same hook [`crate::Input::invalid`] carries, and the same ranking:
    /// the danger border outranks the focus ring.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }
}

impl Styled for NumberInput {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for NumberInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let metrics = self.density.metrics(&theme);
        let presentation = self.state.read(cx).presentation();
        let disabled = presentation.is_disabled();
        let layers = input_layers(self.invalid, disabled, &theme.colors);
        let handle = presentation.focus_handle().clone();
        let (enter, _leave) = feedback_motion(&theme.motion);

        // The step buttons are Ravel's ghost buttons: no surface until
        // hovered, so a row of dialog fields shows one lit control at a time
        // rather than a wall of chrome.
        let buttons = button_layers(ButtonVariant::Ghost, false, disabled, &theme.colors);
        let dressing = StepButton {
            layers: buttons,
            size: metrics.height,
            icon: metrics.icon,
            motion: enter.clone(),
        };

        let content = BaseNumberInput::new(&self.state)
            .disabled(disabled)
            .size_full()
            // Left and right chevrons, not up and down. Base lays the three
            // parts out horizontally — button, text, button — and an arrow
            // that points down while sitting to the *left* of the value reads
            // as neither direction. Left/right is also the DCC convention the
            // plan takes its density from: Blender's number fields carry
            // exactly `<` and `>`. `ChevronUp` / `ChevronDown` would only be
            // right under `controls_right`, which stacks the pair vertically
            // and cannot survive the 20px compact step: two 10px buttons
            // cannot hold a 12px icon.
            .decrement_button({
                let dressing = dressing.clone();
                move |button| dressing.apply(button, UiIcon::ChevronLeft)
            })
            .increment_button(move |button| dressing.apply(button, UiIcon::ChevronRight))
            // The editing engine bare, with no frame of its own — see the
            // module docs. This is the same element `crate::Input` puts inside
            // its own border.
            .input(self.state.clone());

        div()
            .flex()
            .items_center()
            .h(metrics.height)
            .rounded(theme.radius.radius)
            .border_1()
            .text_size(metrics.font_size)
            .bg(layers.rest.surface)
            .text_color(layers.rest.foreground)
            // `focus`, not `focus_visible`, for the reason `crate::input`
            // documents: clicking into the field is how you start typing, so
            // the ring has to follow the caret rather than the keyboard.
            // Chosen here rather than installed as a semantic layer because
            // this frame is a plain element, not an `InputBase`.
            .border_color(match layers.focused {
                Some(face) if frame_is_focused(&handle, disabled, window) => face.border,
                _ => layers.rest.border,
            })
            // The step buttons fill the frame's full height, so their hover
            // surfaces would paint square over its rounded corners. Clipping
            // to the frame is one property; the alternative is per-corner
            // radius arithmetic on both buttons. This clips nothing that
            // matters — the focus ring is this element's own inset border,
            // not an outset one.
            .overflow_hidden()
            // Only the border animates, over the feedback token: UX invariant
            // 11 allows state feedback and nothing else.
            .transitions(|transitions| transitions.border_color(enter))
            .child(content)
            .refine_style(&self.style)
    }
}

/// The dressing both step buttons get.
///
/// A struct rather than a closure because the two buttons need the *same*
/// appearance and base hands each one to a separate `FnOnce`: a closure would
/// have to be cloned to be called twice, and cloning a closure to call it
/// twice is a worse way to say "these two are identical" than naming the
/// thing they share.
#[derive(Clone)]
struct StepButton {
    layers: ButtonLayers,
    size: gpui::Pixels,
    icon: gpui::Pixels,
    motion: gpui::Motion,
}

impl StepButton {
    fn apply(self, button: BaseButton, icon: UiIcon) -> BaseButton {
        let motion = self.motion;
        button
            // Square at the frame's own height, which is what lines the three
            // parts up without a magic number.
            .w(self.size)
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(self.layers.rest.foreground)
            .bg(self.layers.rest.surface)
            .transitions(move |transitions| transitions.bg(motion.clone()).text_color(motion))
            .when_some(self.layers.hover, |button, face| {
                button.hover(move |style| style.bg(face.surface).text_color(face.foreground))
            })
            .when_some(self.layers.pressed, |button, face| {
                button.active(move |style| style.bg(face.surface).text_color(face.foreground))
            })
            // No focus ring: base marks these `focusable(false)` so stepping
            // never pulls the caret out of the field, and a ring on something
            // Tab cannot reach would be a lie.
            .child(Icon::new(icon).size(self.icon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
        TestAppContext, VisualTestContext, div, point, px,
    };
    use gpui_base::StepAction;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Every step this harness saw, in order, however it arrived.
    type Steps = Rc<RefCell<Vec<StepAction>>>;

    struct StepHarness {
        state: Entity<InputState>,
        steps: Steps,
    }

    impl Render for StepHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let steps = self.steps.clone();
            // The element under test is Ravel's, but `on_step` is base's one
            // handler — which is exactly the seam being pinned. Ravel's
            // `NumberInput` does not expose `on_step`, so the harness reaches
            // the base element with Ravel's dressing left off; what it
            // measures is the routing, not the paint.
            div().id("harness").tab_group().size(px(200.0)).child(
                BaseNumberInput::new(&self.state)
                    .on_step(move |action, _, _| steps.borrow_mut().push(action))
                    .decrement_button(|button| button.w(px(20.0)).h(px(20.0)))
                    .increment_button(|button| button.w(px(20.0)).h(px(20.0)))
                    .input(crate::Input::new(&self.state).w(px(120.0)))
                    .w(px(180.0))
                    .h(px(20.0))
                    .into_any_element(),
            )
        }
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

    /// **Both the buttons and the arrow keys reach one handler.** This is the
    /// completion criterion for the unit, and it fails the moment a future
    /// edit gives either input source a step path of its own: the counts stop
    /// agreeing, or the pointer stops arriving at all.
    #[gpui::test]
    fn the_buttons_and_the_keys_step_through_one_handler(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let steps: Steps = Rc::new(RefCell::new(Vec::new()));
        let (view, cx) = cx.add_window_view({
            let steps = steps.clone();
            |window, cx| StepHarness {
                state: cx.new(|cx| InputState::new(window, cx).default_value("5")),
                steps,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // The decrement button sits left of the text region, the increment
        // button right of it.
        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        cx.simulate_click(point(px(170.0), px(10.0)), Modifiers::default());

        assert_eq!(
            *steps.borrow(),
            [StepAction::Decrement, StepAction::Increment],
            "the step buttons did not reach on_step"
        );
        steps.borrow_mut().clear();

        let state = view.read_with(cx, |harness, _| harness.state.clone());
        cx.update(|window, cx| {
            state.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        press(cx, "up");
        press(cx, "down");

        assert_eq!(
            *steps.borrow(),
            [StepAction::Increment, StepAction::Decrement],
            "the arrow keys did not reach the same on_step the buttons did"
        );
    }

    /// A harness that renders **Ravel's** element, not base's, so what is
    /// under test is the wiring: whether `NumberInput` passes the state's
    /// disabled flag through to base's gate.
    struct RavelHarness {
        state: Entity<InputState>,
    }

    impl Render for RavelHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("ravel-harness")
                .tab_group()
                .size(px(200.0))
                .child(NumberInput::new(&self.state).w(px(180.0)))
        }
    }

    fn ravel_harness(
        cx: &mut TestAppContext,
        disabled: bool,
    ) -> (&mut VisualTestContext, Entity<InputState>) {
        cx.update(gpui_base::init);
        let (view, cx) = cx.add_window_view(|window, cx| RavelHarness {
            state: cx.new(|cx| InputState::new(window, cx).default_value("5").step(1.0)),
        });
        let state = view.read_with(cx, |harness, _| harness.state.clone());
        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                if disabled {
                    state.set_disabled(true, cx);
                }
                state.focus(window, cx);
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, state)
    }

    /// The enabled baseline: pressing the right-hand arrow on Ravel's element
    /// really does move the value. Without this the disabled test below could
    /// pass because nothing steps at all.
    #[gpui::test]
    fn the_increment_arrow_moves_the_value(cx: &mut TestAppContext) {
        let (cx, state) = ravel_harness(cx, false);

        // The increment button is the right-hand end of the 180px frame.
        cx.simulate_click(point(px(170.0), px(10.0)), Modifiers::default());

        assert_eq!(
            state.read_with(cx, |state, _| state.value().to_string()),
            "6",
            "the increment arrow did not step the value"
        );
    }

    /// UX invariant 6 for the spin button: a disabled field must not step from
    /// the pointer *or* the keyboard. Base gates both on its own `disabled`
    /// flag, and Ravel feeds that flag from the state — dropping
    /// `.disabled(disabled)` in `render` fails exactly here.
    #[gpui::test]
    fn a_disabled_number_input_steps_from_neither_source(cx: &mut TestAppContext) {
        let (cx, state) = ravel_harness(cx, true);

        cx.simulate_click(point(px(170.0), px(10.0)), Modifiers::default());
        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        press(cx, "up");
        press(cx, "down");

        assert_eq!(
            state.read_with(cx, |state, _| state.value().to_string()),
            "5",
            "a disabled spin button stepped its value"
        );
    }

    /// The dressing, without a window: the step buttons are ghost buttons and
    /// they lose their hover surface when the field is disabled. A disabled
    /// spin button whose arrows still lit up would be UX invariant 6 broken.
    #[test]
    fn the_step_buttons_stop_reacting_when_the_field_is_disabled() {
        let colors = crate::tokens::Colors::light();

        let enabled = button_layers(ButtonVariant::Ghost, false, false, &colors);
        assert!(enabled.hover.is_some());
        assert!(enabled.pressed.is_some());

        let disabled = button_layers(ButtonVariant::Ghost, false, true, &colors);
        assert_eq!(disabled.hover, None, "a disabled arrow still hovers");
        assert_eq!(disabled.pressed, None, "a disabled arrow still presses");
    }
}

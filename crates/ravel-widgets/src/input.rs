// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's text input.
//!
//! Behaviour is borrowed and appearance is Ravel's, the same split
//! [`crate::button`] makes — but the borrowed half is far larger here. IME
//! composition, text selection, undo, the blinking caret, clipboard, the
//! accessibility role and the Tab stop all live in `gpui_base`'s
//! [`InputState`], which is the *same type* gpui-component re-exported: moving
//! off gpui-component changes no behaviour at all, because there was never any
//! gpui-component behaviour in the state to lose.
//!
//! What this module decides is the frame: a border, two density steps, and
//! which of the states owns the border colour. That is decided in one pure
//! function, [`input_layers`], so the rules are readable and testable without a
//! window.
//!
//! # The focus ring is `focus`, not `focus_visible`
//!
//! **This is the one place Ravel deliberately disagrees with
//! [`crate::button`].** A button's ring is `focus_visible`, so it appears only
//! under keyboard navigation: a clicked button is already telling you where it
//! is by being under the pointer, and a ring on every click is noise.
//!
//! A text input is the opposite. Clicking into a field *is* the normal way to
//! start typing, and the field that has the caret is the one thing the user
//! must be able to find before they type — the pointer has usually moved on by
//! then. So the ring follows plain focus, and it does so through
//! [`InputBase::focused`], which this module feeds from
//! `FocusHandle::is_focused` rather than from GPUI's `focus_visible`
//! pseudo-state. `the_ring_follows_focus_from_the_pointer_too` is the test
//! that pins it, and it is the deliberate mirror of the button's
//! `the_ring_follows_the_keyboard_and_not_the_pointer`.
//!
//! # Which state owns the border
//!
//! There is one border and three states want it, so they are ranked:
//! **disabled, then invalid, then focused.** Disabled outranks everything
//! because `gpui_base`'s `state_style` contract resolves it last by
//! construction. Invalid outranks focus because of the three it is the only
//! one with no second channel: focus is also being told by the caret sitting
//! in the field, while a value that cannot be parsed has nothing but the
//! border to say so — and hiding that exactly while the user is in the field
//! fixing it is the worst possible moment to hide it.
//!
//! # What this module does not build
//!
//! No prefix, suffix, clear button, mask toggle or context menu. Those are
//! gpui-component's `Input` trimmings and Ravel called none of them; the
//! measured call sites use two things, a density step and a width.

use gpui::{
    App, Entity, FocusHandle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StyleRefinement, Styled, Window, prelude::FluentBuilder as _,
};
use gpui_base::input::InputState;
use gpui_base::{InputBase, StyledExt as _};

use crate::theme::ActiveTokens as _;
use crate::tokens::{Colors, Density};

/// One layer of an input's appearance.
///
/// Unlike a button's, every layer carries a border: the border is the input's
/// whole shape (the plan gives Input one and Button none), so a layer that
/// left it unset would erase the frame rather than inherit it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputFace {
    /// The surface behind the text.
    pub surface: Hsla,
    /// Text colour. The caret and the selection are *not* set here — see
    /// [`crate::theme::set_active_tokens`] for why they come from `gpui-base`.
    pub foreground: Hsla,
    /// The 1px border, inside the element's bounds.
    pub border: Hsla,
}

/// Every layer an input installs, in the order they resolve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputLayers {
    /// The instance style.
    pub rest: InputFace,
    /// `InputStyles::focused`. `None` while disabled or invalid, which is how
    /// those two outrank focus for the border — see the module docs.
    pub focused: Option<InputFace>,
    /// `InputStyles::disabled`, resolved last.
    pub disabled: Option<InputFace>,
}

/// The appearance of an input in every state it can reach.
///
/// Four states, and none of them is `hover`: an input has no hover appearance
/// at all. A button's hover says "this is pressable"; a text field's cursor
/// already says "this is typeable", and a field that lit up under the pointer
/// would compete with the one that actually holds the caret. gpui-component's
/// `Input` painted no hover either, so this is the behaviour Ravel already had.
///
/// The surface is `background` in every layer, including `disabled`. The plan
/// removes a disabled control's surface only where the surface is what says
/// "pressable" (a button's fill, a hover step); an input's surface is its
/// *frame*, so it stays and only the text fades. Removing it would make a
/// disabled field look like a hole in the panel.
pub fn input_layers(invalid: bool, disabled: bool, colors: &Colors) -> InputLayers {
    let rest = InputFace {
        // The plan gives an Input a border precisely so it does not need a
        // surface of its own to be findable, and models no input-surface
        // token; the panel's own ground is what is left, and it is what
        // gpui-component painted here too.
        surface: colors.background,
        foreground: colors.foreground,
        border: if invalid {
            colors.danger
        } else {
            colors.border
        },
    };

    InputLayers {
        rest,
        focused: (!disabled && !invalid).then(|| InputFace {
            border: colors.focus_ring(),
            ..rest
        }),
        disabled: disabled.then(|| InputFace {
            foreground: colors.disabled_foreground(),
            // The border is named rather than inherited from `rest`, which
            // carries `danger` while `invalid`. Disabled outranks invalid: a
            // field the user cannot reach is not a warning they can act on,
            // and shouting at them about a value they are not allowed to fix
            // is the one thing worse than saying nothing.
            border: colors.border,
            ..rest
        }),
    }
}

/// Whether an input's frame paints its focused layer.
///
/// **This one line is what the module's focus section is about**, so it is a
/// named function rather than an expression inside `render`: a test can call
/// exactly what `render` calls, and a change to `focus_visible` semantics —
/// ANDing in `Window::last_input_was_keyboard` — then fails
/// `the_ring_follows_focus_from_the_pointer_too` instead of passing silently.
pub fn frame_is_focused(handle: &FocusHandle, disabled: bool, window: &Window) -> bool {
    // Deliberately *not* gated on `window.last_input_was_keyboard()`. That
    // gate is what makes a Button's ring keyboard-only; a text field needs the
    // ring whenever it holds the caret, however the caret got there.
    handle.is_focused(window) && !disabled
}

/// A Ravel text input.
///
/// The state is the caller's: `Input` borrows an [`InputState`] entity and
/// paints it. Everything configurable about the *value* — the placeholder, the
/// default, whether it is disabled or read-only, the validation pattern — lives
/// on that state and is set where the state is built. This element decides
/// appearance only, which is why it has four builders and not forty.
#[derive(IntoElement)]
pub struct Input {
    state: Entity<InputState>,
    density: Density,
    invalid: bool,
    accessibility_label: Option<SharedString>,
    tab_index: isize,
    style: StyleRefinement,
}

impl Input {
    /// Paint `state` on the default density step.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            density: Density::default(),
            invalid: false,
            accessibility_label: None,
            tab_index: 0,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the input on the compact step (`row.compact` tall).
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Draw the input on `density`.
    pub fn density(mut self, density: Density) -> Self {
        self.density = density;
        self
    }

    /// Whether the text currently in the box cannot be used.
    ///
    /// This is the hook the plan asks for, and it exists because
    /// [`crate::scrub_input`] already has the state it describes: a scrubber
    /// in text-edit mode accepts arbitrary keystrokes and only finds out
    /// whether they parse when the edit is committed, at which point an
    /// unparseable value is silently reverted. `invalid` is how a caller says
    /// "this would be reverted" *before* the commit, so the revert stops being
    /// a surprise.
    ///
    /// It is presentation only: it does not stop editing, because the whole
    /// point is that the user is mid-way through typing something that is not
    /// valid yet.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    /// The label exposed to accessibility clients.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Where the input sits in the Tab order.
    ///
    /// Whether Tab reaches it at all is not a question this element answers:
    /// [`InputState`] builds its focus handle with `tab_stop(true)`, so every
    /// Ravel input is keyboard-reachable by construction (UX invariant 10) and
    /// there is no flag here that could turn it off by accident.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }
}

impl Styled for Input {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let metrics = self.density.metrics(&theme);
        // `disabled` is read from the state, never written to it. An element
        // that took a `disabled` flag would have to push it into the state
        // from `render`, and a `render` that mutates state is what the GPUI
        // rules forbid — so the state stays the single owner and this is a
        // read. It is also the honest model: `set_disabled` is what actually
        // stops the editing, and only the state can do that.
        let presentation = self.state.read(cx).presentation();
        let disabled = presentation.is_disabled();
        let layers = input_layers(self.invalid, disabled, &theme.colors);
        let handle = presentation.focus_handle().clone();
        let (enter, _leave) = crate::button::feedback_motion(&theme.motion);

        InputBase::new(("ravel-input", self.state.entity_id()))
            // Plain focus, not `focus_visible`: see [`frame_is_focused`] and
            // the module docs. A click into the field paints the ring, which
            // is the whole difference from `Button`.
            .focused(frame_is_focused(&handle, disabled, window))
            .disabled(disabled)
            .track_focus(&handle)
            .tab_index(self.tab_index)
            .flex()
            .items_center()
            .h(metrics.height)
            .px(metrics.padding_x)
            .rounded(theme.radius.radius)
            // A permanent 1px border, inside the bounds, so the ring cannot be
            // clipped by an `overflow_hidden` ancestor and the field never
            // changes size when it takes focus.
            .border_1()
            .text_size(metrics.font_size)
            .bg(layers.rest.surface)
            .text_color(layers.rest.foreground)
            .border_color(layers.rest.border)
            // Only the border animates, and only over the feedback token: UX
            // invariant 11 allows state feedback and nothing else. The text
            // must not animate — a value easing toward its target is a
            // correctness bug in a field you can type into.
            .transitions(|transitions| transitions.border_color(enter))
            .styles(|styles| {
                styles
                    .focused(|style| match layers.focused {
                        Some(face) => style.border_color(face.border),
                        None => style,
                    })
                    .disabled(|style| match layers.disabled {
                        Some(face) => style.text_color(face.foreground),
                        None => style,
                    })
            })
            .when_some(self.accessibility_label, |input, label| {
                input.accessibility_label(label)
            })
            // The editing engine itself. `Entity<InputState>` is the element
            // that lays out and paints the text, the caret and the selection.
            .child(self.state.clone())
            .refine_style(&self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Colors;

    fn layers(invalid: bool, disabled: bool) -> InputLayers {
        input_layers(invalid, disabled, &Colors::light())
    }

    /// The disabled half of the state machine. A disabled field must not paint
    /// a focus ring: it cannot take the caret, so a ring would point at a
    /// field that will never receive a keystroke.
    #[test]
    fn a_disabled_input_reaches_no_focus_ring() {
        for invalid in [false, true] {
            let disabled = layers(invalid, true);
            assert_eq!(disabled.focused, None, "ring while disabled");
            assert!(disabled.disabled.is_some(), "lost its disabled layer");
        }
    }

    /// The plan's disabled rule, the half that is easy to get wrong: fade the
    /// text, keep the frame. A mechanical "remove the surface" would leave a
    /// disabled field looking like a hole in the panel.
    #[test]
    fn a_disabled_input_fades_its_text_and_keeps_its_frame() {
        let colors = Colors::light();
        let face = layers(false, true).disabled.expect("the disabled layer");

        assert_eq!(face.foreground, colors.disabled_foreground());
        assert_eq!(face.surface, colors.background, "the frame lost its ground");
        assert_eq!(face.border, colors.border, "the frame lost its border");
    }

    #[test]
    fn an_invalid_input_shows_a_danger_border() {
        let colors = Colors::light();
        assert_eq!(layers(false, false).rest.border, colors.border);
        assert_eq!(layers(true, false).rest.border, colors.danger);
    }

    /// The precedence the module docs argue for. If focus won instead, the
    /// danger border would vanish the moment the user clicked into the field
    /// to fix the value — which is the only moment it is useful.
    #[test]
    fn invalid_outranks_focus_for_the_border() {
        let colors = Colors::light();

        let valid = layers(false, false).focused.expect("the focused layer");
        assert_eq!(valid.border, colors.focus_ring());

        assert_eq!(
            layers(true, false).focused,
            None,
            "focus overwrote the danger border"
        );
        assert_eq!(layers(true, false).rest.border, colors.danger);
    }

    /// The module ranks disabled above invalid, so the danger border has to
    /// stop at the disabled layer. Inheriting it from `rest` — which carries
    /// `danger` while invalid — would have let invalid win instead.
    #[test]
    fn disabled_outranks_invalid_for_the_border() {
        let colors = Colors::light();

        let both = layers(true, true);
        assert_eq!(
            both.rest.border, colors.danger,
            "the resting layer still records that the value is invalid"
        );
        assert_eq!(
            both.disabled.expect("the disabled layer").border,
            colors.border,
            "but the layer resolved last drops the warning the user cannot act on"
        );
        assert_eq!(both.focused, None, "and focus is not in the running either");
    }

    /// One rule, both palettes: the derivation never asks which mode is on.
    #[test]
    fn both_palettes_take_their_colours_from_their_own_tokens() {
        for colors in [Colors::light(), Colors::dark()] {
            let resting = input_layers(false, false, &colors).rest;
            assert_eq!(resting.surface, colors.background);
            assert_eq!(resting.foreground, colors.foreground);
            assert_eq!(resting.border, colors.border);
        }
    }

    // -----------------------------------------------------------------------
    // What only a window can answer: that the ring is driven by focus rather
    // than by the input source, and that Tab reaches the field.
    // -----------------------------------------------------------------------

    use gpui::{
        AppContext as _, Context, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers,
        Render, TestAppContext, VisualTestContext, div, point, px,
    };

    struct InputHarness {
        state: Entity<InputState>,
    }

    impl Render for InputHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("harness")
                .tab_group()
                .size(px(200.0))
                .child(Input::new(&self.state).w(px(160.0)))
        }
    }

    fn harness(cx: &mut TestAppContext) -> (&mut VisualTestContext, Entity<InputState>) {
        cx.update(gpui_base::init);
        let (view, cx) = cx.add_window_view(|window, cx| InputHarness {
            state: cx.new(|cx| InputState::new(window, cx)),
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = view.read_with(cx, |harness, _| harness.state.clone());
        (cx, state)
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

    /// **The deliberate mirror of the button's
    /// `the_ring_follows_the_keyboard_and_not_the_pointer`.** A pointer click
    /// leaves `last_input_was_keyboard()` false, so a ring hung off
    /// `focus_visible` — the button's rule — would not appear. This asserts
    /// that the input's ring appears anyway, which is only true because it is
    /// driven by `is_focused`.
    #[gpui::test]
    fn the_ring_follows_focus_from_the_pointer_too(cx: &mut TestAppContext) {
        let (cx, state) = harness(cx);

        cx.simulate_click(point(px(30.0), px(10.0)), Modifiers::default());

        cx.update(|window, cx| {
            let handle = state.read(cx).presentation().focus_handle().clone();
            assert!(
                handle.is_focused(window),
                "the click did not focus the input"
            );
            assert!(
                !window.last_input_was_keyboard(),
                "a click must not arm focus_visible; without this the next \
                 assertion could pass for the wrong reason"
            );

            // The predicate `render` actually feeds `InputBase::focused`.
            // Gating it on `last_input_was_keyboard` — the Button rule — makes
            // this false, which is the regression being pinned.
            assert!(
                frame_is_focused(&handle, false, window),
                "a pointer-focused input does not paint its ring"
            );
            // And a disabled field paints none, however it was focused.
            assert!(!frame_is_focused(&handle, true, window));
        });
    }

    /// UX invariant 10: Tab must reach a text field. The stop comes from
    /// `InputState`'s own handle rather than from this element, so this pins
    /// the thing that would break if a future edit wrapped the state in a
    /// frame that swallowed the focus.
    #[gpui::test]
    fn tab_reaches_the_input(cx: &mut TestAppContext) {
        let (cx, state) = harness(cx);

        cx.update(|window, cx| {
            let handle = state.read(cx).presentation().focus_handle().clone();
            assert!(!handle.is_focused(window), "focused before Tab");
            window.focus_next(cx);
        });

        cx.update(|window, cx| {
            let handle = state.read(cx).presentation().focus_handle().clone();
            assert!(handle.is_focused(window), "Tab did not reach the input");
        });
    }

    /// The completion criterion the plan states as behaviour rather than
    /// appearance, and the reason `disabled` is read from the state instead of
    /// being an element flag: `set_disabled` is what actually refuses the
    /// keystroke, and nothing this module does can weaken it.
    #[gpui::test]
    fn a_disabled_input_refuses_edits(cx: &mut TestAppContext) {
        let (cx, state) = harness(cx);

        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.set_value("start", window, cx);
                state.set_disabled(true, cx);
                state.focus(window, cx);
            });
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        press(cx, "x");

        cx.update(|_, cx| {
            assert_eq!(
                state.read(cx).value(),
                "start",
                "a disabled input accepted a keystroke"
            );
            assert!(
                state.read(cx).presentation().is_disabled(),
                "the state lost its disabled flag"
            );
        });
    }

    struct TabOrderHarness {
        states: Vec<Entity<InputState>>,
        handles: Vec<FocusHandle>,
    }

    impl Render for TabOrderHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("tab-order")
                .tab_group()
                .child(Input::new(&self.states[0]).tab_index(0).w(px(80.0)))
                .child(Input::new(&self.states[1]).tab_index(1).w(px(80.0)))
        }
    }

    /// Two fields, declared in order: Tab visits the first and then the
    /// second. Without this, a `tab_index` that never reached the element
    /// would look identical in a one-field test.
    #[gpui::test]
    fn tab_visits_the_declared_order(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let states: Vec<_> = (0..2)
                .map(|_| cx.new(|cx| InputState::new(window, cx)))
                .collect();
            let handles = states
                .iter()
                .map(|state| state.read(cx).presentation().focus_handle().clone())
                .collect();
            TabOrderHarness { states, handles }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let handles = view.read_with(cx, |harness, _| harness.handles.clone());

        cx.update(|window, cx| window.focus_next(cx));
        cx.update(|window, _| {
            assert!(handles[0].is_focused(window), "Tab missed the first input");
        });

        cx.update(|window, cx| window.focus_next(cx));
        cx.update(|window, _| {
            assert!(handles[1].is_focused(window), "Tab missed the second input");
        });
    }
}

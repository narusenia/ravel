// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's checkbox.
//!
//! Behaviour is borrowed and appearance is Ravel's, the same split
//! [`crate::button`] makes. [`gpui_base::Checkbox`] owns the focus handle, the
//! tab stop, the `CheckBox` accessibility role with its three toggle states,
//! and **one activation path for the pointer, Enter and Space** — including
//! the rule for what an activation lands on, which is why this module does not
//! compute a next state of its own. That is UX invariant 10 held by
//! construction; this module must never grow an `on_key_down`.
//!
//! What this module decides is the appearance of the states, in one pure
//! function, [`checkbox_layers`], so the rules are readable and testable
//! without a window. `render` only applies what that function returned.
//!
//! # The paint that says "checked" survives being disabled
//!
//! The one place this widget's disabled rule differs from a button's. A button
//! loses its surface when disabled, because that surface only ever said "you
//! can press this". A checkbox's fill says something else — *it is checked* —
//! so removing it would draw a disabled, checked box as an empty one. The fill
//! therefore stays at [`DISABLED_ALPHA`] instead of going away, and the same
//! goes for the border and the mark. Only the layers that speak about
//! pressability (hover, press) are dropped outright.
//!
//! # Three states, and why the third one is drawn
//!
//! No caller passes [`CheckboxState::Indeterminate`] today. It is drawn anyway,
//! as a dash, because the primitive models it and the Properties panel's
//! multi-selection is what it is for: a bool that differs across the selected
//! nodes is neither on nor off. A widget that could not paint it would push
//! that panel back into inventing a third look of its own.
//!
//! # The mark is `background`, except on the faded fill
//!
//! The visual spec punches the mark out in `background`, and that is what a
//! full-strength fill gets. It is deliberately **not** [`Colors::readable_on`]:
//! measured on the shipped palettes, `readable_on(primary)` picks *foreground*
//! in the light one (`#5B6EE1` is 4.8:1 against black and 4.4:1 against white),
//! so a mark derived that way would be a black check in the light theme and a
//! near-black one in the dark theme — the same ink on the same blue, rather
//! than the punched-out mark the spec chose.
//!
//! The disabled fill is where the spec has nothing to say, and where a
//! `background` mark stops working: 38% of `primary` over a light ground is
//! pale, and white on pale is 1.3:1. There the readable end is asked for, so
//! the check stays a check — which is the whole point of keeping the fill at
//! all.

use std::rc::Rc;

use gpui::{
    App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _, transparent_black,
};
use gpui_base::{Checkbox as BaseCheckbox, CheckboxState, StyledExt as _};

use crate::button::feedback_motion;
use crate::icon::{Icon, UiIcon};
use crate::theme::ActiveTokens as _;
use crate::tokens::{
    CHECKBOX_MARK_SIZE, CHECKBOX_SIZE, Colors, DISABLED_ALPHA, Density, with_alpha,
};

type ChangeHandler = Rc<dyn Fn(&CheckboxState, &mut Window, &mut App)>;

/// The glyph inside the box, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckboxMark {
    /// Nothing: an unchecked box is its frame and no more.
    None,
    /// A check: this one is on.
    Check,
    /// A dash: the selection disagrees with itself.
    Dash,
}

impl CheckboxMark {
    /// The icon that draws this mark, or `None` for an empty box.
    pub fn icon(self) -> Option<UiIcon> {
        match self {
            Self::None => None,
            Self::Check => Some(UiIcon::Check),
            Self::Dash => Some(UiIcon::Minus),
        }
    }
}

/// The 14px box: its fill, its frame, and what is drawn in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckboxBox {
    /// The fill. `transparent_black()` means "no fill", which is a real value
    /// rather than `None` because a style *refinement* can only overwrite a
    /// field, never unset one.
    pub surface: Hsla,
    /// The 1px frame.
    pub border: Hsla,
    /// Which glyph sits in the box.
    pub mark: CheckboxMark,
    /// The glyph's colour. Meaningless when `mark` is [`CheckboxMark::None`].
    pub mark_color: Hsla,
}

/// Every layer a checkbox installs, in the order they resolve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckboxLayers {
    /// The box itself, which carries the state.
    pub indicator: CheckboxBox,
    /// The label's colour.
    pub label: Hsla,
    /// The surface the whole control shows under the pointer. `None` while
    /// disabled.
    pub hover: Option<Hsla>,
    /// The surface it shows while held. `None` while disabled.
    pub pressed: Option<Hsla>,
    /// The keyboard focus ring. `None` while disabled.
    pub focus_ring: Option<Hsla>,
}

/// The appearance of a checkbox in every state it can reach.
///
/// # How the states are derived
///
/// The box carries the state and the control carries the feedback, which is
/// the split that makes both readable: the fill and the mark say *what the
/// value is*, so they are `primary` and the readable end of the ramp on it;
/// hover and press say *what the pointer is doing*, so they are the same two
/// surfaces a ghost button uses ([`Colors::hover_surface`],
/// [`Colors::pressed_surface`]) and they sit behind the box and its label
/// alike. Nothing here asks which palette is in force.
///
/// Disabling keeps the state paint at [`DISABLED_ALPHA`] and drops the
/// feedback layers entirely — see the module docs for why the two halves are
/// treated differently.
pub fn checkbox_layers(state: CheckboxState, disabled: bool, colors: &Colors) -> CheckboxLayers {
    let mark = match state {
        CheckboxState::Unchecked => CheckboxMark::None,
        CheckboxState::Checked => CheckboxMark::Check,
        CheckboxState::Indeterminate => CheckboxMark::Dash,
    };
    // Everything the fill has to say is said by being there, so both marked
    // states carry it.
    let filled = mark != CheckboxMark::None;
    let fade = |color: Hsla| {
        if disabled {
            with_alpha(color, DISABLED_ALPHA)
        } else {
            color
        }
    };

    let surface = if filled {
        fade(colors.primary)
    } else {
        transparent_black()
    };
    let border = if filled {
        // The same colour as the fill: a frame in `border` around a `primary`
        // fill reads as a box with a smaller box painted inside it.
        fade(colors.primary)
    } else {
        fade(colors.border)
    };

    CheckboxLayers {
        indicator: CheckboxBox {
            surface,
            border,
            mark,
            // The spec's punched-out mark at full strength; the readable end
            // once the fill is faded, measured against the fill as it
            // composites over the panel — see the module docs.
            mark_color: if disabled {
                colors.readable_on(surface)
            } else {
                colors.background
            },
        },
        label: if disabled {
            colors.disabled_foreground()
        } else {
            colors.foreground
        },
        // Not installed at all while disabled: GPUI resolves `hover` *after*
        // the element style, so a layer left installed would still light up
        // under the pointer.
        hover: (!disabled).then(|| colors.hover_surface()),
        pressed: (!disabled).then(|| colors.pressed_surface()),
        focus_ring: (!disabled).then(|| colors.focus_ring()),
    }
}

/// A Ravel checkbox.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    density: Density,
    state: CheckboxState,
    disabled: bool,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    tab_index: isize,
    tab_stop: bool,
    on_change: Option<ChangeHandler>,
    style: StyleRefinement,
}

impl Checkbox {
    /// An unchecked checkbox on the default density step.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            density: Density::default(),
            state: CheckboxState::Unchecked,
            disabled: false,
            label: None,
            accessibility_label: None,
            tab_index: 0,
            tab_stop: true,
            on_change: None,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the checkbox on the compact step (`row.compact` tall).
    ///
    /// The box does not shrink — [`CHECKBOX_SIZE`] is one size — but the row it
    /// occupies and the label on it do.
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Draw the checkbox on `density`.
    pub fn density(mut self, density: Density) -> Self {
        self.density = density;
        self
    }

    /// The controlled value.
    pub fn checked(mut self, checked: bool) -> Self {
        self.state = if checked {
            CheckboxState::Checked
        } else {
            CheckboxState::Unchecked
        };
        self
    }

    /// The controlled value, including [`CheckboxState::Indeterminate`].
    pub fn state(mut self, state: CheckboxState) -> Self {
        self.state = state;
        self
    }

    /// The checkbox's text. Clicking it toggles, as it is part of the control.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Whether the checkbox refuses pointer and keyboard activation.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The label exposed to accessibility clients. Defaults to [`Checkbox::label`].
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Where the checkbox sits in the Tab order.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }

    /// Whether Tab reaches the checkbox at all.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    /// The activation handler, for the pointer, Enter and Space alike.
    ///
    /// The handler is given the state the activation lands on, which the
    /// primitive decides: unchecked *and* indeterminate become checked, checked
    /// becomes unchecked. The activating `ClickEvent` is dropped on the way
    /// through — no Ravel call site reads its modifiers, and a handler shaped
    /// `Fn(&T, &mut Window, &mut App)` is the one `Context::listener` produces.
    pub fn on_change(
        mut self,
        handler: impl Fn(&CheckboxState, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl Styled for Checkbox {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let metrics = self.density.metrics(&theme);
        let layers = checkbox_layers(self.state, self.disabled, &theme.colors);
        let (enter, _leave) = feedback_motion(&theme.motion);

        let accessibility_label = self
            .accessibility_label
            .clone()
            .or_else(|| self.label.clone());

        let indicator = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(CHECKBOX_SIZE)
            .rounded(theme.radius.radius)
            .border_1()
            .border_color(layers.indicator.border)
            .bg(layers.indicator.surface)
            .when_some(layers.indicator.mark.icon(), |box_, icon| {
                box_.child(
                    Icon::new(icon)
                        .size(CHECKBOX_MARK_SIZE)
                        .text_color(layers.indicator.mark_color),
                )
            });

        BaseCheckbox::new(self.id)
            .state(self.state)
            .disabled(self.disabled)
            .flex()
            .items_center()
            .h(metrics.height)
            .gap(metrics.gap)
            .rounded(theme.radius.radius)
            // The same arrangement `button` uses: a permanent 1px border that
            // only takes a colour under `focus_visible`, so the ring is inside
            // the bounds — two dozen `overflow_hidden` ancestors would clip an
            // outset one — and the control never changes size.
            .border_1()
            .border_color(transparent_black())
            .text_size(metrics.font_size)
            .text_color(layers.label)
            // Only the state feedback animates, over the feedback token: UX
            // invariant 11 allows that and nothing else, so no size or
            // position is listed here.
            .transitions(|transitions| {
                transitions
                    .bg(enter.clone())
                    .text_color(enter.clone())
                    .border_color(enter)
            })
            .when_some(layers.hover, |checkbox, surface| {
                checkbox.hover(move |style| style.bg(surface))
            })
            .when_some(layers.pressed, |checkbox, surface| {
                checkbox.active(move |style| style.bg(surface))
            })
            .when_some(layers.focus_ring, |checkbox, ring| {
                checkbox.focus_visible(move |style| style.border_color(ring))
            })
            .tab_index(self.tab_index)
            .tab_stop(self.tab_stop)
            .when_some(accessibility_label, |checkbox, label| {
                checkbox.accessibility_label(label)
            })
            .when_some(self.on_change, |checkbox, on_change| {
                checkbox.on_change(move |state, _event, window, cx| on_change(&state, window, cx))
            })
            .child(indicator)
            .when_some(self.label, |checkbox, label| {
                checkbox.child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
            })
            .refine_style(&self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Colors;

    const STATES: [CheckboxState; 3] = [
        CheckboxState::Unchecked,
        CheckboxState::Checked,
        CheckboxState::Indeterminate,
    ];

    fn palettes() -> [Colors; 2] {
        [Colors::light(), Colors::dark()]
    }

    /// The four states the gallery has to be able to show apart. Rest is the
    /// control with no surface at all, so the three feedback surfaces are
    /// compared against each other and against the ground they sit on.
    #[test]
    fn the_four_states_can_be_told_apart() {
        for colors in palettes() {
            for state in STATES {
                let enabled = checkbox_layers(state, false, &colors);
                let disabled = checkbox_layers(state, true, &colors);

                let hover = enabled.hover.expect("an enabled checkbox hovers");
                let pressed = enabled.pressed.expect("an enabled checkbox presses");

                assert_ne!(hover, colors.background, "{state:?} hover is invisible");
                assert_ne!(pressed, hover, "{state:?} press cannot be told from hover");
                assert_ne!(
                    disabled.label, enabled.label,
                    "{state:?} disabled reads as enabled"
                );
            }
        }
    }

    /// A control that cannot act must not react. GPUI resolves `hover` after
    /// the element style, so the only way to hold this is to not install the
    /// layer at all.
    #[test]
    fn a_disabled_checkbox_reaches_no_hover_press_or_ring() {
        for colors in palettes() {
            for state in STATES {
                let disabled = checkbox_layers(state, true, &colors);
                assert_eq!(disabled.hover, None, "{state:?} hover while disabled");
                assert_eq!(disabled.pressed, None, "{state:?} press while disabled");
                assert_eq!(disabled.focus_ring, None, "{state:?} ring while disabled");
            }
        }
    }

    /// The rule this widget exists to get right: the fill is *state*, not
    /// pressability, so disabling fades it instead of removing it. Removing it
    /// would draw a disabled checked box exactly like an unchecked one.
    #[test]
    fn a_disabled_check_is_faded_and_still_a_check() {
        for colors in palettes() {
            for state in [CheckboxState::Checked, CheckboxState::Indeterminate] {
                let disabled = checkbox_layers(state, true, &colors).indicator;
                let unchecked = checkbox_layers(CheckboxState::Unchecked, true, &colors).indicator;

                assert_eq!(disabled.surface, with_alpha(colors.primary, DISABLED_ALPHA));
                assert_ne!(disabled.surface, transparent_black());
                assert_ne!(
                    disabled.surface, unchecked.surface,
                    "{state:?} disabled looks unchecked"
                );
                assert_ne!(disabled.mark, CheckboxMark::None);
            }
        }
    }

    /// Which glyph belongs to which state, and that the empty box really is
    /// empty. This is the mapping the Properties panel's multi-selection will
    /// read, so it is pinned rather than left to `render`.
    #[test]
    fn each_state_has_its_own_mark() {
        for colors in palettes() {
            let mark = |state| checkbox_layers(state, false, &colors).indicator.mark;

            assert_eq!(mark(CheckboxState::Unchecked), CheckboxMark::None);
            assert_eq!(mark(CheckboxState::Checked), CheckboxMark::Check);
            assert_eq!(mark(CheckboxState::Indeterminate), CheckboxMark::Dash);

            assert_eq!(CheckboxMark::None.icon(), None);
            assert_eq!(CheckboxMark::Check.icon(), Some(UiIcon::Check));
            assert_eq!(CheckboxMark::Dash.icon(), Some(UiIcon::Minus));
        }
    }

    /// Unchecked is the frame and nothing else, and the frame is the `border`
    /// token — the visual spec's "枠だけで面なし".
    #[test]
    fn an_unchecked_box_is_a_frame_with_no_fill() {
        for colors in palettes() {
            let indicator = checkbox_layers(CheckboxState::Unchecked, false, &colors).indicator;
            assert_eq!(indicator.surface, transparent_black());
            assert_eq!(indicator.border, colors.border);
        }
    }

    /// The fill is `primary` and the mark is punched out of it in
    /// `background` — the spec's "マークを background 色で抜く". Not
    /// `readable_on`, which measures *foreground* as the better end on the
    /// light palette's `primary` and would paint a black check there.
    #[test]
    fn a_checked_box_is_primary_with_the_mark_punched_out_of_it() {
        for colors in palettes() {
            let checked = checkbox_layers(CheckboxState::Checked, false, &colors).indicator;
            assert_eq!(checked.surface, colors.primary);
            assert_eq!(checked.border, colors.primary);
            assert_eq!(checked.mark_color, colors.background);
        }

        // The measurement that rules `readable_on` out at full strength, kept
        // here so a palette change that makes the two agree is visible rather
        // than silent.
        let light = Colors::light();
        assert_eq!(light.readable_on(light.primary), light.foreground);
    }

    /// The faded fill is the one place the mark is measured instead of named:
    /// `background` on 38% `primary` is 1.3:1 on a light ground, which would
    /// undo the whole reason the fill is kept.
    #[test]
    fn the_mark_on_a_faded_fill_is_the_readable_end() {
        for colors in palettes() {
            let faded = checkbox_layers(CheckboxState::Checked, true, &colors).indicator;
            assert_eq!(faded.mark_color, colors.readable_on(faded.surface));
        }
    }

    // -----------------------------------------------------------------------
    // What only a window can answer: which state an activation lands on, and
    // that the pointer and the keyboard both arrive through one handler.
    // -----------------------------------------------------------------------

    use gpui::{
        Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext,
        VisualTestContext, point, px,
    };
    use std::cell::RefCell;

    struct ActivationHarness {
        state: CheckboxState,
        disabled: bool,
        changes: Rc<RefCell<Vec<CheckboxState>>>,
    }

    impl Render for ActivationHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let changes = self.changes.clone();
            div().id("harness").tab_group().size(px(100.0)).child(
                Checkbox::new("checkbox")
                    .state(self.state)
                    .disabled(self.disabled)
                    .label("Remember me")
                    .size_full()
                    .on_change(move |state, _, _| changes.borrow_mut().push(*state)),
            )
        }
    }

    fn activation(
        cx: &mut TestAppContext,
        state: CheckboxState,
        disabled: bool,
    ) -> (&mut VisualTestContext, Rc<RefCell<Vec<CheckboxState>>>) {
        let changes = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let changes = changes.clone();
            move |_, _| ActivationHarness {
                state,
                disabled,
                changes,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, changes)
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

    /// The transition table, read through the widget rather than restated by
    /// it: unchecked and indeterminate both activate to checked, and checked
    /// activates to unchecked. A copy of this rule in Ravel would be a second
    /// place for it to rot.
    #[gpui::test]
    fn every_state_activates_to_the_one_the_primitive_names(cx: &mut TestAppContext) {
        for (from, to) in [
            (CheckboxState::Unchecked, CheckboxState::Checked),
            (CheckboxState::Indeterminate, CheckboxState::Checked),
            (CheckboxState::Checked, CheckboxState::Unchecked),
        ] {
            let (cx, changes) = activation(cx, from, false);
            cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
            assert_eq!(&*changes.borrow(), &[to], "{from:?} activated wrongly");
        }
    }

    /// UX invariant 10, the half that rots silently: Enter and Space arrive
    /// through **the same handler** a click does. An `on_key_down` beside it
    /// would make this count 4 instead of 3.
    #[gpui::test]
    fn enter_and_space_activate_through_the_change_handler(cx: &mut TestAppContext) {
        let (cx, changes) = activation(cx, CheckboxState::Unchecked, false);

        // Focus it the way a user would; that click is the first change.
        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        cx.update(|window, cx| {
            assert!(
                window.focused(cx).is_some(),
                "the checkbox did not take focus"
            );
            window.draw(cx).clear(cx);
        });

        press(cx, "enter");
        press(cx, "space");

        assert_eq!(
            &*changes.borrow(),
            &[
                CheckboxState::Checked,
                CheckboxState::Checked,
                CheckboxState::Checked
            ],
            "the pointer, Enter and Space did not all reach on_change"
        );
    }

    /// A disabled checkbox is inert to the pointer *and* to the keyboard.
    #[gpui::test]
    fn a_disabled_checkbox_activates_from_neither_input(cx: &mut TestAppContext) {
        let (cx, changes) = activation(cx, CheckboxState::Unchecked, true);

        cx.simulate_click(point(px(10.0), px(10.0)), Modifiers::default());
        cx.update(|window, cx| window.focus_next(cx));
        press(cx, "enter");
        press(cx, "space");

        assert!(changes.borrow().is_empty());
    }
}

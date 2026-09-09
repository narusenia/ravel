// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The colour picker owns the arrow keys while its popup is open.
//!
//! `default.toml` binds `Left` / `Right` to frame stepping, and until this unit
//! the predicate those bindings carry yielded to two things only: a focused
//! text input and an open menu. A colour picker's popup is neither — it is not
//! a `PopupMenu`, and its face is not an `Input` — so the workspace kept the
//! arrows while the popup was open. That is `MED-APP-16` a third time, one
//! context deeper again.
//!
//! **Measured, by deleting the widget's `key_context` and running this file:**
//! the playhead steps and the face gets *nothing*. gpui dispatches the matched
//! binding and stops there, so the face's own key handler never runs — the
//! symptom is a nudge that does nothing while the playhead jumps, not two
//! things happening at once.
//!
//! `crates/ravel-app/tests/keybinding_overrides.rs` pins the predicate's
//! *text*. This file pins the behaviour, which is the only thing that proves
//! the context reaches the element: the widget could declare
//! `ColorPickerSurface` on a node the popup's focus never sits under and every
//! predicate assertion would still pass.
//!
//! The two halves are asserted against each other in one test rather than
//! separately, because "the arrow did nothing at all" would satisfy each half
//! on its own.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, TestAppContext, Window, div, px,
};
use ravel_app::workspace::{FrameStepForward, build_keybindings, workspace_binding_context};
use ravel_ui::shell::AppShell;
use ravel_widgets::{ColorPicker, ColorPickerState, face_point, hsla_from_hsv};

/// The context the workspace's own bindings sit under, so the control case has
/// a node their predicate can match at.
const WORKSPACE_CONTEXT: &str = "Workspace";

/// A picker whose face point is `(0.5, 0.8)`: room on both sides of every
/// axis, so a nudge that lands anywhere else is a real difference.
fn seed() -> gpui::Hsla {
    hsla_from_hsv(0.5, 0.5, 0.8, 1.0)
}

struct Harness {
    state: Entity<ColorPickerState>,
    focus: gpui::FocusHandle,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus)
            .tab_group()
            .size(px(400.0))
            .child(ColorPicker::new(&self.state))
    }
}

/// **The unit's completion criterion.** With the face focused, `Right` moves
/// the saturation and the playhead's command does not run; with the same key
/// pressed outside the popup, the command runs. Both halves, one test.
#[gpui::test]
fn an_arrow_on_the_face_moves_the_colour_and_not_the_playhead(cx: &mut TestAppContext) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");

    // The playhead's own binding, registered exactly as the application
    // registers it — from the shell's table, through `build_keybindings`.
    let steps = Rc::new(Cell::new(0usize));
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.bind_keys(build_keybindings(&AppShell::default()));
        let counted = steps.clone();
        cx.on_action(move |_: &FrameStepForward, _| {
            counted.set(counted.get() + 1);
        });
    });
    assert!(
        workspace_binding_context().contains("!ColorPickerSurface"),
        "the workspace bindings do not yield to the picker at all"
    );

    let (view, cx) = cx.add_window_view(|window, cx| Harness {
        state: cx.new(|cx| ColorPickerState::new(window, cx).default_value(seed())),
        focus: cx.focus_handle(),
    });
    let (state, root) = view.read_with(cx, |harness, _| {
        (harness.state.clone(), harness.focus.clone())
    });

    // The control: the same keystroke, with the popup shut. Without this the
    // assertion below could pass because the binding never worked here.
    cx.update(|window, cx| {
        root.focus(window, cx);
        window.draw(cx).clear(cx);
    });
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    assert_eq!(
        steps.get(),
        1,
        "the playhead binding must reach the workspace while no picker is open"
    );

    // Open the picker and Tab into the face.
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.set_open(true, cx));
        window.draw(cx).clear(cx);
    });
    cx.update(|window, cx| window.focus_next(cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let before = state
        .read_with(cx, |state, _| state.value())
        .expect("the seeded value");
    cx.simulate_keystrokes("right");
    cx.run_until_parked();

    let after = state
        .read_with(cx, |state, _| state.value())
        .expect("still a value");
    assert!(
        face_point(after).saturation > face_point(before).saturation,
        "the arrow did not move the saturation: {:?} → {:?}",
        face_point(before),
        face_point(after)
    );
    assert_eq!(
        steps.get(),
        1,
        "the arrow stepped the playhead as well as the colour"
    );
}

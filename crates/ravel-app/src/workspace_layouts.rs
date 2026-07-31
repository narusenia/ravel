// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Manage Layouts dialog body (REQ-UI-005).
//!
//! `PresetLibrary::save_custom` has existed since the preset library did, with
//! nothing in the UI reaching it — a named layout could be stored but never
//! created or recalled. This view is that route: a name to save the current
//! arrangement under, the list of saved ones with apply and delete, and the
//! opt-in that decides whether a saved project carries the layout with it.
//!
//! The opt-in lives here rather than in the Save dialog because the Save dialog
//! is the platform's own file chooser (`App::prompt_for_new_path`), which cannot
//! host a control of ours. Making it a preference instead of a per-save question
//! also matches how it behaves: it is a standing choice about how this user's
//! projects are written, not something to re-answer on every save.
//!
//! The view owns no layout state. It reads the session's named layouts on every
//! render and acts through [`crate::workspace::RavelWorkspace`], so nothing here
//! can drift from the shell.

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable as _};
use ravel_i18n::t;

use crate::workspace::RavelWorkspace;

/// The Manage Layouts dialog body.
pub struct WorkspaceLayoutsForm {
    /// Name for the layout being saved.
    name: Entity<InputState>,
    session: WeakEntity<RavelWorkspace>,
    focus_handle: FocusHandle,
}

impl WorkspaceLayoutsForm {
    /// Builds the form against the live session.
    pub fn new(
        session: WeakEntity<RavelWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(SharedString::from(t!("workspace.layouts.name_placeholder")))
        });
        // Focus stays with the dialog's own focus trap: a form must not grab
        // focus while it is being constructed (`.agents/rules/gpui.md`).
        let focus_handle = cx.focus_handle();
        Self {
            name,
            session,
            focus_handle,
        }
    }

    /// The saved layout names, in library order.
    fn saved_names(&self, cx: &App) -> Vec<SharedString> {
        self.session
            .upgrade()
            .map(|session| {
                session
                    .read(cx)
                    .shell()
                    .presets()
                    .custom_names()
                    .map(SharedString::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Saves the current arrangement under the typed name, and clears the field
    /// so the button cannot silently overwrite it on a second click.
    fn save_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.with_session(cx, move |session, cx| {
            session.save_current_layout_as(name, cx);
        });
        self.name.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        cx.notify();
    }

    fn apply(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.with_session(cx, move |session, cx| {
            session.apply_custom_layout(name.as_ref(), cx);
        });
        cx.notify();
    }

    fn delete(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.with_session(cx, move |session, cx| {
            session.remove_custom_layout(name.as_ref(), cx);
        });
        cx.notify();
    }

    fn with_session(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut RavelWorkspace, &mut Context<RavelWorkspace>),
    ) {
        let Some(session) = self.session.upgrade() else {
            tracing::warn!("the session was dropped before the layout dialog acted");
            return;
        };
        session.update(cx, f);
    }

    fn saved_row(&self, name: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let apply = name.clone();
        let delete = name.clone();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .py(px(2.0))
            .child(div().flex_grow().truncate().text_sm().child(name))
            .child(
                Button::new(SharedString::from(format!("layout-apply-{apply}")))
                    .xsmall()
                    .label(SharedString::from(t!("workspace.layouts.apply")))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.apply(apply.clone(), cx);
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("layout-delete-{delete}")))
                    .xsmall()
                    .ghost()
                    .label(SharedString::from(t!("workspace.layouts.delete")))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.delete(delete.clone(), cx);
                    })),
            )
    }
}

impl Focusable for WorkspaceLayoutsForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspaceLayoutsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let names = self.saved_names(cx);
        let embed = crate::layout_persist::embed_in_projects(cx);
        let muted = cx.theme().colors.muted_foreground;

        let saved: AnyElement = if names.is_empty() {
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(t!("workspace.layouts.empty")))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(
                    names
                        .into_iter()
                        .map(|name| self.saved_row(name, cx).into_any_element()),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_3()
            .track_focus(&self.focus_handle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_grow().child(Input::new(&self.name).small()))
                    .child(
                        Button::new("layout-save-current")
                            .small()
                            .primary()
                            .label(SharedString::from(t!("workspace.layouts.save")))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.save_current(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(t!("workspace.layouts.saved"))),
            )
            .child(saved)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new("layout-embed-in-projects")
                            .label(SharedString::from(t!("workspace.layouts.embed")))
                            .checked(embed)
                            .on_click(|checked, _window, cx| {
                                crate::layout_persist::set_embed_in_projects(*checked, cx);
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(t!("workspace.layouts.embed_hint"))),
                    ),
            )
    }
}

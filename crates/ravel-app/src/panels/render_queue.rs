// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The render queue panel (`render-export-plan.md`, unit 5): what has been
//! submitted, how far it has got, and a way to stop it.
//!
//! The panel owns no queue state. Rows come from
//! [`RenderService`](crate::export::RenderService), which outlives every
//! panel — closing this one does not stop a render, and reopening it shows
//! the jobs that were already running. The arithmetic behind "47 of 120
//! frames" is `ravel-core`'s [`JobProgress`](ravel_core::runtime::JobProgress),
//! read through the headless rows; this file is text and pixels.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::progress::Progress;
use gpui_component::{ActiveTheme, Sizable as _};
use ravel_core::runtime::RenderJobId;
use ravel_i18n::t;
use ravel_ui::layout::PanelInstanceId;
use ravel_ui::panels::render_queue::RenderQueueRow;

use crate::export::RenderService;

const HEADER_HEIGHT: f32 = 24.0;

pub struct RenderQueueGpuiPanel {
    /// The session's queue; `None` only when the panel outlives it (tests
    /// build panels without a workspace).
    service: Option<Entity<RenderService>>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    service_sub: Option<Subscription>,
}

impl RenderQueueGpuiPanel {
    pub fn new(instance: PanelInstanceId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = crate::export::render_service(cx);
        // The service notifies when a row changes, which is the only thing
        // this panel draws — no document mirroring, so no epoch gate.
        let service_sub = service
            .as_ref()
            .map(|service| cx.observe(service, |_this: &mut Self, _service, cx| cx.notify()));
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);
        Self {
            service,
            focus_handle,
            focus_subscriptions,
            service_sub,
        }
    }

    fn cancel(&mut self, job: RenderJobId, cx: &mut Context<Self>) {
        if let Some(service) = &self.service {
            service.update(cx, |service, _cx| service.cancel(job));
        }
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        if let Some(service) = &self.service {
            service.update(cx, |service, cx| service.clear_finished(cx));
        }
    }

    fn render_header(&self, has_finished: bool, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        div()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(colors.border)
            .when(has_finished, |header| {
                header.child(
                    Button::new("render-queue-clear")
                        .ghost()
                        .xsmall()
                        .label(SharedString::from(t!("render_queue.clear_finished")))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.clear_finished(cx);
                        })),
                )
            })
    }

    fn render_row(
        &self,
        index: usize,
        row: &RenderQueueRow,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let job = row.job();
        let failed = row.failure().is_some();
        let heading = format!(
            "{}  ·  {} / {} {}",
            row.composition(),
            row.rendered(),
            row.total_frames(),
            t!("render_queue.frames"),
        );
        div()
            .id(("render-queue-row", index))
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(72.0))
                            .flex_shrink_0()
                            .text_xs()
                            .truncate()
                            .text_color(if failed {
                                colors.danger
                            } else {
                                colors.muted_foreground
                            })
                            .child(SharedString::from(t!(row.state_key()))),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .text_xs()
                            .truncate()
                            .child(SharedString::from(heading)),
                    )
                    .when(row.is_cancellable(), |header| {
                        header.child(
                            Button::new(("render-queue-cancel", index))
                                .ghost()
                                .xsmall()
                                .label(SharedString::from(t!("render_queue.cancel")))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.cancel(job, cx);
                                })),
                        )
                    }),
            )
            .child(
                Progress::new(("render-queue-progress", index))
                    .value(row.fraction() * 100.0)
                    .h(px(4.0)),
            )
            .child(
                div()
                    .text_xs()
                    .truncate()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(
                        row.directory().to_string_lossy().into_owned(),
                    )),
            )
            .when_some(row.failure().map(str::to_owned), |element, message| {
                element.child(
                    div()
                        .text_xs()
                        .text_color(colors.danger)
                        .child(SharedString::from(message)),
                )
            })
    }
}

impl Focusable for RenderQueueGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RenderQueueGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        // Cloned out of the service so the row loop does not hold a borrow of
        // it across the `cx.listener` calls the buttons need.
        let rows: Vec<RenderQueueRow> = self
            .service
            .as_ref()
            .map(|service| service.read(cx).rows().rows().to_vec())
            .unwrap_or_default();
        let has_finished = rows.iter().any(RenderQueueRow::is_finished);

        let mut list = div()
            .id("render-queue-list")
            .debug_selector(|| "render-queue-panel".into())
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_y_scroll();
        if rows.is_empty() {
            // Nothing has been submitted. `File ▸ Export…` is the way in, and
            // saying so is more use than an empty box.
            list = list.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("render_queue.empty"))),
            );
        } else {
            for (index, row) in rows.iter().enumerate() {
                list = list.child(self.render_row(index, row, cx));
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.list)
            .track_focus(&self.focus_handle)
            .child(self.render_header(has_finished, cx))
            .child(list)
    }
}

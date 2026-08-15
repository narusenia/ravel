// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The attribute spreadsheet panel (REQ-CORE-010 inspection UI,
//! `attribute-spreadsheet-plan.md` unit 3): the evaluated geometry of the
//! selected node, one row per element and one column per attribute.
//!
//! The panel owns no evaluation of its own. The selection declares its target
//! through [`super::selected_node_eval_target`] and
//! `project_state::scoped_eval_targets` folds it into the Viewer's request, so
//! the value read here is the one the frame on screen was built from. What it
//! reads is [`EvalResults`], keyed by `(path, node)` — never by `NodeId` alone,
//! which is not an identity (`OVL-2`).
//!
//! Everything decidable from a `Geometry` alone — the column order, the cell
//! text, the empty states — lives headless in
//! [`ravel_ui::panels::attribute_spreadsheet`]. This file is the
//! [`TableDelegate`] around it plus the domain tab bar.
//!
//! Read-only in v1: attributes are the graph's output, so a cell edit would be
//! erased by the next evaluation (see the plan's "決定事項"). Column sorting and
//! reordering are switched off for the same reason — the delegate implements
//! neither, and a control that does nothing is worse than no control.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::table::{Column, ColumnFixed, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme, v_flex};
use ravel_core::geometry::{AttributeType, Domain, Geometry};
use ravel_core::types::NodeData;
use ravel_i18n::t;
use ravel_ui::layout::PanelInstanceId;
use ravel_ui::panels::attribute_spreadsheet::{
    AttributeSpreadsheetPanel, DOMAINS, SheetColumn, SheetEmpty, cell_text, columns,
    domain_label_key, element_count, empty_state,
};
use std::sync::Arc;

use crate::project_state::ProjectState;

use super::viewer::geometry::as_geometry;
use super::viewer::overlay::EvalResults;

const HEADER_HEIGHT: f32 = 24.0;

/// What one resolution of the globals found. Assembled in one place so the
/// three questions the empty states ask are answered from a single reading of
/// the selection, the document and the results.
#[derive(Default)]
struct Resolved {
    /// A node target in the composition on screen.
    has_node_target: bool,
    /// That node declares a geometry output, so a target was requested.
    has_geometry_output: bool,
    /// The value that target evaluated to, if it has arrived.
    value: Option<Arc<dyn NodeData>>,
}

/// The table's data source: the resolved geometry plus the columns derived
/// from it.
///
/// The columns are cached rather than rebuilt per call because `column()` is
/// asked once per column on every layout pass, and deriving one means walking
/// and sorting the whole attribute listing.
pub struct AttributeSheetDelegate {
    sheet: AttributeSpreadsheetPanel,
    value: Option<Arc<dyn NodeData>>,
    columns: Vec<SheetColumn>,
    /// Element count per [`DOMAINS`] entry, for the tab labels.
    counts: [usize; DOMAINS.len()],
    empty: Option<SheetEmpty>,
}

impl AttributeSheetDelegate {
    fn new() -> Self {
        Self {
            sheet: AttributeSpreadsheetPanel::new(),
            value: None,
            columns: Vec::new(),
            counts: [0; DOMAINS.len()],
            empty: Some(SheetEmpty::NoSelection),
        }
    }

    fn geometry(&self) -> Option<&Geometry> {
        self.value.as_ref().and_then(as_geometry)
    }

    /// Replaces the snapshot. Returns whether anything on screen changed —
    /// `set_global` wakes an observer even when it stored an identical value,
    /// so an unchanged evaluation must not cost a table rebuild.
    fn apply(&mut self, resolved: Resolved) -> bool {
        let same_value = match (&self.value, &resolved.value) {
            (None, None) => true,
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            _ => false,
        };
        self.value = resolved.value;
        let domain = self.sheet.domain();
        let geometry = self.geometry();
        let empty = empty_state(
            resolved.has_node_target,
            resolved.has_geometry_output,
            geometry,
            domain,
        );
        let next_columns = geometry.map(|g| columns(g, domain)).unwrap_or_default();
        let next_counts = DOMAINS.map(|domain| {
            geometry
                .map(|g| element_count(g, domain))
                .unwrap_or_default()
        });
        let changed = !same_value
            || empty != self.empty
            || next_columns != self.columns
            || next_counts != self.counts;
        self.empty = empty;
        self.columns = next_columns;
        self.counts = next_counts;
        changed
    }

    /// Rows in the domain on show, or none while a message is up.
    fn rows(&self) -> usize {
        match (self.empty, self.geometry()) {
            (None, Some(geometry)) => element_count(geometry, self.sheet.domain()),
            _ => 0,
        }
    }
}

/// The pixel width a column of `ty` opens at, wide enough for the formatted
/// value without a horizontal scroll for the common case.
fn column_width(ty: Option<AttributeType>) -> f32 {
    match ty {
        None => 56.0,
        Some(AttributeType::F32) => 96.0,
        Some(AttributeType::I32) => 72.0,
        Some(AttributeType::Bool) => 72.0,
        Some(AttributeType::Vec2) => 152.0,
        Some(AttributeType::Vec3) => 208.0,
        Some(AttributeType::Vec4 | AttributeType::Color) => 264.0,
        Some(AttributeType::Str) => 160.0,
    }
}

/// Whether values of `ty` read better right-aligned (every numeric type).
fn is_numeric(ty: Option<AttributeType>) -> bool {
    !matches!(ty, Some(AttributeType::Bool | AttributeType::Str))
}

impl TableDelegate for AttributeSheetDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        if self.empty.is_some() {
            return 0;
        }
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(column) = self.columns.get(col_ix) else {
            return Column::new("", "");
        };
        let name: SharedString = if column.is_row_number() {
            SharedString::from(t!("attribute_spreadsheet.row"))
        } else {
            SharedString::from(column.name.to_string())
        };
        Column::new(column.name.to_string(), name)
            .width(px(column_width(column.ty)))
            .when(is_numeric(column.ty), Column::text_right)
            // The element number stays visible while the attribute columns
            // scroll past it: a value with no row to attach it to is unusable.
            .when(column.is_row_number(), |column| {
                column.fixed(ColumnFixed::Left)
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = match (self.geometry(), self.columns.get(col_ix)) {
            (Some(geometry), Some(column)) => {
                cell_text(geometry, self.sheet.domain(), row_ix, column)
            }
            _ => String::new(),
        };
        SharedString::from(text)
    }
}

pub struct AttributeSpreadsheetGpuiPanel {
    table: Entity<TableState<AttributeSheetDelegate>>,
    /// The session's document; `None` only when the panel outlives it (tests
    /// build panels without a workspace).
    project: Option<Entity<ProjectState>>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    results_sub: Subscription,
}

impl AttributeSpreadsheetGpuiPanel {
    pub fn new(instance: PanelInstanceId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let table = cx.new(|cx| {
            TableState::new(AttributeSheetDelegate::new(), window, cx)
                // Neither is implemented, and the plan puts cell editing and
                // statistics out of scope for v1; showing the affordances of a
                // sort and a column drag that go nowhere is the "UI on
                // something that does not move" mistake.
                .sortable(false)
                .col_movable(false)
        });

        // The selection decides which node's result is read, and it changes
        // without an evaluation behind it (clicking a second node in the same
        // graph, or clearing the selection).
        let selection_sub =
            cx.observe_global::<super::SelectedPropertiesTarget>(|this: &mut Self, cx| {
                this.refresh(cx);
            });
        // The results are republished wholesale with every evaluation, which is
        // the only way new attribute values arrive. `ProjectState` deliberately
        // does not notify its observers when it publishes them.
        let results_sub = cx.observe_global::<EvalResults>(|this: &mut Self, cx| {
            this.refresh(cx);
        });

        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);

        let mut panel = Self {
            table,
            project,
            focus_handle,
            focus_subscriptions,
            selection_sub,
            results_sub,
        };
        // A panel opened while something is already selected has to show it:
        // the observers above only fire on later writes.
        panel.refresh(cx);
        panel
    }

    /// Reads the selection, the document and the results once, and hands the
    /// answer to the delegate.
    fn resolve(&self, cx: &App) -> Resolved {
        let Some(document) = self
            .project
            .as_ref()
            .map(|project| project.read(cx).document())
        else {
            return Resolved::default();
        };
        let target = super::selected_node_eval_target(document, cx);
        let value = target.as_ref().and_then(|target| {
            super::viewer::overlay::eval_result(cx.try_global::<EvalResults>()?, document, target)
        });
        Resolved {
            has_node_target: super::selected_node_for_inspection(cx).is_some(),
            has_geometry_output: target.is_some(),
            value,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        let changed = self.table.update(cx, |state, cx| {
            let changed = state.delegate_mut().apply(resolved);
            if changed {
                // Column set and widths are prepared once per refresh, not per
                // frame; without this a new domain keeps the old layout.
                state.refresh(cx);
            }
            changed
        });
        if changed {
            cx.notify();
        }
    }

    fn set_domain(&mut self, domain: Domain, cx: &mut Context<Self>) {
        let changed = self.table.update(cx, |state, _cx| {
            state.delegate_mut().sheet.set_domain(domain)
        });
        if !changed {
            return;
        }
        // Rebuilds the columns and the empty state for the new domain, and
        // refreshes the table layout when they differ. The notify is
        // unconditional: two domains may share a column set and still hold
        // different values.
        self.refresh(cx);
        cx.notify();
    }

    fn render_domain_tab(&self, domain: Domain, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let delegate = self.table.read(cx).delegate();
        let index = DOMAINS.iter().position(|d| *d == domain).unwrap_or(0);
        let count = delegate.counts[index];
        let active = delegate.sheet.domain() == domain;
        // A domain with no elements is not a place to go: switching to it can
        // only show the "no elements" line.
        let enabled = count > 0;
        div()
            .id(SharedString::from(format!(
                "attribute-spreadsheet-domain-{}",
                domain_label_key(domain)
            )))
            .h(px(18.0))
            .px_1p5()
            .flex()
            .items_center()
            .gap_1()
            .rounded_sm()
            .text_xs()
            .text_color(if active {
                colors.foreground
            } else {
                colors.muted_foreground
            })
            .when(active, |tab| tab.bg(colors.list_active))
            .when(enabled, |tab| {
                tab.cursor_pointer()
                    .hover(|style| style.bg(colors.list_hover))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_domain(domain, cx);
                    }))
            })
            .when(!enabled, |tab| tab.opacity(0.5))
            .child(SharedString::from(t!(domain_label_key(domain))))
            .child(
                div()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(count.to_string())),
            )
    }
}

impl Focusable for AttributeSpreadsheetGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AttributeSpreadsheetGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let mut tabs = div()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .border_b_1()
            .border_color(colors.border);
        for domain in DOMAINS {
            tabs = tabs.child(self.render_domain_tab(domain, cx));
        }

        // The message replaces the table rather than being drawn inside it
        // (`TableDelegate::render_empty`, which would also leave an empty
        // header strip above it). Nothing to show means nothing to lay out:
        // the tab bar stays, because the counts on it are how a user finds the
        // domain that does have elements.
        let empty = self.table.read(cx).delegate().empty;
        v_flex()
            .size_full()
            .bg(colors.list)
            .track_focus(&self.focus_handle)
            .child(tabs)
            .child(match empty {
                Some(empty) => div()
                    .flex_grow()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!(empty.message_key())))
                    .into_any_element(),
                None => div()
                    .flex_grow()
                    .overflow_hidden()
                    .child(DataTable::new(&self.table).stripe(true))
                    .into_any_element(),
            })
    }
}

// A `use super::*;` glob in a test module in a file that expands the gpui
// proc macros crashes rustc 1.95 (SIGBUS); name what the tests need instead.
#[cfg(test)]
mod tests {
    use super::{AttributeSheetDelegate, AttributeSpreadsheetGpuiPanel, Resolved};
    use gpui_component::table::{ColumnFixed, TableDelegate as _};
    use ravel_core::geometry::{AttributeArray, Domain, Geometry, names};
    use ravel_core::types::{NodeData, Vec2};
    use ravel_ui::panels::attribute_spreadsheet::SheetEmpty;
    use std::sync::Arc;

    fn scatter_like() -> Arc<dyn NodeData> {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        geometry
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1, 2]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(2.0, 0.0)]),
            )
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.0, 0.5, 1.0]))
            .unwrap();
        Arc::new(geometry)
    }

    fn delegate_with(value: Arc<dyn NodeData>) -> AttributeSheetDelegate {
        let mut delegate = AttributeSheetDelegate::new();
        assert!(delegate.apply(Resolved {
            has_node_target: true,
            has_geometry_output: true,
            value: Some(value),
        }));
        delegate
    }

    /// The delegate answers `columns_count` / `rows_count` / `column` without a
    /// window, which is the interface `DataTable` drives.
    #[gpui::test]
    fn the_delegate_describes_the_selected_domain(cx: &mut gpui::TestAppContext) {
        let mut delegate = delegate_with(scatter_like());
        cx.update(|cx| {
            assert_eq!(delegate.rows_count(cx), 2);
            assert_eq!(delegate.columns_count(cx), 3);
            let keys: Vec<String> = (0..delegate.columns_count(cx))
                .map(|ix| delegate.column(ix, cx).key.to_string())
                .collect();
            assert_eq!(keys, ["#", "index", "P"]);
            // The element number is pinned; the attribute columns are not.
            assert_eq!(delegate.column(0, cx).fixed, Some(ColumnFixed::Left));
            assert_eq!(delegate.column(1, cx).fixed, None);

            assert!(delegate.sheet.set_domain(Domain::Instance));
            assert!(delegate.apply(Resolved {
                has_node_target: true,
                has_geometry_output: true,
                value: delegate.value.clone(),
            }));
            assert_eq!(delegate.rows_count(cx), 3);
            let keys: Vec<String> = (0..delegate.columns_count(cx))
                .map(|ix| delegate.column(ix, cx).key.to_string())
                .collect();
            assert_eq!(keys, ["#", "index", "P", "rot"]);
        });
    }

    /// Every empty state closes the table down to no columns and no rows, so
    /// the message is what the panel shows rather than an empty grid.
    #[gpui::test]
    fn the_empty_states_have_neither_rows_nor_columns(cx: &mut gpui::TestAppContext) {
        let value = scatter_like();
        let cases = [
            (false, false, None),
            (true, false, None),
            (true, true, None),
            (
                true,
                true,
                Some(Arc::new(Geometry::new()) as Arc<dyn NodeData>),
            ),
        ];
        cx.update(|cx| {
            for (has_node_target, has_geometry_output, value) in cases {
                let mut delegate = AttributeSheetDelegate::new();
                delegate.apply(Resolved {
                    has_node_target,
                    has_geometry_output,
                    value,
                });
                assert!(delegate.empty.is_some());
                assert_eq!(delegate.rows_count(cx), 0);
                assert_eq!(delegate.columns_count(cx), 0);
            }
            let mut delegate = AttributeSheetDelegate::new();
            delegate.apply(Resolved {
                has_node_target: true,
                has_geometry_output: true,
                value: Some(value),
            });
            assert!(delegate.empty.is_none());
        });
    }

    /// A message and a grid must not be on screen at once: while an empty
    /// state is up the table reports no rows, even if the last geometry is
    /// still cached in the delegate.
    #[gpui::test]
    fn a_message_leaves_no_rows_behind_it(cx: &mut gpui::TestAppContext) {
        let mut delegate = delegate_with(scatter_like());
        cx.update(|cx| {
            assert_eq!(delegate.rows_count(cx), 2);
            delegate.apply(Resolved {
                has_node_target: false,
                has_geometry_output: false,
                value: delegate.value.clone(),
            });
            assert_eq!(delegate.empty, Some(SheetEmpty::NoSelection));
            assert!(delegate.geometry().is_some());
            assert_eq!(delegate.rows_count(cx), 0);
        });
    }

    /// An identical republication must not cost a table rebuild
    /// (`set_global` wakes observers even when the value did not move).
    #[gpui::test]
    fn an_unchanged_result_reports_no_change(_cx: &mut gpui::TestAppContext) {
        let value = scatter_like();
        let mut delegate = delegate_with(value.clone());
        assert!(!delegate.apply(Resolved {
            has_node_target: true,
            has_geometry_output: true,
            value: Some(value),
        }));
    }

    /// The panel draws its tab bar and its table.
    #[gpui::test]
    fn the_panel_draws_without_a_project(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            AttributeSpreadsheetGpuiPanel::new(ravel_ui::layout::PanelInstanceId(1), window, cx)
        });
        let visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(gpui::size(gpui::px(520.0), gpui::px(300.0)));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, cx| {
                let delegate = panel.table.read(cx).delegate();
                assert_eq!(delegate.rows(), 0);
                assert!(delegate.empty.is_some());
            })
            .unwrap();
    }

    /// The table state survives a domain switch driven through the panel.
    #[gpui::test]
    fn switching_the_domain_repaints_the_table(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            AttributeSpreadsheetGpuiPanel::new(ravel_ui::layout::PanelInstanceId(1), window, cx)
        });
        let visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(gpui::size(gpui::px(520.0), gpui::px(300.0)));
        cx.run_until_parked();
        window
            .update(cx, |panel, _window, cx| {
                panel.table.update(cx, |state, _cx| {
                    let delegate = state.delegate_mut();
                    delegate.apply(Resolved {
                        has_node_target: true,
                        has_geometry_output: true,
                        value: Some(scatter_like()),
                    });
                });
                panel.set_domain(Domain::Instance, cx);
                let delegate = panel.table.read(cx).delegate();
                assert_eq!(delegate.sheet.domain(), Domain::Instance);
            })
            .unwrap();
        cx.run_until_parked();
    }
}

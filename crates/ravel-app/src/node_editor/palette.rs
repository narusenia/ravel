// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node search palette (DISC-3, REQ-UI-002): an incremental-search overlay
//! for adding nodes, opened with Tab or a canvas double-click, and from a
//! wire dropped on empty canvas (then only connectable types are offered).
//!
//! The palette is a transient component: the node editor creates a fresh
//! entity on every open and drops it on close, so no query text, selection,
//! or category filter survives into the next invocation. The list of
//! recently used types is the only state that persists, and it lives on the
//! panel (session memory), not here.
//!
//! Candidate generation and node creation are shared with the add-node
//! context menu: [`search_candidates`] flattens the same menu model, and
//! accepting a row calls the panel's existing `add_node_from_template` /
//! `add_node_from_edge_drop` paths, so a palette accept is the same Document
//! change (one undo step) as the equivalent menu pick.
//!
//! Keyboard handling: the query input keeps real focus (IME needs it), so
//! the input's own `Input`-context bindings own the arrow/enter/escape keys.
//! The palette intercepts those actions in the *capture* phase
//! (`capture_action`), which runs before the input's bubble-phase handlers —
//! no new actions are declared and no raw key checks are needed.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{self, Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Icon, Sizable as _};
use ravel_core::graph::Graph;
use ravel_core::id::NodeId;
use ravel_core::registry::{NodeCategory, NodeRegistry};
use ravel_i18n::t;
use ravel_ui::node_search::{SearchCandidate, filter_candidates};

use crate::assets::RavelIcon;
use crate::node_editor::painting::PortHit;
use crate::panels::node_editor::{
    AddNodeMenuGroup, add_node_menu_model, first_compatible_port, node_category_label,
    node_category_order,
};

/// Row height is fixed so [`ScrollHandle::scroll_to_item`] lines the
/// keyboard selection up with the rendered rows.
const ROW_HEIGHT: f32 = 26.0;

/// Result of the user's interaction with the palette.
pub enum PaletteEvent {
    /// A candidate was chosen (click or Enter); carries its `type_key`.
    Accept(String),
    /// The palette was closed without a choice (Escape, click outside, Tab).
    Dismiss,
}

/// Builds the palette's candidate list from the same model as the add-node
/// context menu, so both surfaces always offer the same node types. Labels
/// come from the menu model; descriptions are resolved through the locale
/// here (they are the second search target).
///
/// Public so the shipped-catalog integration tests
/// (`tests/node_search_palette.rs`) can search the locale-resolved strings.
pub fn search_candidates(registry: &NodeRegistry) -> Vec<SearchCandidate> {
    add_node_menu_model(registry)
        .into_iter()
        .flat_map(|group: AddNodeMenuGroup| {
            group.items.into_iter().map(move |item| SearchCandidate {
                description: crate::node_locale::description(&item.type_key),
                category: group.category,
                type_key: item.type_key,
                label: item.label,
            })
        })
        .collect()
}

/// Drops the candidates that could not connect to the dragged port `from`.
/// Compatibility is judged by the same `first_compatible_port` the edge-drop
/// accept path uses, so the filter never disagrees with what a pick would
/// have connected.
pub(crate) fn retain_connectable(
    candidates: Vec<SearchCandidate>,
    registry: &NodeRegistry,
    graph: &Graph,
    from: &PortHit,
) -> Vec<SearchCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| {
            // The scratch id never enters a graph; it only satisfies
            // `create_node`'s signature for the port-compatibility probe.
            registry
                .create_node(&candidate.type_key, NodeId::new(0))
                .is_some_and(|node| first_compatible_port(graph, from, &node).is_some())
        })
        .collect()
}

/// The categories present in `candidates`, in menu order. Drives the
/// category-filter chips, which hide categories the current invocation
/// cannot offer anyway (e.g. everything a wire drop filtered out).
fn present_categories(candidates: &[SearchCandidate]) -> Vec<NodeCategory> {
    let mut present: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.category)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    present.sort_by_key(|category| node_category_order(*category));
    present
}

/// The search palette overlay. Owns the query input and the ranked,
/// filtered view of its candidates.
pub struct SearchPalette {
    pub(crate) input: Entity<InputState>,
    pub(crate) candidates: Vec<SearchCandidate>,
    recents: Vec<String>,
    pub(crate) category_filter: Option<NodeCategory>,
    pub(crate) visible: Vec<usize>,
    pub(crate) selected: usize,
    scroll: ScrollHandle,
    #[allow(dead_code)]
    input_sub: Subscription,
}

impl SearchPalette {
    pub fn new(
        candidates: Vec<SearchCandidate>,
        recents: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(SharedString::from(t!("node_graph.search_placeholder")))
        });
        let input_sub = cx.subscribe_in(
            &input,
            window,
            |this, _state, event: &InputEvent, _window, cx| {
                if let InputEvent::Change = event {
                    this.refilter(cx);
                }
            },
        );
        let mut palette = Self {
            input,
            candidates,
            recents,
            category_filter: None,
            visible: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            input_sub,
        };
        palette.refilter(cx);
        palette
    }

    /// Focus the query input. Called once when the palette opens; the input
    /// keeps focus for the palette's whole lifetime (IME text entry).
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    /// The query input's focus handle. Teardown paths check this against
    /// the window's focus to move focus back to the canvas only when the
    /// palette actually holds it.
    pub(crate) fn input_focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }

    /// The current query text (tests).
    pub(crate) fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.visible = filter_candidates(
            &self.candidates,
            &self.query(cx),
            self.category_filter,
            &self.recents,
        );
        self.selected = 0;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(crate) fn set_category_filter(
        &mut self,
        category: Option<NodeCategory>,
        cx: &mut Context<Self>,
    ) {
        self.category_filter = category;
        self.refilter(cx);
    }

    /// Moves the selection by `delta` rows, wrapping around the ends, and
    /// scrolls the new selection into view.
    pub(crate) fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(len) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn accept_selected(&mut self, cx: &mut Context<Self>) {
        let Some(&index) = self.visible.get(self.selected) else {
            return;
        };
        cx.emit(PaletteEvent::Accept(
            self.candidates[index].type_key.clone(),
        ));
    }

    fn category_chip(
        &self,
        label: String,
        value: Option<NodeCategory>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let active = self.category_filter == value;
        div()
            .id(SharedString::from(format!(
                "palette-cat-{}",
                value.map_or("all".to_string(), |c| format!("{c:?}"))
            )))
            .px_2()
            .h(px(20.0))
            .flex()
            .items_center()
            .rounded_sm()
            .text_xs()
            .cursor_pointer()
            .when(active, |chip| chip.bg(colors.list_active))
            .when(!active, |chip| chip.text_color(colors.muted_foreground))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.set_category_filter(value, cx);
            }))
            .child(SharedString::from(label))
    }
}

impl EventEmitter<PaletteEvent> for SearchPalette {}

impl Render for SearchPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let visible = self.visible.clone();
        let selected = self.selected;

        let rows: Vec<_> = visible
            .iter()
            .enumerate()
            .map(|(row, &index)| {
                let candidate = &self.candidates[index];
                let icon = RavelIcon::for_node_type(&candidate.type_key, Some(candidate.category));
                let label = SharedString::from(candidate.label.clone());
                let category = SharedString::from(node_category_label(candidate.category));
                let type_key = candidate.type_key.clone();
                div()
                    .id(SharedString::from(format!("palette-row-{row}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(ROW_HEIGHT))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .when(row == selected, |el| el.bg(colors.list_active))
                    .hover(|el| el.bg(colors.list_hover))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.selected = row;
                        cx.emit(PaletteEvent::Accept(type_key.clone()));
                    }))
                    .child(Icon::new(icon).size_4().text_color(colors.muted_foreground))
                    .child(
                        div()
                            .flex_grow()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(category),
                    )
            })
            .collect();

        let mut chips = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border)
            .child(self.category_chip(t!("node_graph.search_all_categories"), None, cx));
        for category in present_categories(&self.candidates) {
            chips =
                chips.child(self.category_chip(node_category_label(category), Some(category), cx));
        }

        div()
            .id("node-search-palette")
            .flex()
            .flex_col()
            .w(px(340.0))
            .bg(colors.popover)
            .border_1()
            .border_color(colors.border)
            .rounded_md()
            .shadow_lg()
            .overflow_hidden()
            // The capture phase fires before the focused input's own handlers
            // (see the module docs): arrows move the row selection instead of
            // the text cursor, Enter picks the row, Escape closes.
            .capture_action(cx.listener(|this, _: &input::MoveUp, _window, cx| {
                this.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::MoveDown, _window, cx| {
                this.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Enter, _window, cx| {
                this.accept_selected(cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|_this, _: &input::Escape, _window, cx| {
                cx.emit(PaletteEvent::Dismiss);
                cx.stop_propagation();
            }))
            // Clicks inside the palette must not reach the overlay's
            // click-outside-to-dismiss handler underneath it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .p_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(Input::new(&self.input).small()),
            )
            .child(chips)
            .child(
                div()
                    .id("node-search-palette-rows")
                    .flex()
                    .flex_col()
                    .max_h(px(320.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .py_1()
                    .when(visible.is_empty(), |el| {
                        el.child(
                            div()
                                .px_2()
                                .py_3()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child(SharedString::from(t!("node_graph.search_no_matches"))),
                        )
                    })
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use super::*` re-exports gpui's `test` attribute macro (via the
    // module's `use gpui::*`); shadow it back to the built-in one so plain
    // `#[test]` keeps its meaning (same trick as the node editor tests).
    use core::prelude::v1::test;
    use ravel_core::registry::NodeTemplate;
    use ravel_core::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        registry
    }

    fn keys(candidates: &[SearchCandidate]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.type_key.as_str())
            .collect()
    }

    /// The `[node]` table of a shipped locale catalog, read straight from the
    /// asset file so the test needs no global i18n state.
    fn node_catalog(locale: &str) -> toml::Table {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/locales/");
        let text =
            std::fs::read_to_string(format!("{path}{locale}.toml")).expect("locale file not found");
        let catalog: toml::Table = text.parse().expect("locale file is invalid TOML");
        catalog
            .get("node")
            .and_then(toml::Value::as_table)
            .expect("locale file has no [node] tables")
            .clone()
    }

    fn catalog_entry(catalog: &toml::Table, type_key: &str, field: &str) -> Option<String> {
        catalog
            .get(type_key)
            .and_then(toml::Value::as_table)
            .and_then(|entry| entry.get(field))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    }

    /// Japanese search end to end: the shipped `ja.toml` strings feed the
    /// candidates, and a Japanese query finds them — through the label and
    /// through the description.
    #[test]
    fn japanese_locale_strings_are_searchable_with_japanese_queries() {
        let catalog = node_catalog("ja");
        let candidates = vec![
            SearchCandidate {
                type_key: "field.noise".into(),
                label: catalog_entry(&catalog, "field.noise", "label").expect("ja label"),
                description: catalog_entry(&catalog, "field.noise", "description"),
                category: NodeCategory::Field,
            },
            SearchCandidate {
                type_key: "blur".into(),
                label: catalog_entry(&catalog, "blur", "label").expect("ja label"),
                description: catalog_entry(&catalog, "blur", "description"),
                category: NodeCategory::Image,
            },
        ];

        // Querying with a Japanese phrase finds the node by its Japanese label…
        assert_eq!(
            filter_candidates(&candidates, "ノイズ", None, &[]),
            vec![0],
            "label match: {:?}",
            candidates[0].label
        );
        // …and by words that only appear in the Japanese description.
        assert_eq!(
            filter_candidates(&candidates, "ぼかす", None, &[]),
            vec![1],
            "description match: {:?}",
            candidates[1].description
        );
    }

    /// The wire-drop filter drops every template that has no port compatible
    /// with the dragged one — in both drag directions.
    #[test]
    fn the_type_filter_drops_unconnectable_candidates() {
        let registry = registry();
        let blur_id = NodeId::new(1);
        let blur = registry
            .create_node("blur", blur_id)
            .expect("blur template");
        let graph = Graph::new().add_node(blur).unwrap();
        let candidates = search_candidates(&registry);

        // Dragging from blur's FRAME_BUFFER output: `blur` itself takes that
        // input; `constant` has no inputs at all and must disappear.
        let from_output = PortHit {
            node_id: blur_id,
            is_output: true,
            port_index: 0,
            center: (0.0, 0.0),
        };
        let filtered = retain_connectable(candidates.clone(), &registry, &graph, &from_output);
        let filtered_keys = keys(&filtered);
        assert!(filtered_keys.contains(&"blur"));
        assert!(!filtered_keys.contains(&"constant"));
        assert!(
            filtered.len() < candidates.len(),
            "the filter must remove something"
        );

        // Dragging from blur's FRAME_BUFFER input: `media` outputs a frame
        // buffer; `constant` (scalar output) does not qualify.
        let from_input = PortHit {
            node_id: blur_id,
            is_output: false,
            port_index: 0,
            center: (0.0, 0.0),
        };
        let filtered = retain_connectable(candidates, &registry, &graph, &from_input);
        let filtered_keys = keys(&filtered);
        assert!(filtered_keys.contains(&"media"));
        assert!(!filtered_keys.contains(&"constant"));
    }

    /// The palette offers exactly the types the context menu offers (the
    /// custom-path exclusion applies to both).
    #[test]
    fn candidates_match_the_menu_model() {
        let mut registry = NodeRegistry::new();
        registry.register(NodeTemplate::new(
            "geometry.alpha",
            "Alpha",
            NodeCategory::Geometry,
        ));
        registry.register(NodeTemplate::new(
            "shape.custom_path",
            "Custom Path",
            NodeCategory::Geometry,
        ));
        let candidates = search_candidates(&registry);
        let candidate_keys = keys(&candidates);
        assert!(candidate_keys.contains(&"geometry.alpha"));
        assert!(!candidate_keys.contains(&"shape.custom_path"));
    }
}

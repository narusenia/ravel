// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A single-line expression field with syntax colouring.
//!
//! The editing engine is `gpui-base`'s [`EditorState`] — IME, selection, undo
//! and the caret come from it, exactly as for [`crate::input`]. What this module
//! adds is the colour: an [`InputHighlighter`] that runs
//! [`ravel_core::expression::highlight_tokens`] on the full text and maps each
//! [`TokenClass`] to a token colour ([`token_color`]).
//!
//! The frame is [`input_layers`]' (the same border, ring and disabled rules as
//! a text input), so the field sits among ordinary inputs without a seam.
//!
//! # What the spike fixed (see the expression editor plan, EXED-1)
//!
//! - There is no `auto_grow` on `EditorState`: the height is the frame's, one
//!   row for now.
//! - `submit_on_enter(true)`: Enter emits [`InputEvent::PressEnter`] and inserts
//!   no newline. Shift+Enter and paste still would, so the field catches those
//!   two (below) and keeps to one row.
//! - `InputHighlighter::update` sees the full text once per edit and `styles`
//!   is called many times per paint, so the tokens are computed in `update` and
//!   only sliced in `styles`.
//!
//! # Completion
//!
//! [`crate::expression_completion`] decides what is offered; the kit's menu
//! state carries it (a [`CompletionProvider`](gpui_base::input::CompletionProvider)
//! fills it on every edit) and `CompletionPopup` draws it under the field. The
//! keys are the kit's own bindings, not new ones: Up, Down, Enter and Escape
//! reach the menu through the kit's overlay-action hook, and Tab, Shift+Enter
//! and paste are caught in the capture phase on the field. Each of them lets the
//! action through unchanged when the menu is closed. The rows also take the
//! pointer: hovering moves the selection, and a click accepts the row without
//! taking the focus from the editor.
//!
//! # A palette switch needs one call
//!
//! The highlighter snapshots the token set in `update`, so after
//! [`crate::set_active_tokens`] the host calls [`refresh_expression_colours`]
//! for each state it owns.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Action, App, AppContext as _, Context, Empty, Entity, EntityInputHandler as _, HighlightStyle,
    Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Subscription, Window, anchored, deferred, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_base::input::{
    Diagnostic, DiagnosticColors, DiagnosticSeverity, EditorState, Enter, Escape, FoldRange,
    HighlightStyleResolver, IndentInline, InputEdit, InputEditorStyle, InputHighlighter,
    InputOverlayKind, MoveDown, MoveUp, Paste, Position, Rope,
};
use gpui_base::{InputBase, StyledExt as _};
use lsp_types::CompletionItem;
use ravel_core::expression::{Span, TokenClass, Vocabulary, highlight_tokens};

use crate::expression_completion::{ExpressionCompletions, word_at};
use crate::input::{frame_is_focused, input_layers};
use crate::theme::ActiveTokens as _;
use crate::tokens::{Colors, Density, mix};

/// The language name the editor state is configured with. The factory ignores
/// it; it only has to be the same string on both sides.
pub const EXPRESSION_LANGUAGE: &str = "expression";

/// How far the two derived syntax colours move toward `foreground`.
const SYNTAX_TINT: f32 = 0.4;

/// The colour a run of a given class is painted in.
///
/// Every colour is a token or a derivation of tokens ([`mix`]), so both
/// palettes get one and none is a literal. The `match` has no `_` arm: a new
/// [`TokenClass`] must be given a colour here to compile.
pub fn token_color(class: TokenClass, colors: &Colors) -> Hsla {
    match class {
        TokenClass::Whitespace | TokenClass::Variable => colors.foreground,
        TokenClass::Function => colors.primary,
        TokenClass::Constant => colors.info,
        TokenClass::Number => mix(colors.info, colors.foreground, SYNTAX_TINT),
        TokenClass::Attribute => mix(colors.primary, colors.foreground, SYNTAX_TINT),
        TokenClass::Operator | TokenClass::Paren | TokenClass::Punctuation => {
            colors.muted_foreground
        }
        TokenClass::Error => colors.danger,
    }
}

/// Style runs that cover `range` with no gap and no overlap.
///
/// `tokens` is what [`highlight_tokens`] returned for the whole text; each is
/// clipped to `range`. A stretch no token covers (the text changed and `update`
/// has not run yet) gets the default style, so the contract the editor relies
/// on — a gapless cover — holds regardless.
pub fn style_runs(
    tokens: &[(Span, TokenClass)],
    range: &Range<usize>,
    colors: &Colors,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut runs = Vec::new();
    let mut at = range.start;
    for (span, class) in tokens {
        let start = (span.start as usize).max(at);
        let end = (span.end as usize).min(range.end);
        if start >= end {
            continue;
        }
        if start > at {
            runs.push((at..start, HighlightStyle::default()));
        }
        runs.push((
            start..end,
            HighlightStyle {
                color: Some(token_color(*class, colors)),
                ..HighlightStyle::default()
            },
        ));
        at = end;
    }
    if at < range.end {
        runs.push((at..range.end, HighlightStyle::default()));
    }
    runs
}

struct ExpressionHighlighter {
    tokens: Vec<(Span, TokenClass)>,
    colors: Colors,
}

impl InputHighlighter for ExpressionHighlighter {
    fn language(&self) -> SharedString {
        EXPRESSION_LANGUAGE.into()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        // The full text is rescanned rather than patched with `_edit`: the scan
        // is microseconds for an expression, and it keeps the tokens correct
        // after a paste or a replace that an incremental patch would have to
        // special-case.
        self.tokens = highlight_tokens(&text.to_string());
        self.colors = cx.tokens().colors;
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        style_runs(&self.tokens, range, &self.colors)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

/// The colours an error underline and its siblings are drawn in. The kit leaves
/// them transparent unless the host projects some, so without this the
/// underline would be there and invisible.
pub fn diagnostic_colors(colors: &Colors) -> DiagnosticColors {
    DiagnosticColors {
        error: colors.danger,
        warning: colors.danger,
        info: colors.info,
        hint: colors.muted_foreground,
    }
}

fn install_highlighter(state: &mut EditorState, cx: &mut Context<EditorState>) {
    state.set_editor_style(InputEditorStyle {
        diagnostics: diagnostic_colors(&cx.tokens().colors),
        ..InputEditorStyle::default()
    });
    state.set_highlighter_factory(
        Rc::new(|_| {
            Some(Box::new(ExpressionHighlighter {
                tokens: Vec::new(),
                colors: Colors::light(),
            }))
        }),
        cx,
    );
}

/// Build the state an [`ExpressionEditor`] paints, holding `value`.
///
/// One row, no line numbers, no folding, Enter submits. Read the text with
/// `state.read(cx).value()` and subscribe to [`crate::InputEvent`] for `Change`
/// and `PressEnter`, the same as for an [`crate::InputState`].
///
/// `vocabulary` is what completion offers: pass
/// `ravel_core::expression::vocabulary(&Scope::parameter_context())` for a
/// parameter expression and `Scope::field_context()` for a field expression.
pub fn expression_editor_state(
    value: impl Into<SharedString>,
    vocabulary: Vocabulary,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    let value = value.into();
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx)
            .line_number(false)
            .folding(false)
            // One row: no wrapping (a wrapped second row is clipped but
            // scrollable) and no empty scroll space below the last line,
            // which code-editor mode adds by default.
            .soft_wrap(false)
            .scroll_beyond_last_line(Some(0))
            .submit_on_enter(true)
            .default_value(value);
        install_highlighter(&mut state, cx);
        state.lsp_mut().completion_provider = Some(Rc::new(ExpressionCompletions { vocabulary }));
        state.set_overlay_action_handler(overlay_action);
        // The kit measures the typed query from the *first* trigger offset it
        // remembers and never resets it, so a menu opened after editing earlier
        // text than the first edit would be skipped. Remembering 0 makes the
        // query always start at the beginning, which the provider ignores.
        state.present_completion_items(0, "", Vec::new(), cx);
        state
    })
}

/// The left gutter the kit reserves in a code editor even with line numbers
/// and folding off (`LINE_NUMBER_RIGHT_MARGIN`, private there). The field pulls
/// the editor left by this much; `the_text_starts_at_the_frames_padding` fails
/// if the kit's value ever changes.
const KIT_GUTTER: gpui::Pixels = px(10.0);

/// How many rows the menu shows before it scrolls with the selection.
const MENU_ROWS: usize = 8;

/// Test lookup keys for the visible rows (`debug_bounds` takes a `&'static str`),
/// counted from the first row shown.
const ROW_SELECTORS: [&str; MENU_ROWS] = [
    "ravel-expression-completion-0",
    "ravel-expression-completion-1",
    "ravel-expression-completion-2",
    "ravel-expression-completion-3",
    "ravel-expression-completion-4",
    "ravel-expression-completion-5",
    "ravel-expression-completion-6",
    "ravel-expression-completion-7",
];

/// The one-line form of pasted text: line breaks become a single space, so the
/// field stays one row until multi-line expressions exist.
pub fn single_line(text: &str) -> String {
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Up, Down, Enter and Escape while the menu is open. The kit calls this from
/// inside an update of the state, so anything that touches the state is
/// deferred to just after it.
fn overlay_action(
    kind: InputOverlayKind,
    action: Box<dyn Action>,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> bool {
    if kind != InputOverlayKind::Completion {
        return false;
    }
    if action.partial_eq(&MoveUp) {
        cx.defer_in(window, |state, _, cx| move_selection(state, -1, cx));
    } else if action.partial_eq(&MoveDown) {
        cx.defer_in(window, |state, _, cx| move_selection(state, 1, cx));
    } else if Enter::is_primary(&*action) {
        // The kit closes the menu after this returns true; the items it held
        // are still there to read.
        cx.defer_in(window, |state, window, cx| {
            accept_selected(state, window, cx);
        });
    } else if !action.partial_eq(&Escape) {
        // Escape closes the menu and is consumed (the kit does the closing);
        // every other action is the editor's.
        return false;
    }
    true
}

/// Move the highlighted row by `delta`, wrapping at both ends.
///
/// The menu keeps no selection of its own, so it is carried on the items
/// (`preselect`) and the menu is presented again.
fn move_selection(state: &mut EditorState, delta: isize, cx: &mut Context<EditorState>) {
    let menu = state.completion_menu_state();
    let mut items = menu.items.clone();
    if items.is_empty() {
        return;
    }
    let (start, query) = (menu.trigger_start_offset.unwrap_or(0), menu.query.clone());
    let current = selected_index(&items);
    let next = (current as isize + delta).rem_euclid(items.len() as isize) as usize;
    select_row(&mut items, next);
    state.present_completion_items(start, query, items, cx);
}

/// Mark `index` as the one selected row. An index past the end selects nothing
/// new and leaves the rows as they were, so a stale hover cannot clear the
/// selection.
fn select_row(items: &mut [CompletionItem], index: usize) -> bool {
    if index >= items.len() {
        return false;
    }
    for (row, item) in items.iter_mut().enumerate() {
        item.preselect = Some(row == index);
    }
    true
}

/// Move the highlight to row `index` (the pointer is over it). Nothing is
/// presented again when it is already there, so hovering a row does not repaint
/// on every mouse move.
fn hover_row(state: &mut EditorState, index: usize, cx: &mut Context<EditorState>) {
    let menu = state.completion_menu_state();
    if !menu.open || selected_index(&menu.items) == index {
        return;
    }
    let mut items = menu.items.clone();
    let (start, query) = (menu.trigger_start_offset.unwrap_or(0), menu.query.clone());
    if select_row(&mut items, index) {
        state.present_completion_items(start, query, items, cx);
    }
}

/// Accept row `index`, the same way Tab and Enter accept the selected one.
fn click_row(
    state: &mut EditorState,
    index: usize,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> bool {
    let menu = state.completion_menu_state();
    let mut items = menu.items.clone();
    let (start, query) = (menu.trigger_start_offset.unwrap_or(0), menu.query.clone());
    if !select_row(&mut items, index) {
        return false;
    }
    state.present_completion_items(start, query, items, cx);
    accept_selected(state, window, cx)
}

fn selected_index(items: &[CompletionItem]) -> usize {
    items
        .iter()
        .position(|item| item.preselect == Some(true))
        .unwrap_or(0)
}

/// Replace the typed prefix with the highlighted candidate and close the menu.
/// One replacement, so one undo step. False when there was nothing to accept.
fn accept_selected(
    state: &mut EditorState,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> bool {
    let items = &state.completion_menu_state().items;
    let Some(item) = items.get(selected_index(items)).cloned() else {
        return false;
    };
    let text = state.value();
    let Some(word) = word_at(&text, state.cursor()) else {
        state.dismiss_completion_overlay(cx);
        return false;
    };
    state.insert_completion(&item, word.range, window, cx);
    state.dismiss_completion_overlay(cx);
    true
}

#[cfg(test)]
fn menu_is_open(state: &Entity<EditorState>, cx: &App) -> bool {
    state.read(cx).completion_menu_state().open
}

/// The menu, drawn under the field. A separate view because the kit notifies
/// the *state* when the menu changes, and nothing above this element would
/// repaint for that. It owns no focus handle: the editor keeps the keyboard.
struct CompletionPopup {
    state: Entity<EditorState>,
    /// The menu as last drawn (revision, open). The state notifies on every
    /// blink and scroll; repainting only when the menu changed keeps this view
    /// (and, through the keyed state, the host) from repainting forever.
    drawn: (u64, bool),
    /// Where the caret's bottom edge was when last drawn, to notice it moving.
    anchor: Option<gpui::Point<gpui::Pixels>>,
    _subscriptions: [Subscription; 2],
}

impl CompletionPopup {
    fn new(state: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let handle = state.read(cx).presentation().focus_handle().clone();
        Self {
            state: state.clone(),
            drawn: (0, false),
            anchor: None,
            _subscriptions: [
                cx.observe(state, |this, state, cx| {
                    let menu = state.read(cx).completion_menu_state();
                    let now = (menu.revision(), menu.open);
                    if now != this.drawn {
                        this.drawn = now;
                        cx.notify();
                    }
                }),
                // The kit ignores a blur while its menu is open (and emits no
                // `Blur`), so the menu would outlive the focus without this.
                cx.on_blur(&handle, window, |this, _, cx| {
                    this.state
                        .update(cx, |state, cx| state.dismiss_completion_overlay(cx));
                }),
            ],
        }
    }
}

impl Render for CompletionPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let menu = self.state.read(cx).completion_menu_state();
        if !menu.open || menu.items.is_empty() {
            return Empty.into_any_element();
        }
        let selected = selected_index(&menu.items);
        let handle = self.state.read(cx).presentation().focus_handle().clone();
        let first = (selected + 1).saturating_sub(MENU_ROWS);
        let colors = theme.colors;
        let rows = menu
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(MENU_ROWS)
            .map(|(index, item)| {
                let (hovered, clicked) = (self.state.clone(), self.state.clone());
                let handle = handle.clone();
                div()
                    .id(("ravel-expression-completion", index))
                    .debug_selector(move || ROW_SELECTORS[index - first].into())
                    .cursor_pointer()
                    .on_hover(move |over, _, cx| {
                        if *over {
                            hovered.update(cx, |state, cx| hover_row(state, index, cx));
                        }
                    })
                    // The row takes the click, so nothing under it does; the
                    // editor never loses focus, which is what keeps a click from
                    // committing a half-typed expression on blur.
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        window.focus(&handle, cx);
                        clicked.update(cx, |state, cx| click_row(state, index, window, cx));
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(theme.spacing.md)
                    .h(theme.rows.compact)
                    .px(theme.spacing.sm)
                    .when(index == selected, |row| row.bg(colors.hover_surface()))
                    .child(SharedString::from(item.label.clone()))
                    .child(
                        div()
                            .text_color(colors.muted_foreground)
                            .child(SharedString::from(item.detail.clone().unwrap_or_default())),
                    )
            });
        // Directly under the caret, in window coordinates. The caret is where
        // the editor last painted it, so a keystroke shows the old position for
        // one frame; asking for another frame when it moved settles it, and
        // costs nothing once it holds still. Before the first paint there is no
        // caret, and the list hangs from the field's bottom edge instead.
        let anchor = self
            .state
            .read(cx)
            .cursor_layout()
            .map(|(caret, _)| gpui::point(caret.origin.x, caret.bottom()));
        if anchor != self.anchor {
            self.anchor = anchor;
            window.request_animation_frame();
        }
        let list = div()
            .debug_selector(|| "ravel-expression-completions".into())
            .occlude()
            .mt(theme.spacing.xs)
            .min_w(px(160.0))
            .max_w(px(320.0))
            .py(theme.spacing.xs)
            .rounded(theme.radius.radius)
            .border_1()
            .border_color(colors.border)
            .bg(colors.raised_surface())
            .text_color(colors.foreground)
            .font_family(theme.text.mono_font_family.clone())
            .text_size(theme.text.mono_font_size)
            .children(rows);
        let anchored = anchored().snap_to_window_with_margin(px(8.0));
        match anchor {
            Some(position) => deferred(anchored.position(position).child(list)).into_any_element(),
            None => div()
                .absolute()
                .left_0()
                .top(relative(1.0))
                .child(deferred(anchored.child(list)))
                .into_any_element(),
        }
    }
}

/// Re-read the colours after the token set changed.
///
/// A palette switch changes no text, so nothing would re-run the highlighter's
/// `update` and the old colours would stay until the next keystroke. The host
/// that installs the tokens ([`crate::set_active_tokens`]) calls this for each
/// editor state it owns. It is an explicit call rather than a global observer
/// because `scripts/lint-patterns.sh` (`observe-global`) rules those out, and
/// it must not run from `render`.
pub fn refresh_expression_colours(state: &Entity<EditorState>, cx: &mut App) {
    state.update(cx, install_highlighter);
}

/// The bytes an error underline covers for `span` in `text`.
///
/// A span can be empty (an expression that ends too soon is reported *at* the
/// end), and an empty underline cannot be seen, so it widens to the character
/// after it, or the one before when it is at the end. `None` only for empty text.
pub fn underline_range(text: &str, span: Span) -> Option<Range<usize>> {
    let clamp = |at: u32| {
        let mut at = (at as usize).min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let (start, end) = (clamp(span.start), clamp(span.end));
    if start < end {
        return Some(start..end);
    }
    if let Some(next) = text[start..].chars().next() {
        return Some(start..start + next.len_utf8());
    }
    let previous = text[..start].chars().next_back()?;
    Some(start - previous.len_utf8()..start)
}

/// Where `range` sits as the kit's line / character positions.
///
/// Worked out with [`Span::line_column`] — the same rule the error message's
/// line and column use — so a multi-line expression underlines the character
/// the message names. `line_column` is 1-based, the kit's `Position` 0-based.
pub fn diagnostic_positions(text: &str, range: &Range<usize>) -> Range<Position> {
    let position = |offset: usize| {
        let (line, column) = Span::empty_at(offset as u32).line_column(text);
        Position::new(line - 1, column - 1)
    };
    position(range.start)..position(range.end)
}

/// Underline where an expression fails to compile, or clear the underline.
///
/// `error` is the span from the compile error and its message. The underline is
/// the danger colour and the message stays wherever the host shows it (under
/// the field); the editor clears the underline itself on the next edit, so the
/// host calls this again after each `Change` with the new compile result.
/// It writes the state, so call it from an event handler, never from `render`.
pub fn set_expression_error(
    state: &Entity<EditorState>,
    error: Option<(Span, &str)>,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        let text = state.text().clone();
        let source = text.to_string();
        let Some(diagnostics) = state.diagnostics_mut() else {
            return;
        };
        diagnostics.reset(&text);
        if let Some((span, message)) = error
            && let Some(range) = underline_range(&source, span)
        {
            let positions = diagnostic_positions(&source, &range);
            diagnostics.push(
                Diagnostic::new(positions, message.to_string())
                    .with_severity(DiagnosticSeverity::Error),
            );
        }
        cx.notify();
    });
}

/// A single-line expression field with syntax colouring.
///
/// The state is the caller's ([`expression_editor_state`]); this element only
/// decides appearance, with [`crate::Input`]'s frame and one density step.
#[derive(IntoElement)]
pub struct ExpressionEditor {
    state: Entity<EditorState>,
    density: Density,
    invalid: bool,
    accessibility_label: Option<SharedString>,
    tab_index: isize,
    style: StyleRefinement,
}

impl ExpressionEditor {
    /// Paint `state` on the default density step.
    pub fn new(state: &Entity<EditorState>) -> Self {
        Self {
            state: state.clone(),
            density: Density::default(),
            invalid: false,
            accessibility_label: None,
            tab_index: 0,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the field on the compact step.
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Show the danger border, as [`crate::Input::invalid`] does.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    /// The label exposed to accessibility clients.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Where the field sits in the Tab order.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }
}

impl Styled for ExpressionEditor {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ExpressionEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        let metrics = self.density.metrics(&theme);
        let presentation = self.state.read(cx).presentation();
        let disabled = presentation.is_disabled();
        let layers = input_layers(self.invalid, disabled, &theme.colors);
        let handle = presentation.focus_handle().clone();
        let (enter, _leave) = crate::button::feedback_motion(&theme.motion);
        let popup = window.use_keyed_state(
            ("ravel-expression-completions", self.state.entity_id()),
            cx,
            |window, cx| CompletionPopup::new(&self.state, window, cx),
        );
        // The keys below are the kit's own bindings (`tab`, `shift-enter`,
        // `cmd-v`) caught on the way in, before the editor's handler runs. No
        // binding is added, so nothing else that uses Tab is touched: when the
        // menu is closed these let the action through unchanged.
        let on_tab = self.state.clone();
        let on_paste = self.state.clone();

        InputBase::new(("ravel-expression-editor", self.state.entity_id()))
            .focused(frame_is_focused(&handle, disabled, window))
            .disabled(disabled)
            .track_focus(&handle)
            .tab_index(self.tab_index)
            .relative()
            .flex()
            .items_center()
            .h(metrics.height)
            .px(metrics.padding_x)
            .rounded(theme.radius.radius)
            .border_1()
            .font_family(theme.text.mono_font_family.clone())
            .text_size(metrics.font_size)
            .bg(layers.rest.surface)
            .text_color(layers.rest.foreground)
            .border_color(layers.rest.border)
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
            .when_some(self.accessibility_label, |editor, label| {
                editor.accessibility_label(label)
            })
            .capture_action(move |_: &IndentInline, window, cx| {
                if on_tab.update(cx, |state, cx| {
                    state.completion_menu_state().open && accept_selected(state, window, cx)
                }) {
                    cx.stop_propagation();
                }
            })
            .capture_action(move |action: &Enter, _, cx| {
                // The field is one row: Shift+Enter would add a second. With the
                // menu open the kit would also take it for an accept, so it is
                // swallowed either way.
                if action.shift {
                    cx.stop_propagation();
                }
            })
            .capture_action(move |_: &Paste, window, cx| {
                let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                    return;
                };
                if !text.contains(['\r', '\n']) {
                    return;
                }
                let text = single_line(&text);
                on_paste.update(cx, |state, cx| {
                    state.replace_text_in_range(None, &text, window, cx);
                });
                cx.stop_propagation();
            })
            .child(div().ml(-KIT_GUTTER).size_full().child(self.state.clone()))
            .child(popup)
            .refine_style(&self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSES: [TokenClass; 10] = [
        TokenClass::Whitespace,
        TokenClass::Number,
        TokenClass::Function,
        TokenClass::Constant,
        TokenClass::Variable,
        TokenClass::Attribute,
        TokenClass::Operator,
        TokenClass::Paren,
        TokenClass::Punctuation,
        TokenClass::Error,
    ];

    #[test]
    fn the_classes_that_read_differently_get_different_colours() {
        for colors in [Colors::light(), Colors::dark()] {
            let distinct = [
                TokenClass::Function,
                TokenClass::Constant,
                TokenClass::Number,
                TokenClass::Attribute,
                TokenClass::Variable,
                TokenClass::Operator,
                TokenClass::Error,
            ];
            for (i, a) in distinct.iter().enumerate() {
                for b in &distinct[i + 1..] {
                    assert_ne!(
                        token_color(*a, &colors),
                        token_color(*b, &colors),
                        "{a:?} and {b:?} paint alike"
                    );
                }
            }
        }
    }

    #[test]
    fn an_error_is_danger_and_plain_text_is_foreground() {
        for colors in [Colors::light(), Colors::dark()] {
            assert_eq!(token_color(TokenClass::Error, &colors), colors.danger);
            assert_eq!(
                token_color(TokenClass::Variable, &colors),
                colors.foreground
            );
            assert_eq!(
                token_color(TokenClass::Whitespace, &colors),
                colors.foreground
            );
        }
    }

    #[test]
    fn every_class_has_a_colour_in_both_palettes() {
        // The exhaustive `match` is what guarantees coverage; this pins that
        // none of them comes out transparent.
        for colors in [Colors::light(), Colors::dark()] {
            for class in CLASSES {
                assert!(
                    token_color(class, &colors).alpha > 0.0,
                    "{class:?} is invisible"
                );
            }
        }
    }

    fn assert_gapless(runs: &[(Range<usize>, HighlightStyle)], range: &Range<usize>) {
        let mut at = range.start;
        for (run, _) in runs {
            assert_eq!(run.start, at, "gap or overlap before {run:?} in {runs:?}");
            assert!(run.end > run.start, "empty run {run:?}");
            at = run.end;
        }
        assert_eq!(at, range.end, "runs stop short of {range:?}: {runs:?}");
    }

    #[test]
    fn style_runs_cover_any_range_without_gaps() {
        let colors = Colors::light();
        let source = "sin(@P.x * 2) + $ ?";
        let tokens = highlight_tokens(source);
        for start in 0..source.len() {
            for end in start..=source.len() {
                let range = start..end;
                assert_gapless(&style_runs(&tokens, &range, &colors), &range);
            }
        }
    }

    #[test]
    fn stale_tokens_leave_a_defaulted_tail_not_a_gap() {
        let colors = Colors::light();
        let tokens = highlight_tokens("1+2");
        let range = 0..8;
        let runs = style_runs(&tokens, &range, &colors);
        assert_gapless(&runs, &range);
        assert_eq!(runs.last().unwrap().1, HighlightStyle::default());
    }

    #[test]
    fn a_bad_character_is_coloured_as_an_error() {
        let colors = Colors::dark();
        let runs = style_runs(&highlight_tokens("1 $"), &(0..3), &colors);
        let (range, style) = runs.last().unwrap();
        assert_eq!(*range, 2..3);
        assert_eq!(style.color, Some(colors.danger));
    }

    use gpui::{ClipboardItem, TestAppContext, VisualTestContext};
    use gpui_base::input::InputEvent;
    use ravel_core::expression::{Scope, vocabulary};
    use std::cell::Cell;

    struct Host {
        editor: Entity<EditorState>,
        /// Whether an `Escape` reached an ancestor of the field.
        escaped: Rc<Cell<bool>>,
    }

    impl gpui::Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let escaped = self.escaped.clone();
            div()
                .on_action(move |_: &Escape, _, _| escaped.set(true))
                .child(ExpressionEditor::new(&self.editor).w(gpui::px(360.0)))
        }
    }

    fn scope() -> Vocabulary {
        vocabulary(&Scope::parameter_context())
    }

    thread_local! {
        static FIELD_SCOPE: Cell<bool> = const { Cell::new(false) };
    }

    /// A window holding an editor with `text`, focused, with the caret at the end.
    fn built_with<'a>(
        cx: &'a mut TestAppContext,
        text: &'static str,
    ) -> (
        &'a mut VisualTestContext,
        Entity<EditorState>,
        Rc<Cell<bool>>,
    ) {
        cx.update(gpui_base::init);
        let escaped = Rc::new(Cell::new(false));
        let flag = escaped.clone();
        let (view, cx) = cx.add_window_view(move |window, cx| Host {
            editor: expression_editor_state(
                text,
                if FIELD_SCOPE.get() {
                    vocabulary(&Scope::field_context())
                } else {
                    scope()
                },
                window,
                cx,
            ),
            escaped: flag,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = view.read_with(cx, |host, _| host.editor.clone());
        let handle = cx.update(|_, cx| state.read(cx).presentation().focus_handle().clone());
        cx.update(|window, cx| window.focus(&handle, cx));
        cx.simulate_keystrokes("end");
        (cx, state, escaped)
    }

    fn built(cx: &mut TestAppContext) -> (&mut VisualTestContext, Entity<EditorState>) {
        let (cx, state, _) = built_with(cx, "sin(2) $");
        (cx, state)
    }

    fn type_text(cx: &mut VisualTestContext, text: &str) {
        cx.simulate_input(text);
        cx.run_until_parked();
    }

    fn labels(cx: &mut VisualTestContext, state: &Entity<EditorState>) -> Vec<(String, bool)> {
        cx.update(|_, cx| {
            state
                .read(cx)
                .completion_menu_state()
                .items
                .iter()
                .map(|item| (item.label.clone(), item.preselect == Some(true)))
                .collect()
        })
    }

    fn menu_open(cx: &mut VisualTestContext, state: &Entity<EditorState>) -> bool {
        cx.update(|_, cx| menu_is_open(state, cx))
    }

    fn value(cx: &mut VisualTestContext, state: &Entity<EditorState>) -> String {
        cx.update(|_, cx| state.read(cx).value().to_string())
    }

    fn is_focused(cx: &mut VisualTestContext, state: &Entity<EditorState>) -> bool {
        cx.update(|window, cx| {
            state
                .read(cx)
                .presentation()
                .focus_handle()
                .is_focused(window)
        })
    }

    /// The widget builds, paints, and holds its value; its highlighter, driven
    /// the way the editor drives it (`update` with the full text), covers the
    /// whole text and paints the stray `$` as an error.
    #[gpui::test]
    fn the_built_editor_holds_its_value_and_colours_it(cx: &mut TestAppContext) {
        let (cx, state) = built(cx);
        assert_eq!(cx.update(|_, cx| state.read(cx).value()), "sin(2) $");

        let mut highlighter = ExpressionHighlighter {
            tokens: Vec::new(),
            colors: Colors::light(),
        };
        let text = Rope::from("sin(2) $");
        cx.update(|window, cx| {
            state.update(cx, |_, cx| {
                highlighter.update(None, &text, false, window, cx);
            });
        });
        let range = 0..text.len();
        let runs = highlighter.styles(&range, &NoStyles);
        assert_gapless(&runs, &range);
        let danger = cx.update(|_, cx| cx.tokens().colors.danger);
        assert_eq!(runs.last().unwrap().1.color, Some(danger));
    }

    /// The field is one row: the viewport must not be scrollable. Without
    /// `scroll_beyond_last_line(Some(0))` the code-editor mode reserves empty
    /// space below the last line and a scroll request sticks.
    #[gpui::test]
    fn the_one_row_field_cannot_be_scrolled(cx: &mut TestAppContext) {
        let (cx, state) = built(cx);
        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.set_scroll_offset(gpui::point(gpui::px(0.0), gpui::px(-60.0)), cx);
            });
            window.draw(cx).clear(cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let offset = cx.update(|_, cx| state.read(cx).scroll_offset());
        assert_eq!(
            offset.y,
            gpui::px(0.0),
            "the single row scrolled: {offset:?}"
        );
    }

    #[test]
    fn pasted_line_breaks_become_one_space() {
        assert_eq!(single_line("a +\nb"), "a + b");
        assert_eq!(single_line("a\r\n  b \r\n"), "a b");
        assert_eq!(single_line("\n\n"), "");
        assert_eq!(single_line("no break"), "no break");
    }

    #[gpui::test]
    fn typing_a_prefix_opens_the_menu_with_the_first_row_selected(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        assert!(!menu_open(cx, &state), "nothing typed, nothing offered");
        type_text(cx, "si");
        assert!(menu_open(cx, &state));
        assert_eq!(
            labels(cx, &state),
            [("sin".to_string(), true), ("sign".to_string(), false)]
        );
        // Typing on narrows it; a finished name with nothing longer closes it.
        type_text(cx, "g");
        assert_eq!(labels(cx, &state), [("sign".to_string(), true)]);
        assert!(menu_open(cx, &state));
        type_text(cx, "n");
        assert!(!menu_open(cx, &state));
        // A space ends the word.
        type_text(cx, " + fr");
        assert_eq!(
            labels(cx, &state)
                .iter()
                .map(|(l, _)| l.as_str())
                .collect::<Vec<_>>(),
            ["fract", "frame"]
        );
        type_text(cx, " ");
        assert!(!menu_open(cx, &state));
    }

    #[gpui::test]
    fn up_and_down_move_the_selection_and_wrap(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        type_text(cx, "si");
        let selected =
            |cx: &mut VisualTestContext| labels(cx, &state).iter().position(|(_, on)| *on).unwrap();
        assert_eq!(selected(cx), 0);
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(selected(cx), 1);
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(selected(cx), 0, "wraps past the last row");
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(selected(cx), 1, "wraps before the first row");
        assert!(menu_open(cx, &state));
        assert_eq!(
            value(cx, &state),
            "si",
            "moving the selection edits nothing"
        );
    }

    #[gpui::test]
    fn tab_accepts_in_one_undo_step_and_the_editor_keeps_focus(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "1 + ");
        type_text(cx, "si");
        cx.simulate_keystrokes("down tab");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "1 + sign");
        assert!(!menu_open(cx, &state));
        assert!(is_focused(cx, &state), "focus stays in the editor");

        cx.update(|window, cx| {
            window.dispatch_action(Box::new(gpui_base::input::Undo), cx);
        });
        assert_eq!(
            value(cx, &state),
            "1 + si",
            "one undo undoes the completion"
        );
    }

    #[gpui::test]
    fn enter_accepts_when_open_and_submits_when_closed(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        let presses = Rc::new(Cell::new(0));
        let counter = presses.clone();
        cx.update(|_, cx| {
            cx.subscribe(&state, move |_, event: &InputEvent, _| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    counter.set(counter.get() + 1);
                }
            })
            .detach();
        });
        type_text(cx, "smo");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "smoothstep");
        assert_eq!(presses.get(), 0, "the Enter that accepted did not submit");
        assert!(!menu_open(cx, &state));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            presses.get(),
            1,
            "with the menu closed Enter submits as before"
        );
    }

    #[gpui::test]
    fn escape_closes_the_menu_and_otherwise_passes_through(cx: &mut TestAppContext) {
        let (cx, state, escaped) = built_with(cx, "");
        type_text(cx, "si");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!menu_open(cx, &state));
        assert!(
            !escaped.get(),
            "the Escape that closed the menu was consumed"
        );
        assert_eq!(value(cx, &state), "si");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(
            escaped.get(),
            "with the menu closed Escape reaches the host"
        );
    }

    #[gpui::test]
    fn with_the_menu_closed_tab_and_the_arrows_are_the_editors_own(cx: &mut TestAppContext) {
        // The same keys on an editor the widget does not wrap.
        let bare = |cx: &mut TestAppContext| {
            struct Bare(Entity<EditorState>);
            impl gpui::Render for Bare {
                fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                    let handle = self.0.read(cx).presentation().focus_handle().clone();
                    InputBase::new("bare")
                        .track_focus(&handle)
                        .child(self.0.clone())
                }
            }
            cx.update(gpui_base::init);
            let (view, cx) = cx.add_window_view(|window, cx| {
                Bare(expression_editor_state("1 + 2", scope(), window, cx))
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let state = view.read_with(cx, |bare, _| bare.0.clone());
            let handle = cx.update(|_, cx| state.read(cx).presentation().focus_handle().clone());
            cx.update(|window, cx| window.focus(&handle, cx));
            cx.simulate_keystrokes("end tab left up down");
            cx.run_until_parked();
            let caret = cx.update(|_, cx| state.read(cx).selected_range());
            (value(cx, &state), caret)
        };
        let without_widget = bare(cx);
        let (cx, state, _) = built_with(cx, "1 + 2");
        cx.simulate_keystrokes("tab left up down");
        cx.run_until_parked();
        let caret = cx.update(|_, cx| state.read(cx).selected_range());
        assert_eq!((value(cx, &state), caret), without_widget);
    }

    #[gpui::test]
    fn shift_enter_adds_no_second_line_and_paste_flattens(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "1");
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "1");

        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("+\n2".into())));
        cx.simulate_keystrokes("space secondary-v");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "1 + 2", "the break became a space");

        // Text with no line break goes through the editor's own paste.
        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(" * 3".into())));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "1 + 2 * 3");
    }

    #[gpui::test]
    fn losing_focus_closes_the_menu(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        type_text(cx, "si");
        assert!(menu_open(cx, &state));
        // The test platform's window starts inactive, and an inactive window
        // reports no focus movement to listeners.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        // A frame must have painted the focus before the blur is seen as one.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.blur(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        assert!(!menu_open(cx, &state));
    }

    #[test]
    fn selecting_a_row_marks_exactly_that_row_and_ignores_a_stale_index() {
        let item = |label: &str, on: bool| CompletionItem {
            label: label.into(),
            preselect: Some(on),
            ..CompletionItem::default()
        };
        let mut items = vec![item("a", true), item("b", false), item("c", false)];
        assert!(select_row(&mut items, 2));
        let on: Vec<_> = items.iter().map(|i| i.preselect).collect();
        assert_eq!(on, [Some(false), Some(false), Some(true)]);
        assert!(!select_row(&mut items, 3), "past the end selects nothing");
        assert_eq!(selected_index(&items), 2, "and leaves the selection alone");
    }

    fn row_centre(cx: &mut VisualTestContext, index: usize) -> gpui::Point<gpui::Pixels> {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds(ROW_SELECTORS[index])
            .expect("the row is drawn")
            .center()
    }

    #[gpui::test]
    fn hovering_a_row_moves_the_selection_without_editing(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        type_text(cx, "si");
        let second = row_centre(cx, 1);
        cx.simulate_mouse_move(second, None, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            labels(cx, &state),
            [("sin".to_string(), false), ("sign".to_string(), true)]
        );
        assert_eq!(value(cx, &state), "si");
        assert!(menu_open(cx, &state));
    }

    /// The click accepts through the path Tab uses (one undo step), and the
    /// editor is never blurred: a blur would commit a half-typed expression.
    #[gpui::test]
    fn clicking_a_row_accepts_it_and_the_editor_keeps_focus(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "1 + ");
        let blurs = Rc::new(Cell::new(0));
        let counter = blurs.clone();
        cx.update(|_, cx| {
            cx.subscribe(&state, move |_, event: &InputEvent, _| {
                if matches!(event, InputEvent::Blur) {
                    counter.set(counter.get() + 1);
                }
            })
            .detach();
        });
        type_text(cx, "si");
        let second = row_centre(cx, 1);
        cx.simulate_click(second, gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "1 + sign");
        assert!(!menu_open(cx, &state));
        assert!(is_focused(cx, &state), "focus stays in the editor");
        assert_eq!(blurs.get(), 0, "no blur, so no blur-commit");

        cx.update(|window, cx| {
            window.dispatch_action(Box::new(gpui_base::input::Undo), cx);
        });
        assert_eq!(value(cx, &state), "1 + si", "one undo undoes the click");
    }

    #[gpui::test]
    fn the_menu_sits_below_the_field_and_draws_only_while_open(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("ravel-expression-completions").is_none());
        type_text(cx, "si");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let height = cx.update(|_, cx| {
            let theme = cx.tokens().clone();
            Density::default().metrics(&theme).height
        });
        let menu = cx
            .debug_bounds("ravel-expression-completions")
            .expect("the open menu is drawn");
        assert!(
            menu.origin.y >= height,
            "{menu:?} overlaps a field {height:?} tall"
        );
        assert!(
            menu.origin.x < gpui::px(40.0),
            "{menu:?} is not at the field's left edge"
        );
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("ravel-expression-completions").is_none());
        let _ = state;
    }

    #[test]
    fn an_empty_span_widens_to_a_character_you_can_see() {
        let at = |text: &str, offset: u32| underline_range(text, Span::empty_at(offset));
        assert_eq!(at("1 +", 3), Some(2..3), "at the end: the last character");
        assert_eq!(at("1 + 2", 2), Some(2..3), "inside: the next character");
        assert_eq!(at("é+", 2), Some(2..3), "on a boundary of a wide character");
        assert_eq!(
            at("é", 1),
            Some(0..2),
            "inside a wide character: snapped back"
        );
        assert_eq!(at("", 0), None);
        assert_eq!(
            underline_range("sin(1 + $)", Span { start: 8, end: 9 }),
            Some(8..9)
        );
        assert_eq!(
            underline_range("ab", Span { start: 1, end: 99 }),
            Some(1..2),
            "a span past the end is clamped"
        );
    }

    #[test]
    fn positions_use_the_same_rule_as_the_error_message() {
        // `Span::line_column` is 1-based; the kit's `Position` is 0-based.
        let text = "1 +\n  $ * αβ";
        let dollar = text.find('$').unwrap();
        let positions = diagnostic_positions(text, &(dollar..dollar + 1));
        assert_eq!(positions.start, Position::new(1, 2));
        assert_eq!(positions.end, Position::new(1, 3));
        let (line, column) = Span::empty_at(dollar as u32).line_column(text);
        assert_eq!(
            (line - 1, column - 1),
            (positions.start.line, positions.start.character)
        );
        // Columns count characters, not bytes.
        let beta = text.find('β').unwrap();
        assert_eq!(
            diagnostic_positions(text, &(beta..beta + 2)).start,
            Position::new(1, 7)
        );
    }

    #[test]
    fn the_underline_is_the_danger_colour_in_both_palettes() {
        for colors in [Colors::light(), Colors::dark()] {
            let diagnostics = diagnostic_colors(&colors);
            assert_eq!(diagnostics.error, colors.danger);
            assert!(diagnostics.error.alpha > 0.0, "an invisible underline");
        }
    }

    fn underlined(cx: &mut VisualTestContext, state: &Entity<EditorState>) -> Vec<Range<usize>> {
        cx.update(|_, cx| {
            let state = state.read(cx);
            let len = state.text().len();
            state
                .diagnostics()
                .expect("a code-editor state has diagnostics")
                .range(0..len)
                .map(|entry| entry.range.clone())
                .collect()
        })
    }

    #[gpui::test]
    fn an_error_underlines_its_span_and_editing_or_clearing_removes_it(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "sin(1 + $)");
        assert!(underlined(cx, &state).is_empty());

        cx.update(|_, cx| {
            set_expression_error(&state, Some((Span { start: 8, end: 9 }, "bad")), cx);
        });
        assert_eq!(underlined(cx, &state), vec![Range { start: 8, end: 9 }]);
        let severity = cx.update(|_, cx| {
            state
                .read(cx)
                .diagnostics()
                .unwrap()
                .range(0..10)
                .next()
                .unwrap()
                .severity
        });
        assert_eq!(severity, DiagnosticSeverity::Error);

        // Fixing the text clears it (the host then sets the new result).
        cx.simulate_keystrokes("left backspace backspace");
        cx.run_until_parked();
        assert!(
            underlined(cx, &state).is_empty(),
            "stale underline after an edit"
        );

        cx.update(|_, cx| {
            set_expression_error(&state, Some((Span { start: 0, end: 3 }, "bad")), cx);
        });
        assert_eq!(underlined(cx, &state), vec![Range { start: 0, end: 3 }]);
        cx.update(|_, cx| set_expression_error(&state, None, cx));
        assert!(underlined(cx, &state).is_empty());
    }

    #[gpui::test]
    fn shift_enter_with_the_menu_open_does_nothing(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        type_text(cx, "si");
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        assert_eq!(value(cx, &state), "si", "it must not accept the row");
        assert!(menu_open(cx, &state), "nor close the menu");
    }

    /// The kit reserves a 10px gutter on the left of a code editor even with
    /// line numbers off; the field cancels it so the text starts at the frame's
    /// padding like a plain input's.
    #[gpui::test]
    fn the_text_starts_at_the_frames_padding(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "abc");
        cx.simulate_keystrokes("home");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let (padding, border) = cx.update(|_, cx| {
            let theme = cx.tokens().clone();
            (Density::default().metrics(&theme).padding_x, gpui::px(1.0))
        });
        let (caret, _) = cx
            .update(|_, cx| state.read(cx).cursor_layout())
            .expect("a painted caret");
        let gap = caret.origin.x - (padding + border);
        assert!(
            gap.abs() < gpui::px(2.5),
            "the text starts {gap:?} from the padding edge"
        );
    }

    #[gpui::test]
    fn an_at_opens_the_attribute_menu_wherever_a_token_can_start(cx: &mut TestAppContext) {
        FIELD_SCOPE.set(true);
        for (typed, expected_first) in [
            ("@", "P"),
            ("sin(@", "P"),
            ("1 + @", "P"),
            ("max(a, @P", ""),
        ] {
            let (cx, state, _) = built_with(cx, "");
            type_text(cx, typed);
            let found = labels(cx, &state);
            if expected_first.is_empty() {
                // `@P` is complete and nothing longer matches.
                assert!(found.is_empty(), "{typed:?}: {found:?}");
            } else {
                assert_eq!(
                    found.first().map(|(l, _)| l.as_str()),
                    Some(expected_first),
                    "{typed:?}"
                );
                assert!(menu_open(cx, &state), "{typed:?}");
            }
        }
        FIELD_SCOPE.set(false);
    }

    /// The list hangs from the caret, not from the field's left edge.
    #[gpui::test]
    fn the_menu_opens_directly_under_the_caret(cx: &mut TestAppContext) {
        let (cx, state, _) = built_with(cx, "");
        type_text(cx, "1 + 2 * si");
        for _ in 0..3 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        let (caret, _) = cx
            .update(|_, cx| state.read(cx).cursor_layout())
            .expect("a painted caret");
        let menu = cx
            .debug_bounds("ravel-expression-completions")
            .expect("the open menu is drawn");
        assert!(
            caret.origin.x > gpui::px(40.0),
            "the caret is well right of the field's edge"
        );
        assert!(
            (menu.origin.x - caret.origin.x).abs() < gpui::px(1.5),
            "menu at {:?}, caret at {:?}",
            menu.origin,
            caret.origin
        );
        assert!(
            menu.origin.y >= caret.bottom() && menu.origin.y < caret.bottom() + gpui::px(12.0),
            "menu at {:?}, caret bottom {:?}",
            menu.origin,
            caret.bottom()
        );
    }

    struct NoStyles;

    impl HighlightStyleResolver for NoStyles {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            None
        }
    }
}

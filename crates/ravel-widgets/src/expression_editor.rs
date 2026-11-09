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
//!   no newline. Shift+Enter and paste still can; intercepting them belongs to
//!   the completion unit, not here.
//! - `InputHighlighter::update` sees the full text once per edit and `styles`
//!   is called many times per paint, so the tokens are computed in `update` and
//!   only sliced in `styles`.
//!
//! # A palette switch needs one call
//!
//! The highlighter snapshots the token set in `update`, so after
//! [`crate::set_active_tokens`] the host calls [`refresh_expression_colours`]
//! for each state it owns.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Entity, HighlightStyle, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, SharedString, StyleRefinement, Styled, Window,
    prelude::FluentBuilder as _,
};
use gpui_base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope,
};
use gpui_base::{InputBase, StyledExt as _};
use ravel_core::expression::{Span, TokenClass, highlight_tokens};

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

fn install_highlighter(state: &mut EditorState, cx: &mut Context<EditorState>) {
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
pub fn expression_editor_state(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    let value = value.into();
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx)
            .line_number(false)
            .folding(false)
            .submit_on_enter(true)
            .default_value(value);
        install_highlighter(&mut state, cx);
        state
    })
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

        InputBase::new(("ravel-expression-editor", self.state.entity_id()))
            .focused(frame_is_focused(&handle, disabled, window))
            .disabled(disabled)
            .track_focus(&handle)
            .tab_index(self.tab_index)
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
            .child(self.state.clone())
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

    use gpui::{TestAppContext, VisualTestContext};

    struct Host(Entity<EditorState>);

    impl gpui::Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ExpressionEditor::new(&self.0)
        }
    }

    fn built(cx: &mut TestAppContext) -> (&mut VisualTestContext, Entity<EditorState>) {
        cx.update(gpui_base::init);
        let (view, cx) =
            cx.add_window_view(|window, cx| Host(expression_editor_state("sin(2) $", window, cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = view.read_with(cx, |host, _| host.0.clone());
        (cx, state)
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

    struct NoStyles;

    impl HighlightStyleResolver for NoStyles {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            None
        }
    }
}

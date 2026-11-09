// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the expression editor offers while the author types.
//!
//! [`complete`] is a pure function over a [`Vocabulary`], the text and the
//! caret, so the filtering and the order are tested without a window. The
//! kit-facing half ([`ExpressionCompletions`]) only wraps its answer in the
//! `lsp_types` items the editor's menu carries.

use std::ops::Range;

use gpui::{App, Result, Task, Window};
use gpui_base::input::{CompletionProvider, Rope};
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse};
use ravel_core::expression::{Arity, Vocabulary};
use ravel_i18n::t;

/// What a candidate names, which decides its detail text and its menu icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    /// A built-in function; the detail is its arity.
    Function,
    /// `pi`, `e`.
    Constant,
    /// A scope variable (`frame`, `P`, `res.width`).
    Variable,
    /// A declared `@attribute`, offered only straight after the `@`.
    Attribute,
}

/// One entry of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The text that replaces the typed prefix.
    pub name: String,
    pub kind: CandidateKind,
    /// Set for a function.
    pub arity: Option<Arity>,
}

/// The identifier being typed: the bytes before the caret that can only be the
/// start of a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// What the completion replaces: from the first identifier character to the
    /// caret. The `@` of an attribute reference is not part of it.
    pub range: Range<usize>,
    /// Whether the word follows an `@`.
    pub attribute: bool,
}

fn is_word_char(byte: u8) -> bool {
    // `.` is in because scope variables are dotted (`res.width`); it cannot
    // start a word, so `x+.5` and `1.5` are not mistaken for one.
    byte == b'_' || byte == b'.' || byte.is_ascii_alphanumeric()
}

/// The identifier ending at `caret`, if the caret is inside one.
///
/// `None` for a caret in the middle of a character, past the end, after a
/// number (`2e`, `1.5`) or after anything that is not a name.
pub fn word_at(text: &str, caret: usize) -> Option<Word> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut start = caret;
    while start > 0 && is_word_char(bytes[start - 1]) {
        start -= 1;
    }
    let attribute = start > 0 && bytes[start - 1] == b'@';
    // An empty word is only a prompt after `@`; elsewhere nothing is typed yet.
    if start == caret {
        return attribute.then_some(Word {
            range: start..caret,
            attribute,
        });
    }
    let first = bytes[start];
    (first == b'_' || first.is_ascii_alphabetic()).then_some(Word {
        range: start..caret,
        attribute,
    })
}

/// The candidates for the word at `caret`, best first; `None` when there is
/// nothing to offer.
///
/// After `@` only attributes are offered; anywhere else functions, constants
/// and variables, in that order. Matching is a case-insensitive prefix match.
/// An exact (case-sensitive) match leads, then the rest keep the vocabulary's
/// order. When the word is already complete and nothing longer matches, the
/// answer is `None`, so Enter on a finished name still confirms the expression
/// rather than closing a menu that offers nothing new.
pub fn complete(
    vocabulary: &Vocabulary,
    text: &str,
    caret: usize,
) -> Option<(Word, Vec<Candidate>)> {
    let word = word_at(text, caret)?;
    let prefix = &text[word.range.clone()];
    let lower = prefix.to_ascii_lowercase();
    let matches = |name: &str| name.to_ascii_lowercase().starts_with(&lower);

    let mut candidates: Vec<Candidate> = if word.attribute {
        vocabulary
            .attributes
            .iter()
            .filter(|attribute| matches(&attribute.name))
            .map(|attribute| Candidate {
                name: attribute.name.to_string(),
                kind: CandidateKind::Attribute,
                arity: None,
            })
            .collect()
    } else {
        let functions = vocabulary
            .functions
            .iter()
            .filter(|function| matches(function.name))
            .map(|function| Candidate {
                name: function.name.to_string(),
                kind: CandidateKind::Function,
                arity: Some(function.arity),
            });
        let constants = vocabulary
            .constants
            .iter()
            .filter(|name| matches(name))
            .map(|name| Candidate {
                name: (*name).to_string(),
                kind: CandidateKind::Constant,
                arity: None,
            });
        let variables = vocabulary
            .variables
            .iter()
            .filter(|name| matches(name))
            .map(|name| Candidate {
                name: name.to_string(),
                kind: CandidateKind::Variable,
                arity: None,
            });
        functions.chain(constants).chain(variables).collect()
    };
    // Stable: ties keep the order built above.
    candidates.sort_by_key(|candidate| candidate.name != prefix);

    if candidates.iter().all(|candidate| candidate.name == prefix) {
        return None;
    }
    Some((word, candidates))
}

fn detail(candidate: &Candidate) -> String {
    match (candidate.kind, candidate.arity) {
        (CandidateKind::Function, Some(arity)) => arity.describe().to_string(),
        (CandidateKind::Function, None) => t!("expression_editor.kind.function"),
        (CandidateKind::Constant, _) => t!("expression_editor.kind.constant"),
        (CandidateKind::Variable, _) => t!("expression_editor.kind.variable"),
        (CandidateKind::Attribute, _) => t!("expression_editor.kind.attribute"),
    }
}

/// The menu item for a candidate. `selected` marks the highlighted row: the
/// menu has no selection of its own, so it travels in `preselect`.
pub fn completion_item(candidate: &Candidate, selected: bool) -> CompletionItem {
    CompletionItem {
        label: candidate.name.clone(),
        kind: Some(match candidate.kind {
            CandidateKind::Function => CompletionItemKind::FUNCTION,
            CandidateKind::Constant => CompletionItemKind::CONSTANT,
            CandidateKind::Variable => CompletionItemKind::VARIABLE,
            CandidateKind::Attribute => CompletionItemKind::FIELD,
        }),
        detail: Some(detail(candidate)),
        preselect: Some(selected),
        ..CompletionItem::default()
    }
}

/// Feeds the editor's completion trigger from a [`Vocabulary`].
pub(crate) struct ExpressionCompletions {
    pub(crate) vocabulary: Vocabulary,
}

impl CompletionProvider for ExpressionCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let items = complete(&self.vocabulary, &text.to_string(), offset)
            .map(|(_, candidates)| {
                candidates
                    .iter()
                    .enumerate()
                    .map(|(index, candidate)| completion_item(candidate, index == 0))
                    .collect()
            })
            .unwrap_or_default();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// Every edit asks. The menu closes by itself when the answer is empty,
    /// which is how a backspace past the word, or a space, dismisses it.
    fn is_completion_trigger(&self, _offset: usize, _new_text: &str, _cx: &mut App) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::expression::{Scope, vocabulary};

    fn names(found: Option<(Word, Vec<Candidate>)>) -> Vec<String> {
        found
            .map(|(_, candidates)| candidates.into_iter().map(|c| c.name).collect())
            .unwrap_or_default()
    }

    fn parameter(text: &str) -> Option<(Word, Vec<Candidate>)> {
        complete(&vocabulary(&Scope::parameter_context()), text, text.len())
    }

    #[test]
    fn a_prefix_keeps_only_the_names_that_start_with_it() {
        assert_eq!(names(parameter("1 + si")), ["sin", "sign"]);
        assert_eq!(names(parameter("sm")), ["smoothstep"]);
        assert_eq!(names(parameter("zzz")), Vec::<String>::new());
    }

    #[test]
    fn functions_come_before_constants_before_variables() {
        // `fl` is `floor` (function); `fr` is `fract` then `frame`; `c` has
        // functions and the comp.* variables; `e` mixes a constant in.
        assert_eq!(names(parameter("fr")), ["fract", "frame"]);
        let (_, found) = parameter("c").expect("candidates");
        let first_variable = found
            .iter()
            .position(|c| c.kind == CandidateKind::Variable)
            .expect("comp.width");
        assert!(
            found[..first_variable]
                .iter()
                .all(|c| c.kind == CandidateKind::Function)
        );
    }

    #[test]
    fn an_exact_match_leads_the_longer_ones() {
        // `e` is the constant; `exp`, `elem`... start with it too.
        let (word, found) = parameter("2 * e").expect("candidates");
        assert_eq!(word.range, 4..5);
        assert_eq!(found[0].name, "e");
        assert!(found.len() > 1, "longer names follow: {found:?}");
        assert!(found[1..].iter().all(|c| c.name != "e"));
    }

    #[test]
    fn a_finished_name_with_nothing_longer_offers_nothing() {
        // Enter must still confirm the expression, not close an empty menu.
        assert_eq!(names(parameter("frame")), Vec::<String>::new());
    }

    #[test]
    fn matching_ignores_case_but_the_exact_match_does_not() {
        assert_eq!(names(parameter("FR")), ["fract", "frame"]);
        // `@P` typed in full leaves nothing longer to offer; `@p` is not yet
        // that name, so it is offered.
        let field = vocabulary(&Scope::field_context());
        assert_eq!(names(complete(&field, "@p", 2)), ["P"]);
        assert_eq!(names(complete(&field, "@P", 2)), Vec::<String>::new());
    }

    #[test]
    fn after_an_at_only_attributes_are_offered() {
        let field = vocabulary(&Scope::field_context());
        let (word, found) = complete(&field, "@", 1).expect("every attribute");
        assert!(word.attribute);
        assert_eq!(word.range, 1..1);
        assert!(found.iter().all(|c| c.kind == CandidateKind::Attribute));
        assert!(found.iter().any(|c| c.name == "P"));

        let (word, found) = complete(&field, "sin(@C", 6).expect("candidates");
        assert_eq!(word.range, 5..6);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Cd");
    }

    #[test]
    fn an_at_in_a_parameter_scope_offers_nothing() {
        assert_eq!(names(parameter("@")), Vec::<String>::new());
    }

    #[test]
    fn the_scope_decides_the_variables() {
        let parameter_scope = vocabulary(&Scope::parameter_context());
        let field_scope = vocabulary(&Scope::field_context());
        let has = |vocab: &Vocabulary, text: &str, name: &str| {
            names(complete(vocab, text, text.len())).contains(&name.to_string())
        };
        assert!(has(&parameter_scope, "fr", "frame"));
        assert!(has(&field_scope, "el", "elem.count"));
        assert!(!has(&parameter_scope, "el", "elem.count"));
    }

    #[test]
    fn a_dotted_variable_is_matched_whole() {
        let found = names(parameter("res.w"));
        assert_eq!(found, ["res.width"]);
    }

    #[test]
    fn numbers_and_operators_are_not_words() {
        for text in ["", "1", "2e", "1.5", "x + ", "(", "sin(1) "] {
            assert_eq!(word_at(text, text.len()), None, "{text:?}");
        }
        // A caret inside a character is refused, not sliced.
        assert_eq!(word_at("é", 1), None);
        assert_eq!(word_at("ab", 9), None);
    }

    #[test]
    fn the_word_stops_at_the_caret() {
        // Text after the caret is left alone: only the typed prefix matches.
        let vocab = vocabulary(&Scope::parameter_context());
        let (word, found) = complete(&vocab, "si + 1", 2).expect("candidates");
        assert_eq!(word.range, 0..2);
        assert!(found.iter().any(|c| c.name == "sin"));
    }

    #[test]
    fn a_function_carries_its_arity_and_the_item_its_detail() {
        let (_, found) = parameter("si").expect("sin and sign");
        let sin = found.iter().find(|c| c.name == "sin").expect("sin");
        assert_eq!(
            sin.arity.map(|a| a.describe().to_string()),
            Some("1 argument".into())
        );
        let item = completion_item(sin, true);
        assert_eq!(item.label, "sin");
        assert_eq!(item.detail.as_deref(), Some("1 argument"));
        assert_eq!(item.preselect, Some(true));
        assert_eq!(completion_item(sin, false).preselect, Some(false));
    }
}

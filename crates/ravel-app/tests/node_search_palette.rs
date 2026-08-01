// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shipped-catalog coverage for the node search palette
//! (`docs/implementation/node-discoverability-plan.md`, DISC-3).
//!
//! The lib unit tests run with an empty i18n store — initializing the
//! global store there would leak into every other test of that binary.
//! These tests load the real catalogs and assert the full chain: the
//! palette's candidates carry locale-resolved strings, and a Japanese query
//! matches them (REQ: 日本語ロケールで日本語の語句で検索できる).

use ravel_app::node_editor::palette::search_candidates;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_ui::node_search::filter_candidates;
use std::sync::Mutex;

/// The i18n store is process-global and these tests switch the active
/// locale; serialize them the same way `ravel-i18n`'s own tests do, so a
/// parallel test never observes another test's locale.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    ravel_i18n::init(&dir, "en").expect("the shipped locale catalogs load");
}

fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    registry
}

/// With the ja catalog active, the candidate labels/descriptions are the
/// resolved Japanese strings and a Japanese query finds nodes through both.
#[test]
fn japanese_queries_match_locale_resolved_candidates() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    ravel_i18n::set_locale("ja").expect("ja catalog is shipped");

    let candidates = search_candidates(&registry());
    let noise = candidates
        .iter()
        .position(|c| c.type_key == "field.noise")
        .expect("field.noise is a builtin");
    let blur = candidates
        .iter()
        .position(|c| c.type_key == "blur")
        .expect("blur is a builtin");

    assert!(
        candidates[noise].label.contains("ノイズ"),
        "the candidate label is locale-resolved, not a key: {:?}",
        candidates[noise].label
    );

    // Querying with a Japanese phrase finds the node by its Japanese label…
    let matched = filter_candidates(&candidates, "ノイズ", None, &[]);
    assert!(
        matched.contains(&noise),
        "label search: {:?}",
        matched
            .iter()
            .map(|i| candidates[*i].type_key.as_str())
            .collect::<Vec<_>>()
    );
    // …and by words that only appear in the Japanese description.
    let matched = filter_candidates(&candidates, "ぼかす", None, &[]);
    assert!(
        matched.contains(&blur),
        "description search: {:?}",
        matched
            .iter()
            .map(|i| candidates[*i].type_key.as_str())
            .collect::<Vec<_>>()
    );
}

/// The same chain in the default locale: English queries keep working.
#[test]
fn english_queries_match_locale_resolved_candidates() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();

    let candidates = search_candidates(&registry());
    let noise = candidates
        .iter()
        .position(|c| c.type_key == "field.noise")
        .expect("field.noise is a builtin");

    let matched = filter_candidates(&candidates, "noise", None, &[]);
    assert_eq!(matched.first(), Some(&noise));
}

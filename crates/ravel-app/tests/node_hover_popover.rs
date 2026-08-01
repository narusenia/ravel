// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Locale-resolving coverage for the node hover popover and its Properties
//! counterpart (`docs/implementation/node-discoverability-plan.md`, DISC-2).
//!
//! The lib unit tests run with an empty i18n store — initializing the
//! global store there would leak into every other test of that binary
//! (e.g. `driven_params` label assertions depend on the fallback). These
//! tests load the real catalogs and assert the positive direction: a type
//! with a locale entry shows its description, parameter docs, and
//! localized port type names.

use ravel_app::node_editor::hover_popover::{data_type_name, hover_info};
use ravel_app::panels::properties::append_node_description;
use ravel_core::id::{DataTypeId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_ui::properties::PropertyField;
use ravel_ui::properties::node::sections_for_node;
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

/// A type with a locale entry shows its description and per-parameter docs
/// in the popover content model (en catalog: `blur`).
#[test]
fn hover_info_includes_the_description_when_the_locale_defines_one() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    let registry = registry();
    let node = registry
        .create_node("blur", NodeId::new(1))
        .expect("blur is registered");
    let info = hover_info(&node, &registry, 0);
    assert!(
        info.description.as_deref().is_some_and(|d| !d.is_empty()),
        "blur has a description in the en catalog"
    );
    let radius = info
        .params
        .iter()
        .find(|p| p.key == "radius")
        .expect("radius");
    assert!(radius.description.is_some(), "radius has a param doc");
}

/// Port type names resolve through the locale catalogs in both shipped
/// locales.
#[test]
fn port_type_names_are_localized() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    assert_eq!(data_type_name(DataTypeId::FRAME_BUFFER), "Frame Buffer");
    assert_eq!(data_type_name(DataTypeId::GEOMETRY), "Geometry");
    ravel_i18n::set_locale("ja").expect("ja catalog is shipped");
    assert_eq!(data_type_name(DataTypeId::FRAME_BUFFER), "フレームバッファ");
}

/// The Properties Node Info section carries the type's description — the
/// keyboard-reachable counterpart of the pointer-only hover popover — as
/// resolved text, not a raw key.
#[test]
fn node_info_section_carries_the_description_when_the_locale_defines_one() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    let registry = registry();
    let node = registry
        .create_node("blur", NodeId::new(1))
        .expect("blur is registered");
    let mut sections = sections_for_node(&node, &registry, 0, &[]);
    append_node_description(&mut sections, &node.type_key);

    let description = sections[0].fields.iter().find_map(|field| match field {
        PropertyField::ReadOnly { key, value } if key == "description" => Some(value),
        _ => None,
    });
    let description = description.expect("the info section gains a description field");
    assert!(!description.is_empty());
    assert!(
        !description.starts_with("node."),
        "the field shows resolved text, not a key: {description}"
    );
}

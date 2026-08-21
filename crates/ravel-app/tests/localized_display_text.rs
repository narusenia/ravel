// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Locale coverage for the display strings the headless crates emit as keys.
//!
//! `ravel-ui` has no i18n dependency, so a Properties row that names a state
//! ("Null") or swallows a number ("300 frames"), and a Timeline channel row
//! named by a word ("Value"), travel as locale keys and are resolved by the
//! host. Two things have to hold and neither is visible from one crate alone:
//! the *stored* value must not depend on the active locale, and the
//! *displayed* value must be translated rather than showing a raw key.
//!
//! The lib unit tests run with an empty i18n store — initializing the global
//! store there would leak into every other test of that binary — so this
//! coverage lives in its own binary with the real catalogs loaded.

use ravel_app::panels::properties::read_only_value;
use ravel_app::panels::timeline::channel_name_label;
use ravel_app::panels::viewer::resolution_label;
use ravel_core::composition::{Composition, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Graph, Node};
use ravel_core::id::{CompId, DataTypeId, LayerId, NodeId};
use ravel_core::network::{NET_OUT_TYPE_KEY, PORT_FRAME};
use ravel_core::types::FrameRate;
use ravel_ui::keyframes::{CHANNEL_VALUE, property_rows};
use ravel_ui::panels::viewer::ViewerResolution;
use ravel_ui::properties::layer::{
    DURATION_FRAMES, SOURCE_AUDIO, SOURCE_NETWORK, SOURCE_NULL, sections_for_layer,
};
use ravel_ui::properties::{PropertyField, split_counted_value};
use std::sync::Mutex;

/// The i18n store is process-global and these tests switch the active locale;
/// serialize them so a parallel test never observes another test's locale.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    ravel_i18n::init(&dir, "en").expect("the shipped locale catalogs load");
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
}

fn comp() -> Composition {
    Composition::new(
        CompId::new(1),
        "Comp",
        (1920, 1080),
        FrameRate::new(30, 1),
        300,
    )
}

/// A layer with a frame output, visible from frame 0 to 300.
fn network_layer() -> Layer {
    let out = Node::new(NodeId::new(1), NET_OUT_TYPE_KEY)
        .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
    let network = Graph::new().add_node(out).expect("out node");
    Layer::new(LayerId::new(1), "Test Layer", network).with_time(0, 0, 300)
}

fn read_only(layer: &Layer, section: &str, key: &str) -> String {
    let sections = sections_for_layer(layer, &comp(), &ctx(), None);
    let section = sections
        .iter()
        .find(|s| s.title == section)
        .unwrap_or_else(|| panic!("{section} missing"));
    match section.fields.iter().find(|f| f.key() == key) {
        Some(PropertyField::ReadOnly { value, .. }) => value.clone(),
        _ => panic!("{key} is not a read-only row"),
    }
}

/// What the section carries is the same string in every locale: the source
/// kind and the duration are stored as locale keys, so switching language
/// cannot change a comparison, a persisted value, or an edit.
#[test]
fn property_row_values_do_not_depend_on_the_locale() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    let layer = network_layer();

    let mut seen = Vec::new();
    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");
        seen.push((
            read_only(&layer, "properties.section.layer", "source"),
            read_only(&layer, "properties.section.timing", "duration"),
        ));
    }
    assert_eq!(seen[0], seen[1], "the stored rows followed the locale");
    assert_eq!(
        split_counted_value(&seen[0].0),
        Some((SOURCE_NETWORK, "1")),
        "the source row is a locale key plus its node count"
    );
    assert_eq!(
        split_counted_value(&seen[0].1),
        Some((DURATION_FRAMES, "300")),
        "the duration row is a locale key plus its frame count"
    );
}

/// The displayed text is translated and carries the count, in both shipped
/// locales — a missing key would surface `properties.value.*` in the panel.
#[test]
fn property_rows_display_translated_text_with_their_count() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    let layer = network_layer();
    let source = read_only(&layer, "properties.section.layer", "source");
    let duration = read_only(&layer, "properties.section.timing", "duration");

    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");
        for (value, count) in [(&source, "1"), (&duration, "300")] {
            let shown = read_only_value(value);
            assert!(
                shown.contains(count),
                "{locale}: the count is missing from {shown}"
            );
            assert!(
                !shown.contains("{count}") && !shown.starts_with("properties."),
                "{locale}: unresolved phrase {shown}"
            );
        }
    }

    ravel_i18n::set_locale("en").expect("en catalog is shipped");
    assert_eq!(read_only_value(&source), "Network (1 nodes)");
    assert_eq!(read_only_value(&duration), "300 frames");
    assert_eq!(read_only_value(SOURCE_NULL), "Null");
    assert_eq!(read_only_value(SOURCE_AUDIO), "Audio");
    ravel_i18n::set_locale("ja").expect("ja catalog is shipped");
    assert_ne!(
        read_only_value(&duration),
        "300 frames",
        "the Japanese catalog is not reaching the display boundary"
    );
}

/// Channel rows: a word is a key and translates, an axis letter is notation
/// and passes through unchanged in every locale
/// (`docs/specifications/ui/timeline.md`).
#[test]
fn channel_names_translate_words_and_keep_axis_letters() {
    let _lock = TEST_LOCK.lock().unwrap();
    init_i18n();
    let rows = property_rows(&network_layer());
    let position = rows
        .iter()
        .find(|row| row.channel_names.len() == 2)
        .expect("the Position group has two components");

    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");
        assert_eq!(
            position.channel_names,
            ["X", "Y"],
            "{locale}: axis letters are not stored per locale"
        );
        assert_eq!(channel_name_label("X"), "X", "{locale}: X was translated");
        assert_eq!(channel_name_label("Y"), "Y", "{locale}: Y was translated");
        for channel in ["R", "G", "B", "A"] {
            assert_eq!(
                channel_name_label(channel),
                channel,
                "{locale}: {channel} was translated"
            );
        }
        let value = channel_name_label(CHANNEL_VALUE);
        assert_ne!(value, CHANNEL_VALUE, "{locale}: the raw key would be shown");
        assert!(!value.is_empty());
    }

    ravel_i18n::set_locale("en").expect("en catalog is shipped");
    assert_eq!(channel_name_label(CHANNEL_VALUE), "Value");
    assert_eq!(channel_name_label("timeline.property.rotation"), "Rotation");
    ravel_i18n::set_locale("ja").expect("ja catalog is shipped");
    assert_eq!(channel_name_label(CHANNEL_VALUE), "値");
    assert_eq!(channel_name_label("timeline.property.rotation"), "回転");
}

/// Every reason a declaration can fail to reach its parameter has its own
/// translated sentence in **both** catalogs (REQ-PROJ-006, EXPO-5).
///
/// Nothing else enforces this: `ravel-ui` emits a locale key per
/// `BindingIssueReason` and the panel resolves it at the display boundary, so
/// a missing key shows the user `properties.exposed.issue.node_missing`
/// instead of a sentence. The ja side in particular is not mechanically
/// checked anywhere (`docs/dev/add-locale.md`).
#[test]
fn every_binding_issue_reason_reads_as_a_sentence_in_every_locale() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_i18n();

    let reasons = [
        ravel_core::exposed::apply::BindingIssueReason::NodeMissing,
        ravel_core::exposed::apply::BindingIssueReason::ParameterMissing,
        ravel_core::exposed::apply::BindingIssueReason::KindMismatch {
            declared: ravel_core::exposed::ExposedType::Float,
            parameter_kind: "string",
        },
        ravel_core::exposed::apply::BindingIssueReason::AnimatedComponents {
            components: vec![0],
        },
        ravel_core::exposed::apply::BindingIssueReason::NotAMediaNode {
            type_key: "transform".into(),
        },
        ravel_core::exposed::apply::BindingIssueReason::NotAnAssetReference {
            expected: "asset_id",
        },
    ];

    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");
        for reason in &reasons {
            let key = ravel_ui::properties::exposed::issue_key(reason);
            let text = ravel_i18n::translate(key);
            assert_ne!(
                text, key,
                "{locale}: {key} has no translation, so the user would see the key"
            );
            assert!(!text.is_empty(), "{locale}: {key} translates to nothing");
        }
    }
    ravel_i18n::set_locale("en").expect("en catalog is shipped");
}

/// The strings the declarations section itself shows: its title, the empty
/// state, the row buttons, the toggle tooltip, and every refusal.
#[test]
fn the_declarations_section_reads_as_sentences_in_every_locale() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_i18n();

    let keys = [
        ravel_ui::properties::exposed::SECTION_EXPOSED,
        "properties.toggle.exposed",
        "properties.toggle.exposed_remove",
        "properties.exposed.empty",
        "properties.exposed.description",
        "properties.exposed.remove",
        "properties.exposed.move_up",
        "properties.exposed.move_down",
        "properties.exposed.error.empty_name",
        "properties.exposed.error.duplicate",
        "properties.exposed.error.not_exposable",
        "properties.exposed.error.already_exposed",
        "properties.exposed.error.failed",
        ravel_ui::command::CommandId::ProjectExposedParameters.label_key(),
    ];

    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");
        for key in keys {
            let text = ravel_i18n::translate(key);
            assert_ne!(text, key, "{locale}: {key} has no translation");
            assert!(!text.is_empty(), "{locale}: {key} translates to nothing");
        }
    }
    ravel_i18n::set_locale("en").expect("en catalog is shipped");
}

/// The Viewer toolbar's preview resolution label (REQ-UI-004). Two things it
/// has to get right, and neither is visible from the lib tests (which run with
/// an empty i18n store, so every `t!` returns its key):
///
/// - the pair is a *translated* pattern with both factors substituted, not the
///   raw `viewer.resolution_effective` key and not one factor's name twice;
/// - it shows the pair only while the effective factor differs from the
///   selected one. Nothing can make them differ yet — `VRES-4`'s adaptive
///   downgrade will — so the divergence is constructed here.
#[test]
fn the_preview_resolution_label_distinguishes_selected_from_effective() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_i18n();

    for locale in ["en", "ja"] {
        ravel_i18n::set_locale(locale).expect("catalog is shipped");

        for factor in ViewerResolution::ALL {
            let name = ravel_i18n::translate(factor.label_key());
            assert_ne!(
                name,
                factor.label_key(),
                "{locale}: {factor:?} has no translation, so the toolbar shows the key"
            );
            // Agreeing factors read as one name: a permanent "1/2 → 1/2"
            // would train the user to ignore the one signal that matters.
            assert_eq!(resolution_label(factor, factor), name, "{locale}");
        }

        for selected in ViewerResolution::ALL {
            for effective in ViewerResolution::ALL {
                if selected == effective {
                    continue;
                }
                let label = resolution_label(selected, effective);
                assert!(
                    label.contains(&ravel_i18n::translate(selected.label_key())),
                    "{locale}: {label:?} dropped the selected factor {selected:?}"
                );
                assert!(
                    label.contains(&ravel_i18n::translate(effective.label_key())),
                    "{locale}: {label:?} dropped the effective factor {effective:?}"
                );
                assert!(
                    !label.contains('{'),
                    "{locale}: {label:?} left a placeholder unfilled"
                );
            }
        }
    }
    ravel_i18n::set_locale("en").expect("en catalog is shipped");
}

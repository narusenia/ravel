// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::composition::{Composition, Layer};
use crate::graph::Graph;
use crate::registry::builtin::register_builtins;
use crate::types::FrameRate;

fn registry() -> NodeRegistry {
    let mut reg = NodeRegistry::new();
    register_builtins(&mut reg);
    reg
}

fn comp_ref(id: CompId) -> CompRef {
    CompRef::Id(id)
}

fn alias(name: &str) -> Option<String> {
    Some(name.to_string())
}

fn add_comp(name: &str, as_: Option<String>) -> EditOp {
    EditOp::AddComposition {
        settings: CompositionSettings::fallback(name),
        alias: as_,
    }
}

fn add_layer(comp: CompRef, template: &str, as_: Option<String>) -> EditOp {
    EditOp::AddLayer {
        comp,
        template: template.to_string(),
        alias: as_,
    }
}

/// One composition holding one layer; `locked` sets the layer's lock.
fn doc_with_layer(locked: bool) -> (Document, CompId, LayerId) {
    let comp_id = CompId::next();
    let layer_id = LayerId::next();
    let mut layer = Layer::new(layer_id, "Layer", Graph::new()).with_time(0, 0, 300);
    layer.locked = locked;
    let comp =
        Composition::new(comp_id, "Comp", (16, 16), FrameRate::new(30, 1), 300).add_layer(layer);
    (
        Document::default().with_composition(comp),
        comp_id,
        layer_id,
    )
}

#[test]
fn add_composition_then_layer_through_an_alias() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let ops = [
        add_comp("Shot", alias("shot")),
        add_layer(CompRef::Alias("shot".into()), "solid", alias("bg")),
    ];
    let applied = apply_batch(&Document::default(), &ops, &cx).unwrap();
    applied.document.validate().unwrap();

    let Some(&Created::Composition(comp)) = applied.aliases.get("shot") else {
        panic!("alias `shot` must resolve to a composition");
    };
    let Some(&Created::Layer(layer)) = applied.aliases.get("bg") else {
        panic!("alias `bg` must resolve to a layer");
    };
    assert_eq!(
        applied.created,
        vec![Created::Composition(comp), Created::Layer(layer)]
    );
    let composition = applied.document.get_composition(comp).unwrap();
    assert_eq!(composition.name, "Shot");
    assert_eq!(composition.layer_count(), 1);
    assert!(composition.get_layer(layer).is_some());
    assert_eq!(applied.document.root_comp, Some(comp));
}

#[test]
fn remove_composition_and_layer_produce_the_expected_document() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let (doc, comp, layer) = doc_with_layer(false);

    let removed = apply(
        &doc,
        &EditOp::RemoveLayer {
            comp: comp_ref(comp),
            layers: vec![LayerRef::Id(layer)],
        },
        &cx,
    )
    .unwrap();
    removed.document.validate().unwrap();
    assert_eq!(
        removed
            .document
            .get_composition(comp)
            .unwrap()
            .layer_count(),
        0
    );

    let removed = apply(
        &doc,
        &EditOp::RemoveComposition {
            comp: comp_ref(comp),
        },
        &cx,
    )
    .unwrap();
    removed.document.validate().unwrap();
    assert!(removed.document.get_composition(comp).is_none());
}

/// Every refusal returns `Err` with the code it should, and the input document
/// is byte-for-byte what it was.
#[test]
fn refused_operations_leave_the_document_unchanged() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let (open, comp, layer) = doc_with_layer(false);
    let (locked, locked_comp, locked_layer) = doc_with_layer(true);
    let missing_comp = CompId::next();
    let missing_layer = LayerId::next();

    let cases: Vec<(&Document, EditOp, &str)> = vec![
        (
            &open,
            EditOp::RemoveComposition {
                comp: comp_ref(missing_comp),
            },
            "composition-not-found",
        ),
        (
            &open,
            add_layer(comp_ref(missing_comp), "solid", None),
            "composition-not-found",
        ),
        (
            &open,
            add_layer(comp_ref(comp), "no-such-template", None),
            "unknown-template",
        ),
        (
            &open,
            EditOp::RemoveLayer {
                comp: comp_ref(missing_comp),
                layers: vec![LayerRef::Id(layer)],
            },
            "composition-not-found",
        ),
        (
            &open,
            EditOp::RemoveLayer {
                comp: comp_ref(comp),
                layers: vec![LayerRef::Id(missing_layer)],
            },
            "layer-not-found",
        ),
        (
            &open,
            // The first layer is removable, the second is not: nothing may stick.
            EditOp::RemoveLayer {
                comp: comp_ref(comp),
                layers: vec![LayerRef::Id(layer), LayerRef::Id(missing_layer)],
            },
            "layer-not-found",
        ),
        (
            &locked,
            EditOp::RemoveLayer {
                comp: comp_ref(locked_comp),
                layers: vec![LayerRef::Id(locked_layer)],
            },
            "layer-locked",
        ),
        (
            &open,
            EditOp::RemoveLayer {
                comp: CompRef::Alias("nope".into()),
                layers: vec![],
            },
            "unknown-alias",
        ),
    ];
    assert!(!cases.is_empty());
    for (doc, op, code) in cases {
        let before = doc.clone();
        let err = apply(doc, &op, &cx).unwrap_err();
        assert_eq!(err.code(), code, "{op:?}");
        assert_eq!(*doc, before, "{op:?} changed the document");
    }
}

#[test]
fn a_failing_batch_applies_nothing_and_names_the_failing_index() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let (doc, comp, _) = doc_with_layer(false);
    let before = doc.clone();
    let ops = [
        add_layer(comp_ref(comp), "solid", None),
        add_layer(comp_ref(comp), "solid", None),
        add_layer(comp_ref(comp), "no-such-template", None),
        add_layer(comp_ref(comp), "solid", None),
    ];
    let err = apply_batch(&doc, &ops, &cx).unwrap_err();
    assert_eq!(err.index, 2);
    assert_eq!(err.error.code(), "unknown-template");
    assert_eq!(doc, before);
}

#[test]
fn duplicate_and_unknown_aliases_fail_the_whole_batch() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let (doc, _, _) = doc_with_layer(false);
    let before = doc.clone();

    let duplicate = [add_comp("A", alias("x")), add_comp("B", alias("x"))];
    let err = apply_batch(&doc, &duplicate, &cx).unwrap_err();
    assert_eq!((err.index, err.error.code()), (1, "duplicate-alias"));
    assert_eq!(doc, before);

    // An alias is only visible to the operations after the one defining it.
    let unknown = [
        add_layer(CompRef::Alias("later".into()), "solid", None),
        add_comp("A", alias("later")),
    ];
    let err = apply_batch(&doc, &unknown, &cx).unwrap_err();
    assert_eq!((err.index, err.error.code()), (0, "unknown-alias"));
    assert_eq!(doc, before);

    // A layer alias is not a composition.
    let mismatch = [
        add_comp("A", alias("c")),
        add_layer(CompRef::Alias("c".into()), "solid", alias("l")),
        add_layer(CompRef::Alias("l".into()), "solid", None),
    ];
    let err = apply_batch(&doc, &mismatch, &cx).unwrap_err();
    assert_eq!((err.index, err.error.code()), (2, "alias-kind-mismatch"));
    assert_eq!(doc, before);
}

#[test]
fn aliases_do_not_outlive_their_batch() {
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
    };
    let first = apply(&Document::default(), &add_comp("A", alias("a")), &cx).unwrap();
    let err = apply(
        &first.document,
        &add_layer(CompRef::Alias("a".into()), "solid", None),
        &cx,
    )
    .unwrap_err();
    assert_eq!(err.code(), "unknown-alias");
}

#[test]
fn edit_ops_round_trip_through_json() {
    let ops = vec![
        add_comp("Shot", alias("shot")),
        add_comp("Plain", None),
        add_layer(CompRef::Alias("shot".into()), "solid", alias("bg")),
        add_layer(CompRef::Id(CompId::new(7)), "null", None),
        EditOp::RemoveLayer {
            comp: CompRef::Id(CompId::new(7)),
            layers: vec![LayerRef::Id(LayerId::new(3)), LayerRef::Alias("bg".into())],
        },
        EditOp::RemoveComposition {
            comp: CompRef::Alias("shot".into()),
        },
    ];
    let json = serde_json::to_string(&ops).unwrap();
    // The wire form: a number is an id, a string an alias, `as` names a result.
    assert!(json.contains(r#""as":"shot""#), "{json}");
    assert!(json.contains(r#""comp":7"#), "{json}");
    assert!(json.contains(r#""comp":"shot""#), "{json}");
    let back: Vec<EditOp> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ops);
}

#[test]
fn only_removals_are_destructive() {
    assert!(!add_comp("A", None).is_destructive());
    assert!(!add_layer(CompRef::Id(CompId::new(1)), "solid", None).is_destructive());
    assert!(
        EditOp::RemoveComposition {
            comp: CompRef::Id(CompId::new(1))
        }
        .is_destructive()
    );
    assert!(
        EditOp::RemoveLayer {
            comp: CompRef::Id(CompId::new(1)),
            layers: vec![],
        }
        .is_destructive()
    );
}

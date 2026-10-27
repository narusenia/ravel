// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A document built only from `ravel_core::edit` operations survives
//! `ProjectFile::save` -> `load` and still validates. `ravel-core` cannot test
//! this itself (it does not depend on `ravel-project`).

use ravel_core::composition::Document;
use ravel_core::edit::{
    CompRef, CompositionSettings, Created, EditContext, EditOp, Endpoint, NetworkPath, NodeRef,
    ParamInput, PortRef, apply_batch,
};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_project::ProjectFile;
use std::collections::BTreeMap;

const LAYERS: usize = 3;

fn endpoint(node: &str, port: u32) -> Endpoint {
    Endpoint {
        node: NodeRef::Alias(node.into()),
        port: PortRef::Index(port),
    }
}

#[test]
fn a_document_built_from_edit_ops_saves_loads_and_validates() {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let cx = EditContext {
        registry: &registry,
        local_frame: None,
        allow_driven_overwrite: false,
    };

    // A path names a layer by id, so the layers must exist before a batch can
    // address their networks: the structure is one batch, the nodes another.
    let mut structure = vec![EditOp::AddComposition {
        settings: CompositionSettings::fallback("Edited"),
        alias: Some("comp".into()),
    }];
    for i in 0..LAYERS {
        structure.push(EditOp::AddLayer {
            comp: CompRef::Alias("comp".into()),
            template: "null".into(),
            alias: Some(format!("layer{i}")),
        });
    }
    let built = apply_batch(&Document::default(), &structure, &cx).unwrap();
    let Created::Composition(comp) = built.aliases["comp"] else {
        panic!("`comp` is not a composition");
    };

    let mut ops = Vec::new();
    for i in 0..LAYERS {
        let Created::Layer(layer) = built.aliases[&format!("layer{i}")] else {
            panic!("`layer{i}` is not a layer");
        };
        let path = NetworkPath::layer(comp, layer);
        let node = |key: &str, name: &str, params: BTreeMap<String, ParamInput>| EditOp::AddNode {
            path: path.clone(),
            type_key: key.into(),
            position: None,
            params,
            alias: Some(format!("{name}{i}")),
        };
        ops.push(node(
            "shape.rect",
            "rect",
            BTreeMap::from([("width".into(), ParamInput::Float(10.0 + i as f32))]),
        ));
        ops.push(node("geometry.transform", "xf", BTreeMap::new()));
        ops.push(node(
            "style.stroke",
            "stroke",
            BTreeMap::from([("width".into(), ParamInput::Float(2.0))]),
        ));
        ops.push(EditOp::Connect {
            path: path.clone(),
            from: endpoint(&format!("rect{i}"), 0),
            to: endpoint(&format!("xf{i}"), 0),
        });
    }
    assert_eq!(ops.len(), LAYERS * 4);
    let applied = apply_batch(&built.document, &ops, &cx).unwrap();
    applied.document.validate().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edited.ravprj");
    ProjectFile::from_document("Edited", "2026-10-27T00:00:00Z", applied.document.clone())
        .save(&path)
        .unwrap();
    let loaded = ProjectFile::load(&path).unwrap();

    loaded.document.validate().unwrap();
    // Document: PartialEq, and a document with no media assets has nothing for
    // `with_resolved_assets` to rewrite, so equality is the right bar.
    assert_eq!(loaded.document, applied.document);
    let composition = loaded.document.get_composition(comp).unwrap();
    assert_eq!(composition.layer_count(), LAYERS);
    // The edits survived, not just an empty shell. A `null` layer network is
    // its boundary nodes only, so compare against that baseline.
    let baseline = built
        .document
        .get_composition(comp)
        .unwrap()
        .layers
        .iter()
        .map(|l| l.network.node_count())
        .collect::<Vec<_>>();
    for (layer, base) in composition.layers.iter().zip(baseline) {
        assert_eq!(layer.network.node_count(), base + 3);
        assert_eq!(layer.network.edge_count(), 1);
    }
}

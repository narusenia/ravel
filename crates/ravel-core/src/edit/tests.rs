// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::composition::{Composition, Layer};
use crate::graph::Graph;
use crate::id::{EdgeId, NodeId};
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
        EditOp::AddNode {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)).entered(NodeId::new(11)),
            type_key: "geometry.transform".into(),
            position: Some((10.0, -4.5)),
            alias: alias("xf"),
        },
        EditOp::AddNode {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)),
            type_key: "constant".into(),
            position: None,
            alias: None,
        },
        EditOp::Connect {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)),
            from: Endpoint {
                node: NodeRef::Alias("xf".into()),
                port: PortRef::Index(0),
            },
            to: Endpoint {
                node: NodeRef::Id(NodeId::new(12)),
                port: PortRef::Name("geometry".into()),
            },
        },
        EditOp::Disconnect {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)),
            edge: EdgeId::new(5),
        },
        EditOp::RemoveNode {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)),
            node: NodeRef::Alias("xf".into()),
        },
    ];
    let json = serde_json::to_string(&ops).unwrap();
    // The wire form: a number is an id, a string an alias, `as` names a result.
    assert!(json.contains(r#""as":"shot""#), "{json}");
    assert!(json.contains(r#""comp":7"#), "{json}");
    assert!(json.contains(r#""comp":"shot""#), "{json}");
    assert!(json.contains(r#""port":"geometry""#), "{json}");
    assert!(json.contains(r#""port":0"#), "{json}");
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

// ----- network operations -----------------------------------------------------

fn cx(reg: &NodeRegistry) -> EditContext<'_> {
    EditContext {
        registry: reg,
        local_frame: None,
    }
}

fn add_node(path: &NetworkPath, type_key: &str, as_: &str) -> EditOp {
    EditOp::AddNode {
        path: path.clone(),
        type_key: type_key.to_string(),
        position: None,
        alias: alias(as_),
    }
}

fn node_ref(name: &str) -> NodeRef {
    NodeRef::Alias(name.to_string())
}

fn endpoint(name: &str, port: PortRef) -> Endpoint {
    Endpoint {
        node: node_ref(name),
        port,
    }
}

fn connect(path: &NetworkPath, from: (&str, PortRef), to: (&str, PortRef)) -> EditOp {
    EditOp::Connect {
        path: path.clone(),
        from: endpoint(from.0, from.1),
        to: endpoint(to.0, to.1),
    }
}

/// An empty layer network and the path that addresses it.
fn empty_network() -> (Document, NetworkPath) {
    let (doc, comp, layer) = doc_with_layer(false);
    (doc, NetworkPath::layer(comp, layer))
}

fn network_of<'a>(doc: &'a Document, path: &NetworkPath) -> &'a Graph {
    document::resolve_network(doc, path).expect("network")
}

fn created_node(applied: &Applied, name: &str) -> NodeId {
    match applied.aliases.get(name) {
        Some(Created::Node(id)) => *id,
        other => panic!("alias {name} is {other:?}, not a node"),
    }
}

fn variadic_slots(graph: &Graph, node: NodeId) -> Vec<bool> {
    let node = graph.node(node).unwrap();
    node.inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.is_variadic)
        .map(|(index, _)| {
            graph
                .edges()
                .any(|e| e.target == node.id && e.target_port.0 as usize == index)
        })
        .collect()
}

#[test]
fn bad_connections_are_refused_and_leave_the_document_alone() {
    let reg = registry();
    let (doc, path) = empty_network();
    let setup = apply_batch(
        &doc,
        &[
            add_node(&path, "shape.rect", "rect"),
            add_node(&path, "constant", "value"),
            add_node(&path, "geometry.transform", "xf"),
        ],
        &cx(&reg),
    )
    .unwrap();
    let doc = setup.document.clone();
    // The alias table is per batch, so later calls name the nodes by id.
    let at = |name: &str, port: PortRef| Endpoint {
        node: NodeRef::Id(created_node(&setup, name)),
        port,
    };
    let wire = |from: Endpoint, to: Endpoint| EditOp::Connect {
        path: path.clone(),
        from,
        to,
    };
    let ghost = Endpoint {
        node: NodeRef::Id(NodeId::new(u64::MAX - 1)),
        port: PortRef::Index(0),
    };
    let cases = [
        // SCALAR output into a geometry-only input.
        (
            wire(at("value", PortRef::Index(0)), at("xf", PortRef::Index(0))),
            "port-type-mismatch",
        ),
        // Output and input numbers past the end, and a name nobody has.
        (
            wire(at("rect", PortRef::Index(9)), at("xf", PortRef::Index(0))),
            "port-not-found",
        ),
        (
            wire(at("rect", PortRef::Index(0)), at("xf", PortRef::Index(9))),
            "port-not-found",
        ),
        (
            wire(
                at("rect", PortRef::Name("nope".into())),
                at("xf", PortRef::Index(0)),
            ),
            "port-not-found",
        ),
        // A node that is not there.
        (wire(ghost, at("xf", PortRef::Index(0))), "node-not-found"),
        // A node connected to itself is a cycle.
        (
            wire(at("xf", PortRef::Index(0)), at("xf", PortRef::Index(0))),
            "graph",
        ),
    ];
    for (op, code) in cases {
        let before = doc.clone();
        let err = apply(&doc, &op, &cx(&reg)).unwrap_err();
        assert_eq!(err.code(), code, "{err}");
        assert_eq!(doc, before, "{code}: the caller's document is unchanged");
    }
}

#[test]
fn a_cycle_through_two_nodes_is_refused() {
    let reg = registry();
    let (doc, path) = empty_network();
    let ops = [
        add_node(&path, "geometry.transform", "a"),
        add_node(&path, "geometry.transform", "b"),
        connect(
            &path,
            ("a", PortRef::Index(0)),
            ("b", PortRef::Name("geometry".into())),
        ),
        connect(
            &path,
            ("b", PortRef::Index(0)),
            ("a", PortRef::Name("geometry".into())),
        ),
    ];
    let err = apply_batch(&doc, &ops, &cx(&reg)).unwrap_err();
    assert_eq!(err.index, 3);
    assert_eq!(err.error.code(), "graph");
}

#[test]
fn an_alias_from_add_node_reaches_connect_in_the_same_batch() {
    let reg = registry();
    let (doc, path) = empty_network();
    let ops = [
        add_node(&path, "shape.rect", "rect"),
        add_node(&path, "geometry.transform", "xf"),
        connect(
            &path,
            ("rect", PortRef::Index(0)),
            ("xf", PortRef::Name("geometry".into())),
        ),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    let graph = network_of(&applied.document, &path);
    assert_eq!(graph.nodes().count(), 2);
    let edge = graph.edges().next().expect("one edge");
    assert_eq!(edge.source, created_node(&applied, "rect"));
    assert_eq!(edge.target, created_node(&applied, "xf"));
    assert!(matches!(applied.created.last(), Some(Created::Edge(id)) if *id == edge.id));
    // A node alias cannot stand where a layer is wanted.
    let err = apply_batch(
        &doc,
        &[
            add_node(&path, "shape.rect", "rect"),
            add_layer(CompRef::Alias("rect".into()), "solid", None),
        ],
        &cx(&reg),
    )
    .unwrap_err();
    assert_eq!(err.error.code(), "alias-kind-mismatch");
}

#[test]
fn variadic_inputs_keep_one_free_slot_and_compact_on_disconnect() {
    let reg = registry();
    let (doc, path) = empty_network();
    let ops = [
        add_node(&path, "shape.rect", "a"),
        add_node(&path, "shape.ellipse", "b"),
        add_node(&path, "scatter.grid", "scatter"),
        connect(
            &path,
            ("a", PortRef::Index(0)),
            ("scatter", PortRef::Index(0)),
        ),
        connect(
            &path,
            ("b", PortRef::Index(0)),
            ("scatter", PortRef::Index(1)),
        ),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    let scatter = created_node(&applied, "scatter");
    let graph = network_of(&applied.document, &path);
    assert_eq!(variadic_slots(graph, scatter), [true, true, false]);

    // Cutting the first source moves the second up and keeps one free slot.
    let first = graph
        .edges()
        .find(|e| e.source == created_node(&applied, "a"))
        .unwrap()
        .id;
    let cut = apply(
        &applied.document,
        &EditOp::Disconnect {
            path: path.clone(),
            edge: first,
        },
        &cx(&reg),
    )
    .unwrap();
    let graph = network_of(&cut.document, &path);
    assert_eq!(variadic_slots(graph, scatter), [true, false]);

    // Removing the remaining source's node leaves a single free slot.
    let gone = apply(
        &cut.document,
        &EditOp::RemoveNode {
            path: path.clone(),
            node: NodeRef::Id(created_node(&applied, "b")),
        },
        &cx(&reg),
    )
    .unwrap();
    let graph = network_of(&gone.document, &path);
    assert_eq!(variadic_slots(graph, scatter), [false]);
    assert!(
        apply(
            &gone.document,
            &EditOp::Disconnect {
                path: path.clone(),
                edge: first,
            },
            &cx(&reg),
        )
        .is_err_and(|e| e.code() == "edge-not-found")
    );
}

#[test]
fn connecting_to_an_occupied_input_replaces_the_edge() {
    let reg = registry();
    let (doc, path) = empty_network();
    let ops = [
        add_node(&path, "shape.rect", "a"),
        add_node(&path, "shape.ellipse", "b"),
        add_node(&path, "geometry.transform", "xf"),
        connect(&path, ("a", PortRef::Index(0)), ("xf", PortRef::Index(0))),
        connect(&path, ("b", PortRef::Index(0)), ("xf", PortRef::Index(0))),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    let graph = network_of(&applied.document, &path);
    let edges: Vec<_> = graph.edges().collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source, created_node(&applied, "b"));
}

#[test]
fn remove_node_refuses_the_boundary_and_missing_nodes() {
    let reg = registry();
    let (doc, comp, _) = doc_with_layer(false);
    // A template layer owns net.in / net.out.
    let applied = apply(
        &doc,
        &add_layer(CompRef::Id(comp), "solid", None),
        &cx(&reg),
    )
    .unwrap();
    let Created::Layer(layer) = applied.created[0] else {
        panic!("a layer was created");
    };
    let path = NetworkPath::layer(comp, layer);
    let out = network_of(&applied.document, &path)
        .nodes()
        .find(|n| crate::network::is_out_node(n))
        .unwrap()
        .id;
    let err = apply(
        &applied.document,
        &EditOp::RemoveNode {
            path: path.clone(),
            node: NodeRef::Id(out),
        },
        &cx(&reg),
    )
    .unwrap_err();
    assert_eq!(err.code(), "boundary-node");
    let err = apply(
        &applied.document,
        &EditOp::RemoveNode {
            path,
            node: NodeRef::Id(NodeId::new(u64::MAX - 1)),
        },
        &cx(&reg),
    )
    .unwrap_err();
    assert_eq!(err.code(), "node-not-found");
}

#[test]
fn an_unknown_type_key_and_a_missing_network_are_refused() {
    let reg = registry();
    let (doc, path) = empty_network();
    let err = apply(&doc, &add_node(&path, "no.such.node", "x"), &cx(&reg)).unwrap_err();
    assert_eq!(err.code(), "unknown-node-type");
    let missing_layer = NetworkPath::layer(path.comp, LayerId::new(u64::MAX - 1));
    let err = apply(&doc, &add_node(&missing_layer, "constant", "x"), &cx(&reg)).unwrap_err();
    assert_eq!(err.code(), "layer-not-found");
    let missing_subnet = path.entered(NodeId::new(u64::MAX - 1));
    let err = apply(&doc, &add_node(&missing_subnet, "constant", "x"), &cx(&reg)).unwrap_err();
    assert_eq!(err.code(), "network-not-found");
}

/// Every node id in `graph`, nested networks included.
fn all_node_ids(graph: &Graph, out: &mut Vec<NodeId>) {
    for node in graph.nodes() {
        out.push(node.id);
        if let Some(inner) = node.subnet.as_deref() {
            all_node_ids(inner, out);
        }
    }
}

#[test]
fn subnet_and_iterate_bring_inner_nodes_with_document_wide_unique_ids() {
    let reg = registry();
    let (doc, path) = empty_network();
    let ops = [
        add_node(&path, "subnet", "sub"),
        add_node(&path, "geometry.iterate", "it"),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    applied.document.validate().unwrap();
    let mut ids = Vec::new();
    all_node_ids(network_of(&applied.document, &path), &mut ids);
    assert!(ids.len() > 2, "inner net.in / net.out exist: {ids:?}");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn editing_inside_a_subnet_is_one_document_with_its_pins_in_step() {
    let reg = registry();
    let (doc, path) = empty_network();
    let applied = apply(&doc, &add_node(&path, "subnet", "sub"), &cx(&reg)).unwrap();
    let sub = created_node(&applied, "sub");
    // Give the subnet an input pin by declaring a custom port on its inner In.
    let inner_path = path.entered(sub);
    let inner = network_of(&applied.document, &inner_path).clone();
    let in_id = crate::network::find_in_node(&inner).unwrap().id;
    let inner = crate::network::add_custom_port(
        inner,
        in_id,
        "amount",
        crate::network::CustomPortType::Float,
        crate::network::NetworkContext::Subnet,
    )
    .unwrap();
    let doc = document::replace_network(&applied.document, &inner_path, inner).unwrap();
    let ops = [
        add_node(&inner_path, "shape.rect", "c"),
        add_node(&inner_path, "geometry.transform", "d"),
        connect(
            &inner_path,
            ("c", PortRef::Index(0)),
            ("d", PortRef::Index(0)),
        ),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    applied.document.validate().unwrap();
    let inner = network_of(&applied.document, &inner_path);
    assert!(inner.nodes().any(|n| n.id == created_node(&applied, "c")));
    // The enclosing subnet node still carries the pin the inner In declares.
    let sub_node = network_of(&applied.document, &path).node(sub).unwrap();
    assert!(
        sub_node.inputs.iter().any(|p| p.name == "amount"),
        "pin survives the inner edit: {:?}",
        sub_node.inputs
    );
    // And the original document was untouched.
    assert!(
        network_of(&doc, &inner_path)
            .nodes()
            .all(|n| n.type_key != "shape.rect")
    );
}

/// The showcase generator's `fill_body`: a `geometry.transform` between the
/// body's `net.in` and `net.out` of a `geometry.iterate`, as operations.
#[test]
fn fill_body_is_add_node_disconnect_connect() {
    let reg = registry();
    let (doc, path) = empty_network();
    let made = apply(&doc, &add_node(&path, "geometry.iterate", "it"), &cx(&reg)).unwrap();
    let iterate = created_node(&made, "it");
    let body = path.entered(iterate);
    let inner = network_of(&made.document, &body);
    let in_id = crate::network::find_in_node(inner).unwrap().id;
    let out_id = crate::network::find_out_node(inner).unwrap().id;
    let straight = inner
        .edges()
        .find(|e| e.source == in_id && e.target == out_id)
        .expect("the seeded pass-through wire")
        .id;

    let ops = [
        EditOp::Disconnect {
            path: body.clone(),
            edge: straight,
        },
        EditOp::AddNode {
            path: body.clone(),
            type_key: "geometry.transform".into(),
            position: Some((180.0, 0.0)),
            alias: alias("xf"),
        },
        EditOp::Connect {
            path: body.clone(),
            from: Endpoint {
                node: NodeRef::Id(in_id),
                port: PortRef::Index(0),
            },
            to: endpoint("xf", PortRef::Index(0)),
        },
        EditOp::Connect {
            path: body.clone(),
            from: endpoint("xf", PortRef::Index(0)),
            to: Endpoint {
                node: NodeRef::Id(out_id),
                port: PortRef::Index(0),
            },
        },
    ];
    let applied = apply_batch(&made.document, &ops, &cx(&reg)).unwrap();
    applied.document.validate().unwrap();
    let xf = created_node(&applied, "xf");
    let inner = network_of(&applied.document, &body);
    assert_eq!(inner.node(xf).unwrap().metadata.position, (180.0, 0.0));
    let wires: Vec<_> = inner.edges().map(|e| (e.source, e.target)).collect();
    assert_eq!(wires.len(), 2, "{wires:?}");
    assert!(wires.contains(&(in_id, xf)));
    assert!(wires.contains(&(xf, out_id)));
    // The pass-through wire is gone.
    assert!(!wires.contains(&(in_id, out_id)));
}

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
        allow_driven_overwrite: false,
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
        allow_driven_overwrite: false,
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
        allow_driven_overwrite: false,
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
        allow_driven_overwrite: false,
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
        allow_driven_overwrite: false,
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
        allow_driven_overwrite: false,
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

/// The Adjustment template makes an adjustment layer whose network passes
/// the lower stack (`source`) straight to Out's `frame`.
#[test]
fn adjustment_template_adds_a_pass_through_adjustment_layer() {
    use crate::network as net;
    let reg = registry();
    let cx = EditContext {
        registry: &reg,
        local_frame: None,
        allow_driven_overwrite: false,
    };
    let ops = [
        add_comp("Shot", alias("shot")),
        add_layer(CompRef::Alias("shot".into()), "adjustment", alias("adj")),
    ];
    let applied = apply_batch(&Document::default(), &ops, &cx).unwrap();
    applied.document.validate().unwrap();
    let (Some(&Created::Composition(comp)), Some(&Created::Layer(layer))) =
        (applied.aliases.get("shot"), applied.aliases.get("adj"))
    else {
        panic!("aliases must resolve");
    };
    let layer = applied
        .document
        .get_composition(comp)
        .unwrap()
        .get_layer(layer)
        .unwrap();
    assert!(layer.adjustment);
    assert!(layer.has_frame_output());

    let in_node = net::find_in_node(&layer.network).unwrap();
    let out_node = net::find_out_node(&layer.network).unwrap();
    let source = net::output_port_index(in_node, net::PORT_SOURCE).unwrap();
    let frame = net::frame_port_index(out_node).unwrap() as u32;
    let into_frame: Vec<_> = layer
        .network
        .edges()
        .filter(|e| e.target == out_node.id && e.target_port.0 == frame)
        .map(|e| (e.source, e.source_port))
        .collect();
    assert_eq!(into_frame, [(in_node.id, source)]);
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
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3))
                .entered(NodeId::new(11))
                .into(),
            type_key: "geometry.transform".into(),
            position: Some((10.0, -4.5)),
            params: Default::default(),
            alias: alias("xf"),
        },
        EditOp::AddNode {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)).into(),
            type_key: "constant".into(),
            position: None,
            params: Default::default(),
            alias: None,
        },
        EditOp::Connect {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)).into(),
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
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)).into(),
            edge: EdgeId::new(5),
        },
        EditOp::RemoveNode {
            path: NetworkPath::layer(CompId::new(7), LayerId::new(3)).into(),
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
        allow_driven_overwrite: false,
    }
}

fn add_node(path: &NetworkPath, type_key: &str, as_: &str) -> EditOp {
    EditOp::AddNode {
        path: path.clone().into(),
        type_key: type_key.to_string(),
        position: None,
        params: Default::default(),
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
        path: path.clone().into(),
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
        path: path.clone().into(),
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
            path: path.clone().into(),
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
            path: path.clone().into(),
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
                path: path.clone().into(),
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
            path: path.clone().into(),
            node: NodeRef::Id(out),
        },
        &cx(&reg),
    )
    .unwrap_err();
    assert_eq!(err.code(), "boundary-node");
    let err = apply(
        &applied.document,
        &EditOp::RemoveNode {
            path: path.into(),
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
            path: body.clone().into(),
            edge: straight,
        },
        EditOp::AddNode {
            path: body.clone().into(),
            type_key: "geometry.transform".into(),
            position: Some((180.0, 0.0)),
            params: Default::default(),
            alias: alias("xf"),
        },
        EditOp::Connect {
            path: body.clone().into(),
            from: Endpoint {
                node: NodeRef::Id(in_id),
                port: PortRef::Index(0),
            },
            to: endpoint("xf", PortRef::Index(0)),
        },
        EditOp::Connect {
            path: body.clone().into(),
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

// ----- parameter writes -------------------------------------------------------

use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::animation::curve::KeyframeCurve;
use crate::animation::interpolation::Interpolation;
use crate::graph::{Parameter, ParameterValue};

fn set_param(path: &NetworkPath, node: NodeRef, key: &str, value: ParamInput) -> EditOp {
    EditOp::SetParameter {
        path: path.clone().into(),
        node,
        key: key.to_string(),
        value,
    }
}

fn param_of(doc: &Document, path: &NetworkPath, node: NodeId, key: &str) -> ParameterValue {
    network_of(doc, path)
        .node(node)
        .unwrap()
        .parameters
        .iter()
        .find(|p| p.key == key)
        .unwrap_or_else(|| panic!("no parameter {key}"))
        .value
        .clone()
}

/// An empty network holding one node of `type_key`, aliased `n`.
fn doc_with_node(type_key: &str) -> (Document, NetworkPath, NodeId) {
    let reg = registry();
    let (doc, path) = empty_network();
    let applied = apply(&doc, &add_node(&path, type_key, "n"), &cx(&reg)).unwrap();
    let node = created_node(&applied, "n");
    (applied.document, path, node)
}

/// The same document with `key` of `node` replaced by `value`, bypassing the
/// edit layer: a way to put an animated parameter in place.
fn with_param_value(
    doc: &Document,
    path: &NetworkPath,
    node: NodeId,
    key: &str,
    value: ParameterValue,
) -> Document {
    let graph = network_of(doc, path)
        .clone()
        .set_params(
            node,
            &[Parameter {
                key: key.to_string(),
                value,
            }],
        )
        .unwrap();
    document::replace_network(doc, path, graph).unwrap()
}

fn keyed_channel() -> AnimationChannel {
    let mut curve = KeyframeCurve::new();
    curve.insert(0, 1.0, Interpolation::Linear);
    curve.insert(10, 5.0, Interpolation::Linear);
    AnimationChannel::keyframes(curve)
}

#[test]
fn param_input_round_trips_through_json() {
    use crate::param_curve::CurveParam;
    use crate::param_ramp::RampParam;
    use crate::types::Color;
    let inputs = vec![
        ParamInput::Float(1.0),
        ParamInput::Int(1),
        ParamInput::Bool(true),
        ParamInput::String("point".into()),
        ParamInput::Color {
            r: 0.25,
            g: 0.5,
            b: 0.75,
            a: 1.0,
        },
        ParamInput::Vector(vec![1.0, -2.0]),
        ParamInput::Curve(CurveParam::linear([(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)])),
        ParamInput::Ramp(RampParam::linear([
            (0.0, Color::WHITE),
            (0.5, Color::BLACK),
        ])),
    ];
    for input in &inputs {
        let json = serde_json::to_string(input).unwrap();
        let back: ParamInput = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, input, "{json}");
    }
    // An int and a float stay apart on the wire.
    assert_eq!(
        serde_json::to_string(&ParamInput::Int(1)).unwrap(),
        r#"{"type":"int","value":1}"#
    );
    let op = set_param(
        &NetworkPath::layer(CompId::new(1), LayerId::new(2)),
        NodeRef::Alias("n".into()),
        "width",
        ParamInput::Float(2.0),
    );
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<EditOp>(&json).unwrap(), op);
    assert!(!op.is_destructive());
}

#[test]
fn add_node_params_round_trip_and_apply() {
    let reg = registry();
    let (doc, path) = empty_network();
    let mut op = add_node(&path, "style.stroke", "s");
    let EditOp::AddNode { params, .. } = &mut op else {
        unreachable!()
    };
    params.insert("width".into(), ParamInput::Float(3.0));
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<EditOp>(&json).unwrap(), op);

    let applied = apply(&doc, &op, &cx(&reg)).unwrap();
    let node = created_node(&applied, "s");
    assert!(
        matches!(
            param_of(&applied.document, &path, node, "width"),
            ParameterValue::Float(3.0)
        ) || matches!(
            param_of(&applied.document, &path, node, "width"),
            ParameterValue::Channel(AnimationChannel { source: ChannelSource::Constant(v), .. }) if v == 3.0
        )
    );

    // An unknown key refuses the whole node: nothing is added.
    let EditOp::AddNode { params, .. } = &mut op else {
        unreachable!()
    };
    params.insert("no_such_key".into(), ParamInput::Float(1.0));
    let before = doc.clone();
    let err = apply(&doc, &op, &cx(&reg)).unwrap_err();
    assert_eq!(err.code(), "unknown-parameter");
    assert_eq!(doc, before);
}

#[test]
fn set_parameter_clamps_to_the_registry_range() {
    let reg = registry();
    let (doc, path, node) = doc_with_node("style.stroke");
    let range = reg.param_range("style.stroke", "width").unwrap().clone();
    assert!(*range.hard.end() < 1e6, "the test needs a bounded range");
    for (written, expected) in [
        (*range.hard.end() + 5000.0, *range.hard.end()),
        (*range.hard.start() - 3.0, *range.hard.start()),
    ] {
        let op = set_param(
            &path,
            NodeRef::Id(node),
            "width",
            ParamInput::Float(written),
        );
        let applied = apply(&doc, &op, &cx(&reg)).unwrap();
        match param_of(&applied.document, &path, node, "width") {
            ParameterValue::Float(v) => assert_eq!(v, expected),
            ParameterValue::Channel(AnimationChannel {
                source: ChannelSource::Constant(v),
                ..
            }) => assert_eq!(v, expected),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_refused_parameter_write_leaves_the_document_alone() {
    let reg = registry();
    let (doc, path, node) = doc_with_node("style.stroke");
    let animated = with_param_value(
        &doc,
        &path,
        node,
        "width",
        ParameterValue::Channel(keyed_channel()),
    );
    let missing = NodeId::next();
    let cases: Vec<(&str, Document, EditOp)> = vec![
        (
            "unknown-parameter",
            doc.clone(),
            set_param(&path, NodeRef::Id(node), "nope", ParamInput::Float(1.0)),
        ),
        (
            "parameter-type-mismatch",
            doc.clone(),
            set_param(&path, NodeRef::Id(node), "width", ParamInput::Bool(true)),
        ),
        (
            "parameter-type-mismatch",
            doc.clone(),
            set_param(
                &path,
                NodeRef::Id(node),
                "width",
                ParamInput::String("x".into()),
            ),
        ),
        (
            "node-not-found",
            doc.clone(),
            set_param(&path, NodeRef::Id(missing), "width", ParamInput::Float(1.0)),
        ),
        (
            "unknown-alias",
            doc.clone(),
            set_param(
                &path,
                NodeRef::Alias("ghost".into()),
                "width",
                ParamInput::Float(1.0),
            ),
        ),
        (
            "animated-parameter",
            animated.clone(),
            set_param(&path, NodeRef::Id(node), "width", ParamInput::Float(2.0)),
        ),
    ];
    assert!(!cases.is_empty());
    for (code, document, op) in &cases {
        let before = document.clone();
        let err = apply(document, op, &cx(&reg)).unwrap_err();
        assert_eq!(err.code(), *code, "{op:?}");
        assert_eq!(document, &before);
    }
}

#[test]
fn an_animated_parameter_gets_a_key_at_the_local_frame() {
    let reg = registry();
    let (doc, path, node) = doc_with_node("style.stroke");
    let animated = with_param_value(
        &doc,
        &path,
        node,
        "width",
        ParameterValue::Channel(keyed_channel()),
    );
    let at_frame = EditContext {
        registry: &reg,
        local_frame: Some(5),
        allow_driven_overwrite: false,
    };
    let op = set_param(&path, NodeRef::Id(node), "width", ParamInput::Float(9.0));
    let applied = apply(&animated, &op, &at_frame).unwrap();
    let ParameterValue::Channel(channel) = param_of(&applied.document, &path, node, "width") else {
        panic!("the parameter stays a channel");
    };
    let ChannelSource::Keyframes(curve) = channel.source else {
        panic!("the animation survives");
    };
    assert_eq!(curve.len(), 3, "a key was added at frame 5");
    assert_eq!(curve.sample(5.0), 9.0);
    assert_eq!(curve.sample(0.0), 1.0, "the existing keys are untouched");
    assert_eq!(curve.sample(10.0), 5.0, "the existing keys are untouched");
}

#[test]
fn attribute_set_type_reshapes_the_value() {
    let reg = registry();
    let (doc, path, node) = doc_with_node("attribute.set");
    let before_arity = param_of(&doc, &path, node, "value");
    assert!(matches!(before_arity, ParameterValue::Channel(_)));
    let op = set_param(
        &path,
        NodeRef::Id(node),
        "type",
        ParamInput::String("vec3".into()),
    );
    let applied = apply(&doc, &op, &cx(&reg)).unwrap();
    assert!(
        matches!(
            param_of(&applied.document, &path, node, "value"),
            ParameterValue::Channel3(_)
        ),
        "`value` follows `type` in the same document"
    );
}

#[test]
fn layer_ref_output_follows_the_port_it_names() {
    use crate::composition::Layer;
    use crate::id::DataTypeId;
    let reg = registry();
    // Layer A exposes a geometry port on its `net.out`; layer B will hold the
    // reference.
    let a = LayerId::new(900);
    let b = LayerId::new(901);
    let out = crate::graph::Node::new(NodeId::next(), crate::network::NET_OUT_TYPE_KEY)
        .with_input("geo", &[DataTypeId::GEOMETRY]);
    let comp_id = CompId::next();
    let comp = Composition::new(comp_id, "C", (16, 16), FrameRate::new(30, 1), 100)
        .add_layer(Layer::new(a, "A", Graph::new().add_node(out).unwrap()))
        .add_layer(Layer::new(b, "B", Graph::new()));
    let doc = Document::default().with_composition(comp);
    let path = NetworkPath::layer(comp_id, b);
    let ops = vec![
        add_node(&path, "layer.ref", "r"),
        set_param(
            &path,
            node_ref("r"),
            "layer",
            ParamInput::String("900".into()),
        ),
        set_param(
            &path,
            node_ref("r"),
            "port",
            ParamInput::String("geo".into()),
        ),
    ];
    let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
    let node = created_node(&applied, "r");
    let output = &network_of(&applied.document, &path)
        .node(node)
        .unwrap()
        .outputs[0];
    assert_eq!(output.data_type, DataTypeId::GEOMETRY);
}

/// `Document::validate()` runs after every operation, so its cost is the floor
/// of one `apply`. Not a regression test (timings are machine-bound): run it in
/// release mode to refresh the numbers in the plan's 検証 section.
///
/// `cargo test --release -p ravel-core --lib edit::tests::measure -- --ignored --nocapture`
#[test]
#[ignore = "timing measurement"]
fn measure_validate_and_apply_on_a_showcase_scale_document() {
    use std::time::Instant;
    let reg = registry();
    let cx = cx(&reg);
    // Showcase scale (6 compositions, 43 layers, about 6 nodes a layer): 42
    // layers here, then four times that.
    for (comps, layers_per_comp) in [(6usize, 7usize), (6, 28)] {
        let mut ops = Vec::new();
        for c in 0..comps {
            ops.push(add_comp(&format!("C{c}"), alias(&format!("c{c}"))));
            for l in 0..layers_per_comp {
                ops.push(add_layer(
                    CompRef::Alias(format!("c{c}")),
                    "null",
                    alias(&format!("l{c}_{l}")),
                ));
            }
        }
        let built = apply_batch(&Document::default(), &ops, &cx).unwrap();
        // Paths name layers by id; add four nodes and one edge to every layer.
        let mut fill = Vec::new();
        let mut layers = 0;
        for (name, made) in &built.aliases {
            let Created::Layer(layer) = made else {
                continue;
            };
            let comp = built
                .document
                .compositions
                .iter()
                .find(|(_, comp)| comp.get_layer(*layer).is_some())
                .map(|(id, _)| *id)
                .unwrap();
            let path = NetworkPath::layer(comp, *layer);
            fill.push(add_node(&path, "shape.rect", &format!("r{name}")));
            fill.push(add_node(&path, "geometry.transform", &format!("x{name}")));
            fill.push(add_node(&path, "style.stroke", &format!("s{name}")));
            fill.push(add_node(&path, "constant", &format!("k{name}")));
            fill.push(connect(
                &path,
                (&format!("r{name}"), PortRef::Index(0)),
                (&format!("x{name}"), PortRef::Index(0)),
            ));
            layers += 1;
        }
        let doc = apply_batch(&built.document, &fill, &cx).unwrap().document;
        let nodes: usize = doc
            .compositions
            .values()
            .flat_map(|c| c.layers.iter())
            .map(|l| l.network.node_count())
            .sum();

        const REPS: u32 = 200;
        let start = Instant::now();
        for _ in 0..REPS {
            doc.validate().unwrap();
        }
        let validate = start.elapsed() / REPS;

        let (comp, layer) = doc
            .compositions
            .iter()
            .find_map(|(id, c)| c.layers.front().map(|l| (*id, l.id)))
            .unwrap();
        let op = add_node(&NetworkPath::layer(comp, layer), "constant", "k");
        let start = Instant::now();
        for _ in 0..REPS {
            apply(&doc, &op, &cx).unwrap();
        }
        let apply_one = start.elapsed() / REPS;
        println!(
            "{comps} comps, {layers} layers, {nodes} nodes: validate {:?}/call, apply(AddNode) {:?}/op",
            validate, apply_one
        );
    }
}

#[test]
fn a_driven_parameter_refuses_a_write_unless_overwriting_is_allowed() {
    use crate::animation::channel::ParameterExpression;
    let reg = registry();
    let (doc, path, node) = doc_with_node("style.stroke");
    let driven = with_param_value(
        &doc,
        &path,
        node,
        "width",
        ParameterValue::Channel(AnimationChannel::new(ChannelSource::Expression(
            ParameterExpression::new("frame"),
        ))),
    );
    let op = set_param(&path, NodeRef::Id(node), "width", ParamInput::Float(2.0));
    let before = driven.clone();
    let err = apply(&driven, &op, &cx(&reg)).unwrap_err();
    assert_eq!(err.code(), "driven-parameter");
    assert_eq!(driven, before);
    // A blend holding an expression is driven too.
    let blended = with_param_value(
        &doc,
        &path,
        node,
        "width",
        ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Constant(1.0)),
            Box::new(ChannelSource::Expression(ParameterExpression::new("frame"))),
            crate::animation::blend::BlendMode::Mix,
            0.5,
        ))),
    );
    assert_eq!(
        apply(&blended, &op, &cx(&reg)).unwrap_err().code(),
        "driven-parameter"
    );
    // The UI scrub's explicit opt-in keeps collapsing it to a constant.
    let allowed = EditContext {
        allow_driven_overwrite: true,
        ..cx(&reg)
    };
    let applied = apply(&driven, &op, &allowed).unwrap();
    assert_eq!(
        param_of(&applied.document, &path, node, "width"),
        ParameterValue::Float(2.0)
    );
}

#[test]
fn constant_and_keyframed_parameters_still_take_a_write() {
    let reg = registry();
    let (doc, path, node) = doc_with_node("style.stroke");
    let op = set_param(&path, NodeRef::Id(node), "width", ParamInput::Float(3.0));
    let applied = apply(&doc, &op, &cx(&reg)).unwrap();
    assert_eq!(
        param_of(&applied.document, &path, node, "width"),
        ParameterValue::Float(3.0)
    );
    let keyed = with_param_value(
        &doc,
        &path,
        node,
        "width",
        ParameterValue::Channel(keyed_channel()),
    );
    let at_frame = EditContext {
        local_frame: Some(5),
        ..cx(&reg)
    };
    let applied = apply(&keyed, &op, &at_frame).unwrap();
    assert_ne!(applied.document, keyed, "the keyed write changed the curve");
}

#[test]
fn one_batch_builds_a_document_addressing_networks_by_alias() {
    let reg = registry();
    // As an agent writes it: every reference by alias, in JSON.
    let ops: Vec<EditOp> = serde_json::from_value(serde_json::json!([
        {"op": "add_composition", "settings": CompositionSettings::fallback("Shot"), "as": "c"},
        {"op": "add_layer", "comp": "c", "template": "solid", "as": "l"},
        {"op": "add_node", "path": {"comp": "c", "layer": "l"}, "type_key": "shape.rect", "as": "a"},
        {"op": "add_node", "path": {"comp": "c", "layer": "l", "subnets": []},
         "type_key": "geometry.transform", "as": "xf"},
        {"op": "connect", "path": {"comp": "c", "layer": "l"},
         "from": {"node": "a", "port": 0}, "to": {"node": "xf", "port": 0}},
        {"op": "set_parameter", "path": {"comp": "c", "layer": "l"}, "node": "a",
         "key": "width", "value": {"type": "float", "value": 7.0}},
    ]))
    .unwrap();
    let applied = apply_batch(&Document::default(), &ops, &cx(&reg)).unwrap();
    let (Created::Composition(comp), Created::Layer(layer)) =
        (applied.aliases["c"], applied.aliases["l"])
    else {
        panic!("aliases name a composition and a layer");
    };
    let path = NetworkPath::layer(comp, layer);
    let rect = created_node(&applied, "a");
    let xf = created_node(&applied, "xf");
    assert!(
        network_of(&applied.document, &path)
            .edges()
            .any(|e| e.source == rect && e.target == xf),
        "the alias-addressed connection exists"
    );
    assert_eq!(
        param_of(&applied.document, &path, rect, "width"),
        ParameterValue::Float(7.0)
    );
}

#[test]
fn an_unknown_alias_in_a_network_path_is_refused() {
    let reg = registry();
    let ops: Vec<EditOp> = serde_json::from_value(serde_json::json!([
        {"op": "add_node", "path": {"comp": "ghost", "layer": 1}, "type_key": "shape.rect"},
    ]))
    .unwrap();
    let err = apply_batch(&Document::default(), &ops, &cx(&reg)).unwrap_err();
    assert_eq!((err.index, err.error.code()), (0, "unknown-alias"));
}

#[test]
fn a_network_path_written_with_ids_still_reads() {
    let path = NetworkPath::layer(CompId::new(7), LayerId::new(3)).entered(NodeId::new(11));
    // The resolved type's own JSON is what older callers wrote.
    let json = serde_json::to_value(&path).unwrap();
    let read: NetworkRef = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(read, NetworkRef::from(&path));
    assert_eq!(read.comp, CompRef::Id(CompId::new(7)));
    // And `subnets` may be left out.
    let short: NetworkRef =
        serde_json::from_value(serde_json::json!({"comp": 7, "layer": 3})).unwrap();
    assert!(short.subnets.is_empty());
}

// ----- keyframes and the layer shell --------------------------------------------

mod animation {
    use super::*;
    use crate::animation::channel::ParameterExpression;
    use crate::composition::ShellProperty;
    use crate::eval::EvalContext;

    fn key(frame: u64, value: f32, interpolation: KeyInterpolation) -> KeyInput {
        KeyInput {
            frame,
            value,
            interpolation,
        }
    }

    fn linear(frame: u64, value: f32) -> KeyInput {
        key(frame, value, KeyInterpolation::Linear)
    }

    fn param_target(path: &NetworkPath, node: NodeId, key: &str) -> ChannelTarget {
        ChannelTarget::Parameter {
            path: path.clone().into(),
            node: NodeRef::Id(node),
            key: key.to_string(),
        }
    }

    fn shell_target(comp: CompId, layer: LayerId, property: ShellProperty) -> ChannelTarget {
        ChannelTarget::Shell {
            comp: CompRef::Id(comp),
            layer: LayerRef::Id(layer),
            property,
        }
    }

    fn set_keys(target: ChannelTarget, component: u32, keys: Vec<KeyInput>) -> EditOp {
        EditOp::SetKeyframes {
            target,
            component,
            keys,
        }
    }

    fn clear(target: ChannelTarget, component: u32, value: f32) -> EditOp {
        EditOp::ClearAnimation {
            target,
            component,
            value,
        }
    }

    /// A layer starting at composition frame 10 that holds one `style.stroke`.
    fn started_layer() -> (Document, NetworkPath, NodeId, CompId, LayerId) {
        let (doc, path, node) = doc_with_node("style.stroke");
        let (comp, layer) = (path.comp, path.layer);
        let doc = document::update_layer(&doc, comp, layer, |l| l.start_frame = 10).unwrap();
        (doc, path, node, comp, layer)
    }

    fn layer_of(doc: &Document, comp: CompId, layer: LayerId) -> Layer {
        doc.get_composition(comp)
            .unwrap()
            .get_layer(layer)
            .unwrap()
            .clone()
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (16, 16))
    }

    /// The channel's value at composition frame `comp_frame`, read the way
    /// evaluation reads it: at the layer-local frame.
    fn at(channel: &AnimationChannel, layer: &Layer, comp_frame: u64) -> f32 {
        channel.evaluate(layer.local_frame(comp_frame) as f64, &ctx())
    }

    #[test]
    fn set_keyframes_on_a_node_parameter_evaluates_to_the_keys_at_comp_frames() {
        let reg = registry();
        let (doc, path, node, comp, layer) = started_layer();
        let op = set_keys(
            param_target(&path, node, "width"),
            0,
            vec![linear(10, 2.0), linear(20, 8.0), linear(30, 4.0)],
        );
        let applied = apply(&doc, &op, &cx(&reg)).unwrap();
        let ParameterValue::Channel(channel) = param_of(&applied.document, &path, node, "width")
        else {
            panic!("a keyed Float becomes a channel");
        };
        let layer = layer_of(&applied.document, comp, layer);
        assert_eq!(at(&channel, &layer, 10), 2.0);
        assert_eq!(at(&channel, &layer, 20), 8.0);
        assert_eq!(at(&channel, &layer, 30), 4.0);
        assert_eq!(at(&channel, &layer, 15), 5.0, "linear between the keys");
        let ChannelSource::Keyframes(curve) = &channel.source else {
            panic!("keys");
        };
        let frames: Vec<u64> = curve.keyframes().iter().map(|k| k.frame).collect();
        assert_eq!(frames, [0, 10, 20], "stored layer-local, not comp frames");
    }

    #[test]
    fn set_keyframes_replaces_one_component_and_clamps_to_the_range() {
        let reg = registry();
        let (doc, path, node, comp, layer) = started_layer();
        let hard_end = *reg.param_range("style.stroke", "width").unwrap().hard.end();
        let ops = [
            set_keys(
                param_target(&path, node, "color"),
                3,
                vec![linear(10, 0.0), linear(20, 1.0)],
            ),
            set_keys(
                param_target(&path, node, "width"),
                0,
                vec![linear(10, hard_end + 1000.0)],
            ),
        ];
        let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
        let layer = layer_of(&applied.document, comp, layer);
        let ParameterValue::Channel4(chs) = param_of(&applied.document, &path, node, "color")
        else {
            panic!("colour");
        };
        assert_eq!(at(&chs[3], &layer, 10), 0.0);
        assert_eq!(at(&chs[3], &layer, 20), 1.0);
        assert!(
            matches!(chs[0].source, ChannelSource::Constant(1.0)),
            "r untouched"
        );
        let ParameterValue::Channel(width) = param_of(&applied.document, &path, node, "width")
        else {
            panic!("channel");
        };
        assert_eq!(at(&width, &layer, 10), hard_end);
    }

    /// An int channel rounds on read, so the key is stored already rounded and
    /// evaluates to the value it holds.
    #[test]
    fn set_keyframes_on_an_int_parameter_stores_whole_values() {
        let reg = registry();
        let (doc, path, node) = doc_with_node("shape.polygon");
        let op = set_keys(param_target(&path, node, "sides"), 0, vec![linear(0, 7.4)]);
        let applied = apply(&doc, &op, &cx(&reg)).unwrap();
        let ParameterValue::IntChannel(AnimationChannel {
            source: ChannelSource::Keyframes(curve),
        }) = param_of(&applied.document, &path, node, "sides")
        else {
            panic!("a keyed Int becomes an int channel");
        };
        assert_eq!(curve.keyframes()[0].value, 7.0);
    }

    #[test]
    fn a_bezier_key_gets_the_editors_default_handles() {
        let reg = registry();
        let (doc, path, node, ..) = started_layer();
        let op = set_keys(
            param_target(&path, node, "width"),
            0,
            vec![
                key(10, 0.0, KeyInterpolation::Bezier),
                key(19, 9.0, KeyInterpolation::Step),
            ],
        );
        let applied = apply(&doc, &op, &cx(&reg)).unwrap();
        let ParameterValue::Channel(AnimationChannel {
            source: ChannelSource::Keyframes(curve),
        }) = param_of(&applied.document, &path, node, "width")
        else {
            panic!("keys");
        };
        let [first, second] = [curve.keyframes()[0], curve.keyframes()[1]];
        assert_eq!(first.interpolation, Interpolation::Bezier);
        assert_eq!(first.tangent_out, crate::types::Vec2(3.0, 3.0));
        assert_eq!(second.tangent_in, crate::types::Vec2(-3.0, -3.0));
        assert_eq!(second.interpolation, Interpolation::Step);
    }

    #[test]
    fn set_keyframes_on_a_shell_property_evaluates_to_the_keys() {
        let reg = registry();
        let (doc, _, _, comp, layer) = started_layer();
        let ops = [
            set_keys(
                shell_target(comp, layer, ShellProperty::Opacity),
                0,
                vec![linear(10, 0.0), linear(20, 3.0)],
            ),
            set_keys(
                shell_target(comp, layer, ShellProperty::Position),
                1,
                vec![linear(10, 100.0), linear(30, -100.0)],
            ),
            set_keys(
                shell_target(comp, layer, ShellProperty::TimeRemap),
                0,
                vec![linear(10, 0.0), linear(20, 50.0)],
            ),
        ];
        let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
        let l = layer_of(&applied.document, comp, layer);
        assert_eq!(at(&l.opacity, &l, 10), 0.0);
        assert_eq!(at(&l.opacity, &l, 20), 1.0, "opacity is clamped to 0..1");
        assert_eq!(at(&l.transform.position[1], &l, 10), 100.0);
        assert_eq!(at(&l.transform.position[1], &l, 30), -100.0);
        assert_eq!(at(&l.transform.position[1], &l, 20), 0.0);
        assert!(matches!(
            l.transform.position[0].source,
            ChannelSource::Constant(_)
        ));
        let remap = l.time_remap.as_ref().expect("the time remap is created");
        assert_eq!(at(remap, &l, 20), 50.0);
    }

    #[test]
    fn a_layer_alias_from_the_same_batch_addresses_the_shell() {
        let reg = registry();
        let (doc, comp, _) = doc_with_layer(false);
        let ops = [
            add_layer(comp_ref(comp), "solid", alias("l")),
            set_keys(
                ChannelTarget::Shell {
                    comp: comp_ref(comp),
                    layer: LayerRef::Alias("l".into()),
                    property: ShellProperty::Opacity,
                },
                0,
                vec![linear(0, 0.25)],
            ),
        ];
        let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
        let Some(Created::Layer(id)) = applied.aliases.get("l").copied() else {
            panic!("alias");
        };
        let l = layer_of(&applied.document, comp, id);
        assert!(matches!(l.opacity.source, ChannelSource::Keyframes(_)));
    }

    #[test]
    fn clear_animation_leaves_a_constant() {
        let reg = registry();
        let (doc, path, node, comp, layer) = started_layer();
        let keyed = [
            set_keys(
                param_target(&path, node, "width"),
                0,
                vec![linear(10, 2.0), linear(20, 8.0)],
            ),
            set_keys(
                shell_target(comp, layer, ShellProperty::Opacity),
                0,
                vec![linear(10, 0.0), linear(20, 1.0)],
            ),
        ];
        let keyed = apply_batch(&doc, &keyed, &cx(&reg)).unwrap().document;
        let cleared = [
            clear(param_target(&path, node, "width"), 0, 3.0),
            clear(shell_target(comp, layer, ShellProperty::Opacity), 0, 0.5),
        ];
        let cleared = apply_batch(&keyed, &cleared, &cx(&reg)).unwrap().document;
        assert!(matches!(
            param_of(&cleared, &path, node, "width"),
            ParameterValue::Channel(AnimationChannel {
                source: ChannelSource::Constant(3.0)
            })
        ));
        let l = layer_of(&cleared, comp, layer);
        assert!(matches!(l.opacity.source, ChannelSource::Constant(0.5)));

        // An expression can be dropped too; a plain Float stays a Float.
        let expr = with_param_value(
            &doc,
            &path,
            node,
            "width",
            ParameterValue::Channel(AnimationChannel::new(ChannelSource::Expression(
                ParameterExpression::new("frame"),
            ))),
        );
        let op = clear(param_target(&path, node, "width"), 0, 4.0);
        let done = apply(&expr, &op, &cx(&reg)).unwrap().document;
        assert!(matches!(
            param_of(&done, &path, node, "width"),
            ParameterValue::Channel(AnimationChannel {
                source: ChannelSource::Constant(4.0)
            })
        ));
        let plain = apply(&doc, &op, &cx(&reg)).unwrap().document;
        assert_eq!(
            param_of(&plain, &path, node, "width"),
            ParameterValue::Float(4.0)
        );
    }

    /// Every refusal names its code and leaves the document as it was.
    /// A keyed identifier would stop resolving its target, so it is refused
    /// as the Properties keyframe toggle refuses it.
    #[test]
    fn keying_an_identifier_parameter_is_refused() {
        let reg = registry();
        let (doc, path, node) = doc_with_node("layer.ref");
        let op = set_keys(param_target(&path, node, "layer"), 0, vec![linear(0, 1.0)]);
        let err = apply(&doc, &op, &cx(&reg)).unwrap_err();
        assert_eq!(err.code(), "invalid-keyframes");
    }

    #[test]
    fn refused_animation_edits_leave_the_document_unchanged() {
        let reg = registry();
        let (doc, path, node, comp, layer) = started_layer();
        let expr =
            AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new("frame")));
        let driven_param = with_param_value(
            &doc,
            &path,
            node,
            "width",
            ParameterValue::Channel(expr.clone()),
        );
        let driven_shell =
            document::update_layer(&doc, comp, layer, |l| l.opacity = expr.clone()).unwrap();
        let animated_shell = document::update_layer(&doc, comp, layer, |l| {
            l.transform.position[0] = keyed_channel()
        })
        .unwrap();
        let blended = with_param_value(
            &doc,
            &path,
            node,
            "width",
            ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
                Box::new(ChannelSource::Constant(1.0)),
                Box::new(ChannelSource::Constant(2.0)),
                crate::animation::blend::BlendMode::Mix,
                0.5,
            ))),
        );
        let missing_layer = LayerId::next();
        let width = || param_target(&path, node, "width");
        let ok_keys = || vec![linear(10, 1.0)];
        let cases: Vec<(&Document, EditOp, &str)> = vec![
            (
                &driven_param,
                set_keys(width(), 0, ok_keys()),
                "driven-parameter",
            ),
            (
                &blended,
                set_keys(width(), 0, ok_keys()),
                "driven-parameter",
            ),
            (&blended, clear(width(), 0, 1.0), "driven-parameter"),
            (
                &driven_shell,
                set_keys(
                    shell_target(comp, layer, ShellProperty::Opacity),
                    0,
                    ok_keys(),
                ),
                "driven-parameter",
            ),
            (
                &driven_shell,
                layer_op(comp, layer, |o| o.opacity = Some(0.5)),
                "driven-parameter",
            ),
            (
                &animated_shell,
                layer_op(comp, layer, |o| o.position = Some([1.0, 2.0])),
                "animated-parameter",
            ),
            (&doc, set_keys(width(), 0, vec![]), "invalid-keyframes"),
            (
                &doc,
                set_keys(width(), 0, vec![linear(10, 1.0), linear(10, 2.0)]),
                "invalid-keyframes",
            ),
            (
                &doc,
                set_keys(width(), 0, vec![linear(10, f32::NAN)]),
                "invalid-keyframes",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| o.start_frame = Some(i64::MIN)),
                "invalid-layer-edit",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| o.out_frame = Some(u64::MAX)),
                "invalid-layer-edit",
            ),
            (
                &doc,
                set_keys(width(), 0, vec![linear(9, 1.0)]),
                "key-before-layer-start",
            ),
            (&doc, set_keys(width(), 1, ok_keys()), "unknown-channel"),
            (
                &doc,
                set_keys(
                    shell_target(comp, layer, ShellProperty::Opacity),
                    1,
                    ok_keys(),
                ),
                "unknown-channel",
            ),
            (
                &doc,
                set_keys(
                    shell_target(comp, layer, ShellProperty::AudioGain),
                    0,
                    ok_keys(),
                ),
                "unknown-channel",
            ),
            (
                &doc,
                set_keys(param_target(&path, node, "cap"), 0, ok_keys()),
                "parameter-type-mismatch",
            ),
            (
                &doc,
                set_keys(param_target(&path, node, "nope"), 0, ok_keys()),
                "unknown-parameter",
            ),
            (
                &doc,
                set_keys(
                    shell_target(comp, missing_layer, ShellProperty::Opacity),
                    0,
                    ok_keys(),
                ),
                "layer-not-found",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| {
                    o.in_frame = Some(50);
                    o.out_frame = Some(40);
                }),
                "invalid-layer-edit",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| o.parent = Some(Some(LayerRef::Id(layer)))),
                "invalid-layer-edit",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| {
                    o.parent = Some(Some(LayerRef::Id(missing_layer)))
                }),
                "layer-not-found",
            ),
            (
                &doc,
                layer_op(comp, layer, |o| {
                    o.parent = Some(Some(LayerRef::Alias("x".into())))
                }),
                "unknown-alias",
            ),
        ];
        assert!(!cases.is_empty());
        for (doc, op, code) in cases {
            let before = doc.clone();
            let err = apply(doc, &op, &cx(&reg)).unwrap_err();
            assert_eq!(err.code(), code, "{op:?}: {err}");
            assert_eq!(*doc, before, "{op:?} changed the document");
        }
    }

    /// A blank `SetLayer`, for a test to fill in.
    #[derive(Default)]
    struct LayerFields {
        name: Option<String>,
        start_frame: Option<i64>,
        in_frame: Option<u64>,
        out_frame: Option<u64>,
        position: Option<[f32; 2]>,
        rotation: Option<f32>,
        opacity: Option<f32>,
        blend_mode: Option<crate::composition::BlendMode>,
        parent: Option<Option<LayerRef>>,
        solo: Option<bool>,
        locked: Option<bool>,
    }

    fn layer_op(comp: CompId, layer: LayerId, f: impl FnOnce(&mut LayerFields)) -> EditOp {
        let mut o = LayerFields::default();
        f(&mut o);
        EditOp::SetLayer {
            comp: comp_ref(comp),
            layer: LayerRef::Id(layer),
            name: o.name,
            start_frame: o.start_frame,
            in_frame: o.in_frame,
            out_frame: o.out_frame,
            anchor_point: None,
            position: o.position,
            scale: None,
            rotation: o.rotation,
            opacity: o.opacity,
            blend_mode: o.blend_mode,
            parent: o.parent,
            solo: o.solo,
            muted: None,
            locked: o.locked,
        }
    }

    #[test]
    fn set_layer_writes_the_shell() {
        let reg = registry();
        let (doc, comp, layer) = doc_with_layer(false);
        let other = LayerId::next();
        let doc = document::add_layer(&doc, comp, Layer::new(other, "B", Graph::new())).unwrap();
        let op = layer_op(comp, layer, |o| {
            o.name = Some("Renamed".into());
            o.start_frame = Some(-5);
            o.in_frame = Some(2);
            o.out_frame = Some(90);
            o.position = Some([4.0, -6.0]);
            o.rotation = Some(45.0);
            o.opacity = Some(7.0);
            o.blend_mode = Some(crate::composition::BlendMode::Screen);
            o.parent = Some(Some(LayerRef::Id(other)));
            o.solo = Some(true);
            o.locked = Some(true);
        });
        let applied = apply(&doc, &op, &cx(&reg)).unwrap();
        let l = layer_of(&applied.document, comp, layer);
        assert_eq!(l.name, "Renamed");
        assert_eq!((l.start_frame, l.in_frame, l.out_frame), (-5, 2, 90));
        assert!(matches!(
            l.transform.position[0].source,
            ChannelSource::Constant(4.0)
        ));
        assert!(matches!(
            l.transform.position[1].source,
            ChannelSource::Constant(-6.0)
        ));
        assert!(matches!(
            l.transform.rotation.source,
            ChannelSource::Constant(45.0)
        ));
        assert!(
            matches!(l.opacity.source, ChannelSource::Constant(1.0)),
            "clamped"
        );
        assert_eq!(l.blend_mode, crate::composition::BlendMode::Screen);
        assert_eq!(l.parent, Some(other));
        assert!(l.solo && l.locked);

        // `other` is now `layer`'s parent: the reverse link would be a cycle.
        let cycle = layer_op(comp, other, |o| o.parent = Some(Some(LayerRef::Id(layer))));
        assert_eq!(
            apply(&applied.document, &cycle, &cx(&reg))
                .unwrap_err()
                .code(),
            "invalid-layer-edit"
        );

        // `parent: null` clears it, an absent `parent` leaves it.
        let clear_parent = layer_op(comp, layer, |o| o.parent = Some(None));
        let cleared = apply(&applied.document, &clear_parent, &cx(&reg)).unwrap();
        assert_eq!(layer_of(&cleared.document, comp, layer).parent, None);
        let untouched = layer_op(comp, layer, |o| o.name = Some("Again".into()));
        let kept = apply(&applied.document, &untouched, &cx(&reg)).unwrap();
        assert_eq!(layer_of(&kept.document, comp, layer).parent, Some(other));
    }

    #[test]
    fn a_batch_of_animation_edits_is_one_document() {
        // One `apply_batch` result is the one value a caller commits, which is
        // what makes the batch a single undo step; `commit_edit_ops` is
        // tested at that level in ravel-app.
        let reg = registry();
        let (doc, path, node, comp, layer) = started_layer();
        let ops = [
            set_keys(param_target(&path, node, "width"), 0, vec![linear(10, 1.0)]),
            set_keys(
                shell_target(comp, layer, ShellProperty::Opacity),
                0,
                vec![linear(10, 1.0)],
            ),
            layer_op(comp, layer, |o| o.name = Some("X".into())),
        ];
        let applied = apply_batch(&doc, &ops, &cx(&reg)).unwrap();
        assert_ne!(applied.document, doc);
        assert!(applied.created.is_empty());
    }

    #[test]
    fn animation_ops_round_trip_through_json() {
        let path = NetworkPath::layer(CompId::new(7), LayerId::new(3));
        let ops = vec![
            set_keys(
                param_target(&path, NodeId::new(5), "width"),
                0,
                vec![key(10, 1.5, KeyInterpolation::Bezier), linear(20, 2.0)],
            ),
            clear(
                shell_target(CompId::new(7), LayerId::new(3), ShellProperty::TimeRemap),
                0,
                0.0,
            ),
            layer_op(CompId::new(7), LayerId::new(3), |o| o.parent = Some(None)),
            layer_op(CompId::new(7), LayerId::new(3), |o| o.opacity = Some(0.5)),
        ];
        let json = serde_json::to_string(&ops).unwrap();
        assert!(json.contains(r#""property":"time_remap""#), "{json}");
        assert!(json.contains(r#""interpolation":"bezier""#), "{json}");
        assert!(json.contains(r#""parent":null"#), "{json}");
        let back: Vec<EditOp> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ops);
        // The wire form an agent writes: the target's fields beside the op's.
        let wire = r#"{"op":"set_keyframes","comp":7,"layer":"l","property":"position","component":1,"keys":[{"frame":0,"value":1,"interpolation":"step"}]}"#;
        let EditOp::SetKeyframes {
            target,
            component,
            keys,
        } = serde_json::from_str(wire).unwrap()
        else {
            panic!("set_keyframes");
        };
        assert!(matches!(
            target,
            ChannelTarget::Shell {
                property: ShellProperty::Position,
                ..
            }
        ));
        assert_eq!((component, keys.len()), (1, 1));
    }

    #[test]
    fn animation_ops_are_not_destructive() {
        let path = NetworkPath::layer(CompId::new(7), LayerId::new(3));
        assert!(!set_keys(param_target(&path, NodeId::new(5), "w"), 0, vec![]).is_destructive());
        assert!(!clear(param_target(&path, NodeId::new(5), "w"), 0, 0.0).is_destructive());
        assert!(!layer_op(CompId::new(7), LayerId::new(3), |_| {}).is_destructive());
    }
}

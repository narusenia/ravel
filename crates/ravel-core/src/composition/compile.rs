// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shell compiler: expands a Composition's layer stack into the synthetic
//! DAG chain that composites the layers' networks (REQ-LAYER-007).
//!
//! Each layer with a `frame` output becomes a chain:
//!
//! ```text
//! normal layer:     [Network boundary] → Transform → Opacity → Merge
//! adjustment layer: [Network boundary] → Transform → Merge(adjustment)
//!                        ▲ source                ▲ background
//! ```
//!
//! The boundary node evaluates the layer's owned network under a
//! layer-local [`crate::eval::EvalContext`]; `Transform` / `Opacity` /
//! `Merge` apply the shell's generic properties. Layers without a `frame`
//! output (null layers) only receive a `Transform` node so parenting
//! references keep working.
//!
//! All generated nodes use deterministic IDs derived from `(CompId, LayerId,
//! Role)` and are marked `synthetic = true` so they are excluded from
//! persistence and hidden in the node editor UI.

use crate::composition::{BlendMode, Composition, Layer};
use crate::graph::{Graph, GraphError, InputPort, Node, NodeMetadata, OutputPort};
use crate::id::{CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use thiserror::Error;

// ===========================================================================
// Role enum for deterministic ID computation
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeRole {
    /// Boundary between the shell chain and the layer's owned network.
    Network = 0,
    Transform = 1,
    Opacity = 2,
    Merge = 3,
}

// ===========================================================================
// Deterministic ID generation
// ===========================================================================

/// Derive a deterministic NodeId from composition, layer, and role.
///
/// Layout: `comp_id[31:0] << 32 | layer_id[23:0] << 8 | role[7:0]`
pub fn deterministic_node_id(comp_id: CompId, layer_id: LayerId, role: NodeRole) -> NodeId {
    let id = (comp_id.raw() << 32) | (layer_id.raw() << 8) | (role as u64);
    NodeId::new(id)
}

/// Decode the `(comp, layer, role)` packed by [`deterministic_node_id`].
///
/// Synthetic shell processors use this to resolve the layer whose properties
/// they apply, without capturing anything at registration time.
pub fn decode_deterministic_node_id(id: NodeId) -> Option<(CompId, LayerId, NodeRole)> {
    let raw = id.raw();
    let role = match raw & 0xFF {
        0 => NodeRole::Network,
        1 => NodeRole::Transform,
        2 => NodeRole::Opacity,
        3 => NodeRole::Merge,
        _ => return None,
    };
    Some((
        CompId::new(raw >> 32),
        LayerId::new((raw >> 8) & 0xFF_FFFF),
        role,
    ))
}

/// Derive a deterministic EdgeId from source and target NodeIds.
fn deterministic_edge_id(source: NodeId, target: NodeId) -> EdgeId {
    let id = source.raw().wrapping_mul(0x9E37_79B9) ^ target.raw();
    EdgeId::new(id)
}

// ===========================================================================
// Compilation result
// ===========================================================================

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("graph error during compilation: {0}")]
    GraphError(#[from] GraphError),

    #[error("composition {0:?} has no active layers")]
    NoActiveLayers(CompId),
}

/// Result of compiling a single composition.
#[derive(Debug)]
pub struct CompilationResult {
    /// The graph with all synthetic nodes inserted.
    pub graph: Graph,
    /// The NodeId of the final output (last Merge node).
    pub output_node: NodeId,
    /// All synthetic NodeIds generated during compilation.
    pub synthetic_nodes: Vec<NodeId>,
}

// ===========================================================================
// Synthetic node builders
// ===========================================================================

fn synthetic_node(id: NodeId, type_key: &str, label: &str) -> Node {
    Node {
        id,
        type_key: type_key.to_string(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        parameters: Vec::new(),
        metadata: NodeMetadata {
            label: Some(label.to_string()),
            synthetic: true,
            ..NodeMetadata::default()
        },
        subnet: None,
    }
}

fn blend_mode_type_key(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "comp.merge.normal",
        BlendMode::Add => "comp.merge.add",
        BlendMode::Multiply => "comp.merge.multiply",
        BlendMode::Screen => "comp.merge.screen",
        BlendMode::Overlay => "comp.merge.overlay",
    }
}

fn fb_input(name: &str) -> InputPort {
    InputPort {
        name: name.to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    }
}

fn fb_output() -> OutputPort {
    OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    }
}

/// Boundary node: evaluates the layer's network under layer-local time.
fn make_network_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Network);
    let label = format!("{} [Network]", layer.name);
    let mut node = synthetic_node(id, "comp.network", &label);
    // Adjustment layers receive the composited lower stack here.
    node.inputs.push(fb_input("source"));
    node.outputs.push(fb_output());
    node
}

fn make_transform_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Transform);
    let label = format!("{} [Transform]", layer.name);
    let mut node = synthetic_node(id, "comp.transform", &label);
    node.inputs.push(fb_input("input"));
    node.inputs.push(fb_input("parent_transform"));
    node.outputs.push(fb_output());
    node
}

fn make_opacity_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Opacity);
    let label = format!("{} [Opacity]", layer.name);
    let mut node = synthetic_node(id, "comp.opacity", &label);
    node.inputs.push(fb_input("input"));
    node.outputs.push(fb_output());
    node
}

fn make_merge_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Merge);
    let label = format!("{} [Merge]", layer.name);
    let type_key = if layer.adjustment {
        "comp.merge.adjustment"
    } else {
        blend_mode_type_key(layer.blend_mode)
    };
    let mut node = synthetic_node(id, type_key, &label);
    node.inputs.push(fb_input("background"));
    node.inputs.push(fb_input("foreground"));
    node.outputs.push(fb_output());
    node
}

// ===========================================================================
// Solo/mute pre-pass
// ===========================================================================

/// Determine which layers are active after solo/mute filtering.
fn active_layers(comp: &Composition) -> Vec<&Layer> {
    let any_solo = comp.layers.iter().any(|l| l.solo);

    comp.layers
        .iter()
        .filter(|l| {
            if l.muted {
                return false;
            }
            if any_solo && !l.solo {
                return false;
            }
            true
        })
        .collect()
}

// ===========================================================================
// Main compiler
// ===========================================================================

/// Compile a Composition's layers into the synthetic shell chain.
///
/// The resulting graph contains all existing nodes plus the synthetic nodes
/// generated from the composition's layers. Layer networks are **not**
/// flattened into the graph; the boundary node evaluates them at pull time
/// (REQ-LAYER-007).
pub fn compile_composition(
    comp: &Composition,
    graph: Graph,
) -> Result<CompilationResult, CompileError> {
    let active = active_layers(comp);

    if active.is_empty() {
        return Err(CompileError::NoActiveLayers(comp.id));
    }

    let mut g = graph;
    let mut synthetic_nodes = Vec::new();
    let mut prev_merge_id: Option<NodeId> = None;

    // Process layers bottom-to-top (index 0 = bottom).
    for layer in &active {
        let has_frame = layer.has_frame_output();

        // 1. Network boundary (only for layers that produce a frame).
        let mut chain_tip: Option<NodeId> = None;
        if has_frame {
            let network = make_network_node(comp.id, layer);
            let network_id = network.id;
            synthetic_nodes.push(network_id);
            g = g.add_node(network)?;

            if layer.adjustment
                && let Some(prev_id) = prev_merge_id
            {
                // The composited lower stack feeds the adjustment network.
                g = g.add_edge(
                    deterministic_edge_id(prev_id, network_id),
                    prev_id,
                    OutputPortIndex(0),
                    network_id,
                    InputPortIndex(0),
                )?;
            }
            chain_tip = Some(network_id);
        }

        // 2. Transform node (always: null layers keep it for parenting).
        let transform = make_transform_node(comp.id, layer);
        let transform_id = transform.id;
        synthetic_nodes.push(transform_id);
        g = g.add_node(transform)?;

        if let Some(tip) = chain_tip {
            g = g.add_edge(
                deterministic_edge_id(tip, transform_id),
                tip,
                OutputPortIndex(0),
                transform_id,
                InputPortIndex(0),
            )?;
        }

        // 2b. Parent transform edge (if parent exists and is active).
        if let Some(parent_id) = layer.parent
            && active.iter().any(|l| l.id == parent_id)
        {
            let parent_transform_id =
                deterministic_node_id(comp.id, parent_id, NodeRole::Transform);
            g = g.add_edge(
                deterministic_edge_id(parent_transform_id, transform_id),
                parent_transform_id,
                OutputPortIndex(0),
                transform_id,
                InputPortIndex(1),
            )?;
        }

        // Layers without a frame output stop here (null layers).
        if !has_frame {
            continue;
        }

        // 3. Merge node. Adjustment layers skip the Opacity node: their
        //    opacity acts as the effect strength inside the adjustment merge
        //    (REQ-LAYER-010).
        let merge = make_merge_node(comp.id, layer);
        let merge_id = merge.id;
        synthetic_nodes.push(merge_id);
        g = g.add_node(merge)?;

        let foreground_tip = if layer.adjustment {
            transform_id
        } else {
            let opacity = make_opacity_node(comp.id, layer);
            let opacity_id = opacity.id;
            synthetic_nodes.push(opacity_id);
            g = g.add_node(opacity)?;

            g = g.add_edge(
                deterministic_edge_id(transform_id, opacity_id),
                transform_id,
                OutputPortIndex(0),
                opacity_id,
                InputPortIndex(0),
            )?;
            opacity_id
        };

        // Background input: previous merge output (if any).
        if let Some(prev_id) = prev_merge_id {
            g = g.add_edge(
                deterministic_edge_id(prev_id, merge_id),
                prev_id,
                OutputPortIndex(0),
                merge_id,
                InputPortIndex(0),
            )?;
        }

        // Foreground input.
        g = g.add_edge(
            deterministic_edge_id(foreground_tip, merge_id),
            foreground_tip,
            OutputPortIndex(0),
            merge_id,
            InputPortIndex(1),
        )?;

        prev_merge_id = Some(merge_id);
    }

    let output_node = prev_merge_id.ok_or(CompileError::NoActiveLayers(comp.id))?;

    Ok(CompilationResult {
        output_node,
        graph: g,
        synthetic_nodes,
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::id::{CompId, LayerId};

    fn test_comp() -> Composition {
        Composition::new(
            CompId::new(1),
            "Test",
            (1920, 1080),
            crate::types::FrameRate::new(30, 1),
            300,
        )
    }

    /// Layer whose network has an Out node with a `frame` input.
    fn frame_layer(id: u64) -> Layer {
        let out = Node::new(NodeId::new(10_000 + id), crate::network::NET_OUT_TYPE_KEY)
            .with_input(crate::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new().add_node(out).unwrap();
        Layer::new(LayerId::new(id), format!("Layer {id}"), network).with_time(0, 0, 300)
    }

    /// Null layer: network without a frame output.
    fn null_layer(id: u64) -> Layer {
        Layer::new(LayerId::new(id), format!("Null {id}"), Graph::new()).with_time(0, 0, 300)
    }

    #[test]
    fn deterministic_id_is_stable() {
        let id1 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Network);
        let id2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Network);
        assert_eq!(id1, id2);
    }

    #[test]
    fn deterministic_id_roundtrip() {
        let id = deterministic_node_id(CompId::new(7), LayerId::new(42), NodeRole::Merge);
        let (c, l, r) = decode_deterministic_node_id(id).unwrap();
        assert_eq!(c, CompId::new(7));
        assert_eq!(l, LayerId::new(42));
        assert_eq!(r, NodeRole::Merge);
    }

    #[test]
    fn compile_single_layer() {
        let comp = test_comp().add_layer(frame_layer(1));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        // Network + Transform + Opacity + Merge = 4 nodes
        assert_eq!(result.synthetic_nodes.len(), 4);
        assert_eq!(result.graph.node_count(), 4);

        for node in result.graph.nodes() {
            assert!(node.metadata.synthetic);
        }
    }

    #[test]
    fn compile_three_layers() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer(frame_layer(2))
            .add_layer(frame_layer(3));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        assert_eq!(result.synthetic_nodes.len(), 12);

        let merge_3 = deterministic_node_id(CompId::new(1), LayerId::new(3), NodeRole::Merge);
        assert_eq!(result.output_node, merge_3);
    }

    #[test]
    fn null_layer_only_gets_transform() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer(null_layer(2));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        // frame layer: 4 nodes; null layer: 1 Transform node.
        assert_eq!(result.synthetic_nodes.len(), 5);
        let null_transform =
            deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Transform);
        assert!(result.graph.node(null_transform).is_some());
        // Output stays the frame layer's merge.
        let merge_1 = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        assert_eq!(result.output_node, merge_1);
    }

    #[test]
    fn all_null_layers_returns_error() {
        let comp = test_comp().add_layer(null_layer(1));
        let err = compile_composition(&comp, Graph::new()).unwrap_err();
        assert!(matches!(err, CompileError::NoActiveLayers(_)));
    }

    #[test]
    fn solo_filters_non_solo_layers() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer({
                let mut l = frame_layer(2);
                l.solo = true;
                l
            })
            .add_layer(frame_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        assert_eq!(result.synthetic_nodes.len(), 4);
    }

    #[test]
    fn muted_layer_excluded() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer({
                let mut l = frame_layer(2);
                l.muted = true;
                l
            })
            .add_layer(frame_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        assert_eq!(result.synthetic_nodes.len(), 8);
    }

    #[test]
    fn parent_transform_edge() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer(frame_layer(2).with_parent(LayerId::new(1)));

        let result = compile_composition(&comp, Graph::new()).unwrap();

        let parent_transform =
            deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Transform);
        let child_transform =
            deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Transform);

        let has_parent_edge = result.graph.edges().any(|e| {
            e.source == parent_transform
                && e.target == child_transform
                && e.target_port == InputPortIndex(1)
        });
        assert!(has_parent_edge);
    }

    #[test]
    fn null_parent_still_receives_transform_node_for_edge() {
        let comp = test_comp()
            .add_layer(null_layer(1))
            .add_layer(frame_layer(2).with_parent(LayerId::new(1)));

        let result = compile_composition(&comp, Graph::new()).unwrap();

        let parent_transform =
            deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Transform);
        assert!(result.graph.node(parent_transform).is_some());
    }

    #[test]
    fn merge_chain_connects_sequentially() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer(frame_layer(2))
            .add_layer(frame_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();

        let merge_1 = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        let merge_2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Merge);
        let merge_3 = deterministic_node_id(CompId::new(1), LayerId::new(3), NodeRole::Merge);

        let has_1_to_2 = result.graph.edges().any(|e| {
            e.source == merge_1 && e.target == merge_2 && e.target_port == InputPortIndex(0)
        });
        assert!(has_1_to_2);

        let has_2_to_3 = result.graph.edges().any(|e| {
            e.source == merge_2 && e.target == merge_3 && e.target_port == InputPortIndex(0)
        });
        assert!(has_2_to_3);
    }

    #[test]
    fn layer_chain_topology() {
        let comp = test_comp().add_layer(frame_layer(1));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        let network = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Network);
        let transform = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Transform);
        let opacity = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Opacity);
        let merge = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);

        let has_edge = |from: NodeId, to: NodeId| {
            result
                .graph
                .edges()
                .any(|e| e.source == from && e.target == to)
        };

        assert!(has_edge(network, transform));
        assert!(has_edge(transform, opacity));
        assert!(has_edge(opacity, merge));
    }

    #[test]
    fn adjustment_layer_topology() {
        let mut adj = frame_layer(2);
        adj.adjustment = true;
        let comp = test_comp().add_layer(frame_layer(1)).add_layer(adj);

        let result = compile_composition(&comp, Graph::new()).unwrap();

        let merge_1 = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        let network_2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Network);
        let transform_2 =
            deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Transform);
        let merge_2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Merge);

        // Adjustment merge uses the dedicated type key.
        let merge_node = result.graph.node(merge_2).unwrap();
        assert_eq!(merge_node.type_key, "comp.merge.adjustment");

        let has_edge = |from: NodeId, to: NodeId, port: u32| {
            result.graph.edges().any(|e| {
                e.source == from && e.target == to && e.target_port == InputPortIndex(port)
            })
        };

        // Lower stack → boundary source; lower stack → adjustment merge bg;
        // boundary → transform → adjustment merge fg. No Opacity node.
        assert!(has_edge(merge_1, network_2, 0));
        assert!(has_edge(merge_1, merge_2, 0));
        assert!(has_edge(network_2, transform_2, 0));
        assert!(has_edge(transform_2, merge_2, 1));
        let opacity_2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Opacity);
        assert!(result.graph.node(opacity_2).is_none());

        // 4 (layer 1) + 3 (boundary + transform + merge) = 7
        assert_eq!(result.synthetic_nodes.len(), 7);
    }

    #[test]
    fn blend_mode_produces_correct_type_key() {
        let mut layer = frame_layer(1);
        layer.blend_mode = BlendMode::Multiply;
        let comp = test_comp().add_layer(layer);

        let result = compile_composition(&comp, Graph::new()).unwrap();
        let merge_id = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        let merge_node = result.graph.node(merge_id).unwrap();
        assert_eq!(merge_node.type_key, "comp.merge.multiply");
    }

    #[test]
    fn recompile_produces_same_ids() {
        let comp = test_comp().add_layer(frame_layer(1));

        let r1 = compile_composition(&comp, Graph::new()).unwrap();
        let r2 = compile_composition(&comp, Graph::new()).unwrap();

        assert_eq!(r1.output_node, r2.output_node);
        assert_eq!(r1.synthetic_nodes, r2.synthetic_nodes);
    }

    #[test]
    fn topological_sort_succeeds_after_compile() {
        let comp = test_comp()
            .add_layer(frame_layer(1))
            .add_layer(frame_layer(2).with_parent(LayerId::new(1)))
            .add_layer(frame_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        let order = result.graph.topological_sort();
        assert!(order.is_ok());
        assert_eq!(order.unwrap().len(), result.graph.node_count());
    }
}

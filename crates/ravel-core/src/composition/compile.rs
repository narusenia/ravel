// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition compiler: expands Layer stacks into DAG node chains.
//!
//! Each Layer becomes a chain:
//! `Source → TimeOffset → [Effects] → Transform → Opacity → Merge`
//!
//! All generated nodes use deterministic IDs derived from `(CompId, LayerId, Role)`
//! and are marked `synthetic = true` so they are excluded from persistence and
//! can be hidden in the node editor UI.

use crate::composition::{BlendMode, Composition, Layer, LayerSource};
use crate::graph::{Graph, GraphError, InputPort, Node, NodeMetadata, OutputPort};
use crate::id::{CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use thiserror::Error;

// ===========================================================================
// Role enum for deterministic ID computation
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeRole {
    Source = 0,
    Effects = 1,
    Transform = 2,
    Opacity = 3,
    Merge = 4,
    TimeOffset = 5,
    ShapeRasterize = 6,
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
    }
}

fn source_type_key(source: &LayerSource) -> &'static str {
    match source {
        LayerSource::Media { .. } => "comp.source.media",
        LayerSource::Solid { .. } => "comp.source.solid",
        LayerSource::Shape { .. } => "comp.source.shape",
        LayerSource::Text { .. } => "comp.source.text",
        LayerSource::PreComp { .. } => "comp.source.precomp",
        LayerSource::Generator { .. } => "comp.source.generator",
        LayerSource::Null => "comp.source.null",
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

fn make_source_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Source);
    let label = format!("{} [Source]", layer.name);
    let mut node = synthetic_node(id, source_type_key(&layer.source), &label);
    let is_shape = matches!(&layer.source, LayerSource::Shape { .. });
    if is_shape {
        node.inputs.push(InputPort {
            name: "geometry".to_string(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        });
    }
    let out_type = if is_shape {
        DataTypeId::GEOMETRY
    } else {
        DataTypeId::FRAME_BUFFER
    };
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: out_type,
    });
    node
}

fn make_shape_rasterize_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::ShapeRasterize);
    let label = format!("{} [Rasterize]", layer.name);
    let mut node = synthetic_node(id, "rasterize", &label);
    node.inputs.push(InputPort {
        name: "geometry".to_string(),
        accepted_types: vec![DataTypeId::GEOMETRY],
    });
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    });
    node
}

fn make_time_offset_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::TimeOffset);
    let label = format!("{} [TimeOffset]", layer.name);
    let mut node = synthetic_node(id, "comp.time_offset", &label);
    node.inputs.push(InputPort {
        name: "input".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    });
    node
}

fn make_transform_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Transform);
    let label = format!("{} [Transform]", layer.name);
    let mut node = synthetic_node(id, "comp.transform", &label);
    node.inputs.push(InputPort {
        name: "input".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    // Optional parent transform input.
    node.inputs.push(InputPort {
        name: "parent_transform".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    });
    node
}

fn make_opacity_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Opacity);
    let label = format!("{} [Opacity]", layer.name);
    let mut node = synthetic_node(id, "comp.opacity", &label);
    node.inputs.push(InputPort {
        name: "input".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    });
    node
}

fn make_merge_node(comp_id: CompId, layer: &Layer) -> Node {
    let id = deterministic_node_id(comp_id, layer.id, NodeRole::Merge);
    let label = format!("{} [Merge]", layer.name);
    let mut node = synthetic_node(id, blend_mode_type_key(layer.blend_mode), &label);
    // Background (lower layer result).
    node.inputs.push(InputPort {
        name: "background".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    // Foreground (this layer after transform+opacity).
    node.inputs.push(InputPort {
        name: "foreground".to_string(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
    });
    node.outputs.push(OutputPort {
        name: "output".to_string(),
        data_type: DataTypeId::FRAME_BUFFER,
    });
    node
}

// ===========================================================================
// Solo/mute pre-pass
// ===========================================================================

/// Determine which layers are active after solo/mute filtering.
///
/// - If any layer has `solo = true`, only solo layers are active.
/// - Muted layers are excluded, unless they have children that reference
///   them as parents (in which case they're kept for Transform only — the
///   caller handles this via the `muted` flag on the Layer).
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

/// Compile a Composition's layers into a DAG node chain.
///
/// The resulting graph contains all existing nodes plus the synthetic nodes
/// generated from the composition's layers.
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
        // 1. Source node
        let source = make_source_node(comp.id, layer);
        let source_id = source.id;
        synthetic_nodes.push(source_id);
        g = g.add_node(source)?;

        // 1b. Shape layers: insert synthetic rasterize between Source and TimeOffset.
        //     Optionally connect the referenced shape node → Source input.
        let fb_tip = if let LayerSource::Shape { node_id } = &layer.source {
            let rasterize = make_shape_rasterize_node(comp.id, layer);
            let rasterize_id = rasterize.id;
            synthetic_nodes.push(rasterize_id);
            g = g.add_node(rasterize)?;

            // Shape node → Source (if the shape node exists in the graph)
            if g.node(*node_id).is_some() {
                g = g.add_edge(
                    deterministic_edge_id(*node_id, source_id),
                    *node_id,
                    OutputPortIndex(0),
                    source_id,
                    InputPortIndex(0),
                )?;
            }

            // Source(GEOMETRY) → Rasterize
            g = g.add_edge(
                deterministic_edge_id(source_id, rasterize_id),
                source_id,
                OutputPortIndex(0),
                rasterize_id,
                InputPortIndex(0),
            )?;

            rasterize_id
        } else {
            source_id
        };

        // 2. TimeOffset node
        let time_offset = make_time_offset_node(comp.id, layer);
        let time_offset_id = time_offset.id;
        synthetic_nodes.push(time_offset_id);
        g = g.add_node(time_offset)?;

        // fb_tip → TimeOffset
        g = g.add_edge(
            deterministic_edge_id(fb_tip, time_offset_id),
            fb_tip,
            OutputPortIndex(0),
            time_offset_id,
            InputPortIndex(0),
        )?;

        let mut chain_tip = time_offset_id;

        // 3. Effect graph (if present) — for now, effects are a future feature.
        //    When effect_graph is Some, we would insert the subgraph here.
        //    For now, skip effects and connect directly to transform.
        if layer.effect_graph.is_some() {
            let effects_id = deterministic_node_id(comp.id, layer.id, NodeRole::Effects);
            let effects = {
                let label = format!("{} [Effects]", layer.name);
                let mut node = synthetic_node(effects_id, "comp.effects", &label);
                node.inputs.push(InputPort {
                    name: "input".to_string(),
                    accepted_types: vec![DataTypeId::FRAME_BUFFER],
                });
                node.outputs.push(OutputPort {
                    name: "output".to_string(),
                    data_type: DataTypeId::FRAME_BUFFER,
                });
                node
            };
            synthetic_nodes.push(effects_id);
            g = g.add_node(effects)?;

            // chain_tip → Effects
            g = g.add_edge(
                deterministic_edge_id(chain_tip, effects_id),
                chain_tip,
                OutputPortIndex(0),
                effects_id,
                InputPortIndex(0),
            )?;
            chain_tip = effects_id;
        }

        // 4. Transform node
        let transform = make_transform_node(comp.id, layer);
        let transform_id = transform.id;
        synthetic_nodes.push(transform_id);
        g = g.add_node(transform)?;

        // chain_tip → Transform (input port 0)
        g = g.add_edge(
            deterministic_edge_id(chain_tip, transform_id),
            chain_tip,
            OutputPortIndex(0),
            transform_id,
            InputPortIndex(0),
        )?;

        // 4b. Parent transform edge (if parent exists and is active).
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

        // 5. Opacity node
        let opacity = make_opacity_node(comp.id, layer);
        let opacity_id = opacity.id;
        synthetic_nodes.push(opacity_id);
        g = g.add_node(opacity)?;

        // Transform → Opacity
        g = g.add_edge(
            deterministic_edge_id(transform_id, opacity_id),
            transform_id,
            OutputPortIndex(0),
            opacity_id,
            InputPortIndex(0),
        )?;

        // 6. Merge node
        let merge = make_merge_node(comp.id, layer);
        let merge_id = merge.id;
        synthetic_nodes.push(merge_id);
        g = g.add_node(merge)?;

        // Background input: previous merge output (or nothing for first layer).
        if let Some(prev_id) = prev_merge_id {
            g = g.add_edge(
                deterministic_edge_id(prev_id, merge_id),
                prev_id,
                OutputPortIndex(0),
                merge_id,
                InputPortIndex(0),
            )?;
        }

        // Foreground input: Opacity output.
        g = g.add_edge(
            deterministic_edge_id(opacity_id, merge_id),
            opacity_id,
            OutputPortIndex(0),
            merge_id,
            InputPortIndex(1),
        )?;

        prev_merge_id = Some(merge_id);
    }

    Ok(CompilationResult {
        output_node: prev_merge_id.unwrap(),
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
    use crate::composition::{Composition, Layer, LayerSource};
    use crate::id::{CompId, LayerId};
    use crate::types::{Color, FrameRate};

    fn test_comp() -> Composition {
        Composition::new(
            CompId::new(1),
            "Test",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
    }

    fn solid_layer(id: u64) -> Layer {
        Layer::new(
            LayerId::new(id),
            format!("Layer {id}"),
            LayerSource::Solid {
                color: Color::WHITE,
                width: 1920,
                height: 1080,
            },
        )
        .with_time(0, 0, 300)
    }

    fn media_layer(id: u64) -> Layer {
        Layer::new(
            LayerId::new(id),
            format!("Media {id}"),
            LayerSource::Media {
                asset_id: format!("clip_{id}.mov"),
            },
        )
        .with_time(0, 0, 150)
    }

    #[test]
    fn deterministic_id_is_stable() {
        let id1 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Source);
        let id2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Source);
        assert_eq!(id1, id2);
    }

    #[test]
    fn deterministic_id_varies_by_role() {
        let source = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Source);
        let transform = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Transform);
        assert_ne!(source, transform);
    }

    #[test]
    fn compile_single_layer() {
        let comp = test_comp().add_layer(solid_layer(1));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        // Source + TimeOffset + Transform + Opacity + Merge = 5 nodes
        assert_eq!(result.synthetic_nodes.len(), 5);
        assert_eq!(result.graph.node_count(), 5);

        // All nodes are synthetic.
        for node in result.graph.nodes() {
            assert!(node.metadata.synthetic);
        }
    }

    #[test]
    fn compile_three_layers() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(media_layer(2))
            .add_layer(solid_layer(3));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        // 3 layers × 5 nodes = 15
        assert_eq!(result.synthetic_nodes.len(), 15);

        // Merges chain: layer1.merge ← layer2.merge ← layer3.merge
        let merge_3 = deterministic_node_id(CompId::new(1), LayerId::new(3), NodeRole::Merge);
        assert_eq!(result.output_node, merge_3);
    }

    #[test]
    fn compile_with_existing_graph() {
        let existing_node =
            Node::new(NodeId::new(999), "user_node").with_output("out", DataTypeId::FRAME_BUFFER);
        let graph = Graph::new().add_node(existing_node).unwrap();

        let comp = test_comp().add_layer(solid_layer(1));
        let result = compile_composition(&comp, graph).unwrap();

        // 5 synthetic + 1 existing = 6
        assert_eq!(result.graph.node_count(), 6);
        assert!(result.graph.node(NodeId::new(999)).is_some());
        assert!(
            !result
                .graph
                .node(NodeId::new(999))
                .unwrap()
                .metadata
                .synthetic
        );
    }

    #[test]
    fn solo_filters_non_solo_layers() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer({
                let mut l = solid_layer(2);
                l.solo = true;
                l
            })
            .add_layer(solid_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        // Only layer 2 is active → 5 nodes
        assert_eq!(result.synthetic_nodes.len(), 5);
    }

    #[test]
    fn muted_layer_excluded() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer({
                let mut l = solid_layer(2);
                l.muted = true;
                l
            })
            .add_layer(solid_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        // 2 active layers × 5 = 10
        assert_eq!(result.synthetic_nodes.len(), 10);
    }

    #[test]
    fn all_muted_returns_error() {
        let comp = test_comp().add_layer({
            let mut l = solid_layer(1);
            l.muted = true;
            l
        });

        let err = compile_composition(&comp, Graph::new()).unwrap_err();
        assert!(matches!(err, CompileError::NoActiveLayers(_)));
    }

    #[test]
    fn parent_transform_edge() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(2).with_parent(LayerId::new(1)));

        let result = compile_composition(&comp, Graph::new()).unwrap();

        // Verify parent transform edge exists:
        // parent(layer 1) Transform → child(layer 2) Transform input port 1
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
    fn merge_chain_connects_sequentially() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(2))
            .add_layer(solid_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();

        let merge_1 = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        let merge_2 = deterministic_node_id(CompId::new(1), LayerId::new(2), NodeRole::Merge);
        let merge_3 = deterministic_node_id(CompId::new(1), LayerId::new(3), NodeRole::Merge);

        // merge_1 output → merge_2 background (port 0)
        let has_1_to_2 = result.graph.edges().any(|e| {
            e.source == merge_1 && e.target == merge_2 && e.target_port == InputPortIndex(0)
        });
        assert!(has_1_to_2);

        // merge_2 output → merge_3 background (port 0)
        let has_2_to_3 = result.graph.edges().any(|e| {
            e.source == merge_2 && e.target == merge_3 && e.target_port == InputPortIndex(0)
        });
        assert!(has_2_to_3);
    }

    #[test]
    fn layer_chain_topology() {
        let comp = test_comp().add_layer(solid_layer(1));
        let result = compile_composition(&comp, Graph::new()).unwrap();

        let source = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Source);
        let time_off = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::TimeOffset);
        let transform = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Transform);
        let opacity = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Opacity);
        let merge = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);

        // Verify chain: Source → TimeOffset → Transform → Opacity → Merge
        let has_edge = |from: NodeId, to: NodeId| {
            result
                .graph
                .edges()
                .any(|e| e.source == from && e.target == to)
        };

        assert!(has_edge(source, time_off));
        assert!(has_edge(time_off, transform));
        assert!(has_edge(transform, opacity));
        assert!(has_edge(opacity, merge));
    }

    #[test]
    fn recompile_produces_same_ids() {
        let comp = test_comp().add_layer(solid_layer(1));

        let r1 = compile_composition(&comp, Graph::new()).unwrap();
        let r2 = compile_composition(&comp, Graph::new()).unwrap();

        assert_eq!(r1.output_node, r2.output_node);
        assert_eq!(r1.synthetic_nodes, r2.synthetic_nodes);
    }

    #[test]
    fn all_layer_sources_compile() {
        let sources = vec![
            LayerSource::Media {
                asset_id: "test.mp4".into(),
            },
            LayerSource::Solid {
                color: Color::WHITE,
                width: 100,
                height: 100,
            },
            LayerSource::Shape {
                node_id: NodeId::new(1),
            },
            LayerSource::Text {
                node_id: NodeId::new(2),
            },
            LayerSource::PreComp {
                comp_id: CompId::new(99),
            },
            LayerSource::Generator {
                node_id: NodeId::new(3),
            },
            LayerSource::Null,
        ];

        for (i, source) in sources.into_iter().enumerate() {
            let comp = test_comp().add_layer(
                Layer::new(LayerId::new(i as u64 + 100), "test", source).with_time(0, 0, 100),
            );
            let result = compile_composition(&comp, Graph::new());
            assert!(result.is_ok(), "failed for source variant {i}");
        }
    }

    #[test]
    fn topological_sort_succeeds_after_compile() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(2).with_parent(LayerId::new(1)))
            .add_layer(media_layer(3));

        let result = compile_composition(&comp, Graph::new()).unwrap();
        let order = result.graph.topological_sort();
        assert!(order.is_ok());
        assert_eq!(order.unwrap().len(), result.graph.node_count());
    }

    #[test]
    fn shape_layer_inserts_rasterize_node() {
        let shape_node =
            Node::new(NodeId::new(500), "shape.rect").with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new().add_node(shape_node).unwrap();

        let comp = test_comp().add_layer(
            Layer::new(
                LayerId::new(1),
                "Shape 1",
                LayerSource::Shape {
                    node_id: NodeId::new(500),
                },
            )
            .with_time(0, 0, 300),
        );
        let result = compile_composition(&comp, graph).unwrap();

        let source = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Source);
        let rasterize =
            deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::ShapeRasterize);
        let time_off = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::TimeOffset);

        // Source outputs GEOMETRY, rasterize outputs FRAME_BUFFER
        let source_node = result.graph.node(source).unwrap();
        assert_eq!(source_node.type_key, "comp.source.shape");
        assert_eq!(source_node.outputs[0].data_type, DataTypeId::GEOMETRY);

        let rasterize_node = result.graph.node(rasterize).unwrap();
        assert_eq!(rasterize_node.type_key, "rasterize");
        assert_eq!(
            rasterize_node.outputs[0].data_type,
            DataTypeId::FRAME_BUFFER
        );

        let has_edge = |from: NodeId, to: NodeId| {
            result
                .graph
                .edges()
                .any(|e| e.source == from && e.target == to)
        };

        // shape_node → Source → Rasterize → TimeOffset → ...
        assert!(has_edge(NodeId::new(500), source));
        assert!(has_edge(source, rasterize));
        assert!(has_edge(rasterize, time_off));

        // 6 synthetic nodes: Source + ShapeRasterize + TimeOffset + Transform + Opacity + Merge
        assert_eq!(result.synthetic_nodes.len(), 6);
    }

    #[test]
    fn shape_layer_without_shape_node_still_compiles() {
        let comp = test_comp().add_layer(
            Layer::new(
                LayerId::new(1),
                "Shape",
                LayerSource::Shape {
                    node_id: NodeId::new(999),
                },
            )
            .with_time(0, 0, 300),
        );
        let result = compile_composition(&comp, Graph::new());
        assert!(result.is_ok());
    }

    #[test]
    fn blend_mode_produces_correct_type_key() {
        let mut layer = solid_layer(1);
        layer.blend_mode = BlendMode::Multiply;
        let comp = test_comp().add_layer(layer);

        let result = compile_composition(&comp, Graph::new()).unwrap();
        let merge_id = deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge);
        let merge_node = result.graph.node(merge_id).unwrap();
        assert_eq!(merge_node.type_key, "comp.merge.multiply");
    }
}

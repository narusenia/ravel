// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validation for Composition/Layer structures.
//!
//! Detects:
//! - PreComp circular references (A contains B which contains A)
//! - Layer parenting cycles within a Composition
//! - Layer Ref circular references within a Composition (REQ-LAYER-005)

use crate::composition::Composition;
use crate::graph::{Graph, ParameterValue};
use crate::id::{CompId, LayerId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

/// Type key of the PreComp node (references another composition's output).
///
/// The node itself lands with the layer templates (REQ-LAYER-008); the
/// convention is fixed here so validation is forward-compatible.
pub const PRECOMP_TYPE_KEY: &str = "precomp";

/// Parameter on the PreComp node holding the referenced composition id.
pub const PRECOMP_COMP_ID_PARAM: &str = "comp_id";

/// Type key of the Layer Ref node (references another layer's out port
/// within the same composition, REQ-LAYER-005).
pub const LAYER_REF_TYPE_KEY: &str = "layer.ref";

/// Parameter on the Layer Ref node holding the referenced layer id.
pub const LAYER_REF_LAYER_PARAM: &str = "layer";

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("circular PreComp reference: {0:?} → {1:?}")]
    CircularPreComp(CompId, CompId),

    #[error("circular layer parenting in comp {comp:?}: {chain:?}")]
    CircularParenting { comp: CompId, chain: Vec<LayerId> },

    #[error("layer {layer:?} in comp {comp:?} references non-existent parent {parent:?}")]
    ParentNotFound {
        comp: CompId,
        layer: LayerId,
        parent: LayerId,
    },

    #[error("circular Layer Ref in comp {comp:?}: {chain:?}")]
    CircularLayerRef { comp: CompId, chain: Vec<LayerId> },
}

/// Extract referenced composition ids from a layer's network (PreComp nodes).
fn precomp_references(comp: &Composition) -> Vec<CompId> {
    comp.layers
        .iter()
        .flat_map(|layer| layer.network.nodes())
        .filter(|node| node.type_key == PRECOMP_TYPE_KEY)
        .filter_map(|node| {
            node.parameters
                .iter()
                .find(|p| p.key == PRECOMP_COMP_ID_PARAM)
                .and_then(|p| match &p.value {
                    ParameterValue::Int(v) if *v >= 0 => Some(CompId::new(*v as u64)),
                    _ => None,
                })
        })
        .collect()
}

/// Check for circular PreComp references across a set of compositions.
///
/// DFS from each composition; if we re-visit a node on the current path,
/// a cycle exists.
pub fn validate_precomp_cycles(
    compositions: &im::HashMap<CompId, Arc<Composition>>,
) -> Result<(), ValidationError> {
    for &comp_id in compositions.keys() {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        check_precomp_dfs(comp_id, compositions, &mut path, &mut visited)?;
    }
    Ok(())
}

fn check_precomp_dfs(
    comp_id: CompId,
    compositions: &im::HashMap<CompId, Arc<Composition>>,
    path: &mut Vec<CompId>,
    visited: &mut HashSet<CompId>,
) -> Result<(), ValidationError> {
    if path.contains(&comp_id) {
        let parent = *path.last().unwrap();
        return Err(ValidationError::CircularPreComp(parent, comp_id));
    }

    if visited.contains(&comp_id) {
        return Ok(());
    }

    path.push(comp_id);

    if let Some(comp) = compositions.get(&comp_id) {
        for child_id in precomp_references(comp) {
            check_precomp_dfs(child_id, compositions, path, visited)?;
        }
    }

    path.pop();
    visited.insert(comp_id);
    Ok(())
}

/// Layer ids referenced by `layer.ref` nodes inside a network, including
/// nested subnet graphs (REQ-LAYER-003). Also used by the evaluator to
/// invalidate referencing scopes when a referenced layer's shell changes.
pub(crate) fn layer_ref_targets(network: &Graph, targets: &mut Vec<LayerId>) {
    for node in network.nodes() {
        if node.type_key == LAYER_REF_TYPE_KEY
            && let Some(id) = node
                .parameters
                .iter()
                .find(|p| p.key == LAYER_REF_LAYER_PARAM)
                .and_then(|p| match &p.value {
                    ParameterValue::Int(v) if *v >= 0 => Some(LayerId::new(*v as u64)),
                    _ => None,
                })
        {
            targets.push(id);
        }
        if let Some(inner) = node.subnet.as_deref() {
            layer_ref_targets(inner, targets);
        }
    }
}

/// Check for circular Layer Ref references within a single composition
/// (REQ-LAYER-005): a layer's network referencing a layer whose network
/// (transitively) references it back — including self references — is
/// rejected. Runs at the same validation layer as
/// [`validate_precomp_cycles`].
pub fn validate_layer_ref_cycles(comp: &Composition) -> Result<(), ValidationError> {
    let mut refs: HashMap<LayerId, Vec<LayerId>> = HashMap::new();
    for layer in comp.layers.iter() {
        let mut targets = Vec::new();
        layer_ref_targets(&layer.network, &mut targets);
        refs.insert(layer.id, targets);
    }

    let mut visited = HashSet::new();
    for layer in comp.layers.iter() {
        let mut path = Vec::new();
        check_layer_ref_dfs(comp.id, layer.id, &refs, &mut path, &mut visited)?;
    }
    Ok(())
}

fn check_layer_ref_dfs(
    comp: CompId,
    layer: LayerId,
    refs: &HashMap<LayerId, Vec<LayerId>>,
    path: &mut Vec<LayerId>,
    visited: &mut HashSet<LayerId>,
) -> Result<(), ValidationError> {
    if let Some(pos) = path.iter().position(|&l| l == layer) {
        let mut chain = path[pos..].to_vec();
        chain.push(layer);
        return Err(ValidationError::CircularLayerRef { comp, chain });
    }
    if visited.contains(&layer) {
        return Ok(());
    }
    path.push(layer);
    if let Some(targets) = refs.get(&layer) {
        for &target in targets {
            check_layer_ref_dfs(comp, target, refs, path, visited)?;
        }
    }
    path.pop();
    visited.insert(layer);
    Ok(())
}

/// Check for circular layer parenting within a single composition.
///
/// For each layer with a parent, follow the chain; if we revisit a layer,
/// there's a cycle.
pub fn validate_parenting_cycles(comp: &Composition) -> Result<(), ValidationError> {
    let layer_ids: HashSet<LayerId> = comp.layers.iter().map(|l| l.id).collect();

    for layer in comp.layers.iter() {
        if let Some(parent_id) = layer.parent {
            if !layer_ids.contains(&parent_id) {
                return Err(ValidationError::ParentNotFound {
                    comp: comp.id,
                    layer: layer.id,
                    parent: parent_id,
                });
            }

            let mut visited = HashSet::new();
            visited.insert(layer.id);
            let mut current = parent_id;
            let mut chain = vec![layer.id, parent_id];

            loop {
                if visited.contains(&current) {
                    return Err(ValidationError::CircularParenting {
                        comp: comp.id,
                        chain,
                    });
                }
                visited.insert(current);

                let parent_layer = comp.layers.iter().find(|l| l.id == current);
                match parent_layer.and_then(|l| l.parent) {
                    Some(next) => {
                        chain.push(next);
                        current = next;
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::Layer;
    use crate::graph::{Graph, Node, ParameterValue};
    use crate::id::{CompId, LayerId, NodeId};
    use crate::types::FrameRate;

    fn comp(id: u64) -> Composition {
        Composition::new(
            CompId::new(id),
            format!("Comp {id}"),
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
    }

    fn empty_layer(id: u64) -> Layer {
        Layer::new(LayerId::new(id), format!("Layer {id}"), Graph::new()).with_time(0, 0, 100)
    }

    /// Layer whose network contains a PreComp node referencing `target`.
    fn precomp_layer(id: u64, node_id: u64, target: CompId) -> Layer {
        let node = Node::new(NodeId::new(node_id), PRECOMP_TYPE_KEY).with_param(
            PRECOMP_COMP_ID_PARAM,
            ParameterValue::Int(target.raw() as i32),
        );
        let network = Graph::new().add_node(node).unwrap();
        Layer::new(LayerId::new(id), "PreComp", network)
    }

    // ---- PreComp cycles ---------------------------------------------------

    #[test]
    fn no_precomp_cycle() {
        let comp1 = comp(1).add_layer(precomp_layer(1, 100, CompId::new(2)));
        let comp2 = comp(2).add_layer(empty_layer(2));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));

        assert!(validate_precomp_cycles(&comps).is_ok());
    }

    #[test]
    fn direct_precomp_cycle() {
        let comp1 = comp(1).add_layer(precomp_layer(1, 100, CompId::new(2)));
        let comp2 = comp(2).add_layer(precomp_layer(2, 200, CompId::new(1)));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));

        let err = validate_precomp_cycles(&comps).unwrap_err();
        assert!(matches!(err, ValidationError::CircularPreComp(_, _)));
    }

    #[test]
    fn transitive_precomp_cycle() {
        let comp1 = comp(1).add_layer(precomp_layer(1, 100, CompId::new(2)));
        let comp2 = comp(2).add_layer(precomp_layer(2, 200, CompId::new(3)));
        let comp3 = comp(3).add_layer(precomp_layer(3, 300, CompId::new(1)));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));
        comps.insert(CompId::new(3), Arc::new(comp3));

        assert!(validate_precomp_cycles(&comps).is_err());
    }

    #[test]
    fn self_referencing_precomp() {
        let comp1 = comp(1).add_layer(precomp_layer(1, 100, CompId::new(1)));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));

        assert!(validate_precomp_cycles(&comps).is_err());
    }

    // ---- Parenting cycles -------------------------------------------------

    #[test]
    fn no_parenting_cycle() {
        let comp = comp(1)
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(2).with_parent(LayerId::new(1)));

        assert!(validate_parenting_cycles(&comp).is_ok());
    }

    #[test]
    fn direct_parenting_cycle() {
        let comp = comp(1)
            .add_layer(empty_layer(1).with_parent(LayerId::new(2)))
            .add_layer(empty_layer(2).with_parent(LayerId::new(1)));

        let err = validate_parenting_cycles(&comp).unwrap_err();
        assert!(matches!(err, ValidationError::CircularParenting { .. }));
    }

    #[test]
    fn transitive_parenting_cycle() {
        let comp = comp(1)
            .add_layer(empty_layer(1).with_parent(LayerId::new(3)))
            .add_layer(empty_layer(2).with_parent(LayerId::new(1)))
            .add_layer(empty_layer(3).with_parent(LayerId::new(2)));

        assert!(validate_parenting_cycles(&comp).is_err());
    }

    #[test]
    fn parent_not_found() {
        let comp = comp(1).add_layer(empty_layer(1).with_parent(LayerId::new(999)));

        let err = validate_parenting_cycles(&comp).unwrap_err();
        assert!(matches!(err, ValidationError::ParentNotFound { .. }));
    }

    // ---- Layer Ref cycles ---------------------------------------------------

    /// Layer whose network contains a `layer.ref` node targeting `target`.
    fn layer_ref_layer(id: u64, node_id: u64, target: LayerId) -> Layer {
        let node = Node::new(NodeId::new(node_id), LAYER_REF_TYPE_KEY).with_param(
            LAYER_REF_LAYER_PARAM,
            ParameterValue::Int(target.raw() as i32),
        );
        let network = Graph::new().add_node(node).unwrap();
        Layer::new(LayerId::new(id), format!("Ref {id}"), network)
    }

    #[test]
    fn no_layer_ref_cycle() {
        let comp =
            comp(1)
                .add_layer(empty_layer(1))
                .add_layer(layer_ref_layer(2, 100, LayerId::new(1)));
        assert!(validate_layer_ref_cycles(&comp).is_ok());
    }

    #[test]
    fn direct_layer_ref_cycle() {
        let comp = comp(1)
            .add_layer(layer_ref_layer(1, 100, LayerId::new(2)))
            .add_layer(layer_ref_layer(2, 200, LayerId::new(1)));
        let err = validate_layer_ref_cycles(&comp).unwrap_err();
        assert!(matches!(err, ValidationError::CircularLayerRef { .. }));
    }

    #[test]
    fn transitive_layer_ref_cycle() {
        let comp = comp(1)
            .add_layer(layer_ref_layer(1, 100, LayerId::new(2)))
            .add_layer(layer_ref_layer(2, 200, LayerId::new(3)))
            .add_layer(layer_ref_layer(3, 300, LayerId::new(1)));
        assert!(validate_layer_ref_cycles(&comp).is_err());
    }

    #[test]
    fn self_layer_ref_cycle() {
        let comp = comp(1).add_layer(layer_ref_layer(1, 100, LayerId::new(1)));
        let err = validate_layer_ref_cycles(&comp).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::CircularLayerRef { chain, .. }
                if chain == vec![LayerId::new(1), LayerId::new(1)]
        ));
    }

    #[test]
    fn layer_ref_cycle_inside_subnet_is_detected() {
        // Layer 1's network holds the layer.ref inside a nested subnet.
        let ref_node = Node::new(NodeId::new(100), LAYER_REF_TYPE_KEY)
            .with_param(LAYER_REF_LAYER_PARAM, ParameterValue::Int(2));
        let inner = Graph::new().add_node(ref_node).unwrap();
        let subnet_node = Node::new(NodeId::new(101), "subnet").with_subnet(inner);
        let network = Graph::new().add_node(subnet_node).unwrap();
        let comp = comp(1)
            .add_layer(Layer::new(LayerId::new(1), "Sub", network))
            .add_layer(layer_ref_layer(2, 200, LayerId::new(1)));
        assert!(validate_layer_ref_cycles(&comp).is_err());
    }

    #[test]
    fn diamond_layer_refs_are_not_cycles() {
        // 1 and 2 both reference 3; 4 references 1 and 2. No cycle.
        let ref_node = |node_id: u64, target: u64| {
            Node::new(NodeId::new(node_id), LAYER_REF_TYPE_KEY)
                .with_param(LAYER_REF_LAYER_PARAM, ParameterValue::Int(target as i32))
        };
        let comp = comp(1)
            .add_layer(layer_ref_layer(1, 100, LayerId::new(3)))
            .add_layer(layer_ref_layer(2, 200, LayerId::new(3)))
            .add_layer(empty_layer(3))
            .add_layer(Layer::new(
                LayerId::new(4),
                "Ref 4",
                Graph::new()
                    .add_node(ref_node(300, 1))
                    .unwrap()
                    .add_node(ref_node(301, 2))
                    .unwrap(),
            ));
        assert!(validate_layer_ref_cycles(&comp).is_ok());
    }

    #[test]
    fn deep_parenting_chain_without_cycle() {
        let comp = comp(1)
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(2).with_parent(LayerId::new(1)))
            .add_layer(empty_layer(3).with_parent(LayerId::new(2)))
            .add_layer(empty_layer(4).with_parent(LayerId::new(3)));

        assert!(validate_parenting_cycles(&comp).is_ok());
    }
}

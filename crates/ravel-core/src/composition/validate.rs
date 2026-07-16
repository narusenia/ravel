// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validation for Composition/Layer structures.
//!
//! Detects:
//! - PreComp circular references (A contains B which contains A)
//! - Layer parenting cycles within a Composition

use crate::composition::{Composition, LayerSource};
use crate::id::{CompId, LayerId};
use std::collections::HashSet;
use std::sync::Arc;
use thiserror::Error;

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
        for layer in comp.layers.iter() {
            if let LayerSource::PreComp { comp_id: child_id } = &layer.source {
                check_precomp_dfs(*child_id, compositions, path, visited)?;
            }
        }
    }

    path.pop();
    visited.insert(comp_id);
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
    use crate::composition::{Composition, Layer, LayerSource};
    use crate::id::{CompId, LayerId};
    use crate::types::{Color, FrameRate};

    fn comp(id: u64) -> Composition {
        Composition::new(
            CompId::new(id),
            format!("Comp {id}"),
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
    }

    fn solid(id: u64) -> Layer {
        Layer::new(
            LayerId::new(id),
            format!("Layer {id}"),
            LayerSource::Solid {
                color: Color::WHITE,
                width: 100,
                height: 100,
            },
        )
        .with_time(0, 0, 100)
    }

    // ---- PreComp cycles ---------------------------------------------------

    #[test]
    fn no_precomp_cycle() {
        // Comp 1 contains PreComp referencing Comp 2.
        let comp1 = comp(1).add_layer(Layer::new(
            LayerId::new(1),
            "PreComp",
            LayerSource::PreComp {
                comp_id: CompId::new(2),
            },
        ));
        let comp2 = comp(2).add_layer(solid(2));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));

        assert!(validate_precomp_cycles(&comps).is_ok());
    }

    #[test]
    fn direct_precomp_cycle() {
        // Comp 1 ↔ Comp 2
        let comp1 = comp(1).add_layer(Layer::new(
            LayerId::new(1),
            "PC",
            LayerSource::PreComp {
                comp_id: CompId::new(2),
            },
        ));
        let comp2 = comp(2).add_layer(Layer::new(
            LayerId::new(2),
            "PC",
            LayerSource::PreComp {
                comp_id: CompId::new(1),
            },
        ));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));

        let err = validate_precomp_cycles(&comps).unwrap_err();
        assert!(matches!(err, ValidationError::CircularPreComp(_, _)));
    }

    #[test]
    fn transitive_precomp_cycle() {
        // Comp 1 → Comp 2 → Comp 3 → Comp 1
        let comp1 = comp(1).add_layer(Layer::new(
            LayerId::new(1),
            "PC",
            LayerSource::PreComp {
                comp_id: CompId::new(2),
            },
        ));
        let comp2 = comp(2).add_layer(Layer::new(
            LayerId::new(2),
            "PC",
            LayerSource::PreComp {
                comp_id: CompId::new(3),
            },
        ));
        let comp3 = comp(3).add_layer(Layer::new(
            LayerId::new(3),
            "PC",
            LayerSource::PreComp {
                comp_id: CompId::new(1),
            },
        ));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));
        comps.insert(CompId::new(2), Arc::new(comp2));
        comps.insert(CompId::new(3), Arc::new(comp3));

        assert!(validate_precomp_cycles(&comps).is_err());
    }

    #[test]
    fn self_referencing_precomp() {
        let comp1 = comp(1).add_layer(Layer::new(
            LayerId::new(1),
            "Self",
            LayerSource::PreComp {
                comp_id: CompId::new(1),
            },
        ));

        let mut comps = im::HashMap::new();
        comps.insert(CompId::new(1), Arc::new(comp1));

        assert!(validate_precomp_cycles(&comps).is_err());
    }

    // ---- Parenting cycles -------------------------------------------------

    #[test]
    fn no_parenting_cycle() {
        let comp = comp(1)
            .add_layer(solid(1))
            .add_layer(solid(2).with_parent(LayerId::new(1)));

        assert!(validate_parenting_cycles(&comp).is_ok());
    }

    #[test]
    fn direct_parenting_cycle() {
        let comp = comp(1)
            .add_layer(solid(1).with_parent(LayerId::new(2)))
            .add_layer(solid(2).with_parent(LayerId::new(1)));

        let err = validate_parenting_cycles(&comp).unwrap_err();
        assert!(matches!(err, ValidationError::CircularParenting { .. }));
    }

    #[test]
    fn transitive_parenting_cycle() {
        let comp = comp(1)
            .add_layer(solid(1).with_parent(LayerId::new(3)))
            .add_layer(solid(2).with_parent(LayerId::new(1)))
            .add_layer(solid(3).with_parent(LayerId::new(2)));

        assert!(validate_parenting_cycles(&comp).is_err());
    }

    #[test]
    fn parent_not_found() {
        let comp = comp(1).add_layer(solid(1).with_parent(LayerId::new(999)));

        let err = validate_parenting_cycles(&comp).unwrap_err();
        assert!(matches!(err, ValidationError::ParentNotFound { .. }));
    }

    #[test]
    fn deep_parenting_chain_without_cycle() {
        let comp = comp(1)
            .add_layer(solid(1))
            .add_layer(solid(2).with_parent(LayerId::new(1)))
            .add_layer(solid(3).with_parent(LayerId::new(2)))
            .add_layer(solid(4).with_parent(LayerId::new(3)));

        assert!(validate_parenting_cycles(&comp).is_ok());
    }
}

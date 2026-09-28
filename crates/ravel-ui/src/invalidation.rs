// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What an edit to a layer invalidates (REQ-LAYER-009).
//!
//! The Properties panel and the Timeline edit the **same** layer shell from
//! two different vocabularies — Properties names a field key, the Timeline
//! names a property row — so the decision lives here once rather than in
//! either panel. Two copies of the list below would mean the next shell
//! field reaches one panel's hint and not the other's, and the panel that
//! was missed would go on serving a stale picture with nothing to say so.
//!
//! Three outcomes, by what the edit moves:
//!
//! * a **custom parameter** feeds the layer network's In node, so only that
//!   node's processor is stale — [`InvalidationHint::Params`];
//! * [`STRUCTURAL_LAYER_FIELDS`] move the *shape* of the compiled shell
//!   chain (REQ-LAYER-007), not a value in it, so it has to be rebuilt —
//!   [`InvalidationHint::Structural`];
//! * everything else — transform, time placement, opacity, audio — is a
//!   shell value the shell processors read off the `Document` at process
//!   time. Nothing in the chain goes stale, but a node that *reads* the
//!   shell does, and `Shell` is what says so without dragging in the full
//!   `Structural` rebuild a scrub would pay per mouse move (`RESP-3`, #193).
//!
//! The third arm is deliberately a catch-all, and that makes it wider than
//! it strictly has to be: `locked` gates editing rather than rendering and
//! no information node reads it, so its `Shell` hint invalidates readers
//! that could not have changed. That is the cheaper mistake. Splitting the
//! catch-all would mean a second list of field keys, and the next shell
//! field would then have to be added to the right one of two — which is the
//! drift this module exists to remove. Over-invalidating on a click costs
//! one composition-wide scan; missing a field costs a stale picture nobody
//! can explain.

use crate::keyframes::PropertyRowId;
use crate::properties::layer::{CUSTOM_FIELD_PREFIX, in_node_id};
use ravel_core::composition::Layer;
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::InvalidationHint;

/// The shell fields the compiled chain bakes in rather than reads back from
/// the `Document`, so that moving one has to rebuild it.
///
/// `blend_mode` picks the merge node's type key and `adjustment` picks a
/// different merge and drops the opacity node; `solo` and `muted` decide
/// which layers are compiled at all; `parent` is here for the same reason —
/// `compile.rs` wires an edge from the parent's synthetic Transform node, so
/// re-parenting changes the graph's shape.
pub const STRUCTURAL_LAYER_FIELDS: [&str; 5] =
    ["blend_mode", "solo", "muted", "adjustment", "parent"];

/// The hint an edit to the layer field `key` posts (the Properties panel's
/// vocabulary). See the module documentation for the three outcomes.
pub fn layer_field_hint(key: &str, comp: CompId, layer: &Layer) -> InvalidationHint {
    if key.starts_with(CUSTOM_FIELD_PREFIX) {
        return in_node_id(layer)
            .map(|id| InvalidationHint::Params(vec![id]))
            .unwrap_or(InvalidationHint::None);
    }
    if STRUCTURAL_LAYER_FIELDS.contains(&key) {
        return InvalidationHint::Structural;
    }
    shell_hint(comp, layer.id)
}

/// The same decision for a Timeline property row, which names what it edits
/// by row rather than by field key.
///
/// A row is either a network parameter — already a node id, so the narrowest
/// hint there is — or a shell group, which is a shell edit like any other.
/// No row names one of [`STRUCTURAL_LAYER_FIELDS`]: none of them is a
/// channel, so none of them has a property row to scrub.
pub fn property_row_hint(row: &PropertyRowId, comp: CompId, layer: LayerId) -> InvalidationHint {
    match row {
        PropertyRowId::Network { node, .. } => InvalidationHint::Params(vec![*node]),
        PropertyRowId::Shell(_) => shell_hint(comp, layer),
    }
}

/// The hint for a bar gesture — a time-placement edit that may move several
/// layers of one composition as a single document change.
///
/// Folded into one hint rather than one per layer because that is what the
/// gesture commits: `InvalidationHint::merge` would union the scopes anyway,
/// and a hint per layer would post a document change per layer.
pub fn shell_hint_for_layers(
    comp: CompId,
    layers: impl IntoIterator<Item = LayerId>,
) -> InvalidationHint {
    let mut hint = InvalidationHint::None;
    for layer in layers {
        hint = hint.merge(shell_hint(comp, layer));
    }
    hint
}

fn shell_hint(comp: CompId, layer: LayerId) -> InvalidationHint {
    InvalidationHint::shell(comp, Some(layer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::timeline::PropertyGroup;
    use crate::properties::layer::sections_for_layer;
    use ravel_core::composition::Composition;
    use ravel_core::eval::EvalContext;
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::network as net;
    use ravel_core::runtime::ShellScope;
    use ravel_core::types::FrameRate;

    fn layer_with_in_node() -> Layer {
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output("amount", DataTypeId::SCALAR),
            )
            .unwrap();
        Layer::new(LayerId::next(), "L", network).with_time(0, 0, 300)
    }

    fn comp_of(layer: &Layer) -> Composition {
        Composition::new(
            CompId::next(),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
        .add_layer(layer.clone())
    }

    fn scope(comp: CompId, layer: LayerId) -> ShellScope {
        ShellScope {
            comp,
            layer: Some(layer),
        }
    }

    /// Every field key the Properties panel actually offers for a layer,
    /// taken from the production section builder rather than written out
    /// here — a list typed by hand drifts, and a key that no longer exists
    /// falls into the catch-all and pins nothing.
    fn offered_field_keys(layer: &Layer) -> Vec<String> {
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        let keys: Vec<String> = sections_for_layer(layer, &comp_of(layer), &ctx, None)
            .iter()
            .flat_map(|section| section.fields.iter())
            .map(|field| field.key().to_string())
            .collect();
        // The enumeration is the test's foundation: if it ever comes back
        // empty or without the transform, everything below passes vacuously.
        for expected in [
            "position_x",
            "position_y",
            "rotation",
            "opacity",
            "blend_mode",
        ] {
            assert!(
                keys.iter().any(|key| key == expected),
                "the layer sections no longer offer {expected}: {keys:?}"
            );
        }
        keys
    }

    /// `RESP-3` (#193): every offered field is classified, and the shell
    /// fields get `Shell` naming their own shell rather than `Structural` —
    /// which a transform scrub would pay once per mouse move, dropping every
    /// cache and recompiling every GPU pipeline at that rate.
    ///
    /// Asserting equality, not `!= Structural`: a catch-all that returned
    /// `None` would satisfy the weaker form while leaving every shell reader
    /// stale.
    #[test]
    fn every_offered_field_is_classified_and_shell_fields_post_shell() {
        let layer = layer_with_in_node();
        let comp = comp_of(&layer).id;
        let shell = InvalidationHint::Shell {
            scopes: vec![scope(comp, layer.id)],
            params: Vec::new(),
        };
        let in_node = in_node_id(&layer).expect("the fixture network has an In node");

        for key in offered_field_keys(&layer) {
            let expected = if key.starts_with(CUSTOM_FIELD_PREFIX) {
                InvalidationHint::Params(vec![in_node])
            } else if STRUCTURAL_LAYER_FIELDS.contains(&key.as_str()) {
                InvalidationHint::Structural
            } else {
                shell.clone()
            };
            assert_eq!(
                layer_field_hint(&key, comp, &layer),
                expected,
                "{key} is classified wrongly"
            );
        }
    }

    /// The five that stay `Structural`, named one by one so that dropping
    /// one from the list is a failure rather than a silent reclassification
    /// the loop above would happily agree with.
    #[test]
    fn the_merge_chain_fields_stay_structural() {
        let layer = layer_with_in_node();
        let comp = comp_of(&layer).id;
        for key in ["blend_mode", "solo", "muted", "adjustment", "parent"] {
            assert_eq!(
                layer_field_hint(key, comp, &layer),
                InvalidationHint::Structural,
                "{key} stopped rebuilding the compiled chain"
            );
            assert!(
                STRUCTURAL_LAYER_FIELDS.contains(&key),
                "{key} left STRUCTURAL_LAYER_FIELDS"
            );
        }
        assert_eq!(
            STRUCTURAL_LAYER_FIELDS.len(),
            5,
            "a field joined or left the structural list without a reason here"
        );
    }

    /// A custom parameter is the layer network's In node, not the shell.
    #[test]
    fn a_custom_parameter_names_the_in_node() {
        let layer = layer_with_in_node();
        let comp = comp_of(&layer).id;
        let in_node = in_node_id(&layer).expect("the fixture network has an In node");
        assert_eq!(
            layer_field_hint(&format!("{CUSTOM_FIELD_PREFIX}amount"), comp, &layer),
            InvalidationHint::Params(vec![in_node])
        );
    }

    /// A Timeline row reaches the same two answers from its own vocabulary:
    /// a shell group scrubs the shell, a network row names its node.
    #[test]
    fn a_property_row_agrees_with_the_field_key_it_stands_for() {
        let layer = layer_with_in_node();
        let comp = comp_of(&layer).id;
        assert_eq!(
            property_row_hint(
                &PropertyRowId::Shell(PropertyGroup::Position),
                comp,
                layer.id
            ),
            layer_field_hint("position_x", comp, &layer),
            "the Timeline and the Properties panel disagree about a shell edit"
        );

        let node = NodeId::next();
        assert_eq!(
            property_row_hint(
                &PropertyRowId::Network {
                    node,
                    key: "radius".into()
                },
                comp,
                layer.id
            ),
            InvalidationHint::Params(vec![node])
        );
    }

    /// A bar gesture moving several layers posts one hint naming all of
    /// them, and an empty gesture posts nothing.
    #[test]
    fn a_multi_layer_bar_gesture_names_every_layer_it_moved() {
        let comp = CompId::next();
        let (a, b) = (LayerId::next(), LayerId::next());
        assert_eq!(
            shell_hint_for_layers(comp, [a, b, a]),
            InvalidationHint::Shell {
                scopes: vec![scope(comp, a), scope(comp, b)],
                params: Vec::new(),
            }
        );
        assert_eq!(
            shell_hint_for_layers(comp, []),
            InvalidationHint::None,
            "a gesture that moved nothing invalidated something"
        );
    }
}

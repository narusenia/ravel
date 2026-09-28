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
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::network as net;
    use ravel_core::runtime::ShellScope;

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

    fn scope(comp: CompId, layer: LayerId) -> ShellScope {
        ShellScope {
            comp,
            layer: Some(layer),
        }
    }

    /// `RESP-3` (#193): a shell field edit names the shell it touched and
    /// stays below `Structural`, which a transform scrub would otherwise pay
    /// once per mouse move.
    #[test]
    fn every_shell_field_posts_a_shell_hint_without_escalating() {
        let comp = CompId::next();
        let layer = layer_with_in_node();
        let expected = InvalidationHint::Shell {
            scopes: vec![scope(comp, layer.id)],
            params: Vec::new(),
        };
        for key in [
            "transform.position",
            "transform.rotation",
            "transform.scale",
            "transform.anchor",
            "opacity",
            "start_frame",
            "in_frame",
            "out_frame",
            "audio_gain",
            "name",
            "locked",
        ] {
            assert_eq!(
                layer_field_hint(key, comp, &layer),
                expected,
                "{key} did not post a Shell hint naming its own shell"
            );
        }
    }

    /// The two edits that are *not* shell hints, and why.
    #[test]
    fn merge_chain_fields_stay_structural_and_custom_params_stay_params() {
        let comp = CompId::next();
        let layer = layer_with_in_node();
        for key in STRUCTURAL_LAYER_FIELDS {
            assert_eq!(
                layer_field_hint(key, comp, &layer),
                InvalidationHint::Structural,
                "{key} stopped rebuilding the compiled chain"
            );
        }
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
        let comp = CompId::next();
        let layer = layer_with_in_node();
        assert_eq!(
            property_row_hint(
                &PropertyRowId::Shell(PropertyGroup::Position),
                comp,
                layer.id
            ),
            layer_field_hint("transform.position", comp, &layer),
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

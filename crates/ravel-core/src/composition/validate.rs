// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validation for Composition/Layer structures.
//!
//! Detects:
//! - PreComp circular references (A contains B which contains A)
//! - Layer parenting cycles within a Composition
//! - Layer Ref circular references within a Composition (REQ-LAYER-005)

use crate::composition::{Composition, Layer};
use crate::graph::{Graph, Node};
use crate::id::{CompId, LayerId, NodeId};
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

/// Type key of the Layer Info node (reads another layer's **shell** — its
/// placement, transform and opacity — without evaluating its network,
/// REQ-LAYER-002/005).
pub const LAYER_INFO_TYPE_KEY: &str = "layer.info";

/// Parameter on the Layer Info node holding the layer it reads. `-1` is the
/// owning layer itself, which is why this one has a spelling `layer.ref`'s
/// has not.
pub const LAYER_INFO_LAYER_PARAM: &str = "layer";

/// Type key of the Comp Info node (reads a composition's own fields —
/// resolution, frame rate, duration, background, layer count — without
/// evaluating anything, REQ-LAYER-002/005).
pub const COMP_INFO_TYPE_KEY: &str = "comp.info";

/// Parameter on the Comp Info node holding the composition it reads. `-1` is
/// the composition the node's network belongs to.
///
/// Spelled as **text** holding the decimal [`CompId`], like `layer.ref`'s and
/// `layer.info`'s target and unlike `precomp`'s [`PRECOMP_COMP_ID_PARAM`]:
/// what the user picks is a composition, and only a string parameter can
/// carry a labelled candidate list.
pub const COMP_INFO_COMP_PARAM: &str = "comp";

/// Every node type whose `layer` parameter names a layer by raw id.
///
/// The reservation in
/// [`Document::id_watermarks`](crate::composition::Document::id_watermarks)
/// is about the *stored number*, so it covers every such type: a reference
/// the composition no longer holds would otherwise have its id reallocated
/// and silently point at an unrelated layer.
///
/// [`layer_ref_targets`] is deliberately the narrower question, because the
/// other two readers of it are about **evaluation**: `layer.ref` pulls its
/// target's network (so it can form a cycle, and so a target's shell edit
/// must invalidate the referrer's scope), while `layer.info` reads shell
/// fields only. Its invalidation runs through
/// [`InvalidationHint::Shell`](crate::runtime::InvalidationHint::Shell)
/// instead, and the cycle it *can* form needs a shell binding on the other
/// side — [`validate_shell_bind_cycles`], not this list.
const LAYER_TARGET_TYPE_KEYS: &[&str] = &[LAYER_REF_TYPE_KEY, LAYER_INFO_TYPE_KEY];

/// Whether the parameter `param_key` on a node of type `type_key` names an
/// **identifier** rather than a value of its own — a reference read back
/// through
/// [`ParameterValue::identifier`](crate::graph::ParameterValue::identifier),
/// which is the one mouth every reader of one goes through
/// ([`node_asset_reference`](super::node_asset_reference) and evaluation
/// included).
///
/// Such a parameter cannot be animated, whatever its kind: a value that does
/// not stand still names **nothing**. The value between two keys is as real as
/// the keys, so a keyframed reference gives no finite answer to which ids
/// [`Document::id_watermarks`](crate::composition::Document::id_watermarks)
/// must reserve (REQ-LAYER-009), nor to which referencing scopes to invalidate
/// when a referenced layer's shell changes. Rather than refuse such a
/// document — one that saved and then cannot be opened is data loss — the
/// reference is dropped and
/// [`Document::dynamic_identifiers`](crate::composition::Document::dynamic_identifiers)
/// hands it to `ravel-cli render` to report.
///
/// Four parameters qualify, in the two spellings a raw id has:
///
/// - `precomp`'s `comp_id` is an `Int` holding a raw [`CompId`].
/// - `comp.info`'s `comp` is a **`String`** holding a raw [`CompId`], for the
///   same reason `layer.info`'s target is text: the picker labels its
///   candidates.
/// - `layer.ref`'s `layer` is a **`String`** holding a raw [`LayerId`] as
///   decimal digits (`.ravprj` v13 — see
///   [`layer_ref_upgrade`](super::layer_ref_upgrade)), so the Properties row
///   can offer named candidates.
/// - a media node's `asset_id` is a **`String`** holding a raw
///   [`AssetId`](crate::id::AssetId). Re-typing it to
///   `ParameterValue::StringSteps` used to make the watermark scan stop seeing
///   the reference while evaluation still sampled it per frame, so the next
///   minted `AssetId` could reuse an id a key still named — the
///   reference-reconnects-to-unrelated-footage bug the v9 asset-identity
///   format exists to have killed. Both sides read the same mouth now, so the
///   step curve names no asset anywhere: the node is simply offline.
///
/// It lives here — one predicate beside the constants it is made of — because
/// the same question asked in three places is a question that gets a different
/// answer in one of them the next time a reference node is added.
pub fn is_identifier_parameter(type_key: &str, param_key: &str) -> bool {
    if super::MEDIA_TYPE_KEYS.contains(&type_key) && param_key == super::MEDIA_ASSET_PARAM_KEY {
        return true;
    }
    matches!(
        (type_key, param_key),
        (PRECOMP_TYPE_KEY, PRECOMP_COMP_ID_PARAM)
            | (LAYER_REF_TYPE_KEY, LAYER_REF_LAYER_PARAM)
            | (LAYER_INFO_TYPE_KEY, LAYER_INFO_LAYER_PARAM)
            | (COMP_INFO_TYPE_KEY, COMP_INFO_COMP_PARAM)
    )
}

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

    #[error("circular shell binding in comp {comp:?}: {chain:?}")]
    CircularShellBinding { comp: CompId, chain: Vec<LayerId> },
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
                .and_then(|p| p.value.static_identifier())
                .map(CompId::new)
        })
        .collect()
}

/// Composition ids referenced by **any** composition-reading node inside a
/// network — `precomp` and `comp.info` — including nested subnet graphs. The
/// composition-valued twin of [`layer_target_ids`].
///
/// Used by [`Document::id_watermarks`](crate::composition::Document::id_watermarks)
/// so a fresh `CompId` can never land on an id a stored reference already
/// names. That matters most for a reference the composition table no longer
/// holds: allocating its id would reconnect the reference to an unrelated
/// composition, the same silent mis-link the asset watermark exists to
/// prevent — and `comp.info` would then read that unrelated composition's
/// resolution and frame rate with no error anywhere.
///
/// The two type keys are read through **different mouths** because they store
/// the id differently: `precomp`'s `comp_id` is an `Int`, `comp.info`'s
/// `comp` is decimal text. Asking the numeric one for a string parameter
/// answers `None` for every node, which is the shape this reservation exists
/// to prevent.
pub(crate) fn comp_target_ids(network: &Graph, targets: &mut Vec<CompId>) {
    for node in network.nodes() {
        let target = match node.type_key.as_str() {
            PRECOMP_TYPE_KEY => node
                .parameters
                .iter()
                .find(|p| p.key == PRECOMP_COMP_ID_PARAM)
                .and_then(|p| p.value.static_identifier()),
            COMP_INFO_TYPE_KEY => node
                .parameters
                .iter()
                .find(|p| p.key == COMP_INFO_COMP_PARAM)
                .and_then(|p| p.value.static_text_identifier()),
            _ => None,
        };
        if let Some(id) = target {
            targets.push(CompId::new(id));
        }
        if let Some(inner) = node.subnet.as_deref() {
            comp_target_ids(inner, targets);
        }
    }
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
///
/// Read through [`ParameterValue::static_text_identifier`] because the target
/// is spelled as **text** (`.ravprj` v13 — the `layer` parameter is a
/// `String` holding the decimal id so the picker can label its candidates).
/// The numeric mouth would answer `None` for every node, and three things
/// would then stop working with no error anywhere: cycles would pass
/// [`validate_layer_ref_cycles`], a fresh `LayerId` could land on a stored
/// reference ([`Document::id_watermarks`](crate::composition::Document::id_watermarks)),
/// and a referring scope would not be invalidated when its target's shell
/// moves.
///
/// [`ParameterValue::static_text_identifier`]: crate::graph::ParameterValue::static_text_identifier
pub(crate) fn layer_ref_targets(network: &Graph, targets: &mut Vec<LayerId>) {
    targets_of(network, &[LAYER_REF_TYPE_KEY], targets)
}

/// Layer ids referenced by **any** layer-reading node inside a network
/// ([`LAYER_TARGET_TYPE_KEYS`]), subnets included — the id reservation's
/// question, as opposed to [`layer_ref_targets`]'s evaluation one.
pub(crate) fn layer_target_ids(network: &Graph, targets: &mut Vec<LayerId>) {
    targets_of(network, LAYER_TARGET_TYPE_KEYS, targets)
}

fn targets_of(network: &Graph, type_keys: &[&str], targets: &mut Vec<LayerId>) {
    for node in network.nodes() {
        targets_of_node(node, type_keys, targets);
    }
}

/// [`targets_of`] for a single node and the subnet it owns. Split out so a
/// caller that has already decided *which* nodes matter — the shell-binding
/// walk below, which looks only at the ones feeding a bound channel — asks
/// the same question about them, subnets included.
fn targets_of_node(node: &Node, type_keys: &[&str], targets: &mut Vec<LayerId>) {
    if type_keys.contains(&node.type_key.as_str())
        && let Some(id) = node
            .parameters
            .iter()
            .find(|p| p.key == LAYER_REF_LAYER_PARAM)
            .and_then(|p| p.value.static_text_identifier())
            .map(LayerId::new)
    {
        targets.push(id);
    }
    if let Some(inner) = node.subnet.as_deref() {
        targets_of(inner, type_keys, targets);
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

    match first_layer_cycle(comp, &refs) {
        Some(chain) => Err(ValidationError::CircularLayerRef {
            comp: comp.id,
            chain,
        }),
        None => Ok(()),
    }
}

/// Check for circular **shell bindings** within a single composition
/// (REQ-LAYER-004): a layer whose shell is driven by a node output that is
/// fed, through its own network, by a `layer.info` reading a layer whose
/// shell is (transitively) driven back by this one.
///
/// ```text
/// A.transform ← a node in A's network ← layer.info(B).position
/// B.transform ← a node in B's network ← layer.info(A).position
/// ```
///
/// **No graph holds this cycle**, which is why it needs its own pass: the
/// shell binding is a [`ChannelSource::NodeOutput`] rather than an edge, so
/// `Graph::add_edge`'s cycle check never sees it, and the reference crosses
/// layers, so neither does the evaluator's per-graph re-entry guard.
/// [`validate_layer_ref_cycles`] does not see it either — `layer.ref` pulls a
/// *network*, `layer.info` reads a *shell*, and the loop here closes through
/// the shell.
///
/// The edge is drawn only from the bound node's **upstream cone**, not from
/// the whole network: a `layer.info` that feeds only the layer's picture,
/// beside an unrelated shell binding, is not a dependency of the shell, and
/// treating it as one would refuse a document that computes nothing circular.
/// The cone spans wire edges *and* the hidden `NodeOutput` parameter bindings
/// ([`Graph::downstream_adjacency`]), because a value reaches the bound node
/// through either.
///
/// Runs at the same validation layer as [`validate_layer_ref_cycles`] and
/// [`validate_precomp_cycles`], and shares their walk ([`first_layer_cycle`]).
///
/// # Why `comp.info` is not in this
///
/// `comp.info` reads [`Composition`]'s own fields — resolution, frame rate,
/// duration, background, layer count, name — and every one of them is a plain
/// stored value, not an [`AnimationChannel`]. Nothing a layer does can drive
/// one, so there is no path from a shell binding back to a composition field
/// and no cycle to detect. Adding it would draw edges that can never close.
///
/// [`ChannelSource::NodeOutput`]: crate::animation::channel::ChannelSource::NodeOutput
/// [`AnimationChannel`]: crate::animation::channel::AnimationChannel
pub fn validate_shell_bind_cycles(comp: &Composition) -> Result<(), ValidationError> {
    match first_shell_bind_cycle(comp) {
        Some(chain) => Err(ValidationError::CircularShellBinding {
            comp: comp.id,
            chain,
        }),
        None => Ok(()),
    }
}

/// The first shell-binding cycle in `comp`, as the chain that closes it.
///
/// The load-time repair calls this directly and in a loop: it clears the
/// chain's bindings and asks again, which is why the answer is the chain
/// rather than a [`ValidationError`].
pub(crate) fn first_shell_bind_cycle(comp: &Composition) -> Option<Vec<LayerId>> {
    let refs: HashMap<LayerId, Vec<LayerId>> = comp
        .layers
        .iter()
        .map(|layer| (layer.id, shell_bind_targets(layer)))
        .collect();
    first_layer_cycle(comp, &refs)
}

/// The layers whose **shell** `layer`'s shell reads — the edges
/// [`validate_shell_bind_cycles`] looks for a cycle in.
///
/// Empty unless the shell is bound to a node output at all, which is the
/// common case and costs one walk of the shell's channels.
fn shell_bind_targets(layer: &Layer) -> Vec<LayerId> {
    let bound: HashSet<NodeId> = layer
        .shell_parameter_sources()
        .into_iter()
        .map(|(node, _port)| node)
        .collect();
    if bound.is_empty() {
        return Vec::new();
    }

    // Flood *upstream* from the bound nodes once, rather than downstream from
    // each candidate reader: one inversion plus one traversal is linear in the
    // network, where asking each reader separately is quadratic in it.
    let mut upstream: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (source, targets) in layer.network.downstream_adjacency() {
        for target in targets {
            upstream.entry(target).or_default().push(source);
        }
    }
    let mut feeds_the_shell: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = bound.into_iter().collect();
    while let Some(id) = stack.pop() {
        if !feeds_the_shell.insert(id) {
            continue;
        }
        if let Some(sources) = upstream.get(&id) {
            stack.extend(sources.iter().copied());
        }
    }

    let mut targets = Vec::new();
    for node in layer.network.nodes() {
        if feeds_the_shell.contains(&node.id) {
            layer_info_targets_of_node(node, layer.id, &mut targets);
        }
    }
    targets
}

/// The layers a node's `layer.info` reads, the ones inside the subnet it owns
/// included, with `-1` resolved to `owner`.
///
/// A subnet is taken whole: whatever is inside it reaches the enclosing
/// network through the subnet node's outputs, so a subnet that feeds the
/// shell feeds it everything it reads. Same treatment [`targets_of_node`]
/// gives a subnet, and the reason the brief's "a `layer.info` nested in a
/// subnet still counts" holds.
fn layer_info_targets_of_node(node: &Node, owner: LayerId, targets: &mut Vec<LayerId>) {
    if node.type_key == LAYER_INFO_TYPE_KEY
        && let Some(target) = layer_info_target(node, owner)
    {
        targets.push(target);
    }
    if let Some(inner) = node.subnet.as_deref() {
        for inner_node in inner.nodes() {
            layer_info_targets_of_node(inner_node, owner, targets);
        }
    }
}

/// The layer a `layer.info` node reads, given the layer whose network it sits
/// in. `None` when it names none.
///
/// **Not [`layer_ref_targets`]'s reading**, and the difference is the whole
/// reason this exists: `layer.info`'s target defaults to `-1`, its own layer,
/// and `-1` is not a [`LayerId`] — `ParameterValue::static_text_identifier`
/// answers `None` for it. A shell bound to a node fed by `layer.info(-1)` is
/// a layer reading its own shell to compute its own shell, which is the
/// shortest cycle there is, so the spelling has to resolve here or the
/// self-reference passes.
///
/// Mirrors `layer_info::target_layer` in `ravel-nodes`, down to refusing `0`
/// (the reserved "no layer" id `eval::identifier_overlay` writes over a target
/// that does not stand still) and every unparsable spelling: a target the
/// processor refuses is a reference that resolves to nothing, so it is no
/// edge either.
fn layer_info_target(node: &Node, owner: LayerId) -> Option<LayerId> {
    let Some(parameter) = node
        .parameters
        .iter()
        .find(|p| p.key == LAYER_INFO_LAYER_PARAM)
    else {
        // The template writes `-1`; a node without the parameter reads the
        // same default through `ResolvedParams::str_or`.
        return Some(owner);
    };
    let crate::graph::ParameterValue::String(text) = &parameter.value else {
        return None;
    };
    match text.parse::<i64>() {
        Ok(-1) => Some(owner),
        Ok(raw) if raw > 0 => Some(LayerId::new(raw as u64)),
        _ => None,
    }
}

/// The first cycle in a layer → layers adjacency, as the chain that closes
/// it, or `None` when there is none.
///
/// Shared by [`validate_layer_ref_cycles`] and
/// [`validate_shell_bind_cycles`]: the two differ only in which edges they
/// hand it and which error they wrap the answer in. A second walk of its own
/// would be a second chance to disagree about what a cycle is — a self
/// reference in particular, which both must reject.
fn first_layer_cycle(
    comp: &Composition,
    refs: &HashMap<LayerId, Vec<LayerId>>,
) -> Option<Vec<LayerId>> {
    let mut visited = HashSet::new();
    for layer in comp.layers.iter() {
        let mut path = Vec::new();
        if let Some(chain) = layer_cycle_dfs(layer.id, refs, &mut path, &mut visited) {
            return Some(chain);
        }
    }
    None
}

fn layer_cycle_dfs(
    layer: LayerId,
    refs: &HashMap<LayerId, Vec<LayerId>>,
    path: &mut Vec<LayerId>,
    visited: &mut HashSet<LayerId>,
) -> Option<Vec<LayerId>> {
    if let Some(pos) = path.iter().position(|&l| l == layer) {
        let mut chain = path[pos..].to_vec();
        chain.push(layer);
        return Some(chain);
    }
    if visited.contains(&layer) {
        return None;
    }
    path.push(layer);
    if let Some(targets) = refs.get(&layer) {
        for &target in targets {
            if let Some(chain) = layer_cycle_dfs(target, refs, path, visited) {
                return Some(chain);
            }
        }
    }
    path.pop();
    visited.insert(layer);
    None
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
    /// The reference parameters, and nothing else. A key alone does not make a
    /// parameter an identifier — `scatter.grid` has a `count_x` that is a plain
    /// animatable count, and any node may name a parameter `layer`.
    #[test]
    fn only_the_reference_parameters_are_identifiers() {
        assert!(super::is_identifier_parameter("layer.ref", "layer"));
        assert!(super::is_identifier_parameter("precomp", "comp_id"));
        assert!(!super::is_identifier_parameter("layer.ref", "port"));
        assert!(!super::is_identifier_parameter("scatter.grid", "layer"));
        assert!(!super::is_identifier_parameter("precomp", "layer"));
    }

    /// A media node's `asset_id` is an identifier even though it is a
    /// `String`: `node_asset_reference` reads a plain `String` and nothing
    /// else, so animating it would hide the reference from the watermark scan.
    /// Every type key the asset reader accepts has to answer the same way, or
    /// the alias is the hole.
    #[test]
    fn a_media_asset_reference_is_an_identifier() {
        for type_key in super::super::MEDIA_TYPE_KEYS {
            assert!(
                super::is_identifier_parameter(type_key, super::super::MEDIA_ASSET_PARAM_KEY),
                "{type_key}'s asset reference must not be animatable"
            );
        }
        assert!(!super::is_identifier_parameter("media", "fit"));
        assert!(
            !super::is_identifier_parameter("shape.rect", super::super::MEDIA_ASSET_PARAM_KEY),
            "the key alone does not make a parameter a media reference"
        );
    }
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
    ///
    /// The target is a `String` holding the decimal id, as `.ravprj` v13
    /// stores it. The cycle tests below are what catches a
    /// [`layer_ref_targets`] that reads the numeric spelling instead: it would
    /// answer with no targets at all, every cycle would validate, and no other
    /// test would notice.
    fn layer_ref_layer(id: u64, node_id: u64, target: LayerId) -> Layer {
        let node = Node::new(NodeId::new(node_id), LAYER_REF_TYPE_KEY).with_param(
            LAYER_REF_LAYER_PARAM,
            ParameterValue::String(target.raw().to_string()),
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
            .with_param(LAYER_REF_LAYER_PARAM, ParameterValue::String("2".into()));
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
            Node::new(NodeId::new(node_id), LAYER_REF_TYPE_KEY).with_param(
                LAYER_REF_LAYER_PARAM,
                ParameterValue::String(target.to_string()),
            )
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

    // ---- Shell binding cycles -----------------------------------------------

    use crate::animation::channel::{AnimationChannel, ChannelSource};
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};

    /// A `layer.info` reading `target`, spelled as the text the picker
    /// writes. `"-1"` is the template default: the node's own layer.
    fn layer_info_node(node_id: u64, target: &str) -> Node {
        Node::new(NodeId::new(node_id), LAYER_INFO_TYPE_KEY)
            .with_param(
                LAYER_INFO_LAYER_PARAM,
                ParameterValue::String(target.to_string()),
            )
            .with_output("position", DataTypeId::VEC2)
    }

    /// A node that consumes a value and produces a scalar — the thing a shell
    /// channel can actually be bound to.
    fn driver_node(node_id: u64) -> Node {
        Node::new(NodeId::new(node_id), "math.scalar")
            .with_input("value", &[DataTypeId::VEC2])
            .with_output("out", DataTypeId::SCALAR)
    }

    /// Drive `layer`'s shell x position from node `node`'s first output.
    fn bind_shell(mut layer: Layer, node: u64) -> Layer {
        layer.transform.position[0] = AnimationChannel::new(ChannelSource::NodeOutput(
            NodeId::new(node),
            OutputPortIndex(0),
        ));
        layer
    }

    /// A layer whose shell is driven by a node wired downstream of a
    /// `layer.info(target)` — the shape the cycle is made of. Node ids are
    /// `base` (the reader) and `base + 1` (the driver the shell binds).
    fn shell_reader(id: u64, base: u64, target: &str) -> Layer {
        let network = Graph::new()
            .add_node(layer_info_node(base, target))
            .unwrap()
            .add_node(driver_node(base + 1))
            .unwrap()
            .add_edge(
                EdgeId::new(base + 2),
                NodeId::new(base),
                OutputPortIndex(0),
                NodeId::new(base + 1),
                InputPortIndex(0),
            )
            .unwrap();
        bind_shell(
            Layer::new(LayerId::new(id), format!("Shell {id}"), network),
            base + 1,
        )
    }

    /// Two layers whose shells drive each other through `layer.info`. No
    /// graph holds this cycle, so nothing but this pass rejects it: drop the
    /// shell binding from the edge set and this document validates.
    #[test]
    fn mutual_shell_bindings_are_a_cycle() {
        let comp = comp(1)
            .add_layer(shell_reader(1, 100, "2"))
            .add_layer(shell_reader(2, 200, "1"));
        let err = validate_shell_bind_cycles(&comp).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::CircularShellBinding { chain, .. }
                if chain == vec![LayerId::new(1), LayerId::new(2), LayerId::new(1)]
        ));
    }

    #[test]
    fn shell_bindings_across_three_layers_are_a_cycle() {
        let comp = comp(1)
            .add_layer(shell_reader(1, 100, "2"))
            .add_layer(shell_reader(2, 200, "3"))
            .add_layer(shell_reader(3, 300, "1"));
        assert!(validate_shell_bind_cycles(&comp).is_err());
    }

    /// One layer reading another's shell is the ordinary use of the feature,
    /// and the pass must not refuse it.
    #[test]
    fn a_one_way_shell_binding_is_not_a_cycle() {
        let comp = comp(1)
            .add_layer(shell_reader(1, 100, "2"))
            .add_layer(empty_layer(2));
        assert!(validate_shell_bind_cycles(&comp).is_ok());
    }

    /// A layer whose shell is driven by a node fed by a reader of **its own**
    /// shell is the shortest cycle there is — and the one a picker offering
    /// `-1` makes easiest to build.
    #[test]
    fn a_shell_reading_its_own_layer_is_a_cycle() {
        let comp = comp(1).add_layer(shell_reader(1, 100, "-1"));
        let err = validate_shell_bind_cycles(&comp).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::CircularShellBinding { chain, .. }
                if chain == vec![LayerId::new(1), LayerId::new(1)]
        ));
    }

    /// The reader nested inside a subnet still feeds the shell: its value
    /// leaves through the subnet node's output like any other.
    #[test]
    fn a_reader_inside_a_subnet_still_feeds_the_shell() {
        let inner = Graph::new().add_node(layer_info_node(100, "2")).unwrap();
        let network = Graph::new()
            .add_node(Node::new(NodeId::new(101), "subnet").with_subnet(inner))
            .unwrap();
        let comp = comp(1)
            .add_layer(bind_shell(Layer::new(LayerId::new(1), "Sub", network), 101))
            .add_layer(shell_reader(2, 200, "1"));
        assert!(validate_shell_bind_cycles(&comp).is_err());
    }

    /// A `layer.info` that feeds only the layer's **picture** is not a
    /// dependency of its shell, even when the shell is separately bound to
    /// some other node. Reading the whole network instead of the bound node's
    /// upstream cone would refuse this pair, which computes nothing
    /// circular.
    #[test]
    fn a_reader_that_does_not_feed_the_shell_is_no_edge() {
        // Each layer: an unconnected `layer.info` naming the other, plus a
        // shell bound to a driver nothing feeds.
        let unrelated = |id: u64, base: u64, target: &str| {
            let network = Graph::new()
                .add_node(layer_info_node(base, target))
                .unwrap()
                .add_node(driver_node(base + 1))
                .unwrap();
            bind_shell(
                Layer::new(LayerId::new(id), format!("Free {id}"), network),
                base + 1,
            )
        };
        let comp = comp(1)
            .add_layer(unrelated(1, 100, "2"))
            .add_layer(unrelated(2, 200, "1"));
        assert!(validate_shell_bind_cycles(&comp).is_ok());
    }

    /// The cone spans the hidden `NodeOutput` parameter bindings as well as
    /// wires: a reader that drives a node's *parameter* reaches the shell
    /// just as surely as one wired into its input.
    #[test]
    fn a_reader_bound_to_a_parameter_feeds_the_shell() {
        let driven = Node::new(NodeId::new(101), "math.scalar")
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                    NodeId::new(100),
                    OutputPortIndex(0),
                ))),
            )
            .with_output("out", DataTypeId::SCALAR);
        let network = Graph::new()
            .add_node(layer_info_node(100, "2"))
            .unwrap()
            .add_node(driven)
            .unwrap();
        let comp = comp(1)
            .add_layer(bind_shell(
                Layer::new(LayerId::new(1), "Param", network),
                101,
            ))
            .add_layer(shell_reader(2, 200, "1"));
        assert!(validate_shell_bind_cycles(&comp).is_err());
    }

    /// `layer.ref` is not a shell reader, so a `layer.ref` cycle is not this
    /// pass's business — and a shell-binding cycle is not
    /// [`validate_layer_ref_cycles`]'s. Neither pass may answer the other's
    /// question.
    #[test]
    fn the_two_cycle_passes_do_not_answer_for_each_other() {
        let shell_cycle = comp(1)
            .add_layer(shell_reader(1, 100, "2"))
            .add_layer(shell_reader(2, 200, "1"));
        assert!(validate_layer_ref_cycles(&shell_cycle).is_ok());

        let ref_cycle = comp(1)
            .add_layer(layer_ref_layer(1, 100, LayerId::new(2)))
            .add_layer(layer_ref_layer(2, 200, LayerId::new(1)));
        assert!(validate_shell_bind_cycles(&ref_cycle).is_ok());
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

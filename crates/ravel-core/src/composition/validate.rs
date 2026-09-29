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
use crate::id::{CompId, LayerId, NodeId, OutputPortIndex};
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
        Some(cycle) => Err(ValidationError::CircularShellBinding {
            comp: comp.id,
            chain: cycle.chain,
        }),
        None => Ok(()),
    }
}

/// The first shell-binding cycle in `comp`, as the chain that closes it.
///
/// The load-time repair calls this directly and in a loop: it clears the
/// chain's bindings and asks again, which is why the answer is the chain
/// rather than a [`ValidationError`].
pub(crate) fn first_shell_bind_cycle(comp: &Composition) -> Option<ShellBindCycle> {
    shell_bind_cycle(comp, false)
}

/// [`first_shell_bind_cycle`] restricted to the edges the graph states
/// **exactly** — the ones the load-time repair is allowed to act on.
pub(crate) fn first_exact_shell_bind_cycle(comp: &Composition) -> Option<ShellBindCycle> {
    shell_bind_cycle(comp, true)
}

/// One shell-binding cycle, with the bindings that carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellBindCycle {
    /// The layers it runs through, the first repeated last.
    pub chain: Vec<LayerId>,
    /// The shell bindings on the cycle's own edges — `(layer, node, port)`.
    ///
    /// **Not every binding the chain's layers hold.** A layer may drive its
    /// position from one node and its opacity from another; only the one
    /// whose upstream reaches the next layer of the chain is part of the
    /// cycle, and dropping the other would delete work the user did for a
    /// problem it is not part of.
    pub bindings: Vec<(LayerId, NodeId, OutputPortIndex)>,
    /// Whether every edge of the chain is one [`readers_feeding`] could
    /// follow exactly. A cycle that rests on an approximated edge may not be
    /// a cycle at all, so the load-time repair leaves it alone.
    pub exact: bool,
}

fn shell_bind_cycle(comp: &Composition, exact_only: bool) -> Option<ShellBindCycle> {
    let edges: HashMap<LayerId, Vec<ShellBindEdge>> = comp
        .layers
        .iter()
        .map(|layer| {
            let mut found = shell_bind_edges(layer);
            if exact_only {
                found.retain(|edge| edge.exact);
            }
            (layer.id, found)
        })
        .collect();
    let refs: HashMap<LayerId, Vec<LayerId>> = edges
        .iter()
        .map(|(id, found)| (*id, found.iter().map(|edge| edge.target).collect()))
        .collect();
    let chain = first_layer_cycle(comp, &refs)?;

    // Back from the chain to the bindings that carry it. `chain` repeats its
    // first layer last, so every edge of the cycle is one window.
    let mut bindings = Vec::new();
    let mut exact = true;
    for step in chain.windows(2) {
        let (from, to) = (step[0], step[1]);
        let on_the_edge: Vec<&ShellBindEdge> = edges
            .get(&from)
            .into_iter()
            .flatten()
            .filter(|edge| edge.target == to)
            .collect();
        // One exact edge is enough to make the step real, and then the
        // approximated ones beside it say nothing extra.
        let mut certain = on_the_edge.iter().filter(|edge| edge.exact).peekable();
        let chosen: Vec<&ShellBindEdge> = if certain.peek().is_some() {
            certain.copied().collect()
        } else {
            exact = false;
            on_the_edge
        };
        bindings.extend(
            chosen
                .into_iter()
                .map(|edge| (from, edge.binding.0, edge.binding.1)),
        );
    }
    bindings.sort();
    bindings.dedup();
    Some(ShellBindCycle {
        chain,
        bindings,
        exact,
    })
}

/// One way a layer's shell depends on another layer's shell: a shell binding
/// whose upstream reaches a `layer.info` naming `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShellBindEdge {
    target: LayerId,
    /// The shell binding that carries it — the node output a shell channel
    /// names.
    binding: (NodeId, OutputPortIndex),
    /// Whether the path from the binding to the reader is one the graph
    /// states, as opposed to one [`readers_feeding`] had to assume.
    exact: bool,
}

/// The shell-to-shell dependencies `layer`'s own shell bindings create —
/// the edges [`validate_shell_bind_cycles`] looks for a cycle in.
///
/// Empty unless the shell is bound to a node output at all, which is the
/// common case and costs one walk of the shell's channels. One walk of the
/// network **per binding**, because each edge has to name the binding that
/// carries it or the repair cannot drop that one and keep the others; a
/// layer holds a handful of shell channels, so the factor is bounded by the
/// shell, not by the network.
fn shell_bind_edges(layer: &Layer) -> Vec<ShellBindEdge> {
    let mut edges: Vec<ShellBindEdge> = Vec::new();
    for binding in layer.shell_parameter_sources() {
        let mut reached = Vec::new();
        readers_feeding(
            &layer.network,
            vec![(binding.0, binding.1, true)],
            layer.id,
            &mut reached,
        );
        for (target, exact) in reached {
            // One binding can reach one reader by two paths, and a reader
            // answers at every output port it declares. Keep the best answer
            // per (binding, target): an exact path found anywhere makes the
            // edge exact.
            match edges
                .iter_mut()
                .find(|edge| edge.target == target && edge.binding == binding)
            {
                Some(existing) => existing.exact |= exact,
                None => edges.push(ShellBindEdge {
                    target,
                    binding,
                    exact,
                }),
            }
        }
    }
    edges
}

/// Flood **upstream** inside `graph` from `seeds` — `(node, output port,
/// whether the path so far is one the graph states)` — collecting the layers
/// the `layer.info` nodes it reaches read, each with the exactness of the
/// path that found it.
///
/// # Why the output port is carried
///
/// "Which inputs feed which output" is a question the model answers for
/// exactly one node kind. [`OutputPort`](crate::graph::OutputPort) carries a
/// name and a type and nothing about where its value comes from, so for an
/// ordinary node every input has to be followed. A **subnet** is different:
/// its output port `N` *is* its inner `net.out` node's input port `N`
/// ([`network::subnet_pins`](crate::network::subnet_pins) builds the list
/// from that node's inputs, in order), so the walk descends into the inner
/// graph from that one input and sees only what actually leaves through the
/// port the shell named. Without it, a `layer.info` sitting in a subnet
/// branch that feeds a *different* output — or no output at all — would be
/// read as a dependency of the shell, and the load-time repair would delete
/// a binding over a cycle that does not exist.
///
/// # What is approximated, and what that costs
///
/// Two steps cannot be taken exactly, and both are marked rather than
/// guessed, because a wrong "yes" here deletes a user's binding while a wrong
/// "no" only leaves a shell reading the zero it already reads:
///
/// - an ordinary node with **more than one output**: its inputs are followed
///   for whichever output was asked about.
/// - leaving a subnet through its **input** pins: the pin list drops the
///   `net.in` node's fixed ports, so inner output index and outer input index
///   do not line up. The subnet's outer edges are followed like any other
///   node's instead.
///
/// An edge that rests on either is still reported — [`validate_shell_bind_cycles`]
/// is the strict question — but [`Document::break_shell_bind_cycles`] will
/// not act on it.
///
/// [`Document::break_shell_bind_cycles`]: crate::composition::Document::break_shell_bind_cycles
fn readers_feeding(
    graph: &Graph,
    seeds: Vec<(NodeId, OutputPortIndex, bool)>,
    owner: LayerId,
    out: &mut Vec<(LayerId, bool)>,
) -> Vec<(OutputPortIndex, bool)> {
    // Built once per graph: the walk asks for a node's incoming edges as
    // often as it has nodes, and scanning the edge list each time would make
    // a validation pass quadratic in a network the user can make as large as
    // they like.
    // The target port is kept: a subnet walk needs the edges landing on one
    // pin, and re-scanning the edge list for them would put the quadratic
    // cost straight back.
    let mut incoming: HashMap<NodeId, Vec<(NodeId, OutputPortIndex, u32)>> = HashMap::new();
    for edge in graph.edges() {
        incoming.entry(edge.target).or_default().push((
            edge.source,
            edge.source_port,
            edge.target_port.0,
        ));
    }

    let mut reached_in_ports = Vec::new();
    let mut seen: HashMap<(NodeId, OutputPortIndex), bool> = HashMap::new();
    let mut stack = seeds;
    while let Some((id, port, exact)) = stack.pop() {
        // Re-expand only when this arrival is exact and the last one was
        // not: the exactness of a reader is the best path that found it.
        match seen.get(&(id, port)) {
            Some(&before) if before || !exact => continue,
            _ => {
                seen.insert((id, port), exact);
            }
        }
        let Some(node) = graph.node(id) else {
            continue;
        };

        if node.type_key == LAYER_INFO_TYPE_KEY
            && let Some(target) = layer_info_target(node, owner)
        {
            out.push((target, exact));
        }

        // This network's own boundary: the value on this port came in from
        // whatever encloses the graph. An enclosing subnet walk reads these
        // back to decide which of its outer pins it has to follow.
        if crate::network::is_in_node(node) {
            reached_in_ports.push((port, exact));
        }

        if let Some(inner) = node.subnet.as_deref() {
            let in_step = subnet_pins_in_step(node, inner);
            let out_node = crate::network::find_out_node(inner);

            // Descend from the one inner input the chosen output *is*, but
            // only while the stored pins still say what the inner graph
            // says. Out of step, the index means something else than this
            // assumes, and every seed goes in as an approximation instead.
            let inner_seeds: Vec<(NodeId, OutputPortIndex, bool)> = match (in_step, out_node) {
                (true, Some(out_node)) => inner
                    .edges()
                    .filter(|edge| edge.target == out_node.id && edge.target_port.0 == port.0)
                    .map(|edge| (edge.source, edge.source_port, exact))
                    .collect(),
                (false, Some(out_node)) => inner
                    .edges()
                    .filter(|edge| edge.target == out_node.id)
                    .map(|edge| (edge.source, edge.source_port, false))
                    .collect(),
                (_, None) => Vec::new(),
            };
            let reached = readers_feeding(inner, inner_seeds, owner, out);

            if in_step {
                // Back out through the pins the descent actually arrived at.
                // A subnet's input pin `M` is the inner `net.in` node's
                // `M`-th **non-fixed** output — `network::subnet_pins` builds
                // the list by dropping the fixed ones, and this asks
                // `network::is_fixed_port` the same question rather than
                // assuming a 1:1 that is not there. An inner port that *is*
                // fixed (`base_geometry`, `t`, `f`, `source`) is fed by the
                // shell, not by the enclosing network, so it ends the walk.
                //
                // Every other outer pin is left alone, and that is the point:
                // a `layer.info` wired into a pin whose value never reaches
                // the output the shell bound is not a dependency of that
                // shell, exactly as one wired to nothing inside the subnet is
                // not.
                let in_node = crate::network::find_in_node(inner);
                for (inner_port, inner_exact) in reached {
                    let Some(outer) =
                        in_node.and_then(|in_node| outer_pin_of_in_port(in_node, inner_port))
                    else {
                        continue;
                    };
                    for (source, source_port, target_port) in
                        incoming.get(&id).into_iter().flatten()
                    {
                        if *target_port == outer {
                            stack.push((*source, *source_port, exact && inner_exact));
                        }
                    }
                }
                // A promoted parameter reaches the inner In node the same way
                // a pin does, and which output it ends up feeding is the
                // question this branch just answered for pins. It is not
                // answered for parameters — the promotion is keyed by name,
                // not by port index — so they go in approximated.
                for (source, source_port) in node.parameter_sources() {
                    stack.push((source, source_port, false));
                }
                continue;
            }
        }

        // Which of an ordinary node's inputs feed which of its outputs is not
        // something the model states: `OutputPort` carries a name and a type
        // and nothing about where its value comes from. So every input is
        // followed, and that is exact only while the node has one output to
        // be the answer.
        let exact = exact && node.outputs.len() <= 1;
        for (source, source_port, _) in incoming.get(&id).into_iter().flatten() {
            stack.push((*source, *source_port, exact));
        }
        for (source, source_port) in node.parameter_sources() {
            stack.push((source, source_port, exact));
        }
    }
    reached_in_ports
}

/// Whether `node`'s stored pins are the ones its inner graph defines.
///
/// [`network::sync_subnet_pins`](crate::network::sync_subnet_pins) derives
/// them and a load runs it before this pass ever sees the document, so the
/// answer is normally yes. It is asked anyway because the edit-time caller
/// has no such guarantee, and a pin index that means something other than
/// what the walk assumes is precisely the guess this pass must not make: it
/// would follow the wrong branch of a subnet and delete a binding over a
/// cycle that is not there.
fn subnet_pins_in_step(node: &Node, inner: &Graph) -> bool {
    let Some((inputs, outputs)) = crate::network::subnet_pins(inner) else {
        return false;
    };
    inputs.len() == node.inputs.len()
        && outputs.len() == node.outputs.len()
        && inputs
            .iter()
            .zip(node.inputs.iter())
            .all(|(derived, stored)| derived.name == stored.name)
        && outputs
            .iter()
            .zip(node.outputs.iter())
            .all(|(derived, stored)| derived.name == stored.name)
}

/// The enclosing subnet node's input pin index that an inner `net.in` output
/// port arrives on, or `None` when that port is a fixed one the shell feeds
/// and no outer pin carries.
///
/// The inverse of the filter in
/// [`network::subnet_pins`](crate::network::subnet_pins), asked through the
/// same [`network::is_fixed_port`](crate::network::is_fixed_port), so the two
/// cannot drift into disagreeing about which output is a pin.
fn outer_pin_of_in_port(in_node: &Node, inner_port: OutputPortIndex) -> Option<u32> {
    let index = inner_port.0 as usize;
    let port = in_node.outputs.get(index)?;
    if crate::network::is_fixed_port(in_node, crate::graph::PortSide::Output, &port.name) {
        return None;
    }
    let pin = in_node.outputs[..index]
        .iter()
        .filter(|earlier| {
            !crate::network::is_fixed_port(in_node, crate::graph::PortSide::Output, &earlier.name)
        })
        .count();
    Some(pin as u32)
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

    /// A subnet node with `ports` outputs and `pins` input pins, whose inner
    /// graph holds a `layer.info(target)` wired into the inner `net.out`
    /// node's input `wired_to` — or into nothing, when that is `None`.
    ///
    /// The inner graph carries a real `net.in` node and the stored pins are
    /// the ones `network::subnet_pins` derives from it, because the walk
    /// refuses to trust an index on a subnet whose pins have drifted. A
    /// fixture without an In node would exercise that refusal instead of the
    /// port mapping it is here to test.
    fn subnet_with_reader(
        node_id: u64,
        info_id: u64,
        out_id: u64,
        target: &str,
        ports: usize,
        wired_to: Option<usize>,
    ) -> Node {
        subnet_fixture(node_id, info_id, out_id, target, ports, wired_to, 0, None)
    }

    /// [`subnet_with_reader`] with the reader wired to an inner **In** pin
    /// instead of straight to the Out node, plus a bridge from one inner pin
    /// to one inner Out input.
    ///
    /// `pins` input pins exist; the reader drives pin `reader_pin`. The
    /// bridge, when `bridge` is `Some((pin, out_input))`, carries pin `pin`
    /// through to Out input `out_input` — that is what decides whether the
    /// pin the reader drives reaches the output the shell bound.
    #[allow(clippy::too_many_arguments)]
    fn subnet_fixture(
        node_id: u64,
        info_id: u64,
        out_id: u64,
        target: &str,
        ports: usize,
        wired_to: Option<usize>,
        pins: usize,
        bridge: Option<(usize, usize)>,
    ) -> Node {
        let in_id = out_id + 100;
        let mut in_node = Node::new(NodeId::new(in_id), crate::network::NET_IN_TYPE_KEY);
        let mut out_node = Node::new(NodeId::new(out_id), crate::network::NET_OUT_TYPE_KEY);
        let mut subnet = Node::new(NodeId::new(node_id), "subnet");
        for index in 0..pins {
            in_node = in_node.with_output(format!("pin{index}"), DataTypeId::VEC2);
            subnet = subnet.with_input(format!("pin{index}"), &[DataTypeId::VEC2]);
        }
        for index in 0..ports {
            out_node = out_node.with_input(format!("p{index}"), &[DataTypeId::VEC2]);
            subnet = subnet.with_output(format!("p{index}"), DataTypeId::SCALAR);
        }
        let mut inner = Graph::new()
            .add_node(layer_info_node(info_id, target))
            .unwrap()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap();
        if let Some(index) = wired_to {
            inner = inner
                .add_edge(
                    EdgeId::new(out_id + 1),
                    NodeId::new(info_id),
                    OutputPortIndex(0),
                    NodeId::new(out_id),
                    InputPortIndex(index as u32),
                )
                .unwrap();
        }
        if let Some((pin, out_input)) = bridge {
            inner = inner
                .add_edge(
                    EdgeId::new(out_id + 2),
                    NodeId::new(in_id),
                    OutputPortIndex(pin as u32),
                    NodeId::new(out_id),
                    InputPortIndex(out_input as u32),
                )
                .unwrap();
        }
        subnet.with_subnet(inner)
    }

    /// One layer bound to a subnet's output, and layer 2 reading it back.
    fn comp_with_subnet(subnet: Node) -> Composition {
        let node_id = subnet.id.raw();
        let network = Graph::new().add_node(subnet).unwrap();
        comp(1)
            .add_layer(bind_shell(
                Layer::new(LayerId::new(1), "Sub", network),
                node_id,
            ))
            .add_layer(shell_reader(2, 200, "1"))
    }

    /// A reader inside a subnet, wired to the output the shell bound, feeds
    /// the shell: its value leaves through that output like any other.
    #[test]
    fn a_reader_wired_to_the_bound_subnet_output_feeds_the_shell() {
        let comp = comp_with_subnet(subnet_with_reader(101, 100, 102, "2", 1, Some(0)));
        assert!(validate_shell_bind_cycles(&comp).is_err());
    }

    /// The same subnet, with the reader wired to **nothing**. Its value
    /// leaves the subnet nowhere, so the shell does not depend on it — and a
    /// walk that took a subnet whole would delete the user's binding over a
    /// cycle that does not exist.
    #[test]
    fn a_reader_that_reaches_no_subnet_output_is_no_edge() {
        let comp = comp_with_subnet(subnet_with_reader(101, 100, 102, "2", 1, None));
        assert!(validate_shell_bind_cycles(&comp).is_ok());
    }

    /// One node, two independent outputs: the shell binds port 0 and the
    /// reader only reaches port 1. Node-level adjacency cannot tell those
    /// apart; a subnet's ports the graph can, so this is where the walk has
    /// to keep the port it was asked about.
    #[test]
    fn a_reader_on_another_output_of_the_bound_node_is_no_edge() {
        let comp = comp_with_subnet(subnet_with_reader(101, 100, 102, "2", 2, Some(1)));
        assert!(
            validate_shell_bind_cycles(&comp).is_ok(),
            "the shell bound output 0; the reader only reaches output 1"
        );
    }

    /// And the control: the same two-output subnet with the reader on the
    /// port the shell actually bound.
    #[test]
    fn a_reader_on_the_bound_output_of_a_two_output_node_is_an_edge() {
        let comp = comp_with_subnet(subnet_with_reader(101, 100, 102, "2", 2, Some(0)));
        assert!(validate_shell_bind_cycles(&comp).is_err());
    }

    /// A layer whose shell is bound to a subnet's output, with a
    /// `layer.info` wired into one of the subnet's **input pins** from
    /// outside.
    ///
    /// `bridge` decides whether that pin reaches the output the shell bound.
    fn comp_with_pin_reader(
        pins: usize,
        reader_pin: usize,
        bridge: Option<(usize, usize)>,
    ) -> Composition {
        let subnet = subnet_fixture(101, 999, 102, "2", 1, None, pins, bridge);
        // The reader sits in the *outer* network and drives one pin.
        let network = Graph::new()
            .add_node(layer_info_node(100, "2"))
            .unwrap()
            .add_node(subnet)
            .unwrap()
            .add_edge(
                EdgeId::new(103),
                NodeId::new(100),
                OutputPortIndex(0),
                NodeId::new(101),
                InputPortIndex(reader_pin as u32),
            )
            .unwrap();
        comp(1)
            .add_layer(bind_shell(Layer::new(LayerId::new(1), "Pin", network), 101))
            .add_layer(shell_reader(2, 200, "1"))
    }

    /// The input-side mirror of
    /// [`a_reader_that_reaches_no_subnet_output_is_no_edge`]: a reader wired
    /// into a subnet pin whose value never reaches the output the shell
    /// bound. Following every outer pin of a one-output subnet would make
    /// this an **exact** edge and let the load delete a binding over a cycle
    /// that does not exist.
    #[test]
    fn a_reader_on_a_subnet_pin_that_reaches_no_output_is_not_repairable() {
        let comp = comp_with_pin_reader(1, 0, None);
        assert!(
            first_exact_shell_bind_cycle(&comp).is_none(),
            "nothing carries pin 0 to the bound output, so no repair may act on it"
        );
    }

    /// The control: the same subnet with the pin carried through to the
    /// output the shell bound. The dependency is real, and the walk states
    /// it — `network::subnet_pins` derives the pin from the inner In node's
    /// non-fixed outputs, so the index mapping is read, not assumed.
    #[test]
    fn a_reader_on_a_subnet_pin_that_reaches_the_bound_output_is_an_exact_edge() {
        let comp = comp_with_pin_reader(1, 0, Some((0, 0)));
        assert!(validate_shell_bind_cycles(&comp).is_err());
        assert!(
            first_exact_shell_bind_cycle(&comp).is_some(),
            "the pin reaches the bound output through the inner graph"
        );
    }

    /// Two pins on one subnet, one of which reaches the bound output and one
    /// of which reaches nothing: the reader on the **dead** pin is not a
    /// dependency of the shell.
    ///
    /// This is what pins the pin filter itself. The single-pin test beside it
    /// cannot: there, no inner port reaches the output at all, so the walk
    /// never gets as far as choosing between pins and the filter is never
    /// asked. Only a subnet where one pin lives and another does not tells
    /// the two apart — and getting it wrong is a binding deleted over a cycle
    /// that is not there.
    #[test]
    fn a_reader_on_a_dead_pin_beside_a_live_one_is_no_edge() {
        // Pin 0 is carried through to the output the shell bound; pin 1 ends
        // in the subnet. The reader drives pin 1.
        let comp = comp_with_pin_reader(2, 1, Some((0, 0)));
        assert!(
            validate_shell_bind_cycles(&comp).is_ok(),
            "pin 1 reaches no output, so the layer it reads is not a dependency"
        );
        assert!(first_exact_shell_bind_cycle(&comp).is_none());

        // The mirror, same subnet: the reader on the pin that *is* carried
        // through. Without this the test above would also pass on a walk
        // that had simply stopped following pins.
        let live = comp_with_pin_reader(2, 0, Some((0, 0)));
        assert!(validate_shell_bind_cycles(&live).is_err());
        assert!(
            first_exact_shell_bind_cycle(&live).is_some(),
            "pin 0 reaches the bound output, and the walk states the path"
        );
    }

    /// A subnet whose stored pins have drifted from its inner graph: the
    /// index on an edge no longer means what the walk would assume, so the
    /// walk refuses to read it and the cycle becomes unrepairable.
    ///
    /// Here the node stores one output while the inner Out node declares
    /// two, so "output 0" names a different inner input under each reading.
    /// Trusting the stored list would descend the wrong branch, reach the
    /// reader, and call the edge exact — a binding deleted over an index
    /// nobody checked. A load runs `sync_subnet_pins` first, so this is the
    /// edit-time path's problem; the check costs one derivation.
    #[test]
    fn a_subnet_whose_pins_drifted_is_not_repairable() {
        let out_node = Node::new(NodeId::new(102), crate::network::NET_OUT_TYPE_KEY)
            .with_input("px", &[DataTypeId::VEC2])
            .with_input("p0", &[DataTypeId::VEC2]);
        let inner = Graph::new()
            .add_node(layer_info_node(100, "2"))
            .unwrap()
            .add_node(Node::new(NodeId::new(202), crate::network::NET_IN_TYPE_KEY))
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(103),
                NodeId::new(100),
                OutputPortIndex(0),
                NodeId::new(102),
                InputPortIndex(0),
            )
            .unwrap();
        // One stored output against the inner Out node's two: drift.
        let subnet = Node::new(NodeId::new(101), "subnet")
            .with_output("p0", DataTypeId::SCALAR)
            .with_subnet(inner);
        let network = Graph::new().add_node(subnet).unwrap();
        let comp = comp(1)
            .add_layer(bind_shell(
                Layer::new(LayerId::new(1), "Drifted", network),
                101,
            ))
            .add_layer(shell_reader(2, 200, "1"));

        assert!(
            first_exact_shell_bind_cycle(&comp).is_none(),
            "an index the graph does not vouch for must not drive a repair"
        );
    }

    /// A path through an ordinary node with several outputs cannot be
    /// followed exactly — nothing in the model says which input feeds which
    /// output — so the cycle is **reported** and left for the user, and the
    /// load-time repair does not act on it.
    #[test]
    fn a_cycle_through_a_multi_output_node_is_reported_but_not_repairable() {
        let splitter = Node::new(NodeId::new(101), "math.scalar")
            .with_input("value", &[DataTypeId::VEC2])
            .with_output("a", DataTypeId::SCALAR)
            .with_output("b", DataTypeId::SCALAR);
        let network = Graph::new()
            .add_node(layer_info_node(100, "2"))
            .unwrap()
            .add_node(splitter)
            .unwrap()
            .add_edge(
                EdgeId::new(102),
                NodeId::new(100),
                OutputPortIndex(0),
                NodeId::new(101),
                InputPortIndex(0),
            )
            .unwrap();
        let comp = comp(1)
            .add_layer(bind_shell(
                Layer::new(LayerId::new(1), "Split", network),
                101,
            ))
            .add_layer(shell_reader(2, 200, "1"));

        assert!(
            validate_shell_bind_cycles(&comp).is_err(),
            "detection is the strict question: the cycle is named"
        );
        assert!(
            first_exact_shell_bind_cycle(&comp).is_none(),
            "repair is the lenient one: a guess must not delete a binding"
        );
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

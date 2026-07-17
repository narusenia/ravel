// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hybrid Pull + Dirty Notification DAG evaluation engine.
//!
//! The engine implements the model described in
//! `docs/specifications/architecture.md` (REQ-CORE-002), extended for the
//! layer-network model (REQ-LAYER-007):
//!
//! ```text
//! parameter change
//!     │  push: mark the node and everything downstream dirty
//!     ▼
//! output node pull request
//!     │  recursively evaluate inputs first (depth-first)
//!     ▼
//! per node:
//!     dirty == false && cache valid → return cached value
//!     dirty == true  || cache stale → run `process` → cache → clear dirty
//! ```
//!
//! Network scopes (REQ-LAYER-007/009): a node may evaluate another graph
//! (a layer's network, a subnet) through [`EvalScope::evaluate_sub`]. Each
//! such nested evaluation pushes a [`PathSegment`] onto the current path;
//! cache and dirty state are keyed by the full path (`CompId / LayerId /
//! [SubnetNodeId ...] / NodeId`), so the same inner graph evaluated through
//! different owners keeps independent results. Contexts are scoped: the
//! caller passes a rewritten [`EvalContext`] (e.g. layer-local time) which
//! only the nested evaluation sees.
//!
//! Key properties guaranteed by [`Evaluator::evaluate`]:
//!
//! * **Diamond de-duplication** — a node reached through multiple paths in a
//!   single pull is processed at most once (per-run memoization).
//! * **Cycle safety** — a cyclic graph produces [`EvalError::CycleDetected`]
//!   instead of overflowing the stack; re-entering the same network scope
//!   recursively (A → B → A through Layer Ref / PreComp) is likewise rejected.
//! * **Selective re-evaluation** — clean nodes whose inputs did not change are
//!   served from cache; only time-dependent nodes (and their downstream) are
//!   re-evaluated when the [`EvalContext`] frame advances. Nodes with
//!   animated parameters (keyframed channels, node-output bindings) count as
//!   time-dependent.

use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::composition::compile::{NodeRole, deterministic_node_id};
use crate::composition::{Document, Layer};
use crate::graph::{Graph, Node, ParameterValue};
use crate::id::{CompId, InputPortIndex, LayerId, NodeId};
use crate::types::{FrameRate, NodeData, PortRecord, Scalar};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

// ===========================================================================
// Errors
// ===========================================================================

/// Errors that can occur while evaluating the node graph.
#[derive(Debug, Error)]
pub enum EvalError {
    /// A cycle was encountered during the recursive pull.
    #[error("cycle detected during evaluation at node {0}")]
    CycleDetected(NodeId),

    /// No processor was registered for a node that needed evaluation.
    #[error("no processor registered for node {0}")]
    MissingProcessor(NodeId),

    /// The requested node does not exist in the graph.
    #[error("node {0} not found in graph")]
    NodeNotFound(NodeId),

    /// A node's [`NodeProcessor::process`] returned an error.
    #[error("processing failed for node {node}")]
    ProcessFailed {
        node: NodeId,
        #[source]
        source: anyhow::Error,
    },
}

// ===========================================================================
// EvalContext
// ===========================================================================

/// Per-evaluation context describing the point in time being rendered and the
/// target output configuration.
///
/// Internal processing is always 32-bit float with no artificial resolution or
/// frame-rate limits (REQ-CORE-009); `resolution` is therefore an unconstrained
/// `(u32, u32)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalContext {
    /// Frame index being evaluated (0-based).
    pub frame: u64,
    /// Time of `frame` in seconds (`frame / fps`).
    pub time: f64,
    /// Frame rate of the timeline being evaluated.
    pub fps: FrameRate,
    /// Target output resolution in pixels (`width`, `height`).
    pub resolution: (u32, u32),
}

impl EvalContext {
    /// Build a context for `frame`, deriving `time` from `fps`.
    pub fn new(frame: u64, fps: FrameRate, resolution: (u32, u32)) -> Self {
        let time = frame as f64 / fps.as_f64();
        Self {
            frame,
            time,
            fps,
            resolution,
        }
    }
}

// ===========================================================================
// PathSegment / NodeKey
// ===========================================================================

/// One segment of a network ownership path.
///
/// The full path (`CompId / LayerId / [SubnetNodeId ...]`) identifies which
/// network instance an evaluated node belongs to (REQ-LAYER-009); combined
/// with the node id it forms the cache/dirty key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathSegment {
    /// A layer's owned network.
    Layer(CompId, LayerId),
    /// A subnet node's inner graph (id of the subnet node in its parent
    /// graph, REQ-LAYER-003).
    Subnet(NodeId),
    /// A nested composition. Reserved for PreComp (v2).
    Comp(CompId),
}

/// Cache/dirty key: a node id qualified by its ownership path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct NodeKey {
    path: Vec<PathSegment>,
    node: NodeId,
}

/// Named input bindings offered to a nested scope's interface node
/// (e.g. the `source` frame of an adjustment layer's `net.in`).
pub type Bindings = Vec<(String, Arc<dyn NodeData>)>;

/// Compare two binding sets by port name and `Arc` identity.
fn bindings_equal(a: &[(String, Arc<dyn NodeData>)], b: &[(String, Arc<dyn NodeData>)]) -> bool {
    a.len() == b.len()
        && b.iter()
            .all(|(name, value)| a.iter().any(|(n, v)| n == name && Arc::ptr_eq(v, value)))
}

// ===========================================================================
// ResolvedParams
// ===========================================================================

/// A parameter value after evaluation-time resolution (REQ-LAYER-004).
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Str(String),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

/// Per-frame parameter values passed to [`NodeProcessor::process`].
///
/// Built by the evaluator from the node's [`ParameterValue`]s at each
/// `process` call: constants pass through, channels are sampled at the
/// current frame, and `NodeOutput` sources are pulled from the graph.
/// Processors therefore never capture parameter values at construction.
#[derive(Clone, Debug, Default)]
pub struct ResolvedParams {
    values: Vec<(String, ResolvedValue)>,
}

impl ResolvedParams {
    /// Look up a parameter by key.
    pub fn get(&self, key: &str) -> Option<&ResolvedValue> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Float parameter, if present and a float.
    pub fn f32(&self, key: &str) -> Option<f32> {
        match self.get(key) {
            Some(ResolvedValue::Float(v)) => Some(*v),
            _ => None,
        }
    }

    /// Float parameter or `default` when absent.
    pub fn f32_or(&self, key: &str, default: f32) -> f32 {
        self.f32(key).unwrap_or(default)
    }

    /// Int parameter or `default` when absent.
    pub fn i32_or(&self, key: &str, default: i32) -> i32 {
        match self.get(key) {
            Some(ResolvedValue::Int(v)) => *v,
            _ => default,
        }
    }

    /// Bool parameter or `default` when absent.
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some(ResolvedValue::Bool(v)) => *v,
            _ => default,
        }
    }

    /// String parameter or `default` when absent.
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        match self.get(key) {
            Some(ResolvedValue::Str(v)) => v.as_str(),
            _ => default,
        }
    }

    /// Vec2 parameter or `default` when absent.
    pub fn vec2_or(&self, key: &str, default: [f32; 2]) -> [f32; 2] {
        match self.get(key) {
            Some(ResolvedValue::Vec2(v)) => *v,
            _ => default,
        }
    }

    /// Vec3 parameter or `default` when absent.
    pub fn vec3_or(&self, key: &str, default: [f32; 3]) -> [f32; 3] {
        match self.get(key) {
            Some(ResolvedValue::Vec3(v)) => *v,
            _ => default,
        }
    }

    /// Vec4 parameter or `default` when absent.
    pub fn vec4_or(&self, key: &str, default: [f32; 4]) -> [f32; 4] {
        match self.get(key) {
            Some(ResolvedValue::Vec4(v)) => *v,
            _ => default,
        }
    }
}

// ===========================================================================
// NodeProcessor
// ===========================================================================

/// The per-node-type processing logic invoked by the evaluator.
///
/// Implementors transform their (already-evaluated) `inputs` into a single
/// output value — or a [`PortRecord`] holding one value per output port for
/// multi-output nodes. `inputs` has one slot per declared input port (port
/// order); unconnected ports arrive as `None`. Values are `Arc`-shared so
/// interface nodes can pass them through without copying. Per-frame
/// parameter values arrive via `params`; processors must not capture
/// parameters at construction.
pub trait NodeProcessor: Send + Sync {
    /// Process `inputs` for the given evaluation `ctx` and produce one output.
    ///
    /// `node` is the graph node being evaluated (ports, metadata, type key).
    /// `scope` lets processors evaluate nested graphs (network boundary,
    /// subnet) or resolve document references (Layer Ref, PreComp).
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>>;

    /// Whether this node's output depends on the [`EvalContext`] (frame/time).
    ///
    /// Time-dependent nodes (clips, time samplers, audio-reactive sources, …)
    /// are re-evaluated whenever the frame advances; time-independent nodes
    /// (constants, static generators) are served from cache across frames.
    /// Nodes with animated parameters are treated as time-dependent by the
    /// evaluator regardless of this flag.
    fn is_time_dependent(&self) -> bool {
        false
    }
}

// ===========================================================================
// EvalScope
// ===========================================================================

/// Re-entrant evaluation services handed to [`NodeProcessor::process`].
///
/// Implemented by [`Evaluator`]. A processor that owns or references another
/// graph (network boundary node, subnet node, Layer Ref) pulls values from it
/// through [`EvalScope::evaluate_sub`], passing a (possibly rewritten)
/// [`EvalContext`] and input bindings for the inner graph's interface node.
pub trait EvalScope {
    /// Evaluate `output` inside `graph` as the nested scope `segment`.
    ///
    /// `bindings` are named values offered to the inner graph's interface
    /// node (e.g. the `source` frame of an adjustment layer's `net.in`).
    /// Re-entering a scope that is already on the evaluation stack yields
    /// [`EvalError::CycleDetected`].
    fn evaluate_sub(
        &mut self,
        segment: PathSegment,
        graph: &Graph,
        output: NodeId,
        ctx: &EvalContext,
        bindings: Bindings,
    ) -> Result<Arc<dyn NodeData>, EvalError>;

    /// Bindings offered by the caller of the innermost active scope.
    fn bindings(&self) -> &[(String, Arc<dyn NodeData>)];

    /// The document being evaluated, if the evaluator was given one.
    fn document(&self) -> Option<Arc<Document>>;

    /// The ownership path of the scope currently being evaluated
    /// (REQ-LAYER-009). Lets processors locate their enclosing layer —
    /// e.g. Layer Ref resolves "the same composition" from the innermost
    /// [`PathSegment::Layer`].
    fn path(&self) -> &[PathSegment] {
        &[]
    }
}

// ===========================================================================
// Cache entry
// ===========================================================================

#[derive(Clone)]
struct CacheEntry {
    /// Frame this value was computed for (used only for time-dependent nodes).
    frame: u64,
    /// Context this value was computed under. Resolution/FPS changes
    /// invalidate even frame-matching, time-independent entries.
    ctx: EvalContext,
    value: Arc<dyn NodeData>,
}

// ===========================================================================
// Evaluator
// ===========================================================================

/// Hybrid Pull + Dirty Notification evaluator.
///
/// Owns the per-node processors, the result cache, and the dirty set. The
/// graph itself is passed in to each call so the same evaluator can follow an
/// immutable graph across undo/redo (version switching).
///
/// Processors are registered by [`NodeId`] alone: ids are globally unique
/// (`NodeId::next`), so nodes from every graph (root graph, layer networks)
/// share one registry while cache/dirty state is keyed by full path.
#[derive(Default)]
pub struct Evaluator {
    processors: HashMap<NodeId, Arc<dyn NodeProcessor>>,
    cache: HashMap<NodeKey, CacheEntry>,
    dirty: HashSet<NodeKey>,
    document: Option<Arc<Document>>,
    path: Vec<PathSegment>,
    active_scopes: Vec<PathSegment>,
    bindings_stack: Vec<Bindings>,
    /// Node currently being processed, per recursion level. Lets
    /// [`EvalScope::evaluate_sub`] record which node owns each nested scope.
    processing: Vec<NodeKey>,
    /// Nested scope path → the node whose `process` opened it. Scoped
    /// invalidation uses this to drop the owner's cached value too, so a
    /// network edit propagates to the shell chain automatically.
    scope_owners: HashMap<Vec<PathSegment>, NodeKey>,
    /// Bindings last used per nested scope. A scope re-entered with
    /// different bindings (e.g. an adjustment layer's changing lower stack)
    /// has its cached values dropped before evaluation.
    scope_bindings: HashMap<Vec<PathSegment>, Bindings>,
}

impl Evaluator {
    /// Create an evaluator with no processors registered.
    pub fn new() -> Self {
        Self::default()
    }

    // ----- registration ----------------------------------------------------

    /// Register (or replace) the processor for `node`. The node is marked
    /// dirty so its next pull recomputes.
    ///
    /// Replacements drop the node's cached values at every path, and the
    /// caches of the owners of the scopes containing it — otherwise a
    /// same-frame pull could serve the scope owner's stale cache and never
    /// reach the replaced processor.
    pub fn register(&mut self, node: NodeId, processor: Arc<dyn NodeProcessor>) {
        self.processors.insert(node, processor);
        let paths: Vec<Vec<PathSegment>> = self
            .cache
            .keys()
            .filter(|k| k.node == node)
            .map(|k| k.path.clone())
            .chain(
                self.dirty
                    .iter()
                    .filter(|k| k.node == node)
                    .map(|k| k.path.clone()),
            )
            .collect();
        self.cache.retain(|k, _| k.node != node);
        self.dirty.retain(|k| k.node != node);
        for path in paths {
            self.drop_scope_owner_caches(&path);
        }
        self.dirty.insert(NodeKey {
            path: Vec::new(),
            node,
        });
    }

    /// Whether `node` (at the root scope) is currently marked dirty.
    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.dirty.contains(&NodeKey {
            path: Vec::new(),
            node,
        })
    }

    // ----- document ---------------------------------------------------------

    /// Set the document nested evaluations resolve layers/compositions from.
    ///
    /// Replacing the document invalidates the scopes whose networks changed
    /// between the old and new snapshots (structural sharing makes untouched
    /// scopes free), so undo/redo and edits never mix cached results across
    /// snapshots. Resolution/frame-rate changes and removed compositions
    /// conservatively drop every cache.
    pub fn set_document(&mut self, document: Arc<Document>) {
        if let Some(old) = self.document.as_ref().cloned() {
            // Media asset edits (path swaps) are invisible to the network
            // diff, so they conservatively drop every cache too.
            let structural_change = old.media_assets != document.media_assets
                || old.compositions.iter().any(|(id, old_comp)| {
                    match document.compositions.get(id) {
                        None => true, // composition removed
                        Some(new_comp) => {
                            old_comp.resolution != new_comp.resolution
                                || old_comp.frame_rate != new_comp.frame_rate
                        }
                    }
                });
            if structural_change {
                self.invalidate_all();
            } else {
                for prefix in document.changed_network_paths(&old) {
                    self.invalidate_scope(&prefix);
                }
                // Shell-only edits (timing, transform, opacity, blend,
                // parenting) don't change networks but do change what the
                // synthetic shell nodes produce: drop their caches directly.
                for (comp_id, comp) in &document.compositions {
                    let Some(old_comp) = old.compositions.get(comp_id) else {
                        continue;
                    };
                    if Arc::ptr_eq(comp, old_comp) {
                        continue;
                    }
                    for layer in &comp.layers {
                        let Some(old_layer) = old_comp.layers.iter().find(|l| l.id == layer.id)
                        else {
                            continue;
                        };
                        if layer_shell_changed(layer, old_layer) {
                            for role in [
                                NodeRole::Network,
                                NodeRole::Transform,
                                NodeRole::Opacity,
                                NodeRole::Merge,
                            ] {
                                let id = deterministic_node_id(*comp_id, layer.id, role);
                                self.cache.remove(&NodeKey {
                                    path: Vec::new(),
                                    node: id,
                                });
                                self.dirty.remove(&NodeKey {
                                    path: Vec::new(),
                                    node: id,
                                });
                            }
                        }
                    }
                }
                // Layers present only in the old snapshot: drop their scopes.
                for (comp_id, old_comp) in &old.compositions {
                    for layer in &old_comp.layers {
                        let removed = document
                            .compositions
                            .get(comp_id)
                            .is_none_or(|c| c.get_layer(layer.id).is_none());
                        if removed {
                            self.invalidate_scope(&[PathSegment::Layer(*comp_id, layer.id)]);
                        }
                    }
                }
            }
        }
        self.document = Some(document);
    }

    // ----- dirty propagation -----------------------------------------------

    /// Mark `node` and every node reachable downstream from it dirty (root
    /// scope).
    ///
    /// This is the **push** half of the model: invoked when a node's
    /// parameters (or wiring) change so that the next pull recomputes the
    /// affected subgraph and serves everything else from cache.
    pub fn mark_dirty(&mut self, graph: &Graph, node: NodeId) {
        self.mark_dirty_at(graph, &[], node);
    }

    /// [`mark_dirty`](Self::mark_dirty) for a node inside the network scope
    /// `path` (e.g. `&[PathSegment::Layer(comp, layer)]`).
    ///
    /// Also drops the cached values of the scope's owner (and its ancestor
    /// owners), so the next same-frame pull re-enters the dirtied network
    /// instead of serving the boundary's stale cache.
    pub fn mark_dirty_at(&mut self, graph: &Graph, path: &[PathSegment], node: NodeId) {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            let key = NodeKey {
                path: path.to_vec(),
                node: current,
            };
            if self.dirty.insert(key) {
                for downstream in graph.outputs_of(current) {
                    stack.push(downstream);
                }
            }
        }
        self.drop_scope_owner_caches(path);
    }

    /// Drop every cached value and clear the dirty set (forces a full recompute
    /// on the next pull). Processor registrations are kept.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.dirty.clear();
    }

    /// Drop cached values and dirty flags for every node whose path starts
    /// with `prefix` (e.g. one layer's network, subnets included).
    ///
    /// The cached values of the nodes that *own* matching scopes (network
    /// boundary nodes, subnet nodes) — and of the owners of every ancestor
    /// scope — are dropped as well: their recompute marks them fresh, which
    /// cascades to their downstream in the parent graph on the next pull.
    pub fn invalidate_scope(&mut self, prefix: &[PathSegment]) {
        self.cache.retain(|k, _| !k.path.starts_with(prefix));
        self.dirty.retain(|k| !k.path.starts_with(prefix));
        self.drop_scope_owner_caches(prefix);
    }

    /// Drop cached/dirty entries for the owners of `scope` and of every
    /// ancestor scope (e.g. for `[layer, subnet]`: the subnet node *and* the
    /// layer boundary).
    fn drop_scope_owner_caches(&mut self, scope: &[PathSegment]) {
        let owners: Vec<NodeKey> = self
            .scope_owners
            .iter()
            .filter(|(owned, _)| scope.starts_with(owned.as_slice()))
            .map(|(_, owner)| owner.clone())
            .collect();
        for owner in owners {
            self.cache.remove(&owner);
            self.dirty.remove(&owner);
        }
    }

    // ----- evaluation ------------------------------------------------------

    /// Pull-evaluate `output` for `ctx` at the root scope, returning its
    /// computed value.
    ///
    /// Inputs are evaluated recursively (depth-first). Nodes not reachable from
    /// `output` are never touched, satisfying "unused nodes are not evaluated".
    pub fn evaluate(
        &mut self,
        graph: &Graph,
        output: NodeId,
        ctx: &EvalContext,
    ) -> Result<Arc<dyn NodeData>, EvalError> {
        self.path.clear();
        self.active_scopes.clear();
        self.bindings_stack.clear();
        self.bindings_stack.push(Vec::new());
        self.evaluate_inner(graph, output, ctx)
    }

    /// Shared tail of [`evaluate`](Self::evaluate) and
    /// [`EvalScope::evaluate_sub`]: runs the pull with the path/bindings
    /// state already set up by the caller.
    fn evaluate_inner(
        &mut self,
        graph: &Graph,
        output: NodeId,
        ctx: &EvalContext,
    ) -> Result<Arc<dyn NodeData>, EvalError> {
        if graph.node(output).is_none() {
            return Err(EvalError::NodeNotFound(output));
        }
        let span = tracing::debug_span!("evaluate", output = output.raw(), frame = ctx.frame);
        let _guard = span.enter();
        let mut run = HashMap::new();
        let mut visiting = HashSet::new();
        let (value, _fresh) = self.eval_node(graph, output, ctx, &mut run, &mut visiting)?;
        Ok(value)
    }

    /// Returns `(value, fresh)` where `fresh` is `true` if the node was
    /// recomputed during this pull (as opposed to served from cache).
    fn eval_node(
        &mut self,
        graph: &Graph,
        node: NodeId,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
    ) -> Result<(Arc<dyn NodeData>, bool), EvalError> {
        let key = NodeKey {
            path: self.path.clone(),
            node,
        };

        // Already computed in this pull → reuse (diamond de-duplication).
        if let Some(cached) = run.get(&key) {
            return Ok(cached.clone());
        }
        // Re-entering a node still on the recursion stack means a cycle.
        if !visiting.insert(key.clone()) {
            return Err(EvalError::CycleDetected(node));
        }

        let node_ref = graph
            .node(node)
            .cloned()
            .ok_or(EvalError::NodeNotFound(node))?;

        // Evaluate upstream inputs into per-port slots (port order).
        let in_edges: Vec<(InputPortIndex, NodeId, crate::id::OutputPortIndex)> = graph
            .edges()
            .filter(|e| e.target == node)
            .map(|e| (e.target_port, e.source, e.source_port))
            .collect();

        let mut input_values: Vec<Option<Arc<dyn NodeData>>> = vec![None; node_ref.inputs.len()];
        let mut any_input_fresh = false;
        for (target_port, source, source_port) in in_edges {
            let slot = target_port.0 as usize;
            if slot >= input_values.len() {
                return Err(EvalError::ProcessFailed {
                    node,
                    source: anyhow::anyhow!(
                        "edge into port {target_port:?} is out of range \
                         ({} input ports)",
                        input_values.len()
                    ),
                });
            }
            let (value, fresh) = self.eval_node(graph, source, ctx, run, visiting)?;
            any_input_fresh |= fresh;
            let port_count = graph.node(source).map(|n| n.outputs.len()).unwrap_or(1);
            let extracted =
                PortRecord::extract(&value, port_count, source_port).ok_or_else(|| {
                    EvalError::ProcessFailed {
                        node: source,
                        source: anyhow::anyhow!(
                            "edge from port {source_port:?} has no value \
                             (port out of range or missing record)"
                        ),
                    }
                })?;
            input_values[slot] = Some(extracted);
        }

        let processor = self
            .processors
            .get(&node)
            .cloned()
            .ok_or(EvalError::MissingProcessor(node))?;
        let time_dependent = processor.is_time_dependent() || node_has_animated_params(&node_ref);

        // Resolve parameters *before* the cache decision: NodeOutput-bound
        // parameters are hidden dependencies, and a same-frame source change
        // must force a recompute (REQ-LAYER-004).
        let (params, params_fresh) = self.resolve_params(graph, &node_ref, ctx, run, visiting)?;

        // Decide whether the cached value is still valid: the resolution/FPS
        // must match for every node, and the frame must match for
        // time-dependent ones.
        let cache_valid = !self.dirty.contains(&key)
            && !any_input_fresh
            && !params_fresh
            && match self.cache.get(&key) {
                Some(entry) => {
                    entry.ctx.resolution == ctx.resolution
                        && entry.ctx.fps == ctx.fps
                        && (!time_dependent || entry.frame == ctx.frame)
                }
                None => false,
            };

        let result = if cache_valid {
            // SAFETY of unwrap: cache_valid implies the entry exists.
            let value = self.cache.get(&key).unwrap().value.clone();
            (value, false)
        } else {
            let span = tracing::debug_span!(
                "node_process",
                node = node.raw(),
                type_key = %node_ref.type_key
            );
            let _guard = span.enter();
            self.processing.push(key.clone());
            let produced = processor
                .process(&node_ref, ctx, &input_values, &params, self)
                .map_err(|source| EvalError::ProcessFailed { node, source });
            self.processing.pop();
            let value = produced?;
            self.cache.insert(
                key.clone(),
                CacheEntry {
                    frame: ctx.frame,
                    ctx: *ctx,
                    value: value.clone(),
                },
            );
            self.dirty.remove(&key);
            (value, true)
        };

        visiting.remove(&key);
        run.insert(key, result.clone());
        Ok(result)
    }

    // ----- parameter resolution (REQ-LAYER-004) -----------------------------

    /// Build the per-frame [`ResolvedParams`] for `node`.
    ///
    /// Also returns whether any `NodeOutput` source resolved to a *fresh*
    /// (recomputed) value, which the caller uses to force a recompute of the
    /// consuming node even at the same frame.
    fn resolve_params(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
    ) -> Result<(ResolvedParams, bool), EvalError> {
        let mut any_fresh = false;
        let mut values = Vec::with_capacity(node.parameters.len());
        for p in &node.parameters {
            let value = match &p.value {
                ParameterValue::Float(v) => ResolvedValue::Float(*v),
                ParameterValue::Int(v) => ResolvedValue::Int(*v),
                ParameterValue::Bool(v) => ResolvedValue::Bool(*v),
                ParameterValue::String(v) => ResolvedValue::Str(v.clone()),
                ParameterValue::Channel(ch) => {
                    let (v, fresh) = self.resolve_channel(graph, ch, ctx, run, visiting)?;
                    any_fresh |= fresh;
                    ResolvedValue::Float(v)
                }
                ParameterValue::Channel2(chs) => {
                    let mut v = [0.0; 2];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) = self.resolve_channel(graph, ch, ctx, run, visiting)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec2(v)
                }
                ParameterValue::Channel3(chs) => {
                    let mut v = [0.0; 3];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) = self.resolve_channel(graph, ch, ctx, run, visiting)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec3(v)
                }
                ParameterValue::Channel4(chs) => {
                    let mut v = [0.0; 4];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) = self.resolve_channel(graph, ch, ctx, run, visiting)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec4(v)
                }
            };
            values.push((p.key.clone(), value));
        }
        Ok((ResolvedParams { values }, any_fresh))
    }

    fn resolve_channel(
        &mut self,
        graph: &Graph,
        channel: &AnimationChannel,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
    ) -> Result<(f32, bool), EvalError> {
        self.resolve_source(graph, &channel.source, ctx, run, visiting)
    }

    fn resolve_source(
        &mut self,
        graph: &Graph,
        source: &ChannelSource,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
    ) -> Result<(f32, bool), EvalError> {
        match source {
            ChannelSource::NodeOutput(target, port) => {
                let (value, fresh) = self.eval_node(graph, *target, ctx, run, visiting)?;
                let port_count = graph.node(*target).map(|n| n.outputs.len()).unwrap_or(1);
                let extracted =
                    PortRecord::extract(&value, port_count, *port).ok_or_else(|| {
                        EvalError::ProcessFailed {
                            node: *target,
                            source: anyhow::anyhow!(
                                "NodeOutput binding: port {port:?} unavailable"
                            ),
                        }
                    })?;
                let scalar =
                    extracted
                        .downcast_ref::<Scalar>()
                        .ok_or_else(|| EvalError::ProcessFailed {
                            node: *target,
                            source: anyhow::anyhow!(
                                "NodeOutput binding expects a Scalar output, got {:?}",
                                extracted.data_type_id()
                            ),
                        })?;
                Ok((scalar.0, fresh))
            }
            ChannelSource::Blend(a, b, mode, factor) => {
                let factor = *factor;
                let (av, af) = self.resolve_source(graph, a, ctx, run, visiting)?;
                let (bv, bf) = self.resolve_source(graph, b, ctx, run, visiting)?;
                Ok((mode.blend(av, bv, factor), af || bf))
            }
            other => Ok((other.evaluate(ctx.frame, ctx), false)),
        }
    }
}

impl EvalScope for Evaluator {
    fn evaluate_sub(
        &mut self,
        segment: PathSegment,
        graph: &Graph,
        output: NodeId,
        ctx: &EvalContext,
        bindings: Bindings,
    ) -> Result<Arc<dyn NodeData>, EvalError> {
        if self.active_scopes.contains(&segment) {
            return Err(EvalError::CycleDetected(output));
        }
        self.active_scopes.push(segment);
        self.path.push(segment);
        if let Some(owner) = self.processing.last().cloned() {
            self.scope_owners.insert(self.path.clone(), owner);
        }
        // A scope re-entered with different bindings (e.g. an adjustment
        // layer's lower stack) may not reuse its previous cached values.
        let bindings_changed = match self.scope_bindings.get(&self.path) {
            Some(old) => !bindings_equal(old, &bindings),
            None => !bindings.is_empty(),
        };
        if bindings_changed {
            let path = self.path.clone();
            self.cache.retain(|k, _| !k.path.starts_with(&path));
            self.dirty.retain(|k| !k.path.starts_with(&path));
        }
        self.scope_bindings
            .insert(self.path.clone(), bindings.clone());
        self.bindings_stack.push(bindings);

        let result = self.evaluate_inner(graph, output, ctx);

        self.bindings_stack.pop();
        self.path.pop();
        self.active_scopes.pop();
        result
    }

    fn bindings(&self) -> &[(String, Arc<dyn NodeData>)] {
        self.bindings_stack
            .last()
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    fn document(&self) -> Option<Arc<Document>> {
        self.document.clone()
    }

    fn path(&self) -> &[PathSegment] {
        &self.path
    }
}

// ===========================================================================
// Animated-parameter detection
// ===========================================================================

/// Whether evaluation-relevant shell fields changed between two versions of
/// a layer (used by [`Evaluator::set_document`] cache invalidation).
fn layer_shell_changed(new: &Layer, old: &Layer) -> bool {
    new.start_frame != old.start_frame
        || new.in_frame != old.in_frame
        || new.out_frame != old.out_frame
        || new.transform != old.transform
        || new.opacity != old.opacity
        || new.blend_mode != old.blend_mode
        || new.adjustment != old.adjustment
        || new.parent != old.parent
}

/// Whether any parameter of `node` carries a time-varying source (keyframes,
/// expression, audio-reactive, or a node-output binding). Such nodes must be
/// re-evaluated when the frame advances even if the processor itself is
/// time-independent (REQ-LAYER-004).
fn node_has_animated_params(node: &Node) -> bool {
    node.parameters.iter().any(|p| match &p.value {
        ParameterValue::Channel(ch) => channel_is_time_varying(ch),
        ParameterValue::Channel2(chs) => chs.iter().any(channel_is_time_varying),
        ParameterValue::Channel3(chs) => chs.iter().any(channel_is_time_varying),
        ParameterValue::Channel4(chs) => chs.iter().any(channel_is_time_varying),
        _ => false,
    })
}

fn channel_is_time_varying(channel: &AnimationChannel) -> bool {
    source_is_time_varying(&channel.source)
}

fn source_is_time_varying(source: &ChannelSource) -> bool {
    match source {
        ChannelSource::Constant(_) => false,
        ChannelSource::Keyframes(_)
        | ChannelSource::Expression(_)
        | ChannelSource::NodeOutput(_, _)
        | ChannelSource::AudioReactive(_) => true,
        ChannelSource::Blend(a, b, _, _) => source_is_time_varying(a) || source_is_time_varying(b),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, OutputPortIndex};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx_at(frame: u64) -> EvalContext {
        EvalContext::new(frame, FPS, (1920, 1080))
    }

    fn scalar_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "test")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_input("b", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR)
    }

    /// A constant source that counts how many times it is processed.
    struct CountingConst {
        value: f32,
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for CountingConst {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Scalar(self.value)))
        }
    }

    /// A time-dependent source emitting the current frame index as a scalar.
    struct FrameSource {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for FrameSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Scalar(ctx.frame as f32)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Sums all connected scalar inputs and adds 1; counts its invocations.
    struct CountingSum {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for CountingSum {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let mut sum = 1.0f32;
            for input in inputs.iter().flatten() {
                let s = input
                    .downcast_ref::<Scalar>()
                    .ok_or_else(|| anyhow::anyhow!("expected Scalar input"))?;
                sum += s.0;
            }
            Ok(Arc::new(Scalar(sum)))
        }
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- diamond de-duplication -------------------------------------------

    #[test]
    fn diamond_shared_node_evaluated_once() {
        //   1
        //  / \
        // 2   3
        //  \ /
        //   4
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(scalar_node(3))
            .unwrap()
            .add_node(scalar_node(4))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(1),
            )
            .unwrap();

        let shared_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 2.0,
                calls: shared_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(4),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(4), &ctx_at(0)).unwrap();
        // Shared root (node 1) must be processed exactly once.
        assert_eq!(shared_calls.load(Ordering::Relaxed), 1);
        // Value: n1=2; n2=1+2=3; n3=1+2=3; n4=1+3+3=7
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 7.0).abs() < f32::EPSILON);
    }

    // ---- cycle detection ---------------------------------------------------

    #[test]
    fn cycle_returns_error_without_panic() {
        // Build 1 → 2 → 1 via the unchecked test escape hatch.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_edge_unchecked(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .add_edge_unchecked(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            );

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let result = ev.evaluate(&g, NodeId::new(2), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::CycleDetected(_))));
    }

    // ---- dirty propagation -------------------------------------------------

    #[test]
    fn clean_nodes_served_from_cache() {
        // 1 → 2
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: c1.clone(),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(CountingSum { calls: c2.clone() }));

        ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert_eq!(c1.load(Ordering::Relaxed), 1);
        assert_eq!(c2.load(Ordering::Relaxed), 1);

        // Second pull at the same frame: nothing dirty → no recompute.
        ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert_eq!(c1.load(Ordering::Relaxed), 1);
        assert_eq!(c2.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dirty_propagates_downstream_only() {
        // 1 → 2 → 3, plus an unrelated 4 → 3 branch.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(scalar_node(3))
            .unwrap()
            .add_node(scalar_node(4))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(4),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let c3 = Arc::new(AtomicUsize::new(0));
        let c4 = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: c1.clone(),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(CountingSum { calls: c2.clone() }));
        ev.register(
            NodeId::new(4),
            Arc::new(CountingConst {
                value: 9.0,
                calls: c4.clone(),
            }),
        );
        ev.register(NodeId::new(3), Arc::new(CountingSum { calls: c3.clone() }));

        ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert_eq!(c1.load(Ordering::Relaxed), 1);
        assert_eq!(c2.load(Ordering::Relaxed), 1);
        assert_eq!(c3.load(Ordering::Relaxed), 1);
        assert_eq!(c4.load(Ordering::Relaxed), 1);

        // Mark node 2 dirty: 2 and 3 must recompute, 1 and 4 must not.
        ev.mark_dirty(&g, NodeId::new(2));
        assert!(ev.is_dirty(NodeId::new(2)));
        assert!(ev.is_dirty(NodeId::new(3)));
        assert!(!ev.is_dirty(NodeId::new(1)));
        assert!(!ev.is_dirty(NodeId::new(4)));

        ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert_eq!(c1.load(Ordering::Relaxed), 1); // cached
        assert_eq!(c2.load(Ordering::Relaxed), 2); // recomputed
        assert_eq!(c3.load(Ordering::Relaxed), 2); // recomputed (input changed)
        assert_eq!(c4.load(Ordering::Relaxed), 1); // cached
    }

    // ---- frame-change selective re-evaluation ------------------------------

    #[test]
    fn frame_change_reevaluates_only_time_dependent() {
        // time-dependent 1 and constant 2 both feed sum 3.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(scalar_node(3))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let frame_calls = Arc::new(AtomicUsize::new(0));
        let const_calls = Arc::new(AtomicUsize::new(0));
        let sum_calls = Arc::new(AtomicUsize::new(0));

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(FrameSource {
                calls: frame_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingConst {
                value: 10.0,
                calls: const_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(CountingSum {
                calls: sum_calls.clone(),
            }),
        );

        let out0 = ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert_eq!(frame_calls.load(Ordering::Relaxed), 1);
        assert_eq!(const_calls.load(Ordering::Relaxed), 1);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 1);
        // frame 0: 1 + 0 + 10 = 11
        assert!((out0.downcast_ref::<Scalar>().unwrap().0 - 11.0).abs() < f32::EPSILON);

        // Advance the frame. Time-dependent source (and its downstream sum)
        // recompute; the constant stays cached.
        let out5 = ev.evaluate(&g, NodeId::new(3), &ctx_at(5)).unwrap();
        assert_eq!(frame_calls.load(Ordering::Relaxed), 2); // recomputed
        assert_eq!(const_calls.load(Ordering::Relaxed), 1); // cached
        assert_eq!(sum_calls.load(Ordering::Relaxed), 2); // recomputed (input changed)
        // frame 5: 1 + 5 + 10 = 16
        assert!((out5.downcast_ref::<Scalar>().unwrap().0 - 16.0).abs() < f32::EPSILON);
    }

    // ---- unused node isolation ---------------------------------------------

    #[test]
    fn unconnected_nodes_are_not_evaluated() {
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap(); // never connected to the output

        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: c1.clone(),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingConst {
                value: 2.0,
                calls: c2.clone(),
            }),
        );

        ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(c1.load(Ordering::Relaxed), 1);
        assert_eq!(c2.load(Ordering::Relaxed), 0);
    }

    // ---- error handling ----------------------------------------------------

    #[test]
    fn missing_processor_errors() {
        let g = Graph::new().add_node(scalar_node(1)).unwrap();
        let mut ev = Evaluator::new();
        let result = ev.evaluate(&g, NodeId::new(1), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::MissingProcessor(_))));
    }

    #[test]
    fn evaluate_missing_node_errors() {
        let g = Graph::new();
        let mut ev = Evaluator::new();
        let result = ev.evaluate(&g, NodeId::new(42), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::NodeNotFound(_))));
    }

    #[test]
    fn process_failure_is_wrapped() {
        struct Failing;
        impl NodeProcessor for Failing {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                Err(anyhow::anyhow!("boom"))
            }
        }
        let g = Graph::new().add_node(scalar_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Failing));
        let result = ev.evaluate(&g, NodeId::new(1), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::ProcessFailed { .. })));
    }

    // ---- scale -------------------------------------------------------------

    #[test]
    fn hundred_node_chain_completes() {
        // Linear chain 1 → 2 → … → 100.
        let mut g = Graph::new().add_node(scalar_node(1)).unwrap();
        for i in 2..=100u64 {
            g = g.add_node(scalar_node(i)).unwrap();
            g = g
                .add_edge(
                    EdgeId::new(i),
                    NodeId::new(i - 1),
                    OutputPortIndex(0),
                    NodeId::new(i),
                    InputPortIndex(0),
                )
                .unwrap();
        }

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 0.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        for i in 2..=100u64 {
            ev.register(
                NodeId::new(i),
                Arc::new(CountingSum {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            );
        }

        let out = ev.evaluate(&g, NodeId::new(100), &ctx_at(0)).unwrap();
        // Each sum adds 1; chain of 99 sums over a 0.0 source → 99.0.
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 99.0).abs() < f32::EPSILON);
    }

    // ---- parameter resolution (REQ-LAYER-004) ------------------------------

    /// Echoes resolved params into a Scalar for inspection.
    struct ParamEcho;
    impl NodeProcessor for ParamEcho {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Scalar(params.f32_or("value", -1.0))))
        }
    }

    #[test]
    fn keyframed_parameter_animates_without_processor_rebuild() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);

        let node = Node::new(NodeId::new(1), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::keyframes(curve)),
            );
        let g = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ParamEcho));

        let v0 = ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!((v0.downcast_ref::<Scalar>().unwrap().0 - 0.0).abs() < 1e-4);

        // Animated params make the node time-varying: no dirty marking and no
        // processor rebuild, yet the new frame re-evaluates.
        let v5 = ev.evaluate(&g, NodeId::new(1), &ctx_at(5)).unwrap();
        assert!((v5.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < 1e-4);
    }

    #[test]
    fn constant_channel_parameter_stays_cached() {
        let node = Node::new(NodeId::new(1), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::constant(3.0)),
            );
        let g = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ParamEcho));

        ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        let v = ev.evaluate(&g, NodeId::new(1), &ctx_at(9)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn node_output_binding_pulls_source_value() {
        // node 1 (source scalar 4) ──binding──▶ param of node 2
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let bound = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                    NodeId::new(1),
                    OutputPortIndex(0),
                ))),
            );
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(bound)
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 4.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(ParamEcho));

        let v = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 4.0).abs() < f32::EPSILON);
    }

    // ---- scoped evaluation (REQ-LAYER-007) ---------------------------------

    /// Pulls `output` of `inner` via the scope with a rewritten frame
    /// (mimics a network boundary evaluating a layer-local context).
    struct ScopedSource {
        inner: Graph,
        inner_output: NodeId,
        segment: PathSegment,
        frame_offset: u64,
    }

    impl NodeProcessor for ScopedSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let mut local = *ctx;
            local.frame += self.frame_offset;
            let value = scope.evaluate_sub(
                self.segment,
                &self.inner,
                self.inner_output,
                &local,
                Vec::new(),
            )?;
            Ok(Arc::new(ScopeWrap(value)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Marker wrapper so the outer value differs from the inner one.
    struct ScopeWrap(Arc<dyn NodeData>);
    impl NodeData for ScopeWrap {
        fn data_type_id(&self) -> DataTypeId {
            DataTypeId::SCALAR
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn evaluate_sub_uses_rewritten_context_and_path_cache() {
        // Inner graph: a single time-dependent node reading ctx.frame.
        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let inner_calls = Arc::new(AtomicUsize::new(0));

        let outer_node = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let outer = Graph::new().add_node(outer_node).unwrap();

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ScopedSource {
                inner: inner.clone(),
                inner_output: NodeId::new(7),
                segment,
                frame_offset: 100,
            }),
        );
        ev.register(
            NodeId::new(7),
            Arc::new(FrameSource {
                calls: inner_calls.clone(),
            }),
        );

        // First pull: inner node evaluates with local frame 100.
        let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        let wrap = out.downcast_ref::<ScopeWrap>().unwrap();
        assert!((wrap.0.downcast_ref::<Scalar>().unwrap().0 - 100.0).abs() < f32::EPSILON);
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Same outer frame again: outer is time-dependent and re-runs, but the
        // inner node sees the same local frame → served from path cache.
        let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        let wrap = out.downcast_ref::<ScopeWrap>().unwrap();
        assert!((wrap.0.downcast_ref::<Scalar>().unwrap().0 - 100.0).abs() < f32::EPSILON);
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Advance the outer frame: local frame changes → re-evaluation.
        let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(1)).unwrap();
        let wrap = out.downcast_ref::<ScopeWrap>().unwrap();
        assert!((wrap.0.downcast_ref::<Scalar>().unwrap().0 - 101.0).abs() < f32::EPSILON);
        assert_eq!(inner_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recursive_scope_reentry_is_a_cycle() {
        struct Reentrant;
        impl NodeProcessor for Reentrant {
            fn process(
                &self,
                node: &Node,
                ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                // Re-enter the same segment → must be rejected.
                let segment = PathSegment::Layer(CompId::new(1), LayerId::new(1));
                let value = scope.evaluate_sub(segment, &Graph::new(), node.id, ctx, Vec::new());
                match value {
                    Err(EvalError::CycleDetected(_)) => Ok(Arc::new(Scalar(1.0))),
                    other => anyhow::bail!("expected cycle error, got {:?}", other.is_ok()),
                }
            }
        }

        let node = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let outer = Graph::new().add_node(node).unwrap();

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(1));
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Reentrant));
        ev.register(
            NodeId::new(9),
            Arc::new(CountingConst {
                value: 0.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        // Wrap the outer node in a scope push via evaluate_sub directly.
        let result = ev.evaluate_sub(segment, &outer, NodeId::new(1), &ctx_at(0), Vec::new());
        // The inner Reentrant node re-enters the same segment → CycleDetected
        // is produced inside and converted to Scalar(1.0) by the processor.
        let v = result.unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn invalidate_scope_drops_only_matching_prefix() {
        // Populate a path-scoped cache entry through evaluate_sub, then
        // invalidate and confirm re-evaluation.
        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let inner_calls = Arc::new(AtomicUsize::new(0));

        let outer_node = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let outer = Graph::new().add_node(outer_node).unwrap();

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ScopedSource {
                inner: inner.clone(),
                inner_output: NodeId::new(7),
                segment,
                frame_offset: 0,
            }),
        );
        ev.register(
            NodeId::new(7),
            Arc::new(FrameSource {
                calls: inner_calls.clone(),
            }),
        );

        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Invalidate an unrelated scope: cache kept.
        ev.invalidate_scope(&[PathSegment::Layer(CompId::new(9), LayerId::new(9))]);
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Invalidate the actual scope: re-evaluated.
        ev.invalidate_scope(&[segment]);
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 2);
    }

    // ---- regression: hidden/stale dependency fixes -------------------------

    /// A scalar source whose value can be swapped between pulls.
    struct MutableSource {
        value: Arc<std::sync::Mutex<f32>>,
    }

    impl NodeProcessor for MutableSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Scalar(*self.value.lock().unwrap())))
        }
    }

    #[test]
    fn node_output_binding_tracks_same_frame_source_changes() {
        // A (mutable scalar) ──NodeOutput binding──▶ param of B
        let a = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let b = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                    NodeId::new(1),
                    OutputPortIndex(0),
                ))),
            );
        let g = Graph::new().add_node(a).unwrap().add_node(b).unwrap();

        let shared = Arc::new(std::sync::Mutex::new(1.0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(MutableSource {
                value: shared.clone(),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(ParamEcho));

        let v = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);

        // Same frame: A changes. The binding must observe the fresh value.
        *shared.lock().unwrap() = 2.0;
        ev.mark_dirty(&g, NodeId::new(1));
        let v = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn edge_from_invalid_port_is_an_error() {
        // node 1 has a single output; wiring from port 1 must fail loudly.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let result = ev.evaluate(&g, NodeId::new(2), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::ProcessFailed { .. })));
    }

    #[test]
    fn mark_dirty_at_cascades_to_scope_owner() {
        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let inner_calls = Arc::new(AtomicUsize::new(0));

        let outer_node = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let outer = Graph::new().add_node(outer_node).unwrap();

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ScopedSource {
                inner: inner.clone(),
                inner_output: NodeId::new(7),
                segment,
                frame_offset: 0,
            }),
        );
        ev.register(
            NodeId::new(7),
            Arc::new(FrameSource {
                calls: inner_calls.clone(),
            }),
        );

        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Dirty an inner node: the boundary's same-frame cache must not hide it.
        ev.mark_dirty_at(&inner, &[segment], NodeId::new(7));
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn set_document_invalidates_changed_layer_networks() {
        use crate::composition::{Composition, Document, Layer};

        let inner1 = Graph::new().add_node(scalar_node(7)).unwrap();
        let inner2 = Graph::new().add_node(scalar_node(8)).unwrap();

        let make_doc =
            |network: Graph| {
                Document::default().with_composition(
                    Composition::new(CompId::new(1), "C", (16, 16), FPS, 100)
                        .add_layer(Layer::new(LayerId::new(1), "L", network)),
                )
            };
        let doc1 = Arc::new(make_doc(inner1.clone()));
        let doc2 = Arc::new(make_doc(inner2.clone()));

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(1));
        let calls7 = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(7),
            Arc::new(CountingConst {
                value: 1.0,
                calls: calls7.clone(),
            }),
        );
        ev.register(
            NodeId::new(8),
            Arc::new(CountingConst {
                value: 2.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        ev.set_document(doc1.clone());
        ev.evaluate_sub(segment, &inner1, NodeId::new(7), &ctx_at(0), Vec::new())
            .unwrap();
        assert_eq!(calls7.load(Ordering::Relaxed), 1);

        // Same snapshot again: nothing changed → cache kept.
        ev.set_document(doc1.clone());
        ev.evaluate_sub(segment, &inner1, NodeId::new(7), &ctx_at(0), Vec::new())
            .unwrap();
        assert_eq!(calls7.load(Ordering::Relaxed), 1);

        // Changed layer network: scope invalidated and re-evaluated.
        ev.set_document(doc2);
        let v = ev
            .evaluate_sub(segment, &inner2, NodeId::new(8), &ctx_at(0), Vec::new())
            .unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < f32::EPSILON);
    }

    // ---- regression: round-2 review fixes ----------------------------------

    /// Emits the evaluation resolution's width; time-independent.
    struct ResolutionSource {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for ResolutionSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Scalar(ctx.resolution.0 as f32)))
        }
    }

    #[test]
    fn context_change_invalidates_cache_at_same_frame() {
        let g = Graph::new().add_node(scalar_node(1)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ResolutionSource {
                calls: calls.clone(),
            }),
        );

        let v = ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 1920.0).abs() < f32::EPSILON);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Same frame, different resolution: must recompute.
        let ctx_small = EvalContext::new(0, FPS, (64, 64));
        let v = ev.evaluate(&g, NodeId::new(1), &ctx_small).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 64.0).abs() < f32::EPSILON);
        assert_eq!(calls.load(Ordering::Relaxed), 2);

        // Same frame, different FPS: must recompute.
        let ctx_fps = EvalContext::new(0, FrameRate::new(24, 1), (64, 64));
        ev.evaluate(&g, NodeId::new(1), &ctx_fps).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn register_replacement_inside_scope_recomputes() {
        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let outer_node = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let outer = Graph::new().add_node(outer_node).unwrap();

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ScopedSource {
                inner: inner.clone(),
                inner_output: NodeId::new(7),
                segment,
                frame_offset: 0,
            }),
        );
        ev.register(
            NodeId::new(7),
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        let wrap = out.downcast_ref::<ScopeWrap>().unwrap();
        assert!((wrap.0.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);

        // Replace the inner processor: the boundary must not hide it.
        ev.register(
            NodeId::new(7),
            Arc::new(CountingConst {
                value: 2.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        let wrap = out.downcast_ref::<ScopeWrap>().unwrap();
        assert!((wrap.0.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn target_port_out_of_range_is_an_error() {
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(9),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let result = ev.evaluate(&g, NodeId::new(2), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::ProcessFailed { .. })));
    }

    #[test]
    fn zero_output_source_edge_is_an_error() {
        // Node with no declared outputs used as an edge source.
        let no_outputs = Node::new(NodeId::new(1), "test");
        let g = Graph::new()
            .add_node(no_outputs)
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let result = ev.evaluate(&g, NodeId::new(2), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::ProcessFailed { .. })));
    }

    /// Emits a Vec2 (non-scalar) value.
    struct Vec2Source;
    impl NodeProcessor for Vec2Source {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(crate::types::Vec2(1.0, 2.0)))
        }
    }

    #[test]
    fn node_output_binding_rejects_non_scalar() {
        let a = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::VEC2);
        let b = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                    NodeId::new(1),
                    OutputPortIndex(0),
                ))),
            );
        let g = Graph::new().add_node(a).unwrap().add_node(b).unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Vec2Source));
        ev.register(NodeId::new(2), Arc::new(ParamEcho));

        let result = ev.evaluate(&g, NodeId::new(2), &ctx_at(0));
        assert!(matches!(result, Err(EvalError::ProcessFailed { .. })));
    }

    #[test]
    fn removed_layer_scope_is_dropped() {
        use crate::composition::{Composition, Document, Layer};

        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let make_doc = |with_layer: bool| {
            let comp = Composition::new(CompId::new(1), "C", (16, 16), FPS, 100);
            let comp = if with_layer {
                comp.add_layer(Layer::new(LayerId::new(1), "L", inner.clone()))
            } else {
                comp
            };
            Document::default().with_composition(comp)
        };

        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(7),
            Arc::new(FrameSource {
                calls: calls.clone(),
            }),
        );

        ev.set_document(Arc::new(make_doc(true)));
        ev.evaluate_sub(segment, &inner, NodeId::new(7), &ctx_at(0), Vec::new())
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Layer removed in the new snapshot: the scope cache is dropped.
        ev.set_document(Arc::new(make_doc(false)));
        ev.evaluate_sub(segment, &inner, NodeId::new(7), &ctx_at(0), Vec::new())
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}

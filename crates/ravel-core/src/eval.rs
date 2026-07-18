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
//! * **Bypass** — a node whose [`crate::graph::NodeMetadata::bypassed`] flag
//!   is set skips `process` and yields, per output port, the value of the
//!   first connected non-parameter input port that accepts the port's data
//!   type (single-output nodes yield the value directly, multi-output nodes
//!   a [`PortRecord`]; see [`bypass_passthrough_plan`]). Only the inputs the
//!   pass-through actually uses are pulled: unused inputs and parameter
//!   sources are never evaluated, so their failure cannot fail the bypass.
//!   A node with no type-matching connected input for some output port is
//!   processed normally — bypass is ignored, never an error. The flag is
//!   part of cache validity, so toggling it recomputes the node even when
//!   no invalidation reached the evaluator.

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

    /// Replace (or insert) the resolved value for `key`. Used by the
    /// evaluator to overlay connected parameter-port values over the
    /// node's stored parameters.
    pub fn set(&mut self, key: &str, value: ResolvedValue) {
        match self.values.iter_mut().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = value,
            None => self.values.push((key.to_string(), value)),
        }
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
    /// The node's bypass flag when this value was produced. Toggling bypass
    /// is a metadata edit that keeps ports and wiring, so the flag is part
    /// of cache validity: a pull after a toggle must not serve the stale
    /// processed (or pass-through) result.
    bypassed: bool,
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
    /// Wall-clock `process()` durations recorded by the current top-level
    /// evaluation (see [`Evaluator::take_timings`]).
    timings: Vec<(NodeId, std::time::Duration)>,
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
                    let mut shell_changed: Vec<LayerId> = Vec::new();
                    for layer in &comp.layers {
                        let Some(old_layer) = old_comp.layers.iter().find(|l| l.id == layer.id)
                        else {
                            continue;
                        };
                        if layer_shell_changed(layer, old_layer) {
                            shell_changed.push(layer.id);
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
                    // Layer Ref reads the referenced layer's shell (time
                    // placement) at process time — a document-side dependency
                    // invisible to the graph. Drop the scopes of layers whose
                    // networks reference a shell-changed layer so their
                    // layer.ref results recompute (REQ-LAYER-005).
                    if !shell_changed.is_empty() {
                        for layer in &comp.layers {
                            let mut targets = Vec::new();
                            crate::composition::validate::layer_ref_targets(
                                &layer.network,
                                &mut targets,
                            );
                            if targets.iter().any(|t| shell_changed.contains(t)) {
                                self.invalidate_scope(&[PathSegment::Layer(*comp_id, layer.id)]);
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
        self.evaluate_at(&[], graph, output, ctx)
    }

    /// [`evaluate`](Self::evaluate) with the ownership path seeded to
    /// `path`, so cache keys and [`EvalScope::path`] match an evaluation
    /// reached through the owners in `path` (e.g. previewing a node inside
    /// a layer's network: `&[PathSegment::Layer(comp, layer)]`,
    /// REQ-LAYER-007/011).
    pub fn evaluate_at(
        &mut self,
        path: &[PathSegment],
        graph: &Graph,
        output: NodeId,
        ctx: &EvalContext,
    ) -> Result<Arc<dyn NodeData>, EvalError> {
        self.path = path.to_vec();
        self.active_scopes = path.to_vec();
        self.bindings_stack.clear();
        self.bindings_stack.push(Vec::new());
        self.timings.clear();
        self.evaluate_inner(graph, output, ctx)
    }

    /// Per-node wall-clock durations of every `process()` run by the most
    /// recent top-level evaluation (cache hits report nothing). Keyed by
    /// [`NodeId`] alone — ids are globally unique, and the display consumer
    /// (node editor load readout) does not distinguish owner instances.
    pub fn take_timings(&mut self) -> Vec<(NodeId, std::time::Duration)> {
        std::mem::take(&mut self.timings)
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

        // Incoming edges (endpoint metadata only — nothing is pulled yet).
        let in_edges: Vec<(InputPortIndex, NodeId, crate::id::OutputPortIndex)> = graph
            .edges()
            .filter(|e| e.target == node)
            .map(|e| (e.target_port, e.source, e.source_port))
            .collect();

        // A bypassed node first derives its pass-through plan from the
        // declared port types: per output port, the single input port whose
        // connected value it passes through. Only those inputs are pulled —
        // unused inputs, parameter resolution, and the processor stay
        // untouched on the pass-through path, so a failing unused input or
        // parameter source cannot fail the bypass. A `None` plan (some
        // output port has no matching connected input) falls back to normal
        // processing below.
        let bypassed = node_ref.metadata.bypassed;
        let bypass_plan = if bypassed {
            bypass_passthrough_plan(&node_ref, &in_edges)
        } else {
            None
        };

        let mut input_values: Vec<Option<Arc<dyn NodeData>>> = vec![None; node_ref.inputs.len()];
        let mut any_input_fresh = false;

        if let Some(plan) = &bypass_plan {
            for (target_port, source, source_port) in in_edges
                .iter()
                .filter(|(port, _, _)| plan.contains(&(port.0 as usize)))
            {
                self.pull_input(
                    graph,
                    node,
                    *target_port,
                    *source,
                    *source_port,
                    ctx,
                    &mut input_values,
                    &mut any_input_fresh,
                    run,
                    visiting,
                )?;
            }
            if let Some(passed) = bypass_passthrough(&node_ref, &input_values, plan) {
                // Pass-through path: no parameter resolution, no processor
                // (no timing recorded — no work was done). Cache validity
                // consumes the freshness of the used inputs pulled above:
                // a recomputed used input re-runs the pass-through, same
                // as a normal node. There is no frame check — the value is
                // a pure function of the used inputs, and the processor
                // (which could declare time dependence) is never consulted
                // on this path.
                let cache_valid = !self.dirty.contains(&key)
                    && !any_input_fresh
                    && match self.cache.get(&key) {
                        Some(entry) => {
                            entry.bypassed
                                && entry.ctx.resolution == ctx.resolution
                                && entry.ctx.fps == ctx.fps
                        }
                        None => false,
                    };
                let result = if cache_valid {
                    // SAFETY of unwrap: cache_valid implies the entry exists.
                    let value = self.cache.get(&key).unwrap().value.clone();
                    (value, false)
                } else {
                    self.cache.insert(
                        key.clone(),
                        CacheEntry {
                            frame: ctx.frame,
                            ctx: *ctx,
                            bypassed: true,
                            value: passed.clone(),
                        },
                    );
                    self.dirty.remove(&key);
                    (passed, true)
                };
                visiting.remove(&key);
                run.insert(key, result.clone());
                return Ok(result);
            }
            // A selected input's value does not carry the output port's
            // type — only possible with a type-invalid edge (edge creation
            // is type-filtered). Fall through: pull the remaining inputs
            // and process the node normally.
        }

        // Evaluate upstream inputs into per-port slots (port order). Slots
        // a failed bypass attempt already pulled are skipped.
        for (target_port, source, source_port) in &in_edges {
            self.pull_input(
                graph,
                node,
                *target_port,
                *source,
                *source_port,
                ctx,
                &mut input_values,
                &mut any_input_fresh,
                run,
                visiting,
            )?;
        }

        let processor = self
            .processors
            .get(&node)
            .cloned()
            .ok_or(EvalError::MissingProcessor(node))?;

        // Parameter ports (REQ-LAYER-008 generalized): a connected
        // `is_param` port drives its parameter — strip the input so
        // processors never see it (all-input scanners like merge stay
        // correct) and convert the value before stored-parameter
        // resolution, so an overridden parameter's stored source is never
        // resolved (a dangling/cyclic stored binding must not fail the
        // node, and an overridden keyframed fallback must not force
        // per-frame recomputes). Unconnected ports and conversion failures
        // fall back to the stored parameter. Freshness of the driving
        // input is already in `any_input_fresh`.
        let mut overlays: Vec<(String, ResolvedValue)> = Vec::new();
        for (index, port) in node_ref.inputs.iter().enumerate() {
            if !port.is_param {
                continue;
            }
            let Some(value) = input_values[index].take() else {
                continue;
            };
            let Some(param) = node_ref.parameters.iter().find(|p| p.key == port.name) else {
                // Validate rejects this shape at document boundaries;
                // tolerate it at eval time.
                continue;
            };
            match param_port_overlay(&param.value, value.as_ref()) {
                Some(resolved) => overlays.push((port.name.clone(), resolved)),
                None => tracing::warn!(
                    node = node.raw(),
                    param = %port.name,
                    got = ?value.data_type_id(),
                    "parameter port value has an unconvertible type; \
                     falling back to the stored parameter"
                ),
            }
        }
        let overridden =
            |key: &str| -> bool { overlays.iter().any(|(overlaid, _)| overlaid == key) };

        let time_dependent =
            processor.is_time_dependent() || node_has_animated_params(&node_ref, &overridden);

        // Resolve stored parameters *before* the cache decision:
        // NodeOutput-bound parameters are hidden dependencies, and a
        // same-frame source change must force a recompute (REQ-LAYER-004).
        // Overridden keys are skipped and receive their overlay instead.
        let (mut params, params_fresh) =
            self.resolve_params(graph, &node_ref, ctx, run, visiting, &overridden)?;
        for (param_key, resolved) in overlays {
            params.set(&param_key, resolved);
        }

        // Decide whether the cached value is still valid: the resolution/FPS
        // must match for every node, the frame must match for time-dependent
        // ones, and the bypass flag must match — toggling bypass is a
        // metadata edit that keeps ports and wiring, so without this check a
        // same-frame pull could serve the stale processed result.
        let cache_valid = !self.dirty.contains(&key)
            && !any_input_fresh
            && !params_fresh
            && match self.cache.get(&key) {
                Some(entry) => {
                    entry.bypassed == bypassed
                        && entry.ctx.resolution == ctx.resolution
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
            let started = std::time::Instant::now();
            let produced = processor
                .process(&node_ref, ctx, &input_values, &params, self)
                .map_err(|source| EvalError::ProcessFailed { node, source });
            self.timings.push((node, started.elapsed()));
            self.processing.pop();
            let value = produced?;
            self.cache.insert(
                key.clone(),
                CacheEntry {
                    frame: ctx.frame,
                    ctx: *ctx,
                    bypassed,
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

    /// Pull the incoming edge at `target_port` of `node` into
    /// `input_values`, OR-ing the source's freshness into `any_input_fresh`.
    /// Slots already filled are skipped (the bypass plan may name one input
    /// for several output ports, and a failed bypass attempt re-enters the
    /// normal path with the used slots already pulled).
    #[allow(clippy::too_many_arguments)]
    fn pull_input(
        &mut self,
        graph: &Graph,
        node: NodeId,
        target_port: InputPortIndex,
        source: NodeId,
        source_port: crate::id::OutputPortIndex,
        ctx: &EvalContext,
        input_values: &mut [Option<Arc<dyn NodeData>>],
        any_input_fresh: &mut bool,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
    ) -> Result<(), EvalError> {
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
        if input_values[slot].is_some() {
            return Ok(());
        }
        let (value, fresh) = self.eval_node(graph, source, ctx, run, visiting)?;
        *any_input_fresh |= fresh;
        let port_count = graph.node(source).map(|n| n.outputs.len()).unwrap_or(1);
        let extracted = PortRecord::extract(&value, port_count, source_port).ok_or_else(|| {
            EvalError::ProcessFailed {
                node: source,
                source: anyhow::anyhow!(
                    "edge from port {source_port:?} has no value \
                         (port out of range or missing record)"
                ),
            }
        })?;
        input_values[slot] = Some(extracted);
        Ok(())
    }

    // ----- parameter resolution (REQ-LAYER-004) -----------------------------

    /// Build the per-frame [`ResolvedParams`] for `node`.
    ///
    /// Also returns whether any `NodeOutput` source resolved to a *fresh*
    /// (recomputed) value, which the caller uses to force a recompute of the
    /// consuming node even at the same frame.
    ///
    /// Parameters for which `skip` returns true (connected parameter ports)
    /// are not resolved at all — the caller overlays their port value.
    fn resolve_params(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
        skip: &dyn Fn(&str) -> bool,
    ) -> Result<(ResolvedParams, bool), EvalError> {
        let mut any_fresh = false;
        let mut values = Vec::with_capacity(node.parameters.len());
        for p in &node.parameters {
            if skip(&p.key) {
                continue;
            }
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

/// Convert a parameter-port input value to the [`ResolvedValue`] shape of
/// the parameter it drives (param-input-ports-plan Phase 2 conversion
/// rules): Scalar → Float / Int (rounded) / Bool (> 0.5), Vec2 → Channel2,
/// Color → Channel4. `None` when the wire value cannot drive the parameter
/// (the caller falls back to the stored value).
fn param_port_overlay(param: &ParameterValue, data: &dyn NodeData) -> Option<ResolvedValue> {
    use crate::types::{Color, Vec2};
    match param {
        ParameterValue::Float(_) | ParameterValue::Channel(_) => data
            .downcast_ref::<Scalar>()
            .map(|s| ResolvedValue::Float(s.0)),
        ParameterValue::Int(_) => data
            .downcast_ref::<Scalar>()
            .map(|s| ResolvedValue::Int(s.0.round() as i32)),
        ParameterValue::Bool(_) => data
            .downcast_ref::<Scalar>()
            .map(|s| ResolvedValue::Bool(s.0 > 0.5)),
        ParameterValue::Channel2(_) => data
            .downcast_ref::<Vec2>()
            .map(|v| ResolvedValue::Vec2([v.0, v.1])),
        ParameterValue::Channel4(_) => data
            .downcast_ref::<Color>()
            .map(|c| ResolvedValue::Vec4([c.r, c.g, c.b, c.a])),
        ParameterValue::String(_) | ParameterValue::Channel3(_) => None,
    }
}

// ===========================================================================
// Bypass pass-through
// ===========================================================================

/// The pass-through plan of a bypassed node: per output port (in port
/// order), the index of the input port whose connected value is passed
/// through — the first non-parameter input port that accepts the output
/// port's data type and has a connected edge. Declared port types stand in
/// for the runtime value types: edge creation is type-filtered, so a
/// selected slot's value always carries the output port's data type
/// ([`bypass_passthrough`] still verifies before committing).
///
/// `None` when any output port has no matching connected input (pure
/// generators, unconnected inputs). The caller then pulls every input,
/// resolves parameters, and runs the node's processor as usual: bypass is
/// *ignored* rather than an error, so a stale or hand-edited `bypassed`
/// flag can never fail evaluation. The editor UI only offers bypass on
/// nodes where every output port matches ([`Node::is_bypassable`]).
fn bypass_passthrough_plan(
    node: &Node,
    in_edges: &[(InputPortIndex, NodeId, crate::id::OutputPortIndex)],
) -> Option<Vec<usize>> {
    if node.outputs.is_empty() {
        return None;
    }
    node.outputs
        .iter()
        .map(|output| {
            node.inputs
                .iter()
                .enumerate()
                .find(|(slot, input)| {
                    !input.is_param
                        && (input.accepted_types.is_empty()
                            || input.accepted_types.contains(&output.data_type))
                        && in_edges
                            .iter()
                            .any(|(target_port, _, _)| target_port.0 as usize == *slot)
                })
                .map(|(slot, _)| slot)
        })
        .collect()
}

/// The pass-through value of a bypassed node: per output port (in port
/// order), the value of the input selected by the `plan`
/// ([`bypass_passthrough_plan`]), yielded unchanged — `process` is never
/// called.
///
/// Follows the output convention of [`PortRecord::extract`]: a single-output
/// node yields the matched value directly, a multi-output node yields a
/// [`PortRecord`] in output-port order.
///
/// `None` when a selected input's value does not carry the output port's
/// data type — only possible with a type-invalid edge (edge creation is
/// type-filtered, so a connected edge's value type matches the port's
/// declared type). The caller then pulls the remaining inputs and runs the
/// node's processor normally: bypass is *ignored* rather than an error.
fn bypass_passthrough(
    node: &Node,
    inputs: &[Option<Arc<dyn NodeData>>],
    plan: &[usize],
) -> Option<Arc<dyn NodeData>> {
    debug_assert_eq!(plan.len(), node.outputs.len());
    let mut values: Vec<Arc<dyn NodeData>> = Vec::with_capacity(plan.len());
    for (output, &slot) in node.outputs.iter().zip(plan) {
        let value = inputs.get(slot)?.as_ref()?;
        if value.data_type_id() != output.data_type {
            return None;
        }
        values.push(value.clone());
    }
    match values.len() {
        // Single-output convention: the value is yielded directly, not
        // wrapped in a record (same as `net.in`/`net.out`).
        1 => Some(values.pop().expect("one entry")),
        _ => Some(Arc::new(PortRecord(values))),
    }
}

/// Whether any parameter of `node` carries a time-varying source (keyframes,
/// expression, audio-reactive, or a node-output binding). Such nodes must be
/// re-evaluated when the frame advances even if the processor itself is
/// time-independent (REQ-LAYER-004). Parameters overridden by a connected
/// parameter port (`skip`) do not count — their stored source is inert.
fn node_has_animated_params(node: &Node, skip: &dyn Fn(&str) -> bool) -> bool {
    node.parameters.iter().any(|p| {
        if skip(&p.key) {
            return false;
        }
        match &p.value {
            ParameterValue::Channel(ch) => channel_is_time_varying(ch),
            ParameterValue::Channel2(chs) => chs.iter().any(channel_is_time_varying),
            ParameterValue::Channel3(chs) => chs.iter().any(channel_is_time_varying),
            ParameterValue::Channel4(chs) => chs.iter().any(channel_is_time_varying),
            _ => false,
        }
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

    // ---- process timings ----------------------------------------------------

    #[test]
    fn take_timings_reports_only_freshly_processed_nodes() {
        let g = Graph::new().add_node(scalar_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        let timings = ev.take_timings();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].0, NodeId::new(1));
        // Draining leaves nothing behind.
        assert!(ev.take_timings().is_empty());

        // A fully cached pull records no process timings.
        ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!(ev.take_timings().is_empty());
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

    // ---- parameter ports (param-input-ports-plan Phase 2) -------------------

    /// Echoes `value` (Float), `count` (Int), and `enabled` (Bool) into a
    /// Scalar, and asserts parameter-port inputs were stripped.
    struct MultiParamEcho;
    impl NodeProcessor for MultiParamEcho {
        fn process(
            &self,
            node: &Node,
            _ctx: &EvalContext,
            inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            for (index, port) in node.inputs.iter().enumerate() {
                if port.is_param {
                    anyhow::ensure!(
                        inputs[index].is_none(),
                        "param port input must be stripped before process"
                    );
                }
            }
            let value = params.f32_or("value", -1.0);
            let count = params.i32_or("count", -1) as f32;
            let enabled = if params.bool_or("enabled", false) {
                100.0
            } else {
                0.0
            };
            Ok(Arc::new(Scalar(value + count * 10.0 + enabled)))
        }
    }

    #[test]
    fn connected_param_ports_drive_and_convert_values() {
        // Scalar 2.6 drives: value (Float → 2.6), count (Int → round 3),
        // enabled (Bool → 2.6 > 0.5 → true).
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(0.0))
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let mut g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
            .unwrap()
            .expose_param_port(NodeId::new(2), "count")
            .unwrap()
            .expose_param_port(NodeId::new(2), "enabled")
            .unwrap();
        for (edge, port) in [(1u64, 0u32), (2, 1), (3, 2)] {
            g = g
                .add_edge(
                    EdgeId::new(edge),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(port),
                )
                .unwrap();
        }

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 2.6,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(MultiParamEcho));

        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        // 2.6 + 3*10 + 100 = 132.6
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 132.6).abs() < 1e-4);
    }

    #[test]
    fn vec2_and_color_param_ports_convert_componentwise() {
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
                Ok(Arc::new(crate::types::Vec2(3.0, -4.0)))
            }
        }
        struct Vec2Echo;
        impl NodeProcessor for Vec2Echo {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                let [x, y] = params.vec2_or("center", [0.0, 0.0]);
                Ok(Arc::new(Scalar(x * 100.0 + y)))
            }
        }
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::VEC2);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "center",
                ParameterValue::Channel2([
                    AnimationChannel::constant(0.0),
                    AnimationChannel::constant(0.0),
                ]),
            );
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "center")
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
        ev.register(NodeId::new(1), Arc::new(Vec2Source));
        ev.register(NodeId::new(2), Arc::new(Vec2Echo));
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 296.0).abs() < 1e-4);
    }

    #[test]
    fn unconnected_param_port_falls_back_to_stored_value() {
        let node = Node::new(NodeId::new(1), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(7.5))
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let g = Graph::new()
            .add_node(node)
            .unwrap()
            .expose_param_port(NodeId::new(1), "value")
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(MultiParamEcho));
        let out = ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 7.5).abs() < 1e-4);
    }

    #[test]
    fn param_port_change_recomputes_downstream() {
        // The driving edge is a real edge, so dirty propagation reaches the
        // consumer when the source is marked dirty (Params-style edit).
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(0.0))
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
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
        ev.register(NodeId::new(2), Arc::new(MultiParamEcho));
        let first = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((first.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < 1e-4);

        // Source value change: swap the processor and dirty the source —
        // the consumer must recompute through the edge.
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 4.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.mark_dirty(&g, NodeId::new(1));
        let second = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((second.downcast_ref::<Scalar>().unwrap().0 - 4.0).abs() < 1e-4);
    }

    #[test]
    fn connected_port_shields_a_broken_stored_binding() {
        // The stored parameter carries a NodeOutput binding to a missing
        // node; with the port connected the stored source must never be
        // resolved (its error would otherwise fail the whole node).
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel {
                    source: ChannelSource::NodeOutput(NodeId::new(999), OutputPortIndex(0)),
                }),
            )
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
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
                value: 5.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(MultiParamEcho));
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < 1e-4);
    }

    #[test]
    fn overridden_keyframed_param_does_not_disable_caching() {
        // A keyframed stored parameter would normally make the node
        // time-dependent; overridden by a constant-driving port, the frame
        // change must not force a recompute.
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "value",
                ParameterValue::Channel(AnimationChannel::keyframes(curve)),
            )
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 2.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        struct CountingParamEcho {
            calls: Arc<AtomicUsize>,
        }
        impl NodeProcessor for CountingParamEcho {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(Scalar(params.f32_or("value", -1.0))))
            }
        }
        ev.register(
            NodeId::new(2),
            Arc::new(CountingParamEcho {
                calls: calls.clone(),
            }),
        );

        let first = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((first.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < 1e-4);
        let second = ev.evaluate(&g, NodeId::new(2), &ctx_at(5)).unwrap();
        assert!((second.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < 1e-4);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "same overlaid params across frames stay cached"
        );
    }

    #[test]
    fn unconvertible_param_port_value_falls_back_with_warning() {
        // A FrameBuffer wired into a Float parameter port cannot convert;
        // the stored parameter value must win.
        struct FrameBufferSource;
        impl NodeProcessor for FrameBufferSource {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                Ok(Arc::new(crate::types::FrameBuffer {
                    width: 1,
                    height: 1,
                    data: vec![0.0; 4].into(),
                }))
            }
        }
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::FRAME_BUFFER);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(7.5))
            .with_param("count", ParameterValue::Int(0))
            .with_param("enabled", ParameterValue::Bool(false));
        let mut g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
            .unwrap();
        // Force the mismatched edge in (bypassing UI type filtering).
        g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(FrameBufferSource));
        ev.register(NodeId::new(2), Arc::new(MultiParamEcho));
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 7.5).abs() < 1e-4);
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

    // ---- bypass (NodeMetadata::bypassed pass-through) -----------------------

    fn bypassed_scalar_node(id: u64) -> Node {
        let mut node = scalar_node(id);
        node.metadata.bypassed = true;
        node
    }

    /// A processor that always fails; counts invocations so tests can prove
    /// it never ran.
    struct Failing {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for Failing {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow::anyhow!("evaluation failed"))
        }
    }

    #[test]
    fn bypassed_node_passes_through_input_without_processing() {
        // 1 → 2 where 2 is bypassed: output is input 1's value, unchanged,
        // and node 2's processor never runs.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(bypassed_scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let sum_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: sum_calls.clone(),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        // Pass-through: 5.0, not the processed 1 + 5 = 6.
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < f32::EPSILON);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bypass_passes_through_first_matching_input_in_port_order() {
        // Two same-type inputs: the first port's value wins.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(bypassed_scalar_node(3))
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
            Arc::new(CountingConst {
                value: 2.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bypass_ignores_failing_unused_input() {
        // 1 → 3.a (healthy), 2 → 3.b (fails upstream): the bypass passes
        // the FIRST matching input through, so the unused second input is
        // never evaluated and cannot fail the pass-through (previously the
        // whole evaluation failed).
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(bypassed_scalar_node(3))
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

        let fail_calls = Arc::new(AtomicUsize::new(0));
        let sum_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(Failing {
                calls: fail_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(CountingSum {
                calls: sum_calls.clone(),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < f32::EPSILON);
        assert_eq!(fail_calls.load(Ordering::Relaxed), 0);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bypass_ignores_failing_node_output_parameter_source() {
        // 1 → 2 (healthy), parameter of 2 ──NodeOutput binding──▶ 3
        // (fails): parameters are not resolved on the pass-through path,
        // so the binding's failing source cannot fail the bypass
        // (previously the whole evaluation failed).
        let mut bound = bypassed_scalar_node(2);
        bound.parameters.push(crate::graph::Parameter {
            key: "drive".to_string(),
            value: ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                NodeId::new(3),
                OutputPortIndex(0),
            ))),
        });
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(bound)
            .unwrap()
            .add_node(scalar_node(3))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let fail_calls = Arc::new(AtomicUsize::new(0));
        let sum_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 7.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: sum_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(Failing {
                calls: fail_calls.clone(),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 7.0).abs() < f32::EPSILON);
        assert_eq!(fail_calls.load(Ordering::Relaxed), 0);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bypass_matches_input_by_output_data_type() {
        // Inputs of different types: the output port's type selects which
        // input passes through (the Vec2 on port 0 must be skipped).
        let vec_source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::VEC2);
        let mut mixer = Node::new(NodeId::new(3), "test")
            .with_input("v", &[DataTypeId::VEC2])
            .with_input("s", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        mixer.metadata.bypassed = true;
        let g = Graph::new()
            .add_node(vec_source)
            .unwrap()
            .add_node(scalar_node(2))
            .unwrap()
            .add_node(mixer)
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

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Vec2Source));
        ev.register(
            NodeId::new(2),
            Arc::new(CountingConst {
                value: 7.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 7.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bypassed_multi_output_node_yields_port_record_in_output_order() {
        // Multi-output bypass: one matched input per output port, wrapped in
        // a PortRecord so downstream edges extract by source_port.
        let vec_source = Node::new(NodeId::new(2), "test").with_output("out", DataTypeId::VEC2);
        let mut multi = Node::new(NodeId::new(3), "test")
            .with_input("s", &[DataTypeId::SCALAR])
            .with_input("v", &[DataTypeId::VEC2])
            .with_output("x", DataTypeId::VEC2)
            .with_output("y", DataTypeId::SCALAR);
        multi.metadata.bypassed = true;
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(vec_source)
            .unwrap()
            .add_node(multi)
            .unwrap()
            .add_node(scalar_node(4))
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
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(3),
                OutputPortIndex(1),
                NodeId::new(4),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 3.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(NodeId::new(2), Arc::new(Vec2Source));
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

        // Pulled directly: a PortRecord in output-port order.
        let out = ev.evaluate(&g, NodeId::new(3), &ctx_at(0)).unwrap();
        let record = out.downcast_ref::<PortRecord>().unwrap();
        assert_eq!(record.0.len(), 2);
        let x = record.0[0].downcast_ref::<crate::types::Vec2>().unwrap();
        assert!((x.0 - 1.0).abs() < f32::EPSILON && (x.1 - 2.0).abs() < f32::EPSILON);
        assert!((record.0[1].downcast_ref::<Scalar>().unwrap().0 - 3.0).abs() < f32::EPSILON);

        // Pulled through a downstream edge on port 1: extraction works.
        let down = ev.evaluate(&g, NodeId::new(4), &ctx_at(0)).unwrap();
        // CountingSum adds 1: 1 + 3 = 4.
        assert!((down.downcast_ref::<Scalar>().unwrap().0 - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bypassed_pure_generator_is_processed_normally() {
        // A node with no inputs cannot pass anything through: bypass is
        // ignored and the processor runs (the UI disables bypass for such
        // nodes; a stale/hand-edited flag must not fail evaluation).
        let mut generator =
            Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        generator.metadata.bypassed = true;
        let g = Graph::new().add_node(generator).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 9.0,
                calls: calls.clone(),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 9.0).abs() < f32::EPSILON);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bypassed_node_with_unconnected_input_is_processed_normally() {
        // A type-matching input port exists but is not connected: nothing to
        // pass through, so the processor runs as if not bypassed.
        let g = Graph::new().add_node(bypassed_scalar_node(1)).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingSum {
                calls: calls.clone(),
            }),
        );

        let out = ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        // CountingSum with no inputs yields its base 1.0.
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn toggling_bypass_invalidates_the_cached_result() {
        // 1 → 2. Toggling bypass is a metadata edit via Graph::replace_node;
        // the cached processed value must not be served afterwards, and
        // toggling back must restore processing — all without any dirty
        // marking (the flag is part of cache validity).
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

        let sum_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: sum_calls.clone(),
            }),
        );

        let with_bypass = |g: &Graph, bypassed: bool| {
            let mut node = (**g.node(NodeId::new(2)).unwrap()).clone();
            node.metadata.bypassed = bypassed;
            g.clone().replace_node(Arc::new(node))
        };

        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 6.0).abs() < f32::EPSILON);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 1);

        // Bypass on, same frame, no invalidation: pass-through, no process.
        let g = with_bypass(&g, true);
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < f32::EPSILON);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 1);

        // Bypass off again: the original processed result is restored.
        let g = with_bypass(&g, false);
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 6.0).abs() < f32::EPSILON);
        assert_eq!(sum_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn bypassed_node_caches_its_pass_through() {
        // A clean bypassed node is served from cache like any other node:
        // the upstream is not re-pulled and the same Arc comes back.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(bypassed_scalar_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let const_calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: const_calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let first = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        let second = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert_eq!(const_calls.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn bypassed_node_tracks_fresh_input_across_frames() {
        // A time-dependent upstream invalidates the bypassed node's cached
        // pass-through through input freshness, not the frame check.
        let g = Graph::new()
            .add_node(scalar_node(1))
            .unwrap()
            .add_node(bypassed_scalar_node(2))
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
            Arc::new(FrameSource {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(CountingSum {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let out0 = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out0.downcast_ref::<Scalar>().unwrap().0 - 0.0).abs() < f32::EPSILON);
        let out5 = ev.evaluate(&g, NodeId::new(2), &ctx_at(5)).unwrap();
        assert!((out5.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < f32::EPSILON);
    }
}

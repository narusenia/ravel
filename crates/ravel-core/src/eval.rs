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
//!   re-evaluated when the [`EvalContext`] moves in time. Nodes with animated
//!   parameters (keyframed channels, node-output bindings) count as
//!   time-dependent. What a cached value is specific to — the quantised
//!   position ([`TimeKey`]), resolution, frame rate, [`Precision`] and the
//!   bypass flag — is stated once, in `CacheIdentity`.
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
use crate::cache_budget::{SharedCacheBudget, Tier};
use crate::composition::compile::{NodeRole, deterministic_node_id};
use crate::composition::{Document, Layer};
use crate::graph::{Graph, Node, ParameterValue};
use crate::id::{CompId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
use crate::network;
use crate::types::{FrameRate, NodeData, PortRecord, Scalar};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

/// Maximum number of recursively pulled nodes (including parameter-source
/// bindings) in one evaluation branch. Returning an error at this boundary
/// keeps malformed or adversarial graphs from overflowing the process stack.
pub const MAX_EVALUATION_DEPTH: usize = 256;

#[derive(Clone, Copy)]
struct ResolveBudget {
    owner: NodeId,
    depth: usize,
}

impl ResolveBudget {
    fn deeper(self) -> Self {
        Self {
            depth: self.depth + 1,
            ..self
        }
    }
}

struct ResolveOptions<'a> {
    skip: &'a dyn Fn(&str) -> bool,
    budget: ResolveBudget,
}

// ===========================================================================
// Errors
// ===========================================================================

/// Errors that can occur while evaluating the node graph.
#[derive(Debug, Error)]
pub enum EvalError {
    /// A cycle was encountered during the recursive pull.
    #[error("cycle detected during evaluation at node {0}")]
    CycleDetected(NodeId),

    #[error("evaluation depth exceeded the limit of {limit} at node {node}")]
    DepthLimitExceeded { node: NodeId, limit: usize },

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
// Cache identity axes: TimeKey / Precision
// ===========================================================================

/// A frame position quantised to 1/[`TimeKey::SUBFRAME_SCALE`] of a frame.
///
/// The same instant is reached by different arithmetic routes — `frame / fps`,
/// a shutter offset around a centre time, a time remap — which agree to within
/// a few ULP but not bit-exactly. Keying the cache on raw `f64` bits would turn
/// that into a silent full miss with no way to trace it, so every route is
/// quantised through [`TimeKey::from_frame_position`], the single rounding site
/// in the engine.
///
/// The quantum is chosen so the collision condition can be *stated*, not so
/// that collisions are impossible: 1/4096 frame is exactly one motion-blur
/// sample interval at the smallest useful shutter angle (11.25° = 1/32 frame)
/// with 128 samples, and 128 ticks apart in the 360° / 32 samples case.
/// `motion-blur-plan.md`'s sample-count clamp reads [`TimeKey::SUBFRAME_SCALE`]
/// to warn when a requested interval would fall below one tick instead of
/// silently collapsing the samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeKey(i64);

impl TimeKey {
    /// Ticks per frame. One tick is the finest distinguishable sub-frame
    /// position.
    pub const SUBFRAME_SCALE: f64 = 4096.0;

    /// The key of a value that does not depend on time at all.
    ///
    /// Time-independent nodes (constants, static generators) carry this so
    /// their cached value keeps being served across frames.
    pub const TIMELESS: TimeKey = TimeKey(i64::MIN);

    /// Quantise a continuous frame position (see
    /// [`EvalContext::sample_frame`]).
    ///
    /// Rounding is half-away-from-zero and happens only here, so two routes
    /// that compute the same position differently land on the same tick.
    /// A non-finite or out-of-range position saturates (`NaN` → tick 0) and
    /// is kept off [`TimeKey::TIMELESS`], which no real position may claim.
    pub fn from_frame_position(frames: f64) -> Self {
        let ticks = (frames * Self::SUBFRAME_SCALE).round() as i64;
        TimeKey(ticks.max(i64::MIN + 1))
    }

    /// The quantised position in ticks. [`TimeKey::TIMELESS`] has no frame
    /// position and yields `i64::MIN`.
    pub fn ticks(self) -> i64 {
        self.0
    }

    /// Whether this is the time-independent key.
    pub fn is_timeless(self) -> bool {
        self == Self::TIMELESS
    }
}

/// Storage precision of a cached value, ordered `U8 < F16 < F32`.
///
/// The one cache-identity axis compared by order instead of equality: a stored
/// entry serves a request whose floor it meets or exceeds, because handing a
/// higher-precision value to a lower requirement is lossless and needs no
/// conversion. The reverse misses, which is what keeps an export from picking
/// up a reduced preview entry (`cache-plan.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Precision {
    /// 8-bit unsigned normalised.
    U8,
    /// 16-bit float.
    F16,
    /// 32-bit float — the working precision of every pixel operation.
    #[default]
    F32,
}

/// How much work an evaluation is allowed to spend on the same picture.
///
/// The sample-count axis: motion blur (and later effects that trade samples
/// for speed) reads it to decide how many shutter samples to integrate. It is
/// **orthogonal to resolution** — the viewer's `ViewerResolution` factor scales
/// `EvalContext::resolution`, and either factor combines with either stage.
///
/// Unlike [`Precision`] this axis has **no order and no downgrade**. A picture
/// built from two shutter samples is not "the same frame, coarser": it is a
/// different image, so a [`Quality::Preview`] entry may never answer a
/// [`Quality::Final`] request and a `Final` entry may never answer a `Preview`
/// one either. Deriving `PartialOrd` would make that mistake expressible, so
/// this type deliberately does not — the only comparison the cache can write
/// is equality. For the same reason the axis is excluded from any future
/// approximate-hit scheme (`cache-plan.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Interactive preview: the fewest samples that still shows the effect.
    Preview,
    /// Full quality, as delivered to an export. The default, so a path that
    /// never mentions quality cannot silently ship a preview-grade picture.
    #[default]
    Final,
}

// ===========================================================================
// EvalContext
// ===========================================================================

/// Per-evaluation context describing the point in time being rendered and the
/// target output configuration.
///
/// Internal processing is always 32-bit float with no artificial resolution or
/// frame-rate limits (REQ-CORE-009); `resolution` is therefore an unconstrained
/// `(u32, u32)`. Geometry coordinates are expressed relative to
/// `comp_resolution` and scaled to the output canvas by pixel-producing nodes.
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
    /// Composition-space resolution used as the geometry coordinate basis.
    pub comp_resolution: (u32, u32),
    /// Lowest storage precision this evaluation accepts.
    ///
    /// Requests declare a floor; a cached entry records the floor it was
    /// produced under and is reused only when that guarantee still covers the
    /// request (see [`Precision`]). Preview paths may lower it; an export
    /// leaves it at [`Precision::F32`] so it can never be served a reduced
    /// entry. The default is [`Precision::F32`].
    pub min_precision: Precision,
    /// Quality stage this evaluation is produced for.
    ///
    /// Interactive paths lower it to [`Quality::Preview`]; everything else —
    /// an export above all — leaves it at the default [`Quality::Final`].
    /// Matched by equality in the cache and never downgraded (see
    /// [`Quality`]).
    pub quality: Quality,
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
            comp_resolution: resolution,
            min_precision: Precision::F32,
            quality: Quality::Final,
        }
    }

    /// Use `comp_resolution` as the coordinate basis for this evaluation.
    pub fn with_comp_resolution(mut self, comp_resolution: (u32, u32)) -> Self {
        self.comp_resolution = comp_resolution;
        self
    }

    /// Accept cached values stored at `min_precision` or above.
    pub fn with_min_precision(mut self, min_precision: Precision) -> Self {
        self.min_precision = min_precision;
        self
    }

    /// Evaluate for `quality`. Values produced under one stage are never
    /// served to the other (see [`Quality`]).
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }

    /// Continuous frame position for sampling animation channels.
    ///
    /// Keyframes are anchored to integer frames, but a context may sit between
    /// them: motion blur evaluates a shutter interval and time remapping maps
    /// onto fractional source frames. `frame` stays the integer index used for
    /// keyframe editing and the UI, while `time` carries the continuous
    /// position.
    ///
    /// Expressed as the integer frame plus the sub-frame offset implied by
    /// `time`, so a context that sits exactly on the frame grid returns
    /// `frame` bit-exactly whatever the frame rate (30000/1001 included).
    pub fn sample_frame(&self) -> f64 {
        let fps = self.fps.as_f64();
        let frame_time = self.frame as f64 / fps;
        self.frame as f64 + (self.time - frame_time) * fps
    }

    /// Scale factor from composition space to the output canvas, per axis.
    /// Composition-space geometry (shell transforms, shape coordinates) is
    /// multiplied by this when it produces pixels; `1.0` when the canvas is
    /// the composition itself, which is the case for every UI-side context.
    pub fn comp_to_canvas_scale(&self) -> (f64, f64) {
        (
            self.resolution.0 as f64 / self.comp_resolution.0 as f64,
            self.resolution.1 as f64 / self.comp_resolution.1 as f64,
        )
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
    /// The `i`th evaluation of an iteration node.
    Iteration(NodeId, u32),
    /// Evaluation beneath a time-shift node, identified by the shifted frame.
    ///
    /// This segment is reserved for time remapping. Motion blur samples
    /// sequentially and does not use it, nor does a layer shell's `time_remap`,
    /// which places the entire layer at one time.
    TimeShift(NodeId, u64),
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

/// Binding names whose value is not the same `Arc` in `old` and `new`,
/// including names present in only one of the two.
///
/// The single place binding identity is decided. Both consumers read this
/// result: scoped invalidation drops what the named ports reach, and the
/// interface node's per-port freshness reports only the named ports as new
/// (`CacheMiss::BindingsChanged`). An empty result means the scope may reuse
/// everything it cached.
///
/// Identity is pointer equality, so the answer is conservative in the safe
/// direction: an unchanged value rebuilt into a fresh `Arc` counts as
/// changed, a changed value never counts as unchanged.
fn binding_delta(
    old: &[(String, Arc<dyn NodeData>)],
    new: &[(String, Arc<dyn NodeData>)],
) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (name, value) in new {
        let same = old
            .iter()
            .find(|(n, _)| n == name)
            .is_some_and(|(_, previous)| Arc::ptr_eq(previous, value));
        if !same {
            changed.push(name.clone());
        }
    }
    for (name, _) in old {
        if !new.iter().any(|(n, _)| n == name) {
            changed.push(name.clone());
        }
    }
    changed
}

/// Whether `key` names a cached value that the `affected` nodes of `scope`
/// invalidate.
///
/// Keys outside the scope are untouched. A key *in* the scope is decided by
/// its own node; a key in a nested scope beneath it is decided by the node
/// that opened that nested scope, so a subnet's inner cache follows the
/// subnet node.
fn binding_change_affects(
    scope: &[PathSegment],
    affected: &HashSet<NodeId>,
    key: &NodeKey,
) -> bool {
    if !key.path.starts_with(scope) {
        return false;
    }
    match key.path.get(scope.len()) {
        None => affected.contains(&key.node),
        Some(segment) => match scope_owner_node(segment) {
            Some(owner) => affected.contains(&owner),
            // A layer or composition segment names no node of this graph, so
            // there is nothing to compare it against. A `layer.ref` inside a
            // network opens exactly such a scope, so this arm is reachable —
            // drop conservatively rather than guess.
            None => true,
        },
    }
}

/// The node of the enclosing graph that owns the scope `segment` opens, if
/// the segment names one.
fn scope_owner_node(segment: &PathSegment) -> Option<NodeId> {
    match segment {
        PathSegment::Subnet(node)
        | PathSegment::Iteration(node, _)
        | PathSegment::TimeShift(node, _) => Some(*node),
        PathSegment::Layer(_, _) | PathSegment::Comp(_) => None,
    }
}

/// Output-port indices of `node` whose value comes from one of the bindings
/// in `changed`.
fn rebound_output_ports(node: &Node, changed: &[String]) -> Vec<usize> {
    if changed.is_empty() {
        return Vec::new();
    }
    node.outputs
        .iter()
        .enumerate()
        .filter(|(_, port)| changed.contains(&port.name))
        .map(|(index, _)| index)
        .collect()
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
    /// Path control points pass through unresolved (constant-only in v1).
    PathPoints(Vec<crate::graph::PathPoint>),
    /// A scalar transfer curve passes through unresolved: the curve's own
    /// shape is not animatable in v1, and no wire type carries one.
    Curve(crate::param_curve::CurveParam),
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

    /// Path control points parameter, if present.
    pub fn path_points(&self, key: &str) -> Option<&[crate::graph::PathPoint]> {
        match self.get(key) {
            Some(ResolvedValue::PathPoints(points)) => Some(points),
            _ => None,
        }
    }

    /// Transfer curve parameter, if present and a curve.
    pub fn curve(&self, key: &str) -> Option<&crate::param_curve::CurveParam> {
        match self.get(key) {
            Some(ResolvedValue::Curve(curve)) => Some(curve),
            _ => None,
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

    /// Whether a change to this processor's node requires constructing it
    /// again.
    ///
    /// A processor that captured values off the node at construction has to be
    /// rebuilt when they change, so the default is `true` — a new node type is
    /// correct without touching this. Processors that read everything they need
    /// from the `node` and `params` handed to [`Self::process`] hold nothing
    /// stale and override it to `false`; the evaluator then only drops their
    /// cached values (see [`Evaluator::invalidate_node`]) instead of paying for
    /// a rebuild, which for the GPU processors means recompiling a shader and
    /// recreating a compute pipeline on every parameter edit.
    fn rebuild_on_node_change(&self) -> bool {
        true
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
// Processor registration
// ===========================================================================

/// Somewhere node processors can be registered.
///
/// [`Evaluator`] is the obvious implementation; the point of the trait is the
/// *other* one. The evaluation worker hands its hook a restricted view rather
/// than the evaluator itself (see `runtime::eval_service::ProcessorSync`), so
/// registration helpers like `ravel_nodes::register_all_processors` are
/// written against this instead of against `&mut Evaluator`.
pub trait ProcessorRegistry {
    /// Register (or replace) the processor for `node`, invalidating it.
    fn register(&mut self, node: NodeId, processor: Arc<dyn NodeProcessor>);

    /// The processor currently registered for `node`.
    fn processor(&self, node: NodeId) -> Option<&Arc<dyn NodeProcessor>>;

    /// Drop `node`'s cached values and mark it dirty, keeping its processor.
    fn invalidate_node(&mut self, node: NodeId);
}

impl ProcessorRegistry for Evaluator {
    fn register(&mut self, node: NodeId, processor: Arc<dyn NodeProcessor>) {
        Evaluator::register(self, node, processor);
    }

    fn processor(&self, node: NodeId) -> Option<&Arc<dyn NodeProcessor>> {
        Evaluator::processor(self, node)
    }

    fn invalidate_node(&mut self, node: NodeId) {
        Evaluator::invalidate_node(self, node);
    }
}

// ===========================================================================
// Cache entry
// ===========================================================================

/// Everything about an evaluation that a cached value is *specific to*.
///
/// One place decides what "the same evaluation" means, so the rule cannot
/// drift between the pass-through path, the processing path and the layers
/// that will key on it later (`cache-plan.md`). Every axis but
/// [`Precision`] is matched by equality; precision is matched by order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheIdentity {
    /// Quantised frame position, or [`TimeKey::TIMELESS`] when the value does
    /// not depend on time.
    time: TimeKey,
    /// Target output resolution.
    resolution: (u32, u32),
    /// Composition-space coordinate basis.
    comp_resolution: (u32, u32),
    /// Frame rate of the timeline the value was produced for.
    fps: FrameRate,
    /// Storage precision the value is guaranteed to hold.
    precision: Precision,
    /// Quality stage the value was produced under. Equality, never order:
    /// a preview-grade picture is a different image, not a coarser copy of
    /// the final one, so neither stage substitutes for the other.
    quality: Quality,
    /// The node's bypass flag when this value was produced. Toggling bypass
    /// is a metadata edit that keeps ports and wiring, so the flag is part
    /// of cache validity: a pull after a toggle must not serve the stale
    /// processed (or pass-through) result.
    bypassed: bool,
}

impl CacheIdentity {
    /// The identity of a value produced for `ctx`.
    ///
    /// `time_dependent` selects the time axis: a time-varying node (a
    /// time-dependent processor or animated parameters) is specific to the
    /// quantised position, everything else is [`TimeKey::TIMELESS`] and keeps
    /// being served across frames.
    fn of(ctx: &EvalContext, time_dependent: bool, bypassed: bool) -> Self {
        Self {
            time: if time_dependent {
                TimeKey::from_frame_position(ctx.sample_frame())
            } else {
                TimeKey::TIMELESS
            },
            resolution: ctx.resolution,
            comp_resolution: ctx.comp_resolution,
            fps: ctx.fps,
            precision: ctx.min_precision,
            quality: ctx.quality,
            bypassed,
        }
    }

    /// Why a value stored under `self` cannot answer a request for `wanted`,
    /// or `None` when it can.
    fn mismatch(&self, wanted: &Self) -> Option<CacheMiss> {
        if self.bypassed != wanted.bypassed {
            Some(CacheMiss::BypassToggled)
        } else if self.resolution != wanted.resolution
            || self.comp_resolution != wanted.comp_resolution
        {
            Some(CacheMiss::ResolutionChanged)
        } else if self.fps != wanted.fps {
            Some(CacheMiss::FpsChanged)
        } else if self.time != wanted.time {
            Some(CacheMiss::FrameAdvanced)
        } else if self.quality != wanted.quality {
            // Equality in both directions on purpose: `Preview` cannot answer
            // `Final` (the export would ship a coarse picture) and `Final`
            // cannot answer `Preview` either, because the two are different
            // images rather than two grades of one.
            Some(CacheMiss::QualityChanged)
        } else if self.precision < wanted.precision {
            // The only ordered axis: a stored value at or above the requested
            // floor is handed over as-is, never converted.
            Some(CacheMiss::PrecisionInsufficient)
        } else {
            None
        }
    }
}

struct CacheEntry {
    /// What this value is specific to. A request whose identity this one
    /// covers is served from `value`.
    identity: CacheIdentity,
    value: Arc<dyn NodeData>,
    /// The budget claim this value holds. Dropping the entry releases it, so
    /// no removal path has to remember to. `None` when the evaluator runs
    /// without a budget (tests, examples, the unbounded pre-`CACHE-3`
    /// behaviour).
    reservation: Option<crate::cache_budget::Reservation>,
    /// What the value cost when it was stored, and where. Kept alongside the
    /// reservation so the store's own per-tier totals stay right even without
    /// a budget, and so a value whose size changed after caching (it cannot —
    /// values are immutable) could never desynchronise the accounting.
    bytes: u64,
    tier: Tier,
}

/// Why a cached value could not be served for a node pull. Surfaced in
/// `trace`/`debug` logs so a stale-looking frame can be classified as a
/// genuine recompute or a cache bug, and counted per reason by
/// [`Evaluator::cache_stats`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CacheMiss {
    /// The node (or upstream) was marked dirty by an edit.
    Dirty,
    /// An input edge delivered a freshly recomputed value in this pull.
    InputFresh,
    /// A `NodeOutput`-bound parameter resolved to a fresh value.
    ParamsFresh,
    /// The node's bypass flag changed since the entry was computed.
    BypassToggled,
    /// The cached entry was computed at a different resolution.
    ResolutionChanged,
    /// The cached entry was computed at a different frame rate.
    FpsChanged,
    /// The node is time-varying and the evaluated position moved since the
    /// entry was computed (a new frame, or a new sub-frame position within
    /// the same frame).
    FrameAdvanced,
    /// The cached entry was computed for the other quality stage. Symmetric:
    /// switching either way misses, because the stages are different images
    /// rather than two grades of one (see [`Quality`]).
    QualityChanged,
    /// The cached entry is stored below the precision the request demands.
    PrecisionInsufficient,
    /// A network interface node whose scope was re-entered with a different
    /// value bound to one of its output ports. Bindings are values rather
    /// than context, so they cannot live in [`CacheIdentity`]; keeping the
    /// reason distinct is what lets the interface node report freshness per
    /// output port instead of poisoning every consumer (MED-CORE-02).
    BindingsChanged,
    /// No cached entry exists for this node at this path.
    NoEntry,
}

impl CacheMiss {
    /// Every reason, in the order [`CacheMiss::index`] assigns.
    pub const ALL: [CacheMiss; 11] = [
        CacheMiss::Dirty,
        CacheMiss::InputFresh,
        CacheMiss::ParamsFresh,
        CacheMiss::BypassToggled,
        CacheMiss::ResolutionChanged,
        CacheMiss::FpsChanged,
        CacheMiss::FrameAdvanced,
        CacheMiss::QualityChanged,
        CacheMiss::PrecisionInsufficient,
        CacheMiss::BindingsChanged,
        CacheMiss::NoEntry,
    ];

    /// How many reasons there are.
    pub const COUNT: usize = Self::ALL.len();

    /// This reason's slot in the per-reason tallies.
    pub const fn index(self) -> usize {
        match self {
            CacheMiss::Dirty => 0,
            CacheMiss::InputFresh => 1,
            CacheMiss::ParamsFresh => 2,
            CacheMiss::BypassToggled => 3,
            CacheMiss::ResolutionChanged => 4,
            CacheMiss::FpsChanged => 5,
            CacheMiss::FrameAdvanced => 6,
            CacheMiss::QualityChanged => 7,
            CacheMiss::PrecisionInsufficient => 8,
            CacheMiss::BindingsChanged => 9,
            CacheMiss::NoEntry => 10,
        }
    }

    /// Stable identifier used in logs and statistics.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheMiss::Dirty => "dirty",
            CacheMiss::InputFresh => "input_fresh",
            CacheMiss::ParamsFresh => "params_fresh",
            CacheMiss::BypassToggled => "bypass_toggled",
            CacheMiss::ResolutionChanged => "resolution_changed",
            CacheMiss::FpsChanged => "fps_changed",
            CacheMiss::FrameAdvanced => "frame_advanced",
            CacheMiss::QualityChanged => "quality_changed",
            CacheMiss::PrecisionInsufficient => "precision_insufficient",
            CacheMiss::BindingsChanged => "bindings_changed",
            CacheMiss::NoEntry => "no_entry",
        }
    }
}

// ===========================================================================
// Cache store
// ===========================================================================

/// The evaluator's cache, dirty set, byte accounting and reverse index, with
/// **no way to reach any of them directly**.
///
/// This is a module, not just a struct, on purpose. The four collections have
/// to move together: a cached value owns a budget reservation, a `(node,
/// path)` pair has to appear in the reverse index exactly while it is present
/// in the cache or the dirty set, and the per-tier byte totals have to match
/// what is stored. There were nine places that mutated the two maps directly
/// and one forgotten index update would have been a silent wrong answer, so
/// the fields are private to `cache_store` and every mutation goes through
/// the handful of methods below (MED-CORE-07).
mod cache_store {
    use super::{CacheEntry, CacheIdentity, NodeKey, PathSegment};
    use crate::cache_budget::{CacheKind, ReservationId, SharedCacheBudget, Tier};
    use crate::id::NodeId;
    use crate::types::NodeData;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    /// Why a `(node, path)` pair is in the reverse index.
    ///
    /// A pair leaves the index when both reasons are gone; keeping one flag
    /// per collection is what lets `cache` and `dirty` be pruned
    /// independently without the index guessing.
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    struct Presence {
        cached: bool,
        dirty: bool,
    }

    impl Presence {
        fn is_empty(self) -> bool {
            !self.cached && !self.dirty
        }
    }

    /// Cache + dirty set + budget accounting + `NodeId → paths` index.
    #[derive(Default)]
    pub(super) struct CacheStore {
        entries: HashMap<NodeKey, CacheEntry>,
        dirty: HashSet<NodeKey>,
        /// `NodeId → the paths that node appears at`, maintained by every
        /// mutation below.
        ///
        /// Exists so [`Self::forget_node`] — reached from `register()` on
        /// every parameter tick — costs the node's own paths instead of a
        /// walk over the whole cache (MED-CORE-07).
        by_node: HashMap<NodeId, HashMap<Vec<PathSegment>, Presence>>,
        /// Reservation → the key that owns it, so an eviction list can be
        /// turned back into cache keys.
        by_reservation: HashMap<ReservationId, NodeKey>,
        budget: Option<SharedCacheBudget>,
        /// Bytes cached per [`Tier`], in [`Tier::ALL`] order.
        used: [u64; 3],
        /// Cache/dirty entries this store has looked at, ever.
        ///
        /// The observable in the MED-CORE-07 regression test: "`register()`
        /// does not scan the cache" is a statement about how many entries a
        /// call touches, and timing it would only produce a flaky test.
        #[cfg(test)]
        entries_examined: usize,
    }

    impl CacheStore {
        /// A store that reports to `budget`. Without one it is unbounded —
        /// the behaviour every test and example keeps.
        pub(super) fn new(budget: Option<SharedCacheBudget>) -> Self {
            Self {
                budget,
                ..Self::default()
            }
        }

        /// The budget this store answers to, so a structural reset can build
        /// a fresh store that still answers to the same one.
        pub(super) fn budget(&self) -> Option<&SharedCacheBudget> {
            self.budget.as_ref()
        }

        #[cfg(test)]
        pub(super) fn entries_examined(&self) -> usize {
            self.entries_examined
        }

        #[cfg(test)]
        fn examine(&mut self, count: usize) {
            self.entries_examined += count;
        }

        #[cfg(not(test))]
        #[inline]
        fn examine(&mut self, _count: usize) {}

        // ----- reads --------------------------------------------------------

        /// The cached entry for `key`, if any. Does not count as a use.
        pub(super) fn peek(&mut self, key: &NodeKey) -> Option<&CacheEntry> {
            self.examine(1);
            self.entries.get(key)
        }

        /// The cached value for `key`, marking it as most recently used.
        ///
        /// Separate from [`Self::peek`] because only a served value is a use:
        /// promoting on every validity probe would make the eviction order
        /// meaningless.
        pub(super) fn get_used(&mut self, key: &NodeKey) -> Option<Arc<dyn NodeData>> {
            self.examine(1);
            let entry = self.entries.get(key)?;
            let value = entry.value.clone();
            if let (Some(budget), Some(reservation)) = (&self.budget, &entry.reservation) {
                budget.touch(reservation.id());
            }
            Some(value)
        }

        /// Whether `key` is marked dirty.
        pub(super) fn is_dirty(&self, key: &NodeKey) -> bool {
            self.dirty.contains(key)
        }

        /// Whether a value is cached for `key`. Neither a use nor an examine:
        /// invalidation assertions must not perturb what they measure.
        #[cfg(test)]
        pub(super) fn contains(&self, key: &NodeKey) -> bool {
            self.entries.contains_key(key)
        }

        /// Number of cached values.
        pub(super) fn len(&self) -> usize {
            self.entries.len()
        }

        /// Bytes cached in each [`Tier`], in [`Tier::ALL`] order.
        pub(super) fn used_bytes(&self) -> [u64; 3] {
            self.used
        }

        // ----- writes -------------------------------------------------------

        /// Store `value` for `key`, clear its dirty flag, and drop whatever
        /// the budget says has to go to make room.
        ///
        /// The eviction list never contains `key` itself: a value the
        /// evaluator just produced is not thrown away before it is returned.
        pub(super) fn insert(
            &mut self,
            key: NodeKey,
            identity: CacheIdentity,
            value: Arc<dyn NodeData>,
        ) {
            self.unmark_dirty(&key);
            // Replacing an entry releases the old claim first, so a node
            // recomputed every frame does not accumulate reservations.
            self.drop_value(&key);

            let bytes = value.byte_size();
            // A GPU-resident value costs VRAM, not host memory: the tier is a
            // property of the value, which is why `CacheKind::NodeResult`
            // carries one.
            let tier = if value.is_gpu_resident() {
                Tier::Vram
            } else {
                Tier::Ram
            };
            let (reservation, evicted) = match &self.budget {
                Some(budget) => {
                    let (reservation, evicted) = budget.reserve(CacheKind::NodeResult(tier), bytes);
                    (Some(reservation), evicted)
                }
                None => (None, Vec::new()),
            };
            if let Some(reservation) = &reservation {
                self.by_reservation.insert(reservation.id(), key.clone());
            }
            self.used[tier_index(tier)] += bytes;
            self.index_mut(&key).cached = true;
            self.examine(1);
            self.entries.insert(
                key,
                CacheEntry {
                    identity,
                    value,
                    reservation,
                    bytes,
                    tier,
                },
            );

            for entry in evicted {
                let Some(victim) = self.by_reservation.remove(&entry.id) else {
                    // Not one of ours. Skipping is only correct because the
                    // owning consumer drops it — an eviction the owner
                    // ignores leaves the budget counting fewer bytes than the
                    // process holds. Nothing else reserves today; `CACHE-5`
                    // and `CACHE-8` each act on the ids they own.
                    continue;
                };
                self.drop_value(&victim);
                self.prune_index(&victim);
            }
        }

        /// Mark `key` dirty. Returns whether it was not already.
        pub(super) fn mark_dirty(&mut self, key: NodeKey) -> bool {
            if self.dirty.contains(&key) {
                return false;
            }
            self.index_mut(&key).dirty = true;
            self.dirty.insert(key);
            true
        }

        /// Clear `key`'s dirty flag.
        pub(super) fn unmark_dirty(&mut self, key: &NodeKey) {
            if self.dirty.remove(key) {
                if let Some(presence) = self.index_get_mut(key) {
                    presence.dirty = false;
                }
                self.prune_index(key);
            }
        }

        /// Drop `key`'s cached value *and* dirty flag.
        ///
        /// The two always move together at every call site — a stale value
        /// left behind a cleared dirty flag would be served on the next pull.
        pub(super) fn remove(&mut self, key: &NodeKey) {
            self.drop_value(key);
            self.dirty.remove(key);
            if let Some(presence) = self.index_get_mut(key) {
                presence.dirty = false;
            }
            self.prune_index(key);
        }

        /// Drop everything about `node`, at every path it appears at.
        ///
        /// Returns the paths it was found at, for the caller's scope-owner
        /// invalidation. **O(paths of `node`)**, not O(cache): this is the
        /// call `register()` makes on every parameter tick, and walking the
        /// cache here was MED-CORE-07's second half.
        pub(super) fn forget_node(&mut self, node: NodeId) -> Vec<Vec<PathSegment>> {
            let Some(paths) = self.by_node.remove(&node) else {
                return Vec::new();
            };
            let paths: Vec<Vec<PathSegment>> = paths.into_keys().collect();
            self.examine(paths.len());
            for path in &paths {
                let key = NodeKey {
                    path: path.clone(),
                    node,
                };
                self.drop_value(&key);
                self.dirty.remove(&key);
            }
            paths
        }

        /// Keep only the entries `keep` accepts, in the cache and the dirty
        /// set alike.
        ///
        /// **O(cache).** Every caller is an invalidation whose extent is a
        /// path predicate rather than a node — a scope drop, a binding
        /// change — and the reverse index is keyed by node, so there is
        /// nothing to look up. `CACHE-7` narrows these callers by time range;
        /// a path index would be the move if they stay hot after that.
        pub(super) fn retain(&mut self, keep: impl Fn(&NodeKey) -> bool) {
            self.examine(self.entries.len() + self.dirty.len());
            let dropped: Vec<NodeKey> = self
                .entries
                .keys()
                .filter(|key| !keep(key))
                .cloned()
                .collect();
            for key in dropped {
                self.drop_value(&key);
                self.prune_index(&key);
            }
            let undirtied: Vec<NodeKey> = self
                .dirty
                .iter()
                .filter(|key| !keep(key))
                .cloned()
                .collect();
            for key in undirtied {
                self.dirty.remove(&key);
                if let Some(presence) = self.index_get_mut(&key) {
                    presence.dirty = false;
                }
                self.prune_index(&key);
            }
        }

        /// Drop every cached value and dirty flag.
        pub(super) fn clear(&mut self) {
            self.examine(self.entries.len() + self.dirty.len());
            // Reservations release as the entries drop.
            self.entries.clear();
            self.dirty.clear();
            self.by_node.clear();
            self.by_reservation.clear();
            self.used = [0; 3];
        }

        // ----- internals ----------------------------------------------------

        /// Remove `key`'s cached value, releasing its bytes. Leaves the
        /// index entry's `cached` flag cleared but does not prune it — the
        /// caller decides, because most callers touch the dirty flag too.
        fn drop_value(&mut self, key: &NodeKey) {
            self.examine(1);
            let Some(entry) = self.entries.remove(key) else {
                return;
            };
            let index = tier_index(entry.tier);
            self.used[index] = self.used[index].saturating_sub(entry.bytes);
            if let Some(reservation) = &entry.reservation {
                self.by_reservation.remove(&reservation.id());
            }
            if let Some(presence) = self.index_get_mut(key) {
                presence.cached = false;
            }
            // `entry` drops here: the reservation releases its bytes back to
            // the budget, with no lock held by this store.
        }

        fn index_mut(&mut self, key: &NodeKey) -> &mut Presence {
            self.by_node
                .entry(key.node)
                .or_default()
                .entry(key.path.clone())
                .or_default()
        }

        fn index_get_mut(&mut self, key: &NodeKey) -> Option<&mut Presence> {
            self.by_node.get_mut(&key.node)?.get_mut(&key.path)
        }

        /// Forget `key` in the index once nothing refers to it any more.
        fn prune_index(&mut self, key: &NodeKey) {
            let Some(paths) = self.by_node.get_mut(&key.node) else {
                return;
            };
            if paths.get(&key.path).is_some_and(|p| p.is_empty()) {
                paths.remove(&key.path);
            }
            if paths.is_empty() {
                self.by_node.remove(&key.node);
            }
        }

        /// Whether the index describes exactly what is stored.
        ///
        /// The invariant the whole module exists to hold. Checked from the
        /// tests after every kind of mutation; a violation here is the silent
        /// failure mode that direct field access would have produced.
        #[cfg(test)]
        pub(super) fn index_is_consistent(&self) -> bool {
            let mut expected: HashMap<NodeId, HashMap<Vec<PathSegment>, Presence>> = HashMap::new();
            for key in self.entries.keys() {
                expected
                    .entry(key.node)
                    .or_default()
                    .entry(key.path.clone())
                    .or_default()
                    .cached = true;
            }
            for key in &self.dirty {
                expected
                    .entry(key.node)
                    .or_default()
                    .entry(key.path.clone())
                    .or_default()
                    .dirty = true;
            }
            if expected != self.by_node {
                return false;
            }
            let mut used = [0u64; 3];
            for entry in self.entries.values() {
                used[tier_index(entry.tier)] += entry.bytes;
            }
            used == self.used
        }
    }

    fn tier_index(tier: Tier) -> usize {
        match tier {
            Tier::Vram => 0,
            Tier::Ram => 1,
            Tier::Disk => 2,
        }
    }
}

use cache_store::CacheStore;

// ===========================================================================
// Scope reach
// ===========================================================================

/// What a scope's bindings can reach, derived once per (scope, graph).
///
/// A binding change may only invalidate what the matching interface output
/// port actually feeds. Computing that means flooding the network, which must
/// not happen per frame — so the answer is kept per scope and reused for as
/// long as the scope's graph is the same object (`Graph` is immutable, and
/// structural sharing makes an untouched network compare equal by pointer).
///
/// The interface node itself is deliberately **not** in the sets. Its
/// recompute is decided in [`Evaluator::eval_node`] through
/// [`CacheMiss::BindingsChanged`], which is also what lets its unrelated
/// output ports stay unfresh for their consumers.
///
/// # Invariant
///
/// The reach is traced from interface nodes only, so **an interface node must
/// be the only kind of processor that reads [`EvalScope::bindings`]**. A
/// processor that read a bound name without sitting downstream of the port
/// exposing it would keep a value built from the previous binding. Today
/// `net.in` is the sole caller; a new one has to expose the value through an
/// interface output port (or the binding name must go unclaimed, which falls
/// back to dropping the whole scope).
struct ScopeReach {
    /// The graph the sets were derived from, compared by pointer identity.
    graph: Graph,
    /// Interface output-port name → every node the port's value feeds,
    /// transitively, through wires and `NodeOutput` parameter bindings.
    ///
    /// A name absent from this map is exposed by no interface node in the
    /// graph, and a change to it cannot be traced (see the fallback in
    /// [`Evaluator::invalidate_changed_bindings`]).
    downstream: HashMap<String, HashSet<NodeId>>,
}

impl ScopeReach {
    fn of(graph: &Graph) -> Self {
        let adjacency = graph.downstream_adjacency();
        // Direct consumers of each output port, wires and parameter pulls
        // alike, indexed once so the per-port flood below is a lookup.
        let mut consumers: HashMap<(NodeId, OutputPortIndex), Vec<NodeId>> = HashMap::new();
        for edge in graph.edges() {
            consumers
                .entry((edge.source, edge.source_port))
                .or_default()
                .push(edge.target);
        }
        for node in graph.nodes() {
            for source in node.parameter_sources() {
                consumers.entry(source).or_default().push(node.id);
            }
        }

        let mut downstream: HashMap<String, HashSet<NodeId>> = HashMap::new();
        for interface in graph.nodes().filter(|node| network::is_in_node(node)) {
            for (index, port) in interface.outputs.iter().enumerate() {
                let mut stack = consumers
                    .get(&(interface.id, OutputPortIndex(index as u32)))
                    .cloned()
                    .unwrap_or_default();
                // Several interface nodes may expose the same port name; the
                // entry is their union.
                let reached = downstream.entry(port.name.clone()).or_default();
                while let Some(current) = stack.pop() {
                    if reached.insert(current) {
                        stack.extend(adjacency.get(&current).into_iter().flatten().copied());
                    }
                }
            }
        }
        Self {
            graph: graph.clone(),
            downstream,
        }
    }
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
    /// Cached values, dirty flags, byte accounting and the `NodeId → paths`
    /// index, behind one API that keeps them consistent (see
    /// [`cache_store`]).
    store: CacheStore,
    document: Option<Arc<Document>>,
    path: Vec<PathSegment>,
    active_scopes: Vec<PathSegment>,
    bindings_stack: Vec<Bindings>,
    /// Node currently being processed and its branch-wide recursion depth,
    /// per recursion level. [`EvalScope::evaluate_sub`] carries that depth
    /// across network boundaries instead of resetting the stack budget.
    processing: Vec<(NodeKey, usize)>,
    /// Nested scope path → the node whose `process` opened it. Scoped
    /// invalidation uses this to drop the owner's cached value too, so a
    /// network edit propagates to the shell chain automatically.
    scope_owners: HashMap<Vec<PathSegment>, NodeKey>,
    /// Bindings last used per nested scope. A scope re-entered with
    /// different bindings (e.g. an adjustment layer's changing lower stack)
    /// has the cached values those bindings reach dropped before evaluation.
    scope_bindings: HashMap<Vec<PathSegment>, Bindings>,
    /// What each nested scope's bindings reach, per scope path. Rebuilt only
    /// when the scope's graph is a different object (see [`ScopeReach`]).
    scope_reach: HashMap<Vec<PathSegment>, ScopeReach>,
    /// Binding names that changed on entry to each active scope, parallel to
    /// `bindings_stack`. Read by [`Self::eval_node`] to decide which of an
    /// interface node's output ports carry a new value.
    binding_changes: Vec<Vec<String>>,
    /// Per-output-port freshness of the interface nodes that recomputed for
    /// a binding change alone, within the current top-level evaluation.
    ///
    /// Absence means "as fresh as the node": only a binding-only recompute
    /// can leave some of a node's ports unchanged, so this map is empty on
    /// every ordinary pull.
    fresh_output_ports: HashMap<NodeKey, Vec<bool>>,
    /// Wall-clock `process()` durations recorded by the current top-level
    /// evaluation (see [`Evaluator::take_timings`]).
    timings: Vec<(NodeId, std::time::Duration)>,
    /// How many times the full [`ResolvedParams`] was materialised.
    ///
    /// Instrumentation for the HIGH-03 regression: materialisation is where
    /// constants (strings, path points, curves) get cloned, and a pull served
    /// from cache must not reach it. Nothing outside the tests reads this, so
    /// it is compiled out of production builds.
    #[cfg(test)]
    param_materializations: usize,
    /// Node pulls served from cache, and the reasons the rest were not.
    ///
    /// Compiled into production builds and read through
    /// [`Evaluator::cache_stats`]: "the cache stopped working" is only
    /// observable as a ratio, and CI has to be able to see it (the same
    /// argument as `GPUCOMP-7`'s readback counter).
    counters: CacheCounters,
}

/// Hit / miss tallies of every node pull an evaluator has served.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CacheCounters {
    hits: u64,
    /// Misses per [`CacheMiss`], indexed by [`CacheMiss::index`].
    misses: [u64; CacheMiss::COUNT],
}

impl CacheCounters {
    /// Count one pull decision. `None` is a hit.
    ///
    /// The single place either tally moves: the pass-through and processing
    /// paths both call it, and each returns before reaching the other, so a
    /// pull is counted exactly once.
    fn record(&mut self, miss: Option<CacheMiss>) {
        match miss {
            None => self.hits += 1,
            Some(miss) => self.misses[miss.index()] += 1,
        }
    }
}

/// What an [`Evaluator`]'s result cache has done and what it is holding.
///
/// Readable in production builds, not only in tests: a regression like "the
/// adjustment layer's scope stopped caching" has no other observable, and CI
/// has to be able to assert on it (the argument `GPUCOMP-7` makes for
/// readback counts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvalCacheStats {
    /// Node pulls served from the cache since the evaluator was built.
    pub hits: u64,
    /// Node pulls that had to recompute, per reason
    /// ([`CacheMiss::index`] order).
    pub misses_by_reason: [u64; CacheMiss::COUNT],
    /// Values currently cached.
    pub entries: usize,
    /// Bytes currently cached per tier, in `Tier::ALL` order.
    pub bytes_by_tier: [u64; 3],
}

impl EvalCacheStats {
    /// Pulls that recomputed, for any reason.
    pub fn misses(&self) -> u64 {
        self.misses_by_reason.iter().sum()
    }

    /// Pulls recorded, hits and misses together.
    pub fn pulls(&self) -> u64 {
        self.hits + self.misses()
    }

    /// Misses attributed to `reason`.
    pub fn misses_for(&self, reason: CacheMiss) -> u64 {
        self.misses_by_reason[reason.index()]
    }

    /// Share of pulls served from cache, or `None` when nothing was pulled.
    pub fn hit_rate(&self) -> Option<f64> {
        let pulls = self.pulls();
        (pulls > 0).then(|| self.hits as f64 / pulls as f64)
    }

    /// Bytes cached in `tier`.
    pub fn bytes(&self, tier: Tier) -> u64 {
        self.bytes_by_tier[match tier {
            Tier::Vram => 0,
            Tier::Ram => 1,
            Tier::Disk => 2,
        }]
    }
}

impl Evaluator {
    /// Create an evaluator with no processors registered and **no cache
    /// limit**.
    ///
    /// Used by tests, examples and any host that has not built a budget. The
    /// application uses [`Evaluator::with_budget`] so its node cache is
    /// accounted for alongside every other cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// An evaluator whose result cache reports to `budget` and evicts,
    /// least-recently-used first, when the budget says to.
    pub fn with_budget(budget: SharedCacheBudget) -> Self {
        Self {
            store: CacheStore::new(Some(budget)),
            ..Self::default()
        }
    }

    /// Drop every processor registration, cached value, dirty flag, document
    /// and scope record — **keeping the cache budget**.
    ///
    /// This is what a structural resync needs, and it exists so nothing has
    /// to write `*evaluator = Evaluator::new()` to get it. That assignment
    /// silently replaced a budgeted evaluator with an unbudgeted one, and
    /// because the evaluation worker escalates its first request to
    /// [`crate::runtime::InvalidationHint::Structural`], it happened before
    /// the first frame: the application's node cache had no limit at all
    /// while every unit test passed. Anything rebuilt from the graph belongs
    /// here; anything the service owns (the budget) must not.
    pub fn reset(&mut self) {
        let budget = self.store.budget().cloned();
        *self = Self {
            store: CacheStore::new(budget),
            ..Self::default()
        };
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
        self.invalidate_node(node);
        tracing::trace!(
            node = node.raw(),
            "processor registered; node caches dropped"
        );
    }

    // ----- statistics ------------------------------------------------------

    /// Hit / miss tallies and the bytes the result cache is holding.
    ///
    /// Counts every node pull since the evaluator was built, or since the
    /// last [`Self::reset_cache_stats`]. Use it to assert that a workflow
    /// stays cached — that scrubbing back over visited frames recomputes
    /// nothing, that an unrelated edit does not empty a scope — rather than
    /// timing it.
    pub fn cache_stats(&self) -> EvalCacheStats {
        EvalCacheStats {
            hits: self.counters.hits,
            misses_by_reason: self.counters.misses,
            entries: self.store.len(),
            bytes_by_tier: self.store.used_bytes(),
        }
    }

    /// Whether a value is currently cached for `key`.
    #[cfg(test)]
    fn cache_contains(&self, key: &NodeKey) -> bool {
        self.store.contains(key)
    }

    /// Put a value straight into the result cache.
    ///
    /// Only for tests that need a warm cache without running an evaluation
    /// (document-diff invalidation, for instance, which has no graph).
    #[cfg(test)]
    fn seed_cache(&mut self, key: NodeKey, identity: CacheIdentity, value: Arc<dyn NodeData>) {
        self.store.insert(key, identity, value);
    }

    /// Zero the hit / miss tallies, keeping the cached values.
    ///
    /// The "measure from here" call: a test warms the cache, resets, and then
    /// asserts on the pulls it cares about.
    pub fn reset_cache_stats(&mut self) {
        self.counters = CacheCounters::default();
    }

    /// The processor currently registered for `node`.
    ///
    /// Lets a worker ask an existing registration whether replacing it would
    /// achieve anything (see [`NodeProcessor::rebuild_on_node_change`]) before
    /// paying to construct a new one.
    pub fn processor(&self, node: NodeId) -> Option<&Arc<dyn NodeProcessor>> {
        self.processors.get(&node)
    }

    /// Drop every cached value of `node` at every path and mark it dirty,
    /// keeping its processor registration.
    ///
    /// This is the invalidation half of [`Self::register`], for a node whose
    /// output changed but whose processor holds nothing derived from it. The
    /// caches of the owners of the scopes containing the node go too —
    /// otherwise a same-frame pull could serve a scope owner's stale cache and
    /// never reach the node at all.
    pub fn invalidate_node(&mut self, node: NodeId) {
        // Costs the node's own paths, not a walk over the cache: the store
        // keeps a `NodeId → paths` index precisely because `register()` (and
        // therefore this) runs for every changed node on every parameter tick
        // during a scrub (MED-CORE-07).
        for path in self.store.forget_node(node) {
            self.drop_scope_owner_caches(&path);
        }
        self.store.mark_dirty(NodeKey {
            path: Vec::new(),
            node,
        });
    }

    /// Whether `node` (at the root scope) is currently marked dirty.
    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.store.is_dirty(&NodeKey {
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
                                || old_comp.background_color != new_comp.background_color
                        }
                    }
                });
            if structural_change {
                tracing::debug!(
                    "document replaced with structural/resolution changes; \
                     invalidating all caches"
                );
                self.invalidate_all();
            } else {
                let changed = document.changed_network_paths(&old);
                if !changed.is_empty() {
                    tracing::debug!(
                        scopes = changed.len(),
                        "document network edits; invalidating changed scopes"
                    );
                }
                for prefix in changed {
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
                    let mut shell_changed: HashSet<LayerId> = HashSet::new();
                    for layer in &comp.layers {
                        let Some(old_layer) = old_comp.layers.iter().find(|l| l.id == layer.id)
                        else {
                            continue;
                        };
                        if layer_shell_changed(layer, old_layer) {
                            shell_changed.insert(layer.id);
                        }
                    }
                    // A layer's world matrix folds in its whole parent chain,
                    // read straight from the document (REQ-LAYER-001), so an
                    // ancestor's shell edit changes what its descendants
                    // render — including their time placement, since each
                    // ancestor is sampled at its own local frame
                    // (REQ-LAYER-006). The compiled `parent_transform` edge
                    // carries that freshness only while the ancestor is
                    // active: a muted or un-soloed parent is not compiled, so
                    // there is no edge to carry it. Dropping the descendants'
                    // shell caches here covers both cases.
                    //
                    // The chain itself is compared old against new: removing a
                    // layer leaves its children's `parent` dangling (the shell
                    // is untouched, so the layer never enters `shell_changed`)
                    // and `world_matrix` then stops at the missing ancestor —
                    // a changed matrix that nothing else would invalidate.
                    for layer in &comp.layers {
                        let chain: Vec<LayerId> =
                            comp.ancestors(layer).iter().map(|l| l.id).collect();
                        let old_chain: Option<Vec<LayerId>> = old_comp
                            .get_layer(layer.id)
                            .map(|old| old_comp.ancestors(old).iter().map(|l| l.id).collect());
                        let stale = shell_changed.contains(&layer.id)
                            || chain.iter().any(|id| shell_changed.contains(id))
                            || old_chain.is_some_and(|old_chain| {
                                old_chain != chain
                                    || old_chain.iter().any(|id| shell_changed.contains(id))
                            });
                        if !stale {
                            continue;
                        }
                        for role in [
                            NodeRole::Network,
                            NodeRole::Transform,
                            NodeRole::Opacity,
                            NodeRole::Merge,
                        ] {
                            let id = deterministic_node_id(*comp_id, layer.id, role);
                            self.store.remove(&NodeKey {
                                path: Vec::new(),
                                node: id,
                            });
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
            if self.store.mark_dirty(key) {
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
        self.store.clear();
        // Scope state is derived from evaluation and rebuilt on the next
        // entry, so a full invalidation drops it too: keeping it would hold
        // the bindings' frame buffers (and the `Graph` clones inside
        // `scope_reach`) alive behind an otherwise empty cache
        // (MED-CORE-07).
        self.scope_owners.clear();
        self.scope_bindings.clear();
        self.scope_reach.clear();
    }

    /// Drop cached values and dirty flags for every node whose path starts
    /// with `prefix` (e.g. one layer's network, subnets included).
    ///
    /// The cached values of the nodes that *own* matching scopes (network
    /// boundary nodes, subnet nodes) — and of the owners of every ancestor
    /// scope — are dropped as well: their recompute marks them fresh, which
    /// cascades to their downstream in the parent graph on the next pull.
    pub fn invalidate_scope(&mut self, prefix: &[PathSegment]) {
        self.store.retain(|k| !k.path.starts_with(prefix));
        // Before the prune: the owner of the scope named by `prefix` is
        // itself recorded under `prefix`, and dropping the record first would
        // leave its cached value behind.
        self.drop_scope_owner_caches(prefix);
        self.prune_scope_state(prefix);
    }

    /// Forget the per-scope state of every scope under `prefix`.
    ///
    /// `scope_bindings` holds `Arc<dyn NodeData>` values — an adjustment
    /// layer's composited lower stack is a frame buffer — and `scope_reach`
    /// holds a `Graph` clone. Neither was ever pruned, so deleting layers
    /// through a long session leaked one of each per removed scope
    /// (MED-CORE-07).
    fn prune_scope_state(&mut self, prefix: &[PathSegment]) {
        self.scope_owners
            .retain(|scope, _| !scope.starts_with(prefix));
        self.scope_bindings
            .retain(|scope, _| !scope.starts_with(prefix));
        self.scope_reach
            .retain(|scope, _| !scope.starts_with(prefix));
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
            self.store.remove(&owner);
        }
    }

    // ----- binding-scoped invalidation (MED-CORE-02) ------------------------

    /// Drop what a scope's changed bindings can reach, and nothing else.
    ///
    /// Freshness propagation alone would recompute the affected nodes that
    /// this pull actually visits, but a node the pull skips (a bypassed
    /// consumer's unused branch, a network evaluated for a different output)
    /// would keep a value produced from the previous bindings and be served
    /// it later, when the interface node is a plain cache hit. Dropping the
    /// reachable keys is what closes that window — the point of the unit is
    /// that "reachable" is no longer "the whole scope".
    fn invalidate_changed_bindings(&mut self, graph: &Graph, changed: &[String]) {
        let scope = self.path.clone();
        self.refresh_scope_reach(graph, &scope);
        // SAFETY of index: `refresh_scope_reach` just inserted the entry.
        let reach = &self.scope_reach[&scope];
        let mut affected: HashSet<NodeId> = HashSet::new();
        let mut traceable = true;
        for name in changed {
            match reach.downstream.get(name) {
                Some(nodes) => affected.extend(nodes.iter().copied()),
                // No interface node in this graph exposes an output port of
                // that name, so nothing here declares where the value goes.
                // A graph with no interface node at all lands here too. Fall
                // back to dropping the whole scope rather than guessing.
                None => {
                    traceable = false;
                    break;
                }
            }
        }

        if !traceable {
            tracing::debug!(
                scope_depth = scope.len(),
                "scope re-entered with a binding no interface port claims; \
                 dropping every scoped cache"
            );
            self.store.retain(|k| !k.path.starts_with(&scope));
            return;
        }
        if affected.is_empty() {
            return;
        }
        tracing::debug!(
            scope_depth = scope.len(),
            affected = affected.len(),
            "scope re-entered with changed bindings; dropping what they reach"
        );
        self.store
            .retain(|k| !binding_change_affects(&scope, &affected, k));
    }

    /// Make `self.scope_reach[scope]` describe `graph`, rebuilding it only
    /// when the scope's graph is a different object than last time.
    fn refresh_scope_reach(&mut self, graph: &Graph, scope: &[PathSegment]) {
        let current = self
            .scope_reach
            .get(scope)
            .is_some_and(|reach| reach.graph.ptr_eq(graph));
        if !current {
            self.scope_reach
                .insert(scope.to_vec(), ScopeReach::of(graph));
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
        self.binding_changes.clear();
        self.binding_changes.push(Vec::new());
        // Per-port freshness only describes the pull that recorded it.
        self.fresh_output_ports.clear();
        self.timings.clear();
        self.evaluate_inner(graph, output, ctx, 0)
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
        depth: usize,
    ) -> Result<Arc<dyn NodeData>, EvalError> {
        if graph.node(output).is_none() {
            return Err(EvalError::NodeNotFound(output));
        }
        let span = tracing::debug_span!("evaluate", output = output.raw(), frame = ctx.frame);
        let _guard = span.enter();
        let mut run = HashMap::new();
        let mut visiting = HashSet::new();
        let result = self.eval_node(graph, output, ctx, &mut run, &mut visiting, depth);
        match &result {
            Ok((_, fresh)) => tracing::debug!(fresh = fresh, "evaluation complete"),
            Err(err) => tracing::debug!(%err, "evaluation failed"),
        }
        let (value, _fresh) = result?;
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
        depth: usize,
    ) -> Result<(Arc<dyn NodeData>, bool), EvalError> {
        if depth >= MAX_EVALUATION_DEPTH {
            return Err(EvalError::DepthLimitExceeded {
                node,
                limit: MAX_EVALUATION_DEPTH,
            });
        }
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
        let in_edges: Vec<(InputPortIndex, NodeId, OutputPortIndex)> = graph
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
                    depth,
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
                // on this path — hence `TimeKey::TIMELESS`.
                let identity = CacheIdentity::of(ctx, false, true);
                // Classified with the same reasons as the processing path so
                // `cache_stats()` reports one vocabulary — a pass-through
                // node that stopped caching has to be as visible as any
                // other.
                let miss = if self.store.is_dirty(&key) {
                    Some(CacheMiss::Dirty)
                } else if any_input_fresh {
                    Some(CacheMiss::InputFresh)
                } else {
                    match self.store.peek(&key) {
                        Some(entry) => entry.identity.mismatch(&identity),
                        None => Some(CacheMiss::NoEntry),
                    }
                };
                self.counters.record(miss);
                let result = if miss.is_none() {
                    // SAFETY of expect: a `None` miss implies the entry exists.
                    let value = self.store.get_used(&key).expect("cache hit has a value");
                    (value, false)
                } else {
                    self.store.insert(key.clone(), identity, passed.clone());
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
                depth,
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

        // Resolve the *channel-backed* parameters before the cache decision:
        // a `NodeOutput` source is a hidden dependency, and a same-frame
        // change there must force a recompute (REQ-LAYER-004). Constants
        // cannot be fresh and are not needed to decide anything, so their
        // materialisation — which clones strings, path points and curves —
        // waits for the miss path (HIGH-03). Overridden keys are skipped
        // entirely and receive their overlay instead.
        let options = ResolveOptions {
            skip: &overridden,
            budget: ResolveBudget {
                owner: node_ref.id,
                depth,
            },
        };
        let (resolved_channels, params_fresh) =
            self.resolve_channel_params(graph, &node_ref, ctx, run, visiting, options)?;

        // Decide whether the cached value is still valid. Everything the
        // value is specific to lives in `CacheIdentity`; the freshness of
        // this pull (dirty, recomputed inputs, fresh parameter sources) is
        // checked first because it outranks any stored identity.
        let identity = CacheIdentity::of(ctx, time_dependent, bypassed);
        let miss = if self.store.is_dirty(&key) {
            Some(CacheMiss::Dirty)
        } else if any_input_fresh {
            Some(CacheMiss::InputFresh)
        } else if params_fresh {
            Some(CacheMiss::ParamsFresh)
        } else {
            match self.store.peek(&key) {
                Some(entry) => entry.identity.mismatch(&identity),
                None => Some(CacheMiss::NoEntry),
            }
        };

        // A network interface node also carries the scope's bindings, which
        // are values and therefore outside `CacheIdentity`. Checking them
        // last is deliberate: `BindingsChanged` may only be the reason when
        // nothing else is, because it is the one reason that leaves the
        // node's other output ports unchanged.
        let interface = network::is_in_node(&node_ref);
        let rebound = if interface {
            rebound_output_ports(
                &node_ref,
                self.binding_changes
                    .last()
                    .map_or(&[][..], |v| v.as_slice()),
            )
        } else {
            Vec::new()
        };
        let miss = if miss.is_none() && !rebound.is_empty() {
            Some(CacheMiss::BindingsChanged)
        } else {
            miss
        };
        self.counters.record(miss);

        match miss {
            None => {
                tracing::trace!(
                    node = node.raw(),
                    frame = ctx.frame,
                    time_dependent,
                    "cache hit"
                );
            }
            Some(CacheMiss::FrameAdvanced) => {
                // The signal for animation correctness: a time-varying node
                // (animated params or time-dependent processor) being
                // re-pulled at a new position. Its *absence* during playback
                // means the cache is wrongly considered fresh.
                tracing::debug!(
                    node = node.raw(),
                    type_key = %node_ref.type_key,
                    frame = ctx.frame,
                    ticks = identity.time.ticks(),
                    cached_ticks = self
                        .store
                        .peek(&key)
                        .map(|entry| entry.identity.time.ticks()),
                    "time-varying node re-pulled at new position"
                );
            }
            Some(miss) => {
                tracing::trace!(
                    node = node.raw(),
                    type_key = %node_ref.type_key,
                    frame = ctx.frame,
                    reason = miss.as_str(),
                    "cache miss"
                );
            }
        }

        let result = if miss.is_none() {
            // SAFETY of expect: a `None` miss implies the entry exists.
            let value = self.store.get_used(&key).expect("cache hit has a value");
            (value, false)
        } else {
            // Only now are the constants materialised: this is the one path
            // that hands parameters to a processor.
            let mut params = self.materialize_params(&node_ref, resolved_channels, &overridden);
            for (param_key, resolved) in overlays {
                params.set(&param_key, resolved);
            }
            let span = tracing::debug_span!(
                "node_process",
                node = node.raw(),
                type_key = %node_ref.type_key
            );
            let _guard = span.enter();
            self.processing.push((key.clone(), depth));
            let started = std::time::Instant::now();
            let produced = processor
                .process(&node_ref, ctx, &input_values, &params, self)
                .map_err(|source| EvalError::ProcessFailed { node, source });
            self.timings.push((node, started.elapsed()));
            self.processing.pop();
            let value = produced?;
            self.store.insert(key.clone(), identity, value.clone());
            (value, true)
        };

        if interface {
            // Report the interface node's freshness per output port, so a
            // rebound `source` does not drag the consumers of `t` or
            // `base_geometry` along with it. Only a binding-only recompute
            // can leave a port unchanged; every other reason (dirty, time,
            // resolution, fresh parameter source) moves all of them, and the
            // entry is removed so no consumer reads a narrower answer than
            // the node deserves.
            match (result.1, miss) {
                (true, Some(CacheMiss::BindingsChanged)) => {
                    let ports = (0..node_ref.outputs.len())
                        .map(|index| rebound.contains(&index))
                        .collect();
                    self.fresh_output_ports.insert(key.clone(), ports);
                }
                _ => {
                    self.fresh_output_ports.remove(&key);
                }
            }
        }

        visiting.remove(&key);
        run.insert(key, result.clone());
        Ok(result)
    }

    /// Whether `port` of `source` — a node this pull recomputed — actually
    /// delivered a new value.
    ///
    /// Only a network interface node recomputed for a binding change reports
    /// freshness per port (see `fresh_output_ports`); for every other node a
    /// recompute makes all of its outputs fresh.
    fn output_port_is_fresh(&self, source: NodeId, port: OutputPortIndex) -> bool {
        if self.fresh_output_ports.is_empty() {
            return true;
        }
        let key = NodeKey {
            path: self.path.clone(),
            node: source,
        };
        match self.fresh_output_ports.get(&key) {
            Some(ports) => ports.get(port.0 as usize).copied().unwrap_or(true),
            None => true,
        }
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
        source_port: OutputPortIndex,
        ctx: &EvalContext,
        input_values: &mut [Option<Arc<dyn NodeData>>],
        any_input_fresh: &mut bool,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
        depth: usize,
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
        let (value, fresh) = self.eval_node(graph, source, ctx, run, visiting, depth + 1)?;
        *any_input_fresh |= fresh && self.output_port_is_fresh(source, source_port);
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

    /// Resolve the channel-backed parameters of `node`, in parameter order.
    ///
    /// Returns the resolved values by parameter index, and whether any
    /// `NodeOutput` source resolved to a *fresh* (recomputed) value — which
    /// the caller uses to force a recompute of the consuming node even at the
    /// same position in time.
    ///
    /// Only channels are resolved: they are the only parameters that can pull
    /// from the graph, hence the only ones the cache decision depends on.
    /// Constants are materialised later by [`Self::materialize_params`], and
    /// only when the node is actually processed (HIGH-03). Parameters for
    /// which `skip` returns true (connected parameter ports) are not resolved
    /// at all — the caller overlays their port value.
    fn resolve_channel_params(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
        options: ResolveOptions<'_>,
    ) -> Result<(Vec<(usize, ResolvedValue)>, bool), EvalError> {
        let mut any_fresh = false;
        // Stays unallocated for the (common) node with no channel parameters.
        let mut values: Vec<(usize, ResolvedValue)> = Vec::new();
        for (index, p) in node.parameters.iter().enumerate() {
            if (options.skip)(&p.key) {
                continue;
            }
            let value = match &p.value {
                ParameterValue::Float(_)
                | ParameterValue::Int(_)
                | ParameterValue::Bool(_)
                | ParameterValue::String(_)
                | ParameterValue::PathPoints(_)
                | ParameterValue::Curve(_) => continue,
                ParameterValue::Channel(ch) => {
                    let (v, fresh) =
                        self.resolve_channel(graph, ch, ctx, run, visiting, options.budget)?;
                    any_fresh |= fresh;
                    ResolvedValue::Float(v)
                }
                ParameterValue::Channel2(chs) => {
                    let mut v = [0.0; 2];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) =
                            self.resolve_channel(graph, ch, ctx, run, visiting, options.budget)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec2(v)
                }
                ParameterValue::Channel3(chs) => {
                    let mut v = [0.0; 3];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) =
                            self.resolve_channel(graph, ch, ctx, run, visiting, options.budget)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec3(v)
                }
                ParameterValue::Channel4(chs) => {
                    let mut v = [0.0; 4];
                    for (i, ch) in chs.iter().enumerate() {
                        let (x, fresh) =
                            self.resolve_channel(graph, ch, ctx, run, visiting, options.budget)?;
                        any_fresh |= fresh;
                        v[i] = x;
                    }
                    ResolvedValue::Vec4(v)
                }
            };
            values.push((index, value));
        }
        Ok((values, any_fresh))
    }

    /// Build the [`ResolvedParams`] handed to [`NodeProcessor::process`],
    /// reusing the channel values already resolved for the cache decision.
    ///
    /// Constants are cloned here and nowhere else, so a node served from
    /// cache never pays for its strings, path points or curves. `channels`
    /// arrives in parameter order, as does the result: the values a processor
    /// sees are identical to resolving every parameter in one pass.
    fn materialize_params(
        &mut self,
        node: &Node,
        channels: Vec<(usize, ResolvedValue)>,
        skip: &dyn Fn(&str) -> bool,
    ) -> ResolvedParams {
        #[cfg(test)]
        {
            self.param_materializations += 1;
        }
        let mut channels = channels.into_iter().peekable();
        let mut values = Vec::with_capacity(node.parameters.len());
        for (index, p) in node.parameters.iter().enumerate() {
            if skip(&p.key) {
                continue;
            }
            let value = match channels.peek() {
                Some((resolved_index, _)) if *resolved_index == index => {
                    // SAFETY of unwrap: `peek` just proved the item exists.
                    channels.next().unwrap().1
                }
                _ => match &p.value {
                    ParameterValue::Float(v) => ResolvedValue::Float(*v),
                    ParameterValue::Int(v) => ResolvedValue::Int(*v),
                    ParameterValue::Bool(v) => ResolvedValue::Bool(*v),
                    ParameterValue::String(v) => ResolvedValue::Str(v.clone()),
                    ParameterValue::PathPoints(points) => ResolvedValue::PathPoints(points.clone()),
                    ParameterValue::Curve(curve) => ResolvedValue::Curve(curve.clone()),
                    // A channel that resolve_channel_params skipped is
                    // impossible: both walk the same parameters with the same
                    // `skip`. Falling back keeps the shape total.
                    ParameterValue::Channel(_)
                    | ParameterValue::Channel2(_)
                    | ParameterValue::Channel3(_)
                    | ParameterValue::Channel4(_) => continue,
                },
            };
            values.push((p.key.clone(), value));
        }
        ResolvedParams { values }
    }

    fn resolve_channel(
        &mut self,
        graph: &Graph,
        channel: &AnimationChannel,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
        budget: ResolveBudget,
    ) -> Result<(f32, bool), EvalError> {
        self.resolve_source(graph, &channel.source, ctx, run, visiting, budget)
    }

    fn resolve_source(
        &mut self,
        graph: &Graph,
        source: &ChannelSource,
        ctx: &EvalContext,
        run: &mut HashMap<NodeKey, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeKey>,
        budget: ResolveBudget,
    ) -> Result<(f32, bool), EvalError> {
        if budget.depth >= MAX_EVALUATION_DEPTH {
            return Err(EvalError::DepthLimitExceeded {
                node: budget.owner,
                limit: MAX_EVALUATION_DEPTH,
            });
        }
        match source {
            ChannelSource::NodeOutput(target, port) => {
                let (value, fresh) =
                    self.eval_node(graph, *target, ctx, run, visiting, budget.depth + 1)?;
                let fresh = fresh && self.output_port_is_fresh(*target, *port);
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
                let (av, af) =
                    self.resolve_source(graph, a, ctx, run, visiting, budget.deeper())?;
                let (bv, bf) =
                    self.resolve_source(graph, b, ctx, run, visiting, budget.deeper())?;
                Ok((mode.blend(av, bv, factor), af || bf))
            }
            other => Ok((other.evaluate(ctx.sample_frame(), ctx), false)),
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
        if self.active_scopes.len() >= MAX_EVALUATION_DEPTH {
            return Err(EvalError::DepthLimitExceeded {
                node: output,
                limit: MAX_EVALUATION_DEPTH,
            });
        }
        if self.active_scopes.contains(&segment) {
            return Err(EvalError::CycleDetected(output));
        }
        self.active_scopes.push(segment);
        self.path.push(segment);
        let depth = self
            .processing
            .last()
            .map_or(0, |(_owner, depth)| depth + 1);
        if let Some((owner, _depth)) = self.processing.last().cloned() {
            self.scope_owners.insert(self.path.clone(), owner);
        }
        // A scope re-entered with different bindings (e.g. an adjustment
        // layer's lower stack) may not reuse the cached values those
        // bindings feed. Everything else in the scope survives — that is
        // what makes an adjustment layer's static generators cacheable
        // across frames (MED-CORE-02).
        let changed = match self.scope_bindings.get(&self.path) {
            Some(old) => binding_delta(old, &bindings),
            None => binding_delta(&[], &bindings),
        };
        if !changed.is_empty() {
            self.invalidate_changed_bindings(graph, &changed);
        }
        self.scope_bindings
            .insert(self.path.clone(), bindings.clone());
        self.binding_changes.push(changed);
        self.bindings_stack.push(bindings);

        let result = self.evaluate_inner(graph, output, ctx, depth);

        self.bindings_stack.pop();
        self.binding_changes.pop();
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
/// Vec3 → Channel3, Color *or* Vec4 → Channel4. `None` when the wire value
/// cannot drive the parameter (the caller falls back to the stored value).
fn param_port_overlay(param: &ParameterValue, data: &dyn NodeData) -> Option<ResolvedValue> {
    use crate::types::{Color, Vec2, Vec3, Vec4};
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
        ParameterValue::Channel3(_) => data
            .downcast_ref::<Vec3>()
            .map(|v| ResolvedValue::Vec3([v.0, v.1, v.2])),
        // A 4-component parameter port accepts both readings of its four
        // floats, so either wire type drives it (`port_accepted_types`).
        ParameterValue::Channel4(_) => data
            .downcast_ref::<Color>()
            .map(|c| ResolvedValue::Vec4([c.r, c.g, c.b, c.a]))
            .or_else(|| {
                data.downcast_ref::<Vec4>()
                    .map(|v| ResolvedValue::Vec4([v.0, v.1, v.2, v.3]))
            }),
        ParameterValue::String(_) | ParameterValue::PathPoints(_) | ParameterValue::Curve(_) => {
            None
        }
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
    in_edges: &[(InputPortIndex, NodeId, OutputPortIndex)],
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

    // ---- EvalContext::sample_frame ----------------------------------------

    #[test]
    fn sample_frame_is_exact_on_the_frame_grid() {
        for frame in [0u64, 1, 7, 30, 1_000_000] {
            assert_eq!(ctx_at(frame).sample_frame(), frame as f64);
        }
    }

    #[test]
    fn sample_frame_is_exact_on_a_non_integer_frame_rate() {
        // 30000/1001 makes `frame / fps` inexact; the sub-frame formulation
        // must still round-trip to the integer frame.
        const NTSC: FrameRate = FrameRate {
            num: 30000,
            den: 1001,
        };
        for frame in [0u64, 1, 7, 30, 12_345] {
            let ctx = EvalContext::new(frame, NTSC, (1920, 1080));
            assert_eq!(ctx.sample_frame(), frame as f64);
        }
    }

    #[test]
    fn sample_frame_follows_a_sub_frame_time() {
        let mut ctx = ctx_at(10);
        // Half a frame past frame 10 at 30 fps.
        ctx.time += 0.5 / FPS.as_f64();
        assert!((ctx.sample_frame() - 10.5).abs() < 1e-9);
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
    fn composition_resolution_change_invalidates_cache() {
        let g = Graph::new().add_node(scalar_node(1)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 5.0,
                calls: calls.clone(),
            }),
        );

        let canvas_ctx = EvalContext::new(0, FPS, (960, 540));
        ev.evaluate(&g, NodeId::new(1), &canvas_ctx).unwrap();
        ev.evaluate(&g, NodeId::new(1), &canvas_ctx).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let scaled_ctx = canvas_ctx.with_comp_resolution((1920, 1080));
        ev.evaluate(&g, NodeId::new(1), &scaled_ctx).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
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

    #[test]
    fn deep_linear_graph_returns_an_error_before_stack_overflow() {
        let node_count = MAX_EVALUATION_DEPTH as u64 + 1;
        let mut graph = Graph::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut evaluator = Evaluator::new();
        for raw in 1..=node_count {
            graph = graph.add_node(scalar_node(raw)).unwrap();
            evaluator.register(
                NodeId::new(raw),
                Arc::new(CountingSum {
                    calls: calls.clone(),
                }),
            );
            if raw > 1 {
                graph = graph
                    .add_edge(
                        EdgeId::new(raw - 1),
                        NodeId::new(raw - 1),
                        OutputPortIndex(0),
                        NodeId::new(raw),
                        InputPortIndex(0),
                    )
                    .unwrap();
            }
        }

        assert!(matches!(
            evaluator.evaluate(&graph, NodeId::new(node_count), &ctx_at(0)),
            Err(EvalError::DepthLimitExceeded {
                limit: MAX_EVALUATION_DEPTH,
                ..
            })
        ));
    }

    #[test]
    fn depth_budget_is_preserved_across_a_network_boundary() {
        const CHAIN_LEN: u64 = 150;

        let chain = |first: u64| {
            let mut graph = Graph::new();
            for offset in 0..CHAIN_LEN {
                let raw = first + offset;
                graph = graph.add_node(scalar_node(raw)).unwrap();
                if offset > 0 {
                    graph = graph
                        .add_edge(
                            EdgeId::new(raw),
                            NodeId::new(raw - 1),
                            OutputPortIndex(0),
                            NodeId::new(raw),
                            InputPortIndex(0),
                        )
                        .unwrap();
                }
            }
            graph
        };

        let inner_first = 1_000;
        let inner = chain(inner_first);
        let outer = chain(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut evaluator = Evaluator::new();
        evaluator.register(
            NodeId::new(1),
            Arc::new(ScopedSource {
                inner,
                inner_output: NodeId::new(inner_first + CHAIN_LEN - 1),
                segment: PathSegment::Subnet(NodeId::new(1)),
                frame_offset: 0,
            }),
        );
        for raw in 2..=CHAIN_LEN {
            evaluator.register(
                NodeId::new(raw),
                Arc::new(CountingSum {
                    calls: calls.clone(),
                }),
            );
        }
        for raw in inner_first..(inner_first + CHAIN_LEN) {
            evaluator.register(
                NodeId::new(raw),
                Arc::new(CountingSum {
                    calls: calls.clone(),
                }),
            );
        }

        let error = match evaluator.evaluate(&outer, NodeId::new(CHAIN_LEN), &ctx_at(0)) {
            Ok(_) => panic!("mixed node/network depth should exceed the branch budget"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            EvalError::ProcessFailed { source, .. }
                if matches!(
                    source.downcast_ref::<EvalError>(),
                    Some(EvalError::DepthLimitExceeded {
                        limit: MAX_EVALUATION_DEPTH,
                        ..
                    })
                )
        ));
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

    /// RESP-3: a processor whose output comes entirely from `node` and `params`
    /// does not need constructing again when the node changes, so the worker
    /// invalidates instead of re-registering. `invalidate_node` has to be as
    /// strong as `register`'s invalidation was, or a parameter edit would serve
    /// the cached value.
    #[test]
    fn invalidate_node_recomputes_without_replacing_the_processor() {
        let node = |value: f32| {
            Node::new(NodeId::new(1), "test")
                .with_output("out", DataTypeId::SCALAR)
                .with_param("value", ParameterValue::Float(value))
        };
        let g0 = Graph::new().add_node(node(1.0)).unwrap();

        let mut ev = Evaluator::new();
        let processor: Arc<dyn NodeProcessor> = Arc::new(ParamEcho);
        ev.register(NodeId::new(1), processor.clone());
        let v = ev.evaluate(&g0, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!((v.downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < f32::EPSILON);

        // The parameter edit: same processor, new graph. Without the
        // invalidation the cached 1.0 would come back.
        let g1 = Graph::new().add_node(node(4.0)).unwrap();
        ev.invalidate_node(NodeId::new(1));
        assert!(ev.is_dirty(NodeId::new(1)), "the node must be marked dirty");
        assert!(
            ev.processor(NodeId::new(1))
                .is_some_and(|current| Arc::ptr_eq(current, &processor)),
            "the registration must survive an invalidation"
        );

        let v = ev.evaluate(&g1, NodeId::new(1), &ctx_at(0)).unwrap();
        assert!(
            (v.downcast_ref::<Scalar>().unwrap().0 - 4.0).abs() < f32::EPSILON,
            "the edited value must be recomputed"
        );
    }

    /// The default has to be the conservative one: a processor that captured
    /// something off its node is the common case, and a new node type must be
    /// correct without anyone remembering to classify it.
    #[test]
    fn processors_are_rebuilt_on_node_change_by_default() {
        assert!(ParamEcho.rebuild_on_node_change());
    }

    /// The part of `register`'s invalidation that is easy to lose when
    /// extracting it: a node inside a layer network is cached under a non-empty
    /// path, and the network boundary that opened that scope caches the value it
    /// returned. Dropping only the nested entry would let the boundary's stale
    /// cache answer the next same-frame pull, so the edit would never be seen.
    #[test]
    fn invalidate_node_reaches_a_nested_node_and_its_scope_owner() {
        let inner_calls = Arc::new(AtomicUsize::new(0));
        let outer_calls = Arc::new(AtomicUsize::new(0));

        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let outer = Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingScopedSource {
                inner: inner.clone(),
                inner_output: NodeId::new(7),
                segment,
                calls: outer_calls.clone(),
            }),
        );
        let nested: Arc<dyn NodeProcessor> = Arc::new(FrameSource {
            calls: inner_calls.clone(),
        });
        ev.register(NodeId::new(7), nested.clone());

        // Warm both the nested value and the boundary's cache of it.
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);
        assert_eq!(outer_calls.load(Ordering::Relaxed), 1);
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(
            (
                inner_calls.load(Ordering::Relaxed),
                outer_calls.load(Ordering::Relaxed)
            ),
            (1, 1),
            "both levels must be cached before the invalidation means anything"
        );

        // The parameter edit on the nested node, same frame.
        ev.invalidate_node(NodeId::new(7));
        assert!(
            ev.processor(NodeId::new(7))
                .is_some_and(|current| Arc::ptr_eq(current, &nested)),
            "the nested registration must survive"
        );
        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(
            inner_calls.load(Ordering::Relaxed),
            2,
            "the nested node must recompute"
        );
        assert_eq!(
            outer_calls.load(Ordering::Relaxed),
            2,
            "and the boundary must re-enter the scope instead of serving its cache"
        );
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

    /// A `Channel4` parameter takes either reading of its four floats, so a
    /// `Vec4` output drives it just like a `Color` one. Without this, folding
    /// four scalar component parameters into one `Channel4` would leave it
    /// undrivable by `vector.construct.vec4`.
    #[test]
    fn vec4_and_color_both_drive_a_channel4_param_port() {
        struct Emit<T: crate::types::NodeData + Clone>(T);
        impl<T: crate::types::NodeData + Clone> NodeProcessor for Emit<T> {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                Ok(Arc::new(self.0.clone()))
            }
        }
        struct Vec4Echo;
        impl NodeProcessor for Vec4Echo {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                let [x, y, z, w] = params.vec4_or("tint", [0.0; 4]);
                Ok(Arc::new(Scalar(x * 1000.0 + y * 100.0 + z * 10.0 + w)))
            }
        }

        let run = |source: Node, processor: Arc<dyn NodeProcessor>| {
            let target = Node::new(NodeId::new(2), "test")
                .with_output("out", DataTypeId::SCALAR)
                .with_param(
                    "tint",
                    ParameterValue::Channel4([
                        AnimationChannel::constant(0.0),
                        AnimationChannel::constant(0.0),
                        AnimationChannel::constant(0.0),
                        AnimationChannel::constant(0.0),
                    ]),
                );
            let g = Graph::new()
                .add_node(source)
                .unwrap()
                .add_node(target)
                .unwrap()
                .expose_param_port(NodeId::new(2), "tint")
                .unwrap();
            assert_eq!(
                g.node(NodeId::new(2)).unwrap().inputs[0].accepted_types,
                vec![DataTypeId::COLOR, DataTypeId::VEC4],
                "a 4-component parameter port accepts both"
            );
            let g = g
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(0),
                )
                .unwrap();
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(1), processor);
            ev.register(NodeId::new(2), Arc::new(Vec4Echo));
            ev.evaluate(&g, NodeId::new(2), &ctx_at(0))
                .unwrap()
                .downcast_ref::<Scalar>()
                .unwrap()
                .0
        };

        let from_vec4 = run(
            Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::VEC4),
            Arc::new(Emit(crate::types::Vec4(1.0, 2.0, 3.0, 4.0))),
        );
        assert!((from_vec4 - 1234.0).abs() < 1e-3);

        let from_color = run(
            Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::COLOR),
            Arc::new(Emit(crate::types::Color::new(1.0, 2.0, 3.0, 4.0))),
        );
        assert!((from_color - 1234.0).abs() < 1e-3);
    }

    /// A `Channel3` parameter exposes a VEC3 port and is driven by a Vec3
    /// output. Without this, folding `translate_x` / `translate_y` into one
    /// `Channel3` would take away the scalar ports they used to expose.
    #[test]
    fn vec3_param_ports_convert_componentwise() {
        struct Vec3Source;
        impl NodeProcessor for Vec3Source {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                Ok(Arc::new(crate::types::Vec3(3.0, -4.0, 5.0)))
            }
        }
        struct Vec3Echo;
        impl NodeProcessor for Vec3Echo {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                let [x, y, z] = params.vec3_or("translate", [0.0, 0.0, 0.0]);
                Ok(Arc::new(Scalar(x * 100.0 + y * 10.0 + z)))
            }
        }
        let source = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::VEC3);
        let target = Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "translate",
                ParameterValue::Channel3([
                    AnimationChannel::constant(0.0),
                    AnimationChannel::constant(0.0),
                    AnimationChannel::constant(0.0),
                ]),
            );
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "translate")
            .unwrap();
        assert_eq!(
            g.node(NodeId::new(2)).unwrap().inputs[0].accepted_types,
            vec![DataTypeId::VEC3]
        );
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Vec3Source));
        ev.register(NodeId::new(2), Arc::new(Vec3Echo));
        let out = ev.evaluate(&g, NodeId::new(2), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 265.0).abs() < 1e-4);
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
                Ok(Arc::new(crate::types::FrameBuffer::from_f32(
                    1,
                    1,
                    vec![0.0; 4],
                )))
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

    /// Like `ScopedSource` but cacheable (not time-dependent) and counting its
    /// own invocations, so a test can tell whether the boundary re-entered the
    /// scope or answered from its own cache.
    struct CountingScopedSource {
        inner: Graph,
        inner_output: NodeId,
        segment: PathSegment,
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for CountingScopedSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let value = scope.evaluate_sub(
                self.segment,
                &self.inner,
                self.inner_output,
                ctx,
                Vec::new(),
            )?;
            Ok(Arc::new(ScopeWrap(value)))
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
        fn byte_size(&self) -> u64 {
            size_of::<Self>() as u64 + self.0.byte_size()
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

    // ---- binding-scoped invalidation (MED-CORE-02) -------------------------

    /// Stands in for `net.in`: one value per declared output port, taken from
    /// the scope's bindings when the name matches and derived from the
    /// context otherwise. Time-dependent, like the real interface node.
    struct TestNetIn;

    impl NodeProcessor for TestNetIn {
        fn process(
            &self,
            node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let mut record: Vec<Arc<dyn NodeData>> = Vec::with_capacity(node.outputs.len());
            for port in &node.outputs {
                let bound = scope
                    .bindings()
                    .iter()
                    .find(|(name, _)| *name == port.name)
                    .map(|(_, value)| value.clone());
                record.push(bound.unwrap_or_else(|| Arc::new(Scalar(ctx.frame as f32))));
            }
            Ok(Arc::new(PortRecord(record)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Stands in for `comp.network` above an adjustment layer: enters the
    /// layer scope with a freshly allocated `source` binding, which is what a
    /// composited lower stack delivers as soon as anything below it varies.
    struct AdjustmentBoundary {
        inner: Graph,
        inner_output: NodeId,
        segment: PathSegment,
        source: Arc<AtomicUsize>,
    }

    impl NodeProcessor for AdjustmentBoundary {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let value = self.source.load(Ordering::Relaxed) as f32;
            let bindings: Bindings = vec![(
                network::PORT_SOURCE.to_string(),
                Arc::new(Scalar(value)) as Arc<dyn NodeData>,
            )];
            let produced =
                scope.evaluate_sub(self.segment, &self.inner, self.inner_output, ctx, bindings)?;
            Ok(Arc::new(ScopeWrap(produced)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Process counts of the adjustment-layer fixture, by role.
    struct AdjustmentCalls {
        /// Consumes the interface node's `source` port.
        source_consumer: Arc<AtomicUsize>,
        /// Consumes the interface node's `t` port.
        time_consumer: Arc<AtomicUsize>,
        /// Connected to nothing — a static generator inside the layer.
        standalone: Arc<AtomicUsize>,
        /// The network's output, fed by all three.
        collector: Arc<AtomicUsize>,
    }

    /// Layer network for the MED-CORE-02 tests:
    ///
    /// ```text
    /// net.in ─[source]─▶ 11 ─┐
    ///        ─[t]──────▶ 12 ─┼─▶ 14 (output)
    ///              13 ───────┘
    /// ```
    ///
    /// Only node 11 is downstream of the `source` port; 12 hangs off `t` and
    /// 13 off nothing at all.
    fn adjustment_network() -> Graph {
        let interface = Node::new(NodeId::new(10), network::NET_IN_TYPE_KEY)
            .with_output(network::PORT_SOURCE, DataTypeId::SCALAR)
            .with_output(network::PORT_TIME, DataTypeId::SCALAR);
        let one_input = |id: u64| {
            Node::new(NodeId::new(id), "test")
                .with_input("a", &[DataTypeId::SCALAR])
                .with_output("out", DataTypeId::SCALAR)
        };
        let standalone = Node::new(NodeId::new(13), "test").with_output("out", DataTypeId::SCALAR);
        let collector = Node::new(NodeId::new(14), "test")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_input("b", &[DataTypeId::SCALAR])
            .with_input("c", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        Graph::new()
            .add_node(interface)
            .unwrap()
            .add_node(one_input(11))
            .unwrap()
            .add_node(one_input(12))
            .unwrap()
            .add_node(standalone)
            .unwrap()
            .add_node(collector)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(11),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(10),
                OutputPortIndex(1),
                NodeId::new(12),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(11),
                OutputPortIndex(0),
                NodeId::new(14),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                NodeId::new(12),
                OutputPortIndex(0),
                NodeId::new(14),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(5),
                NodeId::new(13),
                OutputPortIndex(0),
                NodeId::new(14),
                InputPortIndex(2),
            )
            .unwrap()
    }

    /// Register the network's processors on `ev` and return the counters.
    fn register_adjustment_network(ev: &mut Evaluator) -> AdjustmentCalls {
        let calls = AdjustmentCalls {
            source_consumer: Arc::new(AtomicUsize::new(0)),
            time_consumer: Arc::new(AtomicUsize::new(0)),
            standalone: Arc::new(AtomicUsize::new(0)),
            collector: Arc::new(AtomicUsize::new(0)),
        };
        ev.register(NodeId::new(10), Arc::new(TestNetIn));
        ev.register(
            NodeId::new(11),
            Arc::new(CountingSum {
                calls: calls.source_consumer.clone(),
            }),
        );
        ev.register(
            NodeId::new(12),
            Arc::new(CountingSum {
                calls: calls.time_consumer.clone(),
            }),
        );
        ev.register(
            NodeId::new(13),
            Arc::new(CountingConst {
                value: 2.0,
                calls: calls.standalone.clone(),
            }),
        );
        ev.register(
            NodeId::new(14),
            Arc::new(CountingSum {
                calls: calls.collector.clone(),
            }),
        );
        calls
    }

    /// The outer graph and boundary registration driving the layer scope.
    fn register_adjustment_boundary(ev: &mut Evaluator, source: Arc<AtomicUsize>) -> Graph {
        let boundary = Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR);
        ev.register(
            NodeId::new(1),
            Arc::new(AdjustmentBoundary {
                inner: adjustment_network(),
                inner_output: NodeId::new(14),
                segment: PathSegment::Layer(CompId::new(1), LayerId::new(2)),
                source,
            }),
        );
        Graph::new().add_node(boundary).unwrap()
    }

    /// MED-CORE-02, the playback case: an adjustment layer's lower stack
    /// composites to a new `Arc` on every frame, which used to drop *every*
    /// cached value in the layer's scope. A static generator inside the layer
    /// depends on neither time nor the binding and must survive the whole
    /// pass.
    #[test]
    fn adjustment_scope_keeps_its_static_nodes_across_frames() {
        let mut ev = Evaluator::new();
        let calls = register_adjustment_network(&mut ev);
        let source = Arc::new(AtomicUsize::new(5));
        let outer = register_adjustment_boundary(&mut ev, source.clone());

        for frame in 0..8 {
            source.store(5 + frame as usize, Ordering::Relaxed);
            ev.evaluate(&outer, NodeId::new(1), &ctx_at(frame)).unwrap();
        }
        assert_eq!(
            calls.standalone.load(Ordering::Relaxed),
            1,
            "a static node the changed binding cannot reach must keep its cache"
        );
        assert_eq!(
            calls.source_consumer.load(Ordering::Relaxed),
            8,
            "the `source` branch is time-dependent through the interface node"
        );
    }

    /// MED-CORE-02, the same-frame case: an edit below the adjustment layer
    /// rebinds `source` without moving time. Only the `source` port carries a
    /// new value, so the `t` branch and the static generator keep theirs.
    #[test]
    fn changed_binding_spares_the_ports_it_does_not_back() {
        let mut ev = Evaluator::new();
        let calls = register_adjustment_network(&mut ev);
        let source = Arc::new(AtomicUsize::new(5));
        let outer = register_adjustment_boundary(&mut ev, source.clone());

        ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(calls.time_consumer.load(Ordering::Relaxed), 1);

        for value in 6..10 {
            source.store(value, Ordering::Relaxed);
            // Re-run the boundary at the same frame, as an edit to the lower
            // stack would.
            ev.invalidate_node(NodeId::new(1));
            ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
        }
        assert_eq!(
            calls.standalone.load(Ordering::Relaxed),
            1,
            "a node the changed binding cannot reach must keep its cache"
        );
        assert_eq!(
            calls.time_consumer.load(Ordering::Relaxed),
            1,
            "the interface node's other output ports did not change value"
        );
    }

    /// The reverse regression: everything the rebound port actually feeds is
    /// recomputed, and the new value reaches the network output.
    #[test]
    fn changed_binding_recomputes_the_nodes_its_port_reaches() {
        let mut ev = Evaluator::new();
        let calls = register_adjustment_network(&mut ev);
        let source = Arc::new(AtomicUsize::new(5));
        let outer = register_adjustment_boundary(&mut ev, source.clone());

        let read = |ev: &mut Evaluator| -> f32 {
            let out = ev.evaluate(&outer, NodeId::new(1), &ctx_at(0)).unwrap();
            out.downcast_ref::<ScopeWrap>()
                .unwrap()
                .0
                .downcast_ref::<Scalar>()
                .unwrap()
                .0
        };

        // 11 = source + 1, 12 = frame + 1 = 1, 13 = 2, 14 = 11 + 12 + 13 + 1.
        assert!((read(&mut ev) - 10.0).abs() < f32::EPSILON);
        assert_eq!(calls.source_consumer.load(Ordering::Relaxed), 1);
        assert_eq!(calls.collector.load(Ordering::Relaxed), 1);

        source.store(6, Ordering::Relaxed);
        ev.invalidate_node(NodeId::new(1));
        assert!((read(&mut ev) - 11.0).abs() < f32::EPSILON);
        assert_eq!(
            calls.source_consumer.load(Ordering::Relaxed),
            2,
            "the `source` consumer must see the rebound value"
        );
        assert_eq!(
            calls.collector.load(Ordering::Relaxed),
            2,
            "and its downstream must follow"
        );
    }

    /// Re-evaluating a scope whose content is identical — the lower stack is
    /// itself cached, so the binding is the same `Arc` — must not miss at all,
    /// which drives the hit rate to 1 as the repeats accumulate.
    #[test]
    fn repeating_an_unchanged_scope_drives_the_hit_rate_to_one() {
        let mut ev = Evaluator::new();
        let calls = register_adjustment_network(&mut ev);
        let inner = adjustment_network();
        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let stable: Arc<dyn NodeData> = Arc::new(Scalar(5.0));
        let bindings = || -> Bindings {
            vec![(
                network::PORT_SOURCE.to_string(),
                stable.clone() as Arc<dyn NodeData>,
            )]
        };

        ev.evaluate_sub(segment, &inner, NodeId::new(14), &ctx_at(0), bindings())
            .unwrap();
        let warm_misses = ev.cache_stats().misses();

        const REPEATS: usize = 64;
        for _ in 0..REPEATS {
            ev.evaluate_sub(segment, &inner, NodeId::new(14), &ctx_at(0), bindings())
                .unwrap();
        }
        let stats = ev.cache_stats();
        assert_eq!(
            stats.misses(),
            warm_misses,
            "a repeat with identical bindings must not miss"
        );
        let rate = stats.hit_rate().expect("pulls were recorded");
        assert!(rate > 0.98, "hit rate {rate} should approach 1");
        assert_eq!(calls.standalone.load(Ordering::Relaxed), 1);
        assert_eq!(calls.collector.load(Ordering::Relaxed), 1);
    }

    /// A binding no interface output port claims cannot be traced, so the
    /// scope is dropped wholesale rather than silently kept.
    #[test]
    fn unclaimed_binding_name_falls_back_to_dropping_the_scope() {
        let inner = Graph::new().add_node(scalar_node(7)).unwrap();
        let inner_calls = Arc::new(AtomicUsize::new(0));
        let segment = PathSegment::Layer(CompId::new(1), LayerId::new(2));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(7),
            Arc::new(CountingConst {
                value: 1.0,
                calls: inner_calls.clone(),
            }),
        );

        let bound = |value: f32| -> Bindings {
            vec![(
                "nowhere".to_string(),
                Arc::new(Scalar(value)) as Arc<dyn NodeData>,
            )]
        };
        ev.evaluate_sub(segment, &inner, NodeId::new(7), &ctx_at(0), bound(1.0))
            .unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 1);

        // Same binding value but a new `Arc`: unclaimed, hence conservative.
        ev.evaluate_sub(segment, &inner, NodeId::new(7), &ctx_at(0), bound(1.0))
            .unwrap();
        assert_eq!(inner_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn iteration_and_time_shift_paths_keep_distinct_cache_entries() {
        let node = NodeId::new(7);
        let graph = Graph::new().add_node(scalar_node(node.raw())).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            node,
            Arc::new(CountingConst {
                value: 1.0,
                calls: calls.clone(),
            }),
        );

        let paths = [
            vec![PathSegment::Iteration(NodeId::new(10), 0)],
            vec![PathSegment::Iteration(NodeId::new(10), 1)],
            vec![PathSegment::TimeShift(NodeId::new(20), 10)],
            vec![PathSegment::TimeShift(NodeId::new(20), 20)],
        ];

        for path in &paths {
            ev.evaluate_at(path, &graph, node, &ctx_at(0)).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 4);

        for path in &paths {
            ev.evaluate_at(path, &graph, node, &ctx_at(0)).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert!(paths.iter().all(|path| ev.cache_contains(&NodeKey {
            path: path.clone(),
            node,
        })));
    }

    #[test]
    fn iteration_and_time_shift_scope_invalidation_uses_path_prefixes() {
        let node = NodeId::new(7);
        let graph = Graph::new().add_node(scalar_node(node.raw())).unwrap();
        let mut ev = Evaluator::new();
        ev.register(
            node,
            Arc::new(CountingConst {
                value: 1.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        let iteration_zero = vec![
            PathSegment::Iteration(NodeId::new(10), 0),
            PathSegment::Subnet(NodeId::new(11)),
        ];
        let iteration_one = vec![
            PathSegment::Iteration(NodeId::new(10), 1),
            PathSegment::Subnet(NodeId::new(11)),
        ];
        let shift_ten = vec![
            PathSegment::TimeShift(NodeId::new(20), 10),
            PathSegment::Subnet(NodeId::new(21)),
        ];
        let shift_twenty = vec![
            PathSegment::TimeShift(NodeId::new(20), 20),
            PathSegment::Subnet(NodeId::new(21)),
        ];

        for path in [&iteration_zero, &iteration_one, &shift_ten, &shift_twenty] {
            ev.evaluate_at(path, &graph, node, &ctx_at(0)).unwrap();
        }

        ev.invalidate_scope(&[PathSegment::Iteration(NodeId::new(10), 0)]);
        ev.invalidate_scope(&[PathSegment::TimeShift(NodeId::new(20), 10)]);

        assert!(!ev.cache_contains(&NodeKey {
            path: iteration_zero,
            node,
        }));
        assert!(ev.cache_contains(&NodeKey {
            path: iteration_one,
            node,
        }));
        assert!(!ev.cache_contains(&NodeKey {
            path: shift_ten,
            node,
        }));
        assert!(ev.cache_contains(&NodeKey {
            path: shift_twenty,
            node,
        }));
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

    /// A layer's rendered frame folds in its ancestors' transforms, so a shell
    /// edit anywhere up the parent chain has to drop the descendants' cached
    /// frames. A muted parent is never compiled, so no `parent_transform` edge
    /// exists to carry the freshness — without this the child kept drawing at
    /// the old parent position until something else invalidated the cache.
    #[test]
    fn shell_edit_invalidates_descendant_layers() {
        use crate::animation::channel::AnimationChannel;
        use crate::composition::compile::deterministic_node_id;
        use crate::composition::{Composition, Document, Layer};

        let comp_id = CompId::new(1);
        let parent_id = LayerId::new(1);
        let child_id = LayerId::new(2);
        let sibling_id = LayerId::new(3);
        let document = |parent_x: f32| {
            let mut parent = Layer::new(parent_id, "P", Graph::new());
            parent.muted = true;
            parent.transform.position[0] = AnimationChannel::constant(parent_x);
            Arc::new(
                Document::default().with_composition(
                    Composition::new(comp_id, "C", (16, 16), FPS, 100)
                        .add_layer(parent)
                        .add_layer(Layer::new(child_id, "C", Graph::new()).with_parent(parent_id))
                        .add_layer(Layer::new(sibling_id, "S", Graph::new())),
                ),
            )
        };

        // Deleting a layer leaves its children's `parent` dangling: the child's
        // own shell is untouched, yet its world matrix loses the ancestor.
        let without_parent = || {
            Arc::new(
                Document::default().with_composition(
                    Composition::new(comp_id, "C", (16, 16), FPS, 100)
                        .add_layer(Layer::new(child_id, "C", Graph::new()).with_parent(parent_id))
                        .add_layer(Layer::new(sibling_id, "S", Graph::new())),
                ),
            )
        };

        let cached = |layer: LayerId| NodeKey {
            path: Vec::new(),
            node: deterministic_node_id(comp_id, layer, NodeRole::Transform),
        };
        let seed = |ev: &mut Evaluator| {
            for layer in [child_id, sibling_id] {
                ev.seed_cache(
                    cached(layer),
                    CacheIdentity::of(&ctx_at(0), true, false),
                    Arc::new(Scalar(1.0)),
                );
            }
        };
        let mut ev = Evaluator::new();
        ev.set_document(document(0.0));
        seed(&mut ev);

        ev.set_document(document(50.0));
        assert!(
            !ev.cache_contains(&cached(child_id)),
            "the child inherits the moved parent's transform"
        );
        assert!(
            ev.cache_contains(&cached(sibling_id)),
            "an unrelated layer keeps its cached frame"
        );

        seed(&mut ev);
        ev.set_document(without_parent());
        assert!(
            !ev.cache_contains(&cached(child_id)),
            "the child's chain lost an ancestor"
        );
        assert!(
            ev.cache_contains(&cached(sibling_id)),
            "an unrelated layer keeps its cached frame"
        );
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

    // ---- TimeKey quantisation ---------------------------------------------

    #[test]
    fn time_key_quantises_a_frame_position_to_ticks() {
        assert_eq!(TimeKey::from_frame_position(0.0).ticks(), 0);
        assert_eq!(TimeKey::from_frame_position(1.0).ticks(), 4096);
        assert_eq!(TimeKey::from_frame_position(10.5).ticks(), 43008);
        // Positions closer than half a tick collapse onto one key.
        assert_eq!(
            TimeKey::from_frame_position(1.0),
            TimeKey::from_frame_position(1.0 + 0.4 / TimeKey::SUBFRAME_SCALE)
        );
        // A whole tick apart stays distinguishable.
        assert_ne!(
            TimeKey::from_frame_position(1.0),
            TimeKey::from_frame_position(1.0 + 1.0 / TimeKey::SUBFRAME_SCALE)
        );
    }

    #[test]
    fn time_key_never_collides_with_the_timeless_sentinel() {
        assert!(TimeKey::TIMELESS.is_timeless());
        assert!(!TimeKey::from_frame_position(0.0).is_timeless());
        // Saturating conversions must not land on the sentinel either.
        assert!(!TimeKey::from_frame_position(f64::NEG_INFINITY).is_timeless());
        assert!(!TimeKey::from_frame_position(f64::MIN).is_timeless());
        assert_eq!(TimeKey::from_frame_position(f64::NAN).ticks(), 0);
    }

    #[test]
    fn time_key_agrees_across_arithmetic_routes() {
        // One instant reached by `frame / fps` and by a shutter offset added
        // to a frame's time must land on one key. At 30000/1001 the two
        // routes disagree in the last bits of the continuous position; the
        // quantum absorbs that instead of missing the cache silently.
        const NTSC: FrameRate = FrameRate {
            num: 30000,
            den: 1001,
        };
        let fps = NTSC.as_f64();
        let offset = 1.0 / 32.0; // an 11.25° shutter, expressed in frames

        // Route A: the sub-frame offset is converted to seconds and added.
        let mut shuttered = EvalContext::new(10, NTSC, (16, 16));
        shuttered.time += offset / fps;
        // Route B: the continuous frame position is divided by the frame rate.
        let mut divided = EvalContext::new(10, NTSC, (16, 16));
        divided.time = (10.0 + offset) / fps;

        assert_ne!(
            shuttered.sample_frame(),
            divided.sample_frame(),
            "the routes are expected to disagree in the last bits; without \
             that this test proves nothing"
        );
        assert_eq!(
            TimeKey::from_frame_position(shuttered.sample_frame()),
            TimeKey::from_frame_position(divided.sample_frame())
        );
    }

    // ---- cache identity: the time axis -------------------------------------

    /// A time-dependent source emitting the continuous frame position.
    struct SampleFrameSource {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for SampleFrameSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Scalar(ctx.sample_frame() as f32)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    fn sub_frame_ctx(frame: u64, offset: f64) -> EvalContext {
        let mut ctx = ctx_at(frame);
        ctx.time += offset / FPS.as_f64();
        ctx
    }

    fn single_source_graph() -> Graph {
        Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap()
    }

    fn time_source() -> (Graph, Evaluator, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(SampleFrameSource {
                calls: calls.clone(),
            }),
        );
        (single_source_graph(), ev, calls)
    }

    #[test]
    fn the_same_time_key_is_served_from_cache() {
        let (g, mut ev, calls) = time_source();
        let ctx = sub_frame_ctx(10, 0.25);
        let first = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
        let second = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1, "a re-request must hit");
        assert!(Arc::ptr_eq(&first, &second));

        // A position inside the same tick is the same request.
        let nudged = sub_frame_ctx(10, 0.25 + 0.4 / TimeKey::SUBFRAME_SCALE);
        ev.evaluate(&g, NodeId::new(1), &nudged).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn integer_frame_stepping_keeps_its_cache_behaviour() {
        // The pre-CACHE-2 behaviour, unchanged: a time-dependent node
        // recomputes once per frame and is served from cache within it.
        let (g, mut ev, calls) = time_source();
        for frame in 0..4u64 {
            let value = ev.evaluate(&g, NodeId::new(1), &ctx_at(frame)).unwrap();
            assert_eq!(value.downcast_ref::<Scalar>().unwrap().0, frame as f32);
            ev.evaluate(&g, NodeId::new(1), &ctx_at(frame)).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn sub_frame_positions_within_one_frame_are_evaluated_separately() {
        // MED-CORE-03 and the old BLUR-2: a shutter interval varies `time`
        // while `frame` stands still. Keying validity on the integer frame
        // served every sample from the first one's cache, which is the
        // "motion blur is implemented but nothing blurs" failure.
        let (g, mut ev, calls) = time_source();
        let mut seen = Vec::new();
        for step in 0..4 {
            let ctx = sub_frame_ctx(10, f64::from(step) * 0.25);
            let value = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
            seen.push(value.downcast_ref::<Scalar>().unwrap().0);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 4, "each sample recomputes");
        assert_eq!(seen, vec![10.0, 10.25, 10.5, 10.75]);
    }

    #[test]
    fn a_time_independent_node_still_spans_frames() {
        // TimeKey::TIMELESS: constants keep being served across frames, and
        // across sub-frame positions too.
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 3.0,
                calls: calls.clone(),
            }),
        );
        let g = single_source_graph();
        for frame in 0..5u64 {
            ev.evaluate(&g, NodeId::new(1), &ctx_at(frame)).unwrap();
        }
        ev.evaluate(&g, NodeId::new(1), &sub_frame_ctx(2, 0.5))
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    // ---- cache identity: the precision axis --------------------------------

    fn const_source() -> (Graph, Evaluator, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: calls.clone(),
            }),
        );
        (single_source_graph(), ev, calls)
    }

    #[test]
    fn a_reduced_entry_does_not_answer_a_full_precision_request() {
        // What keeps an export from inheriting a preview's reduced value.
        let (g, mut ev, calls) = const_source();
        ev.evaluate(
            &g,
            NodeId::new(1),
            &ctx_at(0).with_min_precision(Precision::F16),
        )
        .unwrap();
        ev.evaluate(
            &g,
            NodeId::new(1),
            &ctx_at(0).with_min_precision(Precision::F32),
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "an F32 request must not be served an F16 entry"
        );
    }

    #[test]
    fn a_full_precision_entry_answers_a_reduced_request_unchanged() {
        // Precision is the one ordered axis, and reuse is verbatim: the
        // stored value is handed over as it is, never converted down.
        let (g, mut ev, calls) = const_source();
        let stored = ev
            .evaluate(
                &g,
                NodeId::new(1),
                &ctx_at(0).with_min_precision(Precision::F32),
            )
            .unwrap();
        for floor in [Precision::F16, Precision::U8] {
            let served = ev
                .evaluate(&g, NodeId::new(1), &ctx_at(0).with_min_precision(floor))
                .unwrap();
            assert!(
                Arc::ptr_eq(&stored, &served),
                "the stored value is served as-is, with no conversion"
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    // ---- cache identity: the quality axis ----------------------------------

    #[test]
    fn a_context_asks_for_final_quality_unless_it_says_otherwise() {
        // The default is the safe end of the axis, mirroring `Precision::F32`:
        // an evaluation path that has never heard of `Quality` — the export
        // worker `EXPORT-2` will add, a headless tool, an audio mixdown —
        // cannot silently ship a preview-grade picture. Only a path that
        // deliberately opts down gets `Preview`.
        assert_eq!(ctx_at(0).quality, Quality::Final);
        assert_eq!(Quality::default(), Quality::Final);
        assert_eq!(
            ctx_at(0).with_quality(Quality::Preview).quality,
            Quality::Preview
        );
    }

    #[test]
    fn switching_quality_invalidates_the_cached_value() {
        // Symmetric, unlike precision: neither stage answers the other. The
        // `Preview -> Final` direction is what protects an export from
        // inheriting a two-sample blur; the `Final -> Preview` direction is
        // the claim that the stages are different images rather than two
        // grades of one, so it must miss as well.
        for [first, second] in [
            [Quality::Preview, Quality::Final],
            [Quality::Final, Quality::Preview],
        ] {
            let (g, mut ev, calls) = const_source();
            ev.evaluate(&g, NodeId::new(1), &ctx_at(0).with_quality(first))
                .unwrap();
            ev.evaluate(&g, NodeId::new(1), &ctx_at(0).with_quality(second))
                .unwrap();

            assert_eq!(
                calls.load(Ordering::Relaxed),
                2,
                "{first:?} -> {second:?} reused the other stage's value"
            );
            assert_eq!(
                ev.cache_stats().misses_for(CacheMiss::QualityChanged),
                1,
                "{first:?} -> {second:?} missed for the wrong reason"
            );

            // The same stage twice is a hit, so the recompute above is
            // attributable to the stage and not to a cache that never stores.
            ev.evaluate(&g, NodeId::new(1), &ctx_at(0).with_quality(second))
                .unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        }
    }

    /// `EXPORT-2` has not landed, so no production caller asks for `Final`
    /// explicitly yet. What can be pinned today is the wiring an export will
    /// rely on: the request it will build reaches [`CacheIdentity`] and is
    /// refused a preview entry. Without this the axis could be added, left
    /// unread by `CacheIdentity::of`, and pass every other test.
    #[test]
    fn an_export_shaped_request_is_not_served_a_preview_entry() {
        let (g, mut ev, calls) = const_source();
        let preview = ev
            .evaluate(
                &g,
                NodeId::new(1),
                &EvalContext::new(0, FPS, (1920, 1080)).with_quality(Quality::Preview),
            )
            .unwrap();
        let exported = ev
            .evaluate(
                &g,
                NodeId::new(1),
                &EvalContext::new(0, FPS, (1920, 1080)).with_quality(Quality::Final),
            )
            .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(
            !Arc::ptr_eq(&preview, &exported),
            "the export request was handed the preview's value"
        );
    }

    #[test]
    fn quality_and_resolution_are_independent_axes() {
        // The `ViewerResolution` factor scales `resolution`; `quality` counts
        // samples. Every combination is its own cache entry, and neither axis
        // shadows the other: switching stage at a fixed resolution misses, and
        // switching resolution at a fixed stage misses.
        let (g, mut ev, calls) = const_source();
        let ctx = |resolution, quality| {
            EvalContext::new(0, FPS, resolution)
                .with_comp_resolution((1920, 1080))
                .with_quality(quality)
        };
        let combinations = [
            ((1920, 1080), Quality::Preview),
            ((1920, 1080), Quality::Final),
            ((480, 270), Quality::Preview),
            ((480, 270), Quality::Final),
        ];

        for (i, &(resolution, quality)) in combinations.iter().enumerate() {
            ev.evaluate(&g, NodeId::new(1), &ctx(resolution, quality))
                .unwrap();
            assert_eq!(
                calls.load(Ordering::Relaxed),
                i + 1,
                "{resolution:?} x {quality:?} reused another combination"
            );
            // Both `Full x Preview` and `Quarter x Final` evaluate and cache
            // in their own right: the repeat is a hit.
            ev.evaluate(&g, NodeId::new(1), &ctx(resolution, quality))
                .unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), i + 1);
        }
    }

    // ---- deferred parameter materialisation (HIGH-03) ----------------------

    fn path_point(x: f32) -> crate::graph::PathPoint {
        crate::graph::PathPoint {
            p: crate::types::Vec2(x, 0.0),
            in_tan: crate::types::Vec2(0.0, 0.0),
            out_tan: crate::types::Vec2(0.0, 0.0),
        }
    }

    #[test]
    fn a_cache_hit_does_not_materialise_parameters() {
        // HIGH-03: materialisation is where a hand-drawn path gets cloned.
        // A pull served from cache must never reach it.
        let points: Vec<_> = (0..256).map(|i| path_point(i as f32)).collect();
        let g = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("path", ParameterValue::PathPoints(points))
                    .with_param("label", ParameterValue::String("hello".into())),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(CountingConst {
                value: 1.0,
                calls: calls.clone(),
            }),
        );

        ev.evaluate(&g, NodeId::new(1), &ctx_at(0)).unwrap();
        assert_eq!(ev.param_materializations, 1, "the first pull processes");
        for frame in 0..8u64 {
            ev.evaluate(&g, NodeId::new(1), &ctx_at(frame)).unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            ev.param_materializations, 1,
            "cached pulls must not rebuild the parameters"
        );
    }

    /// Records the parameters it was handed.
    struct ParamRecorder {
        seen: Arc<std::sync::Mutex<Vec<(String, ResolvedValue)>>>,
    }

    impl NodeProcessor for ParamRecorder {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            *self.seen.lock().unwrap() = params.values.clone();
            Ok(Arc::new(Scalar(0.0)))
        }
    }

    #[test]
    fn materialised_parameters_keep_stored_order_and_values() {
        // Resolving in two passes must not reorder or drop anything:
        // constants and channels interleave in the node's own order.
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 1.0, Interpolation::Linear);
        curve.insert(10, 11.0, Interpolation::Linear);
        let g = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("first", ParameterValue::Int(7))
                    .with_param(
                        "second",
                        ParameterValue::Channel(AnimationChannel::keyframes(curve)),
                    )
                    .with_param("third", ParameterValue::String("x".into()))
                    .with_param("fourth", ParameterValue::Bool(true)),
            )
            .unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(ParamRecorder { seen: seen.clone() }),
        );
        ev.evaluate(&g, NodeId::new(1), &ctx_at(5)).unwrap();

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                ("first".to_string(), ResolvedValue::Int(7)),
                ("second".to_string(), ResolvedValue::Float(6.0)),
                ("third".to_string(), ResolvedValue::Str("x".into())),
                ("fourth".to_string(), ResolvedValue::Bool(true)),
            ]
        );
    }

    // ---- CACHE-3: budget, eviction, pruning and statistics -----------------

    /// Emits a frame buffer of a fixed size, so a test can state the cache
    /// budget in whole entries.
    struct FrameSized {
        width: u32,
        height: u32,
    }

    impl NodeProcessor for FrameSized {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(crate::types::FrameBuffer::new_zeroed(
                self.width,
                self.height,
            )))
        }
    }

    /// A graph of `count` unconnected frame-buffer sources, all registered.
    fn frame_source_graph(ev: &mut Evaluator, count: u64, size: u32) -> Graph {
        let mut graph = Graph::new();
        for index in 0..count {
            let id = NodeId::new(100 + index);
            graph = graph
                .add_node(Node::new(id, "test").with_output("out", DataTypeId::FRAME_BUFFER))
                .unwrap();
            ev.register(
                id,
                Arc::new(FrameSized {
                    width: size,
                    height: size,
                }),
            );
        }
        graph
    }

    fn budget_of(ram_bytes: u64) -> crate::cache_budget::SharedCacheBudget {
        crate::cache_budget::SharedCacheBudget::new(crate::cache_budget::CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        })
    }

    #[test]
    fn an_unbudgeted_evaluator_caches_without_a_limit() {
        // The pre-CACHE-3 behaviour every test and example still relies on.
        let mut ev = Evaluator::new();
        let graph = frame_source_graph(&mut ev, 32, 64);
        for index in 0..32u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        assert_eq!(ev.cache_stats().entries, 32);
    }

    #[test]
    fn the_budget_evicts_the_oldest_entry_and_holds_the_line() {
        // 64x64 RGBA f32 is 16 KiB of pixels; allow four entries' worth.
        let entry_bytes = 64u64 * 64 * 16;
        let limit = entry_bytes * 4 + 4 * 1024;
        let mut ev = Evaluator::with_budget(budget_of(limit));
        let graph = frame_source_graph(&mut ev, 16, 64);

        for index in 0..16u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }

        let stats = ev.cache_stats();
        assert!(
            stats.bytes(Tier::Ram) <= limit,
            "cached bytes {} exceed the budget {limit}",
            stats.bytes(Tier::Ram)
        );
        assert!(stats.entries < 16, "nothing was evicted");
        // Oldest first: the earliest nodes are gone, the newest are kept.
        assert!(!ev.cache_contains(&NodeKey {
            path: Vec::new(),
            node: NodeId::new(100),
        }));
        assert!(ev.cache_contains(&NodeKey {
            path: Vec::new(),
            node: NodeId::new(115),
        }));
        assert!(ev.store.index_is_consistent());
    }

    #[test]
    fn a_re_read_entry_outlives_an_untouched_one() {
        let entry_bytes = 64u64 * 64 * 16;
        let mut ev = Evaluator::with_budget(budget_of(entry_bytes * 2 + 4 * 1024));
        let graph = frame_source_graph(&mut ev, 3, 64);
        let key = |index: u64| NodeKey {
            path: Vec::new(),
            node: NodeId::new(100 + index),
        };

        ev.evaluate(&graph, NodeId::new(100), &ctx_at(0)).unwrap();
        ev.evaluate(&graph, NodeId::new(101), &ctx_at(0)).unwrap();
        // Serve node 100 from cache: it is now the most recently used.
        ev.evaluate(&graph, NodeId::new(100), &ctx_at(0)).unwrap();
        ev.evaluate(&graph, NodeId::new(102), &ctx_at(0)).unwrap();

        assert!(ev.cache_contains(&key(0)), "the re-read entry was evicted");
        assert!(!ev.cache_contains(&key(1)), "the idle entry survived");
    }

    #[test]
    fn evicting_a_value_releases_its_bytes_to_the_budget() {
        let entry_bytes = 64u64 * 64 * 16;
        let budget = budget_of(entry_bytes * 2 + 4 * 1024);
        let mut ev = Evaluator::with_budget(budget.clone());
        let graph = frame_source_graph(&mut ev, 8, 64);
        for index in 0..8u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        // The store and the budget agree, so nothing leaked a reservation.
        assert_eq!(
            budget.stats().used(Tier::Ram),
            ev.cache_stats().bytes(Tier::Ram)
        );
        ev.invalidate_all();
        assert_eq!(budget.stats().used(Tier::Ram), 0);
        assert_eq!(budget.stats().entries, 0);
    }

    #[test]
    fn reset_clears_the_state_and_keeps_the_budget() {
        let entry_bytes = 64u64 * 64 * 16;
        let budget = budget_of(entry_bytes * 4 + 4 * 1024);
        let mut ev = Evaluator::with_budget(budget.clone());
        let graph = frame_source_graph(&mut ev, 2, 64);
        for index in 0..2u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        assert!(ev.cache_stats().entries > 0);
        assert!(budget.stats().used(Tier::Ram) > 0);

        ev.reset();

        // Everything rebuilt from the graph is gone...
        assert_eq!(ev.cache_stats().entries, 0);
        assert_eq!(budget.stats().used(Tier::Ram), 0);
        assert!(ev.processor(NodeId::new(100)).is_none());
        // ...but the budget is not, so the rebuilt evaluator is still bounded.
        let graph = frame_source_graph(&mut ev, 2, 64);
        ev.evaluate(&graph, NodeId::new(100), &ctx_at(0)).unwrap();
        assert!(
            budget.stats().entries > 0,
            "a reset evaluator stopped reporting to the budget"
        );
    }

    #[test]
    fn removing_a_layer_leaves_no_scope_state_behind() {
        let comp_id = CompId::new(1);
        let layer_id = LayerId::new(2);
        let inner = adjustment_network();
        let mut ev = Evaluator::new();
        register_adjustment_network(&mut ev);
        let segment = PathSegment::Layer(comp_id, layer_id);
        let source: Arc<dyn NodeData> = Arc::new(Scalar(5.0));

        ev.evaluate_sub(
            segment,
            &inner,
            NodeId::new(14),
            &ctx_at(0),
            vec![(network::PORT_SOURCE.to_string(), source)],
        )
        .unwrap();
        assert!(ev.scope_bindings.contains_key(&vec![segment]));
        assert!(ev.scope_reach.contains_key(&vec![segment]));

        // What `set_document` does for a layer present only in the old
        // snapshot.
        ev.invalidate_scope(&[segment]);

        assert!(
            !ev.scope_bindings.contains_key(&vec![segment]),
            "the deleted layer's bindings still hold its source frame"
        );
        assert!(
            !ev.scope_reach.contains_key(&vec![segment]),
            "the deleted layer's reach still holds a Graph clone"
        );
        assert!(
            !ev.scope_owners.contains_key(&vec![segment]),
            "the deleted layer's owner record survived"
        );
    }

    #[test]
    fn deleting_a_layer_through_the_document_prunes_its_scope_state() {
        use crate::composition::Composition;
        let comp_id = CompId::new(1);
        let layer_id = LayerId::new(2);
        let network = adjustment_network();
        let with_layer = Arc::new(Document::default().with_composition(
            Composition::new(comp_id, "C", (16, 16), FPS, 100).add_layer(Layer::new(
                layer_id,
                "L",
                network.clone(),
            )),
        ));
        let without_layer = Arc::new(Document::default().with_composition(Composition::new(
            comp_id,
            "C",
            (16, 16),
            FPS,
            100,
        )));

        let mut ev = Evaluator::new();
        register_adjustment_network(&mut ev);
        ev.set_document(with_layer);
        let segment = PathSegment::Layer(comp_id, layer_id);
        ev.evaluate_sub(
            segment,
            &network,
            NodeId::new(14),
            &ctx_at(0),
            vec![(
                network::PORT_SOURCE.to_string(),
                Arc::new(Scalar(1.0)) as Arc<dyn NodeData>,
            )],
        )
        .unwrap();
        assert!(!ev.scope_bindings.is_empty());

        ev.set_document(without_layer);
        assert!(
            ev.scope_bindings.is_empty(),
            "a removed layer's bindings leaked past the document swap"
        );
        assert!(ev.scope_reach.is_empty());
    }

    #[test]
    fn register_does_not_walk_the_cache() {
        // The MED-CORE-07 regression: `register()` used to iterate the whole
        // cache and the whole dirty set to find a node's paths. Measured in
        // entries examined, not in time — a timing assertion would be flaky
        // and would not say what broke.
        let examined_for = |others: u64| -> usize {
            let mut ev = Evaluator::new();
            let graph = frame_source_graph(&mut ev, others + 1, 8);
            let target = NodeId::new(100 + others);
            for index in 0..=others {
                ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                    .unwrap();
            }
            let before = ev.store.entries_examined();
            ev.register(
                target,
                Arc::new(FrameSized {
                    width: 8,
                    height: 8,
                }),
            );
            ev.store.entries_examined() - before
        };

        let small = examined_for(10);
        let large = examined_for(100);
        assert_eq!(
            small, large,
            "register() examined {small} entries with 11 cached and {large} with 101"
        );
    }

    #[test]
    fn the_reverse_index_survives_every_kind_of_invalidation() {
        let mut ev = Evaluator::new();
        let graph = frame_source_graph(&mut ev, 6, 8);
        let scope = vec![PathSegment::Subnet(NodeId::new(9))];
        for index in 0..6u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
            ev.evaluate_at(&scope, &graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        assert!(ev.store.index_is_consistent());

        ev.mark_dirty(&graph, NodeId::new(101));
        assert!(ev.store.index_is_consistent());

        ev.invalidate_node(NodeId::new(102));
        assert!(ev.store.index_is_consistent());

        ev.invalidate_scope(&scope);
        assert!(ev.store.index_is_consistent());

        // Re-caching after the scope drop, then a full flush.
        ev.evaluate_at(&scope, &graph, NodeId::new(103), &ctx_at(0))
            .unwrap();
        assert!(ev.store.index_is_consistent());
        ev.invalidate_all();
        assert!(ev.store.index_is_consistent());
        assert_eq!(ev.cache_stats().entries, 0);
        assert_eq!(ev.cache_stats().bytes(Tier::Ram), 0);
    }

    #[test]
    fn cache_stats_counts_every_pull_exactly_once() {
        let mut ev = Evaluator::new();
        let graph = frame_source_graph(&mut ev, 4, 8);

        // Cold: four pulls, four misses. Registration marks each node dirty,
        // so that — and not the absent entry — is the reason reported.
        for index in 0..4u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        let cold = ev.cache_stats();
        assert_eq!(cold.pulls(), 4);
        assert_eq!(cold.hits, 0);
        assert_eq!(cold.misses_for(CacheMiss::Dirty), 4);

        // Warm: the same four pulls are hits and nothing else moves.
        ev.reset_cache_stats();
        for index in 0..4u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        let warm = ev.cache_stats();
        assert_eq!(warm.hits, 4);
        assert_eq!(warm.misses(), 0);
        assert_eq!(warm.hit_rate(), Some(1.0));

        // An edit shows up as its own reason.
        ev.reset_cache_stats();
        ev.mark_dirty(&graph, NodeId::new(101));
        ev.evaluate(&graph, NodeId::new(101), &ctx_at(0)).unwrap();
        let dirtied = ev.cache_stats();
        assert_eq!(dirtied.misses_for(CacheMiss::Dirty), 1);
        assert_eq!(dirtied.pulls(), 1);
    }

    #[test]
    fn cache_stats_bytes_track_what_is_held() {
        let mut ev = Evaluator::new();
        let graph = frame_source_graph(&mut ev, 3, 64);
        assert_eq!(ev.cache_stats().bytes(Tier::Ram), 0);
        for index in 0..3u64 {
            ev.evaluate(&graph, NodeId::new(100 + index), &ctx_at(0))
                .unwrap();
        }
        let stats = ev.cache_stats();
        assert_eq!(stats.entries, 3);
        assert!(stats.bytes(Tier::Ram) >= 3 * 64 * 64 * 16);
        // A CPU buffer is not VRAM.
        assert_eq!(stats.bytes(Tier::Vram), 0);
    }

    #[test]
    fn a_bypass_pass_through_is_counted_like_any_other_pull() {
        let mut bypassed = Node::new(NodeId::new(2), "test")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        bypassed.metadata.bypassed = true;
        let graph = Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap()
            .add_node(bypassed)
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
            Arc::new(CountingConst {
                value: 2.0,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );

        ev.evaluate(&graph, NodeId::new(2), &ctx_at(0)).unwrap();
        ev.reset_cache_stats();
        ev.evaluate(&graph, NodeId::new(2), &ctx_at(0)).unwrap();
        let stats = ev.cache_stats();
        // Two nodes, two pulls, both hits — the pass-through included.
        assert_eq!(stats.pulls(), 2);
        assert_eq!(stats.hits, 2);
    }
}

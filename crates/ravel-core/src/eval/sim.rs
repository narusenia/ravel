// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stateful evaluation and the simulation cache (REQ-CORE-011,
//! `docs/implementation/stateful-eval-plan.md`).
//!
//! Hybrid Pull assumes the value at frame `t` follows from `t` alone. A node
//! that integrates over time — particles, trails, physics — needs the state
//! of frame `t - 1`. Such a node implements [`StatefulProcessor`] and is
//! registered through [`stateful_processor`]; to everything downstream it is
//! an ordinary node that produces an `Arc<dyn NodeData>`.
//!
//! # One mechanism
//!
//! - **Keyed like everything else.** A simulation track lives under the same
//!   [`NodeKey`] (`PathSegment` scope included) as a cached node result. There
//!   is no second key type and no per-feature copy of the cache.
//! - **A separate map, not the node-result cache.** A track is an ordered run
//!   of frames filled one after another, and scrubbing over it would otherwise
//!   push hundreds of entries through the node-result LRU. It still reports
//!   to the one [`SharedCacheBudget`], as [`CacheKind::Sim`], whose protected
//!   share ordinary frame pressure cannot spend.
//! - **Invalidated by premises, not by notification.** A track records the
//!   hash of what its states were made from, and a pull compares it with the
//!   current one before using a state (`.agents/rules/rust.md`: a result is
//!   accepted by comparing premises). Dirty marking still recomputes the
//!   node's *output*; whether the *track* survives is decided here.
//!
//! # Determinism
//!
//! `step` may depend on `ctx`, `prev` and the inputs it is given, and on
//! nothing else — no clock, no `rand`. Randomness is a hash of a `seed`
//! parameter and the element's `id` attribute. A pull at frame 10 and a pull
//! at frame 10 after scrubbing back from 300 must return the same state.
//!
//! The states of frames `1..=t` are made from the node's inputs and
//! parameters **at those frames**, not at the frame that happened to be
//! requested: [`Evaluator::sim_state_at`] pulls them per frame, as the scope
//! `TimeShift(node, frame)`.

use super::{
    CancelCheck, EvalContext, EvalError, EvalScope, Evaluator, NodeData, NodeKey, NodeProcessor,
    PathId, PathSegment, ResolveBudget, ResolveOptions, ResolvedParams, parameter_overlays,
};
use crate::cache_budget::{
    CacheKind, Evicted, Reservation, ReservationId, SharedCacheBudget, Tier,
};
use crate::composition::compile::{NodeRole, decode_deterministic_node_id};
use crate::composition::validate::{
    COMP_INFO_COMP_PARAM, COMP_INFO_TYPE_KEY, LAYER_INFO_TYPE_KEY, LAYER_REF_TYPE_KEY,
};
use crate::composition::{Composition, Document, Layer};
use crate::graph::{Graph, Node};
use crate::id::{CompId, LayerId, NodeId};
use crate::network;
use crate::types::FrameRate;
use std::any::Any;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

/// The frame a simulation starts at. `initial` produces this frame's state;
/// every later frame is one `step`.
pub const SIM_START_FRAME: u64 = 0;

// ===========================================================================
// The trait authors implement
// ===========================================================================

/// What `initial` and `step` are given besides the context: the node, its
/// inputs and its resolved parameters **at the frame being computed**.
pub struct SimInputs<'a> {
    /// The node being simulated.
    pub node: &'a Node,
    /// One slot per declared input port, in port order; `None` where nothing
    /// is wired. Parameter ports are stripped, as for a [`NodeProcessor`].
    pub inputs: &'a [Option<Arc<dyn NodeData>>],
    /// Parameters at this frame (keyframes, expressions and parameter ports
    /// resolved for it).
    pub params: &'a ResolvedParams,
}

/// A node whose value at frame `t` is a function of its value at `t - 1`.
///
/// Implementations must be deterministic (see the module documentation).
/// `ctx` carries the frame and the frame rate; reading the output resolution
/// or quality from it makes a state depend on how the frame was requested,
/// which the cache does not (and must not) tell apart.
pub trait StatefulProcessor: Send + Sync + 'static {
    /// The simulation state of one frame, shared behind an `Arc`.
    type State: Send + Sync + 'static;

    /// The state at [`SIM_START_FRAME`].
    fn initial(&self, ctx: &EvalContext, inputs: &SimInputs<'_>) -> Self::State;

    /// The state at `ctx.frame`, given the state of the frame before it.
    fn step(&self, prev: &Self::State, ctx: &EvalContext, inputs: &SimInputs<'_>) -> Self::State;

    /// What the node outputs for `state`. Downstream nodes see only this.
    fn output(&self, state: &Self::State) -> Arc<dyn NodeData>;

    /// Bytes `state` holds, charged to the simulation share of the cache
    /// budget. The default counts the struct itself only; a state that owns
    /// buffers should say how large they are.
    fn byte_size(&self, state: &Self::State) -> u64 {
        let _ = state;
        std::mem::size_of::<Self::State>() as u64
    }
}

/// The per-frame inputs and parameters a step is given.
type StepArgs = (Vec<Option<Arc<dyn NodeData>>>, ResolvedParams);

/// A type-erased simulation state.
pub type SimState = Arc<dyn Any + Send + Sync>;

/// [`StatefulProcessor`] with its state type erased, which is what the
/// evaluator drives.
pub trait SimStepper: Send + Sync {
    /// The state at [`SIM_START_FRAME`].
    fn initial(&self, ctx: &EvalContext, inputs: &SimInputs<'_>) -> SimState;
    /// The state after `prev`.
    fn step(&self, prev: &SimState, ctx: &EvalContext, inputs: &SimInputs<'_>) -> SimState;
    /// The node output for `state`.
    fn output(&self, state: &SimState) -> Option<Arc<dyn NodeData>>;
    /// Bytes `state` holds.
    fn byte_size(&self, state: &SimState) -> u64;
}

/// The adapter that makes a [`StatefulProcessor`] an ordinary
/// [`NodeProcessor`]. Build one with [`stateful_processor`].
pub struct StatefulAdapter<P: StatefulProcessor>(P);

/// Wrap `processor` for registration with [`crate::eval::Evaluator::register`]
/// or any [`crate::eval::ProcessorRegistry`].
pub fn stateful_processor<P: StatefulProcessor>(processor: P) -> Arc<dyn NodeProcessor> {
    Arc::new(StatefulAdapter(processor))
}

impl<P: StatefulProcessor> SimStepper for StatefulAdapter<P> {
    fn initial(&self, ctx: &EvalContext, inputs: &SimInputs<'_>) -> SimState {
        Arc::new(self.0.initial(ctx, inputs))
    }

    fn step(&self, prev: &SimState, ctx: &EvalContext, inputs: &SimInputs<'_>) -> SimState {
        match prev.downcast_ref::<P::State>() {
            Some(prev) => Arc::new(self.0.step(prev, ctx, inputs)),
            // A track made by another processor type cannot happen while
            // registration drops tracks; starting over is the safe answer
            // if it ever does.
            None => Arc::new(self.0.initial(ctx, inputs)),
        }
    }

    fn output(&self, state: &SimState) -> Option<Arc<dyn NodeData>> {
        state.downcast_ref::<P::State>().map(|s| self.0.output(s))
    }

    fn byte_size(&self, state: &SimState) -> u64 {
        state
            .downcast_ref::<P::State>()
            .map_or(0, |s| self.0.byte_size(s))
    }
}

impl<P: StatefulProcessor> NodeProcessor for StatefulAdapter<P> {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let state = scope.sim_state(node, ctx, self)?;
        self.output(&state)
            .ok_or_else(|| anyhow::anyhow!("simulation state does not belong to this processor"))
    }

    /// The output is the state of a frame, so it moves with the frame.
    fn is_time_dependent(&self) -> bool {
        true
    }

    /// The inputs are pulled once per simulated frame by the fill, not once
    /// at the requested frame by the evaluator.
    fn pulls_inputs_itself(&self) -> bool {
        true
    }
}

// ===========================================================================
// The cache
// ===========================================================================

/// A pull that answered with a stateful node's last held state instead of
/// the frame asked for, and what is needed to dirty it again.
pub(super) struct ProvisionalSim {
    graph: Graph,
    path: Vec<PathSegment>,
    node: NodeId,
}

/// What a track's states were made from: the digest the track is accepted
/// by, and the cheap evidence that the digest still describes the graph in
/// hand.
///
/// Hashing the upstream subgraph costs a walk over it and a serialisation of
/// every parameter, and a pull happens once per played frame. `graph` lets a
/// pull whose graph is the very object the digest was made from skip that
/// ([`Graph::ptr_eq`] is `O(1)`, and `true` proves identical content); a
/// `false` only costs the recomputation. A track whose upstream reads from
/// outside its scope also carries an [`ExternalStamp`], which holds the same
/// kind of evidence for what lives there.
#[derive(Clone)]
pub(super) struct Premise {
    graph: Graph,
    fps: FrameRate,
    comp_resolution: (u32, u32),
    hash: u64,
    /// `Evaluator::epoch_clock` when the hash was made: unchanged means no
    /// node was invalidated since, so the hash cannot have moved on that axis.
    clock: u64,
    external: Option<ExternalStamp>,
}

/// The objects an upstream that reaches outside its scope was hashed from: the
/// document its layers and shells were read from, and the graph each
/// binding-producing scope was hashed in. Every one is immutable, so pointer
/// equality of all of them proves the digest still describes what is there; any
/// difference only costs the recomputation.
#[derive(Clone)]
struct ExternalStamp {
    document: Option<Arc<Document>>,
    producers: Vec<Graph>,
}

/// What [`ExternalStamp`] is compared with: the same objects as they are now.
pub(super) struct LiveExternal {
    document: Option<Arc<Document>>,
    producers: Vec<Graph>,
}

impl ExternalStamp {
    fn holds(&self, live: &LiveExternal) -> bool {
        let same_document = match (&self.document, &live.document) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        // The stamp holds the levels the hash walked; deeper live levels were
        // never read, so they cannot have changed it.
        same_document
            && self.producers.len() <= live.producers.len()
            && self
                .producers
                .iter()
                .zip(&live.producers)
                .all(|(then, now)| then.ptr_eq(now))
    }
}

impl Premise {
    /// Whether `hash` was made from `graph`, the frame axes of `ctx` and — when
    /// the upstream reads outside its scope — the external objects `live`
    /// names (`None` when the caller did not look them up).
    fn describes(
        &self,
        graph: &Graph,
        ctx: &EvalContext,
        clock: u64,
        live: Option<&LiveExternal>,
    ) -> bool {
        self.clock == clock
            && self.graph.ptr_eq(graph)
            && self.fps == ctx.fps
            && self.comp_resolution == ctx.comp_resolution
            && match (&self.external, live) {
                (None, _) => true,
                (Some(stamp), Some(live)) => stamp.holds(live),
                (Some(_), None) => false,
            }
    }
}

/// A stored state and what it was charged: its own bytes **plus the
/// bookkeeping the track keeps for it** ([`ENTRY_OVERHEAD`]), so the budget
/// sees the memory a long track really holds, not just the states.
struct Held {
    state: SimState,
    bytes: u64,
}

/// What one stored entry costs beyond the state: the `(frame, Held)` slot and
/// the `Arc` handle. Charged per entry, so the overhead is proportional to
/// the states kept and thinning gives it back.
const ENTRY_OVERHEAD: u64 = std::mem::size_of::<(u64, Held)>() as u64;

/// One node's states over a contiguous run of frames, `start_frame` first.
///
/// # Checkpoints
///
/// The track **covers** `start_frame..end` but need not **store** every
/// frame. When it outgrows its share of the budget it thins itself to the
/// start frame, the latest frame, and every `stride`-th frame between, and
/// doubles `stride` until it fits (the start frame is always kept, and so is
/// the latest, because the next step needs it). A pull of a frame that is not
/// stored re-steps from the nearest earlier stored frame, so a rewind costs
/// fewer than `stride` steps. The rule is a function of the frame number and
/// the budget alone, so thinning never changes a state — only how far away the
/// nearest stored one is — and it is the rule a simulation held in VRAM would
/// use for its own tier.
struct SimTrack {
    start_frame: u64,
    /// The stored frames, ascending, as `(frame, state)` — **sparse**: its
    /// size is the retained states, not the frames covered. The first entry
    /// is `start_frame` and the last is `end - 1`.
    stored: Vec<(u64, Held)>,
    /// One past the latest frame covered.
    end: u64,
    /// Spacing of the stored frames between the start and the latest.
    stride: u64,
    /// What the states were made from (see [`Evaluator::sim_premise_hash`]).
    premise: Premise,
    bytes: u64,
    /// The budget claim for `bytes`, replaced as the track grows. Dropping
    /// the track releases it.
    reservation: Option<Reservation>,
}

/// What a track says about one frame.
pub(super) enum Lookup {
    /// The state is stored.
    Hit(SimState),
    /// Covered but thinned: the nearest stored state before it, and its frame.
    Thinned { from: u64, state: SimState },
    /// Not covered: the track has to grow (or does not exist).
    Beyond,
}

impl SimTrack {
    fn end_frame(&self) -> u64 {
        self.end
    }

    /// Whether `frame` is kept when only checkpoints are.
    fn is_checkpoint(&self, frame: u64) -> bool {
        frame == self.start_frame || frame.is_multiple_of(self.stride)
    }

    /// Drop the frame before the latest if it is not a checkpoint.
    fn thin_previous_latest(&mut self) {
        let Some(index) = self.stored.len().checked_sub(2) else {
            return;
        };
        if !self.is_checkpoint(self.stored[index].0) {
            self.bytes -= self.stored.remove(index).1.bytes;
        }
    }

    /// Double the stride, dropping non-checkpoints, until the stored bytes fit
    /// `cap` or nothing but the start and latest frames can be dropped.
    fn thin_to(&mut self, cap: u64) {
        while self.bytes > cap {
            let latest = self.end - 1 - self.start_frame;
            if self.stride > latest {
                return;
            }
            self.stride *= 2;
            let (start, stride, last) = (self.start_frame, self.stride, self.end - 1);
            let mut freed = 0;
            self.stored.retain(|(frame, held)| {
                let keep = *frame == start || *frame == last || frame.is_multiple_of(stride);
                if !keep {
                    freed += held.bytes;
                }
                keep
            });
            self.bytes -= freed;
        }
    }
}

/// Simulation tracks by [`NodeKey`], and the reservations that back them.
#[derive(Default)]
pub(super) struct SimCache {
    tracks: HashMap<NodeKey, SimTrack>,
    by_reservation: HashMap<ReservationId, NodeKey>,
}

impl SimCache {
    /// The frames `key`'s track holds, `None` without one.
    pub(super) fn frames(&self, key: &NodeKey) -> Option<Range<u64>> {
        let track = self.tracks.get(key)?;
        Some(track.start_frame..track.end_frame())
    }

    /// How many states `key`'s track stores (fewer than it covers once it has
    /// thinned).
    pub(super) fn stored(&self, key: &NodeKey) -> Option<usize> {
        let track = self.tracks.get(key)?;
        Some(track.stored.len())
    }

    #[cfg(test)]
    pub(super) fn stride(&self, key: &NodeKey) -> Option<u64> {
        Some(self.tracks.get(key)?.stride)
    }

    /// What the track can say about `frame`, marking the track as used.
    pub(super) fn lookup(
        &self,
        key: &NodeKey,
        frame: u64,
        budget: Option<&SharedCacheBudget>,
    ) -> Lookup {
        let Some(track) = self.tracks.get(key) else {
            return Lookup::Beyond;
        };
        if frame < track.start_frame || frame >= track.end {
            return Lookup::Beyond;
        }
        if let (Some(budget), Some(reservation)) = (budget, &track.reservation) {
            budget.touch(reservation.id());
        }
        match track.stored.binary_search_by_key(&frame, |(f, _)| *f) {
            Ok(i) => Lookup::Hit(track.stored[i].1.state.clone()),
            // The start frame is always stored, so `i >= 1`.
            Err(i) => Lookup::Thinned {
                from: track.stored[i - 1].0,
                state: track.stored[i - 1].1.state.clone(),
            },
        }
    }

    /// The last state the track holds and its frame.
    pub(super) fn last(&self, key: &NodeKey) -> Option<(u64, SimState)> {
        let track = self.tracks.get(key)?;
        let (frame, held) = track.stored.last()?;
        Some((*frame, held.state.clone()))
    }

    /// `key`'s track's premise when it was made from this very graph, these
    /// frame axes and these external objects, so the caller can skip
    /// recomputing it.
    pub(super) fn known(
        &self,
        key: &NodeKey,
        graph: &Graph,
        ctx: &EvalContext,
        clock: u64,
        live: Option<&LiveExternal>,
    ) -> Option<Premise> {
        let premise = &self.tracks.get(key)?.premise;
        premise
            .describes(graph, ctx, clock, live)
            .then(|| premise.clone())
    }

    /// Whether `key`'s track was made from something outside its scope, so a
    /// pull has to look at what is there now before trusting [`Self::known`].
    pub(super) fn has_external(&self, key: &NodeKey) -> bool {
        self.tracks
            .get(key)
            .is_some_and(|track| track.premise.external.is_some())
    }

    /// The acceptance check: drop the track unless it was made from
    /// `premise.hash`, and otherwise remember the graph it was just proven
    /// against. Returns whether it was dropped.
    pub(super) fn discard_if_stale(&mut self, key: &NodeKey, premise: &Premise) -> bool {
        match self.tracks.get_mut(key) {
            Some(track) if track.premise.hash != premise.hash => {
                self.remove(key);
                true
            }
            Some(track) => {
                track.premise = premise.clone();
                false
            }
            None => false,
        }
    }

    /// Append `state` (made from `premise`) to `key`'s track and re-claim
    /// the track's bytes. Returns the eviction entries that are not
    /// simulation tracks, for the caller to hand to the node-result cache.
    pub(super) fn push(
        &mut self,
        key: NodeKey,
        premise: &Premise,
        state: SimState,
        bytes: u64,
        budget: Option<&SharedCacheBudget>,
    ) -> Vec<Evicted> {
        let track = self.tracks.entry(key).or_insert_with(|| SimTrack {
            start_frame: SIM_START_FRAME,
            stored: Vec::new(),
            end: SIM_START_FRAME,
            stride: 1,
            premise: premise.clone(),
            bytes: 0,
            reservation: None,
        });
        let bytes = bytes + ENTRY_OVERHEAD;
        track.stored.push((track.end, Held { state, bytes }));
        track.end += 1;
        track.bytes += bytes;
        let Some(budget) = budget else {
            return Vec::new();
        };
        // The latest frame stays stored whatever its number; the one it
        // replaces as latest only stays if it is a checkpoint.
        track.thin_previous_latest();
        track.thin_to(budget.stats().sim_reserved[Tier::Ram as usize]);
        // The old claim goes first, so the budget never sees the track
        // competing with its own previous size and never evicts it.
        if let Some(old) = track.reservation.take() {
            self.by_reservation.remove(&old.id());
        }
        let (reservation, evicted) = budget.reserve(CacheKind::Sim, track.bytes);
        self.by_reservation.insert(reservation.id(), key);
        track.reservation = Some(reservation);
        self.drop_evicted(&evicted)
    }

    /// Drop the tracks `evicted` names, returning the entries that are not
    /// ours.
    pub(super) fn drop_evicted(&mut self, evicted: &[Evicted]) -> Vec<Evicted> {
        let mut rest = Vec::new();
        for entry in evicted {
            match self.by_reservation.get(&entry.id).copied() {
                Some(key) => self.remove(&key),
                None => rest.push(*entry),
            }
        }
        rest
    }

    /// Drop `key`'s track.
    pub(super) fn remove(&mut self, key: &NodeKey) {
        if let Some(track) = self.tracks.remove(key)
            && let Some(reservation) = &track.reservation
        {
            self.by_reservation.remove(&reservation.id());
        }
    }

    /// Drop every track of `node`, at every path.
    pub(super) fn remove_node(&mut self, node: NodeId) {
        self.retain(|key| key.node != node);
    }

    /// Keep only the tracks `keep` accepts.
    pub(super) fn retain(&mut self, keep: impl Fn(&NodeKey) -> bool) {
        let dropped: Vec<NodeKey> = self
            .tracks
            .keys()
            .filter(|key| !keep(key))
            .copied()
            .collect();
        for key in dropped {
            self.remove(&key);
        }
    }

    /// Drop every track.
    pub(super) fn clear(&mut self) {
        self.tracks.clear();
        self.by_reservation.clear();
    }

    pub(super) fn len(&self) -> usize {
        self.tracks.len()
    }
}

// ===========================================================================
// Filling a track
// ===========================================================================

impl Evaluator {
    /// The frames the simulation of `node` at scope `path` currently holds,
    /// or `None` when it has no track. Read-only, and not a use.
    pub fn sim_cached_frames(&self, path: &[PathSegment], node: NodeId) -> Option<Range<u64>> {
        let path = self.paths.id_of(&sim_scope(path))?;
        self.sim.frames(&NodeKey { path, node })
    }

    /// How many states the track stores — fewer than
    /// [`sim_cached_frames`](Self::sim_cached_frames) covers once it has
    /// thinned to checkpoints.
    pub fn sim_stored_states(&self, path: &[PathSegment], node: NodeId) -> Option<usize> {
        let path = self.paths.id_of(&sim_scope(path))?;
        self.sim.stored(&NodeKey { path, node })
    }

    /// How many nodes hold a simulation track.
    pub fn sim_track_count(&self) -> usize {
        self.sim.len()
    }

    /// The premise of `node`'s track: the hash of everything its states are
    /// made from, other than the frame, and the evidence it is still current.
    /// Two equal hashes mean a track made under one may answer a pull made
    /// under the other.
    ///
    /// Covers the **upstream subgraph** of `node` — itself, every node
    /// reachable through its wires and through `NodeOutput` parameter
    /// bindings — by structure (wiring, node type, bypass flag, port layout)
    /// and by every parameter value, keyframes and expressions included, plus
    /// the frame rate and composition basis the frames are counted in. What
    /// it leaves out is what does not change a state: a node's position,
    /// label, colour and stacking order, and every node that is not upstream.
    ///
    /// **And what that upstream reads from outside its scope**, which is not
    /// in the subgraph at all:
    ///
    /// - a `net.in` reached upstream means the scope's bindings feed the
    ///   states, so the producing scope's node is hashed the same way — its
    ///   own upstream, level by level for as long as that reaches a `net.in`
    ///   of its own (a subnet pin, an adjustment layer's `source`, an
    ///   iteration piece);
    /// - a `layer.ref` reads another layer's network and shell, and a
    ///   `layer.info` / `comp.info` or a compiled shell node reads shells off
    ///   the document. Those are hashed from the document at the granularity
    ///   the reads have (a referenced layer's whole network, closed over its
    ///   own references; a composition's metadata and every layer's shell).
    ///
    /// A sim that reaches none of that hashes exactly what it did before, so
    /// an edit elsewhere in its own network still does not drop its track.
    ///
    /// It does not see inside a processor (a media file replaced on disk, a
    /// registered processor swapped): those reach the track through
    /// [`Evaluator::invalidate_node`] and [`Evaluator::invalidate_all`].
    fn sim_premise(&mut self, graph: &Graph, node: &Node, ctx: &EvalContext) -> Premise {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        ctx.fps.hash(&mut hasher);
        ctx.comp_resolution.hash(&mut hasher);

        let mut reads = ExternalReads::default();
        let scope = UpstreamScope {
            path_id: self.path_id,
            owner: owning_layer(&self.path),
            producer: false,
        };
        let mut reached_interface =
            self.hash_upstream(graph, node.id, scope, &mut hasher, &mut reads);

        // Walk out through the producing scopes while the walk keeps reaching
        // their interface node.
        let mut producers = Vec::new();
        let mut level = self.bindings_stack.last().and_then(|b| b.source.clone());
        while reached_interface
            && let Some(source) = level.take()
            && let Some(anchor) = source.anchor
        {
            let (Some(producer), Some((key, _))) = (
                self.processing_graphs
                    .get(anchor.processing_len - 1)
                    .cloned(),
                self.processing.get(anchor.processing_len - 1).copied(),
            ) else {
                break;
            };
            producers.push(producer.clone());
            let scope = UpstreamScope {
                path_id: anchor.path_id,
                owner: owning_layer(&self.path[..anchor.path_len.min(self.path.len())]),
                producer: true,
            };
            reached_interface =
                self.hash_upstream(&producer, key.node, scope, &mut hasher, &mut reads);
            level = self
                .bindings_stack
                .get(anchor.bindings_len - 1)
                .and_then(|b| b.source.clone());
        }

        let external = (reads.any() || !producers.is_empty()).then(|| {
            let document = self.document.clone();
            if let Some(document) = &document {
                reads.hash_from(document, &mut hasher);
            }
            ExternalStamp {
                document,
                producers,
            }
        });
        Premise {
            graph: graph.clone(),
            fps: ctx.fps,
            comp_resolution: ctx.comp_resolution,
            hash: hasher.finish(),
            clock: self.epoch_clock,
            external,
        }
    }

    /// The objects a stamp is compared with, as they are now: the document and
    /// the graph of every scope that produced bindings for the current one.
    fn live_external(&self) -> LiveExternal {
        let mut producers = Vec::new();
        let mut level = self.bindings_stack.last().and_then(|b| b.source.as_ref());
        while let Some(anchor) = level.and_then(|source| source.anchor) {
            let Some(producer) = self.processing_graphs.get(anchor.processing_len - 1) else {
                break;
            };
            producers.push(producer.clone());
            level = self
                .bindings_stack
                .get(anchor.bindings_len - 1)
                .and_then(|b| b.source.as_ref());
        }
        LiveExternal {
            document: self.document.clone(),
            producers,
        }
    }

    /// Hash when `node` — and every node of a subnet it holds — was last
    /// invalidated or re-registered (see `Evaluator::node_epochs`).
    ///
    /// Only nodes that have an epoch contribute, so an added node nobody has
    /// invalidated leaves the digest alone.
    fn hash_epochs(&self, node: &Node, nested: bool, hasher: &mut impl Hasher) {
        if let Some(epoch) = self.node_epochs.get(&node.id) {
            (node.id, epoch).hash(hasher);
        }
        if let (true, Some(subnet)) = (nested, &node.subnet) {
            for inner in subnet.nodes() {
                self.hash_epochs(inner, true, hasher);
            }
        }
    }

    /// Hash `root` and everything upstream of it in `graph`, collecting what
    /// it reads outside the graph into `reads`. Returns whether a `net.in` was
    /// reached, i.e. whether the scope's bindings feed this subgraph.
    ///
    /// For a producing scope's node (`scope.producer`) only its header and its
    /// upstream are hashed, not its inner network: the bindings are made from
    /// what it was given, and its inner network is the sim's own scope.
    fn hash_upstream(
        &mut self,
        graph: &Graph,
        root: NodeId,
        scope: UpstreamScope,
        hasher: &mut impl Hasher,
        reads: &mut ExternalReads,
    ) -> bool {
        let index = self.graph_index(graph, scope.path_id);
        let mut upstream: BTreeSet<NodeId> = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !upstream.insert(id) {
                continue;
            }
            pending.extend(index.in_edges(id).iter().map(|(_, source, _)| *source));
            if let Some(reached) = graph.node(id) {
                pending.extend(reached.parameter_sources().into_iter().map(|(n, _)| n));
            }
        }
        let mut reached_interface = false;
        for id in upstream {
            id.hash(hasher);
            let Some(reached) = graph.node(id) else {
                // A dangling reference still changes the digest.
                continue;
            };
            // A producer's inner network is the sim's own scope, hashed there.
            self.hash_epochs(reached, !(scope.producer && id == root), hasher);
            if scope.producer && id == root {
                hash_node_header(reached, hasher);
                // What its rebind reads off the document: the layer's own
                // shell (timing, remap), not the network the sim sits in.
                if reached.type_key.starts_with("comp.")
                    && let Some((comp, layer, _)) = decode_deterministic_node_id(reached.id)
                {
                    reads.layer_shells.insert((comp, layer));
                }
            } else {
                hash_node(reached, hasher);
                scan_reads(reached, scope.owner, reads, true);
                reached_interface |= reads.take_interface();
            }
            let mut wires: Vec<_> = index.in_edges(id).iter().collect();
            wires.sort_by_key(|(port, source, source_port)| (port.0, source.raw(), source_port.0));
            for (port, source, source_port) in wires {
                (port.0, source.raw(), source_port.0).hash(hasher);
            }
        }
        reached_interface
    }

    /// Release the tracks the document edit left without an owner.
    ///
    /// `prefix` is a layer scope whose network changed; `network` is the new
    /// network, or `None` when the layer is gone. A track whose node is still
    /// in the network (a subnet's inside included) is **kept** — whether its
    /// states are still right is the premise hash's call at the next pull,
    /// and an edit to an unrelated node in the same network must not cost the
    /// simulation its frames. A track whose node was deleted is never pulled
    /// again, so only this releases the bytes it holds in the protected share.
    pub(super) fn sim_prune_scope(&mut self, prefix: &[PathSegment], network: Option<&Graph>) {
        let under = self.paths.ids_under(prefix);
        let mut alive = HashSet::new();
        if let Some(network) = network {
            collect_node_ids(network, &mut alive);
        }
        self.sim
            .retain(|key| !under.contains(&key.path) || alive.contains(&key.node));
    }

    /// Drop the tracks keyed under the scope paths `under`, for the scopes
    /// the evaluator retires (shift and iteration scopes). Not for a network
    /// edit — see [`Self::sim_prune_scope`] — but required before a path id is
    /// released, because [`PathInterner::release`] lets the id name another
    /// path.
    pub(super) fn drop_sim_under(&mut self, under: &HashSet<PathId>) {
        self.sim.retain(|key| !under.contains(&key.path));
    }

    /// [`Self::sim_prune_scope`] for the layer `prefix` names in `document`.
    pub(super) fn sim_prune_layer(&mut self, prefix: &[PathSegment], document: &Document) {
        let network = match prefix {
            [PathSegment::Layer(comp, layer)] => document
                .get_composition(*comp)
                .and_then(|comp| comp.get_layer(*layer))
                .map(|layer| &layer.network),
            _ => None,
        };
        self.sim_prune_scope(prefix, network);
    }

    /// Whether a fill should stop here: a newer request is waiting, or a
    /// read-ahead has been told to give way.
    fn sim_fill_should_stop(&self) -> bool {
        [&self.read_ahead, &self.sim_cancel]
            .into_iter()
            .flatten()
            .any(|cancelled| cancelled())
    }

    /// Answer a forward jump of more than `after` frames with the last state
    /// the track holds instead of filling the gap, or stop doing so
    /// (`None`, the default: every pull fills to the frame asked for).
    ///
    /// A pull answered this way is **provisional**: it shows a frame the
    /// simulation has not reached. [`Self::has_provisional_sim`] reports it,
    /// and [`Self::settle_provisional`] must run before the next pull of the
    /// same request, or the provisional value stays in the node cache.
    pub fn set_sim_provisional(&mut self, after: Option<u64>) {
        self.sim_provisional_after = after;
    }

    /// Ask a fill to stop between frames when `cancelled` returns `true`
    /// (a newer request is waiting), or stop asking. The frames already
    /// computed stay in the track; the pull fails with
    /// [`EvalError::Cancelled`].
    pub fn set_sim_cancel(&mut self, cancelled: Option<CancelCheck>) {
        self.sim_cancel = cancelled;
    }

    /// Whether a pull since the last [`Self::settle_provisional`] was
    /// answered with a state older than the frame asked for.
    pub fn has_provisional_sim(&self) -> bool {
        !self.sim_provisional.is_empty()
    }

    /// Take back what a provisional pull put in the node cache.
    ///
    /// The nodes that answered provisionally and everything downstream of
    /// them are marked dirty, as an edit to them would, so the pull that
    /// completes the fill recomputes them from the real state instead of
    /// serving the old frame's. The track itself is untouched.
    pub fn settle_provisional(&mut self) {
        for hit in std::mem::take(&mut self.sim_provisional) {
            self.mark_dirty_at(&hit.graph, &hit.path, hit.node);
        }
    }

    /// [`EvalScope::sim_state`]: the state of `node` at `ctx.frame`.
    pub(super) fn sim_state_at(
        &mut self,
        node: &Node,
        ctx: &EvalContext,
        stepper: &dyn SimStepper,
    ) -> Result<SimState, EvalError> {
        let Some(graph) = self.processing_graphs.last().cloned() else {
            return Err(EvalError::ProcessFailed {
                node: node.id,
                source: anyhow::anyhow!("sim_state called outside process"),
            });
        };
        if self.sim_active {
            return Err(EvalError::ProcessFailed {
                node: node.id,
                source: anyhow::anyhow!(
                    "a stateful node cannot sit downstream of another stateful node"
                ),
            });
        }
        // Keyed by the scope *without* its time shifts: a state is a function
        // of the node, the frame and the upstream premise, none of which a
        // shifted view changes, so every shifted view reads one track.
        let scope = sim_scope(&self.path);
        let key = NodeKey {
            path: self.paths.intern(&scope),
            node: node.id,
        };
        // The comparison happens here, at the pull: a track that read from
        // outside its scope is only trusted without rehashing when the
        // document and the producing scopes are the very objects it was made
        // from (see [`ExternalStamp`]).
        let live = self.sim.has_external(&key).then(|| self.live_external());
        let premise = match self
            .sim
            .known(&key, &graph, ctx, self.epoch_clock, live.as_ref())
        {
            Some(premise) => premise,
            None => self.sim_premise(&graph, node, ctx),
        };
        self.sim.discard_if_stale(&key, &premise);
        let budget = self.store.budget().cloned();
        match self.sim.lookup(&key, ctx.frame, budget.as_ref()) {
            Lookup::Hit(state) => return Ok(state),
            Lookup::Thinned { from, state } => {
                self.sim_active = true;
                let replayed = self.replay_sim_frames(&graph, node, ctx, stepper, from, state);
                self.sim_active = false;
                return replayed;
            }
            Lookup::Beyond => {}
        }

        self.sim_active = true;
        let state = self.sim_state_or_provisional(&graph, node, ctx, stepper, key, &premise);
        self.sim_active = false;
        state
    }

    /// The state at `ctx.frame` — or, when a long forward jump would make the
    /// caller wait, the last state the track holds, recorded as provisional.
    ///
    /// The start frame is always filled first: a track with nothing in it has
    /// no "last state" to show.
    fn sim_state_or_provisional(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        stepper: &dyn SimStepper,
        key: NodeKey,
        premise: &Premise,
    ) -> Result<SimState, EvalError> {
        let Some(after) = self.sim_provisional_after else {
            return self.fill_sim_track(graph, node, ctx, stepper, key, premise, ctx.frame);
        };
        let (last, shown) = match self.sim.last(&key) {
            Some(held) => held,
            None => {
                let start =
                    self.fill_sim_track(graph, node, ctx, stepper, key, premise, SIM_START_FRAME)?;
                (SIM_START_FRAME, start)
            }
        };
        if ctx.frame.saturating_sub(last) <= after {
            return self.fill_sim_track(graph, node, ctx, stepper, key, premise, ctx.frame);
        }
        self.sim_provisional.push(ProvisionalSim {
            graph: graph.clone(),
            path: self.path.clone(),
            node: node.id,
        });
        Ok(shown)
    }

    /// The state of `ctx.frame`, a frame the track covers but thinned away:
    /// step from the stored state `from` (fewer than the track's stride
    /// frames back) without storing anything.
    fn replay_sim_frames(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        stepper: &dyn SimStepper,
        from: u64,
        mut state: SimState,
    ) -> Result<SimState, EvalError> {
        for frame in from + 1..=ctx.frame {
            if self.sim_fill_should_stop() {
                return Err(EvalError::Cancelled(node.id));
            }
            let at = ctx.at_frame(frame);
            let (inputs, params) = self.sim_inputs_at(graph, node, &at)?;
            let step_inputs = SimInputs {
                node,
                inputs: &inputs,
                params: &params,
            };
            state = stepper.step(&state, &at, &step_inputs);
        }
        Ok(state)
    }

    /// Step the track of `key` forward until it holds `target`.
    ///
    /// Every state is stored as soon as it exists, so a fill that fails or
    /// gives way leaves the frames it did compute for the next pull.
    #[allow(clippy::too_many_arguments)]
    fn fill_sim_track(
        &mut self,
        graph: &Graph,
        node: &Node,
        ctx: &EvalContext,
        stepper: &dyn SimStepper,
        key: NodeKey,
        premise: &Premise,
        target: u64,
    ) -> Result<SimState, EvalError> {
        let budget = self.store.budget().cloned();
        let (mut frame, mut prev) = match self.sim.last(&key) {
            Some((last, state)) => (last + 1, Some(state)),
            None => (SIM_START_FRAME, None),
        };
        // The track may already hold `target` (the provisional path fills the
        // start frame first, then asks for it again).
        if frame > target
            && let Some(held) = &prev
        {
            return Ok(held.clone());
        }
        loop {
            if self.sim_fill_should_stop() {
                return Err(EvalError::Cancelled(node.id));
            }
            let at = ctx.at_frame(frame);
            let (inputs, params) = self.sim_inputs_at(graph, node, &at)?;
            let step_inputs = SimInputs {
                node,
                inputs: &inputs,
                params: &params,
            };
            let state = match &prev {
                Some(prev) => stepper.step(prev, &at, &step_inputs),
                None => stepper.initial(&at, &step_inputs),
            };
            let bytes = stepper.byte_size(&state);
            let rest = self
                .sim
                .push(key, premise, state.clone(), bytes, budget.as_ref());
            self.store.drop_evicted(&rest);
            if frame >= target {
                return Ok(state);
            }
            prev = Some(state);
            frame += 1;
        }
    }

    /// The inputs and parameters of `node` as they are at `at.frame`.
    ///
    /// Inputs are pulled as the scope `TimeShift(node, frame)`; parameters are
    /// resolved here with the same rules the evaluator applies at the
    /// requested frame (parameter ports, identifier parameters, channels).
    pub(super) fn sim_inputs_at(
        &mut self,
        graph: &Graph,
        node: &Node,
        at: &EvalContext,
    ) -> Result<StepArgs, EvalError> {
        let mut inputs = Vec::with_capacity(node.inputs.len());
        for port in 0..node.inputs.len() {
            inputs.push(self.pull_input_at(node, port, at.frame, at, false)?);
        }
        let overlays = parameter_overlays(node, &mut inputs);
        let overridden =
            |key: &str| -> bool { overlays.iter().any(|(overlaid, _)| overlaid == key) };
        let depth = self.processing.last().map_or(0, |(_, depth)| depth + 1);
        let options = ResolveOptions {
            skip: &overridden,
            budget: ResolveBudget {
                owner: node.id,
                depth,
            },
        };
        let mut run = HashMap::new();
        let mut visiting = HashSet::new();
        let (channels, _) =
            self.resolve_channel_params(graph, node, at, &mut run, &mut visiting, options)?;
        let mut params = self.materialize_params(node, channels, &overridden, at);
        for (key, value) in overlays {
            params.set(&key, value);
        }
        Ok((inputs, params))
    }
}

/// The scope a simulation track is keyed by: `path` with every `TimeShift`
/// removed. `Layer`, `Subnet`, `Comp` and `Iteration` stay — those change what
/// the node's upstream *is* (another network, another piece), a shift only
/// changes which frame of it is looked at.
fn sim_scope(path: &[PathSegment]) -> Vec<PathSegment> {
    path.iter()
        .copied()
        .filter(|segment| !matches!(segment, PathSegment::TimeShift(..)))
        .collect()
}

/// Where [`Evaluator::hash_upstream`] is walking.
#[derive(Clone, Copy)]
struct UpstreamScope {
    /// The interned path of the scope the graph is evaluated in (it keys the
    /// graph's wire index).
    path_id: PathId,
    /// The layer the scope belongs to and its composition, for readers that
    /// name "the same composition" or read the layer's own placement.
    owner: Option<(CompId, LayerId)>,
    /// The graph holds the node that produced the sim's bindings, not the sim.
    producer: bool,
}

/// The composition and layer of the innermost layer scope in `path`.
fn owning_layer(path: &[PathSegment]) -> Option<(CompId, LayerId)> {
    path.iter().rev().find_map(|segment| match segment {
        PathSegment::Layer(comp, layer) => Some((*comp, *layer)),
        _ => None,
    })
}

/// What an upstream reads from the document: which compositions' shells and
/// which layers' networks. Collected while walking, hashed once from the
/// document ([`Self::hash_from`]), so the walk never needs the document.
#[derive(Default)]
struct ExternalReads {
    /// Compositions whose metadata and every layer's shell are read.
    shells: BTreeSet<CompId>,
    /// Compositions whose every layer is read, network and shell (a
    /// `layer.ref` whose target cannot be named statically).
    whole: BTreeSet<CompId>,
    /// Layers whose network (closed over its own references) and shell are
    /// read.
    layers: BTreeSet<(CompId, LayerId)>,
    /// Compositions whose own fields and layer count are read, not the layers.
    comp_meta: BTreeSet<CompId>,
    /// Layers whose shell alone is read.
    layer_shells: BTreeSet<(CompId, LayerId)>,
    /// A `net.in` was just scanned (see [`Self::take_interface`]).
    interface: bool,
}

impl ExternalReads {
    fn any(&self) -> bool {
        !(self.shells.is_empty()
            && self.whole.is_empty()
            && self.layers.is_empty()
            && self.comp_meta.is_empty()
            && self.layer_shells.is_empty())
    }

    /// Whether a `net.in` was scanned since the last call.
    fn take_interface(&mut self) -> bool {
        std::mem::take(&mut self.interface)
    }

    /// Hash everything read, from `document`, in id order.
    fn hash_from(&mut self, document: &Document, hasher: &mut impl Hasher) {
        for comp in std::mem::take(&mut self.whole) {
            if let Some(comp) = document.get_composition(comp) {
                self.shells.insert(comp.id);
                self.layers
                    .extend(comp.layers.iter().map(|layer| (comp.id, layer.id)));
            }
        }
        // A referenced layer's network may read further out; its reads join
        // the set until nothing new appears.
        let mut scanned: BTreeSet<(CompId, LayerId)> = BTreeSet::new();
        while let Some(&next) = self.layers.iter().find(|layer| !scanned.contains(layer)) {
            scanned.insert(next);
            let layer = document
                .get_composition(next.0)
                .and_then(|comp| comp.get_layer(next.1));
            if let Some(layer) = layer {
                for node in layer.network.nodes() {
                    scan_reads(node, Some(next), self, false);
                }
            }
        }
        self.interface = false;
        for id in &self.shells {
            id.hash(hasher);
            match document.get_composition(*id) {
                Some(comp) => hash_comp_shell(comp, hasher),
                None => 0u8.hash(hasher),
            }
        }
        for id in &self.comp_meta {
            id.hash(hasher);
            match document.get_composition(*id) {
                Some(comp) => {
                    hash_comp_fields(comp, hasher);
                    comp.layers.len().hash(hasher);
                }
                None => 0u8.hash(hasher),
            }
        }
        for (comp, layer_id) in &self.layer_shells {
            (comp, layer_id).hash(hasher);
            match document
                .get_composition(*comp)
                .and_then(|comp| comp.get_layer(*layer_id))
            {
                Some(layer) => hash_layer_shell(layer, hasher),
                None => 0u8.hash(hasher),
            }
        }
        for (comp, layer_id) in &self.layers {
            (comp, layer_id).hash(hasher);
            match document
                .get_composition(*comp)
                .and_then(|comp| comp.get_layer(*layer_id))
            {
                Some(layer) => {
                    hash_graph(&layer.network, hasher);
                    hash_layer_shell(layer, hasher);
                }
                None => 0u8.hash(hasher),
            }
        }
    }
}

/// Record what `node` — and every node of a subnet it holds — reads from
/// outside the graph it sits in. `owner` is the layer the graph belongs to. `top` is false for a nested node, whose `net.in` is its
/// subnet's own interface and not the scope's.
fn scan_reads(node: &Node, owner: Option<(CompId, LayerId)>, reads: &mut ExternalReads, top: bool) {
    let identifier = |key: &str| {
        node.parameters
            .iter()
            .find(|param| param.key == key)
            .and_then(|param| param.value.static_text_identifier())
    };
    match node.type_key.as_str() {
        LAYER_REF_TYPE_KEY => {
            // The referrer's shell too: the target's time is derived from it.
            if let Some((owner, _)) = owner {
                reads.shells.insert(owner);
                match identifier("layer") {
                    Some(raw) => {
                        reads.layers.insert((owner, LayerId::new(raw)));
                    }
                    None => {
                        reads.whole.insert(owner);
                    }
                }
            }
        }
        LAYER_INFO_TYPE_KEY => {
            reads.shells.extend(owner.map(|(comp, _)| comp));
        }
        // Composition fields and the layer count of the target, plus the
        // reading layer's own placement (its time is converted to comp time).
        COMP_INFO_TYPE_KEY => {
            if let Some((comp, layer)) = owner {
                reads.layer_shells.insert((comp, layer));
                reads.comp_meta.insert(comp);
            }
            reads
                .comp_meta
                .extend(identifier(COMP_INFO_COMP_PARAM).map(CompId::new));
        }
        key => {
            // A compiled shell node reads its layer's shell (and the network
            // boundary its network) off the document by decoding its id.
            if key.starts_with("comp.")
                && let Some((comp, layer, role)) = decode_deterministic_node_id(node.id)
            {
                reads.shells.insert(comp);
                if role == NodeRole::Network {
                    reads.layers.insert((comp, layer));
                }
            }
        }
    }
    if top && network::is_in_node(node) {
        reads.interface = true;
    }
    if let Some(subnet) = &node.subnet {
        for inner in subnet.nodes() {
            scan_reads(inner, owner, reads, false);
        }
    }
}

/// A composition's own fields and every layer's shell, but no network.
fn hash_comp_shell(comp: &Composition, hasher: &mut impl Hasher) {
    hash_comp_fields(comp, hasher);
    for layer in &comp.layers {
        hash_layer_shell(layer, hasher);
    }
}

/// A composition's own fields, with no layer in them.
fn hash_comp_fields(comp: &Composition, hasher: &mut impl Hasher) {
    comp.resolution.hash(hasher);
    comp.frame_rate.hash(hasher);
    comp.duration_frames.hash(hasher);
    match bincode::serialize(&comp.background_color) {
        Ok(bytes) => bytes.hash(hasher),
        Err(_) => unique().hash(hasher),
    }
}

/// A layer's shell — timing, transform, opacity, blend, parenting, remap — and
/// not the network it owns.
fn hash_layer_shell(layer: &Layer, hasher: &mut impl Hasher) {
    let mut shell = layer.clone();
    shell.network = Graph::new();
    match bincode::serialize(&shell) {
        Ok(bytes) => bytes.hash(hasher),
        Err(_) => unique().hash(hasher),
    }
}

/// A value that equals no other, for a serialisation that cannot fail on plain
/// data: the safe answer is a digest that never matches, so the track is
/// dropped rather than reused.
fn unique() -> u64 {
    static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Everything about `node` that decides what it computes — and nothing about
/// where the editor drew it.
fn hash_node(node: &Node, hasher: &mut impl Hasher) {
    hash_node_header(node, hasher);
    if let Some(subnet) = &node.subnet {
        hash_graph(subnet, hasher);
    }
}

/// [`hash_node`] without the nested network.
fn hash_node_header(node: &Node, hasher: &mut impl Hasher) {
    node.type_key.hash(hasher);
    node.metadata.bypassed.hash(hasher);
    for port in &node.inputs {
        (&port.name, port.is_param, port.is_variadic).hash(hasher);
    }
    node.outputs.len().hash(hasher);
    // `Parameter` is not `Hash` (it holds floats). Its serde form is a
    // structural walk of the whole value — keyframes, tangents, expression
    // sources, curves and ramps — with every float as its raw bits, and the
    // journal already relies on it being stable and positional.
    match bincode::serialize(&node.parameters) {
        Ok(bytes) => bytes.hash(hasher),
        // Unreachable for plain data; if it ever happens, a value that equals
        // nothing is the safe answer (the track is dropped, not reused).
        Err(_) => {
            static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            UNIQUE
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .hash(hasher);
        }
    }
}

/// A whole graph's content, in id order so it does not depend on the maps'
/// iteration order.
fn hash_graph(graph: &Graph, hasher: &mut impl Hasher) {
    let mut nodes: Vec<_> = graph.nodes().collect();
    nodes.sort_by_key(|node| node.id);
    for node in nodes {
        node.id.hash(hasher);
        hash_node(node, hasher);
    }
    let mut edges: Vec<_> = graph
        .edges()
        .map(|e| {
            (
                e.source.raw(),
                e.source_port.0,
                e.target.raw(),
                e.target_port.0,
            )
        })
        .collect();
    edges.sort_unstable();
    edges.hash(hasher);
}

/// Every node id in `graph`, subnets included.
fn collect_node_ids(graph: &Graph, out: &mut HashSet<NodeId>) {
    for node in graph.nodes() {
        out.insert(node.id);
        if let Some(subnet) = &node.subnet {
            collect_node_ids(subnet, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_budget::{CacheBudgetConfig, Tier};
    use crate::eval::{Bindings, NodeProcessor};
    use crate::graph::ParameterValue;
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use crate::types::{FrameRate, Scalar};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx_at(frame: u64) -> EvalContext {
        EvalContext::new(frame, FPS, (64, 64))
    }

    fn value_of(data: &Arc<dyn NodeData>) -> f32 {
        data.downcast_ref::<Scalar>().expect("a scalar").0
    }

    /// A scalar that is the frame number times its `k` parameter, so a
    /// simulation reading it shows *which frames its inputs were pulled at*.
    struct FrameSource;

    impl NodeProcessor for FrameSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Scalar(ctx.frame as f32 * params.f32_or("k", 1.0))))
        }

        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// `state(0) = start + input(0)`, `state(t) = state(t-1) + 1 + input(t)`
    /// where `start` and `input` are read **at frame t**. Counts both calls.
    #[derive(Clone, Default)]
    struct Counter {
        initials: Arc<AtomicUsize>,
        steps: Arc<AtomicUsize>,
        state_bytes: u64,
    }

    fn input_scalar(inputs: &SimInputs<'_>) -> f32 {
        inputs
            .inputs
            .first()
            .and_then(|slot| slot.as_ref())
            .and_then(|data| data.downcast_ref::<Scalar>())
            .map_or(0.0, |scalar| scalar.0)
    }

    impl StatefulProcessor for Counter {
        type State = f32;

        fn initial(&self, _ctx: &EvalContext, inputs: &SimInputs<'_>) -> f32 {
            self.initials.fetch_add(1, Ordering::Relaxed);
            inputs.params.f32_or("start", 0.0) + input_scalar(inputs)
        }

        fn step(&self, prev: &f32, _ctx: &EvalContext, inputs: &SimInputs<'_>) -> f32 {
            self.steps.fetch_add(1, Ordering::Relaxed);
            prev + 1.0 + input_scalar(inputs)
        }

        fn output(&self, state: &f32) -> Arc<dyn NodeData> {
            Arc::new(Scalar(*state))
        }

        fn byte_size(&self, _state: &f32) -> u64 {
            self.state_bytes.max(1)
        }
    }

    /// Mixes the frame and a `seed` parameter into the state: the shape a
    /// particle emitter's randomness has (a hash of seed and id, no clock).
    struct Seeded;

    impl StatefulProcessor for Seeded {
        type State = u64;

        fn initial(&self, _ctx: &EvalContext, inputs: &SimInputs<'_>) -> u64 {
            inputs.params.i32_or("seed", 0) as u64
        }

        fn step(&self, prev: &u64, ctx: &EvalContext, inputs: &SimInputs<'_>) -> u64 {
            let seed = inputs.params.i32_or("seed", 0) as u64;
            (prev ^ seed)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(ctx.frame)
        }

        fn output(&self, state: &u64) -> Arc<dyn NodeData> {
            Arc::new(Scalar((*state % 100_000) as f32))
        }
    }

    fn sim_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "test.sim")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR)
            .with_param("start", ParameterValue::Float(0.0))
            .with_param("seed", ParameterValue::Int(7))
    }

    fn source_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "test.frame")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("k", ParameterValue::Float(1.0))
    }

    fn wire(graph: Graph, edge: u64, from: u64, to: u64) -> Graph {
        graph
            .add_edge(
                EdgeId::new(edge),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// One sim node, nothing wired.
    fn lone() -> (Graph, NodeId) {
        (Graph::new().add_node(sim_node(2)).unwrap(), NodeId::new(2))
    }

    fn counter_evaluator(counter: &Counter, node: u64) -> Evaluator {
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(node), stateful_processor(counter.clone()));
        ev
    }

    #[test]
    fn a_pull_at_frame_10_steps_ten_times() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);

        let out = ev.evaluate(&graph, node, &ctx_at(10)).unwrap();

        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 10);
        assert_eq!(value_of(&out), 10.0);
        assert_eq!(ev.sim_cached_frames(&[], node), Some(0..11));
    }

    #[test]
    fn scrubbing_back_and_forth_over_a_filled_track_steps_nothing() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);

        ev.evaluate(&graph, node, &ctx_at(10)).unwrap();
        let back = ev.evaluate(&graph, node, &ctx_at(5)).unwrap();
        let again = ev.evaluate(&graph, node, &ctx_at(10)).unwrap();

        assert_eq!(value_of(&back), 5.0, "frame 5 reads its own state");
        assert_eq!(value_of(&again), 10.0);
        assert_eq!(
            counter.steps.load(Ordering::Relaxed),
            10,
            "revisiting frames inside the track must not step"
        );
        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn advancing_continues_from_the_last_cached_frame() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);

        ev.evaluate(&graph, node, &ctx_at(4)).unwrap();
        ev.evaluate(&graph, node, &ctx_at(9)).unwrap();

        assert_eq!(counter.steps.load(Ordering::Relaxed), 9);
        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_same_seed_and_inputs_give_the_same_states() {
        let (graph, node) = lone();
        let run = |frames: &[u64]| -> Vec<f32> {
            let mut ev = Evaluator::new();
            ev.register(node, stateful_processor(Seeded));
            frames
                .iter()
                .map(|f| value_of(&ev.evaluate(&graph, node, &ctx_at(*f)).unwrap()))
                .collect()
        };

        let first = run(&[40]);
        let second = run(&[40]);
        // The same frame reached by a different route: one frame at a time,
        // and by scrubbing back over it.
        let stepwise = run(&(0..=40).collect::<Vec<_>>());
        let scrubbed = run(&[40, 3, 40]);

        assert_eq!(first, second);
        assert_eq!(first[0], *stepwise.last().unwrap());
        assert_eq!(first[0], scrubbed[0]);
        assert_eq!(first[0], scrubbed[2]);
        assert_ne!(
            first[0], stepwise[0],
            "frame 0 and frame 40 must differ or the test proves nothing"
        );
    }

    #[test]
    fn a_state_is_made_from_the_inputs_of_its_own_frame() {
        // input(t) = t, so state(3) = 0 + (1 + 1) + (1 + 2) + (1 + 3) = 9
        // however the frames were reached.
        let graph = wire(
            Graph::new()
                .add_node(source_node(1))
                .unwrap()
                .add_node(sim_node(2))
                .unwrap(),
            1,
            1,
            2,
        );
        let node = NodeId::new(2);
        let make = || {
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(1), Arc::new(FrameSource));
            ev.register(node, stateful_processor(Counter::default()));
            ev
        };

        let mut jump = make();
        let jumped = jump.evaluate(&graph, node, &ctx_at(3)).unwrap();
        let mut walk = make();
        let mut walked = None;
        for frame in 0..=3 {
            walked = Some(walk.evaluate(&graph, node, &ctx_at(frame)).unwrap());
        }

        assert_eq!(value_of(&jumped), 9.0);
        assert_eq!(value_of(&walked.unwrap()), 9.0);
    }

    #[test]
    fn parameters_are_read_at_the_frame_being_stepped() {
        // `start` is only read by `initial`, at frame 0, where it is 2.0 —
        // not at the requested frame, where it would be 5.0.
        let mut keyed = crate::animation::curve::KeyframeCurve::new();
        keyed.insert(
            0,
            2.0,
            crate::animation::interpolation::Interpolation::Linear,
        );
        keyed.insert(
            4,
            6.0,
            crate::animation::interpolation::Interpolation::Linear,
        );
        let mut node = sim_node(2);
        node.parameters
            .iter_mut()
            .find(|p| p.key == "start")
            .unwrap()
            .value = ParameterValue::Channel(
            crate::animation::channel::AnimationChannel::keyframes(keyed),
        );
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), stateful_processor(Counter::default()));

        let out = ev.evaluate(&graph, NodeId::new(2), &ctx_at(4)).unwrap();

        assert_eq!(value_of(&out), 2.0 + 4.0);
    }

    #[test]
    fn a_stateful_node_downstream_of_another_is_an_error() {
        let graph = wire(
            Graph::new()
                .add_node(sim_node(1))
                .unwrap()
                .add_node(sim_node(2))
                .unwrap(),
            1,
            1,
            2,
        );
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), stateful_processor(Counter::default()));
        ev.register(NodeId::new(2), stateful_processor(Counter::default()));

        let err = ev
            .evaluate(&graph, NodeId::new(2), &ctx_at(3))
            .err()
            .expect("a simulation behind a simulation must not evaluate");
        assert!(
            format!("{err:#}{:?}", std::error::Error::source(&err)).contains("downstream"),
            "unexpected error: {err:?}"
        );

        // The upstream simulation on its own is fine, and the failure left
        // no fill half-open for it.
        let upstream = ev.evaluate(&graph, NodeId::new(1), &ctx_at(3)).unwrap();
        assert_eq!(value_of(&upstream), 3.0);
    }

    fn budgeted(ram: u64, ratio: f32) -> crate::cache_budget::SharedCacheBudget {
        crate::cache_budget::SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 1 << 20,
            ram_bytes: ram,
            disk_bytes: 0,
            sim_reserve_ratio: ratio,
        })
    }

    #[test]
    fn a_track_charges_the_sim_share_and_releases_it_with_the_node() {
        let budget = budgeted(10_000, 0.25);
        let counter = Counter {
            state_bytes: 100,
            ..Counter::default()
        };
        let (graph, node) = lone();
        let mut ev = Evaluator::with_budget(budget.clone());
        ev.register(node, stateful_processor(counter));

        ev.evaluate(&graph, node, &ctx_at(4)).unwrap();
        assert_eq!(
            budget.stats().sim_used[Tier::Ram as usize],
            5 * (100 + ENTRY_OVERHEAD)
        );

        ev.invalidate_node(node);
        assert_eq!(ev.sim_track_count(), 0);
        assert_eq!(
            budget.stats().sim_used[Tier::Ram as usize],
            0,
            "dropping the track must release its bytes"
        );
    }

    #[test]
    fn sim_pressure_evicts_the_older_track_whole_and_keeps_the_filling_one() {
        // 1000 bytes of RAM, 250 of it the sim share, which is also what one
        // track may keep. Six tracks of 200 or more do not fit, and the one filled
        // first goes (sim yields only to sim), whole.
        let budget = budgeted(1000, 0.25);
        let counter = Counter {
            state_bytes: 100,
            ..Counter::default()
        };
        let mut graph = Graph::new();
        let mut ev = Evaluator::with_budget(budget);
        for id in 1..=6 {
            graph = graph.add_node(sim_node(id)).unwrap();
            ev.register(NodeId::new(id), stateful_processor(counter.clone()));
        }

        for id in 1..=6 {
            ev.evaluate(&graph, NodeId::new(id), &ctx_at(5)).unwrap();
        }

        assert_eq!(ev.sim_cached_frames(&[], NodeId::new(1)), None);
        assert_eq!(ev.sim_cached_frames(&[], NodeId::new(6)), Some(0..6));
    }

    #[test]
    fn registering_again_starts_the_simulation_over() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        ev.evaluate(&graph, node, &ctx_at(3)).unwrap();

        ev.register(node, stateful_processor(counter.clone()));
        ev.evaluate(&graph, node, &ctx_at(3)).unwrap();

        assert_eq!(counter.initials.load(Ordering::Relaxed), 2);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 6);
    }

    // ---- invalidation -----------------------------------------------------

    use crate::graph::Parameter;

    fn set(graph: Graph, node: u64, key: &str, value: ParameterValue) -> Graph {
        graph
            .set_params(
                NodeId::new(node),
                &[Parameter {
                    key: key.into(),
                    value,
                }],
            )
            .unwrap()
    }

    /// `source(1) -> sim(2)`, plus a node (9) that feeds nothing.
    fn fed_graph() -> Graph {
        wire(
            Graph::new()
                .add_node(source_node(1))
                .unwrap()
                .add_node(sim_node(2))
                .unwrap()
                .add_node(source_node(9))
                .unwrap(),
            1,
            1,
            2,
        )
    }

    fn fed_evaluator(counter: &Counter) -> Evaluator {
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(FrameSource));
        ev.register(NodeId::new(9), Arc::new(FrameSource));
        ev.register(NodeId::new(2), stateful_processor(counter.clone()));
        ev
    }

    #[test]
    fn an_upstream_parameter_change_discards_the_track_and_refills_it() {
        let counter = Counter::default();
        let graph = fed_graph();
        let mut ev = fed_evaluator(&counter);
        let node = NodeId::new(2);

        // state(10) = sum of (1 + t * k) for t in 1..=10.
        let before = ev.evaluate(&graph, node, &ctx_at(10)).unwrap();
        assert_eq!(value_of(&before), 10.0 + 55.0);

        let edited = set(graph, 1, "k", ParameterValue::Float(2.0));
        ev.mark_dirty(&edited, NodeId::new(1));
        let after = ev.evaluate(&edited, node, &ctx_at(10)).unwrap();

        assert_eq!(value_of(&after), 10.0 + 110.0, "the old states were reused");
        assert_eq!(counter.initials.load(Ordering::Relaxed), 2);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 20);
        assert_eq!(ev.sim_cached_frames(&[], node), Some(0..11));
    }

    #[test]
    fn a_change_to_the_simulating_node_discards_its_track_and_stays_deterministic() {
        let (graph, node) = lone();
        let mut ev = Evaluator::new();
        ev.register(node, stateful_processor(Seeded));
        ev.evaluate(&graph, node, &ctx_at(20)).unwrap();

        let edited = set(graph, 2, "seed", ParameterValue::Int(9));
        ev.mark_dirty(&edited, node);
        let refilled = ev.evaluate(&edited, node, &ctx_at(20)).unwrap();

        let mut fresh = Evaluator::new();
        fresh.register(node, stateful_processor(Seeded));
        let expected = fresh.evaluate(&edited, node, &ctx_at(20)).unwrap();
        assert_eq!(
            value_of(&refilled),
            value_of(&expected),
            "a refilled track must equal one built from nothing"
        );
        let stale = {
            let mut old = Evaluator::new();
            old.register(node, stateful_processor(Seeded));
            let original = Graph::new().add_node(sim_node(2)).unwrap();
            value_of(&old.evaluate(&original, node, &ctx_at(20)).unwrap())
        };
        assert_ne!(
            value_of(&refilled),
            stale,
            "the seed change must reach the result or this proves nothing"
        );
    }

    #[test]
    fn invalidating_or_replacing_an_upstream_node_discards_the_track() {
        for replace in [false, true] {
            let counter = Counter::default();
            let graph = fed_graph();
            let mut ev = fed_evaluator(&counter);
            let node = NodeId::new(2);
            ev.evaluate(&graph, node, &ctx_at(10)).unwrap();
            assert_eq!(counter.initials.load(Ordering::Relaxed), 1);

            // Nothing in the graph changes: a media reload or a swapped
            // processor reaches the evaluator only through these.
            if replace {
                ev.register(NodeId::new(1), Arc::new(FrameSource));
            } else {
                ev.invalidate_node(NodeId::new(1));
            }
            ev.evaluate(&graph, node, &ctx_at(12)).unwrap();

            assert_eq!(
                counter.initials.load(Ordering::Relaxed),
                2,
                "the track outlived an invalidation upstream (replace: {replace})"
            );
        }
    }

    #[test]
    fn invalidating_an_unrelated_node_keeps_the_track() {
        let counter = Counter::default();
        let graph = fed_graph();
        let mut ev = fed_evaluator(&counter);
        let node = NodeId::new(2);
        ev.evaluate(&graph, node, &ctx_at(10)).unwrap();

        ev.invalidate_node(NodeId::new(9));
        ev.register(NodeId::new(9), Arc::new(FrameSource));
        ev.evaluate(&graph, node, &ctx_at(12)).unwrap();

        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 12);
    }

    #[test]
    fn an_unrelated_change_does_not_discard_the_track() {
        let counter = Counter::default();
        let graph = fed_graph();
        let mut ev = fed_evaluator(&counter);
        let node = NodeId::new(2);
        ev.evaluate(&graph, node, &ctx_at(10)).unwrap();
        assert_eq!(counter.steps.load(Ordering::Relaxed), 10);

        // A node nothing upstream of the simulation reads, edited and
        // dirtied; and an upstream node moved on the canvas.
        let mut edited = set(graph, 9, "k", ParameterValue::Float(5.0));
        ev.mark_dirty(&edited, NodeId::new(9));
        let mut moved = (**edited.node(NodeId::new(1)).unwrap()).clone();
        moved.metadata.position = (300.0, 40.0);
        moved.metadata.label = Some("renamed".into());
        edited = edited.replace_node(Arc::new(moved));
        ev.mark_dirty(&edited, NodeId::new(1));

        let after = ev.evaluate(&edited, node, &ctx_at(12)).unwrap();

        assert_eq!(value_of(&after), 12.0 + 78.0);
        assert_eq!(
            counter.initials.load(Ordering::Relaxed),
            1,
            "an unrelated edit restarted the simulation"
        );
        assert_eq!(
            counter.steps.load(Ordering::Relaxed),
            12,
            "only the two new frames may step"
        );
    }

    // ---- layer networks ----------------------------------------------------

    use crate::composition::{Composition, Layer};
    use crate::id::{CompId, LayerId};

    fn comp_id() -> CompId {
        CompId::new(1)
    }

    fn layer_id() -> LayerId {
        LayerId::new(1)
    }

    fn layer_scope() -> [PathSegment; 1] {
        [PathSegment::Layer(comp_id(), layer_id())]
    }

    fn document(network: Graph, start_frame: i64) -> Arc<Document> {
        Arc::new(
            Document::default().with_composition(
                Composition::new(comp_id(), "C", (16, 16), FPS, 300)
                    .add_layer(Layer::new(layer_id(), "L", network).with_time(start_frame, 0, 300)),
            ),
        )
    }

    fn pull_in_layer(ev: &mut Evaluator, network: &Graph, frame: u64) -> f32 {
        let out = ev
            .evaluate_sub(
                layer_scope()[0],
                network,
                NodeId::new(2),
                &ctx_at(frame),
                Bindings::default(),
            )
            .unwrap();
        value_of(&out)
    }

    fn layer_evaluator(counter: &Counter, network: &Graph) -> Evaluator {
        let mut ev = fed_evaluator(counter);
        ev.set_document(document(network.clone(), 0));
        ev
    }

    #[test]
    fn a_shell_edit_does_not_discard_a_track_inside_the_layer_network() {
        let counter = Counter::default();
        let network = fed_graph();
        let mut ev = layer_evaluator(&counter, &network);
        assert_eq!(pull_in_layer(&mut ev, &network, 8), 8.0 + 36.0);
        assert_eq!(
            ev.sim_cached_frames(&layer_scope(), NodeId::new(2)),
            Some(0..9)
        );

        // The shell moves in time; the network is the same object.
        ev.set_document(document(network.clone(), 10));

        assert_eq!(
            ev.sim_cached_frames(&layer_scope(), NodeId::new(2)),
            Some(0..9),
            "a shell edit dropped the track"
        );
        assert_eq!(pull_in_layer(&mut ev, &network, 8), 8.0 + 36.0);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn an_edit_elsewhere_in_the_layer_network_keeps_the_track() {
        let counter = Counter::default();
        let network = fed_graph();
        let mut ev = layer_evaluator(&counter, &network);
        pull_in_layer(&mut ev, &network, 8);

        let edited = network.add_node(source_node(50)).unwrap();
        ev.set_document(document(edited.clone(), 0));
        let value = pull_in_layer(&mut ev, &edited, 10);

        assert_eq!(value, 10.0 + 55.0);
        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 10);
    }

    // ---- what an upstream reads from outside its scope -----------------------

    /// `reader(1) -> sim(2)` where `reader` has type `type_key` and, when
    /// given, a `param` naming its target.
    fn reading_network(type_key: &str, target: Option<(&str, &str)>) -> Graph {
        let mut reader = Node::new(NodeId::new(1), type_key).with_output("out", DataTypeId::SCALAR);
        if let Some((key, value)) = target {
            reader = reader.with_param(key, ParameterValue::String(value.into()));
        }
        wire(
            Graph::new()
                .add_node(reader)
                .unwrap()
                .add_node(sim_node(2))
                .unwrap(),
            1,
            1,
            2,
        )
    }

    /// Comp 1 with the layer the simulation sits in (layer 1, `network`),
    /// the layer it reads (layer 2, `target`, starting at `target_start`) and
    /// a bystander (layer 3, `bystander`).
    fn three_layer_document(
        network: &Graph,
        target: &Graph,
        target_start: i64,
        bystander: &Graph,
    ) -> Arc<Document> {
        let layer = |id: u64, network: &Graph, start: i64| {
            Layer::new(LayerId::new(id), format!("L{id}"), network.clone()).with_time(start, 0, 300)
        };
        Arc::new(
            Document::default().with_composition(
                Composition::new(comp_id(), "C", (16, 16), FPS, 300)
                    .add_layer(layer(1, network, 0))
                    .add_layer(layer(2, target, target_start))
                    .add_layer(layer(3, bystander, 0)),
            ),
        )
    }

    /// An evaluator over `reading_network`, with the reader's value coming
    /// from `FrameSource` whatever its type key.
    fn reading_evaluator(counter: &Counter, document: Arc<Document>) -> Evaluator {
        let mut ev = fed_evaluator(counter);
        ev.set_document(document);
        ev
    }

    /// Pull the simulation at `frame` and report how many times it has
    /// started from nothing since the evaluator was made.
    fn restarts_after_pulling(
        ev: &mut Evaluator,
        counter: &Counter,
        network: &Graph,
        frame: u64,
    ) -> usize {
        pull_in_layer(ev, network, frame);
        counter.initials.load(Ordering::Relaxed)
    }

    #[test]
    fn editing_a_layer_a_layer_ref_upstream_points_at_discards_the_track() {
        let counter = Counter::default();
        let network = reading_network("layer.ref", Some(("layer", "2")));
        let target = Graph::new();
        let bystander = Graph::new();
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        // The referenced layer's network changes; nothing in the sim's own
        // network does, so only the premise can tell.
        let edited = target.add_node(source_node(60)).unwrap();
        ev.set_document(three_layer_document(&network, &edited, 0, &bystander));

        assert_eq!(
            restarts_after_pulling(&mut ev, &counter, &network, 8),
            2,
            "the track outlived an edit to the layer it reads"
        );
        assert_eq!(
            ev.sim_cached_frames(&layer_scope(), NodeId::new(2)),
            Some(0..9)
        );
    }

    #[test]
    fn editing_the_shell_of_a_layer_ref_target_discards_the_track() {
        let counter = Counter::default();
        let network = reading_network("layer.ref", Some(("layer", "2")));
        let (target, bystander) = (Graph::new(), Graph::new());
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        ev.set_document(three_layer_document(&network, &target, 12, &bystander));

        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 8), 2);
    }

    #[test]
    fn a_shell_reader_upstream_makes_a_shell_edit_discard_the_track() {
        let counter = Counter::default();
        let network = reading_network("layer.info", Some(("layer", "2")));
        let (target, bystander) = (Graph::new(), Graph::new());
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        // Same networks, one layer's time placement moved.
        ev.set_document(three_layer_document(&network, &target, 12, &bystander));

        assert_eq!(
            restarts_after_pulling(&mut ev, &counter, &network, 8),
            2,
            "the track outlived an edit to a shell its upstream reads"
        );
    }

    #[test]
    fn a_comp_info_reader_keeps_the_track_through_a_sibling_shell_edit() {
        let counter = Counter::default();
        let network = reading_network("comp.info", None);
        let (target, bystander) = (Graph::new(), Graph::new());
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        // Layer 2's placement moves; comp.info reads composition fields and
        // the layer count, and this layer's own placement, not its siblings'.
        ev.set_document(three_layer_document(&network, &target, 12, &bystander));

        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 8), 1);
    }

    #[test]
    fn a_comp_info_reader_discards_the_track_when_a_composition_field_changes() {
        let counter = Counter::default();
        let network = reading_network("comp.info", None);
        let (target, bystander) = (Graph::new(), Graph::new());
        let document = three_layer_document(&network, &target, 0, &bystander);
        let mut ev = reading_evaluator(&counter, document.clone());
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        let mut comp = (**document.get_composition(comp_id()).unwrap()).clone();
        comp.duration_frames += 10;
        ev.set_document(Arc::new((*document).clone().with_composition(comp)));

        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 8), 2);
    }

    #[test]
    fn an_edit_to_a_layer_nothing_upstream_reads_keeps_the_track() {
        let counter = Counter::default();
        let network = reading_network("layer.ref", Some(("layer", "2")));
        let (target, bystander) = (Graph::new(), Graph::new());
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        // Layer 3's network changes, and so does the document — but layer 3 is
        // not what the reader names. (Shells are compared per composition, so
        // only a network edit is "unrelated" here.)
        let edited = bystander.add_node(source_node(70)).unwrap();
        ev.set_document(three_layer_document(&network, &target, 0, &edited));

        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 8), 1);
    }

    #[test]
    fn a_sim_that_reads_nothing_outside_keeps_its_track_through_any_document_edit() {
        let counter = Counter::default();
        let network = fed_graph();
        let (target, bystander) = (Graph::new(), Graph::new());
        let mut ev = reading_evaluator(
            &counter,
            three_layer_document(&network, &target, 0, &bystander),
        );
        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 6), 1);

        ev.set_document(three_layer_document(
            &network,
            &target.add_node(source_node(60)).unwrap(),
            12,
            &bystander,
        ));

        assert_eq!(restarts_after_pulling(&mut ev, &counter, &network, 8), 1);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn deleting_the_node_or_the_layer_releases_its_track() {
        let counter = Counter::default();
        let network = fed_graph();
        let mut ev = layer_evaluator(&counter, &network);
        pull_in_layer(&mut ev, &network, 4);
        assert_eq!(ev.sim_track_count(), 1);

        // The node is gone from the edited network: nothing will pull it.
        let without = Graph::new().add_node(source_node(1)).unwrap();
        ev.set_document(document(without, 0));
        assert_eq!(ev.sim_track_count(), 0, "a deleted node kept its track");

        // And the same for a whole layer.
        let mut ev = layer_evaluator(&counter, &network);
        pull_in_layer(&mut ev, &network, 4);
        assert_eq!(ev.sim_track_count(), 1);
        ev.set_document(Arc::new(
            Document::default().with_composition(Composition::new(
                comp_id(),
                "C",
                (16, 16),
                FPS,
                300,
            )),
        ));
        assert_eq!(ev.sim_track_count(), 0, "a deleted layer kept its track");
    }

    #[test]
    fn a_changed_frame_rate_discards_the_track() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        ev.evaluate(&graph, node, &ctx_at(5)).unwrap();

        let other = EvalContext::new(5, FrameRate { num: 24, den: 1 }, (64, 64));
        ev.mark_dirty(&graph, node);
        ev.evaluate(&graph, node, &other).unwrap();

        assert_eq!(
            counter.initials.load(Ordering::Relaxed),
            2,
            "states counted at 30 fps answered a 24 fps pull"
        );
    }

    // ---- provisional answers and cancellation --------------------------------

    /// `input + 1000`, so a value that went through a downstream node is
    /// recognisable.
    struct PlusThousand;

    impl NodeProcessor for PlusThousand {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let input = inputs[0]
                .as_ref()
                .and_then(|data| data.downcast_ref::<Scalar>())
                .ok_or_else(|| anyhow::anyhow!("expected a scalar"))?;
            Ok(Arc::new(Scalar(input.0 + 1000.0)))
        }
    }

    /// `sim(2) -> plus(3)`.
    fn consumed_sim() -> Graph {
        wire(
            Graph::new()
                .add_node(sim_node(2))
                .unwrap()
                .add_node(
                    Node::new(NodeId::new(3), "test.plus")
                        .with_input("a", &[DataTypeId::SCALAR])
                        .with_output("out", DataTypeId::SCALAR),
                )
                .unwrap(),
            1,
            2,
            3,
        )
    }

    fn consumed_evaluator(counter: &Counter) -> Evaluator {
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), stateful_processor(counter.clone()));
        ev.register(NodeId::new(3), Arc::new(PlusThousand));
        ev
    }

    #[test]
    fn a_long_forward_jump_answers_with_the_last_state_and_does_not_poison_the_cache() {
        let counter = Counter::default();
        let graph = consumed_sim();
        let mut ev = consumed_evaluator(&counter);
        let sink = NodeId::new(3);
        ev.set_sim_provisional(Some(5));

        // The first pull has no last state to show, so it fills frame 0 and
        // is not provisional: it did reach the frame it was asked for.
        let first = ev.evaluate(&graph, sink, &ctx_at(0)).unwrap();
        assert_eq!(value_of(&first), 1000.0);
        assert!(!ev.has_provisional_sim());

        let jumped = ev.evaluate(&graph, sink, &ctx_at(300)).unwrap();
        assert_eq!(value_of(&jumped), 1000.0, "frame 0 stands in for frame 300");
        assert!(ev.has_provisional_sim());
        assert_eq!(
            counter.steps.load(Ordering::Relaxed),
            0,
            "the jump must answer before filling anything"
        );

        // Completing the fill: the old frame's value must not be served from
        // the node cache, at the sim node or below it.
        ev.settle_provisional();
        assert!(!ev.has_provisional_sim());
        ev.set_sim_provisional(None);
        let settled = ev.evaluate(&graph, sink, &ctx_at(300)).unwrap();

        assert_eq!(value_of(&settled), 1000.0 + 300.0);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 300);
        assert!(!ev.has_provisional_sim());
    }

    #[test]
    fn a_short_forward_jump_fills_instead_of_answering_provisionally() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        ev.set_sim_provisional(Some(5));
        ev.evaluate(&graph, node, &ctx_at(0)).unwrap();

        let out = ev.evaluate(&graph, node, &ctx_at(5)).unwrap();

        assert_eq!(value_of(&out), 5.0);
        assert!(!ev.has_provisional_sim());
        assert_eq!(counter.steps.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn a_fill_that_gives_way_keeps_its_frames_and_resumes() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        let checks = Arc::new(AtomicUsize::new(0));
        let seen = checks.clone();
        // Gives way on the fifth look: frames 0..=3 are computed first.
        ev.set_sim_cancel(Some(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst) >= 4
        })));

        let err = ev
            .evaluate(&graph, node, &ctx_at(50))
            .err()
            .expect("the fill was told to stop");
        assert!(err.is_cancelled(), "unexpected error: {err:?}");
        assert_eq!(ev.sim_cached_frames(&[], node), Some(0..4));

        ev.set_sim_cancel(None);
        let out = ev.evaluate(&graph, node, &ctx_at(50)).unwrap();

        assert_eq!(value_of(&out), 50.0);
        assert_eq!(
            counter.steps.load(Ordering::Relaxed),
            50,
            "the frames computed before giving way were computed again"
        );
        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
    }

    /// A value that costs 400 bytes of the budget, to put ordinary pressure on it.
    struct Blob;

    impl NodeData for Blob {
        fn data_type_id(&self) -> DataTypeId {
            DataTypeId::SCALAR
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn byte_size(&self) -> u64 {
            400
        }
    }

    struct BlobSource;

    impl NodeProcessor for BlobSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Blob))
        }
    }

    #[test]
    fn ordinary_pressure_never_evicts_a_sim_track() {
        // 1000 bytes of RAM, 250 held back for sim. The track costs 200, and
        // ten 400-byte node results go through behind it.
        let budget = budgeted(1000, 0.25);
        let counter = Counter {
            state_bytes: 100,
            ..Counter::default()
        };
        let mut graph = Graph::new().add_node(sim_node(2)).unwrap();
        let mut ev = Evaluator::with_budget(budget.clone());
        ev.register(NodeId::new(2), stateful_processor(counter));
        for id in 10..20 {
            graph = graph
                .add_node(
                    Node::new(NodeId::new(id), "test.blob").with_output("out", DataTypeId::SCALAR),
                )
                .unwrap();
            ev.register(NodeId::new(id), Arc::new(BlobSource));
        }
        ev.evaluate(&graph, NodeId::new(2), &ctx_at(1)).unwrap();

        for id in 10..20 {
            ev.evaluate(&graph, NodeId::new(id), &ctx_at(1)).unwrap();
        }

        assert_eq!(
            ev.sim_cached_frames(&[], NodeId::new(2)),
            Some(0..2),
            "ordinary frame pressure evicted the track"
        );
        let stats = budget.stats();
        assert!(
            stats.used[Tier::Ram as usize] - stats.sim_used[Tier::Ram as usize] <= 1000 - 250,
            "ordinary entries spent the sim share: {stats:?}"
        );
        assert!(
            ev.cache_stats().entries < 10,
            "the node results were not under pressure, so the test proves nothing"
        );
    }

    #[test]
    fn a_keyframe_edit_upstream_discards_the_track() {
        use crate::animation::channel::AnimationChannel;
        use crate::animation::curve::KeyframeCurve;
        use crate::animation::interpolation::Interpolation::Linear;
        let keyed = |end: f32| {
            let mut curve = KeyframeCurve::new();
            curve.insert(0, 1.0, Linear);
            curve.insert(10, end, Linear);
            ParameterValue::Channel(AnimationChannel::keyframes(curve))
        };
        let counter = Counter::default();
        let graph = set(fed_graph(), 1, "k", keyed(1.0));
        let mut ev = fed_evaluator(&counter);
        ev.evaluate(&graph, NodeId::new(2), &ctx_at(6)).unwrap();

        // Same shape, one key's value moved: only the structural walk of the
        // parameter tree can see it.
        let edited = set(graph, 1, "k", keyed(2.0));
        ev.mark_dirty(&edited, NodeId::new(1));
        ev.evaluate(&edited, NodeId::new(2), &ctx_at(6)).unwrap();

        assert_eq!(counter.initials.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn time_shifted_views_share_one_track() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        let shifted = |ev: &mut Evaluator, owner: u64, frame: u64| {
            let out = ev
                .evaluate_sub(
                    PathSegment::TimeShift(NodeId::new(owner), frame),
                    &graph,
                    node,
                    &ctx_at(frame),
                    Bindings::default(),
                )
                .unwrap();
            value_of(&out)
        };

        assert_eq!(shifted(&mut ev, 99, 5), 5.0);
        assert_eq!(shifted(&mut ev, 99, 8), 8.0);
        // Another owner's shift, and the unshifted scope, read the same track.
        assert_eq!(shifted(&mut ev, 98, 3), 3.0);
        assert_eq!(
            value_of(&ev.evaluate(&graph, node, &ctx_at(8)).unwrap()),
            8.0
        );

        assert_eq!(counter.initials.load(Ordering::Relaxed), 1);
        assert_eq!(counter.steps.load(Ordering::Relaxed), 8);
        assert_eq!(ev.sim_track_count(), 1);
        assert_eq!(ev.sim_cached_frames(&[], node), Some(0..9));
    }

    #[test]
    fn iteration_scopes_keep_separate_tracks() {
        let counter = Counter::default();
        let (graph, node) = lone();
        let mut ev = counter_evaluator(&counter, 2);
        for piece in 0..2 {
            ev.evaluate_sub(
                PathSegment::Iteration(NodeId::new(99), piece),
                &graph,
                node,
                &ctx_at(4),
                Bindings::default(),
            )
            .unwrap();
        }

        assert_eq!(ev.sim_track_count(), 2, "pieces are different upstreams");
        assert_eq!(counter.initials.load(Ordering::Relaxed), 2);
    }

    // ---- checkpoints ---------------------------------------------------------

    /// `source(1) -> sim(2)`, so each state differs from every other and a
    /// state reached by the wrong route cannot pass for the right one.
    fn thinning_setup(
        ram: u64,
    ) -> (
        Graph,
        Counter,
        Evaluator,
        crate::cache_budget::SharedCacheBudget,
    ) {
        let counter = Counter {
            state_bytes: 100,
            ..Counter::default()
        };
        let budget = budgeted(ram, 0.25);
        let mut ev = Evaluator::with_budget(budget.clone());
        ev.register(NodeId::new(1), Arc::new(FrameSource));
        ev.register(NodeId::new(2), stateful_processor(counter.clone()));
        (fed_graph(), counter, ev, budget)
    }

    /// The value at every frame of an evaluator with no budget at all.
    fn unthinned_values(frames: u64) -> Vec<f32> {
        let counter = Counter::default();
        let mut ev = fed_evaluator(&counter);
        let graph = fed_graph();
        (0..frames)
            .map(|f| value_of(&ev.evaluate(&graph, NodeId::new(2), &ctx_at(f)).unwrap()))
            .collect()
    }

    #[test]
    fn a_track_over_its_share_thins_to_checkpoints_and_stays_inside_it() {
        // 4000 bytes of RAM: a 1000-byte share, ten 100-byte states.
        let (graph, _counter, mut ev, budget) = thinning_setup(4000);
        let node = NodeId::new(2);

        ev.evaluate(&graph, node, &ctx_at(99)).unwrap();

        let stats = budget.stats();
        assert!(
            stats.sim_used[Tier::Ram as usize] <= 1000,
            "the track outgrew its share: {stats:?}"
        );
        let stored = ev.sim_stored_states(&[], node).unwrap();
        assert!((3..=10).contains(&stored), "stored {stored} states");
        assert_eq!(
            ev.sim_cached_frames(&[], node),
            Some(0..100),
            "thinning must not shrink what the track covers"
        );
    }

    #[test]
    fn every_frame_of_a_thinned_track_equals_the_unthinned_run() {
        let reference = unthinned_values(100);
        let (graph, _counter, mut ev, _budget) = thinning_setup(4000);
        let node = NodeId::new(2);
        ev.evaluate(&graph, node, &ctx_at(99)).unwrap();

        // Down, up and through a prime stride, so no frame is a favourite.
        let order: Vec<u64> = (0..100)
            .rev()
            .chain(0..100)
            .chain((0..100).map(|f| (f * 37) % 100))
            .collect();
        for frame in order {
            let got = value_of(&ev.evaluate(&graph, node, &ctx_at(frame)).unwrap());
            assert_eq!(got, reference[frame as usize], "frame {frame} differs");
        }
    }

    #[test]
    fn a_rewind_costs_fewer_steps_than_the_stride() {
        let (graph, counter, mut ev, _budget) = thinning_setup(4000);
        let node = NodeId::new(2);
        ev.evaluate(&graph, node, &ctx_at(99)).unwrap();
        let key = NodeKey {
            path: ev.paths.id_of(&[]).unwrap(),
            node,
        };
        let stride = ev.sim.stride(&key).unwrap();
        assert!(stride > 1, "nothing thinned, so the test proves nothing");

        for frame in 0..99 {
            // Move the playhead off the frame first: the node-result cache
            // would otherwise serve a repeated pull without reaching the track.
            let before = counter.steps.load(Ordering::Relaxed);
            ev.evaluate(&graph, node, &ctx_at(frame)).unwrap();
            let cost = counter.steps.load(Ordering::Relaxed) - before;
            assert!(
                (cost as u64) < stride,
                "frame {frame} took {cost} steps with a stride of {stride}"
            );
        }
    }

    #[test]
    fn a_track_that_fits_its_share_keeps_every_frame() {
        let (graph, counter, mut ev, _budget) = thinning_setup(40_000);
        let node = NodeId::new(2);
        ev.evaluate(&graph, node, &ctx_at(30)).unwrap();
        let before = counter.steps.load(Ordering::Relaxed);

        ev.evaluate(&graph, node, &ctx_at(7)).unwrap();

        assert_eq!(ev.sim_stored_states(&[], node), Some(31));
        assert_eq!(counter.steps.load(Ordering::Relaxed), before);
    }

    #[test]
    fn a_long_track_over_budget_accounts_its_bookkeeping_inside_the_share() {
        // 2000 one-byte states: the states alone would be 2 KB, the per-entry
        // bookkeeping several times that. The share is 1000 bytes, and what
        // the track holds — entries included — must be charged inside it.
        let counter = Counter {
            state_bytes: 1,
            ..Counter::default()
        };
        let budget = budgeted(4000, 0.25);
        let mut ev = Evaluator::with_budget(budget.clone());
        ev.register(NodeId::new(2), stateful_processor(counter));
        let graph = Graph::new().add_node(sim_node(2)).unwrap();
        let node = NodeId::new(2);

        ev.evaluate(&graph, node, &ctx_at(1999)).unwrap();

        let stored = ev.sim_stored_states(&[], node).unwrap() as u64;
        let charged = budget.stats().sim_used[Tier::Ram as usize];
        assert!(
            charged <= 1000,
            "charged {charged} against a 1000-byte share"
        );
        assert!(
            stored * (1 + ENTRY_OVERHEAD) == charged,
            "the charge is not the retained entries: {stored} entries, {charged} bytes"
        );
        assert!(stored < 40, "the track kept {stored} entries");
        assert_eq!(ev.sim_cached_frames(&[], node), Some(0..2000));
    }
}

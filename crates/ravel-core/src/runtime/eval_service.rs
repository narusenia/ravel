// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Background evaluation service (Phase 1 of
//! `docs/implementation/done/eval-render-performance-plan.md`).
//!
//! Owns a dedicated worker thread that runs an [`Evaluator`] so graph
//! evaluation never blocks the UI thread. Requests carry a monotonically
//! increasing generation number and are **latest-wins**: when several
//! requests queue up while the worker is busy (e.g. every `Change` event of
//! a parameter scrub), the worker drains the queue and evaluates only the
//! newest one, merging the [`InvalidationHint`]s of the skipped requests so
//! no processor rebuild is lost. Coalescing is per *request*, not per target:
//! a request names one or more output nodes and either all of them are
//! evaluated or the whole request is dropped.
//!
//! Multiple targets run through the same [`Evaluator`], which is the point of
//! carrying them in one request — an inspection target upstream of the
//! composition output is a cache hit rather than a second full pull, and a
//! second service would duplicate the cache and the GPU pipeline instead.
//! The update is emitted once, after the last target: adding a target
//! therefore delays the first one's arrival by whatever the rest cost, so the
//! composition output is only as prompt as the slowest inspection target
//! riding along with it.
//!
//! The service is generic over [`EvalWorkerHooks`] so `ravel-core` stays
//! free of GPU and UI dependencies: the host supplies processor
//! registration (`sync`) and output post-processing (`finalize`, e.g.
//! rasterizing a `Geometry` for the viewer) and receives results through
//! the `on_update` callback, which is invoked on the worker thread.

use crate::composition::Document;
use crate::eval::{
    CacheIdentity, EvalContext, EvalError, Evaluator, PathSegment, ProcessorRegistry,
};
use crate::graph::Graph;
use crate::id::{CompId, NodeId};
use crate::runtime::frame_cache::SharedFrameCache;
use crate::types::NodeData;
use crossbeam_channel::{Sender, unbounded};
use std::sync::Arc;
use std::thread::JoinHandle;

/// What changed in the graph since the previous request.
///
/// Drives how [`EvalWorkerHooks::sync`] refreshes processor registrations.
/// Hints of coalesced (skipped) requests are merged, keeping the strongest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvalidationHint {
    /// Nothing changed; pull only (e.g. selection switch).
    None,
    /// Only parameters of these nodes changed; rebuilding just their
    /// processors preserves the evaluator cache for everything else.
    Params(Vec<NodeId>),
    /// Topology changed (nodes/edges added or removed, undo/redo);
    /// registrations must be rebuilt from scratch.
    Structural,
}

impl InvalidationHint {
    /// Merge with the hint of a newer request, keeping the strongest.
    /// `Structural` absorbs everything; `Params` unions node lists.
    pub fn merge(self, newer: Self) -> Self {
        use InvalidationHint::*;
        match (self, newer) {
            (Structural, _) | (_, Structural) => Structural,
            (Params(mut a), Params(b)) => {
                for id in b {
                    if !a.contains(&id) {
                        a.push(id);
                    }
                }
                Params(a)
            }
            (Params(a), None) => Params(a),
            (None, other) => other,
        }
    }
}

/// What one target of an [`EvalRequest`] evaluated to.
pub type EvalOutput = Result<Arc<dyn NodeData>, EvalError>;

/// Result of one background evaluation, delivered via `on_update`.
pub struct EvalUpdate {
    /// Generation of the request that produced this result. Consumers must
    /// drop updates whose generation is older than the latest they issued.
    pub generation: u64,
    /// Frame the request evaluated at, so a consumer-side drop can be
    /// correlated with the worker's "eval result sent" log line.
    pub frame: u64,
    /// One (finalized) outcome per requested node, **in the order of
    /// [`EvalRequest::nodes`]** and with the same length: a target that
    /// failed contributes its `Err` rather than dropping out, so a consumer
    /// can address results positionally as well as by id.
    pub results: Vec<(NodeId, EvalOutput)>,
    /// Per-node `process()` durations of this evaluation (cache hits are
    /// absent). Drives the node editor's load readout.
    ///
    /// Aggregated over *all* targets of the request, because the targets
    /// share one [`Evaluator`] pass: a node evaluated for the first target
    /// is normally a cache hit for the second and therefore appears once,
    /// which is exactly the cache sharing the multi-target form exists for.
    /// An eviction between targets can still process the same node twice and
    /// list it twice, so consumers must not sum by id — the node editor's
    /// readout takes the last entry.
    pub timings: Vec<(NodeId, std::time::Duration)>,
}

/// One background evaluation request (see [`EvalService::request`]).
pub struct EvalRequest {
    /// The graph the `nodes` live in (a compiled shell graph or one
    /// layer/subnet network — nested networks are pulled through the
    /// document).
    pub graph: Graph,
    /// The output nodes to pull, evaluated in order through one
    /// [`Evaluator`] so later targets reuse what earlier ones computed.
    /// A failing target does not stop the rest.
    pub nodes: Vec<NodeId>,
    /// Which composition this request evaluates, for the output-stage frame
    /// cache (`CACHE-5`).
    ///
    /// `Some` opts the request's **first** target — the composition output by
    /// the convention every caller already follows — into the frame cache,
    /// keyed by `(comp, TimeKey)`. The remaining targets are inspection
    /// points inside the graph and go through the evaluator as before.
    ///
    /// `None` opts out entirely, and so does a request with a non-empty
    /// [`path`](Self::path) or no [`document`](Self::document): a render, a
    /// benchmark and a network preview each want their own evaluation rather
    /// than the interactive cache, and the invalidation this layer relies on
    /// is a document diff it has nothing to compare without a document.
    pub comp: Option<CompId>,
    /// Ownership path the evaluation runs under; empty for the root scope.
    /// A node previewed inside a layer network passes
    /// `[PathSegment::Layer(comp, layer), ...]` so cache keys and
    /// `layer.ref` resolution match the shell-driven evaluation
    /// (REQ-LAYER-007/011).
    pub path: Vec<PathSegment>,
    pub ctx: EvalContext,
    /// Document snapshot for nested evaluation (network boundaries,
    /// `layer.ref`, media assets). Replacing it invalidates changed scopes
    /// via [`Evaluator::set_document`].
    pub document: Option<Arc<Document>>,
    pub hint: InvalidationHint,
}

/// The evaluator as a [`EvalWorkerHooks::sync`] implementation sees it:
/// processor registration and nothing else.
///
/// A hook used to receive `&mut Evaluator`, and the natural way to write a
/// structural resync was `*evaluator = Evaluator::new()`. That threw away
/// state the *service* owns — the cache budget — and since the worker
/// escalates its first request to [`InvalidationHint::Structural`], it
/// happened before the first frame was ever evaluated. The result was an
/// application whose node cache had no limit while every unit test passed.
///
/// Handing out a view instead makes that assignment a compile error, and the
/// reset it was written for now happens in the service (see
/// [`EvalService::spawn_with_budget`]) before `sync` is called at all.
pub struct ProcessorSync<'a> {
    evaluator: &'a mut Evaluator,
}

impl<'a> ProcessorSync<'a> {
    /// Lend `evaluator` to a [`EvalWorkerHooks::sync`] call.
    ///
    /// Built by whoever *owns* the evaluator — the service, or a test driving
    /// a hook directly. A hook only ever sees `&mut ProcessorSync`, so it
    /// cannot reach the evaluator through this.
    pub fn new(evaluator: &'a mut Evaluator) -> Self {
        Self { evaluator }
    }
}

impl ProcessorRegistry for ProcessorSync<'_> {
    fn register(&mut self, node: NodeId, processor: Arc<dyn crate::eval::NodeProcessor>) {
        self.evaluator.register(node, processor);
    }

    fn processor(&self, node: NodeId) -> Option<&Arc<dyn crate::eval::NodeProcessor>> {
        self.evaluator.processor(node)
    }

    fn invalidate_node(&mut self, node: NodeId) {
        self.evaluator.invalidate_node(node);
    }
}

/// Host-supplied policy run on the worker thread.
pub trait EvalWorkerHooks: Send + 'static {
    /// Refresh processor registrations according to `hint`. `document`
    /// carries the request's document snapshot so layer networks (recursively
    /// including subnets) can be registered alongside `graph`. The first
    /// request a worker sees is always escalated to
    /// [`InvalidationHint::Structural`], so implementations may treat
    /// `None` as a strict no-op.
    ///
    /// **A structural hint arrives with the evaluator already reset**
    /// ([`Evaluator::reset`]): registrations, caches, dirty flags and scope
    /// state are gone and the cache budget is intact. An implementation
    /// registers what the graph and document need and nothing more — it has
    /// no way to replace the evaluator, by design (see [`ProcessorSync`]).
    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    );

    /// Post-process a successful evaluation output (e.g. read a GPU frame
    /// back, or rasterize `Geometry` into a `FrameBuffer` for the viewer).
    /// Defaults to a pass-through.
    ///
    /// **`None` means the post-processing failed.** The worker then delivers
    /// `value` unchanged — the same picture a hook that swallowed its own
    /// error would have produced — but **does not cache it**. That
    /// distinction is the whole reason this returns an `Option`: the frame
    /// cache stores the finalized form and a hit never re-runs this method,
    /// so caching a fallback would freeze one transient failure (a readback
    /// that lost the device, a rasterize that ran out of memory) into a
    /// viewer that stays blank until the composition is edited.
    fn finalize(
        &mut self,
        value: &Arc<dyn NodeData>,
        ctx: &EvalContext,
    ) -> Option<Arc<dyn NodeData>> {
        let _ = ctx;
        Some(value.clone())
    }
}

struct Request {
    inner: EvalRequest,
    generation: u64,
}

/// Handle owned by the UI thread. Dropping it shuts the worker down.
pub struct EvalService {
    tx: Option<Sender<Request>>,
    generation: u64,
    worker: Option<JoinHandle<()>>,
    frames: SharedFrameCache,
}

impl EvalService {
    /// Spawn the worker thread with an **unbounded** result cache.
    ///
    /// `on_update` is invoked on the worker thread for every completed
    /// evaluation; forward it to the UI through a channel or executor of the
    /// host's choosing. An application uses
    /// [`spawn_with_budget`](Self::spawn_with_budget) so the worker's cache
    /// is accounted for with every other cache in the process.
    pub fn spawn<H, F>(hooks: H, on_update: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        Self::spawn_inner(hooks, None, on_update)
    }

    /// Spawn the worker thread with a result cache bounded by `budget`.
    ///
    /// The budget is created by the application and shared with every other
    /// cache — notably the texture pool, whose idle allowance is whatever the
    /// resident side of the same VRAM tier leaves over. The worker builds its
    /// [`Evaluator`] on its own thread, so the budget has to be handed in
    /// here rather than attached afterwards.
    pub fn spawn_with_budget<H, F>(
        hooks: H,
        budget: crate::cache_budget::SharedCacheBudget,
        on_update: F,
    ) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        Self::spawn_inner(hooks, Some(budget), on_update)
    }

    fn spawn_inner<H, F>(
        mut hooks: H,
        budget: Option<crate::cache_budget::SharedCacheBudget>,
        on_update: F,
    ) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        let (tx, rx) = unbounded::<Request>();
        let frames = SharedFrameCache::new(budget.clone());
        let worker_frames = frames.clone();
        let worker = std::thread::Builder::new()
            .name("ravel-eval-service".into())
            .spawn(move || {
                let frames = worker_frames;
                // Kept beside the evaluator so the per-request log line can
                // say what the caches are spending *against* — a byte figure
                // with no ceiling next to it says nothing about pressure.
                let worker_budget = budget.clone();
                let mut evaluator = match budget {
                    Some(budget) => Evaluator::with_budget(budget),
                    None => Evaluator::new(),
                };
                // The document the frame cache last invalidated against. Kept
                // here rather than read back from the evaluator because a
                // structural resync clears the evaluator's copy, and the
                // frame cache must still be able to tell an edit from a
                // composition switch.
                let mut cached_document: Option<Arc<Document>> = None;
                let mut first = true;
                while let Ok(first_req) = rx.recv() {
                    // Latest-wins: drain everything queued behind the first
                    // request, merging hints so skipped rebuilds still occur.
                    let mut req = first_req;
                    let mut coalesced = 0u32;
                    while let Ok(newer) = rx.try_recv() {
                        coalesced += 1;
                        let prev_hint = req.inner.hint;
                        req = newer;
                        req.inner.hint = prev_hint.merge(std::mem::replace(
                            &mut req.inner.hint,
                            InvalidationHint::None,
                        ));
                    }
                    if first {
                        req.inner.hint = InvalidationHint::Structural;
                        first = false;
                    }
                    tracing::debug!(
                        generation = req.generation,
                        targets = req.inner.nodes.len(),
                        frame = req.inner.ctx.frame,
                        hint = ?req.inner.hint,
                        path_depth = req.inner.path.len(),
                        coalesced,
                        "eval request picked up"
                    );
                    // A structural resync starts from an empty evaluator, and
                    // the service performs that reset itself. Hooks used to
                    // do it by assignment, which also discarded the budget
                    // the service owns; keeping it here means the one place
                    // that knows about the budget is the one place that
                    // clears the evaluator.
                    if matches!(req.inner.hint, InvalidationHint::Structural) {
                        evaluator.reset();
                    }
                    hooks.sync(
                        &mut ProcessorSync::new(&mut evaluator),
                        &req.inner.graph,
                        req.inner.document.as_deref(),
                        &req.inner.hint,
                    );
                    // The document diff drives scoped cache invalidation
                    // (network edits, shell edits, layer.ref referrers).
                    // Installed strictly *after* the reset above, which drops
                    // any document installed beforehand.
                    if let Some(document) = &req.inner.document {
                        evaluator.set_document(document.clone());
                        // The frame cache reads the same diff: many document
                        // commits carry `InvalidationHint::None` and rely on
                        // it, so a hint-driven frame cache would serve those
                        // edits a stale picture.
                        frames.sync_document(cached_document.as_deref(), document);
                        cached_document = Some(document.clone());
                    }
                    // Only the first target is the composition output, and
                    // only a root-scope request with a document has the
                    // invalidation signal this layer needs.
                    let cached_comp = req
                        .inner
                        .comp
                        .filter(|_| req.inner.path.is_empty() && req.inner.document.is_some());
                    let frame_identity = CacheIdentity::of_frame(&req.inner.ctx);
                    let started = std::time::Instant::now();
                    let mut results = Vec::with_capacity(req.inner.nodes.len());
                    let mut timings = Vec::new();
                    for (index, &node) in req.inner.nodes.iter().enumerate() {
                        let frame_comp = cached_comp.filter(|_| index == 0);
                        if let Some(comp) = frame_comp
                            && let Some(value) = frames.get(comp, &frame_identity)
                        {
                            // A hit skips `evaluate_at` *and* `finalize`, so
                            // nothing is processed and no GPU frame is read
                            // back for this target.
                            results.push((node, Ok(value)));
                            continue;
                        }
                        // `finalize` reporting failure keeps its picture but
                        // loses its place in the cache: see the trait method.
                        let mut finalized = true;
                        let result = evaluator
                            .evaluate_at(&req.inner.path, &req.inner.graph, node, &req.inner.ctx)
                            .map(|value| match hooks.finalize(&value, &req.inner.ctx) {
                                Some(value) => value,
                                None => {
                                    finalized = false;
                                    value
                                }
                            });
                        // The budget's tiers are shared, so a node-result
                        // reservation can push a cached frame out. Whoever is
                        // handed an id it does not own routes it to the cache
                        // that does — an eviction nobody acts on leaves the
                        // budget counting fewer bytes than the process holds.
                        let foreign = evaluator.take_foreign_evictions();
                        if !foreign.is_empty() {
                            frames.drop_evicted(&foreign);
                        }
                        if let (Some(comp), Ok(value)) = (frame_comp.filter(|_| finalized), &result)
                        {
                            frames.insert(comp, frame_identity, value.clone());
                            let foreign = frames.take_foreign_evictions();
                            if !foreign.is_empty() {
                                evaluator.drop_evicted(&foreign);
                            }
                        }
                        // Drained per target: `evaluate_at` clears the
                        // evaluator's timing buffer on entry, so reading it
                        // only after the loop would report the last target
                        // alone and silently blank the load readout of the
                        // composition output whenever a second target is
                        // requested.
                        timings.append(&mut evaluator.take_timings());
                        // One failing target must not cost the others their
                        // result: the viewer keeps drawing while an
                        // inspection target is broken, and vice versa.
                        if let Err(err) = &result {
                            tracing::debug!(
                                generation = req.generation,
                                node = node.raw(),
                                frame = req.inner.ctx.frame,
                                %err,
                                "eval target failed"
                            );
                        }
                        results.push((node, result));
                    }
                    let elapsed = started.elapsed();
                    // Per-request outcome: a frozen viewer with a stream of
                    // `ok = 0` results is an evaluation error; results that
                    // never reach the viewer were dropped by the consumer
                    // (stale generation); a result stream that stops entirely
                    // means no requests are being posted.
                    //
                    // The frame-cache figures ride along because the question
                    // they answer — "is the cache still working?" — is only
                    // readable as a ratio over a session, not from any one
                    // request (`CACHE-6`).
                    let frame_stats = frames.stats();
                    let node_stats = evaluator.cache_stats();
                    let budget_stats = worker_budget.as_ref().map(|budget| budget.stats());
                    tracing::debug!(
                        generation = req.generation,
                        frame = req.inner.ctx.frame,
                        targets = results.len(),
                        ok = results.iter().filter(|(_, r)| r.is_ok()).count(),
                        timings = timings.len(),
                        ?elapsed,
                        frames_cached = frame_stats.entries,
                        frame_hit_rate = frame_stats.hit_rate(),
                        frame_bytes_vram = frame_stats.bytes(crate::cache_budget::Tier::Vram),
                        frame_bytes_ram = frame_stats.bytes(crate::cache_budget::Tier::Ram),
                        node_hit_rate = node_stats.hit_rate(),
                        budget_ram_used =
                            budget_stats.map(|stats| stats.used(crate::cache_budget::Tier::Ram)),
                        budget_ram_limit =
                            budget_stats.map(|stats| stats.limit(crate::cache_budget::Tier::Ram)),
                        budget_vram_used =
                            budget_stats.map(|stats| stats.used(crate::cache_budget::Tier::Vram)),
                        budget_vram_limit =
                            budget_stats.map(|stats| stats.limit(crate::cache_budget::Tier::Vram)),
                        "eval result sent"
                    );
                    on_update(EvalUpdate {
                        generation: req.generation,
                        frame: req.inner.ctx.frame,
                        results,
                        timings,
                    });
                }
            })
            .expect("failed to spawn eval service worker");
        Self {
            tx: Some(tx),
            generation: 0,
            worker: Some(worker),
            frames,
        }
    }

    /// The output-stage frame cache this service fills (`CACHE-5`).
    ///
    /// Shared with the worker, so the UI thread may read
    /// [`cached_ranges`](SharedFrameCache::cached_ranges) for the timeline's
    /// cache band and [`stats`](SharedFrameCache::stats) for diagnostics
    /// while an evaluation is running.
    pub fn frame_cache(&self) -> &SharedFrameCache {
        &self.frames
    }

    /// Post an evaluation request and return its generation number.
    pub fn request(&mut self, request: EvalRequest) -> u64 {
        self.generation += 1;
        let generation = self.generation;
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request {
                inner: request,
                generation,
            });
        }
        generation
    }

    /// Invalidate all in-flight results without posting a new request
    /// (e.g. when the selection is cleared and the viewer is blanked).
    /// Returns the new latest generation.
    pub fn cancel_pending(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Generation of the most recent `request` / `cancel_pending` call.
    /// Consumers publish updates monotonically (newer than the last one
    /// they published); after `cancel_pending` they fence at the returned
    /// generation so in-flight results cannot overwrite the cancellation.
    pub fn latest_generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for EvalService {
    fn drop(&mut self) {
        // Closing the channel lets the worker finish its current evaluation
        // and exit on its own. Do NOT join here: the drop may happen on the
        // UI thread (panel teardown, layout rebuild) and a join would block
        // it for up to one full evaluation.
        drop(self.tx.take());
        drop(self.worker.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_budget::{CacheBudgetConfig, SharedCacheBudget};
    use crate::eval::NodeProcessor;
    use crate::graph::{Node, ParameterValue};
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use crate::types::{FrameRate, Scalar};
    use crossbeam_channel::Receiver;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, (16, 16))
    }

    fn value_node(id: u64, value: f32) -> Node {
        Node::new(NodeId::new(id), "test.value")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
    }

    /// `upstream → downstream`: the shape an inspection target has relative to
    /// the composition output, so evaluating the downstream node also
    /// evaluates the upstream one.
    fn chain_graph(upstream: NodeId, downstream: NodeId) -> Graph {
        Graph::new()
            .add_node(value_node(upstream.raw(), 1.0))
            .unwrap()
            .add_node(value_node(downstream.raw(), 2.0).with_input("in", &[DataTypeId::SCALAR]))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                upstream,
                OutputPortIndex(0),
                downstream,
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// Emits the node's `value` parameter; optionally blocks on a gate
    /// channel first and records the processing thread's name.
    struct GatedValue {
        value: f32,
        gate: Option<Receiver<()>>,
        process_count: Arc<AtomicUsize>,
        thread_name: Arc<Mutex<Option<String>>>,
    }

    impl NodeProcessor for GatedValue {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            _scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            *self.thread_name.lock().unwrap() = std::thread::current().name().map(String::from);
            self.process_count.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.recv_timeout(Duration::from_secs(5))
                    .expect("gate closed");
            }
            Ok(Arc::new(Scalar(self.value)))
        }
    }

    /// Hooks that register a `GatedValue` for every graph node and log the
    /// hints they were synced with.
    struct StubHooks {
        gate: Option<Receiver<()>>,
        process_count: Arc<AtomicUsize>,
        thread_name: Arc<Mutex<Option<String>>>,
        hints: Arc<Mutex<Vec<InvalidationHint>>>,
    }

    impl StubHooks {
        fn register_node(&self, evaluator: &mut ProcessorSync<'_>, node: &Node) {
            let value = node
                .parameters
                .iter()
                .find(|p| p.key == "value")
                .and_then(|p| match p.value {
                    ParameterValue::Float(v) => Some(v),
                    _ => None,
                })
                .unwrap_or(0.0);
            evaluator.register(
                node.id,
                Arc::new(GatedValue {
                    value,
                    gate: self.gate.clone(),
                    process_count: self.process_count.clone(),
                    thread_name: self.thread_name.clone(),
                }),
            );
        }
    }

    impl EvalWorkerHooks for StubHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            self.hints.lock().unwrap().push(hint.clone());
            match hint {
                InvalidationHint::None => {}
                InvalidationHint::Params(ids) => {
                    for id in ids {
                        if let Some(node) = graph.node(*id) {
                            self.register_node(evaluator, node);
                        }
                    }
                }
                InvalidationHint::Structural => {
                    // Reset by the service before `sync`.
                    for node in graph.nodes() {
                        self.register_node(evaluator, node);
                    }
                }
            }
        }
    }

    fn req(graph: Graph, node: NodeId, hint: InvalidationHint) -> EvalRequest {
        req_multi(graph, vec![node], hint)
    }

    fn req_multi(graph: Graph, nodes: Vec<NodeId>, hint: InvalidationHint) -> EvalRequest {
        EvalRequest {
            graph,
            nodes,
            comp: None,
            path: Vec::new(),
            ctx: ctx(),
            document: None,
            hint,
        }
    }

    /// The scalar of the `index`-th target of a multi-target update.
    fn scalar_at(update: &EvalUpdate, index: usize) -> f32 {
        update.results[index]
            .1
            .as_ref()
            .expect("evaluation succeeded")
            .downcast_ref::<Scalar>()
            .expect("scalar output")
            .0
    }

    fn scalar_of(update: &EvalUpdate) -> f32 {
        assert_eq!(update.results.len(), 1, "single-target update expected");
        scalar_at(update, 0)
    }

    #[test]
    fn latest_wins_coalesces_queued_requests() {
        let (gate_tx, gate_rx) = unbounded();
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hints = Arc::new(Mutex::new(Vec::new()));
        let hooks = StubHooks {
            gate: Some(gate_rx),
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: hints.clone(),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let gen1 = service.request(req(
            Graph::new().add_node(value_node(1, 1.0)).unwrap(),
            node,
            InvalidationHint::None,
        ));
        // Wait until the worker is inside process() for gen1.
        while process_count.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        // Queue three more scrub ticks while the worker is busy.
        for (i, value) in [2.0f32, 3.0, 4.0].iter().enumerate() {
            let graph = Graph::new().add_node(value_node(1, *value)).unwrap();
            let generation =
                service.request(req(graph, node, InvalidationHint::Params(vec![node])));
            assert_eq!(generation, gen1 + i as u64 + 1);
        }
        // Release gen1, then the (single, coalesced) follow-up evaluation.
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();

        let first = update_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("gen1 update");
        assert_eq!(first.generation, gen1);
        assert_eq!(scalar_of(&first), 1.0);

        let second = update_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("coalesced update");
        assert_eq!(second.generation, service.latest_generation());
        assert_eq!(scalar_of(&second), 4.0);

        // Generations 2 and 3 were skipped: exactly two evaluations ran.
        assert_eq!(process_count.load(Ordering::SeqCst), 2);
        assert!(update_rx.try_recv().is_err());
    }

    #[test]
    fn first_request_escalates_to_structural_and_params_rebuilds() {
        let (update_tx, update_rx) = unbounded();
        let hints = Arc::new(Mutex::new(Vec::new()));
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: hints.clone(),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph_v1 = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph_v1.clone(), node, InvalidationHint::None));
        let first = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&first), 1.0);

        // Parameter edit: only the changed node is re-registered and the
        // new value takes effect.
        let graph_v2 = Graph::new().add_node(value_node(1, 2.0)).unwrap();
        service.request(req(graph_v2, node, InvalidationHint::Params(vec![node])));
        let second = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&second), 2.0);

        let hints = hints.lock().unwrap();
        assert_eq!(hints[0], InvalidationHint::Structural, "first escalated");
        assert_eq!(hints[1], InvalidationHint::Params(vec![node]));
    }

    #[test]
    fn structural_swap_follows_new_graph() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph_a = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph_a, NodeId::new(1), InvalidationHint::None));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Undo/redo-style swap: different node set entirely.
        let graph_b = Graph::new().add_node(value_node(2, 9.0)).unwrap();
        service.request(req(graph_b, NodeId::new(2), InvalidationHint::Structural));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results[0].0, NodeId::new(2));
        assert_eq!(scalar_of(&update), 9.0);
    }

    // ---- multiple targets per request --------------------------------------

    /// A request names one or more outputs and the update carries one entry
    /// per target, positionally aligned with `EvalRequest::nodes`.
    #[test]
    fn every_requested_target_reports_its_own_result() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new()
            .add_node(value_node(1, 1.0))
            .unwrap()
            .add_node(value_node(2, 2.0))
            .unwrap();
        service.request(req_multi(
            graph,
            vec![NodeId::new(1), NodeId::new(2)],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results.len(), 2, "one result per requested target");
        assert_eq!(update.results[0].0, NodeId::new(1));
        assert_eq!(update.results[1].0, NodeId::new(2));
        assert_eq!(scalar_at(&update, 0), 1.0);
        assert_eq!(scalar_at(&update, 1), 2.0);
        // One update for the whole request, not one per target: the request
        // is the unit the latest-wins queue coalesces.
        assert!(update_rx.try_recv().is_err());
    }

    /// The reason the targets share a request rather than a second service:
    /// they share the evaluator's cache. A target upstream of another is
    /// already computed by the time its own turn comes, which shows up as the
    /// *absence* of a second timing entry for it (`take_timings` reports only
    /// freshly processed nodes).
    #[test]
    fn a_target_upstream_of_another_hits_the_shared_cache() {
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hooks = StubHooks {
            gate: None,
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let upstream = NodeId::new(1);
        let downstream = NodeId::new(2);
        service.request(req_multi(
            chain_graph(upstream, downstream),
            vec![downstream, upstream],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_at(&update, 0), 2.0);
        assert_eq!(scalar_at(&update, 1), 1.0);

        assert_eq!(
            update
                .timings
                .iter()
                .filter(|(id, _)| *id == upstream)
                .count(),
            1,
            "the second target re-processed the shared upstream: {:?}",
            update.timings
        );
        // Counting alone would also pass if the chain edge were lost: the
        // upstream would then be processed once too, just by the *second*
        // target instead of as the first target's dependency. Order is what
        // separates a real cache hit from that silent regression — the
        // upstream has to be processed while the first target is pulling it.
        assert_eq!(
            update.timings.len(),
            2,
            "expected one timing per distinct node: {:?}",
            update.timings
        );
        assert_eq!(
            update.timings[0].0, upstream,
            "the upstream was not processed as the first target's dependency: {:?}",
            update.timings
        );
        // The aggregate spans every target: reading the evaluator's timings
        // only after the last one would drop the first target's entirely,
        // because `evaluate_at` clears the buffer on entry.
        assert!(
            update.timings.iter().any(|(id, _)| *id == downstream),
            "the first target's own timing was lost: {:?}",
            update.timings
        );
        assert_eq!(
            process_count.load(Ordering::SeqCst),
            2,
            "two distinct nodes, so two process() calls"
        );
    }

    /// One broken target must not blank the others: the viewer keeps drawing
    /// while an inspection target is unevaluable, and vice versa.
    #[test]
    fn a_failing_target_does_not_cost_the_others_their_result() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let missing = NodeId::new(99);
        let present = NodeId::new(1);
        service.request(req_multi(
            Graph::new().add_node(value_node(1, 1.0)).unwrap(),
            vec![missing, present],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results.len(), 2, "the failure keeps its slot");
        assert_eq!(update.results[0].0, missing);
        assert!(
            matches!(update.results[0].1, Err(EvalError::NodeNotFound(id)) if id == missing),
            "expected the first target to fail as missing"
        );
        assert_eq!(update.results[1].0, present);
        assert_eq!(scalar_at(&update, 1), 1.0);
    }

    #[test]
    fn evaluation_runs_on_the_worker_thread() {
        let (update_tx, update_rx) = unbounded();
        let thread_name = Arc::new(Mutex::new(None));
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: thread_name.clone(),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert_eq!(
            thread_name.lock().unwrap().as_deref(),
            Some("ravel-eval-service")
        );
    }

    #[test]
    fn cancel_pending_outdates_inflight_generations() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        let generation = service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        let cancelled_at = service.cancel_pending();
        assert!(cancelled_at > generation);

        // The update still arrives, but consumers comparing against
        // latest_generation() must treat it as stale.
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.generation < service.latest_generation());
    }

    #[test]
    fn drop_shuts_down_worker_without_hanging() {
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let service = EvalService::spawn(hooks, |_| {});
        drop(service);
    }

    /// Emits 1.0 when the evaluator has a document, errors otherwise —
    /// mirrors the document dependency of the shell processors
    /// (`comp.network`, `layer.ref`).
    struct DocProbe;

    impl NodeProcessor for DocProbe {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            anyhow::ensure!(scope.document().is_some(), "no document set");
            Ok(Arc::new(Scalar(1.0)))
        }
    }

    /// Hooks that re-register everything on Structural (like `GpuEvalHooks`),
    /// relying on the service having reset the evaluator first.
    struct ResettingHooks;

    impl EvalWorkerHooks for ResettingHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(node.id, Arc::new(DocProbe));
                }
            }
        }
    }

    /// A structural sync replaces the evaluator; the request's document must
    /// survive it (regression: the document was installed before sync and
    /// silently dropped, failing every document-dependent evaluation right
    /// after a structural change).
    #[test]
    fn document_survives_a_structural_evaluator_reset() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(ResettingHooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "probe").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        service.request(EvalRequest {
            comp: None,
            graph,
            nodes: vec![node],
            path: Vec::new(),
            ctx: ctx(),
            document: Some(Arc::new(Document::default())),
            // First request escalates to Structural anyway.
            hint: InvalidationHint::Structural,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (_, result) = update.results.into_iter().next().expect("one result");
        assert!(
            result.is_ok(),
            "document-dependent evaluation must succeed after a structural reset: {:?}",
            result.err().map(|e| e.to_string())
        );
    }

    /// The cache budget must survive a structural resync.
    ///
    /// The worker escalates its *first* request to `Structural`, so a hook
    /// that rebuilt the evaluator would throw the budget away before a single
    /// frame was evaluated — the node cache would be unbounded in the real
    /// application while every unit test, which hands the budget straight to
    /// the cache, still passed (`cache-plan.md`: "a test that never reaches
    /// the limit passes even when the budget code is dead").
    #[test]
    fn the_cache_budget_survives_a_structural_resync() {
        let budget = SharedCacheBudget::new(CacheBudgetConfig::default());
        let (update_tx, update_rx) = unbounded();
        let mut service =
            EvalService::spawn_with_budget(ResettingHooks, budget.clone(), move |update| {
                let _ = update_tx.send(update);
            });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "probe").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        service.request(EvalRequest {
            comp: None,
            graph,
            nodes: vec![node],
            path: Vec::new(),
            ctx: ctx(),
            document: Some(Arc::new(Document::default())),
            hint: InvalidationHint::Structural,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.results[0].1.is_ok(), "evaluation failed");
        assert!(
            budget.stats().entries > 0,
            "the evaluated value was cached outside the budget: the \
             structural resync dropped it"
        );
    }

    // ---- the output-stage frame cache (`CACHE-5`) --------------------------

    /// Emits a frame and counts how often it was asked to.
    struct FrameSource(Arc<AtomicUsize>);

    impl NodeProcessor for FrameSource {
        /// A composition output moves with the playhead; without this the
        /// evaluator's own entry is `TIMELESS` and answers every frame, and
        /// the frame cache would never be reached.
        fn is_time_dependent(&self) -> bool {
            true
        }

        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            _scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(crate::types::FrameBuffer::from_f32(
                2,
                2,
                vec![0.25; 2 * 2 * 4],
            )))
        }
    }

    /// Registers [`FrameSource`] for every node and counts `finalize` calls.
    ///
    /// `finalize` is where `GpuEvalHooks` reads a GPU frame back for the
    /// viewer — the only GPU→CPU transfer in the chain — so counting it is
    /// the readback counter `GPUCOMP-7` established, available headlessly.
    struct FrameHooks {
        processed: Arc<AtomicUsize>,
        finalized: Arc<AtomicUsize>,
        /// How many leading `finalize` calls report failure (`0`: none).
        fails_until: usize,
    }

    impl EvalWorkerHooks for FrameHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(node.id, Arc::new(FrameSource(self.processed.clone())));
                }
            }
        }

        fn finalize(
            &mut self,
            value: &Arc<dyn NodeData>,
            _ctx: &EvalContext,
        ) -> Option<Arc<dyn NodeData>> {
            self.finalized.fetch_add(1, Ordering::SeqCst);
            // `fails_until` finalize failures first, then success — the shape
            // of a transient readback loss.
            let ok = self.finalized.load(Ordering::SeqCst) > self.fails_until;
            ok.then(|| value.clone())
        }
    }

    fn comp_id() -> crate::id::CompId {
        crate::id::CompId::new(1)
    }

    fn frame_document() -> Arc<Document> {
        let mut document = Document::default();
        document.compositions.insert(
            comp_id(),
            Arc::new(crate::composition::Composition::new(
                comp_id(),
                "c",
                (2, 2),
                FPS,
                100,
            )),
        );
        Arc::new(document)
    }

    /// A composition whose `Arc` differs from `frame_document`'s — what a
    /// document edit looks like to the frame cache.
    fn edited_document() -> Arc<Document> {
        let mut document = Document::default();
        let mut comp = crate::composition::Composition::new(comp_id(), "c", (2, 2), FPS, 100);
        comp.name = "edited".into();
        document.compositions.insert(comp_id(), Arc::new(comp));
        Arc::new(document)
    }

    fn frame_request(
        graph: Graph,
        node: NodeId,
        frame: u64,
        document: Arc<Document>,
        hint: InvalidationHint,
    ) -> EvalRequest {
        EvalRequest {
            graph,
            nodes: vec![node],
            comp: Some(comp_id()),
            path: Vec::new(),
            ctx: EvalContext::new(frame, FPS, (2, 2)),
            document: Some(document),
            hint,
        }
    }

    /// The core of `cache-plan.md`: scrubbing forward and back must not
    /// recompute anything. `take_timings` reports only nodes that actually
    /// ran, so an empty list *is* "no `process()` call".
    #[test]
    fn scrubbing_back_over_a_visited_frame_processes_nothing() {
        let processed = Arc::new(AtomicUsize::new(0));
        let finalized = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: processed.clone(),
                finalized: finalized.clone(),
                fails_until: 0,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();

        for frame in [0u64, 1] {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(processed.load(Ordering::SeqCst), 2, "two distinct frames");
        let readbacks = finalized.load(Ordering::SeqCst);

        // Back to a frame already visited.
        service.request(frame_request(
            graph,
            node,
            0,
            document,
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(
            update.timings.is_empty(),
            "the revisited frame was recomputed: {:?}",
            update.timings
        );
        assert_eq!(
            processed.load(Ordering::SeqCst),
            2,
            "process() ran for a frame that was already cached"
        );
        // A hit skips `finalize` too, which is where a GPU-resident frame
        // would be read back to host memory.
        assert_eq!(
            finalized.load(Ordering::SeqCst),
            readbacks,
            "a cache hit paid for a GPU→CPU transfer"
        );
        assert!(update.results[0].1.is_ok(), "the hit produced no value");
        assert_eq!(service.frame_cache().stats().hits, 1);
    }

    /// A `finalize` failure must not become a permanent one.
    ///
    /// The cache stores the *finalized* form and a hit never re-runs
    /// `finalize`, so caching the fallback a failed readback returns would
    /// freeze one transient loss into a viewer that never recovers. The
    /// failure is delivered, not stored, and the next request retries.
    #[test]
    fn a_failed_finalize_is_not_cached_and_is_retried() {
        let finalized = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: Arc::new(AtomicUsize::new(0)),
                finalized: finalized.clone(),
                // The first call fails, every later one succeeds.
                fails_until: 1,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();

        service.request(frame_request(
            graph.clone(),
            node,
            0,
            document.clone(),
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            update.results[0].1.is_ok(),
            "the fallback picture was not delivered"
        );
        assert_eq!(
            service.frame_cache().stats().entries,
            0,
            "the failed finalize was cached"
        );

        // Same frame again: the retry reaches `finalize`, succeeds, and only
        // now is the frame worth keeping.
        service.request(frame_request(
            graph,
            node,
            0,
            document,
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            finalized.load(Ordering::SeqCst),
            2,
            "the retry never reached finalize"
        );
        assert_eq!(service.frame_cache().stats().entries, 1);
    }

    /// The frame cache follows the document, not the invalidation hint: many
    /// document commits carry `InvalidationHint::None` and rely on the
    /// evaluator's own diff, so a hint-driven frame cache would serve those
    /// edits a stale picture.
    #[test]
    fn a_document_edit_drops_the_frames_even_without_a_hint() {
        let processed = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: processed.clone(),
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();

        service.request(frame_request(
            graph.clone(),
            node,
            0,
            frame_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(service.frame_cache().stats().entries, 1);

        // Same frame, edited composition, weakest possible hint.
        service.request(frame_request(
            graph,
            node,
            0,
            edited_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            service.frame_cache().stats().hits,
            0,
            "the edit was served the pre-edit frame"
        );
    }

    /// The band's recompute guard (`CACHE-6`): an evaluation served from the
    /// frame cache changes nothing, so the UI thread must be able to see that
    /// without walking every entry. Scrubbing back over visited frames is
    /// exactly when a user generates the most evaluations.
    #[test]
    fn a_cache_hit_leaves_the_frame_cache_version_alone() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: Arc::new(AtomicUsize::new(0)),
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        let frames = service.frame_cache().clone();
        let post = |service: &mut EvalService, frame: u64| {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        };

        post(&mut service, 0);
        post(&mut service, 1);
        let after_fill = frames.version();
        // Back over a frame already cached: a hit, nothing stored.
        post(&mut service, 0);
        assert_eq!(
            frames.version(),
            after_fill,
            "a cache hit moved the version and would force a band recompute"
        );
        // A new frame does move it.
        post(&mut service, 2);
        assert_ne!(frames.version(), after_fill);
    }

    /// A request that names no composition keeps today's behaviour exactly:
    /// a render and a benchmark evaluate rather than share the interactive
    /// cache.
    #[test]
    fn a_request_without_a_composition_is_not_frame_cached() {
        let processed = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: processed.clone(),
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        for _ in 0..2 {
            let mut request = frame_request(
                graph.clone(),
                node,
                0,
                frame_document(),
                InvalidationHint::None,
            );
            request.comp = None;
            service.request(request);
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        // The evaluator's own single-entry cache still answers the repeat,
        // so what this pins is the *frame* cache staying out of it.
        assert!(processed.load(Ordering::SeqCst) >= 1);
        assert_eq!(service.frame_cache().stats().entries, 0);
        assert_eq!(service.frame_cache().stats().requests(), 0);
    }

    /// The band the Timeline draws (`CACHE-6`) is `cached_ranges` over the
    /// frames playback has already produced: it grows as the playhead
    /// advances and is gone the moment the composition is edited.
    #[test]
    fn the_cached_range_grows_with_playback_and_vanishes_on_an_edit() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks {
                processed: Arc::new(AtomicUsize::new(0)),
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        let frames = service.frame_cache().clone();
        let ranges = || frames.cached_ranges(comp_id(), &EvalContext::new(0, FPS, (2, 2)));

        for frame in 0..3u64 {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                ranges(),
                vec![0..frame + 1],
                "the band did not follow the playhead"
            );
        }

        // An edit to the composition, with the weakest hint there is.
        service.request(frame_request(
            graph,
            node,
            0,
            edited_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            ranges(),
            vec![0..1],
            "the edit left the pre-edit frames in the band"
        );
    }

    /// Cross-cache eviction routing: the frame cache and the node-result
    /// cache share the budget's pots, so an eviction list one of them is
    /// handed can name the other's entries. An id nobody drops leaves the
    /// budget counting fewer bytes than the process holds.
    #[test]
    fn a_frame_insert_evicts_node_results_through_the_worker() {
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            // Room for a couple of 2x2 RGBA f32 frames and their node-result
            // copies, so the third request has to push something out.
            ram_bytes: 512,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn_with_budget(
            FrameHooks {
                processed: Arc::new(AtomicUsize::new(0)),
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
            },
            budget.clone(),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        for frame in 0..6u64 {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        let stats = budget.stats();
        assert!(
            stats.used(crate::cache_budget::Tier::Ram) <= 512,
            "the RAM tier stayed over its limit: {stats:?}"
        );
        // What the budget believes it is holding and what the two caches
        // actually hold have to agree, or an eviction was dropped on the
        // floor by whichever cache did not own it. The evaluator holds one
        // node result or none, depending on whether the frame insert of the
        // last request pushed it out — a detail of the byte arithmetic, not
        // of the routing under test.
        let frames = service.frame_cache().stats().entries;
        assert!(
            (frames..=frames + 1).contains(&stats.entries),
            "budget entries ({}) and cached values ({frames}) disagree: {stats:?}",
            stats.entries
        );
    }
}

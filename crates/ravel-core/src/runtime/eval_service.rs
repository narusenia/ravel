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
//!
//! The service is generic over [`EvalWorkerHooks`] so `ravel-core` stays
//! free of GPU and UI dependencies: the host supplies processor
//! registration (`sync`) and output post-processing (`finalize`, e.g.
//! rasterizing a `Geometry` for the viewer) and receives results through
//! the `on_update` callback, which is invoked on the worker thread.

use crate::composition::Document;
use crate::eval::{EvalContext, EvalError, Evaluator, PathSegment, ProcessorRegistry};
use crate::graph::Graph;
use crate::id::NodeId;
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
    /// is a cache hit for the second and therefore appears once, which is
    /// exactly the cache sharing the multi-target form exists for.
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

    /// Post-process a successful evaluation output (e.g. rasterize
    /// `Geometry` into a `FrameBuffer` for the viewer). Defaults to a
    /// pass-through.
    fn finalize(&mut self, value: Arc<dyn NodeData>, ctx: &EvalContext) -> Arc<dyn NodeData> {
        let _ = ctx;
        value
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
        let worker = std::thread::Builder::new()
            .name("ravel-eval-service".into())
            .spawn(move || {
                let mut evaluator = match budget {
                    Some(budget) => Evaluator::with_budget(budget),
                    None => Evaluator::new(),
                };
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
                    }
                    let started = std::time::Instant::now();
                    let mut results = Vec::with_capacity(req.inner.nodes.len());
                    let mut timings = Vec::new();
                    for &node in &req.inner.nodes {
                        let result = evaluator
                            .evaluate_at(&req.inner.path, &req.inner.graph, node, &req.inner.ctx)
                            .map(|value| hooks.finalize(value, &req.inner.ctx));
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
                    tracing::debug!(
                        generation = req.generation,
                        frame = req.inner.ctx.frame,
                        targets = results.len(),
                        ok = results.iter().filter(|(_, r)| r.is_ok()).count(),
                        timings = timings.len(),
                        ?elapsed,
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
        }
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
    use crate::id::DataTypeId;
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
}

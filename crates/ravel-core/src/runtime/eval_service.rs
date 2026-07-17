// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Background evaluation service (Phase 1 of
//! `docs/implementation/eval-render-performance-plan.md`).
//!
//! Owns a dedicated worker thread that runs an [`Evaluator`] so graph
//! evaluation never blocks the UI thread. Requests carry a monotonically
//! increasing generation number and are **latest-wins**: when several
//! requests queue up while the worker is busy (e.g. every `Change` event of
//! a parameter scrub), the worker drains the queue and evaluates only the
//! newest one, merging the [`InvalidationHint`]s of the skipped requests so
//! no processor rebuild is lost.
//!
//! The service is generic over [`EvalWorkerHooks`] so `ravel-core` stays
//! free of GPU and UI dependencies: the host supplies processor
//! registration (`sync`) and output post-processing (`finalize`, e.g.
//! rasterizing a `Geometry` for the viewer) and receives results through
//! the `on_update` callback, which is invoked on the worker thread.

use crate::eval::{EvalContext, EvalError, Evaluator};
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

/// Result of one background evaluation, delivered via `on_update`.
pub struct EvalUpdate {
    /// Generation of the request that produced this result. Consumers must
    /// drop updates whose generation is older than the latest they issued.
    pub generation: u64,
    /// The node that was evaluated.
    pub node: NodeId,
    /// The (finalized) evaluation output.
    pub result: Result<Arc<dyn NodeData>, EvalError>,
}

/// Host-supplied policy run on the worker thread.
pub trait EvalWorkerHooks: Send + 'static {
    /// Refresh processor registrations before an evaluation according to
    /// `hint`. The first request a worker sees is always escalated to
    /// [`InvalidationHint::Structural`], so implementations may treat
    /// `None` as a strict no-op.
    fn sync(&mut self, evaluator: &mut Evaluator, graph: &Graph, hint: &InvalidationHint);

    /// Post-process a successful evaluation output (e.g. rasterize
    /// `Geometry` into a `FrameBuffer` for the viewer). Defaults to a
    /// pass-through.
    fn finalize(&mut self, value: Arc<dyn NodeData>, ctx: &EvalContext) -> Arc<dyn NodeData> {
        let _ = ctx;
        value
    }
}

struct Request {
    graph: Graph,
    node: NodeId,
    ctx: EvalContext,
    generation: u64,
    hint: InvalidationHint,
}

/// Handle owned by the UI thread. Dropping it shuts the worker down.
pub struct EvalService {
    tx: Option<Sender<Request>>,
    generation: u64,
    worker: Option<JoinHandle<()>>,
}

impl EvalService {
    /// Spawn the worker thread. `on_update` is invoked on the worker thread
    /// for every completed evaluation; forward it to the UI through a
    /// channel or executor of the host's choosing.
    pub fn spawn<H, F>(mut hooks: H, on_update: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        let (tx, rx) = unbounded::<Request>();
        let worker = std::thread::Builder::new()
            .name("ravel-eval-service".into())
            .spawn(move || {
                let mut evaluator = Evaluator::new();
                let mut first = true;
                while let Ok(first_req) = rx.recv() {
                    // Latest-wins: drain everything queued behind the first
                    // request, merging hints so skipped rebuilds still occur.
                    let mut req = first_req;
                    while let Ok(newer) = rx.try_recv() {
                        let prev_hint = req.hint;
                        req = newer;
                        req.hint = prev_hint
                            .merge(std::mem::replace(&mut req.hint, InvalidationHint::None));
                    }
                    if first {
                        req.hint = InvalidationHint::Structural;
                        first = false;
                    }
                    hooks.sync(&mut evaluator, &req.graph, &req.hint);
                    let result = evaluator
                        .evaluate(&req.graph, req.node, &req.ctx)
                        .map(|value| hooks.finalize(value, &req.ctx));
                    on_update(EvalUpdate {
                        generation: req.generation,
                        node: req.node,
                        result,
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
    pub fn request(
        &mut self,
        graph: Graph,
        node: NodeId,
        ctx: EvalContext,
        hint: InvalidationHint,
    ) -> u64 {
        self.generation += 1;
        let generation = self.generation;
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request {
                graph,
                node,
                ctx,
                generation,
                hint,
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
    /// Updates older than this must be dropped by the consumer.
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
        fn register_node(&self, evaluator: &mut Evaluator, node: &Node) {
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
        fn sync(&mut self, evaluator: &mut Evaluator, graph: &Graph, hint: &InvalidationHint) {
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
                    *evaluator = Evaluator::new();
                    for node in graph.nodes() {
                        self.register_node(evaluator, node);
                    }
                }
            }
        }
    }

    fn scalar_of(update: &EvalUpdate) -> f32 {
        update
            .result
            .as_ref()
            .expect("evaluation succeeded")
            .downcast_ref::<Scalar>()
            .expect("scalar output")
            .0
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
        let gen1 = service.request(
            Graph::new().add_node(value_node(1, 1.0)).unwrap(),
            node,
            ctx(),
            InvalidationHint::None,
        );
        // Wait until the worker is inside process() for gen1.
        while process_count.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        // Queue three more scrub ticks while the worker is busy.
        for (i, value) in [2.0f32, 3.0, 4.0].iter().enumerate() {
            let graph = Graph::new().add_node(value_node(1, *value)).unwrap();
            let generation =
                service.request(graph, node, ctx(), InvalidationHint::Params(vec![node]));
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
        service.request(graph_v1.clone(), node, ctx(), InvalidationHint::None);
        let first = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&first), 1.0);

        // Parameter edit: only the changed node is re-registered and the
        // new value takes effect.
        let graph_v2 = Graph::new().add_node(value_node(1, 2.0)).unwrap();
        service.request(graph_v2, node, ctx(), InvalidationHint::Params(vec![node]));
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
        service.request(graph_a, NodeId::new(1), ctx(), InvalidationHint::None);
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Undo/redo-style swap: different node set entirely.
        let graph_b = Graph::new().add_node(value_node(2, 9.0)).unwrap();
        service.request(graph_b, NodeId::new(2), ctx(), InvalidationHint::Structural);
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.node, NodeId::new(2));
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
        service.request(graph, NodeId::new(1), ctx(), InvalidationHint::None);
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
        let generation = service.request(graph, NodeId::new(1), ctx(), InvalidationHint::None);
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
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hybrid Pull + Dirty Notification DAG evaluation engine.
//!
//! The engine implements the model described in
//! `docs/specifications/architecture.md` (REQ-CORE-002):
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
//! Key properties guaranteed by [`Evaluator::evaluate`]:
//!
//! * **Diamond de-duplication** — a node reached through multiple paths in a
//!   single pull is processed at most once (per-run memoization).
//! * **Cycle safety** — a cyclic graph produces [`EvalError::CycleDetected`]
//!   instead of overflowing the stack.
//! * **Selective re-evaluation** — clean nodes whose inputs did not change are
//!   served from cache; only time-dependent nodes (and their downstream) are
//!   re-evaluated when the [`EvalContext`] frame advances.

use crate::graph::Graph;
use crate::id::{InputPortIndex, NodeId};
use crate::types::{FrameRate, NodeData};
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
// NodeProcessor
// ===========================================================================

/// The per-node-type processing logic invoked by the evaluator.
///
/// Implementors transform their (already-evaluated) `inputs` into a single
/// output value. Inputs are ordered by their target input-port index so a
/// processor can rely on a stable argument order.
pub trait NodeProcessor: Send + Sync {
    /// Process `inputs` for the given evaluation `ctx` and produce one output.
    fn process(
        &self,
        ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>>;

    /// Whether this node's output depends on the [`EvalContext`] (frame/time).
    ///
    /// Time-dependent nodes (clips, time samplers, audio-reactive sources, …)
    /// are re-evaluated whenever the frame advances; time-independent nodes
    /// (constants, static generators) are served from cache across frames.
    fn is_time_dependent(&self) -> bool {
        false
    }
}

// ===========================================================================
// Cache entry
// ===========================================================================

#[derive(Clone)]
struct CacheEntry {
    /// Frame this value was computed for (used only for time-dependent nodes).
    frame: u64,
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
#[derive(Default)]
pub struct Evaluator {
    processors: HashMap<NodeId, Arc<dyn NodeProcessor>>,
    cache: HashMap<NodeId, CacheEntry>,
    dirty: HashSet<NodeId>,
}

impl Evaluator {
    /// Create an evaluator with no processors registered.
    pub fn new() -> Self {
        Self::default()
    }

    // ----- registration ----------------------------------------------------

    /// Register (or replace) the processor for `node`. The node is marked
    /// dirty so its next pull recomputes.
    pub fn register(&mut self, node: NodeId, processor: Arc<dyn NodeProcessor>) {
        self.processors.insert(node, processor);
        self.dirty.insert(node);
    }

    /// Whether `node` is currently marked dirty.
    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.dirty.contains(&node)
    }

    // ----- dirty propagation -----------------------------------------------

    /// Mark `node` and every node reachable downstream from it dirty.
    ///
    /// This is the **push** half of the model: invoked when a node's
    /// parameters (or wiring) change so that the next pull recomputes the
    /// affected subgraph and serves everything else from cache.
    pub fn mark_dirty(&mut self, graph: &Graph, node: NodeId) {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if self.dirty.insert(current) {
                for downstream in graph.outputs_of(current) {
                    stack.push(downstream);
                }
            }
        }
    }

    /// Drop every cached value and clear the dirty set (forces a full recompute
    /// on the next pull). Processor registrations are kept.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.dirty.clear();
    }

    // ----- evaluation ------------------------------------------------------

    /// Pull-evaluate `output` for `ctx`, returning its computed value.
    ///
    /// Inputs are evaluated recursively (depth-first). Nodes not reachable from
    /// `output` are never touched, satisfying "unused nodes are not evaluated".
    pub fn evaluate(
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
        run: &mut HashMap<NodeId, (Arc<dyn NodeData>, bool)>,
        visiting: &mut HashSet<NodeId>,
    ) -> Result<(Arc<dyn NodeData>, bool), EvalError> {
        // Already computed in this pull → reuse (diamond de-duplication).
        if let Some(cached) = run.get(&node) {
            return Ok(cached.clone());
        }
        // Re-entering a node still on the recursion stack means a cycle.
        if !visiting.insert(node) {
            return Err(EvalError::CycleDetected(node));
        }

        // Evaluate upstream inputs first, ordered by target port index.
        let mut in_edges: Vec<(InputPortIndex, NodeId)> = graph
            .edges()
            .filter(|e| e.target == node)
            .map(|e| (e.target_port, e.source))
            .collect();
        in_edges.sort_by_key(|(port, _)| port.0);

        let mut input_values: Vec<Arc<dyn NodeData>> = Vec::with_capacity(in_edges.len());
        let mut any_input_fresh = false;
        for (_, source) in &in_edges {
            let (value, fresh) = self.eval_node(graph, *source, ctx, run, visiting)?;
            any_input_fresh |= fresh;
            input_values.push(value);
        }

        let processor = self
            .processors
            .get(&node)
            .cloned()
            .ok_or(EvalError::MissingProcessor(node))?;
        let time_dependent = processor.is_time_dependent();

        // Decide whether the cached value is still valid.
        let cache_valid = !self.dirty.contains(&node)
            && !any_input_fresh
            && match self.cache.get(&node) {
                Some(entry) => !time_dependent || entry.frame == ctx.frame,
                None => false,
            };

        let result = if cache_valid {
            // SAFETY of unwrap: cache_valid implies the entry exists.
            let value = self.cache.get(&node).unwrap().value.clone();
            (value, false)
        } else {
            let type_key = graph
                .node(node)
                .map(|n| n.type_key.clone())
                .unwrap_or_default();
            let span =
                tracing::debug_span!("node_process", node = node.raw(), type_key = %type_key);
            let _guard = span.enter();
            let input_refs: Vec<&dyn NodeData> = input_values.iter().map(|a| a.as_ref()).collect();
            let produced = processor
                .process(ctx, &input_refs)
                .map_err(|source| EvalError::ProcessFailed { node, source })?;
            let value: Arc<dyn NodeData> = Arc::from(produced);
            self.cache.insert(
                node,
                CacheEntry {
                    frame: ctx.frame,
                    value: value.clone(),
                },
            );
            self.dirty.remove(&node);
            (value, true)
        };

        visiting.remove(&node);
        run.insert(node, result.clone());
        Ok(result)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, OutputPortIndex};
    use crate::types::Scalar;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            _ctx: &EvalContext,
            _inputs: &[&dyn NodeData],
        ) -> anyhow::Result<Box<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(Scalar(self.value)))
        }
    }

    /// A time-dependent source emitting the current frame index as a scalar.
    struct FrameSource {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for FrameSource {
        fn process(
            &self,
            ctx: &EvalContext,
            _inputs: &[&dyn NodeData],
        ) -> anyhow::Result<Box<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(Scalar(ctx.frame as f32)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Sums all scalar inputs and adds 1; counts its invocations.
    struct CountingSum {
        calls: Arc<AtomicUsize>,
    }

    impl NodeProcessor for CountingSum {
        fn process(
            &self,
            _ctx: &EvalContext,
            inputs: &[&dyn NodeData],
        ) -> anyhow::Result<Box<dyn NodeData>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let mut sum = 1.0f32;
            for input in inputs {
                let s = input
                    .downcast_ref::<Scalar>()
                    .ok_or_else(|| anyhow::anyhow!("expected Scalar input"))?;
                sum += s.0;
            }
            Ok(Box::new(Scalar(sum)))
        }
    }

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
                _ctx: &EvalContext,
                _inputs: &[&dyn NodeData],
            ) -> anyhow::Result<Box<dyn NodeData>> {
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
}

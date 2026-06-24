// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Criterion benchmarks for graph evaluation.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameRate, NodeData, Scalar};
use std::sync::Arc;

const FPS: FrameRate = FrameRate { num: 30, den: 1 };

struct PassThrough;

impl NodeProcessor for PassThrough {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        if let Some(first) = inputs.first() {
            let s = first
                .downcast_ref::<Scalar>()
                .copied()
                .unwrap_or(Scalar(0.0));
            Ok(Box::new(s))
        } else {
            Ok(Box::new(Scalar(0.0)))
        }
    }
}

fn build_chain(len: u64) -> (Graph, Evaluator) {
    let mut g = Graph::new();
    let node = |id: u64| -> Node {
        Node::new(NodeId::new(id), "bench")
            .with_input("in", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR)
    };
    g = g.add_node(node(1));
    for i in 2..=len {
        g = g.add_node(node(i));
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
    for i in 1..=len {
        ev.register(NodeId::new(i), Arc::new(PassThrough));
    }
    (g, ev)
}

fn bench_chain_eval(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_eval");
    for &len in &[10, 100, 1000] {
        group.bench_function(format!("len_{len}"), |b| {
            let (g, mut ev) = build_chain(len);
            let ctx = EvalContext::new(0, FPS, (1920, 1080));
            b.iter(|| {
                ev.invalidate_all();
                black_box(ev.evaluate(&g, NodeId::new(len), &ctx).unwrap());
            });
        });
    }
    group.finish();
}

fn bench_cached_eval(c: &mut Criterion) {
    let (g, mut ev) = build_chain(100);
    let ctx = EvalContext::new(0, FPS, (1920, 1080));
    ev.evaluate(&g, NodeId::new(100), &ctx).unwrap();

    c.bench_function("cached_100", |b| {
        b.iter(|| {
            black_box(ev.evaluate(&g, NodeId::new(100), &ctx).unwrap());
        });
    });
}

criterion_group!(benches, bench_chain_eval, bench_cached_eval);
criterion_main!(benches);

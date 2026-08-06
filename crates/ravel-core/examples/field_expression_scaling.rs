// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Does an expression field cost time proportional to its element count?
//!
//! `ExpressionField::sample` builds its variable slice once and then walks the
//! compiled program once per element, so the answer should be yes with a flat
//! per-element cost. The measurement exists because the alternative failure —
//! parsing, or rebuilding context, inside the loop — shows up as a per-element
//! cost that grows with the batch, and nothing else in the test suite would
//! notice.
//!
//! ```sh
//! cargo run -p ravel-core --release --example field_expression_scaling
//! ```
//!
//! Results are recorded in `docs/implementation/perf-baseline.md`. Run it on
//! an idle machine: a concurrent build moves the absolute numbers by more than
//! the effect being measured. The **shape** is more robust than the absolute
//! numbers, because the three sizes are measured round-robin rather than one
//! size at a time — a load spike lands on all three rather than on whichever
//! one happened to be running.

use std::time::Instant;

use ravel_core::eval::EvalContext;
use ravel_core::geometry::{ExpressionField, Field, FieldSample};
use ravel_core::types::{FrameRate, Vec2};

const COUNTS: [usize; 3] = [1_000, 10_000, 100_000];
const ROUNDS: usize = 9;

/// A batch spread over a 1920×1080 canvas, deterministic so runs compare.
fn positions(count: usize) -> Vec<Vec2> {
    (0..count)
        .map(|index| {
            let index = index as f32;
            Vec2(index * 0.37 % 1920.0, index * 0.11 % 1080.0)
        })
        .collect()
}

fn main() {
    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));

    for (label, source) in [
        ("arithmetic", "@P.x * 2 + @P.y"),
        ("trigonometric", "sin(@P.x * 0.1) * cos(@P.y * 0.1) * 100"),
        (
            "noise (specification example)",
            "noise(@P.x * 0.1, time) * (1 - @P.y / res.height)",
        ),
    ] {
        let field = ExpressionField::new(source, 0.0);
        assert!(
            field.error().is_none(),
            "`{source}` did not compile: {:?}",
            field.error()
        );

        println!("\n{label}: `{source}`");
        println!(
            "{:>10}  {:>12}  {:>14}  {:>10}",
            "elements", "median ms", "ns/element", "max/min"
        );

        let batches: Vec<Vec<Vec2>> = COUNTS.iter().map(|count| positions(*count)).collect();
        let inputs: Vec<FieldSample<'_>> = batches
            .iter()
            .map(|batch| FieldSample::positions_only(batch, &ctx))
            .collect();

        // One untimed pass each, so no round pays for the page faults of an
        // output vector's first allocation.
        for input in &inputs {
            std::hint::black_box(field.sample(input));
        }

        let mut timings: Vec<Vec<f64>> = vec![Vec::with_capacity(ROUNDS); COUNTS.len()];
        for _ in 0..ROUNDS {
            for (index, input) in inputs.iter().enumerate() {
                let started = Instant::now();
                let values = field.sample(input);
                let elapsed = started.elapsed().as_secs_f64();
                std::hint::black_box(values);
                timings[index].push(elapsed);
            }
        }

        let mut per_element = Vec::new();
        for (index, count) in COUNTS.iter().enumerate() {
            timings[index].sort_by(f64::total_cmp);
            let median = timings[index][ROUNDS / 2];
            let spread = timings[index][ROUNDS - 1] / timings[index][0];
            let nanoseconds = median * 1e9 / *count as f64;
            per_element.push(nanoseconds);
            println!(
                "{count:>10}  {:>12.3}  {nanoseconds:>14.1}  {spread:>10.2}x",
                median * 1e3
            );
        }

        let smallest = per_element.first().copied().unwrap_or(0.0);
        let largest = per_element.last().copied().unwrap_or(0.0);
        println!(
            "  per-element cost at 1e5 vs 1e3: {:.2}x  (1.00x = exactly linear)",
            largest / smallest
        );
    }
}

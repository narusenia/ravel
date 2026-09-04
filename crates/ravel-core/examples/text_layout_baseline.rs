// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What does laying out 100 and 1000 characters cost, and which stage pays?
//!
//! The typography plan makes this a completion criterion of unit 2 rather than
//! a curiosity: shaping and glyph outline extraction are inherently CPU work
//! with no GPU path to escape to, so if a full layout does not fit in the
//! 16.6 ms of a 60 fps frame, the answer has to be a glyph cache and the
//! design has to know that before caches are built on top of it. Nothing here
//! is cached today — every call parses the face, shapes every paragraph, and
//! re-extracts every distinct glyph outline — which is deliberately the worst
//! case the numbers could be.
//!
//! ```sh
//! cargo run -p ravel-core --release --example text_layout_baseline
//! ```
//!
//! Results are recorded in `docs/implementation/perf-baseline.md`. Run it on
//! an idle machine: a concurrent build moves the absolute numbers by more than
//! the differences between the rows.

use std::sync::Arc;
use std::time::Instant;

use ravel_core::text::{Align, FontRef, LayoutParams, layout_text_timed};

/// The bundled Latin face — the same one `text.font` falls back to.
const GEIST: &[u8] = include_bytes!("../../../assets/fonts/Geist-Regular.ttf");
/// The bundled Japanese face: 4.5 MB and CFF rather than `glyf` outlines.
/// Included because a face this size is where a per-layout face parse or a
/// per-glyph outline cost would stop being a rounding error.
const NOTO_JP: &[u8] = include_bytes!("../../../assets/fonts/NotoSansJP-Regular.otf");

const ROUNDS: usize = 9;
const COUNTS: [usize; 2] = [100, 1000];

fn font(data: &[u8]) -> FontRef {
    FontRef {
        family: "bundled".into(),
        weight: 400,
        italic: false,
        data: Arc::new(data.to_vec()),
        face_index: 0,
        is_fallback: false,
    }
}

/// A fixture of exactly `count` characters, built by repeating `seed` and
/// cutting on a character boundary.
fn fixture(seed: &str, count: usize) -> String {
    seed.chars().cycle().take(count).collect()
}

fn main() {
    // Wrapping on, so line breaking is inside every measurement: a paragraph
    // that never wraps would not measure the part of placement that grows.
    let params = LayoutParams {
        size: 72.0,
        tracking: 0.0,
        leading: 0.0,
        align: Align::Left,
        wrap_width: 1600.0,
        ..LayoutParams::default()
    };

    for (label, data, seed) in [
        (
            "Geist, Latin prose",
            GEIST,
            "The quick brown fox jumps over the lazy dog while a shaper counts every cluster. ",
        ),
        (
            "Noto Sans JP, Japanese prose",
            NOTO_JP,
            "組版は文字を並べる仕事であり、字形の抽出と配置は別の段で計る。",
        ),
    ] {
        let font = font(data);
        let texts: Vec<String> = COUNTS.iter().map(|count| fixture(seed, *count)).collect();

        // One untimed pass each, so no round pays for a first allocation.
        for text in &texts {
            std::hint::black_box(layout_text_timed(&font, text, &params).expect("lays out"));
        }

        println!("\n{label}");
        println!(
            "{:>7}  {:>10}  {:>10}  {:>10}  {:>10}  {:>9}  {:>7}",
            "chars", "total ms", "shape ms", "glyph ms", "place ms", "instances", "glyphs"
        );

        let mut rows: Vec<Vec<[f64; 4]>> = vec![Vec::with_capacity(ROUNDS); COUNTS.len()];
        let mut shapes = vec![(0usize, 0usize); COUNTS.len()];
        for _ in 0..ROUNDS {
            // Round-robin over the sizes, so a load spike lands on both rows
            // rather than on whichever one happened to be running.
            for (index, text) in texts.iter().enumerate() {
                let started = Instant::now();
                let (geometry, timing) = layout_text_timed(&font, text, &params).expect("lays out");
                let total = started.elapsed().as_secs_f64();
                shapes[index] = (geometry.instance_count(), geometry.sources().len());
                std::hint::black_box(&geometry);
                rows[index].push([
                    total,
                    timing.shaping.as_secs_f64(),
                    timing.outlines.as_secs_f64(),
                    timing.placement.as_secs_f64(),
                ]);
            }
        }

        for (index, count) in COUNTS.iter().enumerate() {
            let median = |column: usize| {
                let mut values: Vec<f64> = rows[index].iter().map(|row| row[column]).collect();
                values.sort_by(f64::total_cmp);
                values[ROUNDS / 2] * 1e3
            };
            let (instances, glyphs) = shapes[index];
            println!(
                "{count:>7}  {:>10.3}  {:>10.3}  {:>10.3}  {:>10.3}  {instances:>9}  {glyphs:>7}",
                median(0),
                median(1),
                median(2),
                median(3),
            );
        }
    }

    println!("\n60 fps budget: 16.600 ms per frame");
}

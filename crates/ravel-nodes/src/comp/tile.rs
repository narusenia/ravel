// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.mirror` and `comp.tile`.
//!
//! One processor, one shader (`comp_tile.wgsl`); [`TileKind`] says which node
//! it is.
//!
//! - `comp.mirror` flips the image about the frame's centre (`mode`:
//!   `horizontal`, `vertical`, `both`). A flip is a texel copy, so it is exact
//!   and applying it twice returns the input. It is a flip, not a reflect-a-half
//!   kaleidoscope (not built).
//! - `comp.tile` repeats the image `columns` x `rows` times, each copy the whole
//!   image shrunk to fit. `columns` / `rows` are rounded to the nearest integer
//!   and kept within 1..=[`MAX_TILES`]; 1 x 1 is the input.
//!
//! **Border definition.** Tile *wraps*: a tap past the source edge reads the
//! opposite edge, so copies meet without a seam. Mirror has no border (it maps
//! the frame onto itself). Neither is affected by the preview scale: both are
//! defined in fractions of the frame.
//!
//! **Not shell nodes**: ordinary user-placed nodes; no `Document` access.

use bytemuck::Zeroable;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_tile.wgsl");

/// Upper bound of `columns` and `rows`.
pub const MAX_TILES: u32 = 256;

pub use ravel_core::registry::builtin::COMP_MIRROR_MODES as MIRROR_MODES;

/// Whether `mode` is a `comp.mirror` mode the processor understands.
pub fn comp_mirror_mode_is_known(mode: &str) -> bool {
    MIRROR_MODES.contains(&mode)
}

/// Which node a [`CompTileProcessor`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    Mirror,
    Tile,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
}

const TILE_MODE: u32 = 3;

fn tile_count(v: f32) -> u32 {
    if v.is_finite() {
        (v.round().max(1.0) as u32).min(MAX_TILES)
    } else {
        1
    }
}

impl TileKind {
    fn label(self) -> &'static str {
        match self {
            Self::Mirror => "comp.mirror",
            Self::Tile => "comp.tile",
        }
    }

    /// An unknown mirror `mode` is `None` (the image passes through).
    fn fill(self, p: &ResolvedParams) -> Option<Params> {
        let mut out = Params::zeroed();
        match self {
            Self::Mirror => {
                out.mode[0] = match p.str_or("mode", "horizontal") {
                    "horizontal" => 0,
                    "vertical" => 1,
                    "both" => 2,
                    _ => return None,
                };
            }
            Self::Tile => {
                out.mode = [
                    TILE_MODE,
                    tile_count(p.f32_or("columns", 1.0)),
                    tile_count(p.f32_or("rows", 1.0)),
                    0,
                ];
            }
        }
        Some(out)
    }
}

pub struct CompTileProcessor {
    kind: TileKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompTileProcessor {
    pub fn new(
        kind: TileKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        let source = gpu_util::with_premultiplied_helpers(SHADER_SRC);
        let pipeline = shaders
            .compute_pipeline(
                "comp_tile",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_tile.wgsl compilation failed");
        Self {
            kind,
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompTileProcessor {
    /// Nothing is captured from the node; every value is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };
        let label = self.kind.label();
        let Some(shader_params) = self.kind.fill(params) else {
            return Ok(input);
        };
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let input_binding = image.binding();
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label,
            pipeline: &self.pipeline,
            inputs: std::slice::from_ref(&input_binding),
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
        image.release(&self.pool);
        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            width,
            height,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::fx_test_util::*;
    use super::*;
    use ravel_core::eval::{Evaluator, ResolvedValue};
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameBuffer;

    fn run(
        gpu: &GpuContext,
        kind: TileKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
    ) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompTileProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        processor
            .process(
                &Node::new(NodeId::new(1), kind.label()),
                &ctx((input.width, input.height)),
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .expect("tile")
    }

    fn mirror(gpu: &GpuContext, mode: &str, input: &FrameBuffer) -> FrameBuffer {
        let out = run(
            gpu,
            TileKind::Mirror,
            &[("mode", ResolvedValue::Str(mode.into()))],
            input,
        );
        readback(out.as_ref())
    }

    fn tile(gpu: &GpuContext, columns: f32, rows: f32, input: &FrameBuffer) -> FrameBuffer {
        let out = run(
            gpu,
            TileKind::Tile,
            &[
                ("columns", ResolvedValue::Float(columns)),
                ("rows", ResolvedValue::Float(rows)),
            ],
            input,
        );
        readback(out.as_ref())
    }

    fn px(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        fb.as_f32()[((y * fb.width + x) * 4) as usize..][..4]
            .try_into()
            .unwrap()
    }

    fn premult(p: [f32; 4]) -> [f32; 4] {
        [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
    }

    #[test]
    fn mirroring_twice_returns_the_input_exactly() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        for mode in ["horizontal", "vertical", "both"] {
            let once = mirror(&gpu, mode, &input);
            assert_ne!(once.as_f32(), input.as_f32(), "{mode} does something");
            let twice = mirror(&gpu, mode, &once);
            assert_eq!(twice.as_f32(), input.as_f32(), "{mode} twice");
        }
    }

    #[test]
    fn mirror_modes_flip_the_named_axis() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(7, 4);
        let (w, h) = (7, 4);
        for (mode, flip_x, flip_y) in [
            ("horizontal", true, false),
            ("vertical", false, true),
            ("both", true, true),
        ] {
            let out = mirror(&gpu, mode, &input);
            for y in 0..h {
                for x in 0..w {
                    let sx = if flip_x { w - 1 - x } else { x };
                    let sy = if flip_y { h - 1 - y } else { y };
                    assert_eq!(px(&out, x, y), px(&input, sx, sy), "{mode} ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn tiling_one_by_one_is_the_input() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        let out = tile(&gpu, 1.0, 1.0, &input);
        for y in 0..5 {
            for x in 0..9 {
                let (g, w) = (premult(px(&out, x, y)), premult(px(&input, x, y)));
                for c in 0..4 {
                    assert!((g[c] - w[c]).abs() < 1e-5, "({x},{y}): {g:?} vs {w:?}");
                }
            }
        }
    }

    /// CPU reference of the tile definition: bilinear at
    /// ((x + 0.5) * columns, (y + 0.5) * rows) with wrap-around taps.
    #[test]
    fn tiling_matches_the_cpu_reference_and_repeats_with_wrap() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(8, 6);
        let (cols, rows) = (2u32, 3u32);
        let out = tile(&gpu, cols as f32, rows as f32, &input);
        let wrap = |i: i32, n: u32| i.rem_euclid(n as i32) as u32;
        for y in 0..6u32 {
            for x in 0..8u32 {
                let sx = (x as f32 + 0.5) * cols as f32 - 0.5;
                let sy = (y as f32 + 0.5) * rows as f32 - 0.5;
                let (x0, y0) = (sx.floor(), sy.floor());
                let (tx, ty) = (sx - x0, sy - y0);
                let mut acc = [0.0f32; 4];
                for (dx, dy, wgt) in [
                    (0, 0, (1.0 - tx) * (1.0 - ty)),
                    (1, 0, tx * (1.0 - ty)),
                    (0, 1, (1.0 - tx) * ty),
                    (1, 1, tx * ty),
                ] {
                    let p = premult(px(&input, wrap(x0 as i32 + dx, 8), wrap(y0 as i32 + dy, 6)));
                    for c in 0..4 {
                        acc[c] += wgt * p[c];
                    }
                }
                let got = premult(px(&out, x, y));
                for c in 0..4 {
                    assert!(
                        (got[c] - acc[c]).abs() < 2e-5,
                        "({x},{y}) {got:?} vs {acc:?}"
                    );
                }
            }
        }
        // 2 columns of an 8 wide frame: the right half is the left half.
        for y in 0..6 {
            for x in 0..4 {
                assert_eq!(px(&out, x, y), px(&out, x + 4, y), "({x},{y}) repeats");
            }
        }
    }

    #[test]
    fn tile_counts_are_rounded_and_bounded() {
        assert_eq!(tile_count(2.4), 2);
        assert_eq!(tile_count(2.6), 3);
        assert_eq!(tile_count(0.0), 1);
        assert_eq!(tile_count(-5.0), 1);
        assert_eq!(tile_count(f32::NAN), 1);
        assert_eq!(tile_count(1e9), MAX_TILES);
    }

    #[test]
    fn an_unknown_mirror_mode_passes_the_image_through() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(3, 3);
        let out = run(
            &gpu,
            TileKind::Mirror,
            &[("mode", ResolvedValue::Str("no_such_mode".into()))],
            &input,
        );
        assert_eq!(
            out.downcast_ref::<FrameBuffer>()
                .expect("passed through")
                .as_f32(),
            input.as_f32()
        );
        assert!(!comp_mirror_mode_is_known("no_such_mode"));
        assert!(MIRROR_MODES.iter().all(|m| comp_mirror_mode_is_known(m)));
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The generator nodes: `comp.gradient`, `comp.noise`, `comp.fractal`,
//! `comp.checkerboard`. None has an input; like `comp.solid` the frame is
//! [`EvalContext::resolution`].
//!
//! One processor, one shader (`comp_generate.wgsl`), one uniform.
//! [`GenerateKind`] says which node a [`CompGenerateProcessor`] is and [`fill`]
//! is the only place that reads a node's parameters into the uniform.
//!
//! **Resolution independence.** A position is either a frame fraction
//! (`start` / `end` of the gradient, the fractal's view, which spans a fixed
//! number of plane units over the frame height) or a composition-pixel length
//! multiplied by [`composition_scale`] (noise `scale` / `offset`, checkerboard
//! `size` / `offset`). A preview at half resolution therefore shows the same
//! picture, and a 2x render the same picture sharper.
//!
//! - `comp.gradient`: `type` `linear` projects the pixel onto `start`..`end`;
//!   `radial` is the distance to `start` over the distance to `end`. The
//!   position indexes the `stops` [`RampParam`], which is **baked on the CPU**
//!   (`RampParam::evaluate` at `i / 255`) into a table the shader interpolates,
//!   as `comp.curves` does with curves: one evaluation implementation, every
//!   interpolation mode honoured. Outside `0..1` the ramp clamps. A
//!   `constant`-interpolation ramp has its step softened over 1/255 of the span.
//! - `comp.noise`: Perlin-style gradient noise (quintic fade, hashed unit
//!   gradients), `octaves` summed at doubling frequency with amplitude scaled by
//!   `roughness` each time, normalised by the amplitude sum, then
//!   `mix(color_a, color_b, 0.5 + 0.5 n)`. The integer hash mixes `seed` (and
//!   the octave), so one seed is one picture. It shares nothing with
//!   `field.noise` and is not parameter-compatible with it. `octaves` and
//!   `seed` are Floats (so they animate) rounded to integers.
//! - `comp.fractal`: `mandelbrot`, or `julia` with the constant `julia`; the
//!   view is `center` with `zoom` 1 showing 3 plane units top to bottom.
//!   Escape time (radius 16, smoothed) over `iterations`, as `sqrt(mu / cap)`
//!   into the `stops` ramp; a point that never escapes is `inside`.
//!   ponytail: f32 arithmetic, so a zoom past ~1e4 turns blocky; go to
//!   emulated double precision then.
//! - `comp.checkerboard`: cell `(floor((p - offset) / size))`, `color_a` where
//!   the cell index sum is even.
//!
//! Colours are written as given (frames carry straight alpha).
//!
//! **Not shell nodes**: ordinary user-placed nodes; no `Document` access.

use bytemuck::Zeroable;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::param_ramp::RampParam;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use crate::composition_scale;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_generate.wgsl");

/// Entries in the baked ramp table (the array length in the shader too).
const TABLE_LEN: usize = 256;

pub use ravel_core::registry::builtin::{
    COMP_FRACTAL_TYPES as FRACTAL_TYPES, COMP_GRADIENT_TYPES as GRADIENT_TYPES,
};

/// Whether `type_` is a `comp.gradient` type the processor understands.
pub fn comp_gradient_type_is_known(type_: &str) -> bool {
    GRADIENT_TYPES.contains(&type_)
}

/// Whether `type_` is a `comp.fractal` type the processor understands.
pub fn comp_fractal_type_is_known(type_: &str) -> bool {
    FRACTAL_TYPES.contains(&type_)
}

/// Which node a [`CompGenerateProcessor`] is. The discriminant is the shader's
/// `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerateKind {
    Gradient = 0,
    Noise = 1,
    Fractal = 2,
    Checkerboard = 3,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
    d: [f32; 4],
    table: [[f32; 4]; TABLE_LEN],
}

impl GenerateKind {
    fn label(self) -> &'static str {
        match self {
            Self::Gradient => "comp.gradient",
            Self::Noise => "comp.noise",
            Self::Fractal => "comp.fractal",
            Self::Checkerboard => "comp.checkerboard",
        }
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn finite2(v: [f32; 2]) -> [f32; 2] {
    [finite(v[0]), finite(v[1])]
}

/// A rounded, non-negative count in `1..=max` (a NaN is the minimum).
fn count(v: f32, max: u32) -> u32 {
    (finite(v).round().max(1.0) as u32).min(max)
}

/// Bake `ramp` at `i / 255` into the table.
fn bake(table: &mut [[f32; 4]; TABLE_LEN], ramp: &RampParam) {
    for (i, entry) in table.iter_mut().enumerate() {
        let c = ramp.evaluate(i as f32 / (TABLE_LEN - 1) as f32);
        *entry = [c.r, c.g, c.b, c.a];
    }
}

/// Read the node's parameters into the uniform for a `size` frame. an unknown `type` is `None` (the node then produces a transparent frame).
fn fill(
    kind: GenerateKind,
    p: &ResolvedParams,
    ctx: &EvalContext,
    size: (u32, u32),
) -> Option<Params> {
    let (sx, sy) = composition_scale(ctx);
    let (sx, sy) = (sx as f32, sy as f32);
    // Isotropic quantities (a noise feature size) use the geometric mean.
    let s = (sx * sy).sqrt();
    let (w, h) = (size.0 as f32, size.1 as f32);
    let mut out = Params::zeroed();
    out.mode[0] = kind as u32;
    match kind {
        GenerateKind::Gradient => {
            out.mode[3] = match p.str_or("type", "linear") {
                "linear" => 0,
                "radial" => 1,
                _ => return None,
            };
            let start = finite2(p.vec2_or("start", [0.0, 0.5]));
            let end = finite2(p.vec2_or("end", [1.0, 0.5]));
            out.a = [start[0] * w, start[1] * h, end[0] * w, end[1] * h];
            bake(
                &mut out.table,
                &p.ramp("stops").cloned().unwrap_or_default(),
            );
        }
        GenerateKind::Noise => {
            let offset = finite2(p.vec2_or("offset", [0.0, 0.0]));
            out.mode[1] = count(p.f32_or("octaves", 4.0), 8);
            out.mode[2] = finite(p.f32_or("seed", 0.0)).round().max(0.0) as u32;
            out.a = [
                (finite(p.f32_or("scale", 100.0)) * s).max(1e-3),
                finite(p.f32_or("roughness", 0.5)).clamp(0.0, 1.0),
                offset[0] * sx,
                offset[1] * sy,
            ];
            out.c = p.vec4_or("color_a", [0.0, 0.0, 0.0, 1.0]);
            out.d = p.vec4_or("color_b", [1.0, 1.0, 1.0, 1.0]);
        }
        GenerateKind::Fractal => {
            out.mode[3] = match p.str_or("type", "mandelbrot") {
                "mandelbrot" => 0,
                "julia" => 1,
                _ => return None,
            };
            out.mode[1] = count(p.f32_or("iterations", 64.0), 1000);
            let center = finite2(p.vec2_or("center", [-0.5, 0.0]));
            let zoom = finite(p.f32_or("zoom", 1.0)).max(1e-3);
            out.a = [center[0], center[1], 3.0 / zoom, 0.0];
            let julia = finite2(p.vec2_or("julia", [-0.8, 0.156]));
            out.b = [julia[0], julia[1], 0.0, 0.0];
            out.c = p.vec4_or("inside", [0.0, 0.0, 0.0, 1.0]);
            bake(
                &mut out.table,
                &p.ramp("stops").cloned().unwrap_or_default(),
            );
        }
        GenerateKind::Checkerboard => {
            let size = finite2(p.vec2_or("size", [50.0, 50.0]));
            let offset = finite2(p.vec2_or("offset", [0.0, 0.0]));
            out.a = [(size[0] * sx).max(1e-3), (size[1] * sy).max(1e-3), 0.0, 0.0];
            out.b = [offset[0] * sx, offset[1] * sy, 0.0, 0.0];
            out.c = p.vec4_or("color_a", [1.0, 1.0, 1.0, 1.0]);
            out.d = p.vec4_or("color_b", [0.0, 0.0, 0.0, 1.0]);
        }
    }
    Some(out)
}

pub struct CompGenerateProcessor {
    kind: GenerateKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompGenerateProcessor {
    pub fn new(
        kind: GenerateKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        // No input texture: the output is binding 0, the uniform binding 1.
        let layout = [
            gpu_util::output_storage_layout_entry(0),
            gpu_util::uniform_layout_entry(1),
        ];
        // Shared by every kind: they differ only in the uniform.
        let pipeline = shaders
            .compute_pipeline(
                "comp_generate",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_generate.wgsl compilation failed");
        Self {
            kind,
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompGenerateProcessor {
    /// Nothing is captured from the node; every value is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let label = self.kind.label();
        let (width, height) = ctx.resolution;
        anyhow::ensure!(
            width > 0 && height > 0,
            "{label}: evaluation resolution {width}x{height} is empty"
        );
        let Some(shader_params) = fill(self.kind, params, ctx, (width, height)) else {
            return Ok(super::transparent(ctx));
        };
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label,
            pipeline: &self.pipeline,
            inputs: &[],
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
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
    use ravel_core::param_ramp::RampInterpolation;
    use ravel_core::types::{Color, FrameBuffer};

    fn f(v: f32) -> ResolvedValue {
        ResolvedValue::Float(v)
    }

    fn s(v: &str) -> ResolvedValue {
        ResolvedValue::Str(v.into())
    }

    fn run_in(
        gpu: &GpuContext,
        kind: GenerateKind,
        values: &[(&str, ResolvedValue)],
        ctx: &EvalContext,
    ) -> FrameBuffer {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompGenerateProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        let out = processor
            .process(
                &Node::new(NodeId::new(1), kind.label()),
                ctx,
                &[],
                &params,
                &mut Evaluator::new(),
            )
            .expect("generate");
        readback(out.as_ref())
    }

    fn run(
        gpu: &GpuContext,
        kind: GenerateKind,
        values: &[(&str, ResolvedValue)],
        size: (u32, u32),
    ) -> FrameBuffer {
        run_in(gpu, kind, values, &ctx(size))
    }

    /// A context rendering a `comp`-sized composition at `size`.
    fn scaled(size: (u32, u32), comp: (u32, u32)) -> EvalContext {
        let mut c = ctx(size);
        c.comp_resolution = comp;
        c
    }

    fn px(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        fb.as_f32()[((y * fb.width + x) * 4) as usize..][..4]
            .try_into()
            .unwrap()
    }

    /// The mean of the `k` x `k` block of `fb` at block `(bx, by)`.
    fn block_mean(fb: &FrameBuffer, bx: u32, by: u32, k: u32) -> [f32; 4] {
        let mut acc = [0.0; 4];
        for y in 0..k {
            for x in 0..k {
                let p = px(fb, bx * k + x, by * k + y);
                for c in 0..4 {
                    acc[c] += p[c] / (k * k) as f32;
                }
            }
        }
        acc
    }

    fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    /// The same composition content at `comp` and at twice that: every block of
    /// the large frame averages to the small frame's pixel, to `tol`.
    fn assert_scales(
        gpu: &GpuContext,
        kind: GenerateKind,
        values: &[(&str, ResolvedValue)],
        tol: f32,
        what: &str,
    ) {
        let comp = (16, 16);
        let small = run_in(gpu, kind, values, &scaled(comp, comp));
        let big = run_in(gpu, kind, values, &scaled((32, 32), comp));
        assert_eq!((big.width, big.height), (32, 32));
        for y in 0..16 {
            for x in 0..16 {
                let (m, p) = (block_mean(&big, x, y, 2), px(&small, x, y));
                assert!(close(m, p, tol), "{what}: ({x},{y}) {m:?} vs {p:?}");
            }
        }
    }

    const RED: Color = Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };

    fn ramp3() -> ResolvedValue {
        ResolvedValue::Ramp(RampParam::linear([
            (0.0, RED),
            (0.5, Color::WHITE),
            (1.0, BLUE),
        ]))
    }

    // ---- gradient --------------------------------------------------------

    /// CPU reference: the ramp evaluated at the pixel's projection, straight
    /// from the module docs.
    fn gradient_reference(
        ramp: &RampParam,
        size: (u32, u32),
        start: [f32; 2],
        end: [f32; 2],
        radial: bool,
    ) -> Vec<f32> {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let s = [start[0] * w, start[1] * h];
        let v = [end[0] * w - s[0], end[1] * h - s[1]];
        let mut out = Vec::new();
        for y in 0..size.1 {
            for x in 0..size.0 {
                let d = [x as f32 + 0.5 - s[0], y as f32 + 0.5 - s[1]];
                let len2 = v[0] * v[0] + v[1] * v[1];
                let t = if radial {
                    (d[0] * d[0] + d[1] * d[1]).sqrt() / len2.sqrt()
                } else {
                    (d[0] * v[0] + d[1] * v[1]) / len2
                };
                let c = ramp.evaluate(t);
                out.extend([c.r, c.g, c.b, c.a]);
            }
        }
        out
    }

    #[test]
    fn linear_and_radial_gradients_match_the_ramp_evaluated_on_the_cpu() {
        let Some(gpu) = gpu_or_skip() else { return };
        let ramp = RampParam::linear([(0.0, RED), (0.5, Color::WHITE), (1.0, BLUE)]);
        let (start, end) = ([0.1, 0.2], [0.9, 0.7]);
        for (type_, radial) in [("linear", false), ("radial", true)] {
            let out = run(
                &gpu,
                GenerateKind::Gradient,
                &[
                    ("type", s(type_)),
                    ("start", ResolvedValue::Vec2(start)),
                    ("end", ResolvedValue::Vec2(end)),
                    ("stops", ResolvedValue::Ramp(ramp.clone())),
                ],
                (24, 16),
            );
            let want = gradient_reference(&ramp, (24, 16), start, end, radial);
            for (i, (g, w)) in out.as_f32().iter().zip(&want).enumerate() {
                // The table is 256 entries of a piecewise-linear ramp: exact
                // up to float error, except within 1/255 of a stop's kink
                // (a slope-2 ramp is off by up to ~0.008 there).
                assert!((g - w).abs() < 1.2e-2, "{type_}: element {i}: {g} vs {w}");
            }
        }
    }

    #[test]
    fn a_gradient_reaches_each_stop_colour_and_clamps_beyond_the_ends() {
        let Some(gpu) = gpu_or_skip() else { return };
        // Start and end on pixel centres of a 11 x 1 frame.
        let out = run(
            &gpu,
            GenerateKind::Gradient,
            &[
                ("start", ResolvedValue::Vec2([0.5 / 11.0, 0.5])),
                ("end", ResolvedValue::Vec2([10.5 / 11.0, 0.5])),
                ("stops", ramp3()),
            ],
            (11, 1),
        );
        assert!(close(px(&out, 0, 0), [1.0, 0.0, 0.0, 1.0], 1e-4));
        assert!(
            close(px(&out, 5, 0), [1.0, 1.0, 1.0, 1.0], 1e-2),
            "the middle stop, to the table's resolution"
        );
        assert!(close(px(&out, 10, 0), [0.0, 0.0, 1.0, 1.0], 1e-4));
        // The line from start to end covers only a sub-span: outside it the
        // end colours hold.
        let out = run(
            &gpu,
            GenerateKind::Gradient,
            &[
                ("start", ResolvedValue::Vec2([0.4, 0.5])),
                ("end", ResolvedValue::Vec2([0.6, 0.5])),
                ("stops", ramp3()),
            ],
            (20, 1),
        );
        assert!(close(px(&out, 0, 0), [1.0, 0.0, 0.0, 1.0], 1e-6));
        assert!(close(px(&out, 19, 0), [0.0, 0.0, 1.0, 1.0], 1e-6));
    }

    #[test]
    fn a_gradient_keeps_the_alpha_of_its_stops() {
        let Some(gpu) = gpu_or_skip() else { return };
        let ramp = RampParam::linear([
            (0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
            (1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
        ]);
        let out = run(
            &gpu,
            GenerateKind::Gradient,
            &[("stops", ResolvedValue::Ramp(ramp))],
            (4, 1),
        );
        assert!(px(&out, 0, 0)[3] > 0.8 && px(&out, 3, 0)[3] < 0.2);
    }

    /// A smooth interpolation changes the middle, which only the CPU baking can
    /// know about: a shader that interpolated stops itself would ignore it.
    #[test]
    fn the_ramp_interpolation_mode_is_honoured() {
        let Some(gpu) = gpu_or_skip() else { return };
        let stops = |mode| {
            ResolvedValue::Ramp(
                RampParam::linear([(0.0, Color::BLACK), (1.0, Color::WHITE)])
                    .with_interpolation(mode),
            )
        };
        let at = |mode| {
            run(
                &gpu,
                GenerateKind::Gradient,
                &[
                    ("start", ResolvedValue::Vec2([0.0, 0.5])),
                    ("end", ResolvedValue::Vec2([1.0, 0.5])),
                    ("stops", stops(mode)),
                ],
                (8, 1),
            )
        };
        let (linear, smooth, constant) = (
            at(RampInterpolation::Linear),
            at(RampInterpolation::Smooth),
            at(RampInterpolation::Constant),
        );
        // t = 1.5 / 8 on the second pixel.
        let t = 1.5 / 8.0f32;
        assert!((px(&linear, 1, 0)[0] - t).abs() < 2e-3);
        assert!((px(&smooth, 1, 0)[0] - t * t * (3.0 - 2.0 * t)).abs() < 2e-3);
        assert!(px(&constant, 6, 0)[0] < 0.01, "a constant ramp holds black");
    }

    #[test]
    fn a_gradient_is_resolution_independent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let values = [
            ("start", ResolvedValue::Vec2([0.0, 0.0])),
            ("end", ResolvedValue::Vec2([1.0, 1.0])),
        ];
        // A linear ramp's 2 x 2 block average is its value at the centre.
        assert_scales(&gpu, GenerateKind::Gradient, &values, 2e-3, "linear");
    }

    #[test]
    fn an_unknown_gradient_type_is_transparent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompGenerateProcessor::new(
            GenerateKind::Gradient,
            gpu.clone(),
            &mut shaders,
            pool(&gpu),
        );
        let mut params = ResolvedParams::default();
        params.set("type", s("no_such_type"));
        let out = processor
            .process(
                &Node::new(NodeId::new(1), "comp.gradient"),
                &ctx((3, 3)),
                &[],
                &params,
                &mut Evaluator::new(),
            )
            .unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().expect("a CPU frame");
        assert_eq!((fb.width, fb.height), (3, 3));
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
        assert!(!comp_gradient_type_is_known("no_such_type"));
        assert!(
            GRADIENT_TYPES
                .iter()
                .all(|t| comp_gradient_type_is_known(t))
        );
        assert!(FRACTAL_TYPES.iter().all(|t| comp_fractal_type_is_known(t)));
    }

    // ---- noise -----------------------------------------------------------

    fn noise(seed: f32) -> Vec<(&'static str, ResolvedValue)> {
        vec![("scale", f(40.0)), ("octaves", f(3.0)), ("seed", f(seed))]
    }

    #[test]
    fn noise_and_fractal_are_deterministic_for_a_fixed_seed() {
        let Some(gpu) = gpu_or_skip() else { return };
        let a = run(&gpu, GenerateKind::Noise, &noise(7.0), (32, 24));
        let b = run(&gpu, GenerateKind::Noise, &noise(7.0), (32, 24));
        assert_eq!(a.as_f32(), b.as_f32(), "same seed, same picture");
        let other = run(&gpu, GenerateKind::Noise, &noise(8.0), (32, 24));
        assert_ne!(a.as_f32(), other.as_f32(), "another seed, another picture");
        // The picture is not flat, and stays inside the two colours.
        let v: Vec<f32> = a.as_f32().chunks_exact(4).map(|p| p[0]).collect();
        let (lo, hi) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
        assert!(hi - lo > 0.3, "range {lo}..{hi}");
        assert!(lo >= 0.0 && hi <= 1.0);

        let fractal = [("zoom", f(1.0)), ("iterations", f(40.0))];
        let a = run(&gpu, GenerateKind::Fractal, &fractal, (32, 24));
        let b = run(&gpu, GenerateKind::Fractal, &fractal, (32, 24));
        assert_eq!(a.as_f32(), b.as_f32());
    }

    #[test]
    fn noise_follows_scale_octaves_and_offset() {
        let Some(gpu) = gpu_or_skip() else { return };
        let base = run(&gpu, GenerateKind::Noise, &noise(1.0), (32, 32));
        // Another feature size changes it; a shift by `offset` shifts it.
        let mut bigger = noise(1.0);
        bigger[0] = ("scale", f(80.0));
        assert_ne!(
            base.as_f32(),
            run(&gpu, GenerateKind::Noise, &bigger, (32, 32)).as_f32()
        );
        let mut shifted = noise(1.0);
        shifted.push(("offset", ResolvedValue::Vec2([4.0, 0.0])));
        let shifted = run(&gpu, GenerateKind::Noise, &shifted, (32, 32));
        // A positive offset moves the pattern right (the sample point is p - offset).
        for y in 0..32 {
            for x in 4..32 {
                assert!(
                    close(px(&shifted, x, y), px(&base, x - 4, y), 1e-3),
                    "({x},{y})"
                );
            }
        }
        // One octave is smoother than four: less variation between neighbours.
        let rough = |octaves: f32| {
            let mut v = noise(1.0);
            v[0] = ("scale", f(100.0));
            v[1] = ("octaves", f(octaves));
            v.push(("roughness", f(0.8)));
            let fb = run(&gpu, GenerateKind::Noise, &v, (32, 32));
            (0..31)
                .map(|x| (px(&fb, x, 5)[0] - px(&fb, x + 1, 5)[0]).abs())
                .sum::<f32>()
        };
        assert!(rough(1.0) < rough(6.0));
    }

    #[test]
    fn noise_maps_between_its_two_colours() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut v = noise(3.0);
        v.push(("color_a", ResolvedValue::Vec4([1.0, 0.0, 0.0, 1.0])));
        v.push(("color_b", ResolvedValue::Vec4([0.0, 0.0, 1.0, 1.0])));
        let out = run(&gpu, GenerateKind::Noise, &v, (16, 16));
        for p in out.as_f32().chunks_exact(4) {
            assert!((p[0] + p[2] - 1.0).abs() < 1e-5 && p[1] == 0.0 && p[3] == 1.0);
        }
    }

    #[test]
    fn noise_is_resolution_independent() {
        let Some(gpu) = gpu_or_skip() else { return };
        // A smooth, large-feature noise: the 2 x 2 average is close to the
        // value at the pixel centre.
        let values = [("scale", f(64.0)), ("octaves", f(1.0)), ("seed", f(5.0))];
        assert_scales(&gpu, GenerateKind::Noise, &values, 0.02, "noise");
    }

    // ---- fractal ---------------------------------------------------------

    #[test]
    fn the_mandelbrot_set_is_inside_and_far_points_escape() {
        let Some(gpu) = gpu_or_skip() else { return };
        let inside = [0.0, 0.0, 1.0, 1.0];
        // Centre 0 + 0i is in the set; centre 1 + 1i escapes at once.
        let values = |re: f32, im: f32| {
            [
                ("center", ResolvedValue::Vec2([re, im])),
                ("zoom", f(1000.0)),
                ("iterations", f(50.0)),
                ("inside", ResolvedValue::Vec4(inside)),
            ]
        };
        let near = run(&gpu, GenerateKind::Fractal, &values(0.0, 0.0), (5, 5));
        assert!(close(px(&near, 2, 2), inside, 1e-6));
        let far = run(&gpu, GenerateKind::Fractal, &values(1.0, 1.0), (5, 5));
        assert!(!close(px(&far, 2, 2), inside, 0.01), "{:?}", px(&far, 2, 2));
        // The default view shows both.
        let both = run(&gpu, GenerateKind::Fractal, &[("zoom", f(1.0))], (48, 32));
        let blackish = both
            .as_f32()
            .chunks_exact(4)
            .filter(|p| p[0] < 1e-6)
            .count();
        assert!(blackish > 50 && blackish < 48 * 32 - 50, "{blackish}");
    }

    #[test]
    fn julia_uses_its_constant_and_differs_from_mandelbrot() {
        let Some(gpu) = gpu_or_skip() else { return };
        let julia = |c: [f32; 2]| {
            run(
                &gpu,
                GenerateKind::Fractal,
                &[
                    ("type", s("julia")),
                    ("center", ResolvedValue::Vec2([0.0, 0.0])),
                    ("julia", ResolvedValue::Vec2(c)),
                ],
                (24, 24),
            )
        };
        assert_ne!(julia([-0.8, 0.156]).as_f32(), julia([0.3, 0.5]).as_f32());
        let mandel = run(
            &gpu,
            GenerateKind::Fractal,
            &[("center", ResolvedValue::Vec2([0.0, 0.0]))],
            (24, 24),
        );
        assert_ne!(julia([-0.8, 0.156]).as_f32(), mandel.as_f32());
    }

    #[test]
    fn a_fractal_is_resolution_independent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let comp = (24, 24);
        let values: &[(&str, ResolvedValue)] = &[("zoom", f(1.0)), ("iterations", f(30.0))];
        let small = run_in(&gpu, GenerateKind::Fractal, values, &scaled(comp, comp));
        let big = run_in(&gpu, GenerateKind::Fractal, values, &scaled((48, 48), comp));
        // The set's boundary is a fractal: demand the pictures agree on most
        // blocks rather than all.
        let agree = (0..24)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| close(block_mean(&big, x, y, 2), px(&small, x, y), 0.15))
            .count();
        assert!(agree >= 24 * 24 * 85 / 100, "{agree} of {} agree", 24 * 24);
    }

    // ---- checkerboard ----------------------------------------------------

    #[test]
    fn a_checkerboard_alternates_its_colours_by_cell() {
        let Some(gpu) = gpu_or_skip() else { return };
        let (a, b) = ([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
        let out = run(
            &gpu,
            GenerateKind::Checkerboard,
            &[
                ("size", ResolvedValue::Vec2([2.0, 3.0])),
                ("color_a", ResolvedValue::Vec4(a)),
                ("color_b", ResolvedValue::Vec4(b)),
            ],
            (8, 9),
        );
        for y in 0..9u32 {
            for x in 0..8u32 {
                let even = (x / 2 + y / 3) % 2 == 0;
                assert_eq!(px(&out, x, y), if even { a } else { b }, "({x},{y})");
            }
        }
        // `offset` slides the cells.
        let out = run(
            &gpu,
            GenerateKind::Checkerboard,
            &[
                ("size", ResolvedValue::Vec2([2.0, 2.0])),
                ("offset", ResolvedValue::Vec2([1.0, 0.0])),
                ("color_a", ResolvedValue::Vec4(a)),
                ("color_b", ResolvedValue::Vec4(b)),
            ],
            (4, 2),
        );
        assert_eq!(px(&out, 0, 0), b, "x = 0 is in the cell left of the offset");
        assert_eq!(px(&out, 1, 0), a);
        assert_eq!(px(&out, 3, 0), b);
    }

    #[test]
    fn a_checkerboard_is_resolution_independent() {
        let Some(gpu) = gpu_or_skip() else { return };
        // 4 comp px cells: every 2 x 2 block of the double-size frame sits in
        // one cell, so the match is exact.
        let values = [
            ("size", ResolvedValue::Vec2([4.0, 4.0])),
            ("offset", ResolvedValue::Vec2([2.0, 0.0])),
        ];
        assert_scales(&gpu, GenerateKind::Checkerboard, &values, 0.0, "checker");
    }

    #[test]
    fn the_size_follows_the_evaluation_resolution_and_an_empty_one_errors() {
        let Some(gpu) = gpu_or_skip() else { return };
        for size in [(1, 1), (4, 9), (16, 2)] {
            let fb = run(&gpu, GenerateKind::Checkerboard, &[], size);
            assert_eq!((fb.width, fb.height), size);
        }
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor =
            CompGenerateProcessor::new(GenerateKind::Noise, gpu.clone(), &mut shaders, pool(&gpu));
        assert!(
            processor
                .process(
                    &Node::new(NodeId::new(1), "comp.noise"),
                    &ctx((0, 4)),
                    &[],
                    &ResolvedParams::default(),
                    &mut Evaluator::new(),
                )
                .is_err()
        );
    }
}

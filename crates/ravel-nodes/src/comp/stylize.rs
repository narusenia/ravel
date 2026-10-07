// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The stylize nodes: `comp.glow`, `comp.drop_shadow`, `comp.stroke`,
//! `comp.emboss`.
//!
//! One processor, one shader (`comp_stylize.wgsl`), one uniform.
//! [`StylizeKind`] says which node a [`CompStylizeProcessor`] is and [`fill`] is
//! the only place that reads a node's parameters into the uniform.
//!
//! All four read neighbouring pixels, so they work in **premultiplied** alpha
//! (GPUCOMP-4) and convert back to straight alpha. Where a blur is needed
//! (glow, shadow) the existing [`BlurProcessor`] is called, as `comp.sharpen`
//! does; the caller multiplies the radius by [`composition_scale`] because
//! `blur` itself does not. Every length is in *composition* pixels and gets the
//! same scale, so a half-resolution preview shows the same picture. A tap
//! outside the image clamps to the edge texel, except the shadow's offset read,
//! which is transparent outside (a shadow may leave the frame).
//!
//! With `B` the blurred copy and `P` the premultiplied pixel:
//!
//! - `comp.glow`: `P + intensity * tint * B` (additive; alpha
//!   `A + intensity * B.a * (1 - A)`, at most 1). `intensity` is a plain number
//!   so it can exceed 1; `color` only tints. A transparent input stays
//!   transparent (`B` is 0 everywhere). There is no brightness threshold: the
//!   whole image glows.
//! - `comp.drop_shadow`: coverage `S = opacity * B.a(p - offset)`; the colour
//!   `color.rgb` is drawn behind the image: alpha `A + S (1 - A)`. The colour's
//!   own alpha is ignored. A transparent input has no shadow.
//! - `comp.stroke`: a ring of `width` px around the alpha shape. `outside`
//!   dilates: `E = max_o A(p+o) w(|o|)` over the disc, `w(d) = clamp(width +
//!   0.5 - d, 0, 1)` (a one-pixel anti-aliased edge), and the stroke colour is
//!   drawn behind the image with coverage `max(E - A, 0)` times the colour's
//!   alpha, so the output alpha grows to `E`. `inside` erodes (`E = min_o 1 -
//!   (1 - A(p+o)) w(|o|)`) and the colour replaces the image's own where the
//!   erosion took coverage away, alpha unchanged. **Width 0 returns the input
//!   exactly**: every weight is at most 0.5 and both rings are clamped at 0.
//!   ponytail: a brute-force `(2r + 1)^2` search, `r` capped at [`MAX_REACH`]
//!   device pixels (a wider stroke is clipped to that); a distance transform
//!   (jump flooding) when it is too slow.
//! - `comp.emboss`: height `h = luma(P.rgb) + A`; the output colour is
//!   `rgb + amount * (h(p - d L) - h(p + d L))`, `L` the unit vector toward
//!   the light (`angle` degrees, y down, 0 = right) and `d` the `distance`
//!   (px). Flat areas are unchanged, `amount` 0 is the identity, alpha is kept
//!   and a transparent pixel keeps what it stores.
//!
//! **Not shell nodes**: ordinary user-placed nodes; no `Document` access.

use bytemuck::Zeroable;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::graph::Node;
use ravel_core::id::NodeId;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::blur::BlurProcessor;
use crate::composition_scale;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_stylize.wgsl");

/// Cap on the stroke's search radius, in device pixels.
const MAX_REACH: u32 = 48;

pub use ravel_core::registry::builtin::COMP_STROKE_POSITIONS as STROKE_POSITIONS;

/// Whether `position` is a `comp.stroke` position the processor understands.
pub fn comp_stroke_position_is_known(position: &str) -> bool {
    STROKE_POSITIONS.contains(&position)
}

/// Which node a [`CompStylizeProcessor`] is. The discriminant is the shader's
/// `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StylizeKind {
    Glow = 0,
    DropShadow = 1,
    Stroke = 2,
    Emboss = 3,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
    a: [f32; 4],
    color: [f32; 4],
}

impl StylizeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Glow => "comp.glow",
            Self::DropShadow => "comp.drop_shadow",
            Self::Stroke => "comp.stroke",
            Self::Emboss => "comp.emboss",
        }
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// Isotropic scale for a length with no direction.
fn iso_scale(ctx: &EvalContext) -> f32 {
    let (sx, sy) = composition_scale(ctx);
    (sx * sy).sqrt() as f32
}

/// The blur radius the node hands the `blur` processor, in device px; `None`
/// for the kinds that do not blur.
fn blur_radius(kind: StylizeKind, p: &ResolvedParams, ctx: &EvalContext) -> Option<f32> {
    let key = match kind {
        StylizeKind::Glow => ("radius", 20.0),
        StylizeKind::DropShadow => ("softness", 5.0),
        _ => return None,
    };
    Some(finite(p.f32_or(key.0, key.1)).max(0.0) * iso_scale(ctx))
}

/// Read the node's parameters into the uniform. An unknown stroke `position`
/// is `None` (the image passes through).
fn fill(kind: StylizeKind, p: &ResolvedParams, ctx: &EvalContext) -> Option<Params> {
    let (sx, sy) = composition_scale(ctx);
    let (sx, sy) = (sx as f32, sy as f32);
    let s = iso_scale(ctx);
    let mut out = Params::zeroed();
    out.mode[0] = kind as u32;
    match kind {
        StylizeKind::Glow => {
            out.a[0] = finite(p.f32_or("intensity", 1.0)).max(0.0);
            out.color = p.vec4_or("color", [1.0; 4]);
        }
        StylizeKind::DropShadow => {
            let offset = p.vec2_or("offset", [5.0, 5.0]);
            out.a = [
                finite(offset[0]) * sx,
                finite(offset[1]) * sy,
                finite(p.f32_or("opacity", 0.75)).clamp(0.0, 1.0),
                0.0,
            ];
            out.color = p.vec4_or("color", [0.0, 0.0, 0.0, 1.0]);
        }
        StylizeKind::Stroke => {
            out.mode[2] = match p.str_or("position", "outside") {
                "outside" => 0,
                "inside" => 1,
                _ => return None,
            };
            let width = finite(p.f32_or("width", 2.0)).max(0.0) * s;
            out.mode[1] = ((width + 0.5).ceil() as u32).min(MAX_REACH);
            out.a[0] = width;
            out.color = p.vec4_or("color", [1.0; 4]);
        }
        StylizeKind::Emboss => {
            let (sin, cos) = finite(p.f32_or("angle", 225.0)).to_radians().sin_cos();
            let step = finite(p.f32_or("distance", 1.0)).max(0.0) * s;
            out.a = [cos * step, sin * step, finite(p.f32_or("amount", 1.0)), 0.0];
        }
    }
    Some(out)
}

pub struct CompStylizeProcessor {
    kind: StylizeKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
    /// Glow and shadow blur through the existing blur node.
    blur: Option<BlurProcessor>,
}

impl CompStylizeProcessor {
    pub fn new(
        kind: StylizeKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry(2),
            gpu_util::uniform_layout_entry(3),
        ];
        let source = gpu_util::with_premultiplied_helpers(SHADER_SRC);
        // Shared by every kind: they differ only in the uniform.
        let pipeline = shaders
            .compute_pipeline(
                "comp_stylize",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_stylize.wgsl compilation failed");
        let blur = matches!(kind, StylizeKind::Glow | StylizeKind::DropShadow).then(|| {
            BlurProcessor::new(
                ctx.clone(),
                shaders,
                pool.clone(),
                &Node::new(NodeId::new(0), "blur"),
            )
        });
        Self {
            kind,
            ctx,
            pipeline,
            pool,
            blur,
        }
    }
}

impl NodeProcessor for CompStylizeProcessor {
    /// Nothing is captured from the node; every value is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let label = self.kind.label();
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };
        let Some(shader_params) = fill(self.kind, params, ctx) else {
            return Ok(input);
        };
        // The second texture slot: the blurred copy, or (never sampled) the
        // image itself.
        let aux: Option<Arc<dyn NodeData>> = match (&self.blur, blur_radius(self.kind, params, ctx))
        {
            (Some(blur), Some(radius)) => {
                let mut blur_params = ResolvedParams::default();
                blur_params.set("radius", ResolvedValue::Float(radius));
                Some(blur.process(node, ctx, &[Some(input.clone())], &blur_params, scope)?)
            }
            _ => None,
        };

        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let aux_image = aux
            .as_ref()
            .map(|a| gpu_util::ensure_gpu(&self.ctx, &self.pool, a.as_ref()))
            .transpose()
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let bindings = [
            image.binding(),
            aux_image
                .as_ref()
                .map_or_else(|| image.binding(), |a| a.binding()),
        ];
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label,
            pipeline: &self.pipeline,
            inputs: &bindings,
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
        image.release(&self.pool);
        if let Some(a) = aux_image {
            a.release(&self.pool);
        }
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
    use ravel_core::eval::Evaluator;
    use ravel_core::types::FrameBuffer;

    fn f(v: f32) -> ResolvedValue {
        ResolvedValue::Float(v)
    }

    fn c4(c: [f32; 4]) -> ResolvedValue {
        ResolvedValue::Vec4(c)
    }

    fn run_in(
        gpu: &GpuContext,
        kind: StylizeKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
        ctx: &EvalContext,
    ) -> FrameBuffer {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompStylizeProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        let out = processor
            .process(
                &Node::new(NodeId::new(1), kind.label()),
                ctx,
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .expect("stylize");
        readback(out.as_ref())
    }

    fn run(
        gpu: &GpuContext,
        kind: StylizeKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
    ) -> FrameBuffer {
        run_in(gpu, kind, values, input, &ctx((input.width, input.height)))
    }

    fn px(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        fb.as_f32()[((y * fb.width + x) * 4) as usize..][..4]
            .try_into()
            .unwrap()
    }

    fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
        (0..4).all(|c| (a[c] - b[c]).abs() <= tol)
    }

    const WHITE: [f32; 4] = [1.0; 4];
    const CLEAR: [f32; 4] = [0.0; 4];
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// An opaque white `side` square at `(x0, y0)` on transparency.
    fn square(w: u32, h: u32, x0: u32, y0: u32, side: u32) -> FrameBuffer {
        let mut pixels = vec![CLEAR; (w * h) as usize];
        for y in y0..y0 + side {
            for x in x0..x0 + side {
                pixels[(y * w + x) as usize] = WHITE;
            }
        }
        frame(w, h, &pixels)
    }

    fn all_zero(fb: &FrameBuffer) -> bool {
        fb.as_f32().iter().all(|v| *v == 0.0)
    }

    // ---- alpha 0 in, alpha 0 out -----------------------------------------

    #[test]
    fn glow_and_shadow_of_a_transparent_image_stay_transparent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let empty = frame(9, 7, &[CLEAR; 63]);
        for (kind, values) in [
            (
                StylizeKind::Glow,
                vec![("radius", f(4.0)), ("intensity", f(5.0))],
            ),
            (
                StylizeKind::DropShadow,
                vec![
                    ("softness", f(3.0)),
                    ("opacity", f(1.0)),
                    ("offset", ResolvedValue::Vec2([2.0, 1.0])),
                ],
            ),
            (StylizeKind::Stroke, vec![("width", f(3.0))]),
        ] {
            let out = run(&gpu, kind, &values, &empty);
            assert!(all_zero(&out), "{kind:?}: {:?}", &out.as_f32()[..8]);
        }
        // Not vacuous: the same settings do draw around a shape.
        let out = run(
            &gpu,
            StylizeKind::Glow,
            &[("radius", f(4.0)), ("intensity", f(5.0))],
            &square(9, 7, 3, 3, 1),
        );
        assert!(px(&out, 1, 3)[3] > 0.0);
    }

    // ---- glow ------------------------------------------------------------

    #[test]
    fn glow_adds_a_halo_and_zero_intensity_is_the_identity() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = square(21, 21, 10, 10, 1);
        let out = run(
            &gpu,
            StylizeKind::Glow,
            &[("radius", f(6.0)), ("intensity", f(1.0))],
            &input,
        );
        // The source pixel stays opaque and gets brighter (additive, so past
        // 1); neighbours get alpha that falls off with distance; far away is
        // untouched.
        assert_eq!(px(&out, 10, 10)[3], 1.0);
        assert!(px(&out, 10, 10)[0] > 1.0);
        let (near, mid) = (px(&out, 11, 10)[3], px(&out, 14, 10)[3]);
        assert!(near > mid && mid > 0.0, "{near} {mid}");
        assert_eq!(px(&out, 0, 0), CLEAR);
        // A white glow on white: the colour is white wherever it is drawn.
        assert!(close(
            px(&out, 12, 10),
            [1.0, 1.0, 1.0, px(&out, 12, 10)[3]],
            1e-4
        ));
        // More intensity, more halo.
        let more = run(
            &gpu,
            StylizeKind::Glow,
            &[("radius", f(6.0)), ("intensity", f(3.0))],
            &input,
        );
        assert!(px(&more, 12, 10)[3] > px(&out, 12, 10)[3]);
        // Intensity 0 adds nothing, whatever the radius.
        let none = run(
            &gpu,
            StylizeKind::Glow,
            &[("radius", f(6.0)), ("intensity", f(0.0))],
            &input,
        );
        assert_eq!(none.as_f32(), input.as_f32());
    }

    #[test]
    fn glow_is_tinted_by_its_colour_and_leaves_opaque_pixels_alone() {
        let Some(gpu) = gpu_or_skip() else { return };
        // An opaque white field: the blurred copy is white everywhere, but the
        // alpha is already 1, so only the colour can change (additively).
        let input = frame(5, 5, &[WHITE; 25]);
        let out = run(
            &gpu,
            StylizeKind::Glow,
            &[
                ("radius", f(0.0)),
                ("intensity", f(1.0)),
                ("color", c4([1.0, 0.0, 0.0, 1.0])),
            ],
            &input,
        );
        // radius 0: B = P, so rgb = 1 + (1, 0, 0) = (2, 1, 1): additive, HDR.
        assert!(
            close(px(&out, 2, 2), [2.0, 1.0, 1.0, 1.0], 1e-5),
            "{:?}",
            px(&out, 2, 2)
        );
    }

    // ---- drop shadow -----------------------------------------------------

    #[test]
    fn a_hard_shadow_sits_behind_the_image_offset_by_the_given_amount() {
        let Some(gpu) = gpu_or_skip() else { return };
        // 4 x 4 square at (3, 3); shadow 2 px right, 1 px down, opacity 0.5.
        let input = square(12, 12, 3, 3, 4);
        let out = run(
            &gpu,
            StylizeKind::DropShadow,
            &[
                ("color", c4([1.0, 0.0, 0.0, 1.0])),
                ("opacity", f(0.5)),
                ("offset", ResolvedValue::Vec2([2.0, 1.0])),
                ("softness", f(0.0)),
            ],
            &input,
        );
        for y in 0..12u32 {
            for x in 0..12u32 {
                let in_square = (3..7).contains(&x) && (3..7).contains(&y);
                let in_shadow = (5..9).contains(&x) && (4..8).contains(&y);
                let want = if in_square {
                    WHITE
                } else if in_shadow {
                    [1.0, 0.0, 0.0, 0.5]
                } else {
                    CLEAR
                };
                assert!(
                    close(px(&out, x, y), want, 1e-5),
                    "({x},{y}) {:?}",
                    px(&out, x, y)
                );
            }
        }
    }

    #[test]
    fn a_soft_shadow_spreads_and_one_offset_off_the_frame_is_harmless() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = square(21, 21, 8, 8, 5);
        let values = |softness: f32| {
            [
                ("opacity", f(1.0)),
                ("offset", ResolvedValue::Vec2([0.0, 0.0])),
                ("softness", f(softness)),
            ]
        };
        let hard = run(&gpu, StylizeKind::DropShadow, &values(0.0), &input);
        let soft = run(&gpu, StylizeKind::DropShadow, &values(4.0), &input);
        assert_eq!(
            px(&hard, 6, 10)[3],
            0.0,
            "no shadow past a zero-offset hard edge"
        );
        assert!(
            px(&soft, 6, 10)[3] > 0.0,
            "a blur reaches outside the shape"
        );
        let away = run(
            &gpu,
            StylizeKind::DropShadow,
            &[("offset", ResolvedValue::Vec2([500.0, -500.0]))],
            &input,
        );
        assert_eq!(away.as_f32(), input.as_f32(), "the shadow left the frame");
    }

    // ---- stroke ----------------------------------------------------------

    #[test]
    fn a_stroke_of_width_zero_is_the_input() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 7);
        for position in ["outside", "inside"] {
            let out = run(
                &gpu,
                StylizeKind::Stroke,
                &[
                    ("width", f(0.0)),
                    ("position", ResolvedValue::Str(position.into())),
                    ("color", c4([1.0, 0.0, 1.0, 1.0])),
                ],
                &input,
            );
            assert_eq!(out.as_f32(), input.as_f32(), "{position}");
        }
        // And a positive width changes the same image (the test can fail).
        let out = run(
            &gpu,
            StylizeKind::Stroke,
            &[("width", f(2.0)), ("color", c4([1.0, 0.0, 1.0, 1.0]))],
            &input,
        );
        assert_ne!(out.as_f32(), input.as_f32());
    }

    #[test]
    fn an_outside_stroke_fades_over_one_pixel_at_its_width() {
        let Some(gpu) = gpu_or_skip() else { return };
        // 4 x 4 square at (8, 8) in a 20 x 20 frame, a width-2 red stroke.
        let input = square(20, 20, 8, 8, 4);
        let out = run(
            &gpu,
            StylizeKind::Stroke,
            &[("width", f(2.0)), ("color", c4(RED))],
            &input,
        );
        // Along the row through the square, left of it: 1 px away is full,
        // 2 px away half (the anti-aliased edge), 3 px away nothing.
        assert!(close(px(&out, 7, 9), RED, 1e-5), "{:?}", px(&out, 7, 9));
        assert!(
            close(px(&out, 6, 9), [1.0, 0.0, 0.0, 0.5], 1e-5),
            "{:?}",
            px(&out, 6, 9)
        );
        assert_eq!(px(&out, 5, 9), CLEAR);
        // The shape itself is untouched.
        assert_eq!(px(&out, 9, 9), WHITE);
        // The ring is round: the diagonal corner pixel (sqrt 2 away) is full,
        // (2, 2) away (2.83) is not.
        assert!(close(px(&out, 7, 7), RED, 1e-5));
        assert!(px(&out, 6, 6)[3] < 0.1, "{:?}", px(&out, 6, 6));
    }

    #[test]
    fn an_inside_stroke_recolours_the_rim_and_keeps_the_alpha() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = square(20, 20, 6, 6, 8);
        let out = run(
            &gpu,
            StylizeKind::Stroke,
            &[
                ("width", f(2.0)),
                ("position", ResolvedValue::Str("inside".into())),
                ("color", c4(RED)),
            ],
            &input,
        );
        // Depth 0 (the rim pixel): fully the stroke colour; depth 1: half;
        // depth 2: the image's own colour. Alpha is 1 throughout the square.
        assert!(close(px(&out, 6, 10), RED, 1e-5), "{:?}", px(&out, 6, 10));
        assert!(
            close(px(&out, 7, 10), [1.0, 0.5, 0.5, 1.0], 1e-5),
            "{:?}",
            px(&out, 7, 10)
        );
        assert_eq!(px(&out, 8, 10), WHITE);
        assert_eq!(px(&out, 10, 10), WHITE);
        assert_eq!(px(&out, 5, 10), CLEAR, "nothing is drawn outside the shape");
    }

    #[test]
    fn an_unknown_stroke_position_passes_the_image_through() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = square(8, 8, 3, 3, 2);
        let out = {
            let mut shaders = ShaderManager::new(gpu.clone());
            let processor = CompStylizeProcessor::new(
                StylizeKind::Stroke,
                gpu.clone(),
                &mut shaders,
                pool(&gpu),
            );
            let mut params = ResolvedParams::default();
            params.set("position", ResolvedValue::Str("no_such_position".into()));
            processor
                .process(
                    &Node::new(NodeId::new(1), "comp.stroke"),
                    &ctx((8, 8)),
                    &[Some(Arc::new(input.clone()))],
                    &params,
                    &mut Evaluator::new(),
                )
                .unwrap()
        };
        assert_eq!(
            out.downcast_ref::<FrameBuffer>().unwrap().as_f32(),
            input.as_f32()
        );
        assert!(!comp_stroke_position_is_known("no_such_position"));
        assert!(
            STROKE_POSITIONS
                .iter()
                .all(|p| comp_stroke_position_is_known(p))
        );
    }

    // ---- emboss ----------------------------------------------------------

    /// CPU reference for `angle` 0, `distance` 1: `rgb + amount * (h(x - 1) -
    /// h(x + 1))` with clamped taps and `h = luma(premultiplied) + alpha`.
    fn emboss_reference(input: &FrameBuffer, amount: f32) -> FrameBuffer {
        let h = |x: i32, y: i32| {
            let x = x.clamp(0, input.width as i32 - 1) as u32;
            let y = y.clamp(0, input.height as i32 - 1) as u32;
            let p = px(input, x, y);
            let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            l * p[3] + p[3]
        };
        let mut out = Vec::new();
        for y in 0..input.height as i32 {
            for x in 0..input.width as i32 {
                let p = px(input, x as u32, y as u32);
                if p[3] > 0.0 {
                    let r = amount * (h(x - 1, y) - h(x + 1, y));
                    out.extend([p[0] + r, p[1] + r, p[2] + r, p[3]]);
                } else {
                    out.extend(p);
                }
            }
        }
        FrameBuffer::from_f32(input.width, input.height, out)
    }

    #[test]
    fn emboss_matches_a_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 7);
        let out = run(
            &gpu,
            StylizeKind::Emboss,
            &[("angle", f(0.0)), ("distance", f(1.0)), ("amount", f(0.5))],
            &input,
        );
        let want = emboss_reference(&input, 0.5);
        for (i, (g, w)) in out.as_f32().iter().zip(want.as_f32().iter()).enumerate() {
            assert!((g - w).abs() < 1e-5, "element {i}: {g} vs {w}");
        }
        assert_ne!(
            out.as_f32(),
            input.as_f32(),
            "the reference must move something"
        );
    }

    #[test]
    fn emboss_lights_the_slope_that_faces_the_light_and_zero_amount_is_the_identity() {
        let Some(gpu) = gpu_or_skip() else { return };
        // Dark left half, bright right half, opaque: a step rising to the right.
        let pixels: Vec<[f32; 4]> = (0..8)
            .map(|x| {
                if x < 4 {
                    [0.2, 0.2, 0.2, 1.0]
                } else {
                    [0.8, 0.8, 0.8, 1.0]
                }
            })
            .collect();
        let input = frame(8, 1, &pixels);
        let lit = |angle: f32| {
            run(
                &gpu,
                StylizeKind::Emboss,
                &[("angle", f(angle)), ("amount", f(1.0))],
                &input,
            )
        };
        // Light from the left (180 degrees): the surface falls toward it... the
        // step rises away from it, so it is lit; from the right it is shaded.
        assert!(px(&lit(180.0), 3, 0)[0] > 0.2 + 0.1);
        assert!(px(&lit(0.0), 3, 0)[0] < 0.2 - 0.1);
        // Flat areas are unchanged.
        assert!(close(px(&lit(180.0), 0, 0), [0.2, 0.2, 0.2, 1.0], 1e-6));
        let none = run(&gpu, StylizeKind::Emboss, &[("amount", f(0.0))], &input);
        assert_eq!(none.as_f32(), input.as_f32());
    }

    // ---- resolution independence -----------------------------------------

    fn scaled(size: (u32, u32), comp: (u32, u32)) -> EvalContext {
        let mut c = ctx(size);
        c.comp_resolution = comp;
        c
    }

    /// The sum of `alpha` along row `y`, in frame pixels.
    fn row_mass(fb: &FrameBuffer, y: u32, from: u32, to: u32) -> f32 {
        (from..to).map(|x| px(fb, x, y)[3]).sum()
    }

    /// The same composition at 1x and 2x: the stroke and the shadow cover the
    /// same composition-pixel extent (twice the device pixels).
    #[test]
    fn stroke_and_shadow_lengths_follow_the_composition_scale() {
        let Some(gpu) = gpu_or_skip() else { return };
        let comp = (24, 24);
        let small_in = square(24, 24, 10, 10, 4);
        let big_in = square(48, 48, 20, 20, 8);
        let (small_ctx, big_ctx) = (scaled(comp, comp), scaled((48, 48), comp));

        // Outside stroke of 3 comp px: the ring on the left of the square.
        let stroke = [("width", f(3.0)), ("color", c4(WHITE))];
        let small = run_in(&gpu, StylizeKind::Stroke, &stroke, &small_in, &small_ctx);
        let big = run_in(&gpu, StylizeKind::Stroke, &stroke, &big_in, &big_ctx);
        let (m_small, m_big) = (row_mass(&small, 11, 0, 10), row_mass(&big, 22, 0, 20));
        assert!(
            (m_big / 2.0 - m_small).abs() < 0.5,
            "stroke {m_small} vs {m_big}"
        );

        // A hard shadow shifted 4 comp px right: a 4 comp px strip.
        let shadow = [
            ("opacity", f(1.0)),
            ("softness", f(0.0)),
            ("offset", ResolvedValue::Vec2([4.0, 0.0])),
        ];
        let small = run_in(
            &gpu,
            StylizeKind::DropShadow,
            &shadow,
            &small_in,
            &small_ctx,
        );
        let big = run_in(&gpu, StylizeKind::DropShadow, &shadow, &big_in, &big_ctx);
        let (m_small, m_big) = (row_mass(&small, 11, 14, 24), row_mass(&big, 22, 28, 48));
        assert_eq!(m_small, 4.0);
        assert_eq!(m_big, 8.0);

        // A blur radius of 4 comp px: the halo of a dot reaches twice as far.
        let dot_small = square(41, 41, 20, 20, 1);
        let dot_big = square(81, 81, 40, 40, 2);
        let glow = [("radius", f(4.0)), ("intensity", f(1.0))];
        let reach = |fb: &FrameBuffer, centre: u32| {
            (0..centre)
                .filter(|x| px(fb, centre - 1 - *x, centre)[3] > 1e-4)
                .count() as f32
        };
        let small = run_in(
            &gpu,
            StylizeKind::Glow,
            &glow,
            &dot_small,
            &scaled((41, 41), (41, 41)),
        );
        let big = run_in(
            &gpu,
            StylizeKind::Glow,
            &glow,
            &dot_big,
            &scaled((81, 81), (41, 41)),
        );
        let (r_small, r_big) = (reach(&small, 20), reach(&big, 40));
        assert!(r_small > 3.0, "{r_small}");
        assert!(
            (r_big / r_small - 2.0).abs() < 0.35,
            "glow reach {r_small} vs {r_big}"
        );
    }
}

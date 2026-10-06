// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The blur, sharpen and distortion nodes: `comp.directional_blur`,
//! `comp.radial_blur`, `comp.sharpen`, `comp.warp`, `comp.lens_distortion`,
//! `comp.ripple`.
//!
//! One processor, one shader (`comp_distort.wgsl`), one uniform. [`DistortKind`]
//! says which node a [`CompDistortProcessor`] is, and [`fill`] is the only place
//! that reads a node's parameters into the uniform.
//!
//! **Border definition (all six).** A tap that falls outside the image is
//! clamped to the nearest edge texel (edge replicate). A uniform opaque image
//! therefore stays uniform up to its border: nothing darkens, fades or turns
//! transparent at the edge. (`comp.transform` is the opposite on purpose — a
//! moved layer must show a transparent border.) Taps are bilinear and are
//! weighted in premultiplied alpha (GPUCOMP-4), the sum converted back to
//! straight alpha.
//!
//! **Units.** Every length is in *composition* pixels and is multiplied by the
//! context's composition-to-canvas scale ([`composition_scale`]), so a preview
//! at half resolution shows the same picture: `length`, `radius`, `amount`
//! (warp), `amplitude` and `wavelength`. Directions are in screen space with y
//! down. Centres (`center`) are fractions of the frame (0.5, 0.5 is the middle)
//! and angles, zooms, `k` and phases are dimensionless, so those need no scale.
//!
//! - `comp.directional_blur`: a box of `length` px along `angle` degrees,
//!   centred on each pixel (angle 0 smears horizontally only).
//! - `comp.radial_blur`: `mode` `spin` rotates each pixel about `center` by up
//!   to `angle` degrees (centred on the pixel); `zoom` smears toward `center`
//!   over `zoom` (0..1) of the pixel's distance. The centre point is fixed.
//! - `comp.sharpen`: unsharp mask. A Gaussian blur of `radius` (the existing
//!   `blur` processor, so the same kernel) is subtracted in premultiplied
//!   alpha: `p + amount * (p - blurred)`. Alpha is the input's own; a
//!   transparent pixel is left as stored.
//! - `comp.warp`: displacement-map warp. The pixel at `p` shows the image at
//!   `p + (map.rg - 0.5) * amount`, `amount` a `(x, y)` in px. The map is the
//!   second input, stretched over the image (as `comp.alpha`'s matte) and read
//!   over mid-gray, so a transparent map texel displaces nothing. With no map
//!   the image passes through.
//! - `comp.lens_distortion`: the pixel at `p` shows the image at
//!   `c + (p - c) * (1 + k r^2)`, `r` the distance to the centre over the
//!   half-diagonal. `amount` is `k`; 0 is the identity, positive samples
//!   outward (barrel), negative inward (pincushion).
//! - `comp.ripple`: the pixel at `p` shows the image at `p + radial *
//!   amplitude * sin(2 pi (r / wavelength - phase))` with `r` the distance to
//!   the centre; amplitude 0 is the identity.
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

const SHADER_SRC: &str = include_str!("../shaders/comp_distort.wgsl");

/// Cap on the taps of a blur. ponytail: a blur longer than this many device
/// pixels is undersampled (visible stepping); raise it or go separable then.
const MAX_TAPS: u32 = 128;

pub use ravel_core::registry::builtin::COMP_RADIAL_BLUR_MODES as RADIAL_BLUR_MODES;

/// Whether `mode` is a `comp.radial_blur` mode the processor understands.
pub fn radial_blur_mode_is_known(mode: &str) -> bool {
    RADIAL_BLUR_MODES.contains(&mode)
}

/// Which node a [`CompDistortProcessor`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistortKind {
    DirectionalBlur,
    RadialBlur,
    Sharpen,
    Warp,
    LensDistortion,
    Ripple,
}

/// The shader's `mode`.
mod mode {
    pub const DIRECTIONAL: u32 = 0;
    pub const SPIN: u32 = 1;
    pub const ZOOM: u32 = 2;
    pub const SHARPEN: u32 = 3;
    pub const WARP: u32 = 4;
    pub const LENS: u32 = 5;
    pub const RIPPLE: u32 = 6;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
    a: [f32; 4],
    b: [f32; 4],
}

impl DistortKind {
    fn label(self) -> &'static str {
        match self {
            Self::DirectionalBlur => "comp.directional_blur",
            Self::RadialBlur => "comp.radial_blur",
            Self::Sharpen => "comp.sharpen",
            Self::Warp => "comp.warp",
            Self::LensDistortion => "comp.lens_distortion",
            Self::Ripple => "comp.ripple",
        }
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// Taps for a blur whose footprint is `extent` device pixels.
fn tap_count(extent: f32) -> u32 {
    (finite(extent).abs().ceil() as u32 + 1).min(MAX_TAPS)
}

/// The distance from `centre` to the farthest corner of a `w` x `h` frame.
fn max_radius(centre: [f32; 2], w: f32, h: f32) -> f32 {
    let dx = centre[0].max(w - centre[0]);
    let dy = centre[1].max(h - centre[1]);
    dx.hypot(dy)
}

/// Read the node's parameters into the uniform for a `size` frame.
fn fill(kind: DistortKind, p: &ResolvedParams, ctx: &EvalContext, size: (u32, u32)) -> Params {
    let (sx, sy) = composition_scale(ctx);
    let (sx, sy) = (sx as f32, sy as f32);
    // Isotropic quantities (a radius, a wavelength) use the geometric mean.
    let s = (sx * sy).sqrt();
    let (w, h) = (size.0 as f32, size.1 as f32);
    let centre = {
        let c = p.vec2_or("center", [0.5, 0.5]);
        [finite(c[0]) * w, finite(c[1]) * h]
    };
    let mut out = Params::zeroed();
    out.mode[1] = 1;
    match kind {
        DistortKind::DirectionalBlur => {
            let length = finite(p.f32_or("length", 0.0)).max(0.0);
            let (sin, cos) = finite(p.f32_or("angle", 0.0)).to_radians().sin_cos();
            let v = [length * cos * sx, length * sin * sy];
            out.mode[0] = mode::DIRECTIONAL;
            out.mode[1] = tap_count(v[0].hypot(v[1]));
            out.a = [v[0], v[1], 0.0, 0.0];
        }
        DistortKind::RadialBlur => {
            let reach = max_radius(centre, w, h);
            if p.str_or("mode", "spin") == "zoom" {
                let zoom = finite(p.f32_or("zoom", 0.0)).clamp(0.0, 1.0);
                out.mode[0] = mode::ZOOM;
                out.mode[1] = tap_count(zoom * reach);
                out.a = [centre[0], centre[1], zoom, 0.0];
            } else {
                let angle = finite(p.f32_or("angle", 0.0)).to_radians();
                out.mode[0] = mode::SPIN;
                out.mode[1] = tap_count(angle * reach);
                out.a = [centre[0], centre[1], angle, 0.0];
            }
        }
        DistortKind::Sharpen => {
            out.mode[0] = mode::SHARPEN;
            out.a[0] = finite(p.f32_or("amount", 0.0));
        }
        DistortKind::Warp => {
            let amount = p.vec2_or("amount", [0.0, 0.0]);
            out.mode[0] = mode::WARP;
            out.a = [finite(amount[0]) * sx, finite(amount[1]) * sy, 0.0, 0.0];
        }
        DistortKind::LensDistortion => {
            out.mode[0] = mode::LENS;
            out.a = [
                centre[0],
                centre[1],
                finite(p.f32_or("amount", 0.0)),
                2.0 / w.hypot(h).max(1.0),
            ];
        }
        DistortKind::Ripple => {
            out.mode[0] = mode::RIPPLE;
            out.a = [
                centre[0],
                centre[1],
                finite(p.f32_or("amplitude", 0.0)) * s,
                (finite(p.f32_or("wavelength", 60.0)) * s).max(1e-3),
            ];
            out.b[0] = finite(p.f32_or("phase", 0.0));
        }
    }
    out
}

/// The blur radius `comp.sharpen` hands the `blur` processor, in device px.
fn sharpen_radius(p: &ResolvedParams, ctx: &EvalContext) -> f32 {
    let (sx, sy) = composition_scale(ctx);
    finite(p.f32_or("radius", 2.0)).max(0.0) * (sx * sy).sqrt() as f32
}

pub struct CompDistortProcessor {
    kind: DistortKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
    /// `comp.sharpen`'s blurred copy comes from the existing blur node.
    blur: Option<BlurProcessor>,
}

impl CompDistortProcessor {
    pub fn new(
        kind: DistortKind,
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
                "comp_distort",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_distort.wgsl compilation failed");
        let blur = (kind == DistortKind::Sharpen).then(|| {
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

impl NodeProcessor for CompDistortProcessor {
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
        // The second texture slot: the displacement map, the blurred copy, or
        // (never sampled) the image itself.
        let aux: Option<Arc<dyn NodeData>> = match self.kind {
            DistortKind::Warp => match inputs.get(1).and_then(|i| i.clone()) {
                Some(map) => Some(map),
                None => return Ok(input),
            },
            DistortKind::Sharpen => {
                let blur = self.blur.as_ref().expect("sharpen owns a blur");
                let mut blur_params = ResolvedParams::default();
                blur_params.set("radius", ResolvedValue::Float(sharpen_radius(params, ctx)));
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

        let shader_params = fill(self.kind, params, ctx, (width, height));
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

    fn params(values: &[(&str, ResolvedValue)]) -> ResolvedParams {
        let mut p = ResolvedParams::default();
        for (key, v) in values {
            p.set(key, v.clone());
        }
        p
    }

    fn run_in(
        gpu: &GpuContext,
        kind: DistortKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
        aux: Option<&FrameBuffer>,
        ctx: &EvalContext,
    ) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompDistortProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let node = Node::new(NodeId::new(1), kind.label());
        processor
            .process(
                &node,
                ctx,
                &[
                    Some(Arc::new(input.clone())),
                    aux.map(|m| Arc::new(m.clone()) as Arc<dyn NodeData>),
                ],
                &params(values),
                &mut Evaluator::new(),
            )
            .expect("distort")
    }

    fn run(
        gpu: &GpuContext,
        kind: DistortKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
        aux: Option<&FrameBuffer>,
    ) -> FrameBuffer {
        let out = run_in(
            gpu,
            kind,
            values,
            input,
            aux,
            &ctx((input.width, input.height)),
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

    fn assert_premult_close(got: &FrameBuffer, want: &FrameBuffer, tol: f32, what: &str) {
        assert_eq!((got.width, got.height), (want.width, want.height));
        for y in 0..got.height {
            for x in 0..got.width {
                let (g, w) = (premult(px(got, x, y)), premult(px(want, x, y)));
                for c in 0..4 {
                    assert!(
                        (g[c] - w[c]).abs() <= tol,
                        "{what}: ({x},{y}) channel {c}: {g:?} vs {w:?}"
                    );
                }
            }
        }
    }

    /// Premultiplied bilinear tap with edge clamping: the definition from the
    /// module docs, written independently of the shader.
    fn ref_tap(img: &FrameBuffer, sx: f32, sy: f32) -> [f32; 4] {
        let (fx, fy) = (sx - 0.5, sy - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let texel = |ix: f32, iy: f32| {
            let cx = (ix as i32).clamp(0, img.width as i32 - 1) as u32;
            let cy = (iy as i32).clamp(0, img.height as i32 - 1) as u32;
            premult(px(img, cx, cy))
        };
        let mut acc = [0.0; 4];
        for (dx, dy, w) in [
            (0.0, 0.0, (1.0 - tx) * (1.0 - ty)),
            (1.0, 0.0, tx * (1.0 - ty)),
            (0.0, 1.0, (1.0 - tx) * ty),
            (1.0, 1.0, tx * ty),
        ] {
            let t = texel(x0 + dx, y0 + dy);
            for c in 0..4 {
                acc[c] += w * t[c];
            }
        }
        acc
    }

    /// CPU reference: for every pixel, the average of `ref_tap` over the
    /// positions `taps(p)` gives, compared in premultiplied alpha.
    fn assert_matches_reference(
        got: &FrameBuffer,
        input: &FrameBuffer,
        taps: impl Fn([f32; 2]) -> Vec<[f32; 2]>,
        what: &str,
    ) {
        let mut want = vec![0.0f32; got.as_f32().len()];
        for y in 0..got.height {
            for x in 0..got.width {
                let ts = taps([x as f32 + 0.5, y as f32 + 0.5]);
                let mut acc = [0.0f32; 4];
                for t in &ts {
                    let v = ref_tap(input, t[0], t[1]);
                    for c in 0..4 {
                        acc[c] += v[c] / ts.len() as f32;
                    }
                }
                let out = if acc[3] > 0.0 {
                    [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]
                } else {
                    [0.0; 4]
                };
                want[((y * got.width + x) * 4) as usize..][..4].copy_from_slice(&out);
            }
        }
        assert_premult_close(
            got,
            &FrameBuffer::from_f32(got.width, got.height, want),
            2e-5,
            what,
        );
    }

    /// A black opaque field with one white pixel at the centre.
    fn dot(size: u32) -> FrameBuffer {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; (size * size) as usize];
        pixels[((size / 2) * size + size / 2) as usize] = [1.0, 1.0, 1.0, 1.0];
        frame(size, size, &pixels)
    }

    fn uniform(w: u32, h: u32, c: [f32; 4]) -> FrameBuffer {
        frame(w, h, &vec![c; (w * h) as usize])
    }

    #[test]
    fn directional_blur_at_angle_zero_smears_only_horizontally() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = dot(9);
        let out = run(
            &gpu,
            DistortKind::DirectionalBlur,
            &[("length", f(4.0)), ("angle", f(0.0))],
            &input,
            None,
        );
        for y in 0..9 {
            for x in 0..9 {
                if y != 4 {
                    assert_eq!(px(&out, x, y), px(&input, x, y), "row {y} must not change");
                }
            }
        }
        // The dot spreads along its own row (taps at -2..2 reach 5 pixels).
        assert!(px(&out, 4, 4)[0] < 1.0 && px(&out, 4, 4)[0] > 0.0);
        assert!(px(&out, 2, 4)[0] > 0.0 && px(&out, 6, 4)[0] > 0.0);
        assert_eq!(px(&out, 0, 4)[0], 0.0);
        // 90 degrees is the same smear down the column.
        let down = run(
            &gpu,
            DistortKind::DirectionalBlur,
            &[("length", f(4.0)), ("angle", f(90.0))],
            &input,
            None,
        );
        assert!(px(&down, 4, 2)[0] > 0.0 && px(&down, 2, 4)[0] == 0.0);
    }

    #[test]
    fn directional_blur_edge_pixels_clamp_to_the_border_texel() {
        let Some(gpu) = gpu_or_skip() else { return };
        // A 5x1 opaque gray ramp, length 2 -> taps at -1, 0, +1.
        let ramp5: Vec<[f32; 4]> = (0..5)
            .map(|i| {
                let v = i as f32 / 4.0;
                [v, v, v, 1.0]
            })
            .collect();
        let input = frame(5, 1, &ramp5);
        let out = run(
            &gpu,
            DistortKind::DirectionalBlur,
            &[("length", f(2.0)), ("angle", f(0.0))],
            &input,
            None,
        );
        // Left edge: (clamped 0 + 0 + 0.25) / 3; right edge: (0.75 + 1 + clamped 1) / 3.
        assert!(
            (px(&out, 0, 0)[0] - 0.25 / 3.0).abs() < 1e-5,
            "{:?}",
            px(&out, 0, 0)
        );
        assert!(
            (px(&out, 4, 0)[0] - 2.75 / 3.0).abs() < 1e-5,
            "{:?}",
            px(&out, 4, 0)
        );
        assert!((px(&out, 2, 0)[0] - 0.5).abs() < 1e-5);
        assert_eq!(
            px(&out, 0, 0)[3],
            1.0,
            "the border never fades to transparent"
        );
    }

    #[test]
    fn directional_blur_matches_the_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 7);
        let (length, angle) = (5.0f32, 30.0f32);
        let out = run(
            &gpu,
            DistortKind::DirectionalBlur,
            &[("length", f(length)), ("angle", f(angle))],
            &input,
            None,
        );
        let (sin, cos) = angle.to_radians().sin_cos();
        let n = length.ceil() as usize + 1;
        assert_matches_reference(
            &out,
            &input,
            |p| {
                (0..n)
                    .map(|i| {
                        let t = i as f32 / (n - 1) as f32 - 0.5;
                        [p[0] + length * cos * t, p[1] + length * sin * t]
                    })
                    .collect()
            },
            "directional",
        );
    }

    #[test]
    fn radial_blur_leaves_the_centre_pixel_unchanged() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 9);
        for (mode, key, value) in [("spin", "angle", 40.0), ("zoom", "zoom", 0.5)] {
            let out = run(
                &gpu,
                DistortKind::RadialBlur,
                &[("mode", ResolvedValue::Str(mode.into())), (key, f(value))],
                &input,
                None,
            );
            let (g, w) = (px(&out, 4, 4), px(&input, 4, 4));
            for c in 0..4 {
                assert!(
                    (premult(g)[c] - premult(w)[c]).abs() < 1e-5,
                    "{mode}: {g:?} vs {w:?}"
                );
            }
            // And the blur does something away from the centre.
            let moved = (0..9).any(|x| (px(&out, x, 0)[0] - px(&input, x, 0)[0]).abs() > 1e-3);
            assert!(moved, "{mode} changed nothing");
        }
    }

    #[test]
    fn radial_blur_matches_the_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 9);
        let c = [4.5f32, 4.5];
        let reach = max_radius(c, 9.0, 9.0);
        let zoom = 0.4f32;
        let out = run(
            &gpu,
            DistortKind::RadialBlur,
            &[
                ("mode", ResolvedValue::Str("zoom".into())),
                ("zoom", f(zoom)),
            ],
            &input,
            None,
        );
        let n = (zoom * reach).ceil() as usize + 1;
        assert_matches_reference(
            &out,
            &input,
            |p| {
                (0..n)
                    .map(|i| {
                        let s = 1.0 - zoom * i as f32 / (n - 1) as f32;
                        [c[0] + (p[0] - c[0]) * s, c[1] + (p[1] - c[1]) * s]
                    })
                    .collect()
            },
            "zoom",
        );
        let angle = 50.0f32;
        let out = run(
            &gpu,
            DistortKind::RadialBlur,
            &[
                ("mode", ResolvedValue::Str("spin".into())),
                ("angle", f(angle)),
            ],
            &input,
            None,
        );
        let total = angle.to_radians();
        let n = (total * reach).ceil() as usize + 1;
        assert_matches_reference(
            &out,
            &input,
            |p| {
                (0..n)
                    .map(|i| {
                        let th = total * (i as f32 / (n - 1) as f32 - 0.5);
                        let (sn, cs) = th.sin_cos();
                        let d = [p[0] - c[0], p[1] - c[1]];
                        [c[0] + d[0] * cs - d[1] * sn, c[1] + d[0] * sn + d[1] * cs]
                    })
                    .collect()
            },
            "spin",
        );
    }

    #[test]
    fn lens_distortion_with_coefficient_zero_is_the_identity() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 7);
        let out = run(
            &gpu,
            DistortKind::LensDistortion,
            &[("amount", f(0.0))],
            &input,
            None,
        );
        assert_premult_close(&out, &input, 1e-5, "k = 0");
        // A non-zero coefficient moves pixels, and matches the reference.
        let k = 0.4f32;
        let out = run(
            &gpu,
            DistortKind::LensDistortion,
            &[("amount", f(k))],
            &input,
            None,
        );
        let c = [4.5f32, 3.5];
        let inv = 2.0 / 9.0f32.hypot(7.0);
        assert_matches_reference(
            &out,
            &input,
            |p| {
                let d = [p[0] - c[0], p[1] - c[1]];
                let r = d[0].hypot(d[1]) * inv;
                let s = 1.0 + k * r * r;
                vec![[c[0] + d[0] * s, c[1] + d[1] * s]]
            },
            "lens",
        );
    }

    #[test]
    fn ripple_matches_the_cpu_reference_and_zero_amplitude_is_the_identity() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(11, 9);
        let out = run(
            &gpu,
            DistortKind::Ripple,
            &[("amplitude", f(0.0))],
            &input,
            None,
        );
        assert_premult_close(&out, &input, 1e-5, "amplitude 0");

        let (amp, wl, phase) = (1.5f32, 6.0f32, 0.25f32);
        let out = run(
            &gpu,
            DistortKind::Ripple,
            &[
                ("amplitude", f(amp)),
                ("wavelength", f(wl)),
                ("phase", f(phase)),
            ],
            &input,
            None,
        );
        let c = [5.5f32, 4.5];
        assert_matches_reference(
            &out,
            &input,
            |p| {
                let d = [p[0] - c[0], p[1] - c[1]];
                let r = d[0].hypot(d[1]);
                let shift = if r > 0.0 {
                    amp * (std::f32::consts::TAU * (r / wl - phase)).sin() / r
                } else {
                    0.0
                };
                vec![[p[0] + d[0] * shift, p[1] + d[1] * shift]]
            },
            "ripple",
        );
    }

    #[test]
    fn warp_displaces_by_the_map_and_stretches_a_map_of_another_size() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(8, 4);
        let amount = ResolvedValue::Vec2([2.0, 0.0]);
        // A neutral gray map displaces nothing.
        let gray = uniform(8, 4, [0.5, 0.5, 0.5, 1.0]);
        let out = run(
            &gpu,
            DistortKind::Warp,
            &[("amount", amount.clone())],
            &input,
            Some(&gray),
        );
        assert_premult_close(&out, &input, 1e-5, "gray map");

        // R = 1 everywhere: every pixel shows the one `amount.x / 2` px to its right.
        let right = uniform(8, 4, [1.0, 0.5, 0.5, 1.0]);
        let out = run(
            &gpu,
            DistortKind::Warp,
            &[("amount", amount.clone())],
            &input,
            Some(&right),
        );
        assert_matches_reference(&out, &input, |p| vec![[p[0] + 1.0, p[1]]], "warp");
        // The last column reads past the edge: clamped to the border texel.
        assert_eq!(premult(px(&out, 7, 2)), premult(px(&input, 7, 2)));

        // A 1x1 map covers the whole frame; a transparent map displaces nothing.
        let one = uniform(1, 1, [1.0, 0.5, 0.5, 1.0]);
        let out = run(
            &gpu,
            DistortKind::Warp,
            &[("amount", amount.clone())],
            &input,
            Some(&one),
        );
        assert_matches_reference(&out, &input, |p| vec![[p[0] + 1.0, p[1]]], "1x1 map");
        let clear = uniform(8, 4, [1.0, 1.0, 1.0, 0.0]);
        let out = run(
            &gpu,
            DistortKind::Warp,
            &[("amount", amount.clone())],
            &input,
            Some(&clear),
        );
        assert_premult_close(&out, &input, 1e-5, "transparent map");

        // No map: the image passes through.
        let out = run_in(
            &gpu,
            DistortKind::Warp,
            &[("amount", amount)],
            &input,
            None,
            &ctx((8, 4)),
        );
        assert_eq!(
            out.downcast_ref::<FrameBuffer>()
                .expect("passed through")
                .as_f32(),
            input.as_f32()
        );
    }

    #[test]
    fn sharpen_boosts_an_edge_and_leaves_flat_areas_and_alpha_alone() {
        let Some(gpu) = gpu_or_skip() else { return };
        let side = |v: f32| [v, v, v, 1.0];
        let mut pixels = Vec::new();
        for _ in 0..3 {
            pixels.extend((0..12).map(|x| if x < 6 { side(0.25) } else { side(0.75) }));
        }
        let input = frame(12, 3, &pixels);
        let zero = run(
            &gpu,
            DistortKind::Sharpen,
            &[("amount", f(0.0)), ("radius", f(2.0))],
            &input,
            None,
        );
        assert_premult_close(&zero, &input, 1e-5, "amount 0");

        let out = run(
            &gpu,
            DistortKind::Sharpen,
            &[("amount", f(1.0)), ("radius", f(2.0))],
            &input,
            None,
        );
        assert!(
            px(&out, 5, 1)[0] < 0.25 - 1e-3,
            "dark side undershoots: {:?}",
            px(&out, 5, 1)
        );
        assert!(
            px(&out, 6, 1)[0] > 0.75 + 1e-3,
            "bright side overshoots: {:?}",
            px(&out, 6, 1)
        );
        // Far from the edge (and at the clamped border) the image is flat.
        assert!((px(&out, 0, 1)[0] - 0.25).abs() < 1e-3);
        assert!((px(&out, 11, 1)[0] - 0.75).abs() < 1e-3);
        for (o, i) in out
            .as_f32()
            .chunks_exact(4)
            .zip(input.as_f32().chunks_exact(4))
        {
            assert_eq!(o[3], i[3], "alpha is the input's");
        }
    }

    /// The border definition: a uniform opaque image stays uniform, corners
    /// included, for every node with a parameter set that moves pixels.
    #[test]
    fn a_uniform_opaque_image_is_unchanged_at_the_border_by_every_node() {
        let Some(gpu) = gpu_or_skip() else { return };
        let c = [0.6, 0.3, 0.2, 1.0];
        let input = uniform(9, 7, c);
        let map = ramp(9, 7);
        let vec2 = |x: f32, y: f32| ResolvedValue::Vec2([x, y]);
        let cases: Vec<(DistortKind, Vec<(&str, ResolvedValue)>)> = vec![
            (
                DistortKind::DirectionalBlur,
                vec![("length", f(8.0)), ("angle", f(25.0))],
            ),
            (
                DistortKind::RadialBlur,
                vec![
                    ("mode", ResolvedValue::Str("spin".into())),
                    ("angle", f(60.0)),
                    ("center", vec2(0.1, 0.2)),
                ],
            ),
            (
                DistortKind::RadialBlur,
                vec![
                    ("mode", ResolvedValue::Str("zoom".into())),
                    ("zoom", f(0.8)),
                    ("center", vec2(0.9, 0.9)),
                ],
            ),
            (
                DistortKind::Sharpen,
                vec![("amount", f(2.0)), ("radius", f(3.0))],
            ),
            (DistortKind::Warp, vec![("amount", vec2(9.0, 9.0))]),
            (DistortKind::LensDistortion, vec![("amount", f(-0.9))]),
            (DistortKind::LensDistortion, vec![("amount", f(0.9))]),
            (
                DistortKind::Ripple,
                vec![("amplitude", f(5.0)), ("wavelength", f(3.0))],
            ),
        ];
        for (kind, values) in cases {
            let out = run(&gpu, kind, &values, &input, Some(&map));
            for p in out.as_f32().chunks_exact(4) {
                for k in 0..4 {
                    assert!((p[k] - c[k]).abs() < 1e-4, "{kind:?}: {p:?}");
                }
            }
        }
    }

    /// A preview at half resolution shows the same picture: the uniform for a
    /// composition-pixel quantity at scale 2 equals the one for twice the value
    /// at scale 1.
    #[test]
    fn lengths_follow_the_composition_scale() {
        let at_scale = |scale: u32| {
            let mut c = ctx((16 * scale, 16 * scale));
            c.comp_resolution = (16, 16);
            c
        };
        let ctx2 = at_scale(2);
        let size2 = (32, 32);
        // Every length at once: the default wavelength scales too.
        let at = |c: &EvalContext, k: f32| {
            let p = params(&[
                ("length", f(4.0 * k)),
                ("amplitude", f(4.0 * k)),
                ("wavelength", f(40.0 * k)),
            ]);
            [
                fill(DistortKind::DirectionalBlur, &p, c, size2),
                fill(DistortKind::Ripple, &p, c, size2),
            ]
        };
        let mut unscaled = ctx((32, 32));
        unscaled.comp_resolution = (32, 32);
        // 4 comp px at scale 2 is 8 device px, as is 8 at scale 1.
        assert_eq!(at(&ctx2, 1.0), at(&unscaled, 2.0));
        assert_eq!(at(&ctx2, 1.0)[1].a[2..], [8.0, 80.0]);
        let a = fill(
            DistortKind::Warp,
            &params(&[("amount", ResolvedValue::Vec2([3.0, 5.0]))]),
            &ctx2,
            size2,
        );
        assert_eq!(a.a[..2], [6.0, 10.0]);
        assert_eq!(sharpen_radius(&params(&[("radius", f(3.0))]), &ctx2), 6.0);
    }

    /// The template's dropdown offers exactly the modes the processor reads.
    #[test]
    fn radial_blur_modes_are_known() {
        assert!(
            RADIAL_BLUR_MODES
                .iter()
                .all(|m| radial_blur_mode_is_known(m))
        );
        assert!(!radial_blur_mode_is_known("no_such_mode"));
    }
}

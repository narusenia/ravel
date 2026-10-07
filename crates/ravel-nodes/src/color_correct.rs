// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The all-in-one grading node (GPU).
//!
//! Sections, after Lumetri: **Basic** (temperature, tint, exposure, brightness,
//! contrast, highlights, shadows, whites, blacks, saturation, vibrance),
//! **Curves** (RGB, red, green, blue, and the three HSL curves), **Wheels**
//! (lift, gamma, gain) and **Creative** (fade, vignette); the node declares them
//! as parameter groups so Properties shows them as collapsible sections. There
//! is no LUT (an undecided design of its own).
//!
//! One pass, one shader. The stages that have a `comp.*` node (brightness /
//! contrast, saturation, both curve kinds, lift / gamma / gain) are the very
//! functions `comp_grade.wgsl` runs, from `grade_stages.wgsl`, and the curve
//! tables are baked by `comp/grade.rs`'s own code; a section run alone therefore
//! gives the same pixel as that node. The stages with none (white balance,
//! exposure, the four tonal bands, vibrance, fade, vignette) live in
//! `color_correct.wgsl`, with their formulas.
//!
//! **Saved projects.** `brightness`, `contrast` and `saturation` are the
//! original three and keep their meaning: brightness is added *before* the
//! contrast (so contrast scales it), which the shared stage reproduces as an
//! offset of `brightness * contrast`. Every other parameter is new and neutral
//! by default, and a document that lacks one reads it as neutral, so an old
//! `.ravprj` opens unchanged with no format bump. A stage whose parameters are
//! all neutral is skipped, not run as a no-op: the curve tables clamp to 0..1,
//! which would alter an HDR pixel for nothing.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::param_curve::CurveParam;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use crate::comp::grade::{TABLE_LEN, bake_curves, bake_hsl};

const SHADER_SRC: &str = include_str!("shaders/color_correct.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    flags: [u32; 4],
    wb: [f32; 4],
    bc: [f32; 4],
    sat: [f32; 4],
    tone: [f32; 4],
    lift: [f32; 4],
    gamma: [f32; 4],
    gain: [f32; 4],
    creative: [f32; 4],
    /// Entries `0..TABLE_LEN`: the RGB curves; `TABLE_LEN..`: the HSL curves.
    table: [[f32; 4]; 2 * TABLE_LEN],
}

/// One bit per stage, in pipeline order (`color_correct.wgsl` lists them).
mod stage {
    pub const WHITE_BALANCE: u32 = 1;
    pub const EXPOSURE: u32 = 1 << 1;
    pub const BRIGHTNESS_CONTRAST: u32 = 1 << 2;
    pub const TONAL: u32 = 1 << 3;
    pub const SATURATION: u32 = 1 << 4;
    pub const VIBRANCE: u32 = 1 << 5;
    pub const CURVES: u32 = 1 << 6;
    pub const HSL: u32 = 1 << 7;
    pub const WHEELS: u32 = 1 << 8;
    pub const FADE: u32 = 1 << 9;
    pub const VIGNETTE: u32 = 1 << 10;
}

fn neutral_hsl() -> CurveParam {
    CurveParam::linear([(0.0, 0.5), (1.0, 0.5)])
}

impl Params {
    /// Read the node's parameters; a missing one is its neutral value.
    fn from_params(p: &ResolvedParams) -> Self {
        let mut out = <Self as bytemuck::Zeroable>::zeroed();
        let f = |key: &str, default: f32| p.f32_or(key, default);
        let v3 = |key: &str, default: f32| {
            let c = p.vec3_or(key, [default; 3]);
            [c[0], c[1], c[2], 0.0]
        };
        let (temperature, tint, exposure, vibrance) = (
            f("temperature", 0.0),
            f("tint", 0.0),
            f("exposure", 0.0),
            f("vibrance", 0.0),
        );
        let (brightness, contrast, saturation) = (
            f("brightness", 0.0),
            f("contrast", 1.0),
            f("saturation", 1.0),
        );
        let tone = [
            f("highlights", 0.0),
            f("shadows", 0.0),
            f("whites", 0.0),
            f("blacks", 0.0),
        ];
        let (lift, gamma, gain) = (v3("lift", 0.0), v3("gamma", 1.0), v3("gain", 1.0));
        let creative = [
            f("fade", 0.0),
            f("vignette", 0.0),
            f("vignette_midpoint", 0.5),
            f("vignette_feather", 0.5),
        ];

        // The legacy order is `(x + brightness - 0.5) * contrast + 0.5`; the
        // shared stage is `(x - pivot) * contrast + pivot + brightness`, the
        // same line with the offset scaled by the contrast.
        out.bc = [brightness * contrast, contrast, 0.5, 0.0];
        out.wb = [temperature, tint, exposure, vibrance];
        out.sat = [saturation, 0.0, 0.0, 0.0];
        out.tone = tone;
        (out.lift, out.gamma, out.gain) = (lift, gamma, gain);
        out.creative = creative;

        bake_curves(&mut out.table[..TABLE_LEN], p);
        bake_hsl(&mut out.table[TABLE_LEN..], p);
        let curves_moved = ["rgb", "red", "green", "blue"]
            .iter()
            .any(|k| p.curve(k).is_some_and(|c| *c != CurveParam::identity()));
        let hsl_moved = ["hue_vs_hue", "hue_vs_sat", "hue_vs_lum"]
            .iter()
            .any(|k| p.curve(k).is_some_and(|c| *c != neutral_hsl()));

        let active = [
            (stage::WHITE_BALANCE, temperature != 0.0 || tint != 0.0),
            (stage::EXPOSURE, exposure != 0.0),
            (
                stage::BRIGHTNESS_CONTRAST,
                brightness != 0.0 || contrast != 1.0,
            ),
            (stage::TONAL, tone != [0.0; 4]),
            (stage::SATURATION, saturation != 1.0),
            (stage::VIBRANCE, vibrance != 0.0),
            (stage::CURVES, curves_moved),
            (stage::HSL, hsl_moved),
            (
                stage::WHEELS,
                lift != [0.0; 4] || gamma != [1.0, 1.0, 1.0, 0.0] || gain != [1.0, 1.0, 1.0, 0.0],
            ),
            (stage::FADE, creative[0] != 0.0),
            (stage::VIGNETTE, creative[1] != 0.0),
        ];
        out.flags[0] = active
            .iter()
            .filter(|(_, on)| *on)
            .fold(0, |bits, (bit, _)| bits | bit);
        out
    }
}

pub struct ColorCorrectProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl ColorCorrectProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        // Shared across every `color_correct` node: the pipeline depends only on the
        // shader and the layout, never on this node.
        let source = gpu_util::with_grade_stages(SHADER_SRC);
        let pipeline = shaders
            .compute_pipeline(
                "color_correct",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("color_correct.wgsl compilation failed");

        Self {
            pool,
            ctx,
            pipeline,
        }
    }
}

impl NodeProcessor for ColorCorrectProcessor {
    /// Nothing here comes off the node: the constructor takes `&Node` only to
    /// match the registry's signature and ignores it, and every value used is
    /// read from `params` at dispatch. Rebuilding on a parameter edit would
    /// recompile the shader and recreate the pipeline for no change at all.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let input = inputs
            .first()
            .and_then(|i| i.clone())
            .ok_or_else(|| anyhow::anyhow!("color_correct: expected FrameBuffer input"))?;
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("color_correct: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let shader_params = Params::from_params(params);
        let input_binding = image.binding();
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "color_correct",
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
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameBuffer, FrameRate};
    use std::sync::Arc;

    fn make_color_correct_node(brightness: f32, contrast: f32, saturation: f32) -> Node {
        Node::new(NodeId::new(1), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(brightness))
            .with_param("contrast", ParameterValue::Float(contrast))
            .with_param("saturation", ParameterValue::Float(saturation))
    }

    fn test_pool(gpu: &GpuContext) -> Arc<Mutex<TexturePool>> {
        Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)))
    }

    fn readback(out: &dyn NodeData) -> FrameBuffer {
        out.downcast_ref::<GpuFrameBuffer>()
            .expect("GPU node outputs a resident frame")
            .to_frame_buffer()
            .expect("readback")
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (4, 4))
    }

    fn solid_fb(width: u32, height: u32, r: f32, g: f32, b: f32, a: f32) -> FrameBuffer {
        let pixel_count = (width * height) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            data.extend_from_slice(&[r, g, b, a]);
        }
        FrameBuffer::from_f32(width, height, data)
    }

    /// Emits a fixed FrameBuffer; stands in for upstream nodes.
    struct FbSource(FrameBuffer);

    impl NodeProcessor for FbSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    /// Evaluate a color_correct node fed by `input` through a real evaluator.
    fn run_color_correct(
        brightness: f32,
        contrast: f32,
        saturation: f32,
        input: FrameBuffer,
    ) -> FrameBuffer {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_color_correct_node(brightness, contrast, saturation);
        let pool = test_pool(&gpu);
        let source =
            Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::FRAME_BUFFER);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), Arc::new(FbSource(input)));
        ev.register(
            NodeId::new(1),
            Arc::new(ColorCorrectProcessor::new(gpu, &mut shaders, pool, &node)),
        );
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        readback(out.as_ref())
    }

    #[test]
    fn identity_preserves_image() {
        let fb = run_color_correct(0.0, 1.0, 1.0, solid_fb(4, 4, 0.5, 0.3, 0.8, 1.0));

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        let px = fb.as_f32();
        for i in 0..16 {
            let base = i * 4;
            assert!((px[base] - 0.5).abs() < 0.01, "r mismatch at pixel {i}");
            assert!((px[base + 1] - 0.3).abs() < 0.01, "g mismatch at pixel {i}");
            assert!((px[base + 2] - 0.8).abs() < 0.01, "b mismatch at pixel {i}");
            assert!((px[base + 3] - 1.0).abs() < 0.01, "a mismatch at pixel {i}");
        }
    }

    #[test]
    fn brightness_shifts_values() {
        let fb = run_color_correct(0.2, 1.0, 1.0, solid_fb(4, 4, 0.5, 0.5, 0.5, 1.0));

        assert!((fb.as_f32()[0] - 0.7).abs() < 0.01);
        assert!((fb.as_f32()[1] - 0.7).abs() < 0.01);
        assert!((fb.as_f32()[2] - 0.7).abs() < 0.01);
    }

    // ---- the grading sections ----

    use crate::comp::{CompGradeProcessor, GradeKind};
    use ravel_core::eval::ResolvedValue;

    fn f(v: f32) -> ResolvedValue {
        ResolvedValue::Float(v)
    }

    fn v3(v: [f32; 3]) -> ResolvedValue {
        ResolvedValue::Vec3(v)
    }

    fn curve(v: CurveParam) -> ResolvedValue {
        ResolvedValue::Curve(v)
    }

    fn resolved(values: &[(&str, ResolvedValue)]) -> ResolvedParams {
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        params
    }

    /// Run `processor` over `input` with the given resolved parameters.
    fn run_with(
        processor: &dyn NodeProcessor,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
    ) -> FrameBuffer {
        let node = Node::new(NodeId::new(1), "test");
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (input.width, input.height));
        let out = processor
            .process(
                &node,
                &ctx,
                &[Some(Arc::new(input.clone()))],
                &resolved(values),
                &mut Evaluator::new(),
            )
            .expect("process");
        readback(out.as_ref())
    }

    fn gpu() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    fn cc(gpu: &GpuContext) -> ColorCorrectProcessor {
        let node = make_color_correct_node(0.0, 1.0, 1.0);
        ColorCorrectProcessor::new(
            gpu.clone(),
            &mut ShaderManager::new(gpu.clone()),
            test_pool(gpu),
            &node,
        )
    }

    fn grade(gpu: &GpuContext, kind: GradeKind) -> CompGradeProcessor {
        CompGradeProcessor::new(
            kind,
            gpu.clone(),
            &mut ShaderManager::new(gpu.clone()),
            test_pool(gpu),
        )
    }

    /// Varied colour (including values outside 0..1, which a clamping stage
    /// would change) and alpha.
    fn varied() -> FrameBuffer {
        let px: Vec<[f32; 4]> = vec![
            [0.0, 0.0, 0.0, 1.0],
            [0.6, 0.3, 0.2, 1.0],
            [0.25, 0.75, 0.5, 0.5],
            [1.0, 1.0, 1.0, 0.25],
            [1.7, -0.2, 0.4, 1.0],
            [0.1, 0.9, 0.9, 0.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.9, 0.1, 0.4, 0.75],
        ];
        FrameBuffer::from_f32(4, 2, px.concat())
    }

    fn assert_same(a: &FrameBuffer, b: &FrameBuffer, tol: f32, what: &str) {
        assert_eq!(a.as_f32().len(), b.as_f32().len());
        for (i, (x, y)) in a.as_f32().iter().zip(b.as_f32().iter()).enumerate() {
            assert!((x - y).abs() <= tol, "{what}: lane {i}: {x} vs {y}");
        }
    }

    /// The pre-grading shader, line for line: brightness, contrast, saturation.
    fn legacy(rgb: [f32; 3], b: f32, c: f32, s: f32) -> [f32; 3] {
        let v = rgb.map(|x| (x + b - 0.5) * c + 0.5);
        let lum = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
        v.map(|x| lum + (x - lum) * s)
    }

    /// A project saved before the grading sections carries only the original
    /// three parameters; it renders the pixels the old shader produced, HDR and
    /// negative values included.
    #[test]
    fn a_saved_node_with_only_the_original_parameters_renders_as_before() {
        let Some(gpu) = gpu() else { return };
        let input = varied();
        let processor = cc(&gpu);
        for (b, c, s) in [
            (0.0, 1.0, 1.0),
            (0.2, 1.0, 1.0),
            (0.0, 1.5, 1.0),
            (0.0, 1.0, 0.0),
            (0.3, 2.0, 0.5),
            (-0.4, 0.5, 1.7),
        ] {
            let out = run_with(
                &processor,
                &[
                    ("brightness", f(b)),
                    ("contrast", f(c)),
                    ("saturation", f(s)),
                ],
                &input,
            );
            for (i, (o, src)) in out
                .as_f32()
                .chunks_exact(4)
                .zip(input.as_f32().chunks_exact(4))
                .enumerate()
            {
                assert_eq!(o[3].to_bits(), src[3].to_bits(), "alpha {i}");
                let want = legacy([src[0], src[1], src[2]], b, c, s);
                for ch in 0..3 {
                    assert!(
                        (o[ch] - want[ch]).abs() < 1e-5,
                        "({b},{c},{s}) pixel {i} ch {ch}: {} vs {}",
                        o[ch],
                        want[ch]
                    );
                }
            }
        }
    }

    /// The three original parameters keep their names, defaults and ranges.
    #[test]
    fn the_original_parameters_keep_their_template() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut reg);
        let t = reg.get("color_correct").unwrap();
        let keys: Vec<_> = t.default_params.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys[..3], ["brightness", "contrast", "saturation"]);
        let default = |k: &str| {
            t.default_params
                .iter()
                .find(|p| p.key == k)
                .map(|p| p.value.clone())
                .unwrap()
        };
        assert_eq!(default("brightness"), ParameterValue::Float(0.0));
        assert_eq!(default("contrast"), ParameterValue::Float(1.0));
        assert_eq!(default("saturation"), ParameterValue::Float(1.0));
        // Every parameter is in exactly one display group.
        for p in &t.default_params {
            let n = t
                .param_group_declarations()
                .iter()
                .filter(|(_, keys)| keys.contains(&p.key))
                .count();
            assert_eq!(n, 1, "{} is in {n} groups", p.key);
        }
    }

    /// Every new parameter at its default is the identity, bit for bit, on
    /// values a clamp would change.
    #[test]
    fn the_new_parameters_at_their_defaults_are_the_identity() {
        let Some(gpu) = gpu() else { return };
        let input = varied();
        let out = run_with(&cc(&gpu), &[], &input);
        for (o, s) in out.as_f32().iter().zip(input.as_f32().iter()) {
            assert_eq!(o.to_bits(), s.to_bits());
        }
        // Explicitly neutral curves, wheels and creative values are the same.
        let out = run_with(
            &cc(&gpu),
            &[
                ("rgb", curve(CurveParam::identity())),
                ("hue_vs_sat", curve(neutral_hsl())),
                ("lift", v3([0.0; 3])),
                ("gamma", v3([1.0; 3])),
                ("gain", v3([1.0; 3])),
                ("vignette_midpoint", f(0.1)),
            ],
            &input,
        );
        for (o, s) in out.as_f32().iter().zip(input.as_f32().iter()) {
            assert_eq!(o.to_bits(), s.to_bits());
        }
    }

    /// A section run alone is the matching `comp.*` node: same stages, same
    /// pixel. (Brightness reaches the shared stage scaled by the contrast.)
    #[test]
    fn each_section_alone_matches_its_standalone_node() {
        let Some(gpu) = gpu() else { return };
        let input = varied();
        let combined = cc(&gpu);

        let a = run_with(
            &combined,
            &[("brightness", f(0.125)), ("contrast", f(2.0))],
            &input,
        );
        let b = run_with(
            &grade(&gpu, GradeKind::BrightnessContrast),
            &[("brightness", f(0.25)), ("contrast", f(2.0))],
            &input,
        );
        assert_same(&a, &b, 1e-6, "brightness / contrast");

        let a = run_with(&combined, &[("saturation", f(0.4))], &input);
        let b = run_with(
            &grade(&gpu, GradeKind::HueSaturation),
            &[("saturation", f(0.4))],
            &input,
        );
        assert_same(&a, &b, 1e-6, "saturation");

        let curves = [
            ("rgb", curve(CurveParam::linear([(0.0, 0.1), (1.0, 0.8)]))),
            (
                "red",
                curve(CurveParam::linear([(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)])),
            ),
            ("blue", curve(CurveParam::linear([(0.0, 0.3), (1.0, 0.6)]))),
        ];
        let a = run_with(&combined, &curves, &input);
        let b = run_with(&grade(&gpu, GradeKind::Curves), &curves, &input);
        assert_same(&a, &b, 1e-6, "curves");
        assert!(
            a.as_f32() != input.as_f32(),
            "the curves test must change the picture"
        );

        let hsl = [
            (
                "hue_vs_hue",
                curve(CurveParam::linear([(0.0, 0.8), (0.5, 0.3), (1.0, 0.2)])),
            ),
            (
                "hue_vs_sat",
                curve(CurveParam::linear([(0.0, 0.25), (1.0, 0.75)])),
            ),
            (
                "hue_vs_lum",
                curve(CurveParam::linear([(0.0, 0.75), (0.4, 0.25), (1.0, 0.5)])),
            ),
        ];
        let a = run_with(&combined, &hsl, &input);
        let b = run_with(&grade(&gpu, GradeKind::HslCurves), &hsl, &input);
        assert_same(&a, &b, 1e-6, "hsl curves");
        assert!(a.as_f32() != input.as_f32());

        let wheels = [
            ("lift", v3([0.05, -0.1, 0.0])),
            ("gamma", v3([1.5, 1.0, 0.7])),
            ("gain", v3([1.2, 0.9, 2.0])),
        ];
        let a = run_with(&combined, &wheels, &input);
        let b = run_with(&grade(&gpu, GradeKind::LiftGammaGain), &wheels, &input);
        assert_same(&a, &b, 1e-6, "lift / gamma / gain");
        assert!(a.as_f32() != input.as_f32());
    }

    /// Run the combined node over a single pixel (alpha 1) and read it back.
    fn one(gpu: &GpuContext, values: &[(&str, ResolvedValue)], rgb: [f32; 3]) -> [f32; 3] {
        let input = FrameBuffer::from_f32(1, 1, vec![rgb[0], rgb[1], rgb[2], 1.0]);
        let px = run_with(&cc(gpu), values, &input).as_f32().to_vec();
        assert_eq!(px[3], 1.0);
        [px[0], px[1], px[2]]
    }

    fn near(got: [f32; 3], want: [f32; 3]) {
        for ch in 0..3 {
            assert!((got[ch] - want[ch]).abs() < 1e-5, "{got:?} vs {want:?}");
        }
    }

    /// The stages with no `comp.*` counterpart, against hand-computed pixels.
    #[test]
    fn the_stages_without_a_standalone_node_hit_known_pixels() {
        let Some(gpu) = gpu() else { return };
        let r2 = 2.0_f32.sqrt();

        // White balance: 2^(t/2) on red, 2^(-t/2) on blue; tint on green.
        near(
            one(&gpu, &[("temperature", f(1.0))], [0.5; 3]),
            [0.5 * r2, 0.5, 0.5 / r2],
        );
        near(one(&gpu, &[("tint", f(2.0))], [0.5; 3]), [0.5, 0.25, 0.5]);
        // Exposure: one stop doubles.
        near(one(&gpu, &[("exposure", f(1.0))], [0.25; 3]), [0.5; 3]);
        // Exposure runs before the contrast: 0.375 -> 0.75 -> (0.75-0.5)*2+0.5.
        near(
            one(
                &gpu,
                &[("exposure", f(1.0)), ("contrast", f(2.0))],
                [0.375; 3],
            ),
            [1.0; 3],
        );
        // Tonal bands add 0.25 * amount * weight to every channel.
        near(one(&gpu, &[("blacks", f(1.0))], [0.0; 3]), [0.25; 3]);
        near(one(&gpu, &[("whites", f(-1.0))], [1.0; 3]), [0.75; 3]);
        near(one(&gpu, &[("shadows", f(1.0))], [0.25; 3]), [0.375; 3]);
        near(one(&gpu, &[("highlights", f(1.0))], [0.75; 3]), [0.875; 3]);
        // Mid gray lies outside every band.
        near(
            one(
                &gpu,
                &[
                    ("highlights", f(1.0)),
                    ("shadows", f(1.0)),
                    ("whites", f(1.0)),
                    ("blacks", f(1.0)),
                ],
                [0.5; 3],
            ),
            [0.5; 3],
        );
        // Vibrance: scale 1 + v * (1 - chroma); chroma of (0.6, 0.3, 0.2) is 2/3.
        let rgb = [0.6_f32, 0.3, 0.2];
        let lum = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let k = 1.0 + 0.9 * (1.0 - 2.0 / 3.0);
        near(
            one(&gpu, &[("vibrance", f(0.9))], rgb),
            rgb.map(|x| lum + (x - lum) * k),
        );
        // A gray has no chroma to scale.
        near(one(&gpu, &[("vibrance", f(1.0))], [0.4; 3]), [0.4; 3]);
        // Fade lifts black to the fade and leaves white.
        near(one(&gpu, &[("fade", f(0.2))], [0.0; 3]), [0.2; 3]);
        near(one(&gpu, &[("fade", f(0.2))], [1.0; 3]), [1.0; 3]);
    }

    /// The vignette is 0 at the centre and 1 at the corners of any aspect.
    #[test]
    fn the_vignette_darkens_the_corners_and_spares_the_centre() {
        let Some(gpu) = gpu() else { return };
        let vignette = [
            ("vignette", f(-0.5)),
            ("vignette_midpoint", f(0.5)),
            ("vignette_feather", f(0.1)),
        ];
        // 9 x 5: the centre pixel is exactly the centre, a corner is at 0.89+.
        let input = solid_fb(9, 5, 0.8, 0.8, 0.8, 1.0);
        let out = run_with(&cc(&gpu), &vignette, &input);
        let at = |x: usize, y: usize| out.as_f32()[(y * 9 + x) * 4];
        assert!((at(4, 2) - 0.8).abs() < 1e-6, "centre {}", at(4, 2));
        assert!((at(0, 0) - 0.4).abs() < 1e-5, "corner {}", at(0, 0));
        assert!((at(8, 4) - 0.4).abs() < 1e-5, "corner {}", at(8, 4));
        // A positive amount lightens the corner towards white: 0.8 + 0.5 * 0.2.
        let out = run_with(
            &cc(&gpu),
            &[
                ("vignette", f(0.5)),
                ("vignette_midpoint", f(0.5)),
                ("vignette_feather", f(0.1)),
            ],
            &input,
        );
        assert!((out.as_f32()[0] - 0.9).abs() < 1e-5);
    }
}

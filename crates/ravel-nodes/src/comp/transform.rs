// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.transform` — the shell's built-in layer transform (REQ-LAYER-001).
//!
//! Applies the owning layer's animatable transform channels (anchor point,
//! position, scale, rotation in degrees) to the layer's frame, composing the
//! parent chain's transforms on top (P/R/S inheritance, REQ-LAYER-001).
//! Channel values are read from the [`Document`] at process time — nothing
//! is captured at construction — and evaluated at the owning layer's local
//! frame (keyframes live in layer-local frames, REQ-LAYER-006).
//!
//! The matrix math lives in [`ravel_core::composition::transform`] so the
//! viewer's bbox and hit test compose the parent chain exactly the way these
//! pixels do.
//!
//! [`CompTransformGpuProcessor`] is the default path and keeps the frame
//! resident in VRAM; [`CompTransformProcessor`] is the CPU reference the golden
//! tests register explicitly. Both inverse-map the *same* world matrix and
//! interpolate in premultiplied alpha, and this module's tests compare them.

use ravel_core::composition::compile::NodeRole;
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

use super::{shell_layer, transparent};
use crate::gpu_util;
use crate::gpu_util::ensure_cpu;

const SHADER_SRC: &str = include_str!("../shaders/comp_transform.wgsl");

/// What the shell transform does to its input at this frame.
enum Mapping {
    /// Identity world matrix: the node is a pass-through.
    PassThrough,
    /// Singular world matrix (zero scale): the layer collapses.
    Collapsed,
    /// Inverse mapping from output pixel to source pixel.
    Inverse(Affine),
}

/// Resolve the owning layer's world matrix and reduce it to the mapping both
/// paths act on, so the short-circuits cannot drift apart.
fn shell_mapping(
    node: &Node,
    ctx: &EvalContext,
    scope: &mut dyn EvalScope,
) -> anyhow::Result<Mapping> {
    let (comp, layer_id) = shell_layer(node, scope, NodeRole::Transform)?;
    let layer = comp
        .get_layer(layer_id)
        .ok_or_else(|| anyhow::anyhow!("comp.transform: layer {layer_id:?} missing"))?;

    let matrix = world_matrix(&comp, layer, ctx);
    if matrix.is_identity() {
        return Ok(Mapping::PassThrough);
    }
    Ok(match matrix.inverse() {
        Some(inverse) => Mapping::Inverse(inverse),
        None => Mapping::Collapsed,
    })
}

// ===========================================================================
// Processor
// ===========================================================================

/// Applies the owning layer's (and its parent chain's) transform to the
/// frame via inverse mapping with premultiplied bilinear sampling.
/// Tolerates a missing input so null layers — which keep a Transform node
/// for parenting — evaluate cleanly.
pub struct CompTransformProcessor;

impl CompTransformProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompTransformProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };

        let inverse = match shell_mapping(node, ctx, scope)? {
            Mapping::PassThrough => return Ok(input),
            // Degenerate transform (zero scale) collapses the layer.
            Mapping::Collapsed => return Ok(transparent(ctx)),
            Mapping::Inverse(inverse) => inverse,
        };

        let source = ensure_cpu(input.as_ref())?;
        let src = source.as_rgba_f32()?;
        let (width, height) = ctx.resolution;
        let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = inverse.apply(x as f32 + 0.5, y as f32 + 0.5);
                let rgba = sample_bilinear(&src, source.width, source.height, sx, sy);
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&rgba);
            }
        }
        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }

    fn is_time_dependent(&self) -> bool {
        // Layer transform channels are hidden (document-side) dependencies.
        true
    }
}

/// Bilinear sample at pixel-space `(sx, sy)`; interpolation happens in
/// premultiplied alpha to avoid fringing, and the result is converted back
/// to the straight-alpha buffer convention. Outside the source: transparent.
fn sample_bilinear(pixels: &[f32], width: u32, height: u32, sx: f32, sy: f32) -> [f32; 4] {
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;

    let mut acc = [0.0f32; 4];
    for (dx, dy, w) in [
        (0.0, 0.0, (1.0 - tx) * (1.0 - ty)),
        (1.0, 0.0, tx * (1.0 - ty)),
        (0.0, 1.0, (1.0 - tx) * ty),
        (1.0, 1.0, tx * ty),
    ] {
        if w <= 0.0 {
            continue;
        }
        let p = premultiplied_at(pixels, width, height, x0 + dx, y0 + dy);
        for (a, v) in acc.iter_mut().zip(p) {
            *a += w * v;
        }
    }
    if acc[3] > 0.0 {
        [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]
    } else {
        [0.0; 4]
    }
}

fn premultiplied_at(pixels: &[f32], width: u32, height: u32, x: f32, y: f32) -> [f32; 4] {
    if x < 0.0 || y < 0.0 || x >= width as f32 || y >= height as f32 {
        return [0.0; 4];
    }
    let idx = ((y as u32 * width + x as u32) * 4) as usize;
    let p = &pixels[idx..idx + 4];
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

// ===========================================================================
// GPU path
// ===========================================================================

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    inv: [f32; 6],
    src_width: f32,
    src_height: f32,
    out_width: f32,
    out_height: f32,
    _pad: [f32; 2],
}

/// The same inverse mapping as [`CompTransformProcessor`], dispatched over the
/// output canvas and left resident in VRAM.
pub struct CompTransformGpuProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompTransformGpuProcessor {
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
        // Shared across every shell transform node: the pipeline depends only
        // on the shader and the layout, never on this node.
        let pipeline = shaders
            .compute_pipeline(
                "comp_transform",
                &gpu_util::with_premultiplied_helpers(SHADER_SRC),
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_transform.wgsl compilation failed");

        Self {
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompTransformGpuProcessor {
    /// The owning layer is decoded from the node id and read from the
    /// `Document` at process time, so a layer edit invalidates instead of
    /// rebuilding — rebuilding would recompile the shader for no change.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };

        // Both short-circuits are the CPU reference's: `shape_layer_golden`'s
        // first case pins pixels that only stay fixed while an identity
        // transform passes its input through untouched.
        let inverse = match shell_mapping(node, ctx, scope)? {
            Mapping::PassThrough => return Ok(input),
            Mapping::Collapsed => return Ok(transparent(ctx)),
            Mapping::Inverse(inverse) => inverse,
        };

        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("comp.transform: {e}"))?;
        let (src_width, src_height) = image.size();
        let (out_width, out_height) = ctx.resolution;
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(out_width, out_height));

        let shader_params = Params {
            inv: inverse.0,
            src_width: src_width as f32,
            src_height: src_height as f32,
            out_width: out_width as f32,
            out_height: out_height as f32,
            _pad: [0.0; 2],
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("comp_transform params"),
                contents: bytemuck::bytes_of(&shader_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let input_view = image
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = self
            .ctx
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("comp_transform"),
                layout: self.pipeline.bind_group_layout(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&input_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&output_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: param_buf.as_entire_binding(),
                    },
                ],
            });

        let mut encoder =
            self.ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("comp_transform"),
                });
        // Dispatch covers the output canvas, not the source frame.
        self.pipeline
            .dispatch(&mut encoder, &bind_group, out_width, out_height);
        self.ctx.queue().submit(Some(encoder.finish()));

        image.release(&self.pool);

        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            out_width,
            out_height,
        )))
    }

    fn is_time_dependent(&self) -> bool {
        // Layer transform channels are hidden (document-side) dependencies.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::composition::compile::deterministic_node_id;
    use ravel_core::composition::{Composition, Document, Layer};
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::Graph;
    use ravel_core::id::{CompId, DataTypeId, LayerId};
    use ravel_core::types::FrameRate;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    const CANVAS: (u32, u32) = (16, 16);

    /// GPU and CPU agree to within f32 rounding; they are not bit-identical
    /// because a GPU is free to contract `acc + w * v` into an FMA, which
    /// rounds once where the CPU rounds twice.
    const EPS: f32 = 1e-5;

    fn transform_node(comp_id: CompId, layer_id: LayerId) -> Node {
        Node::new(
            deterministic_node_id(comp_id, layer_id, NodeRole::Transform),
            "comp.transform",
        )
        .with_input("input", &[DataTypeId::FRAME_BUFFER])
        .with_input("parent_transform", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
    }

    /// An evaluator standing in as the `EvalScope`, carrying a document whose
    /// single layer has the transform `setup` applies.
    fn scope_with(setup: impl FnOnce(&mut Layer)) -> (Evaluator, Node) {
        let comp_id = CompId::new(1);
        let layer_id = LayerId::new(1);
        let mut layer = Layer::new(layer_id, "Layer", Graph::new());
        setup(&mut layer);
        let comp = Composition::new(comp_id, "Comp", CANVAS, FPS, 300).add_layer(layer);
        let mut scope = Evaluator::new();
        scope.set_document(Arc::new(Document::default().with_composition(comp)));
        (scope, transform_node(comp_id, layer_id))
    }

    fn constant(v: f32) -> AnimationChannel {
        AnimationChannel::constant(v)
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, CANVAS)
    }

    /// A source whose alpha steps 0 → 0.5 → 1 from the border inwards, each
    /// ring a different colour. A solid opaque source interpolates the same way
    /// premultiplied or not — exactly the false positive the plan warns about.
    fn ringed_fb(width: u32, height: u32) -> FrameBuffer {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let edge = x.min(y).min(width - 1 - x).min(height - 1 - y);
                let px = match edge {
                    0 => [0.9, 0.1, 0.1, 0.0],
                    1 => [0.1, 0.9, 0.2, 0.5],
                    _ => [0.2, 0.3, 1.0, 1.0],
                };
                data.extend_from_slice(&px);
            }
        }
        FrameBuffer::from_f32(width, height, data)
    }

    fn solid_fb(width: u32, height: u32, rgba: [f32; 4]) -> FrameBuffer {
        let n = (width * height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for _ in 0..n {
            data.extend_from_slice(&rgba);
        }
        FrameBuffer::from_f32(width, height, data)
    }

    fn run_cpu(setup: impl FnOnce(&mut Layer), input: Arc<dyn NodeData>) -> Arc<dyn NodeData> {
        let (mut scope, node) = scope_with(setup);
        CompTransformProcessor
            .process(
                &node,
                &ctx(),
                &[Some(input)],
                &ResolvedParams::default(),
                &mut scope,
            )
            .expect("cpu transform")
    }

    fn run_gpu(
        gpu: &GpuContext,
        setup: impl FnOnce(&mut Layer),
        input: Arc<dyn NodeData>,
    ) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let (mut scope, node) = scope_with(setup);
        let processor = CompTransformGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        processor
            .process(
                &node,
                &ctx(),
                &[Some(input)],
                &ResolvedParams::default(),
                &mut scope,
            )
            .expect("gpu transform")
    }

    fn readback(out: &Arc<dyn NodeData>) -> FrameBuffer {
        out.downcast_ref::<GpuFrameBuffer>()
            .expect("gpu path stays resident")
            .to_frame_buffer()
            .expect("readback")
    }

    fn gpu_or_skip() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    fn alpha_at(fb: &FrameBuffer, x: u32, y: u32) -> f32 {
        fb.as_f32()[((y * fb.width + x) * 4 + 3) as usize]
    }

    /// Compare both paths for one transform, reporting the worst channel.
    fn assert_paths_agree(gpu: &GpuContext, label: &str, setup: fn(&mut Layer)) {
        let input: Arc<dyn NodeData> = Arc::new(ringed_fb(8, 8));
        let cpu = run_cpu(setup, input.clone());
        let cpu = cpu
            .downcast_ref::<FrameBuffer>()
            .expect("cpu path stays on the CPU");
        let out = readback(&run_gpu(gpu, setup, input));

        assert_eq!((out.width, out.height), (cpu.width, cpu.height), "{label}");
        let out_px = out.as_f32();
        let cpu_px = cpu.as_f32();
        let mut worst = 0.0f32;
        let mut worst_at = 0usize;
        for (i, (g, c)) in out_px.iter().zip(cpu_px.iter()).enumerate() {
            let d = (g - c).abs();
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }
        assert!(
            worst <= EPS,
            "{label}: worst difference {worst} at channel {} of pixel {} (gpu {}, cpu {})",
            worst_at % 4,
            worst_at / 4,
            out_px[worst_at],
            cpu_px[worst_at],
        );
        assert!(
            out_px.iter().any(|v| *v > 0.0),
            "{label}: the comparison would pass on two blank frames"
        );
    }

    #[test]
    fn gpu_matches_the_cpu_reference_for_translation() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        // Fractional offsets: an integer shift would never interpolate.
        assert_paths_agree(&gpu, "translate", |layer| {
            layer.transform.position = [constant(3.5), constant(2.25)];
        });
    }

    #[test]
    fn gpu_matches_the_cpu_reference_for_rotation() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        assert_paths_agree(&gpu, "rotate", |layer| {
            layer.transform.anchor_point = [constant(4.0), constant(4.0)];
            layer.transform.position = [constant(8.0), constant(8.0)];
            layer.transform.rotation = constant(31.0);
        });
    }

    #[test]
    fn gpu_matches_the_cpu_reference_for_scale() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        assert_paths_agree(&gpu, "scale up", |layer| {
            layer.transform.scale = [constant(1.7), constant(2.3)];
        });
        assert_paths_agree(&gpu, "scale down", |layer| {
            layer.transform.scale = [constant(0.6), constant(0.45)];
        });
    }

    #[test]
    fn gpu_matches_the_cpu_reference_for_an_anchor_move() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        // The anchor alone changes where the content lands.
        assert_paths_agree(&gpu, "anchor", |layer| {
            layer.transform.anchor_point = [constant(2.5), constant(6.0)];
        });
        assert_paths_agree(&gpu, "anchor + rotate + scale", |layer| {
            layer.transform.anchor_point = [constant(4.0), constant(4.0)];
            layer.transform.position = [constant(9.25), constant(7.5)];
            layer.transform.scale = [constant(1.4), constant(0.8)];
            layer.transform.rotation = constant(-24.0);
        });
    }

    /// Taps outside the source must read as transparent instead of being
    /// clamped to the edge texel: a 2× upscale of an opaque source has to fade
    /// out over the boundary exactly the way the CPU reference does. Clamping
    /// keeps the border opaque and shows up here.
    #[test]
    fn the_source_edge_fades_the_way_the_cpu_reference_does() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let opaque: Arc<dyn NodeData> = Arc::new(solid_fb(4, 4, [1.0, 1.0, 1.0, 1.0]));
        let setup: fn(&mut Layer) = |layer| {
            layer.transform.scale = [constant(2.0), constant(2.0)];
            layer.transform.position = [constant(4.0), constant(4.0)];
        };

        let cpu = run_cpu(setup, opaque.clone());
        let cpu = cpu.downcast_ref::<FrameBuffer>().expect("cpu frame");
        let out = readback(&run_gpu(&gpu, setup, opaque));

        // The source maps to [4, 12) on both axes: one pixel in is opaque, the
        // boundary pixel is a partial — which is what clamping would destroy —
        // and well outside is transparent.
        let boundary = alpha_at(cpu, 3, 8);
        assert!(
            boundary > 0.0 && boundary < 1.0,
            "the CPU reference itself must ramp at the edge, got {boundary}"
        );
        assert!(
            (alpha_at(&out, 3, 8) - boundary).abs() <= EPS,
            "boundary alpha: gpu {}, cpu {boundary}",
            alpha_at(&out, 3, 8)
        );
        assert!(
            (alpha_at(&out, 5, 8) - 1.0).abs() <= EPS,
            "inside stays opaque"
        );
        assert_eq!(alpha_at(&out, 1, 8), 0.0, "well outside stays transparent");

        for (i, (g, c)) in out.as_f32().iter().zip(cpu.as_f32().iter()).enumerate() {
            assert!(
                (g - c).abs() <= EPS,
                "channel {} of pixel {} differs: gpu {g}, cpu {c}",
                i % 4,
                i / 4
            );
        }
    }

    /// An identity transform must return the very same `Arc`:
    /// `shape_layer_golden`'s first case pins pixels that only stay fixed
    /// while the whole shell chain passes its input through.
    #[test]
    fn identity_returns_the_input_unchanged() {
        let input: Arc<dyn NodeData> = Arc::new(ringed_fb(8, 8));
        let cpu = run_cpu(|_| {}, input.clone());
        assert!(
            Arc::ptr_eq(&cpu, &input),
            "the CPU reference must short-circuit"
        );

        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let out = run_gpu(&gpu, |_| {}, input.clone());
        assert!(Arc::ptr_eq(&out, &input), "the GPU path must short-circuit");
    }

    /// Zero scale is singular: the layer collapses instead of erroring.
    #[test]
    fn zero_scale_collapses_to_a_transparent_frame() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let input: Arc<dyn NodeData> = Arc::new(ringed_fb(8, 8));
        let out = run_gpu(
            &gpu,
            |layer| layer.transform.scale = [constant(0.0), constant(0.0)],
            input,
        );
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("a collapsed layer yields a CPU transparent frame");
        assert_eq!((fb.width, fb.height), CANVAS);
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }

    /// Null layers keep a transform node with nothing feeding it.
    #[test]
    fn missing_input_is_a_transparent_frame() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let (mut scope, node) = scope_with(|layer| {
            layer.transform.position = [constant(3.0), constant(3.0)];
        });
        let processor = CompTransformGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        let out = processor
            .process(&node, &ctx(), &[], &ResolvedParams::default(), &mut scope)
            .expect("gpu transform");
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("a missing input yields a CPU transparent frame");
        assert_eq!((fb.width, fb.height), CANVAS);
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }

    /// The output covers the canvas even when the source frame is a different
    /// size — the reason this is a separate shader from `transform.wgsl`.
    #[test]
    fn output_covers_the_canvas_not_the_source() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let input: Arc<dyn NodeData> = Arc::new(ringed_fb(4, 6));
        let out = run_gpu(
            &gpu,
            |layer| layer.transform.position = [constant(2.0), constant(2.0)],
            input,
        );
        let frame = out
            .downcast_ref::<GpuFrameBuffer>()
            .expect("gpu path stays resident");
        assert_eq!((frame.width(), frame.height()), CANVAS);
    }
}

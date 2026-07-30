// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.opacity` — the shell's layer opacity (REQ-LAYER-001).
//!
//! Multiplies the frame's alpha channel by the owning layer's animatable
//! opacity, evaluated at the layer's local frame. The layer is read from the
//! [`Document`] at process time (never captured at construction).
//!
//! Two processors implement the same arithmetic:
//! [`CompOpacityGpuProcessor`] is the default path (`processor_for_node`) and
//! keeps the frame resident in VRAM; [`CompOpacityProcessor`] is the CPU
//! reference the golden tests register explicitly. Their outputs are compared
//! pixel-exactly in this module's tests — a straight alpha multiply has no
//! rounding difference between the two.
//!
//! [`Document`]: ravel_core::composition::Document

use ravel_core::composition::compile::NodeRole;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

use super::{layer_local_frame, shell_layer, transparent};
use crate::gpu_util;
use crate::gpu_util::ensure_cpu;

const SHADER_SRC: &str = include_str!("../shaders/comp_opacity.wgsl");

/// The layer opacity that applies to `ctx`, or `None` when the layer is fully
/// opaque and the node is a pass-through.
///
/// Shared by both processors so the short-circuit threshold cannot drift
/// between the GPU path and the CPU reference.
fn shell_opacity(
    node: &Node,
    ctx: &EvalContext,
    scope: &mut dyn EvalScope,
) -> anyhow::Result<Option<f32>> {
    let (comp, layer_id) = shell_layer(node, scope, NodeRole::Opacity)?;
    let layer = comp
        .get_layer(layer_id)
        .ok_or_else(|| anyhow::anyhow!("comp.opacity: layer {layer_id:?} missing"))?;

    let lf = layer_local_frame(layer, ctx);
    let opacity = layer.opacity.evaluate(lf, ctx).clamp(0.0, 1.0);
    if (opacity - 1.0).abs() < 1e-6 {
        return Ok(None);
    }
    Ok(Some(opacity))
}

// ===========================================================================
// CPU reference
// ===========================================================================

pub struct CompOpacityProcessor;

impl CompOpacityProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompOpacityProcessor {
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

        let Some(opacity) = shell_opacity(node, ctx, scope)? else {
            return Ok(input);
        };

        let source = ensure_cpu(input.as_ref())?;
        let mut pixels = source.as_f32().into_owned();
        for px in pixels.chunks_exact_mut(4) {
            px[3] *= opacity;
        }
        Ok(Arc::new(FrameBuffer::from_f32(
            source.width,
            source.height,
            pixels,
        )))
    }

    fn is_time_dependent(&self) -> bool {
        // The layer opacity channel is a hidden (document-side) dependency.
        true
    }
}

// ===========================================================================
// GPU path
// ===========================================================================

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    opacity: f32,
    _pad: [f32; 3],
}

/// Scales the frame's alpha on the GPU, keeping the result resident so the
/// rest of the shell chain does not have to read it back.
pub struct CompOpacityGpuProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompOpacityGpuProcessor {
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
        // Shared across every shell opacity node: the pipeline depends only on
        // the shader and the layout, never on this node.
        let pipeline = shaders
            .compute_pipeline(
                "comp_opacity",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_opacity.wgsl compilation failed");

        Self {
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompOpacityGpuProcessor {
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

        // A fully opaque layer returns its input untouched — the same
        // short-circuit the CPU reference takes. `shape_layer_golden`'s first
        // case depends on the whole shell chain passing pixels through.
        let Some(opacity) = shell_opacity(node, ctx, scope)? else {
            return Ok(input);
        };

        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("comp.opacity: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let shader_params = Params {
            opacity,
            _pad: [0.0; 3],
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("comp_opacity params"),
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
                label: Some("comp_opacity"),
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
                    label: Some("comp_opacity"),
                });
        self.pipeline
            .dispatch(&mut encoder, &bind_group, width, height);
        self.ctx.queue().submit(Some(encoder.finish()));

        image.release(&self.pool);

        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            width,
            height,
        )))
    }

    fn is_time_dependent(&self) -> bool {
        // The layer opacity channel is a hidden (document-side) dependency.
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

    /// A shell opacity node carrying the deterministic id the processors
    /// decode to find their layer.
    fn opacity_node(comp_id: CompId, layer_id: LayerId) -> Node {
        Node::new(
            deterministic_node_id(comp_id, layer_id, NodeRole::Opacity),
            "comp.opacity",
        )
        .with_input("input", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
    }

    /// An evaluator standing in as the `EvalScope`, carrying a document whose
    /// single layer has the given opacity.
    fn scope_with_opacity(opacity: f32) -> (Evaluator, Node) {
        let comp_id = CompId::new(1);
        let layer_id = LayerId::new(1);
        let mut layer = Layer::new(layer_id, "Layer", Graph::new());
        layer.opacity = AnimationChannel::constant(opacity);
        let comp = Composition::new(comp_id, "Comp", (4, 4), FPS, 300).add_layer(layer);
        let mut scope = Evaluator::new();
        scope.set_document(Arc::new(Document::default().with_composition(comp)));
        (scope, opacity_node(comp_id, layer_id))
    }

    /// A gradient of alphas and colors: a solid frame would hide a shader that
    /// scaled the wrong channel.
    fn ramp_fb(width: u32, height: u32) -> FrameBuffer {
        let n = (width * height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for i in 0..n {
            let t = i as f32 / n as f32;
            data.extend_from_slice(&[t, 1.0 - t, 0.25 + 0.5 * t, t]);
        }
        FrameBuffer::from_f32(width, height, data)
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, (4, 4))
    }

    fn run_cpu(opacity: f32, input: Arc<dyn NodeData>) -> Arc<dyn NodeData> {
        let (mut scope, node) = scope_with_opacity(opacity);
        CompOpacityProcessor
            .process(
                &node,
                &ctx(),
                &[Some(input)],
                &ResolvedParams::default(),
                &mut scope,
            )
            .expect("cpu opacity")
    }

    fn run_gpu(gpu: &GpuContext, opacity: f32, input: Arc<dyn NodeData>) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let (mut scope, node) = scope_with_opacity(opacity);
        let processor = CompOpacityGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        processor
            .process(
                &node,
                &ctx(),
                &[Some(input)],
                &ResolvedParams::default(),
                &mut scope,
            )
            .expect("gpu opacity")
    }

    /// GPU tests need an adapter; skip where there is none (the pattern in
    /// `ravel-gpu/tests/compute_invert.rs`).
    fn gpu_or_skip() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    #[test]
    fn gpu_matches_the_cpu_reference_pixel_for_pixel() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let input: Arc<dyn NodeData> = Arc::new(ramp_fb(8, 8));

        for opacity in [0.0, 0.25, 0.5, 0.75, 0.999] {
            let cpu = run_cpu(opacity, input.clone());
            let cpu = cpu
                .downcast_ref::<FrameBuffer>()
                .expect("cpu path stays on the CPU");
            let gpu_out = run_gpu(&gpu, opacity, input.clone());
            let gpu_out = gpu_out
                .downcast_ref::<GpuFrameBuffer>()
                .expect("gpu path stays resident")
                .to_frame_buffer()
                .expect("readback");

            assert_eq!((gpu_out.width, gpu_out.height), (cpu.width, cpu.height));
            // A straight alpha multiply is the same f32 operation on both
            // paths, so this is an equality — not a tolerance — comparison.
            for (i, (g, c)) in gpu_out.as_f32().iter().zip(cpu.as_f32().iter()).enumerate() {
                assert_eq!(
                    g,
                    c,
                    "channel {} of pixel {} differs at opacity {opacity}",
                    i % 4,
                    i / 4
                );
            }
        }
    }

    #[test]
    fn gpu_scales_alpha_only() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let input: Arc<dyn NodeData> = Arc::new(ramp_fb(4, 4));
        let out = run_gpu(&gpu, 0.5, input);
        let fb = out
            .downcast_ref::<GpuFrameBuffer>()
            .expect("gpu path stays resident")
            .to_frame_buffer()
            .expect("readback");

        let source = ramp_fb(4, 4);
        let px = fb.as_f32();
        let src = source.as_f32();
        for px_i in 0..16usize {
            let base = px_i * 4;
            for ch in 0..3 {
                assert_eq!(
                    px[base + ch],
                    src[base + ch],
                    "rgb must be untouched at pixel {px_i}"
                );
            }
            assert_eq!(
                px[base + 3],
                src[base + 3] * 0.5,
                "alpha must be scaled at pixel {px_i}"
            );
        }
    }

    /// A fully opaque layer must return the very same `Arc`: the golden test
    /// `shape_layer_network_rasterizes_rect_pixels` pins pixels that only stay
    /// fixed because all three shell nodes pass their input through.
    #[test]
    fn opacity_one_returns_the_input_unchanged() {
        let input: Arc<dyn NodeData> = Arc::new(ramp_fb(4, 4));

        let cpu = run_cpu(1.0, input.clone());
        assert!(
            Arc::ptr_eq(&cpu, &input),
            "the CPU reference must short-circuit"
        );

        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let out = run_gpu(&gpu, 1.0, input.clone());
        assert!(Arc::ptr_eq(&out, &input), "the GPU path must short-circuit");
    }

    /// Null layers keep a shell chain with nothing feeding it.
    #[test]
    fn missing_input_is_a_transparent_frame() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let (mut scope, node) = scope_with_opacity(0.5);
        let processor = CompOpacityGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        let out = processor
            .process(&node, &ctx(), &[], &ResolvedParams::default(), &mut scope)
            .expect("gpu opacity");

        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("a missing input yields a CPU transparent frame");
        assert_eq!((fb.width, fb.height), ctx().resolution);
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }

    /// A GPU-resident input is consumed without a round trip through CPU
    /// memory: the whole point of the unit is that the chain stays resident.
    #[test]
    fn a_resident_input_stays_on_the_gpu() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let source = ramp_fb(4, 4);
        let key = gpu_util::tex_key_rw(source.width, source.height);
        let pooled = pool.lock().unwrap().acquire(key);
        ravel_gpu::upload_texture(
            &gpu,
            &pooled.texture,
            key,
            bytemuck::cast_slice(&source.data),
        );
        let resident: Arc<dyn NodeData> = Arc::new(GpuFrameBuffer::new(
            gpu.clone(),
            &pool,
            pooled,
            source.width,
            source.height,
        ));

        let out = run_gpu(&gpu, 0.5, resident);
        let fb = out
            .downcast_ref::<GpuFrameBuffer>()
            .expect("gpu path stays resident")
            .to_frame_buffer()
            .expect("readback");
        let px = fb.as_f32();
        let src = source.as_f32();
        for p in 0..16usize {
            assert_eq!(px[p * 4 + 3], src[p * 4 + 3] * 0.5);
        }
    }
}

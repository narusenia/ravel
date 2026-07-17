// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! 2D affine transform (GPU).
//!
//! Applies translate, rotate, scale via inverse-mapping with bilinear sampling.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

const SHADER_SRC: &str = include_str!("shaders/transform.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    inv_m00: f32,
    inv_m01: f32,
    inv_m02: f32,
    inv_m10: f32,
    inv_m11: f32,
    inv_m12: f32,
    width: f32,
    height: f32,
}

pub struct TransformProcessor {
    ctx: GpuContext,
    pipeline: ComputePipeline,
    pool: Arc<Mutex<TexturePool>>,
}

impl TransformProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        let compiled = shaders
            .compile_source("transform", SHADER_SRC)
            .expect("transform.wgsl compilation failed");

        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        let pipeline =
            ComputePipeline::new(&ctx, &compiled, "main", &layout, gpu_util::WORKGROUP_SIZE);

        Self {
            pool,
            ctx,
            pipeline,
        }
    }

    fn compute_inverse_params(&self, width: u32, height: u32, params: &ResolvedParams) -> Params {
        let translate_x = params.f32_or("translate_x", 0.0);
        let translate_y = params.f32_or("translate_y", 0.0);
        let rotation = params.f32_or("rotation", 0.0);
        let scale = params.f32_or("scale", 1.0);

        let cx = width as f32 / 2.0;
        let cy = height as f32 / 2.0;

        let cos_r = rotation.cos();
        let sin_r = rotation.sin();

        // Forward: translate center to origin → scale → rotate → translate back + user translate.
        // M = T(cx+tx, cy+ty) * R(θ) * S(s) * T(-cx, -cy)
        //
        // Inverse: T(cx, cy) * S(1/s) * R(-θ) * T(-cx-tx, -cy-ty)
        let inv_s = if scale.abs() > 1e-7 { 1.0 / scale } else { 1.0 };

        // Inverse rotation.
        let ic = cos_r * inv_s;
        let is = -sin_r * inv_s;

        // Compose: pixel (x,y) → source (sx,sy)
        // Step 1: subtract (cx+tx, cy+ty)
        // Step 2: inv_rotate_scale
        // Step 3: add (cx, cy)
        let ox = cx + translate_x;
        let oy = cy + translate_y;

        // inv_m * (x - ox) + cx
        // = inv_m * x - inv_m * o + c
        Params {
            inv_m00: ic,
            inv_m01: is,
            inv_m02: -ic * ox - is * oy + cx,
            inv_m10: -is,
            inv_m11: ic,
            inv_m12: is * ox - ic * oy + cy,
            width: width as f32,
            height: height as f32,
        }
    }
}

impl NodeProcessor for TransformProcessor {
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
            .ok_or_else(|| anyhow::anyhow!("transform: expected FrameBuffer input"))?;
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("transform: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let inv_params = self.compute_inverse_params(width, height, params);
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("transform params"),
                contents: bytemuck::bytes_of(&inv_params),
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
                label: Some("transform"),
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
                    label: Some("transform"),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameBuffer, FrameRate};
    use std::sync::Arc;

    fn make_transform_node(tx: f32, ty: f32, rotation: f32, scale: f32) -> Node {
        Node::new(NodeId::new(1), "transform")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("translate_x", ParameterValue::Float(tx))
            .with_param("translate_y", ParameterValue::Float(ty))
            .with_param("rotation", ParameterValue::Float(rotation))
            .with_param("scale", ParameterValue::Float(scale))
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
        EvalContext::new(0, FrameRate::new(30, 1), (8, 8))
    }

    fn solid_fb(width: u32, height: u32, r: f32, g: f32, b: f32, a: f32) -> FrameBuffer {
        let pixel_count = (width * height) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            data.extend_from_slice(&[r, g, b, a]);
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
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

    /// Evaluate a transform node fed by `input` through a real evaluator.
    fn run_transform(
        tx: f32,
        ty: f32,
        rotation: f32,
        scale: f32,
        input: FrameBuffer,
    ) -> FrameBuffer {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_transform_node(tx, ty, rotation, scale);
        let pool = test_pool(&gpu);
        let source = Node::new(NodeId::new(2), "test.source");
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
            Arc::new(TransformProcessor::new(gpu, &mut shaders, pool, &node)),
        );
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        readback(out.as_ref())
    }

    #[test]
    fn identity_transform_preserves_image() {
        let fb = run_transform(0.0, 0.0, 0.0, 1.0, solid_fb(8, 8, 0.5, 0.3, 0.8, 1.0));

        assert_eq!(fb.width, 8);
        assert_eq!(fb.height, 8);
        for i in 0..64 {
            let base = i * 4;
            assert!(
                (fb.data[base] - 0.5).abs() < 0.02,
                "r mismatch at pixel {i}"
            );
        }
    }

    #[test]
    fn large_translate_produces_transparent_pixels() {
        let fb = run_transform(100.0, 100.0, 0.0, 1.0, solid_fb(8, 8, 1.0, 1.0, 1.0, 1.0));

        // All pixels should be transparent (source fully outside).
        for i in 0..64 {
            let base = i * 4;
            assert!(
                fb.data[base + 3] < 0.01,
                "expected transparent at pixel {i}, got alpha={}",
                fb.data[base + 3]
            );
        }
    }
}

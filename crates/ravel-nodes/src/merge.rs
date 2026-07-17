// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Alpha compositing merge (GPU).
//!
//! Blends two FrameBuffer inputs using over, add, or multiply modes.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

const SHADER_SRC: &str = include_str!("shaders/merge.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    operation: u32,
    mix_val: f32,
    _pad0: f32,
    _pad1: f32,
}

fn operation_to_u32(op: &str) -> u32 {
    match op {
        "add" => 1,
        "multiply" => 2,
        _ => 0, // "over" default
    }
}

pub struct MergeProcessor {
    ctx: GpuContext,
    pipeline: ComputePipeline,
    pool: Arc<Mutex<TexturePool>>,
}

impl MergeProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        let compiled = shaders
            .compile_source("merge", SHADER_SRC)
            .expect("merge.wgsl compilation failed");

        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry(2),
            gpu_util::uniform_layout_entry(3),
        ];
        let pipeline =
            ComputePipeline::new(&ctx, &compiled, "main", &layout, gpu_util::WORKGROUP_SIZE);

        Self {
            pool,
            ctx,
            pipeline,
        }
    }
}

impl NodeProcessor for MergeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let input_a = inputs
            .first()
            .and_then(|i| i.clone())
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input A"))?;
        let input_b = inputs
            .get(1)
            .and_then(|i| i.clone())
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input B"))?;

        // Validate before adapting so no pool texture is uploaded and then
        // abandoned on the error path.
        let (width, height) = gpu_util::frame_size(input_a.as_ref())
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input A"))?;
        let size_b = gpu_util::frame_size(input_b.as_ref())
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input B"))?;
        if size_b != (width, height) {
            anyhow::bail!(
                "merge: input dimensions must match (A={}x{}, B={}x{})",
                width,
                height,
                size_b.0,
                size_b.1
            );
        }

        let tex_a = gpu_util::ensure_gpu(&self.ctx, &self.pool, input_a.as_ref())
            .map_err(|e| anyhow::anyhow!("merge (input A): {e}"))?;
        let tex_b = gpu_util::ensure_gpu(&self.ctx, &self.pool, input_b.as_ref())
            .map_err(|e| anyhow::anyhow!("merge (input B): {e}"))?;

        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let shader_params = Params {
            operation: operation_to_u32(params.str_or("operation", "")),
            mix_val: params.f32_or("mix", 1.0),
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("merge params"),
                contents: bytemuck::bytes_of(&shader_params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let view_a = tex_a
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_b = tex_b
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = self
            .ctx
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("merge"),
                layout: self.pipeline.bind_group_layout(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view_a),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view_b),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&output_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: param_buf.as_entire_binding(),
                    },
                ],
            });

        let mut encoder =
            self.ctx
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("merge"),
                });
        self.pipeline
            .dispatch(&mut encoder, &bind_group, width, height);
        self.ctx.queue().submit(Some(encoder.finish()));

        tex_a.release(&self.pool);
        tex_b.release(&self.pool);

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

    fn make_merge_node(operation: &str, mix: f32) -> Node {
        Node::new(NodeId::new(1), "merge")
            .with_input("A", &[DataTypeId::FRAME_BUFFER])
            .with_input("B", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("operation", ParameterValue::String(operation.into()))
            .with_param("mix", ParameterValue::Float(mix))
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

    /// Evaluate a merge node fed by `a`/`b` through a real evaluator.
    fn run_merge(operation: &str, mix: f32, a: FrameBuffer, b: FrameBuffer) -> FrameBuffer {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_merge_node(operation, mix);
        let pool = test_pool(&gpu);
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "test.source")
                    .with_output("out", DataTypeId::FRAME_BUFFER),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(3), "test.source")
                    .with_output("out", DataTypeId::FRAME_BUFFER),
            )
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
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(1),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), Arc::new(FbSource(a)));
        ev.register(NodeId::new(3), Arc::new(FbSource(b)));
        ev.register(
            NodeId::new(1),
            Arc::new(MergeProcessor::new(gpu, &mut shaders, pool, &node)),
        );
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        readback(out.as_ref())
    }

    #[test]
    fn over_opaque_a_covers_b() {
        let fb = run_merge(
            "over",
            1.0,
            solid_fb(4, 4, 1.0, 0.0, 0.0, 1.0),
            solid_fb(4, 4, 0.0, 1.0, 0.0, 1.0),
        );

        // Opaque A should fully cover B.
        for i in 0..16 {
            let base = i * 4;
            assert!((fb.data[base] - 1.0).abs() < 0.01, "r at pixel {i}");
            assert!(fb.data[base + 1] < 0.01, "g at pixel {i}");
        }
    }

    #[test]
    fn add_mode_sums_colors() {
        let fb = run_merge(
            "add",
            1.0,
            solid_fb(4, 4, 0.3, 0.0, 0.0, 1.0),
            solid_fb(4, 4, 0.0, 0.5, 0.0, 1.0),
        );

        assert!((fb.data[0] - 0.3).abs() < 0.01);
        assert!((fb.data[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn multiply_mode() {
        let fb = run_merge(
            "multiply",
            1.0,
            solid_fb(4, 4, 0.5, 0.5, 0.5, 1.0),
            solid_fb(4, 4, 0.8, 0.6, 0.4, 1.0),
        );

        assert!((fb.data[0] - 0.4).abs() < 0.01); // 0.5 * 0.8
        assert!((fb.data[1] - 0.3).abs() < 0.01); // 0.5 * 0.6
        assert!((fb.data[2] - 0.2).abs() < 0.01); // 0.5 * 0.4
    }
}

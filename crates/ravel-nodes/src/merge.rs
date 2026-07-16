// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Alpha compositing merge (GPU).
//!
//! Blends two FrameBuffer inputs using over, add, or multiply modes.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::graph::{Node, ParameterValue};
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
    operation: u32,
    mix_val: f32,
}

impl MergeProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        node: &Node,
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

        let operation = node
            .parameters
            .iter()
            .find(|p| p.key == "operation")
            .and_then(|p| match &p.value {
                ParameterValue::String(s) => Some(operation_to_u32(s)),
                _ => None,
            })
            .unwrap_or(0);

        let mix_val = node
            .parameters
            .iter()
            .find(|p| p.key == "mix")
            .and_then(|p| match &p.value {
                ParameterValue::Float(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(1.0);

        Self {
            pool,
            ctx,
            pipeline,
            operation,
            mix_val,
        }
    }
}

impl NodeProcessor for MergeProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let input_a = *inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input A"))?;
        let input_b = *inputs
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input B"))?;

        // Validate before adapting so no pool texture is uploaded and then
        // abandoned on the error path.
        let (width, height) = gpu_util::frame_size(input_a)
            .ok_or_else(|| anyhow::anyhow!("merge: expected FrameBuffer for input A"))?;
        let size_b = gpu_util::frame_size(input_b)
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

        let tex_a = gpu_util::ensure_gpu(&self.ctx, &self.pool, input_a)
            .map_err(|e| anyhow::anyhow!("merge (input A): {e}"))?;
        let tex_b = gpu_util::ensure_gpu(&self.ctx, &self.pool, input_b)
            .map_err(|e| anyhow::anyhow!("merge (input B): {e}"))?;

        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let params = Params {
            operation: self.operation,
            mix_val: self.mix_val,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("merge params"),
                contents: bytemuck::bytes_of(&params),
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

        Ok(Box::new(GpuFrameBuffer::new(
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
    use ravel_core::id::{DataTypeId, NodeId};
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

    #[test]
    fn over_opaque_a_covers_b() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_merge_node("over", 1.0);
        let pool = test_pool(&gpu);
        let proc = MergeProcessor::new(gpu, &mut shaders, pool, &node);

        let a = solid_fb(4, 4, 1.0, 0.0, 0.0, 1.0);
        let b = solid_fb(4, 4, 0.0, 1.0, 0.0, 1.0);
        let refs: Vec<&dyn NodeData> = vec![&a, &b];
        let out = proc.process(&ctx(), &refs).unwrap();
        let fb = readback(out.as_ref());

        // Opaque A should fully cover B.
        for i in 0..16 {
            let base = i * 4;
            assert!((fb.data[base] - 1.0).abs() < 0.01, "r at pixel {i}");
            assert!(fb.data[base + 1] < 0.01, "g at pixel {i}");
        }
    }

    #[test]
    fn add_mode_sums_colors() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_merge_node("add", 1.0);
        let pool = test_pool(&gpu);
        let proc = MergeProcessor::new(gpu, &mut shaders, pool, &node);

        let a = solid_fb(4, 4, 0.3, 0.0, 0.0, 1.0);
        let b = solid_fb(4, 4, 0.0, 0.5, 0.0, 1.0);
        let refs: Vec<&dyn NodeData> = vec![&a, &b];
        let out = proc.process(&ctx(), &refs).unwrap();
        let fb = readback(out.as_ref());

        assert!((fb.data[0] - 0.3).abs() < 0.01);
        assert!((fb.data[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn multiply_mode() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_merge_node("multiply", 1.0);
        let pool = test_pool(&gpu);
        let proc = MergeProcessor::new(gpu, &mut shaders, pool, &node);

        let a = solid_fb(4, 4, 0.5, 0.5, 0.5, 1.0);
        let b = solid_fb(4, 4, 0.8, 0.6, 0.4, 1.0);
        let refs: Vec<&dyn NodeData> = vec![&a, &b];
        let out = proc.process(&ctx(), &refs).unwrap();
        let fb = readback(out.as_ref());

        assert!((fb.data[0] - 0.4).abs() < 0.01); // 0.5 * 0.8
        assert!((fb.data[1] - 0.3).abs() < 0.01); // 0.5 * 0.6
        assert!((fb.data[2] - 0.2).abs() < 0.01); // 0.5 * 0.4
    }
}

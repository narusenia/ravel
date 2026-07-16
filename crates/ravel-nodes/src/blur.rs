// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Gaussian blur filter (GPU, 2-pass separable).

use crate::gpu_util;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::NodeData;
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

const SHADER_SRC: &str = include_str!("shaders/blur.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    radius: i32,
    horizontal: u32,
    sigma: f32,
    _pad: f32,
}

pub struct BlurProcessor {
    ctx: GpuContext,
    pipeline: ComputePipeline,
    pool: Arc<Mutex<TexturePool>>,
    radius: f32,
}

impl BlurProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        node: &Node,
    ) -> Self {
        let compiled = shaders
            .compile_source("blur", SHADER_SRC)
            .expect("blur.wgsl compilation failed");

        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        let pipeline =
            ComputePipeline::new(&ctx, &compiled, "main", &layout, gpu_util::WORKGROUP_SIZE);

        let radius = node
            .parameters
            .iter()
            .find(|p| p.key == "radius")
            .and_then(|p| match &p.value {
                ParameterValue::Float(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(5.0);

        Self {
            pool,
            ctx,
            pipeline,
            radius,
        }
    }

    fn dispatch_pass(
        &self,
        input: &wgpu::Texture,
        output: &wgpu::Texture,
        width: u32,
        height: u32,
        horizontal: bool,
    ) {
        let radius_int = self.radius.round().max(0.0) as i32;
        let sigma = self.radius.max(0.001) / 3.0;

        let params = Params {
            radius: radius_int,
            horizontal: if horizontal { 1 } else { 0 },
            sigma,
            _pad: 0.0,
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = self
            .ctx
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
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
                    label: Some("blur"),
                });
        self.pipeline
            .dispatch(&mut encoder, &bind_group, width, height);
        self.ctx.queue().submit(Some(encoder.finish()));
    }
}

impl NodeProcessor for BlurProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let input = *inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("blur: expected FrameBuffer input"))?;
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input)
            .map_err(|e| anyhow::anyhow!("blur: {e}"))?;
        let (width, height) = image.size();

        let (intermediate, output_tex) = {
            let mut pool = self.pool.lock().unwrap();
            let key = gpu_util::tex_key_rw(width, height);
            (pool.acquire(key), pool.acquire(key))
        };

        // Pass 1: horizontal
        self.dispatch_pass(image.texture(), &intermediate.texture, width, height, true);
        // Pass 2: vertical
        self.dispatch_pass(
            &intermediate.texture,
            &output_tex.texture,
            width,
            height,
            false,
        );

        // Return temporaries to the pool; queue ordering keeps the queued
        // reads valid even if they are reused by a later submission.
        self.pool.lock().unwrap().release(intermediate);
        image.release(&self.pool);

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

    fn make_blur_node(radius: f32) -> Node {
        Node::new(NodeId::new(1), "blur")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("radius", ParameterValue::Float(radius))
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (8, 8))
    }

    fn checkerboard_fb(width: u32, height: u32) -> FrameBuffer {
        let pixel_count = (width * height) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for y in 0..height {
            for x in 0..width {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                data.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    fn test_pool(gpu: &GpuContext) -> Arc<Mutex<TexturePool>> {
        Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)))
    }

    fn readback(out: &dyn NodeData) -> FrameBuffer {
        out.downcast_ref::<GpuFrameBuffer>()
            .expect("blur outputs a GPU-resident frame")
            .to_frame_buffer()
            .expect("readback")
    }

    #[test]
    fn blur_smooths_checkerboard() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_blur_node(2.0);
        let pool = test_pool(&gpu);
        let proc = BlurProcessor::new(gpu, &mut shaders, pool, &node);

        let input = checkerboard_fb(8, 8);
        let input_ref: &dyn NodeData = &input;
        let out = proc.process(&ctx(), &[input_ref]).unwrap();
        let fb = readback(out.as_ref());

        assert_eq!(fb.width, 8);
        assert_eq!(fb.height, 8);

        // After blur, all center pixels should be closer to 0.5 than before.
        let center = 4 * (3 * 8 + 3); // pixel (3,3)
        let val = fb.data[center];
        assert!(
            (val - 0.5).abs() < 0.3,
            "blurred center pixel should be near 0.5, got {val}"
        );
    }

    #[test]
    fn zero_radius_preserves_image() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_blur_node(0.0);
        let pool = test_pool(&gpu);
        let proc = BlurProcessor::new(gpu, &mut shaders, pool, &node);

        let input = checkerboard_fb(8, 8);
        let input_ref: &dyn NodeData = &input;
        let out = proc.process(&ctx(), &[input_ref]).unwrap();
        let fb = readback(out.as_ref());

        for i in 0..fb.data.len() {
            assert!(
                (fb.data[i] - input.data[i]).abs() < 0.01,
                "pixel mismatch at index {i}"
            );
        }
    }
}

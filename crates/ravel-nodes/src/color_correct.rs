// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color correction filter (GPU).
//!
//! Adjusts brightness, contrast, and saturation per-pixel via a compute shader.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::NodeData;
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

const SHADER_SRC: &str = include_str!("shaders/color_correct.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    _pad: f32,
}

pub struct ColorCorrectProcessor {
    ctx: GpuContext,
    pipeline: ComputePipeline,
    pool: Arc<Mutex<TexturePool>>,
    brightness: f32,
    contrast: f32,
    saturation: f32,
}

impl ColorCorrectProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        node: &Node,
    ) -> Self {
        let compiled = shaders
            .compile_source("color_correct", SHADER_SRC)
            .expect("color_correct.wgsl compilation failed");

        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        let pipeline =
            ComputePipeline::new(&ctx, &compiled, "main", &layout, gpu_util::WORKGROUP_SIZE);

        let brightness = param_f32(node, "brightness", 0.0);
        let contrast = param_f32(node, "contrast", 1.0);
        let saturation = param_f32(node, "saturation", 1.0);

        Self {
            pool,
            ctx,
            pipeline,
            brightness,
            contrast,
            saturation,
        }
    }
}

impl NodeProcessor for ColorCorrectProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let input = *inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("color_correct: expected FrameBuffer input"))?;
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input)
            .map_err(|e| anyhow::anyhow!("color_correct: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let params = Params {
            brightness: self.brightness,
            contrast: self.contrast,
            saturation: self.saturation,
            _pad: 0.0,
        };
        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("color_correct params"),
                contents: bytemuck::bytes_of(&params),
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
                label: Some("color_correct"),
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
                    label: Some("color_correct"),
                });
        self.pipeline
            .dispatch(&mut encoder, &bind_group, width, height);
        self.ctx.queue().submit(Some(encoder.finish()));

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

fn param_f32(node: &Node, key: &str, default: f32) -> f32 {
    node.parameters
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Float(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, NodeId};
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
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    #[test]
    fn identity_preserves_image() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_color_correct_node(0.0, 1.0, 1.0);
        let pool = test_pool(&gpu);
        let proc = ColorCorrectProcessor::new(gpu, &mut shaders, pool, &node);

        let input = solid_fb(4, 4, 0.5, 0.3, 0.8, 1.0);
        let input_ref: &dyn NodeData = &input;
        let out = proc.process(&ctx(), &[input_ref]).unwrap();
        let fb = readback(out.as_ref());

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        for i in 0..16 {
            let base = i * 4;
            assert!(
                (fb.data[base] - 0.5).abs() < 0.01,
                "r mismatch at pixel {i}"
            );
            assert!(
                (fb.data[base + 1] - 0.3).abs() < 0.01,
                "g mismatch at pixel {i}"
            );
            assert!(
                (fb.data[base + 2] - 0.8).abs() < 0.01,
                "b mismatch at pixel {i}"
            );
            assert!(
                (fb.data[base + 3] - 1.0).abs() < 0.01,
                "a mismatch at pixel {i}"
            );
        }
    }

    #[test]
    fn brightness_shifts_values() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_color_correct_node(0.2, 1.0, 1.0);
        let pool = test_pool(&gpu);
        let proc = ColorCorrectProcessor::new(gpu, &mut shaders, pool, &node);

        let input = solid_fb(4, 4, 0.5, 0.5, 0.5, 1.0);
        let input_ref: &dyn NodeData = &input;
        let out = proc.process(&ctx(), &[input_ref]).unwrap();
        let fb = readback(out.as_ref());

        assert!((fb.data[0] - 0.7).abs() < 0.01);
        assert!((fb.data[1] - 0.7).abs() < 0.01);
        assert!((fb.data[2] - 0.7).abs() < 0.01);
    }
}

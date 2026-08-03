// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color correction filter (GPU).
//!
//! Adjusts brightness, contrast, and saturation per-pixel via a compute shader.

use crate::gpu_util;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

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
        let pipeline = shaders
            .compute_pipeline(
                "color_correct",
                SHADER_SRC,
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

        let shader_params = Params {
            brightness: params.f32_or("brightness", 0.0),
            contrast: params.f32_or("contrast", 1.0),
            saturation: params.f32_or("saturation", 1.0),
            _pad: 0.0,
        };
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
}

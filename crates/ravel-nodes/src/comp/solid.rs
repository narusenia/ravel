// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.solid` — a single colour over the whole evaluation frame.
//!
//! A user-placed generator with no input. The frame is
//! [`EvalContext::resolution`] (the same rule as `rasterize`), so it follows
//! the composition and the preview scale. The colour is written as given:
//! frames carry straight alpha, so an RGBA colour is stored without
//! premultiplication. The result is left resident in VRAM.
//!
//! **Not a shell node**: unlike `comp.opacity` / `comp.merge.*` this node does
//! not decode a deterministic node id and never reads the `Document`.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_solid.wgsl");

/// Colour a `comp.solid` node holds when the parameter is missing (a node
/// saved before the parameter existed).
pub(crate) const DEFAULT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    color: [f32; 4],
    width: u32,
    height: u32,
    _pad: [u32; 2],
}

pub struct CompSolidProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompSolidProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        // No input texture: the output is binding 0, the uniform binding 1.
        let layout = [
            gpu_util::output_storage_layout_entry(0),
            gpu_util::uniform_layout_entry(1),
        ];
        let pipeline = shaders
            .compute_pipeline(
                "comp_solid",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_solid.wgsl compilation failed");
        Self {
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompSolidProcessor {
    /// Nothing is captured from the node; the colour is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = ctx.resolution;
        anyhow::ensure!(
            width > 0 && height > 0,
            "comp.solid: evaluation resolution {width}x{height} is empty"
        );
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let shader_params = Params {
            color: params.vec4_or("color", DEFAULT_COLOR),
            width,
            height,
            _pad: [0; 2],
        };
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "comp_solid",
            pipeline: &self.pipeline,
            inputs: &[],
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
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
    use ravel_core::eval::ResolvedValue;
    use ravel_core::types::{FrameBuffer, FrameRate};

    fn run(
        gpu: &GpuContext,
        resolution: (u32, u32),
        color: [f32; 4],
    ) -> anyhow::Result<FrameBuffer> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let node = Node::new(ravel_core::id::NodeId::new(1), "comp.solid");
        let processor = CompSolidProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        let mut params = ResolvedParams::default();
        params.set("color", ResolvedValue::Vec4(color));
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), resolution);
        let out = processor.process(
            &node,
            &ctx,
            &[],
            &params,
            &mut ravel_core::eval::Evaluator::new(),
        )?;
        out.downcast_ref::<GpuFrameBuffer>()
            .expect("the solid stays resident")
            .to_frame_buffer()
            .map_err(Into::into)
    }

    #[test]
    fn every_pixel_is_the_given_color_with_no_input() {
        let Some(gpu) = GpuContext::new_blocking().ok() else {
            return;
        };
        // Straight alpha: the colour is stored as given, not premultiplied.
        let color = [0.25, 0.5, 0.75, 0.5];
        let fb = run(&gpu, (13, 7), color).expect("solid");
        assert_eq!((fb.width, fb.height), (13, 7));
        assert_eq!(fb.as_f32().len(), 13 * 7 * 4);
        for (i, px) in fb.as_f32().chunks_exact(4).enumerate() {
            assert_eq!(px, color, "pixel {i}");
        }
    }

    #[test]
    fn the_size_follows_the_evaluation_resolution() {
        let Some(gpu) = GpuContext::new_blocking().ok() else {
            return;
        };
        for resolution in [(1, 1), (4, 9), (16, 2)] {
            let fb = run(&gpu, resolution, [1.0, 0.0, 0.0, 1.0]).expect("solid");
            assert_eq!((fb.width, fb.height), resolution);
        }
    }

    #[test]
    fn an_empty_resolution_is_an_error_not_a_panic() {
        let Some(gpu) = GpuContext::new_blocking().ok() else {
            return;
        };
        assert!(run(&gpu, (0, 4), [1.0; 4]).is_err());
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Gaussian blur filter (GPU, 2-pass separable).

use crate::gpu_util;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TextureBinding,
    TexturePool,
};
use std::sync::{Arc, Mutex};

const SHADER_SRC: &str = include_str!("shaders/blur.wgsl");

fn sanitized_radius(radius: f32) -> f32 {
    if radius.is_finite() {
        radius.clamp(0.0, ravel_core::registry::builtin::MAX_BLUR_RADIUS)
    } else {
        0.0
    }
}

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
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl BlurProcessor {
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
        // Shared across every `blur` node: the pipeline depends only on the
        // shader and the layout, never on this node.
        let source = gpu_util::with_premultiplied_helpers(SHADER_SRC);
        let pipeline = shaders
            .compute_pipeline("blur", &source, "main", &layout, gpu_util::WORKGROUP_SIZE)
            .expect("blur.wgsl compilation failed");

        Self {
            pool,
            ctx,
            pipeline,
        }
    }

    fn dispatch_pass(
        &self,
        input: &TextureBinding,
        output: &TextureBinding,
        width: u32,
        height: u32,
        horizontal: bool,
        radius: f32,
    ) {
        let radius = sanitized_radius(radius);
        let radius_int = radius.round() as i32;
        let sigma = radius.max(0.001) / 3.0;

        let params = Params {
            radius: radius_int,
            horizontal: if horizontal { 1 } else { 0 },
            sigma,
            _pad: 0.0,
        };
        // Both passes record into the frame's shared encoder: one blur submits
        // once with the rest of the frame, not twice.
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "blur",
            pipeline: &self.pipeline,
            inputs: std::slice::from_ref(input),
            output,
            uniform: bytemuck::bytes_of(&params),
            width,
            height,
        });
    }
}

impl NodeProcessor for BlurProcessor {
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
            .ok_or_else(|| anyhow::anyhow!("blur: expected FrameBuffer input"))?;
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("blur: {e}"))?;
        let (width, height) = image.size();

        let (intermediate, output_tex) = {
            let mut pool = self.pool.lock().unwrap();
            let key = gpu_util::tex_key_rw(width, height);
            (pool.acquire(key), pool.acquire(key))
        };

        let radius = params.f32_or("radius", 5.0);

        let input_binding = image.binding();
        let intermediate_binding = intermediate.binding();
        let output_binding = output_tex.binding();

        // Pass 1: horizontal
        self.dispatch_pass(
            &input_binding,
            &intermediate_binding,
            width,
            height,
            true,
            radius,
        );
        // Pass 2: vertical
        self.dispatch_pass(
            &intermediate_binding,
            &output_binding,
            width,
            height,
            false,
            radius,
        );

        // Return temporaries to the pool; the pool keeps them out of
        // circulation until the batched reads are flushed.
        self.pool.lock().unwrap().release(intermediate);
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

    fn make_blur_node(radius: f32) -> Node {
        Node::new(NodeId::new(1), "blur")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("radius", ParameterValue::Float(radius))
    }

    #[test]
    fn radius_is_bounded_and_non_finite_values_are_neutralized() {
        assert_eq!(sanitized_radius(-1.0), 0.0);
        assert_eq!(sanitized_radius(f32::NAN), 0.0);
        assert_eq!(sanitized_radius(f32::INFINITY), 0.0);
        assert_eq!(
            sanitized_radius(50_000.0),
            ravel_core::registry::builtin::MAX_BLUR_RADIUS
        );
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
        FrameBuffer::from_f32(width, height, data)
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

    /// Evaluate a blur node fed by `input` through a real evaluator.
    fn run_blur(radius: f32, input: FrameBuffer) -> FrameBuffer {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = make_blur_node(radius);
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
            Arc::new(BlurProcessor::new(gpu, &mut shaders, pool, &node)),
        );
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        readback(out.as_ref())
    }

    #[test]
    fn blur_smooths_checkerboard() {
        let fb = run_blur(2.0, checkerboard_fb(8, 8));

        assert_eq!(fb.width, 8);
        assert_eq!(fb.height, 8);

        // After blur, all center pixels should be closer to 0.5 than before.
        let center = 4 * (3 * 8 + 3); // pixel (3,3)
        let val = fb.as_f32()[center];
        assert!(
            (val - 0.5).abs() < 0.3,
            "blurred center pixel should be near 0.5, got {val}"
        );
    }

    /// A frame that is opaque white on the left and fully transparent on the
    /// right — the RGB of the transparent half is 0, as a cleared buffer's is.
    fn half_opaque_fb(width: u32, height: u32) -> FrameBuffer {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..height {
            for x in 0..width {
                if x < width / 2 {
                    data.extend_from_slice(&[1.0, 1.0, 1.0, 1.0]);
                } else {
                    data.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
                }
            }
        }
        FrameBuffer::from_f32(width, height, data)
    }

    /// Issue MED-GPU-02: convolving straight-alpha RGBA lets the transparent
    /// half's black bleed into the opaque half, leaving a dark halo along the
    /// boundary. Filtering in premultiplied alpha keeps the colour of every
    /// partially transparent pixel at the source colour — white here — and only
    /// the alpha ramps.
    #[test]
    fn alpha_boundary_does_not_darken() {
        let fb = run_blur(3.0, half_opaque_fb(16, 4));

        let px = fb.as_f32();
        for y in 0..4 {
            for x in 0..16 {
                let base = ((y * 16 + x) * 4) as usize;
                let a = px[base + 3];
                if a <= 0.0 {
                    continue;
                }
                for (ch, name) in ["r", "g", "b"].iter().enumerate() {
                    assert!(
                        (px[base + ch] - 1.0).abs() < 1e-4,
                        "{name} darkened to {} at ({x}, {y}) where alpha is {a}",
                        px[base + ch]
                    );
                }
            }
        }

        // And the blur did reach across the boundary, so the check above was
        // not vacuous: the first transparent column picked up alpha.
        let boundary = px[(8 * 4 + 3) as usize];
        assert!(
            boundary > 0.0 && boundary < 1.0,
            "expected a partial alpha at the boundary, got {boundary}"
        );
    }

    #[test]
    fn zero_radius_preserves_image() {
        let input = checkerboard_fb(8, 8);
        let fb = run_blur(0.0, input.clone());

        let px = fb.as_f32();
        let input_px = input.as_f32();
        for i in 0..px.len() {
            assert!(
                (px[i] - input_px[i]).abs() < 0.01,
                "pixel mismatch at index {i}"
            );
        }
    }
}

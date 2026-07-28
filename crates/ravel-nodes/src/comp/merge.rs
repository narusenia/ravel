// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.merge.*` — layer compositing for the shell's merge chain
//! (REQ-LAYER-001/010).
//!
//! Straight-alpha Porter-Duff *over* with per-mode color blending (the W3C
//! compositing model: the foreground color is mixed with `B(Cb, Cf)` by the
//! backdrop's alpha before compositing). `comp.merge.adjustment` instead
//! mixes the adjusted stack over the original background with the layer's
//! opacity as effect strength (REQ-LAYER-010).
//!
//! Two processors implement the same arithmetic:
//! [`CompMergeGpuProcessor`] is the default path (`processor_for_node`) and
//! keeps the merged frame resident in VRAM — this is the node that used to
//! force a readback per layer, so it is where the shell chain stops touching
//! CPU memory at all; [`CompMergeProcessor`] is the CPU reference the golden
//! tests register explicitly. Their outputs are compared in this module's
//! tests, within a tolerance: the compositing arithmetic is a sum of products
//! the GPU may contract into FMAs, so it is not bit-identical.

use ravel_core::composition::compile::{NodeRole, decode_deterministic_node_id};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

use super::{layer_local_frame, transparent};
use crate::gpu_util;
use crate::gpu_util::{GpuImage, ensure_cpu};

const SHADER_SRC: &str = include_str!("../shaders/comp_merge.wgsl");
const ADJUSTMENT_SHADER_SRC: &str = include_str!("../shaders/comp_merge_adjustment.wgsl");

#[derive(Clone, Copy, PartialEq)]
enum MergeMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    Adjustment,
}

impl MergeMode {
    /// The `mode` discriminant `comp_merge.wgsl` switches on.
    fn shader_index(self) -> anyhow::Result<u32> {
        Ok(match self {
            MergeMode::Normal => 0,
            MergeMode::Add => 1,
            MergeMode::Multiply => 2,
            MergeMode::Screen => 3,
            MergeMode::Overlay => 4,
            // Whole-frame mix, not a per-pixel composite: it has its own path.
            MergeMode::Adjustment => {
                anyhow::bail!("comp.merge: adjustment does not use the compositing shader")
            }
        })
    }
}

fn merge_mode(type_key: &str) -> anyhow::Result<MergeMode> {
    Ok(match type_key {
        "comp.merge.normal" => MergeMode::Normal,
        "comp.merge.add" => MergeMode::Add,
        "comp.merge.multiply" => MergeMode::Multiply,
        "comp.merge.screen" => MergeMode::Screen,
        "comp.merge.overlay" => MergeMode::Overlay,
        "comp.merge.adjustment" => MergeMode::Adjustment,
        other => anyhow::bail!("comp.merge: unknown type key {other}"),
    })
}

/// Per-channel color blend `B(Cb, Cf)` on straight colors.
fn blend(mode: MergeMode, cb: f32, cf: f32) -> f32 {
    match mode {
        MergeMode::Normal => cf,
        MergeMode::Add => cb + cf,
        MergeMode::Multiply => cb * cf,
        MergeMode::Screen => cb + cf - cb * cf,
        MergeMode::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cf
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cf)
            }
        }
        // Adjustment merges mix whole frames; see `mix_frames`.
        MergeMode::Adjustment => cf,
    }
}

/// What a compositing merge does with its two inputs at this frame.
///
/// Shared by both processors so the short-circuits — which the golden tests
/// and `layer_network.rs` depend on — cannot drift between the GPU path and
/// the CPU reference.
enum Blend {
    /// Neither side is present: a transparent canvas.
    Transparent,
    /// Only one side is present and needs no resizing — or is not a frame at
    /// all (a scalar probe) — so it passes through untouched.
    PassThrough(Arc<dyn NodeData>),
    /// Composite the two sides over the output canvas. A `None` side reads as
    /// transparent everywhere.
    Composite {
        background: Option<Arc<dyn NodeData>>,
        foreground: Option<Arc<dyn NodeData>>,
    },
}

/// Reduce the two merge inputs to the case both paths act on.
///
/// Compositing against transparency is the color identity for every mode, but
/// a lone side must still be normalized to the composition resolution — a
/// single video layer may carry the media's native dimensions.
fn shell_blend(
    ctx: &EvalContext,
    background: Option<Arc<dyn NodeData>>,
    foreground: Option<Arc<dyn NodeData>>,
) -> Blend {
    match (&background, &foreground) {
        (None, None) => return Blend::Transparent,
        (None, Some(only)) | (Some(only), None) => match frame_dims(only.as_ref()) {
            // Undersized/oversized frames are padded/cropped by compositing.
            Some(dims) if dims != ctx.resolution => {}
            // Right-sized frames — and non-frame values — pass through.
            _ => return Blend::PassThrough(only.clone()),
        },
        (Some(_), Some(_)) => {}
    }
    Blend::Composite {
        background,
        foreground,
    }
}

// ===========================================================================
// CPU reference
// ===========================================================================

pub struct CompMergeProcessor;

impl CompMergeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompMergeProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mode = merge_mode(&node.type_key)?;
        // inputs[0] = background, inputs[1] = foreground.
        let background = inputs.first().and_then(|i| i.clone());
        let foreground = inputs.get(1).and_then(|i| i.clone());

        if mode == MergeMode::Adjustment {
            return merge_adjustment(node, ctx, background, foreground, scope);
        }

        let (background, foreground) = match shell_blend(ctx, background, foreground) {
            Blend::Transparent => return Ok(transparent(ctx)),
            Blend::PassThrough(only) => return Ok(only),
            Blend::Composite {
                background,
                foreground,
            } => (
                background.unwrap_or_else(empty_frame),
                foreground.unwrap_or_else(empty_frame),
            ),
        };

        let bg = ensure_cpu(background.as_ref())?;
        let fg = ensure_cpu(foreground.as_ref())?;
        let (width, height) = ctx.resolution;
        let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let b = pixel_at(&bg, x, y);
                let f = pixel_at(&fg, x, y);
                let out = composite(mode, b, f);
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&out);
            }
        }
        Ok(Arc::new(FrameBuffer {
            width,
            height,
            data: pixels.into(),
        }))
    }

    fn is_time_dependent(&self) -> bool {
        // Display-interval checks and the adjustment strength read the
        // document per frame.
        true
    }
}

/// Straight-alpha compositing of foreground `f` over background `b` with the
/// mode's color blend applied where the two overlap.
fn composite(mode: MergeMode, b: [f32; 4], f: [f32; 4]) -> [f32; 4] {
    let ab = b[3];
    let af = f[3];
    let ao = af + ab * (1.0 - af);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        let blended = blend(mode, b[c], f[c]);
        let mixed = (1.0 - ab) * f[c] + ab * blended;
        out[c] = (af * mixed + (1.0 - af) * ab * b[c]) / ao;
    }
    out[3] = ao;
    out
}

/// Adjustment layer merge: `mix(background, adjusted, opacity)` where
/// `adjusted` is the layer network's output over the lower stack and the
/// layer's opacity acts as effect strength. Outside the layer's display
/// interval the background passes through untouched (REQ-LAYER-010).
fn merge_adjustment(
    node: &Node,
    ctx: &EvalContext,
    background: Option<Arc<dyn NodeData>>,
    foreground: Option<Arc<dyn NodeData>>,
    scope: &mut dyn EvalScope,
) -> anyhow::Result<Arc<dyn NodeData>> {
    let background = background.unwrap_or_else(|| transparent(ctx));

    let (foreground, strength) = match shell_adjustment(node, ctx, foreground, scope) {
        Adjust::Background => return Ok(background),
        Adjust::Foreground(foreground) => return Ok(foreground),
        Adjust::Mix(foreground, strength) => (foreground, strength),
    };

    let bg = ensure_cpu(background.as_ref())?;
    let fg = ensure_cpu(foreground.as_ref())?;
    let (width, height) = ctx.resolution;
    let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let b = premultiply(pixel_at(&bg, x, y));
            let f = premultiply(pixel_at(&fg, x, y));
            let mut mixed = [0.0f32; 4];
            for c in 0..4 {
                mixed[c] = b[c] * (1.0 - strength) + f[c] * strength;
            }
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx..idx + 4].copy_from_slice(&unpremultiply(mixed));
        }
    }
    Ok(Arc::new(FrameBuffer {
        width,
        height,
        data: pixels.into(),
    }))
}

/// What an adjustment merge does with its two inputs at this frame. Shared by
/// both processors so the bypass thresholds cannot drift.
enum Adjust {
    /// The background passes through: outside the display interval, no
    /// foreground, or zero strength.
    Background,
    /// Full strength on a right-sized foreground: it passes through.
    Foreground(Arc<dyn NodeData>),
    /// Mix this foreground into the background in premultiplied alpha at this
    /// strength.
    Mix(Arc<dyn NodeData>, f32),
}

fn shell_adjustment(
    node: &Node,
    ctx: &EvalContext,
    foreground: Option<Arc<dyn NodeData>>,
    scope: &mut dyn EvalScope,
) -> Adjust {
    let Some(strength) = adjustment_strength(node, ctx, scope) else {
        // Outside the display interval (or the layer vanished): bypass.
        return Adjust::Background;
    };
    let Some(foreground) = foreground else {
        return Adjust::Background;
    };
    if strength <= 0.0 {
        return Adjust::Background;
    }
    if (strength - 1.0).abs() < 1e-6 && frame_dims(foreground.as_ref()) == Some(ctx.resolution) {
        return Adjust::Foreground(foreground);
    }
    Adjust::Mix(foreground, strength)
}

/// The adjustment layer's opacity at the current frame, or `None` when the
/// layer is outside its display interval (bypass) or cannot be resolved.
fn adjustment_strength(node: &Node, ctx: &EvalContext, scope: &mut dyn EvalScope) -> Option<f32> {
    let (comp_id, layer_id, role) = decode_deterministic_node_id(node.id)?;
    if role != NodeRole::Merge {
        return None;
    }
    let document = scope.document()?;
    let comp = document.get_composition(comp_id)?;
    let layer = comp.get_layer(layer_id)?;
    let local = ctx.frame as i64 - layer.start_frame + layer.in_frame as i64;
    if local < layer.in_frame as i64 || local >= layer.out_frame as i64 {
        return None;
    }
    let lf = layer_local_frame(layer, ctx);
    Some(layer.opacity.evaluate(lf, ctx).clamp(0.0, 1.0))
}

/// Dimensions of a CPU- or GPU-resident frame, without any readback.
fn frame_dims(value: &dyn NodeData) -> Option<(u32, u32)> {
    if let Some(fb) = value.downcast_ref::<FrameBuffer>() {
        return Some((fb.width, fb.height));
    }
    value
        .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
        .map(|fb| (fb.width(), fb.height()))
}

/// Zero-sized stand-in for a missing merge input: `pixel_at` reads it as
/// fully transparent everywhere.
fn empty_frame() -> Arc<dyn NodeData> {
    Arc::new(FrameBuffer::new_zeroed(0, 0))
}

fn pixel_at(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    if x >= fb.width || y >= fb.height {
        return [0.0; 4];
    }
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.data[idx..idx + 4].try_into().unwrap_or([0.0; 4])
}

fn premultiply(p: [f32; 4]) -> [f32; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

fn unpremultiply(p: [f32; 4]) -> [f32; 4] {
    if p[3] > 0.0 {
        [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]]
    } else {
        [0.0; 4]
    }
}

// ===========================================================================
// GPU path
// ===========================================================================

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    bg_width: u32,
    bg_height: u32,
    fg_width: u32,
    fg_height: u32,
    out_width: u32,
    out_height: u32,
    mode: u32,
    _pad: u32,
}

/// `comp_merge_adjustment.wgsl`'s uniform: the same dimensions, with the
/// effect strength in place of the blend mode.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AdjustmentParams {
    bg_width: u32,
    bg_height: u32,
    fg_width: u32,
    fg_height: u32,
    out_width: u32,
    out_height: u32,
    strength: f32,
    _pad: u32,
}

/// One merge input adapted for binding.
///
/// A merge side can be *absent*, which the CPU reference represents with a 0x0
/// `empty_frame`. Zero-sized textures cannot be created, so an absent side
/// binds a pooled 1x1 stand-in and reports dimensions `(0, 0)` in the uniform:
/// every coordinate then fails the shader's bounds check and reads as
/// transparent, so the stand-in's contents are never sampled. Carrying the
/// absence in the dimensions rather than in a separate flag is what keeps the
/// shader's out-of-bounds reading and the CPU's `pixel_at` a single rule.
struct Side<'a> {
    image: GpuImage<'a>,
    /// Dimensions the shader reads with; `(0, 0)` marks an absent side.
    dims: (u32, u32),
}

/// The same arithmetic as [`CompMergeProcessor`], dispatched over the output
/// canvas and left resident in VRAM.
pub struct CompMergeGpuProcessor {
    ctx: GpuContext,
    composite: Arc<ComputePipeline>,
    adjustment: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompMergeGpuProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        // Both shaders take the same bindings: background, foreground, output,
        // uniform.
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry(2),
            gpu_util::uniform_layout_entry(3),
        ];
        // Shared across every shell merge node: the pipelines depend only on
        // the shader and the layout, never on this node (the blend mode
        // arrives in the uniform, so all five compositing type keys share one
        // pipeline).
        let composite = shaders
            .compute_pipeline(
                "comp_merge",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_merge.wgsl compilation failed");
        let adjustment = shaders
            .compute_pipeline(
                "comp_merge_adjustment",
                &gpu_util::with_premultiplied_helpers(ADJUSTMENT_SHADER_SRC),
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_merge_adjustment.wgsl compilation failed");

        Self {
            ctx,
            composite,
            adjustment,
            pool,
        }
    }

    /// Adapt one side for binding, standing in for an absent input.
    fn side<'a>(&self, input: Option<&'a dyn NodeData>) -> anyhow::Result<Side<'a>> {
        match input {
            Some(value) => {
                let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, value)
                    .map_err(|e| anyhow::anyhow!("comp.merge: {e}"))?;
                let dims = image.size();
                Ok(Side { image, dims })
            }
            None => {
                let texture = self
                    .pool
                    .lock()
                    .unwrap()
                    .acquire(gpu_util::tex_key_rw(1, 1));
                Ok(Side {
                    image: GpuImage::Uploaded {
                        texture,
                        width: 1,
                        height: 1,
                    },
                    // Absent: never read. See `Side`.
                    dims: (0, 0),
                })
            }
        }
    }

    /// Adapt both sides, returning the first side's pooled texture if the
    /// second cannot be adapted.
    ///
    /// A non-frame value can reach one merge input while the other carries a
    /// real frame — `shell_blend` only passes a non-frame value through when it
    /// is the *lone* side — so this error path runs once per evaluation for as
    /// long as the graph stays in that state. [`PooledTexture`] has no `Drop`
    /// that returns it, so a plain `?` after the first `side()` would destroy
    /// one uploaded texture per failed evaluation instead of reusing it.
    ///
    /// [`PooledTexture`]: ravel_gpu::PooledTexture
    fn sides<'a>(
        &self,
        background: Option<&'a dyn NodeData>,
        foreground: Option<&'a dyn NodeData>,
    ) -> anyhow::Result<(Side<'a>, Side<'a>)> {
        let bg = self.side(background)?;
        match self.side(foreground) {
            Ok(fg) => Ok((bg, fg)),
            Err(err) => {
                bg.image.release(&self.pool);
                Err(err)
            }
        }
    }

    /// Bind both sides plus `params` and dispatch over the output canvas.
    fn run(
        &self,
        pipeline: &ComputePipeline,
        label: &str,
        out: (u32, u32),
        bg: Side<'_>,
        fg: Side<'_>,
        params: &[u8],
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (out_width, out_height) = out;
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(out_width, out_height));

        let param_buf = self
            .ctx
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: params,
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let bg_view = bg
            .image
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let fg_view = fg
            .image
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = self
            .ctx
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: pipeline.bind_group_layout(),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&bg_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&fg_view),
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

        let mut encoder = self
            .ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        // Dispatch covers the output canvas, not either input.
        pipeline.dispatch(&mut encoder, &bind_group, out_width, out_height);
        self.ctx.queue().submit(Some(encoder.finish()));

        bg.image.release(&self.pool);
        fg.image.release(&self.pool);

        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            out_width,
            out_height,
        )))
    }

    fn composite(
        &self,
        mode: MergeMode,
        ctx: &EvalContext,
        background: Option<&dyn NodeData>,
        foreground: Option<&dyn NodeData>,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (bg, fg) = self.sides(background, foreground)?;
        let (out_width, out_height) = ctx.resolution;
        let params = Params {
            bg_width: bg.dims.0,
            bg_height: bg.dims.1,
            fg_width: fg.dims.0,
            fg_height: fg.dims.1,
            out_width,
            out_height,
            mode: mode.shader_index()?,
            _pad: 0,
        };
        self.run(
            &self.composite,
            "comp_merge",
            ctx.resolution,
            bg,
            fg,
            bytemuck::bytes_of(&params),
        )
    }

    fn adjustment(
        &self,
        node: &Node,
        ctx: &EvalContext,
        background: Option<Arc<dyn NodeData>>,
        foreground: Option<Arc<dyn NodeData>>,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (foreground, strength) = match shell_adjustment(node, ctx, foreground, scope) {
            // A missing background is a transparent canvas, the same default
            // the CPU reference applies.
            Adjust::Background => return Ok(background.unwrap_or_else(|| transparent(ctx))),
            Adjust::Foreground(foreground) => return Ok(foreground),
            Adjust::Mix(foreground, strength) => (foreground, strength),
        };

        // A missing background stays absent rather than being materialised as
        // a transparent frame and uploaded: the shader reads both the same way.
        let (bg, fg) = self.sides(background.as_deref(), Some(foreground.as_ref()))?;
        let (out_width, out_height) = ctx.resolution;
        let params = AdjustmentParams {
            bg_width: bg.dims.0,
            bg_height: bg.dims.1,
            fg_width: fg.dims.0,
            fg_height: fg.dims.1,
            out_width,
            out_height,
            strength,
            _pad: 0,
        };
        self.run(
            &self.adjustment,
            "comp_merge_adjustment",
            ctx.resolution,
            bg,
            fg,
            bytemuck::bytes_of(&params),
        )
    }
}

impl NodeProcessor for CompMergeGpuProcessor {
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
        let mode = merge_mode(&node.type_key)?;
        // inputs[0] = background, inputs[1] = foreground.
        let background = inputs.first().and_then(|i| i.clone());
        let foreground = inputs.get(1).and_then(|i| i.clone());

        if mode == MergeMode::Adjustment {
            return self.adjustment(node, ctx, background, foreground, scope);
        }

        // Every short-circuit is the CPU reference's: `shape_layer_golden`
        // and `layer_network.rs` pin pixels that only stay fixed while a
        // one-sided merge passes its input straight through.
        let (background, foreground) = match shell_blend(ctx, background, foreground) {
            Blend::Transparent => return Ok(transparent(ctx)),
            Blend::PassThrough(only) => return Ok(only),
            Blend::Composite {
                background,
                foreground,
            } => (background, foreground),
        };

        self.composite(mode, ctx, background.as_deref(), foreground.as_deref())
    }

    fn is_time_dependent(&self) -> bool {
        // Display-interval checks and the adjustment strength read the
        // document per frame.
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
    use ravel_core::types::{FrameRate, Scalar};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    /// The five compositing modes. `comp.merge.adjustment` mixes whole frames
    /// and is covered separately.
    const COMPOSITING_KEYS: [&str; 5] = [
        "comp.merge.normal",
        "comp.merge.add",
        "comp.merge.multiply",
        "comp.merge.screen",
        "comp.merge.overlay",
    ];

    /// A shell merge node carrying the deterministic id the processors decode
    /// to find their layer.
    fn merge_node(type_key: &str) -> Node {
        Node::new(
            deterministic_node_id(CompId::new(1), LayerId::new(1), NodeRole::Merge),
            type_key,
        )
        .with_input("background", &[DataTypeId::FRAME_BUFFER])
        .with_input("foreground", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
    }

    fn ctx(width: u32, height: u32) -> EvalContext {
        EvalContext::new(0, FPS, (width, height))
    }

    const CHANNELS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    /// A frame with alphas from `alphas` and channels straddling 0.5.
    ///
    /// Two opaque frames would pin Porter-Duff's denominator at 1 and hide a
    /// wrongly ordered two-stage composite; channels on one side of 0.5 alone
    /// would leave one of Overlay's two branches untested.
    fn translucent_fb(width: u32, height: u32, alphas: [f32; 4], shift: usize) -> FrameBuffer {
        let n = (width * height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for i in 0..n {
            data.extend_from_slice(&[
                CHANNELS[(i + shift) % 5],
                CHANNELS[(i / 2 + shift) % 5],
                CHANNELS[(i / 3 + shift) % 5],
                alphas[i % 4],
            ]);
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    fn bg_fb(width: u32, height: u32) -> FrameBuffer {
        translucent_fb(width, height, [0.0, 0.5, 1.0, 0.5], 0)
    }

    /// The foreground's alphas are offset from the background's so every
    /// combination occurs — including both sides transparent, which is the
    /// only way to reach the `ao <= 0` branch.
    fn fg_fb(width: u32, height: u32) -> FrameBuffer {
        translucent_fb(width, height, [0.0, 1.0, 0.5, 0.25], 2)
    }

    /// GPU tests need an adapter; skip where there is none (the pattern in
    /// `ravel-gpu/tests/compute_invert.rs`).
    fn gpu_or_skip() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    fn run_cpu_in(
        scope: &mut dyn EvalScope,
        type_key: &str,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
    ) -> Arc<dyn NodeData> {
        CompMergeProcessor
            .process(
                &merge_node(type_key),
                ctx,
                inputs,
                &ResolvedParams::default(),
                scope,
            )
            .expect("cpu merge")
    }

    fn run_gpu_in(
        gpu: &GpuContext,
        scope: &mut dyn EvalScope,
        type_key: &str,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
    ) -> Arc<dyn NodeData> {
        let node = merge_node(type_key);
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let processor = CompMergeGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        processor
            .process(&node, ctx, inputs, &ResolvedParams::default(), scope)
            .expect("gpu merge")
    }

    /// The compositing modes never touch the document, so a bare evaluator is
    /// enough of an `EvalScope` for them.
    fn run_cpu(
        type_key: &str,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
    ) -> Arc<dyn NodeData> {
        run_cpu_in(&mut Evaluator::new(), type_key, ctx, inputs)
    }

    fn run_gpu(
        gpu: &GpuContext,
        type_key: &str,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
    ) -> Arc<dyn NodeData> {
        run_gpu_in(gpu, &mut Evaluator::new(), type_key, ctx, inputs)
    }

    /// The compositing arithmetic is a sum of products the GPU may contract
    /// into FMAs while the CPU rounds every step, so the two agree to a
    /// tolerance rather than bit-exactly (unlike `comp.opacity`).
    const TOLERANCE: f32 = 1e-5;

    fn assert_close(actual: &FrameBuffer, expected: &FrameBuffer, what: &str) {
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height),
            "{what}: dimensions differ"
        );
        for (i, (a, e)) in actual.data.iter().zip(expected.data.iter()).enumerate() {
            assert!(
                (a - e).abs() <= TOLERANCE,
                "{what}: channel {} of pixel {} differs: gpu {a} vs cpu {e}",
                i % 4,
                i / 4
            );
        }
    }

    /// Read either representation back for comparison.
    fn frame(value: &Arc<dyn NodeData>) -> FrameBuffer {
        ensure_cpu(value.as_ref())
            .expect("a frame result")
            .into_owned()
    }

    /// Guards the fixtures the mode comparison relies on: without both sides
    /// of the Overlay midpoint and without a pixel where both alphas are zero,
    /// that test would pass while leaving shader branches unexecuted.
    #[test]
    fn the_fixtures_cover_the_branchy_cases() {
        let bg = bg_fb(8, 8);
        let fg = fg_fb(8, 8);
        let backdrop: Vec<f32> = bg.data.chunks_exact(4).map(|p| p[0]).collect();
        assert!(
            backdrop.iter().any(|c| *c <= 0.5) && backdrop.iter().any(|c| *c > 0.5),
            "the backdrop must straddle Overlay's midpoint"
        );
        assert!(
            bg.data
                .chunks_exact(4)
                .zip(fg.data.chunks_exact(4))
                .any(|(b, f)| b[3] == 0.0 && f[3] == 0.0),
            "some pixel must leave both sides transparent (the ao <= 0 branch)"
        );
    }

    #[test]
    fn gpu_matches_the_cpu_reference_for_every_blend_mode() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let inputs: Vec<Option<Arc<dyn NodeData>>> =
            vec![Some(Arc::new(bg_fb(8, 8))), Some(Arc::new(fg_fb(8, 8)))];

        for key in COMPOSITING_KEYS {
            let cpu = frame(&run_cpu(key, &ctx, &inputs));
            let out = run_gpu(&gpu, key, &ctx, &inputs);
            assert!(
                out.downcast_ref::<GpuFrameBuffer>().is_some(),
                "{key}: the merged frame must stay resident — this is the \
                 readback the unit exists to remove"
            );
            assert_close(&frame(&out), &cpu, key);
        }
    }

    /// An undersized layer is padded and an oversized one is cropped, both by
    /// reading outside the side's own dimensions as transparent.
    #[test]
    fn gpu_matches_the_cpu_reference_on_mismatched_sizes() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        for (fg_w, fg_h, what) in [
            (4, 4, "undersized"),
            (12, 12, "oversized"),
            (4, 12, "mixed"),
        ] {
            let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![
                Some(Arc::new(bg_fb(8, 8))),
                Some(Arc::new(fg_fb(fg_w, fg_h))),
            ];
            for key in COMPOSITING_KEYS {
                let cpu = frame(&run_cpu(key, &ctx, &inputs));
                let out = frame(&run_gpu(&gpu, key, &ctx, &inputs));
                assert_close(&out, &cpu, &format!("{key} with a {what} foreground"));
            }
        }
    }

    /// A side that is absent altogether — the CPU's 0x0 `empty_frame`, the
    /// GPU's zero-dimension stand-in. The present side is deliberately the
    /// wrong size, or the merge would short-circuit before compositing.
    #[test]
    fn gpu_matches_the_cpu_reference_when_one_side_is_absent() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let odd: Arc<dyn NodeData> = Arc::new(bg_fb(4, 4));
        for (inputs, what) in [
            (vec![Some(odd.clone()), None], "background only"),
            (vec![None, Some(odd.clone())], "foreground only"),
        ] {
            for key in COMPOSITING_KEYS {
                let cpu = frame(&run_cpu(key, &ctx, &inputs));
                let out = frame(&run_gpu(&gpu, key, &ctx, &inputs));
                assert_eq!((out.width, out.height), ctx.resolution, "{key}: {what}");
                assert_close(&out, &cpu, &format!("{key} with {what}"));
            }
        }
    }

    /// The stand-in bound for an absent side must never be sampled. It comes
    /// from the shared texture pool, which hands back textures without
    /// clearing them, so its texels are whatever the previous user left.
    /// Poisoning the 1x1 slot with opaque white must not move a single pixel:
    /// the absence lives in the uniform's zero dimensions, not in the texels.
    #[test]
    fn the_absent_side_stand_in_is_never_sampled() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));

        // Leave an opaque white 1x1 texture idle for `side()` to pick up.
        let key = gpu_util::tex_key_rw(1, 1);
        let poisoned = pool.lock().unwrap().acquire(key);
        ravel_gpu::upload_texture(
            &gpu,
            &poisoned.texture,
            key,
            bytemuck::cast_slice(&[1.0f32, 1.0, 1.0, 1.0]),
        );
        pool.lock().unwrap().release(poisoned);

        let node = merge_node("comp.merge.normal");
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompMergeGpuProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        let mut scope = Evaluator::new();

        // Undersized on purpose: a right-sized lone side would short-circuit
        // before anything is bound at all.
        let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![Some(Arc::new(bg_fb(4, 4))), None];
        let out = processor
            .process(&node, &ctx, &inputs, &ResolvedParams::default(), &mut scope)
            .expect("gpu merge");
        let expected = frame(&run_cpu("comp.merge.normal", &ctx, &inputs));
        assert_close(&frame(&out), &expected, "poisoned stand-in");
    }

    /// Adapting the second side can fail while the first has already taken a
    /// pooled texture. `PooledTexture` has no `Drop` that returns it, so a
    /// plain `?` there destroys one uploaded texture per failed evaluation —
    /// and this path repeats every frame for as long as the graph stays wired
    /// that way, so the pool would never reuse anything.
    #[test]
    fn a_failed_side_returns_the_other_sides_texture_to_the_pool() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let node = merge_node("comp.merge.normal");
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompMergeGpuProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &node);
        let mut scope = Evaluator::new();

        // A real background with a scalar probe as the foreground: `shell_blend`
        // passes a non-frame value through only when it is the lone side, so
        // this reaches `sides()` and fails on the second adaptation.
        let inputs: Vec<Option<Arc<dyn NodeData>>> =
            vec![Some(Arc::new(bg_fb(8, 8))), Some(Arc::new(Scalar(0.5)))];

        assert!(
            processor
                .process(&node, &ctx, &inputs, &ResolvedParams::default(), &mut scope)
                .is_err(),
            "a non-frame side alongside a frame must be an error"
        );
        let created = pool.lock().unwrap().total_created();
        for _ in 0..5 {
            assert!(
                processor
                    .process(&node, &ctx, &inputs, &ResolvedParams::default(), &mut scope)
                    .is_err()
            );
        }
        assert_eq!(
            pool.lock().unwrap().total_created(),
            created,
            "every failed merge must hand its uploaded texture back to the pool"
        );
    }

    #[test]
    fn both_sides_absent_is_a_transparent_canvas() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let out = run_gpu(&gpu, "comp.merge.normal", &ctx, &[None, None]);
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("nothing to composite yields a CPU transparent frame");
        assert_eq!((fb.width, fb.height), ctx.resolution);
        assert!(fb.data.iter().all(|v| *v == 0.0));
    }

    /// A lone right-sized side must return the very same `Arc`:
    /// `shape_layer_golden` pins pixels for a single-layer composition that
    /// only stay fixed while the merge passes its input straight through.
    #[test]
    fn a_lone_right_sized_side_passes_through() {
        let ctx = ctx(8, 8);
        let only: Arc<dyn NodeData> = Arc::new(bg_fb(8, 8));

        let cpu = run_cpu("comp.merge.normal", &ctx, &[Some(only.clone()), None]);
        assert!(
            Arc::ptr_eq(&cpu, &only),
            "the CPU reference must short-circuit"
        );

        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        for inputs in [
            vec![Some(only.clone()), None],
            vec![None, Some(only.clone())],
        ] {
            let out = run_gpu(&gpu, "comp.merge.normal", &ctx, &inputs);
            assert!(Arc::ptr_eq(&out, &only), "the GPU path must short-circuit");
        }
    }

    /// Non-frame values (scalar probes reaching a merge input) pass through
    /// untouched rather than failing an `ensure_gpu` downcast.
    #[test]
    fn a_lone_non_frame_value_passes_through() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let probe: Arc<dyn NodeData> = Arc::new(Scalar(0.5));
        let out = run_gpu(
            &gpu,
            "comp.merge.normal",
            &ctx,
            &[Some(probe.clone()), None],
        );
        assert!(Arc::ptr_eq(&out, &probe));
    }

    /// The whole point of the unit: a GPU-resident input is composited without
    /// a round trip through CPU memory.
    #[test]
    fn resident_inputs_stay_on_the_gpu() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let upload = |source: &FrameBuffer| -> Arc<dyn NodeData> {
            let key = gpu_util::tex_key_rw(source.width, source.height);
            let pooled = pool.lock().unwrap().acquire(key);
            ravel_gpu::upload_texture(
                &gpu,
                &pooled.texture,
                key,
                bytemuck::cast_slice(&source.data),
            );
            Arc::new(GpuFrameBuffer::new(
                gpu.clone(),
                &pool,
                pooled,
                source.width,
                source.height,
            ))
        };

        let cpu_inputs: Vec<Option<Arc<dyn NodeData>>> =
            vec![Some(Arc::new(bg_fb(8, 8))), Some(Arc::new(fg_fb(8, 8)))];
        let resident: Vec<Option<Arc<dyn NodeData>>> =
            vec![Some(upload(&bg_fb(8, 8))), Some(upload(&fg_fb(8, 8)))];

        let expected = frame(&run_cpu("comp.merge.overlay", &ctx, &cpu_inputs));
        let out = run_gpu(&gpu, "comp.merge.overlay", &ctx, &resident);
        assert!(
            out.downcast_ref::<GpuFrameBuffer>().is_some(),
            "the result must stay resident"
        );
        assert_close(&frame(&out), &expected, "resident inputs");
    }

    // =======================================================================
    // comp.merge.adjustment
    // =======================================================================

    const ADJUSTMENT_KEY: &str = "comp.merge.adjustment";

    /// An evaluator carrying a document whose single adjustment layer has the
    /// given effect strength (its opacity) and display interval.
    fn adjustment_scope(opacity: f32, start_frame: i64) -> Evaluator {
        let mut layer =
            Layer::new(LayerId::new(1), "Adjust", Graph::new()).with_time(start_frame, 0, 300);
        layer.adjustment = true;
        layer.opacity = AnimationChannel::constant(opacity);
        let comp = Composition::new(CompId::new(1), "Comp", (8, 8), FPS, 300).add_layer(layer);
        let mut scope = Evaluator::new();
        scope.set_document(Arc::new(Document::default().with_composition(comp)));
        scope
    }

    #[test]
    fn gpu_adjustment_matches_the_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let inputs: Vec<Option<Arc<dyn NodeData>>> =
            vec![Some(Arc::new(bg_fb(8, 8))), Some(Arc::new(fg_fb(8, 8)))];

        for strength in [0.25, 0.5, 0.75, 0.999] {
            let cpu = frame(&run_cpu_in(
                &mut adjustment_scope(strength, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ));
            let out = run_gpu_in(
                &gpu,
                &mut adjustment_scope(strength, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            );
            assert!(
                out.downcast_ref::<GpuFrameBuffer>().is_some(),
                "the mixed frame must stay resident at strength {strength}"
            );
            assert_close(&frame(&out), &cpu, &format!("strength {strength}"));
        }
    }

    /// An adjustment layer sits over a stack that need not match the canvas.
    #[test]
    fn gpu_adjustment_matches_the_cpu_reference_on_mismatched_sizes() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        for (fg_w, fg_h, what) in [(4, 4, "undersized"), (12, 12, "oversized")] {
            let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![
                Some(Arc::new(bg_fb(8, 8))),
                Some(Arc::new(fg_fb(fg_w, fg_h))),
            ];
            let cpu = frame(&run_cpu_in(
                &mut adjustment_scope(0.5, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ));
            let out = frame(&run_gpu_in(
                &gpu,
                &mut adjustment_scope(0.5, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ));
            assert_close(&out, &cpu, &format!("a {what} adjusted stack"));
        }
    }

    /// The bottom adjustment layer of a stack has nothing under it.
    #[test]
    fn gpu_adjustment_matches_the_cpu_reference_without_a_background() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![None, Some(Arc::new(fg_fb(8, 8)))];
        let cpu = frame(&run_cpu_in(
            &mut adjustment_scope(0.5, 0),
            ADJUSTMENT_KEY,
            &ctx,
            &inputs,
        ));
        let out = frame(&run_gpu_in(
            &gpu,
            &mut adjustment_scope(0.5, 0),
            ADJUSTMENT_KEY,
            &ctx,
            &inputs,
        ));
        assert_close(&out, &cpu, "no background");
    }

    /// Mixing in straight alpha would drag the transparent side's RGB — zero —
    /// into the result. Where the background is transparent and the foreground
    /// opaque white, a half-strength mix must yield white at half alpha, not
    /// mid grey.
    #[test]
    fn gpu_adjustment_mixes_in_premultiplied_alpha() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let clear: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(8, 8));
        let white: Arc<dyn NodeData> = Arc::new(translucent_fb(8, 8, [1.0; 4], 4));
        let out = frame(&run_gpu_in(
            &gpu,
            &mut adjustment_scope(0.5, 0),
            ADJUSTMENT_KEY,
            &ctx,
            &[Some(clear), Some(white.clone())],
        ));

        let source = white
            .downcast_ref::<FrameBuffer>()
            .expect("the fixture is a CPU frame");
        for px in 0..64usize {
            let base = px * 4;
            for ch in 0..3 {
                assert!(
                    (out.data[base + ch] - source.data[base + ch]).abs() <= TOLERANCE,
                    "channel {ch} of pixel {px} was darkened: {} vs {}",
                    out.data[base + ch],
                    source.data[base + ch]
                );
            }
            assert!((out.data[base + 3] - 0.5).abs() <= TOLERANCE, "half alpha");
        }
    }

    /// Outside the layer's display interval the adjustment is bypassed
    /// entirely and the background passes through (REQ-LAYER-010).
    #[test]
    fn adjustment_bypasses_outside_the_display_interval() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let background: Arc<dyn NodeData> = Arc::new(bg_fb(8, 8));
        let inputs = vec![Some(background.clone()), Some(Arc::new(fg_fb(8, 8)) as _)];

        // The layer starts at frame 100; the context is at frame 0.
        for out in [
            run_cpu_in(
                &mut adjustment_scope(0.5, 100),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ),
            run_gpu_in(
                &gpu,
                &mut adjustment_scope(0.5, 100),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ),
        ] {
            assert!(Arc::ptr_eq(&out, &background), "both paths must bypass");
        }
    }

    #[test]
    fn adjustment_bypasses_at_zero_strength() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let background: Arc<dyn NodeData> = Arc::new(bg_fb(8, 8));
        let inputs = vec![Some(background.clone()), Some(Arc::new(fg_fb(8, 8)) as _)];

        for out in [
            run_cpu_in(&mut adjustment_scope(0.0, 0), ADJUSTMENT_KEY, &ctx, &inputs),
            run_gpu_in(
                &gpu,
                &mut adjustment_scope(0.0, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ),
        ] {
            assert!(Arc::ptr_eq(&out, &background), "both paths must bypass");
        }
    }

    /// Full strength on a right-sized adjusted stack is the stack itself.
    #[test]
    fn adjustment_at_full_strength_passes_the_foreground() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let ctx = ctx(8, 8);
        let foreground: Arc<dyn NodeData> = Arc::new(fg_fb(8, 8));
        let inputs = vec![Some(Arc::new(bg_fb(8, 8)) as _), Some(foreground.clone())];

        for out in [
            run_cpu_in(&mut adjustment_scope(1.0, 0), ADJUSTMENT_KEY, &ctx, &inputs),
            run_gpu_in(
                &gpu,
                &mut adjustment_scope(1.0, 0),
                ADJUSTMENT_KEY,
                &ctx,
                &inputs,
            ),
        ] {
            assert!(
                Arc::ptr_eq(&out, &foreground),
                "both paths must short-circuit"
            );
        }
    }

    #[test]
    fn normal_over_matches_porter_duff() {
        // Opaque red under half-transparent green.
        let out = composite(
            MergeMode::Normal,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 0.5],
        );
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn add_blend_sums_where_opaque() {
        let out = composite(MergeMode::Add, [0.25, 0.0, 0.0, 1.0], [0.5, 0.0, 0.0, 1.0]);
        assert!((out[0] - 0.75).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn multiply_blend_darkens() {
        let out = composite(
            MergeMode::Multiply,
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((out[0] - 0.25).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn screen_blend_brightens() {
        let out = composite(
            MergeMode::Screen,
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((out[0] - 0.75).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn overlay_splits_on_backdrop_midpoint() {
        let dark = composite(
            MergeMode::Overlay,
            [0.25, 0.25, 0.25, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((dark[0] - 0.25).abs() < 1e-6, "{dark:?}");
        let bright = composite(
            MergeMode::Overlay,
            [0.75, 0.75, 0.75, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((bright[0] - 0.75).abs() < 1e-6, "{bright:?}");
    }

    #[test]
    fn transparent_foreground_keeps_background() {
        let out = composite(MergeMode::Normal, [0.2, 0.4, 0.6, 0.8], [0.0; 4]);
        assert!((out[0] - 0.2).abs() < 1e-6 && (out[3] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn blend_modes_reduce_to_foreground_over_transparent_backdrop() {
        for mode in [
            MergeMode::Add,
            MergeMode::Multiply,
            MergeMode::Screen,
            MergeMode::Overlay,
        ] {
            let out = composite(mode, [0.0; 4], [0.3, 0.6, 0.9, 0.5]);
            assert!(
                (out[0] - 0.3).abs() < 1e-6 && (out[3] - 0.5).abs() < 1e-6,
                "{out:?}"
            );
        }
    }
}

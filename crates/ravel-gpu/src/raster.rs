// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-pass pipeline helpers for instanced rasterization.
//!
//! [`RasterPipeline`] is the graphics counterpart of [`crate::ComputePipeline`]:
//! it owns a render pipeline and its bind-group layout. Callers never touch it
//! directly beyond construction — a frame of drawing is described as a
//! [`QuadDraw`](crate::QuadDraw) and handed to
//! [`GpuContext::draw_quads`](crate::GpuContext::draw_quads), which builds the
//! bind group, records the pass into the frame's shared encoder, and keeps the
//! attachment out of pool circulation until the batch is submitted.
//!
//! The colour attachment is described in this crate's own vocabulary
//! ([`ColorTarget`] / [`BlendMode`]), like every other description in
//! [`binding`](crate::binding) and [`texture_desc`](crate::texture_desc): the
//! set is deliberately closed over what Ravel actually draws today, and each
//! type converts to wgpu in exactly one place.

use crate::binding::BindingDesc;
use crate::device::GpuContext;
use crate::shader::CompiledShader;
use crate::texture_desc::TextureFormat;

/// How a fragment's colour combines with what the attachment already holds.
///
/// One variant, because Ravel draws exactly one way today. A second blend is
/// added when a second draw path needs it, the same rule
/// [`TextureFormat`] and
/// [`BindingKind`](crate::BindingKind) follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Source-over on premultiplied colour: `src + dst * (1 - src.a)` for both
    /// colour and alpha. The attachment must therefore hold premultiplied
    /// values; the rasterizer converts back to straight alpha in a following
    /// compute pass.
    PremultipliedOver,
}

impl BlendMode {
    /// Convert to the wgpu blend state.
    ///
    /// The single conversion site between this description and wgpu's
    /// vocabulary.
    fn to_wgpu(self) -> wgpu::BlendState {
        match self {
            Self::PremultipliedOver => wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        }
    }
}

/// The colour attachment a render pipeline writes: its pixel format and how
/// fragments blend into it.
///
/// All channels are always written — Ravel has no partial-write draw — so the
/// write mask is not part of the description.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorTarget {
    /// Pixel format of the attachment. Must match the format the attachment
    /// texture was allocated with.
    pub format: TextureFormat,
    /// How a fragment combines with the attachment's current contents.
    pub blend: BlendMode,
}

impl ColorTarget {
    pub const fn new(format: TextureFormat, blend: BlendMode) -> Self {
        Self { format, blend }
    }

    /// Convert to the wgpu colour target state.
    ///
    /// The single conversion site between this description and wgpu's
    /// vocabulary.
    fn to_wgpu(self) -> wgpu::ColorTargetState {
        wgpu::ColorTargetState {
            format: self.format.to_wgpu(),
            blend: Some(self.blend.to_wgpu()),
            write_mask: wgpu::ColorWrites::ALL,
        }
    }
}

/// A render pipeline for drawing procedurally generated vertices.
pub struct RasterPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    label: String,
}

impl RasterPipeline {
    /// Build a render pipeline from `shader`.
    ///
    /// The pipeline uses no vertex buffers; callers supply geometry through
    /// bind groups and the shader expands it from vertex/instance indices.
    pub fn new(
        ctx: &GpuContext,
        shader: &CompiledShader,
        vertex_entry: &str,
        fragment_entry: &str,
        bind_group_layout: &[BindingDesc],
        target: ColorTarget,
    ) -> Self {
        let device = ctx.device();
        let label = shader.name.clone();
        let entries: Vec<wgpu::BindGroupLayoutEntry> = bind_group_layout
            .iter()
            .map(|desc| desc.to_wgpu())
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let targets = [Some(target.to_wgpu())];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader.module,
                entry_point: Some(vertex_entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader.module,
                entry_point: Some(fragment_entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            label,
        }
    }

    /// The bind-group layout expected by this pipeline.
    ///
    /// Crate-internal: the only code that builds a bind group for a raster
    /// pipeline is [`dispatch`](crate::dispatch), which does it from a
    /// [`QuadDraw`](crate::QuadDraw).
    #[inline]
    pub(crate) fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Record a clear followed by one six-vertex quad per instance.
    ///
    /// Crate-internal for the same reason as
    /// [`Self::bind_group_layout`]: callers describe the draw and the dispatch
    /// layer owns the encoder.
    pub(crate) fn draw_quads(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        target: &wgpu::TextureView,
        instance_count: u32,
    ) {
        let color_attachments = [Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&self.label),
            color_attachments: &color_attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..instance_count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_premultiplied_blend_maps_to_the_backend_state() {
        // The rasterizer's attachment holds premultiplied colour; a blend that
        // silently changed would change every drawn edge.
        assert_eq!(
            BlendMode::PremultipliedOver.to_wgpu(),
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
        );
    }

    #[test]
    fn a_color_target_maps_format_blend_and_a_full_write_mask() {
        let target = ColorTarget::new(TextureFormat::Rgba16Float, BlendMode::PremultipliedOver);
        assert_eq!(
            target.to_wgpu(),
            wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            }
        );
    }
}

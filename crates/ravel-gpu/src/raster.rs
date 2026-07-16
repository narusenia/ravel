// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-pass pipeline helpers for instanced rasterization.
//!
//! [`RasterPipeline`] is the graphics counterpart of [`crate::ComputePipeline`]:
//! it owns a render pipeline and its bind-group layout, and records an
//! instanced draw into a caller-provided color attachment.

use crate::device::GpuContext;
use crate::shader::CompiledShader;

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
        bind_group_layout: &[wgpu::BindGroupLayoutEntry],
        target: wgpu::ColorTargetState,
    ) -> Self {
        let device = ctx.device();
        let label = shader.name.clone();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label),
            entries: bind_group_layout,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let targets = [Some(target)];
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
    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Record a clear followed by one six-vertex quad per instance.
    pub fn draw_quads(
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

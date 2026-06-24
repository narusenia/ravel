// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compute pipeline creation and dispatch helpers.
//!
//! [`ComputePipeline`] is a thin wrapper around `wgpu::ComputePipeline` that
//! remembers its bind-group layout and per-axis workgroup size, so dispatching
//! over a texture of a given size only requires the target dimensions.

use crate::device::GpuContext;
use crate::shader::CompiledShader;

/// Compute the number of workgroups needed to cover `extent` elements when
/// each workgroup processes `local_size` elements along that axis.
///
/// This is a ceiling division; a `local_size` of zero is treated as one to
/// avoid division by zero.
#[inline]
pub const fn workgroup_count(extent: u32, local_size: u32) -> u32 {
    if local_size == 0 {
        extent
    } else {
        extent.div_ceil(local_size)
    }
}

/// 3D workgroup count for a 2D image dispatch (depth fixed to 1).
#[inline]
pub const fn workgroup_count_2d(width: u32, height: u32, local: [u32; 2]) -> [u32; 3] {
    [
        workgroup_count(width, local[0]),
        workgroup_count(height, local[1]),
        1,
    ]
}

/// A compute pipeline plus the metadata needed to dispatch it.
pub struct ComputePipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    workgroup_size: [u32; 2],
    label: String,
}

impl ComputePipeline {
    /// Build a compute pipeline from a compiled shader.
    ///
    /// * `entry_point` — the `@compute` function name in the WGSL.
    /// * `bind_group_layout` — the layout entries the shader expects.
    /// * `workgroup_size` — the shader's `@workgroup_size` along x/y, used to
    ///   compute dispatch counts in [`ComputePipeline::dispatch`].
    pub fn new(
        ctx: &GpuContext,
        shader: &CompiledShader,
        entry_point: &str,
        bind_group_layout: &[wgpu::BindGroupLayoutEntry],
        workgroup_size: [u32; 2],
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

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&label),
            layout: Some(&pipeline_layout),
            module: &shader.module,
            entry_point: Some(entry_point),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Self {
            pipeline,
            layout,
            workgroup_size,
            label,
        }
    }

    /// The pipeline's bind group layout (for building bind groups).
    #[inline]
    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The underlying wgpu pipeline.
    #[inline]
    pub fn raw(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }

    /// Record a dispatch covering a `width` x `height` grid into `encoder`.
    pub fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let [gx, gy, gz] = workgroup_count_2d(width, height, self.workgroup_size);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(&self.label),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.dispatch_workgroups(gx, gy, gz);
    }
}

/// A unit of GPU work that records itself into a command encoder.
///
/// Node processors that run on the GPU implement this trait; the eval engine
/// batches their dispatches into a single command buffer per frame.
pub trait GpuTask {
    /// Record this task's commands into `encoder`.
    fn dispatch(&self, encoder: &mut wgpu::CommandEncoder, ctx: &GpuContext);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workgroup_count_rounds_up() {
        assert_eq!(workgroup_count(0, 8), 0);
        assert_eq!(workgroup_count(1, 8), 1);
        assert_eq!(workgroup_count(8, 8), 1);
        assert_eq!(workgroup_count(9, 8), 2);
        assert_eq!(workgroup_count(1920, 8), 240);
        assert_eq!(workgroup_count(1080, 8), 135);
    }

    #[test]
    fn workgroup_count_handles_zero_local_size() {
        assert_eq!(workgroup_count(10, 0), 10);
    }

    #[test]
    fn workgroup_count_2d_fixes_depth_to_one() {
        assert_eq!(workgroup_count_2d(1920, 1080, [8, 8]), [240, 135, 1]);
        assert_eq!(workgroup_count_2d(1, 1, [16, 16]), [1, 1, 1]);
    }
}

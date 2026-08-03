// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compute pipeline creation and dispatch helpers.
//!
//! [`ComputePipeline`] is a thin wrapper around `wgpu::ComputePipeline` that
//! remembers its bind-group layout and per-axis workgroup size, so dispatching
//! over a texture of a given size only requires the target dimensions.

use std::sync::Arc;

use crate::binding::BindingDesc;
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
    /// * `bind_group_layout` — the bindings the shader expects, in
    ///   backend-agnostic terms ([`BindingDesc`]).
    /// * `workgroup_size` — the shader's `@workgroup_size` along x/y, used to
    ///   compute dispatch counts in [`ComputePipeline::dispatch`].
    pub fn new(
        ctx: &GpuContext,
        shader: &CompiledShader,
        entry_point: &str,
        bind_group_layout: &[BindingDesc],
        workgroup_size: [u32; 2],
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

/// Identity of a compute pipeline: everything [`ComputePipeline::new`] builds
/// from.
///
/// Two nodes of the same type ask for the same shader, entry point, bind group
/// layout, and workgroup size, so they can share one pipeline. The layout is
/// keyed by its debug form rather than by `Hash`: [`BindingDesc`] is a plain
/// description with no interior mutability, so its rendering is a faithful
/// identity.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    shader_hash: String,
    entry_point: String,
    layout: String,
    workgroup_size: [u32; 2],
}

/// Compute pipelines shared by (shader, entry point, layout, workgroup size).
///
/// Creating one builds a `BindGroupLayout`, a `PipelineLayout`, and a
/// `ComputePipeline`, the last of which compiles in the driver. Node processors
/// are constructed per node and rebuilt on structural edits, so without this
/// the cost scaled with the number of GPU nodes in the document instead of the
/// number of distinct pipelines they need.
#[derive(Default)]
pub struct PipelineCache {
    entries: std::collections::HashMap<PipelineKey, Arc<ComputePipeline>>,
    created: usize,
}

impl PipelineCache {
    /// The pipeline for this combination, building it on first request.
    pub fn get_or_create(
        &mut self,
        ctx: &GpuContext,
        shader: &CompiledShader,
        entry_point: &str,
        bind_group_layout: &[BindingDesc],
        workgroup_size: [u32; 2],
    ) -> Arc<ComputePipeline> {
        let key = PipelineKey {
            shader_hash: shader.hash.clone(),
            entry_point: entry_point.to_string(),
            layout: format!("{bind_group_layout:?}"),
            workgroup_size,
        };
        if let Some(pipeline) = self.entries.get(&key) {
            return pipeline.clone();
        }
        let pipeline = Arc::new(ComputePipeline::new(
            ctx,
            shader,
            entry_point,
            bind_group_layout,
            workgroup_size,
        ));
        self.created += 1;
        self.entries.insert(key, pipeline.clone());
        pipeline
    }

    /// How many pipelines this cache has actually created. Lets a test assert
    /// that repeated requests are served from the cache.
    pub fn created_count(&self) -> usize {
        self.created
    }
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

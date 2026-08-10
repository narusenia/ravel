// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-agnostic bind-group layout descriptions.
//!
//! Node processors declare their bindings with [`BindingDesc`] — a binding
//! number, a [`BindingKind`], and a [`ShaderVisibility`] — and never name a
//! wgpu type. The crate converts the description to the backend's layout
//! entry in exactly one place (`BindingDesc::to_wgpu`, crate-private), so a second backend
//! (Metal / D3D12 / Vulkan) translates the same declarations instead of every
//! processor being rewritten.

/// The kind of resource a binding slot expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// A read-only sampled 2D float texture (non-filterable).
    InputTexture,
    /// A write-only 2D storage texture of the given format.
    ///
    /// The format is part of the slot, not a crate-wide constant: a storage
    /// binding's layout entry must name the same format the WGSL declares, and
    /// the display transform (`CM-7`) writes `rgba8unorm` while every filter
    /// writes `rgba32float`.
    OutputStorageTexture(crate::texture_desc::TextureFormat),
    /// A uniform buffer.
    UniformBuffer,
    /// A read-only storage buffer.
    ReadOnlyStorageBuffer,
}

/// The pipeline stages a binding is visible from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShaderVisibility(u8);

impl ShaderVisibility {
    /// Visible from the compute stage.
    pub const COMPUTE: Self = Self(0b001);
    /// Visible from the vertex stage.
    pub const VERTEX: Self = Self(0b010);
    /// Visible from the fragment stage.
    pub const FRAGMENT: Self = Self(0b100);
    /// Visible from both raster stages.
    pub const VERTEX_FRAGMENT: Self = Self(0b110);

    fn to_wgpu(self) -> wgpu::ShaderStages {
        // Map flag by flag rather than casting: the bit values are this
        // crate's own and must not depend on wgpu's representation.
        let mut stages = wgpu::ShaderStages::empty();
        if self.0 & Self::COMPUTE.0 != 0 {
            stages |= wgpu::ShaderStages::COMPUTE;
        }
        if self.0 & Self::VERTEX.0 != 0 {
            stages |= wgpu::ShaderStages::VERTEX;
        }
        if self.0 & Self::FRAGMENT.0 != 0 {
            stages |= wgpu::ShaderStages::FRAGMENT;
        }
        stages
    }
}

/// One slot of a bind-group layout, in backend-agnostic terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingDesc {
    /// The `@binding` index the shader declares.
    pub binding: u32,
    /// What kind of resource the slot expects.
    pub kind: BindingKind,
    /// Which stages may see the slot.
    pub visibility: ShaderVisibility,
}

impl BindingDesc {
    pub const fn new(binding: u32, kind: BindingKind, visibility: ShaderVisibility) -> Self {
        Self {
            binding,
            kind,
            visibility,
        }
    }

    /// Convert to the wgpu layout entry.
    ///
    /// This is the single conversion site between the backend-agnostic
    /// description and wgpu's vocabulary; every pipeline builder routes
    /// through it.
    pub(crate) fn to_wgpu(self) -> wgpu::BindGroupLayoutEntry {
        let ty = match self.kind {
            BindingKind::InputTexture => wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            BindingKind::OutputStorageTexture(format) => wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: format.to_wgpu(),
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            BindingKind::UniformBuffer => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            BindingKind::ReadOnlyStorageBuffer => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
        };
        wgpu::BindGroupLayoutEntry {
            binding: self.binding,
            visibility: self.visibility.to_wgpu(),
            ty,
            count: None,
        }
    }
}

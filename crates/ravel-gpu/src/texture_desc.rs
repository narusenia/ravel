// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-agnostic texture format and usage descriptions.
//!
//! Callers describe the textures they want with [`TextureFormat`] and
//! [`TextureUsage`] — never with a wgpu type — so the pool's identity
//! judgement ([`TextureKey`](crate::TextureKey)) is stated in this crate's own
//! vocabulary. Each type converts to wgpu in exactly one place
//! (`TextureFormat::to_wgpu`, `TextureUsage::to_wgpu` — both crate-private), which is what a
//! second backend (Metal / D3D12 / Vulkan) replaces instead of every caller
//! being rewritten.
//!
//! Both sets are deliberately closed over what Ravel actually allocates today.
//! A backend that needs more formats or usages adds them when it needs them.

/// Pixel format of a texture, in backend-agnostic terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// Four 32-bit floats per pixel. The default for intermediate results.
    Rgba32Float,
    /// Four 16-bit floats per pixel. The rasterizer's premultiplied render
    /// target.
    Rgba16Float,
    /// Four 8-bit unsigned normalized channels per pixel.
    Rgba8Unorm,
}

impl TextureFormat {
    /// Bytes one pixel of this format occupies.
    ///
    /// This crate's own answer, not the backend's: the pool's byte accounting
    /// ([`TextureKey::byte_size`](crate::TextureKey::byte_size)) and the
    /// row-stride math in [`transfer`](crate::transfer) read it instead of
    /// asking wgpu, so the VRAM ledger cannot change under a backend swap. A
    /// test pins every variant against wgpu's `block_copy_size`.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba32Float => 16,
            Self::Rgba16Float => 8,
            Self::Rgba8Unorm => 4,
        }
    }

    /// Convert to the wgpu format.
    ///
    /// The single conversion site between this description and wgpu's
    /// vocabulary.
    pub(crate) fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        }
    }
}

/// The operations a texture must support, as a set of flags.
///
/// Combine with `|`; test with [`TextureUsage::contains`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureUsage(u8);

impl TextureUsage {
    /// Bindable as a sampled texture in a shader.
    pub const TEXTURE_BINDING: Self = Self(0b0_0001);
    /// Bindable as a storage texture a shader writes to.
    pub const STORAGE_BINDING: Self = Self(0b0_0010);
    /// Usable as the source of a copy.
    pub const COPY_SRC: Self = Self(0b0_0100);
    /// Usable as the destination of a copy.
    pub const COPY_DST: Self = Self(0b0_1000);
    /// Usable as a render pass colour attachment.
    pub const RENDER_ATTACHMENT: Self = Self(0b1_0000);

    /// Whether every flag of `other` is set in `self`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Convert to the wgpu usage set.
    ///
    /// The single conversion site between this description and wgpu's
    /// vocabulary.
    pub(crate) fn to_wgpu(self) -> wgpu::TextureUsages {
        // Map flag by flag rather than casting: the bit values above are this
        // crate's own and must not depend on wgpu's representation.
        let mut usages = wgpu::TextureUsages::empty();
        if self.contains(Self::TEXTURE_BINDING) {
            usages |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        if self.contains(Self::STORAGE_BINDING) {
            usages |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        if self.contains(Self::COPY_SRC) {
            usages |= wgpu::TextureUsages::COPY_SRC;
        }
        if self.contains(Self::COPY_DST) {
            usages |= wgpu::TextureUsages::COPY_DST;
        }
        if self.contains(Self::RENDER_ATTACHMENT) {
            usages |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        usages
    }
}

impl std::ops::BitOr for TextureUsage {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for TextureUsage {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every format this crate describes. Extend when a variant is added — the
    /// tests below iterate it.
    const ALL_FORMATS: [TextureFormat; 3] = [
        TextureFormat::Rgba32Float,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba8Unorm,
    ];

    /// Every usage flag this crate describes, with its wgpu counterpart.
    const ALL_USAGES: [(TextureUsage, wgpu::TextureUsages); 5] = [
        (
            TextureUsage::TEXTURE_BINDING,
            wgpu::TextureUsages::TEXTURE_BINDING,
        ),
        (
            TextureUsage::STORAGE_BINDING,
            wgpu::TextureUsages::STORAGE_BINDING,
        ),
        (TextureUsage::COPY_SRC, wgpu::TextureUsages::COPY_SRC),
        (TextureUsage::COPY_DST, wgpu::TextureUsages::COPY_DST),
        (
            TextureUsage::RENDER_ATTACHMENT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        ),
    ];

    #[test]
    fn bytes_per_pixel_has_the_expected_values() {
        assert_eq!(TextureFormat::Rgba32Float.bytes_per_pixel(), 16);
        assert_eq!(TextureFormat::Rgba16Float.bytes_per_pixel(), 8);
        assert_eq!(TextureFormat::Rgba8Unorm.bytes_per_pixel(), 4);
    }

    #[test]
    fn bytes_per_pixel_agrees_with_the_backend() {
        // The pool's VRAM ledger and the transfer row strides now read
        // `bytes_per_pixel` instead of wgpu. If the two ever disagreed the
        // accounting would go quietly wrong, so pin them together.
        for format in ALL_FORMATS {
            assert_eq!(
                format.to_wgpu().block_copy_size(None),
                Some(format.bytes_per_pixel()),
                "{format:?} disagrees with wgpu",
            );
        }
    }

    #[test]
    fn each_usage_flag_maps_to_its_backend_flag() {
        for (usage, expected) in ALL_USAGES {
            assert_eq!(usage.to_wgpu(), expected, "{usage:?} maps wrong");
        }
    }

    #[test]
    fn usage_flags_are_distinct_and_combine() {
        let combined = TextureUsage::TEXTURE_BINDING
            | TextureUsage::STORAGE_BINDING
            | TextureUsage::COPY_SRC
            | TextureUsage::COPY_DST;
        assert!(combined.contains(TextureUsage::TEXTURE_BINDING));
        assert!(combined.contains(TextureUsage::COPY_DST));
        assert!(combined.contains(TextureUsage::COPY_SRC | TextureUsage::COPY_DST));
        assert!(!combined.contains(TextureUsage::RENDER_ATTACHMENT));

        assert_eq!(
            combined.to_wgpu(),
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
        );
    }

    #[test]
    fn bitor_assign_adds_a_flag() {
        let mut usage = TextureUsage::TEXTURE_BINDING;
        usage |= TextureUsage::RENDER_ATTACHMENT;
        assert!(usage.contains(TextureUsage::RENDER_ATTACHMENT));
        assert!(usage.contains(TextureUsage::TEXTURE_BINDING));
    }

    #[test]
    fn distinct_usage_sets_are_not_equal() {
        assert_ne!(TextureUsage::COPY_SRC, TextureUsage::COPY_DST);
        assert_ne!(
            TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
            TextureUsage::COPY_SRC
        );
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared GPU helpers for built-in node processors.
//!
//! GPU processors keep their intermediate results resident in VRAM
//! ([`GpuFrameBuffer`]) and only touch CPU memory at true boundaries.
//! [`ensure_gpu`] adapts either frame representation into a texture (an
//! upload happens only for CPU inputs); [`ensure_cpu`] is the inverse for
//! CPU-only processors (a readback happens only for GPU inputs).

use anyhow::Context as _;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{
    BindingDesc, BindingKind, GpuContext, GpuFrameBuffer, PooledTexture, ShaderVisibility,
    TextureFormat, TextureKey, TexturePool, TextureUsage,
};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

pub const WORKGROUP_SIZE: [u32; 2] = [8, 8];

/// Alpha-convention helpers shared by the filtering shaders.
const PREMULTIPLIED_HELPERS: &str = include_str!("shaders/premultiplied.wgsl");

/// Prefix a shader with [`PREMULTIPLIED_HELPERS`].
///
/// WGSL has no include directive and requires a function to be declared before
/// it is called, so the snippet is concatenated ahead of the body. Doing it
/// here rather than copying the helpers into each shader is what keeps the
/// premultiplied filtering identical across `blur`, `transform`, and
/// `comp.transform` — the divergence issue MED-GPU-02 is about.
///
/// The shader cache is keyed by source hash ([`ravel_gpu::source_hash`]), so a
/// composed source caches exactly like a file-backed one.
pub fn with_premultiplied_helpers(body: &str) -> String {
    format!("{PREMULTIPLIED_HELPERS}\n{body}")
}

/// A frame adapted to GPU representation for one dispatch.
pub enum GpuImage<'a> {
    /// Input was already GPU-resident; borrow its texture.
    Resident(&'a GpuFrameBuffer),
    /// Input was a CPU frame uploaded into a pool texture for this call.
    Uploaded {
        texture: PooledTexture,
        width: u32,
        height: u32,
    },
}

impl GpuImage<'_> {
    /// A bindable view of the image's texture for
    /// [`GpuContext::dispatch_compute`](ravel_gpu::GpuContext::dispatch_compute).
    pub fn binding(&self) -> ravel_gpu::TextureBinding {
        match self {
            GpuImage::Resident(frame) => frame.binding(),
            GpuImage::Uploaded { texture, .. } => texture.binding(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            GpuImage::Resident(frame) => (frame.width(), frame.height()),
            GpuImage::Uploaded { width, height, .. } => (*width, *height),
        }
    }

    /// Return an uploaded temporary to the pool (no-op for resident inputs,
    /// whose textures are owned by their `GpuFrameBuffer`). Safe to call
    /// right after recording the dispatch: the pool refuses to hand the
    /// texture to a new owner until the batched commands that read it are
    /// flushed, so the queued reads stay valid.
    pub fn release(self, pool: &Arc<Mutex<TexturePool>>) {
        if let GpuImage::Uploaded { texture, .. } = self {
            pool.lock().unwrap().release(texture);
        }
    }
}

/// Adapt a frame input (CPU or GPU representation) into a bindable texture.
pub fn ensure_gpu<'a>(
    ctx: &GpuContext,
    pool: &Arc<Mutex<TexturePool>>,
    input: &'a dyn NodeData,
) -> anyhow::Result<GpuImage<'a>> {
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Ok(GpuImage::Resident(frame));
    }
    let fb = input
        .downcast_ref::<FrameBuffer>()
        .context("expected FrameBuffer input")?;
    // The pool texture is `Rgba32Float`, so the upload needs four f32
    // channels per pixel whatever the buffer stores. `as_rgba_f32` borrows an
    // `RgbaF32` buffer (the common case, so this stays a zero-copy upload),
    // widens a reduced one, and refuses a shape that would upload garbage.
    let samples = fb.as_rgba_f32()?;
    let key = tex_key_rw(fb.width, fb.height);
    let pooled = pool.lock().unwrap().acquire(key);
    ravel_gpu::upload_texture(ctx, &pooled, bytemuck::cast_slice(samples.as_ref()));
    Ok(GpuImage::Uploaded {
        texture: pooled,
        width: fb.width,
        height: fb.height,
    })
}

/// Adapt a frame input into CPU memory. Reads back (blocking) only when the
/// input is GPU-resident.
pub fn ensure_cpu(input: &dyn NodeData) -> anyhow::Result<Cow<'_, FrameBuffer>> {
    if let Some(fb) = input.downcast_ref::<FrameBuffer>() {
        return Ok(Cow::Borrowed(fb));
    }
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Ok(Cow::Owned(frame.to_frame_buffer()?));
    }
    anyhow::bail!("expected FrameBuffer input")
}

/// Dimensions of a frame value in either representation, without any
/// transfer. Lets processors validate inputs before uploading anything.
pub fn frame_size(input: &dyn NodeData) -> Option<(u32, u32)> {
    if let Some(fb) = input.downcast_ref::<FrameBuffer>() {
        return Some((fb.width, fb.height));
    }
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Some((frame.width(), frame.height()));
    }
    None
}

/// Clone a frame value in either representation (for pass-through
/// processors). Cloning a `GpuFrameBuffer` shares the texture handle.
pub fn clone_frame_value(input: &dyn NodeData) -> Option<Box<dyn NodeData>> {
    if let Some(fb) = input.downcast_ref::<FrameBuffer>() {
        return Some(Box::new(fb.clone()));
    }
    if let Some(frame) = input.downcast_ref::<GpuFrameBuffer>() {
        return Some(Box::new(frame.clone()));
    }
    None
}

/// The key every compute node's intermediate texture uses: `Rgba32Float`,
/// readable and writable by shaders and by copies in both directions.
pub fn tex_key_rw(width: u32, height: u32) -> TextureKey {
    TextureKey::new(
        width,
        height,
        TextureFormat::Rgba32Float,
        TextureUsage::TEXTURE_BINDING
            | TextureUsage::STORAGE_BINDING
            | TextureUsage::COPY_SRC
            | TextureUsage::COPY_DST,
    )
}

/// Layout slot for a sampled input texture. The description is
/// backend-agnostic ([`BindingDesc`]); `ravel-gpu` converts it to the
/// backend's layout entry at pipeline creation.
pub fn input_texture_layout_entry(binding: u32) -> BindingDesc {
    BindingDesc::new(
        binding,
        BindingKind::InputTexture,
        ShaderVisibility::COMPUTE,
    )
}

/// Layout slot for the write-only storage texture a compute pass renders into.
///
/// `Rgba32Float`, the format every filtering pass writes. The one pass that
/// writes something else is the display transform, which states its own format
/// ([`output_storage_layout_entry_of`]).
pub fn output_storage_layout_entry(binding: u32) -> BindingDesc {
    output_storage_layout_entry_of(binding, TextureFormat::Rgba32Float)
}

/// Layout slot for a write-only storage texture of a stated format.
pub fn output_storage_layout_entry_of(binding: u32, format: TextureFormat) -> BindingDesc {
    BindingDesc::new(
        binding,
        BindingKind::OutputStorageTexture(format),
        ShaderVisibility::COMPUTE,
    )
}

/// Layout slot for a uniform parameter buffer.
pub fn uniform_layout_entry(binding: u32) -> BindingDesc {
    BindingDesc::new(
        binding,
        BindingKind::UniformBuffer,
        ShaderVisibility::COMPUTE,
    )
}

/// Coverage for the shader translation path (`ravel_gpu::translate`) over the
/// real built-in WGSL, rather than over samples written for the test.
///
/// This lives here because composition lives here: four of the shaders are only
/// complete once [`with_premultiplied_helpers`] has prefixed them, and a test
/// that translated the raw files would be testing sources no pipeline ever
/// compiles.
#[cfg(test)]
mod shader_translation {
    use super::with_premultiplied_helpers;
    use ravel_gpu::{ShaderTarget, translate_wgsl};
    use std::path::{Path, PathBuf};

    /// Every directory of built-in WGSL in the workspace. Two crates own
    /// shaders; the files inside are discovered, not listed.
    const SHADER_DIRS: [&str; 2] = [
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/shaders"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/../ravel-gpu/src/shaders"),
    ];

    /// How a shader file declares that it is a fragment needing the alpha
    /// helpers prefixed. The four filtering shaders already carry this line for
    /// the reader's sake, so nothing new has to be remembered when one is added.
    const NEEDS_HELPERS: &str = "Prepend `premultiplied.wgsl`";

    /// How many built-in WGSL files exist. Pinned so that adding one is a
    /// visible change here rather than a silent gap: the walk above picks a new
    /// file up automatically, and this line makes the author confirm it.
    const SHADER_COUNT: usize = 12;

    /// Every `.wgsl` under [`SHADER_DIRS`], sorted so failures name a stable
    /// order.
    fn shader_files() -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = SHADER_DIRS
            .iter()
            .flat_map(|dir| {
                std::fs::read_dir(Path::new(dir))
                    .unwrap_or_else(|e| panic!("cannot read shader dir {dir}: {e}"))
                    .map(|entry| entry.expect("dir entry").path())
            })
            .filter(|path| path.extension().is_some_and(|ext| ext == "wgsl"))
            .collect();
        files.sort();
        files
    }

    /// The source a pipeline would actually compile for `path`.
    ///
    /// Read from disk at test time on purpose: it is the same text the
    /// `include_str!` constants hold, so a pass here covers the build-time
    /// embedded form (REQ-GPU-002) and the runtime-loaded form (REQ-GPU-003,
    /// REQ-PLUGIN-002) with one call.
    fn compilable_source(path: &Path) -> String {
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        if raw.contains(NEEDS_HELPERS) {
            with_premultiplied_helpers(&raw)
        } else {
            raw
        }
    }

    #[test]
    fn the_shader_set_is_discovered_not_listed() {
        let files = shader_files();
        assert_eq!(
            files.len(),
            SHADER_COUNT,
            "built-in WGSL count changed; confirm the new shader translates and update \
             SHADER_COUNT: {files:#?}",
        );
    }

    /// Every built-in WGSL translates to every backend shading language.
    #[test]
    fn every_builtin_shader_translates_to_every_target() {
        let files = shader_files();
        assert_eq!(files.len(), SHADER_COUNT);

        let mut failures = Vec::new();
        for path in &files {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let source = compilable_source(path);
            for target in ShaderTarget::ALL {
                match translate_wgsl(&name, &source, target) {
                    Ok(translated) => {
                        assert_eq!(translated.target(), target);
                        assert!(
                            !translated.to_bytes().is_empty(),
                            "{name} to {target} produced nothing",
                        );
                    }
                    Err(e) => failures.push(format!("{name} -> {target}: {e}")),
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} translations failed:\n{}",
            failures.len(),
            files.len() * ShaderTarget::ALL.len(),
            failures.join("\n\n"),
        );
    }

    /// The shader with two pipelines in one module — a draw pass and a compute
    /// pass whose `@binding` numbers deliberately overlap
    /// (`shaders/rasterize.wgsl`) — keeps all three of its entry points in every
    /// target. This is the case a module-wide slot table would silently drop.
    #[test]
    fn the_rasterize_shader_keeps_every_entry_point() {
        let path = shader_files()
            .into_iter()
            .find(|p| p.file_stem().is_some_and(|s| s == "rasterize"))
            .expect("rasterize.wgsl");
        let source = compilable_source(&path);

        for target in ShaderTarget::ALL {
            let translated = translate_wgsl("rasterize", &source, target)
                .unwrap_or_else(|e| panic!("rasterize to {target} failed: {e}"));
            assert_eq!(
                translated.entry_points().len(),
                3,
                "{target} lost an entry point: {:?}",
                translated.entry_points(),
            );
        }
    }

    /// The composed shaders are the ones that need composing: a filtering
    /// shader without its helpers is not valid WGSL, so the marker the walk
    /// keys on has to be present exactly where composition happens.
    #[test]
    fn a_filtering_shader_is_only_translatable_once_composed() {
        let path = shader_files()
            .into_iter()
            .find(|p| p.file_stem().is_some_and(|s| s == "blur"))
            .expect("blur.wgsl");
        let raw = std::fs::read_to_string(&path).expect("read blur.wgsl");
        assert!(raw.contains(NEEDS_HELPERS));

        assert!(
            translate_wgsl("blur", &raw, ShaderTarget::Msl).is_err(),
            "the raw fragment must not pass as a whole module",
        );
        assert!(
            translate_wgsl("blur", &with_premultiplied_helpers(&raw), ShaderTarget::Msl).is_ok(),
            "composing the helpers in must make it translatable",
        );
    }
}

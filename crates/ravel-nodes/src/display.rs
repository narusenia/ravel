// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The viewer's display transform, on the GPU (`CM-7`).
//!
//! The evaluation pipeline composites in linear light and the screen wants
//! display-encoded bytes. `CM-3` put that conversion on the CPU, at the one
//! place the viewer's frames pass through; this module moves it to the
//! dispatch that runs **before** the readback, which does two things at once:
//!
//! * the per-pixel encode and quantise leave the CPU entirely, and
//! * the readback shrinks from 16 bytes per pixel to 4, because what comes
//!   back is the finished image rather than the float frame it was made from.
//!
//! It applies only to the interactive viewer. The render exits keep their own
//! road — `to_output_space` while the frame is still `f32`, then
//! `quantize_u8` — because a 16-bit or EXR exit needs the encoded float, and
//! because an export must not inherit the viewer's display LUT.
//! [`GpuEvalHooks`](crate::GpuEvalHooks) therefore builds a [`DisplayTransform`]
//! only when the host asks for one.
//!
//! # The agreement with the CPU
//!
//! `ravel_core::color::to_display_rgba8` stays the definition. The shader is a
//! second implementation of it and cannot be bit-identical: the CPU evaluates
//! the transfer function in `f64` and WGSL has only `f32`, whose `pow` is
//! specified to a tolerance rather than exactly. The two therefore agree
//! **within one 8-bit code per channel**, which is the criterion
//! `docs/specifications/color-management.md` records and
//! `tests/display_transform.rs` pins.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ravel_core::color::{CubeLut, DisplayChannel};
use ravel_core::id::DataTypeId;
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TextureFormat,
    TextureKey, TexturePool, TextureUsage,
};

use crate::gpu_util;

const SHADER_SRC: &str = include_str!("shaders/display_transform.wgsl");

/// Texels per row of the LUT atlas. A `size = 256` table is 16.7M entries, so
/// the strip has to be folded; 4096 keeps both axes inside the 8192 every
/// adapter guarantees, for every size `.cube` files may declare (2..=256).
const LUT_ROW: u32 = 4096;

/// A frame that has already been through the display transform: the
/// straight-alpha **BGRA** bytes the UI toolkit's image element draws.
///
/// A distinct type rather than a [`FrameBuffer`] with a byte format, because
/// these bytes are display-encoded and byte-swizzled and every reader of a
/// `FrameBuffer` is entitled to assume neither. It never crosses a node port —
/// the evaluator's `finalize` hook produces it at the viewer boundary and the
/// host consumes it — so its [`DataTypeId`] is the frame one it stands in for.
pub struct DisplayFrame {
    width: u32,
    height: u32,
    /// CPU bytes are retained for the compatibility/test fallback. The
    /// interactive macOS path normally carries `gpu` instead.
    bgra: Option<Arc<[u8]>>,
    /// The display-encoded texture the GPUI surface samples directly.
    gpu: Option<GpuFrameBuffer>,
}

impl Clone for DisplayFrame {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            bgra: self.bgra.clone(),
            gpu: self.gpu.clone(),
        }
    }
}

impl DisplayFrame {
    /// Wrap bytes that have already been through a display transform.
    ///
    /// [`DisplayTransform::run`] is what produces one in the application. This
    /// exists for the hosts' own tests, which stand a stub worker in for the
    /// GPU one; it is **not** an invitation to compute display bytes somewhere
    /// else, which is exactly the drift `CM-1` closed.
    pub fn new(width: u32, height: u32, bgra: Arc<[u8]>) -> Self {
        Self {
            width,
            height,
            bgra: Some(bgra),
            gpu: None,
        }
    }

    /// Keep a display-encoded GPU texture for a zero-copy viewer surface.
    pub fn from_gpu(frame: GpuFrameBuffer) -> Self {
        Self {
            width: frame.width(),
            height: frame.height(),
            bgra: None,
            gpu: Some(frame),
        }
    }

    /// Width in pixels of the evaluation buffer this came from.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels of the evaluation buffer this came from.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The straight-alpha BGRA bytes, tightly packed, row-major — `None` on a
    /// frame that carries [`Self::gpu_frame`] instead.
    ///
    /// The two representations are exclusive, and this returns an `Option`
    /// rather than an empty slice on purpose: a caller that measured
    /// `len() != w * h * 4` to reject a degenerate frame would otherwise read
    /// "zero-copy" as "degenerate" and blank the viewer, which is the one
    /// failure this path must not have.
    pub fn bgra(&self) -> Option<&[u8]> {
        self.bgra.as_deref()
    }

    /// The display-encoded GPU texture, when the zero-copy path is active.
    pub fn gpu_frame(&self) -> Option<&GpuFrameBuffer> {
        self.gpu.as_ref()
    }
}

impl NodeData for DisplayFrame {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FRAME_BUFFER
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_gpu_resident(&self) -> bool {
        self.gpu.is_some()
    }

    fn byte_size(&self) -> u64 {
        self.bgra.as_ref().map_or(0, |bgra| bgra.len() as u64)
            + self.gpu.as_ref().map_or(0, |frame| frame.byte_size())
    }
}

/// Uniform block of `display_transform.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    domain_min: [f32; 4],
    domain_max: [f32; 4],
    lut_size: u32,
    lut_row: u32,
    /// [`DisplayChannel`]'s wire value (`INSP-2`).
    channel: u32,
    _pad: u32,
}

/// The display transform's pipeline and its optional user LUT.
///
/// Lives on the evaluation worker, beside the hooks that own the context.
pub struct DisplayTransform {
    /// Compiled on the first frame, not in the constructor: a host builds its
    /// hooks wherever it likes (`ProjectState::new` is on the UI thread) and
    /// shader validation plus pipeline creation belongs on the worker.
    pipeline: Option<Arc<ComputePipeline>>,
    lut: Option<CubeLut>,
    /// The LUT uploaded as an atlas texture, rebuilt when [`Self::set_lut`]
    /// changes the table and never per frame.
    lut_texture: Option<GpuFrameBuffer>,
    /// Shared with the application host. `false` keeps the old worker-side
    /// readback fallback until the host proves that GPUI samples the same
    /// native device; the flag is never read from `render()`.
    zero_copy_surface: Arc<AtomicBool>,
    /// Which channel of the composite to show (`INSP-2`), shared with the
    /// host for the same reason `zero_copy_surface` is: the user picks it on
    /// the UI thread and this transform reads it on the evaluation worker.
    ///
    /// Deliberately **not** part of [`EvalContext`](ravel_core::eval::EvalContext):
    /// a display mode that entered the cache identity would make every node
    /// result a miss on a switch, and the composite this transforms does not
    /// depend on it at all.
    channel: Arc<AtomicU32>,
}

impl Default for DisplayTransform {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayTransform {
    /// A transform that has compiled nothing yet.
    pub fn new() -> Self {
        Self {
            pipeline: None,
            lut: None,
            lut_texture: None,
            zero_copy_surface: Arc::new(AtomicBool::new(false)),
            channel: Arc::new(AtomicU32::new(DisplayChannel::default().to_u32())),
        }
    }

    /// A transform whose output mode follows a host capability flag.
    pub fn with_surface_mode(zero_copy_surface: Arc<AtomicBool>) -> Self {
        Self {
            zero_copy_surface,
            ..Self::new()
        }
    }

    /// A transform whose display channel follows a cell the host writes
    /// (`INSP-2`). Read once per frame, so a switch reaches the next
    /// transform without a new transform being built.
    pub fn with_channel(mut self, channel: Arc<AtomicU32>) -> Self {
        self.channel = channel;
        self
    }

    /// Install (or clear) the user's display LUT.
    ///
    /// `None` restores the built-in transform, which is the display space's
    /// transfer function alone.
    pub fn set_lut(&mut self, lut: Option<CubeLut>) {
        if self.lut == lut {
            return;
        }
        self.lut = lut;
        // Dropping the atlas returns its texture to the pool; the next frame
        // uploads the new table.
        self.lut_texture = None;
    }

    /// The installed LUT, if any.
    pub fn lut(&self) -> Option<&CubeLut> {
        self.lut.as_ref()
    }

    /// Transform one evaluated frame — CPU- or GPU-resident — into the bytes
    /// the viewer draws.
    ///
    /// A GPU-resident input is transformed where it already is. A CPU frame is
    /// uploaded first: one transform for both keeps the answer the viewer
    /// shows independent of which processor happened to produce the frame.
    ///
    /// # Pool leases
    ///
    /// A [`PooledTexture`](ravel_gpu::PooledTexture) does **not** return itself
    /// on drop, so every failure path here has to hand its lease back or the
    /// pool loses track of the texture for the life of the process. The
    /// structure is what enforces it: the pipeline is built before anything is
    /// acquired, and the body that holds leases is a separate function whose
    /// result is handled after they are released.
    pub fn run(
        &mut self,
        ctx: &GpuContext,
        shaders: &mut ShaderManager,
        pool: &Arc<Mutex<TexturePool>>,
        value: &dyn NodeData,
    ) -> anyhow::Result<DisplayFrame> {
        // Both before any acquire: neither a shader that will not compile nor
        // a frame with no pixels may strand a texture on its way out.
        self.ensure_pipeline(shaders)?;
        match gpu_util::frame_size(value) {
            Some((width, height)) if width > 0 && height > 0 => {}
            Some((width, height)) => {
                anyhow::bail!("display transform: degenerate {width}x{height} frame")
            }
            None => anyhow::bail!("display transform: expected a frame"),
        }
        let image = gpu_util::ensure_gpu(ctx, pool, value)?;
        let result = self.transform(ctx, pool, &image);
        image.release(pool);
        result
    }

    /// The dispatch and readback, with `image` already acquired by the caller
    /// (which releases it whatever this returns).
    fn transform(
        &mut self,
        ctx: &GpuContext,
        pool: &Arc<Mutex<TexturePool>>,
        image: &gpu_util::GpuImage<'_>,
    ) -> anyhow::Result<DisplayFrame> {
        let (width, height) = image.size();
        // Acquires only on success, and stores what it acquired in `self`.
        self.ensure_lut_texture(ctx, pool)?;

        // Read per frame, not per transform: the host stores into the cell
        // whenever the user picks a mode, and the next frame carries it.
        let channel = self.channel.load(Ordering::Acquire);
        let params = match &self.lut {
            Some(lut) => {
                let (min, max) = lut.domain();
                Params {
                    domain_min: [min[0], min[1], min[2], 0.0],
                    domain_max: [max[0], max[1], max[2], 0.0],
                    lut_size: lut.size() as u32,
                    lut_row: LUT_ROW,
                    channel,
                    _pad: 0,
                }
            }
            None => Params {
                domain_min: [0.0; 4],
                domain_max: [1.0; 4],
                lut_size: 0,
                lut_row: 1,
                channel,
                _pad: 0,
            },
        };

        // `TEXTURE_BINDING` is what makes the result *sampleable*, and the
        // zero-copy path needs it: without it wgpu asks Metal for a
        // `ShaderWrite`-only texture (`wgpu-hal`'s `map_texture_usage`), and
        // GPUI's surface fragment shader may not read one. The readback road
        // never needed it, which is why it was absent.
        let output = pool.lock().unwrap().acquire(TextureKey::new(
            width,
            height,
            TextureFormat::Rgba8Unorm,
            TextureUsage::STORAGE_BINDING | TextureUsage::COPY_SRC | TextureUsage::TEXTURE_BINDING,
        ));
        let input_binding = image.binding();
        // Nothing reads the LUT slot when there is no LUT, but a bind group
        // cannot leave it empty; the input's own view stands in.
        let lut_binding = self
            .lut_texture
            .as_ref()
            .map(|frame| frame.binding())
            .unwrap_or_else(|| input_binding.clone());
        let output_binding = output.binding();
        ctx.dispatch_compute(&ComputeDispatch {
            label: "display_transform",
            pipeline: self.pipeline.as_ref().expect("built by run"),
            inputs: &[input_binding, lut_binding],
            output: &output_binding,
            uniform: bytemuck::bytes_of(&params),
            width,
            height,
        });

        if self.zero_copy_surface.load(Ordering::Acquire) {
            // GPUI's Metal renderer uses a separate native command queue. A
            // flush would only submit this dispatch; it would still race the
            // surface sampler, so the worker waits before publishing the
            // borrowed texture. Wait only for the submission containing this
            // transform rather than stalling behind unrelated older work.
            // Wait first, release unconditionally, judge afterwards — the same
            // order the readback road below uses. Returning through `?` while
            // still holding `output` would strand the lease: nothing else owns
            // it yet, so the pool would never get it back.
            if let Err(error) = ctx.wait_for_pending() {
                pool.lock().unwrap().release(output);
                return Err(anyhow::anyhow!("display transform wait: {error}"));
            }
            return Ok(DisplayFrame::from_gpu(GpuFrameBuffer::new(
                ctx.clone(),
                pool,
                output,
                width,
                height,
            )));
        }

        // Read first, release unconditionally, judge afterwards.
        let bgra = ravel_gpu::read_texture_shared(ctx, &output);
        pool.lock().unwrap().release(output);
        let bgra = bgra?;

        let expected = width as usize * height as usize * 4;
        if bgra.len() != expected {
            anyhow::bail!(
                "display transform: readback produced {} bytes for {width}x{height}, expected {expected}",
                bgra.len(),
            );
        }
        Ok(DisplayFrame {
            width,
            height,
            bgra: Some(bgra),
            gpu: None,
        })
    }

    /// Compile the shader and create the pipeline, once.
    fn ensure_pipeline(&mut self, shaders: &mut ShaderManager) -> anyhow::Result<()> {
        if self.pipeline.is_some() {
            return Ok(());
        }
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry_of(2, TextureFormat::Rgba8Unorm),
            gpu_util::uniform_layout_entry(3),
        ];
        self.pipeline = Some(
            shaders
                .compute_pipeline(
                    "display_transform",
                    SHADER_SRC,
                    "main",
                    &layout,
                    gpu_util::WORKGROUP_SIZE,
                )
                .map_err(|e| anyhow::anyhow!("display_transform.wgsl: {e}"))?,
        );
        Ok(())
    }

    /// The LUT atlas texture, uploading it on the first frame after a change.
    fn ensure_lut_texture(
        &mut self,
        ctx: &GpuContext,
        pool: &Arc<Mutex<TexturePool>>,
    ) -> anyhow::Result<()> {
        let Some(lut) = &self.lut else {
            return Ok(());
        };
        if self.lut_texture.is_none() {
            let atlas = lut_atlas(lut);
            self.lut_texture = Some(GpuFrameBuffer::from_frame_buffer(
                ctx.clone(),
                pool,
                &atlas,
            )?);
        }
        Ok(())
    }
}

/// Lay a cube out as a texture: the entries in the file's order, wrapped into
/// [`LUT_ROW`] texels per row and padded to fill the last one.
fn lut_atlas(lut: &CubeLut) -> FrameBuffer {
    let entries = lut.entries();
    let row = LUT_ROW.min(entries.len().max(1) as u32);
    let rows = (entries.len() as u32).div_ceil(row);
    let mut pixels = vec![0.0f32; (row as usize) * (rows as usize) * 4];
    for (texel, entry) in pixels.chunks_exact_mut(4).zip(entries) {
        texel[..3].copy_from_slice(entry);
        texel[3] = 1.0;
    }
    FrameBuffer::from_f32(row, rows, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LUT of every size the parser accepts has to land inside the texture
    /// limits every adapter guarantees (8192 on both axes).
    #[test]
    fn the_lut_atlas_fits_a_texture_at_every_size() {
        for size in [2usize, 3, 17, 33, 64, 256] {
            let entries = size * size * size;
            let row = LUT_ROW.min(entries as u32);
            let rows = (entries as u32).div_ceil(row);
            assert!(row <= 8192 && rows <= 8192, "size {size}: {row}x{rows}");
            assert!(
                (row as usize) * (rows as usize) >= entries,
                "size {size} does not fit"
            );
        }
    }

    #[test]
    fn the_atlas_holds_the_entries_in_file_order() {
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for i in 0..8 {
            let v = i as f32 / 7.0;
            text.push_str(&format!("{v} {v} {v}\n"));
        }
        let lut = CubeLut::parse(&text).unwrap();
        let atlas = lut_atlas(&lut);
        assert_eq!((atlas.width, atlas.height), (8, 1));
        let pixels = atlas.as_f32();
        for (i, entry) in lut.entries().iter().enumerate() {
            assert_eq!(&pixels[i * 4..i * 4 + 3], entry.as_slice());
            assert_eq!(pixels[i * 4 + 3], 1.0);
        }
    }
}

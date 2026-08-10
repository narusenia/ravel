// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hierarchical trait-based type system for node data.
//!
//! The hierarchy mirrors the specification in `docs/specifications/data-model.md`:
//!
//! ```text
//! NodeData (trait)
//! ├── BufferData
//! ├── TemporalData
//! ├── GeometricData
//! ├── NumericData
//! ├── AudioData
//! └── TextData
//! ```

use crate::id::DataTypeId;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

// ===========================================================================
// Pixel format
// ===========================================================================

/// Why a frame buffer cannot be read as four `f32` channels per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameFormatError {
    /// A single-channel buffer has no meaning where four channels are indexed.
    #[error("frame buffer is {format:?} with {channels} channel(s), expected 4 (RGBA)")]
    NotRgba {
        /// The buffer's stored format.
        format: PixelFormat,
        /// Channels that format carries.
        channels: usize,
    },

    /// The sample count disagrees with the declared size.
    #[error("frame buffer is {width}x{height}, expected {expected} samples but found {actual}")]
    LengthMismatch {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Samples the declared size implies.
        expected: usize,
        /// Samples the buffer actually holds.
        actual: usize,
    },
}

/// Pixel layout of a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4-channel RGBA, 32-bit float per channel.
    RgbaF32,
    /// 4-channel RGBA, 16-bit half float per channel (IEEE 754 binary16).
    RgbaF16,
    /// 4-channel RGBA, 8-bit unsigned normalized per channel (0–255 → 0.0–1.0).
    Rgba8,
    /// Single channel, 32-bit float (depth / mask).
    MonoF32,
}

impl PixelFormat {
    /// Number of channels per pixel.
    pub const fn channels(self) -> usize {
        match self {
            PixelFormat::RgbaF32 | PixelFormat::RgbaF16 | PixelFormat::Rgba8 => 4,
            PixelFormat::MonoF32 => 1,
        }
    }

    /// Number of bytes per pixel.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::RgbaF32 => 16,
            PixelFormat::RgbaF16 => 8,
            PixelFormat::Rgba8 => 4,
            PixelFormat::MonoF32 => 4,
        }
    }
}

// ===========================================================================
// Frame rate
// ===========================================================================

/// Rational frame rate (numerator / denominator).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    /// # Panics
    /// Panics if `den` is zero.
    pub const fn new(num: u32, den: u32) -> Self {
        assert!(den != 0, "FrameRate denominator must not be zero");
        Self { num, den }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

// ===========================================================================
// Geometric helpers
// ===========================================================================

/// Axis-aligned bounding rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 2D affine transform stored as a 3×2 matrix in column-major order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    pub m: [f32; 6],
}

impl Transform2D {
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };
}

// ===========================================================================
// Root trait — NodeData
// ===========================================================================

/// The root trait for all data types flowing through the node graph.
///
/// Every concrete data type must implement this trait so that the DAG
/// evaluator can query its runtime type tag.
pub trait NodeData: Send + Sync + 'static {
    /// Return the runtime type identifier for this data.
    fn data_type_id(&self) -> DataTypeId;

    /// Upcast to [`Any`](std::any::Any) so the evaluator and node processors
    /// can downcast a `&dyn NodeData` back to a concrete data type.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Whether this value is an opaque handle to GPU-resident memory
    /// (e.g. a texture) rather than CPU-accessible data.
    ///
    /// Core-level consumers that serialize or cache values across sessions
    /// must skip GPU-resident values (they cannot be persisted directly and
    /// must be read back through their owning crate's helpers first).
    fn is_gpu_resident(&self) -> bool {
        false
    }

    /// Approximate footprint of this value, in bytes, including everything it
    /// owns on the heap (or in VRAM, for a GPU-resident value).
    ///
    /// This is the accounting unit of
    /// [`CacheBudget`](crate::cache_budget::CacheBudget): a cache decides
    /// what to evict from the sum of the values it holds. Approximate is enough —
    /// the budget compares against a limit measured in megabytes — but the
    /// order of magnitude must be right, so an implementation counts the
    /// pixel/sample/attribute storage behind its handles and not just
    /// `size_of::<Self>()`.
    ///
    /// **There is deliberately no default implementation.** A default of `0`
    /// would silently under-account every type that forgot to override it,
    /// and the failure mode (a budget that never evicts) is invisible; a
    /// missing implementation must be a compile error instead.
    fn byte_size(&self) -> u64;
}

impl dyn NodeData {
    /// Attempt to downcast this trait object to a concrete data type `T`.
    pub fn downcast_ref<T: NodeData>(&self) -> Option<&T> {
        self.as_any().downcast_ref::<T>()
    }
}

// ===========================================================================
// Category traits
// ===========================================================================

/// Image / pixel-buffer data.
pub trait BufferData: NodeData {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn pixel_format(&self) -> PixelFormat;
}

/// Data with a temporal extent (clips, time remaps, …).
pub trait TemporalData: NodeData {
    fn duration(&self) -> Duration;
    fn frame_rate(&self) -> FrameRate;
}

/// 2D geometric data (shapes, masks, particles, …).
pub trait GeometricData: NodeData {
    fn bounds(&self) -> Rect;
    fn transform(&self) -> Transform2D;
}

/// Scalar and vector numeric data.
pub trait NumericData: NodeData {
    /// Number of components (1 for scalar, 2–4 for vectors).
    fn components(&self) -> usize;

    /// Read component values into `out`. Returns the number written.
    fn read_components(&self, out: &mut [f32]) -> usize;
}

/// Audio PCM data.
pub trait AudioData: NodeData {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u32;
    fn sample_count(&self) -> usize;
}

/// Text data.
pub trait TextData: NodeData {
    fn as_str(&self) -> &str;
}

// ===========================================================================
// Concrete types — BufferData
// ===========================================================================

/// RGBA frame buffer with a tagged pixel format.
///
/// Pixels are stored as raw bytes in row-major order; use
/// [`FrameBuffer::as_f32`] to read them as `f32` channel values. Float
/// formats are borrowed without copying, reduced-precision formats are
/// expanded into an owned vector.
#[derive(Clone, Debug)]
pub struct FrameBuffer {
    pub width: u32,
    pub height: u32,
    /// Pixel format of `data`.
    pub format: PixelFormat,
    /// Pixel bytes in row-major order.
    /// Length must equal `width * height * format.bytes_per_pixel()`.
    pub data: Arc<[u8]>,
}

impl FrameBuffer {
    /// Create a new `RgbaF32` frame buffer filled with zeroes.
    pub fn new_zeroed(width: u32, height: u32) -> Self {
        Self::with_format(width, height, PixelFormat::RgbaF32)
    }

    /// Create a new frame buffer of `format` filled with zeroes.
    pub fn with_format(width: u32, height: u32, format: PixelFormat) -> Self {
        let len = (width as usize) * (height as usize) * format.bytes_per_pixel();
        Self {
            width,
            height,
            format,
            data: vec![0u8; len].into(),
        }
    }

    /// Create an `RgbaF32` frame buffer from `f32` pixels in row-major RGBA
    /// order. `pixels.len()` must equal `width * height * 4`.
    pub fn from_f32(width: u32, height: u32, pixels: Vec<f32>) -> Self {
        debug_assert_eq!(pixels.len(), (width as usize) * (height as usize) * 4);
        Self {
            width,
            height,
            format: PixelFormat::RgbaF32,
            data: bytemuck::cast_slice(&pixels).into(),
        }
    }

    /// Read the pixels as four `f32` channels per pixel, checking that the
    /// buffer actually holds that shape.
    ///
    /// Every compositing and upload path indexes `[i..i + 4]`, so a
    /// single-channel buffer would make them read past the end and a
    /// truncated buffer would slice a partial pixel. Those callers use this
    /// instead of [`FrameBuffer::as_f32`], which reports whatever the buffer
    /// stores.
    pub fn as_rgba_f32(&self) -> Result<Cow<'_, [f32]>, FrameFormatError> {
        if self.format.channels() != 4 {
            return Err(FrameFormatError::NotRgba {
                format: self.format,
                channels: self.format.channels(),
            });
        }
        let samples = self.as_f32();
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if samples.len() != expected {
            return Err(FrameFormatError::LengthMismatch {
                width: self.width,
                height: self.height,
                expected,
                actual: samples.len(),
            });
        }
        Ok(samples)
    }

    /// Read the pixels as `f32` channel values (row-major, one value per
    /// channel per pixel).
    ///
    /// `RgbaF32` and `MonoF32` buffers are borrowed without copying;
    /// `RgbaF16` and `Rgba8` buffers are expanded into an owned vector.
    ///
    /// Callers that index four channels per pixel want
    /// [`FrameBuffer::as_rgba_f32`], which rejects the shapes that would make
    /// that indexing wrong.
    pub fn as_f32(&self) -> Cow<'_, [f32]> {
        match self.format {
            PixelFormat::RgbaF32 | PixelFormat::MonoF32 => {
                match bytemuck::try_cast_slice(&self.data) {
                    Ok(slice) => Cow::Borrowed(slice),
                    // Fall back to a decoded copy if the byte allocation is
                    // not aligned for f32 (possible for foreign-built buffers).
                    Err(_) => Cow::Owned(
                        self.data
                            .chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect(),
                    ),
                }
            }
            PixelFormat::RgbaF16 => Cow::Owned(
                self.data
                    .chunks_exact(2)
                    .map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]])))
                    .collect(),
            ),
            PixelFormat::Rgba8 => {
                Cow::Owned(self.data.iter().map(|&b| f32::from(b) / 255.0).collect())
            }
        }
    }

    /// The same picture stored as [`PixelFormat::RgbaF16`], halving its
    /// footprint.
    ///
    /// The reduction the output-stage frame cache applies when the request
    /// that produced the frame declared a floor of [`Precision::F16`] or
    /// below (`cache-plan.md`, `CACHE-5`): the entry then *is* what it
    /// promised and an `F32` request misses it rather than being served a
    /// reduced picture.
    ///
    /// Buffers that are not `RgbaF32` are returned unchanged — `RgbaF16` and
    /// `Rgba8` are already at or below the target, and `MonoF32` has no
    /// four-channel half form.
    ///
    /// [`Precision::F16`]: crate::eval::Precision::F16
    pub fn to_rgba_f16(&self) -> Self {
        if self.format != PixelFormat::RgbaF32 {
            return self.clone();
        }
        let mut bytes = Vec::with_capacity(self.data.len() / 2);
        for value in self.as_f32().iter() {
            bytes.extend_from_slice(&f32_to_f16(*value).to_le_bytes());
        }
        Self {
            width: self.width,
            height: self.height,
            format: PixelFormat::RgbaF16,
            data: bytes.into(),
        }
    }
}

/// Convert an IEEE 754 binary16 half-float bit pattern to `f32`.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = (u32::from(bits) & 0x8000) << 16;
    let exp = u32::from(bits >> 10) & 0x1f;
    let mant = u32::from(bits & 0x03ff);
    let out = if exp == 0 {
        if mant == 0 {
            sign // ±0
        } else {
            // Subnormal half: normalize into the f32 exponent range.
            let mut mant = mant;
            let mut exp_f32: i32 = 127 - 15 + 1;
            while mant & 0x0400 == 0 {
                mant <<= 1;
                exp_f32 -= 1;
            }
            mant &= 0x03ff;
            sign | ((exp_f32 as u32) << 23) | (mant << 13)
        }
    } else if exp == 0x1f {
        sign | 0x7f80_0000 | (mant << 13) // inf / NaN
    } else {
        sign | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(out)
}

/// Convert an `f32` to the nearest IEEE 754 binary16 half-float bit pattern
/// (round to nearest, ties away from zero; subnormals flush to zero).
pub(crate) fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x007f_ffff;
    if exp <= 0 {
        sign
    } else if exp >= 0x1f {
        sign | 0x7c00
    } else {
        // Round the truncated mantissa.
        let mut out = sign | ((exp as u16) << 10) | ((mant >> 13) as u16);
        if mant & 0x1000 != 0 {
            out = out.wrapping_add(1);
        }
        out
    }
}

impl NodeData for FrameBuffer {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FRAME_BUFFER
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        // `data` is the pixel *byte* blob whatever the format, so its length
        // is the footprint — not a channel or sample count.
        size_of::<Self>() as u64 + self.data.len() as u64
    }
}

impl BufferData for FrameBuffer {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn pixel_format(&self) -> PixelFormat {
        self.format
    }
}

// ===========================================================================
// Concrete types — NumericData
// ===========================================================================

/// Single `f32` value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scalar(pub f32);

impl NodeData for Scalar {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::SCALAR
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl NumericData for Scalar {
    fn components(&self) -> usize {
        1
    }
    fn read_components(&self, out: &mut [f32]) -> usize {
        if let Some(slot) = out.first_mut() {
            *slot = self.0;
            1
        } else {
            0
        }
    }
}

/// 2-component vector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec2(pub f32, pub f32);

impl NodeData for Vec2 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC2
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl NumericData for Vec2 {
    fn components(&self) -> usize {
        2
    }
    fn read_components(&self, out: &mut [f32]) -> usize {
        let vals = [self.0, self.1];
        let n = vals.len().min(out.len());
        out[..n].copy_from_slice(&vals[..n]);
        n
    }
}

/// 3-component vector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec3(pub f32, pub f32, pub f32);

impl NodeData for Vec3 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC3
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl NumericData for Vec3 {
    fn components(&self) -> usize {
        3
    }
    fn read_components(&self, out: &mut [f32]) -> usize {
        let vals = [self.0, self.1, self.2];
        let n = vals.len().min(out.len());
        out[..n].copy_from_slice(&vals[..n]);
        n
    }
}

/// 4-component vector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec4(pub f32, pub f32, pub f32, pub f32);

impl NodeData for Vec4 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC4
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl NumericData for Vec4 {
    fn components(&self) -> usize {
        4
    }
    fn read_components(&self, out: &mut [f32]) -> usize {
        let vals = [self.0, self.1, self.2, self.3];
        let n = vals.len().min(out.len());
        out[..n].copy_from_slice(&vals[..n]);
        n
    }
}

/// RGBA colour (linear, 32-bit float per channel).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Self = Self::new(1.0, 1.0, 1.0, 1.0);
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);
}

impl NodeData for Color {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::COLOR
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl NumericData for Color {
    fn components(&self) -> usize {
        4
    }
    fn read_components(&self, out: &mut [f32]) -> usize {
        let vals = [self.r, self.g, self.b, self.a];
        let n = vals.len().min(out.len());
        out[..n].copy_from_slice(&vals[..n]);
        n
    }
}

// ===========================================================================
// Concrete types — TemporalData
// ===========================================================================

/// SMPTE-style time code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeCode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub frame_rate: FrameRate,
}

impl TimeCode {
    pub fn new(hours: u8, minutes: u8, seconds: u8, frames: u8, frame_rate: FrameRate) -> Self {
        Self {
            hours,
            minutes,
            seconds,
            frames,
            frame_rate,
        }
    }

    /// Total frame count from the start.
    pub fn total_frames(&self) -> u64 {
        let num = self.frame_rate.num as u64;
        let den = self.frame_rate.den as u64;
        let total_secs =
            (self.hours as u64) * 3600 + (self.minutes as u64) * 60 + self.seconds as u64;
        (total_secs * num + self.frames as u64 * den) / den
    }
}

impl NodeData for TimeCode {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::TIME_CODE
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }
}

impl TemporalData for TimeCode {
    fn duration(&self) -> Duration {
        let total_secs = self.hours as f64 * 3600.0
            + self.minutes as f64 * 60.0
            + self.seconds as f64
            + self.frames as f64 / self.frame_rate.as_f64();
        Duration::from_secs_f64(total_secs)
    }

    fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }
}

// ===========================================================================
// Concrete types — AudioData
// ===========================================================================

/// PCM 32-bit float audio buffer.
#[derive(Clone, Debug)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: u32,
    /// Interleaved samples.
    pub data: Arc<[f32]>,
}

impl AudioBuffer {
    pub fn new(sample_rate: u32, channels: u32, data: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            data: data.into(),
        }
    }
}

impl NodeData for AudioBuffer {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::AUDIO_BUFFER
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + (self.data.len() * size_of::<f32>()) as u64
    }
}

impl AudioData for AudioBuffer {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u32 {
        self.channels
    }
    fn sample_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.data.len() / self.channels as usize
        }
    }
}

// ===========================================================================
// Concrete types — TextData
// ===========================================================================

/// Plain UTF-8 text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlainText(pub String);

impl NodeData for PlainText {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::PLAIN_TEXT
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.0.len() as u64
    }
}

impl TextData for PlainText {
    fn as_str(&self) -> &str {
        &self.0
    }
}

// ===========================================================================
// PortRecord — value of multi-output nodes
// ===========================================================================

/// The evaluated value of a node with multiple output ports.
///
/// Nodes with a single output produce their value directly; nodes with
/// several outputs (e.g. the `net.in` / `net.out` network-interface nodes)
/// produce a `PortRecord` holding one value per output port, in port order.
/// The evaluator indexes into the record with the edge's `source_port`.
#[derive(Clone)]
pub struct PortRecord(pub Vec<Arc<dyn NodeData>>);

impl std::fmt::Debug for PortRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortRecord")
            .field("len", &self.0.len())
            .finish()
    }
}

impl PortRecord {
    /// Extract the value at `port` from a (possibly record) node value.
    ///
    /// `port_count` is the number of output ports declared on the source
    /// node: for single-output nodes the value is returned unchanged (and
    /// only port 0 is valid); for multi-output nodes the value must be a
    /// `PortRecord` and is indexed. Returns `None` when the port has no
    /// value — callers treat this as an evaluation error, not as a missing
    /// connection.
    pub fn extract(
        value: &Arc<dyn NodeData>,
        port_count: usize,
        port: crate::id::OutputPortIndex,
    ) -> Option<Arc<dyn NodeData>> {
        match port_count {
            // A node declaring no outputs cannot be an edge source.
            0 => None,
            // Single-output nodes yield their value directly (port 0 only).
            1 => (port.0 == 0).then(|| value.clone()),
            _ => value
                .downcast_ref::<PortRecord>()
                .and_then(|rec| rec.0.get(port.0 as usize).cloned()),
        }
    }
}

impl NodeData for PortRecord {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::RECORD
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn byte_size(&self) -> u64 {
        // The record *is* what a network boundary caches: `net.in` / `net.out`
        // return one of these holding the scope's frame buffers. Leaving the
        // children out would make an entire layer network cost 24 bytes.
        size_of::<Self>() as u64
            + (self.0.len() * size_of::<Arc<dyn NodeData>>()) as u64
            + self.0.iter().map(|value| value.byte_size()).sum::<u64>()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "denominator must not be zero")]
    fn frame_rate_rejects_zero_denominator() {
        FrameRate::new(30, 0);
    }

    // ---- NodeData trait dispatch -------------------------------------------

    // ---- byte_size (cache accounting) --------------------------------------

    #[test]
    fn frame_buffer_byte_size_counts_the_pixel_bytes() {
        let fb = FrameBuffer::new_zeroed(1920, 1080);
        let pixels = 1920 * 1080 * 16;
        assert!(fb.byte_size() >= pixels);
        // Nothing but the struct header on top of the blob.
        assert!(fb.byte_size() < pixels + 256);
    }

    #[test]
    fn reduced_format_frame_buffer_costs_less_than_f32() {
        let f32_buffer = FrameBuffer::with_format(64, 64, PixelFormat::RgbaF32);
        let f16_buffer = FrameBuffer::with_format(64, 64, PixelFormat::RgbaF16);
        // Byte length, not sample count: half the bytes for the same pixels.
        assert_eq!(f32_buffer.byte_size() - f16_buffer.byte_size(), 64 * 64 * 8);
    }

    #[test]
    fn port_record_byte_size_includes_its_children() {
        let child = FrameBuffer::new_zeroed(256, 256);
        let child_bytes = child.byte_size();
        let record = PortRecord(vec![Arc::new(child), Arc::new(Scalar(1.0))]);
        // A network boundary's value must not read as "three pointers".
        assert!(record.byte_size() > child_bytes);
    }

    #[test]
    fn audio_buffer_byte_size_counts_samples_not_the_handle() {
        let audio = AudioBuffer::new(48_000, 2, vec![0.0; 48_000 * 2]);
        assert!(audio.byte_size() >= 48_000 * 2 * 4);
    }

    #[test]
    fn plain_text_byte_size_counts_the_string() {
        let text = PlainText("x".repeat(4096));
        assert!(text.byte_size() >= 4096);
    }

    #[test]
    fn frame_buffer_has_correct_type_id() {
        let fb = FrameBuffer::new_zeroed(4, 4);
        assert_eq!(fb.data_type_id(), DataTypeId::FRAME_BUFFER);
    }

    #[test]
    fn frame_buffer_dimensions() {
        let fb = FrameBuffer::new_zeroed(1920, 1080);
        assert_eq!(BufferData::width(&fb), 1920);
        assert_eq!(BufferData::height(&fb), 1080);
        assert_eq!(fb.pixel_format(), PixelFormat::RgbaF32);
        assert_eq!(fb.data.len(), 1920 * 1080 * 16);
        assert_eq!(fb.as_f32().len(), 1920 * 1080 * 4);
    }

    // ---- FrameBuffer precision polymorphism -------------------------------

    /// The checked accessor gives four channels per pixel or an error. Every
    /// compositing and upload path indexes `[i..i + 4]`, so the shapes that
    /// would make that indexing read past the end must not reach them.
    #[test]
    fn as_rgba_f32_accepts_only_four_channel_buffers() {
        let rgba = FrameBuffer::from_f32(2, 1, vec![0.25; 8]);
        let samples = rgba.as_rgba_f32().expect("rgba is readable");
        assert_eq!(samples.len(), 8);
        assert!(
            matches!(samples, std::borrow::Cow::Borrowed(_)),
            "an RgbaF32 buffer is borrowed, not copied"
        );

        let mono = FrameBuffer::with_format(2, 1, PixelFormat::MonoF32);
        assert_eq!(
            mono.as_rgba_f32(),
            Err(FrameFormatError::NotRgba {
                format: PixelFormat::MonoF32,
                channels: 1,
            })
        );
        // The unchecked reader still reports what the buffer stores.
        assert_eq!(mono.as_f32().len(), 2);

        let reduced = FrameBuffer::with_format(2, 1, PixelFormat::Rgba8);
        assert_eq!(
            reduced.as_rgba_f32().expect("rgba8 widens").len(),
            8,
            "a reduced format is widened to four f32 per pixel"
        );
    }

    /// A buffer whose length disagrees with its declared size is refused
    /// rather than slicing a partial pixel.
    #[test]
    fn as_rgba_f32_rejects_a_truncated_buffer() {
        let mut truncated = FrameBuffer::from_f32(2, 1, vec![0.0; 8]);
        truncated.width = 4;
        assert_eq!(
            truncated.as_rgba_f32(),
            Err(FrameFormatError::LengthMismatch {
                width: 4,
                height: 1,
                expected: 16,
                actual: 8,
            })
        );
    }

    #[test]
    fn new_zeroed_defaults_to_rgba_f32() {
        let fb = FrameBuffer::new_zeroed(2, 2);
        assert_eq!(fb.format, PixelFormat::RgbaF32);
        assert!(fb.as_f32().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn as_f32_borrows_f32_buffers() {
        let fb = FrameBuffer::from_f32(1, 1, vec![0.25, 0.5, 0.75, 1.0]);
        match fb.as_f32() {
            Cow::Borrowed(slice) => {
                assert_eq!(slice, &[0.25, 0.5, 0.75, 1.0]);
            }
            Cow::Owned(_) => panic!("RgbaF32 buffers must be borrowed, not copied"),
        }
    }

    #[test]
    fn as_f32_expands_f16_buffers() {
        let values = [0.0f32, 0.5, 1.0, -2.0, 65504.0, 0.25, -0.0, 1.0];
        let mut bytes = Vec::with_capacity(values.len() * 2);
        for &v in &values {
            bytes.extend_from_slice(&f32_to_f16(v).to_le_bytes());
        }
        let fb = FrameBuffer {
            width: 2,
            height: 1,
            format: PixelFormat::RgbaF16,
            data: bytes.into(),
        };
        match fb.as_f32() {
            Cow::Owned(slice) => {
                assert_eq!(slice.as_slice(), &values);
            }
            Cow::Borrowed(_) => panic!("RgbaF16 buffers must expand into an owned vec"),
        }
        assert_eq!(fb.pixel_format(), PixelFormat::RgbaF16);
    }

    #[test]
    fn as_f32_expands_u8_buffers() {
        let bytes = [0u8, 127, 128, 255];
        let fb = FrameBuffer {
            width: 1,
            height: 1,
            format: PixelFormat::Rgba8,
            data: bytes.as_slice().into(),
        };
        let slice = fb.as_f32();
        assert_eq!(slice[0], 0.0);
        assert!((slice[1] - 127.0 / 255.0).abs() < 1e-7);
        assert!((slice[2] - 128.0 / 255.0).abs() < 1e-7);
        assert_eq!(slice[3], 1.0);
    }

    #[test]
    fn with_format_zeroes_reduced_precision_buffers() {
        let fb = FrameBuffer::with_format(4, 2, PixelFormat::RgbaF16);
        assert_eq!(fb.data.len(), 4 * 2 * 8);
        assert!(fb.data.iter().all(|&b| b == 0));
        assert!(fb.as_f32().iter().all(|&v| v == 0.0));
        let fb8 = FrameBuffer::with_format(4, 2, PixelFormat::Rgba8);
        assert_eq!(fb8.data.len(), 4 * 2 * 4);
        assert!(fb8.as_f32().iter().all(|&v| v == 0.0));
    }

    // ---- NumericData ------------------------------------------------------

    #[test]
    fn scalar_read_components() {
        let s = Scalar(2.5);
        let mut buf = [0.0f32; 4];
        let n = s.read_components(&mut buf);
        assert_eq!(n, 1);
        assert!((buf[0] - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn vec2_read_components() {
        let v = Vec2(1.0, 2.0);
        assert_eq!(v.components(), 2);
        let mut buf = [0.0f32; 4];
        let n = v.read_components(&mut buf);
        assert_eq!(n, 2);
        assert!((buf[0] - 1.0).abs() < f32::EPSILON);
        assert!((buf[1] - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn vec3_read_components() {
        let v = Vec3(1.0, 2.0, 3.0);
        assert_eq!(v.components(), 3);
        let mut buf = [0.0f32; 4];
        let n = v.read_components(&mut buf);
        assert_eq!(n, 3);
    }

    #[test]
    fn vec4_read_components() {
        let v = Vec4(1.0, 2.0, 3.0, 4.0);
        assert_eq!(v.components(), 4);
        let mut buf = [0.0f32; 4];
        let n = v.read_components(&mut buf);
        assert_eq!(n, 4);
        assert!((buf[3] - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn color_read_components() {
        let c = Color::WHITE;
        let mut buf = [0.0f32; 4];
        let n = c.read_components(&mut buf);
        assert_eq!(n, 4);
        assert!((buf[0] - 1.0).abs() < f32::EPSILON);
        assert!((buf[3] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn read_components_with_smaller_buffer() {
        let v = Vec4(1.0, 2.0, 3.0, 4.0);
        let mut buf = [0.0f32; 2];
        let n = v.read_components(&mut buf);
        assert_eq!(n, 2);
        assert!((buf[1] - 2.0).abs() < f32::EPSILON);
    }

    // ---- TemporalData -----------------------------------------------------

    #[test]
    fn timecode_total_frames() {
        let tc = TimeCode::new(1, 0, 0, 0, FrameRate::new(30, 1));
        assert_eq!(tc.total_frames(), 108_000); // 1h × 3600s × 30fps
    }

    #[test]
    fn timecode_total_frames_with_denominator() {
        // 29.97fps = 30000/1001, 1 second = 30000/1001 ≈ 29.97 frames
        let tc = TimeCode::new(0, 0, 1, 0, FrameRate::new(30000, 1001));
        // 1 * 30000 / 1001 = 29 (integer division)
        assert_eq!(tc.total_frames(), 29);
    }

    #[test]
    fn timecode_duration() {
        let tc = TimeCode::new(0, 1, 30, 15, FrameRate::new(30, 1));
        let dur = TemporalData::duration(&tc);
        let expected = 90.5; // 60 + 30 + 15/30
        assert!((dur.as_secs_f64() - expected).abs() < 1e-9);
    }

    // ---- AudioData --------------------------------------------------------

    #[test]
    fn audio_buffer_sample_count() {
        // stereo, 48kHz, 1 second
        let samples = vec![0.0f32; 48_000 * 2];
        let ab = AudioBuffer::new(48_000, 2, samples);
        assert_eq!(AudioData::sample_rate(&ab), 48_000);
        assert_eq!(AudioData::channels(&ab), 2);
        assert_eq!(ab.sample_count(), 48_000);
    }

    // ---- TextData ---------------------------------------------------------

    #[test]
    fn plain_text_roundtrip() {
        let t = PlainText("hello world".into());
        assert_eq!(t.data_type_id(), DataTypeId::PLAIN_TEXT);
        assert_eq!(TextData::as_str(&t), "hello world");
    }

    // ---- Type id uniqueness -----------------------------------------------

    #[test]
    fn all_concrete_types_have_distinct_ids() {
        let ids = [
            FrameBuffer::new_zeroed(1, 1).data_type_id(),
            Scalar(0.0).data_type_id(),
            Vec2(0.0, 0.0).data_type_id(),
            Vec3(0.0, 0.0, 0.0).data_type_id(),
            Vec4(0.0, 0.0, 0.0, 0.0).data_type_id(),
            Color::BLACK.data_type_id(),
            TimeCode::new(0, 0, 0, 0, FrameRate::new(30, 1)).data_type_id(),
            AudioBuffer::new(44100, 1, vec![]).data_type_id(),
            PlainText(String::new()).data_type_id(),
        ];
        let set: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            set.len(),
            ids.len(),
            "all concrete types must have distinct DataTypeId"
        );
    }
}

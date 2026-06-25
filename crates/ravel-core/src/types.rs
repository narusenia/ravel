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
use std::sync::Arc;
use std::time::Duration;

// ===========================================================================
// Pixel format
// ===========================================================================

/// Pixel layout of a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4-channel RGBA, 32-bit float per channel.
    RgbaF32,
    /// Single channel, 32-bit float (depth / mask).
    MonoF32,
}

// ===========================================================================
// Frame rate
// ===========================================================================

/// Rational frame rate (numerator / denominator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// RGBA 32-bit float frame buffer.
#[derive(Clone, Debug)]
pub struct FrameBuffer {
    pub width: u32,
    pub height: u32,
    /// Pixel data in row-major RGBA order.
    /// Length must equal `width * height * 4`.
    pub data: Arc<Vec<f32>>,
}

impl FrameBuffer {
    /// Create a new frame buffer filled with zeroes.
    pub fn new_zeroed(width: u32, height: u32) -> Self {
        let len = (width as usize) * (height as usize) * 4;
        Self {
            width,
            height,
            data: Arc::new(vec![0.0; len]),
        }
    }
}

impl NodeData for FrameBuffer {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FRAME_BUFFER
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
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
        PixelFormat::RgbaF32
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2(pub f32, pub f32);

impl NodeData for Vec2 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC2
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec3(pub f32, pub f32, pub f32);

impl NodeData for Vec3 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC3
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec4(pub f32, pub f32, pub f32, pub f32);

impl NodeData for Vec4 {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::VEC4
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub data: Arc<Vec<f32>>,
}

impl AudioBuffer {
    pub fn new(sample_rate: u32, channels: u32, data: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            data: Arc::new(data),
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
}

impl TextData for PlainText {
    fn as_str(&self) -> &str {
        &self.0
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
        assert_eq!(fb.data.len(), 1920 * 1080 * 4);
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

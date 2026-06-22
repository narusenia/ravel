// Copyright The Ravel Authors.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hierarchical, trait-based type system for data flowing between nodes.
//!
//! Every value that travels along a graph edge implements [`NodeData`]. On top
//! of that base trait sit *category* traits ([`BufferData`], [`TemporalData`],
//! [`GeometricData`], [`NumericData`], [`AudioData`], [`TextData`]) that
//! describe what kind of data a concrete type carries and let the engine decide
//! — at compile time for static call sites, at runtime for dynamic ports —
//! whether two ports can be connected.

pub mod audio;
pub mod buffer;
pub mod geometric;
pub mod numeric;
pub mod temporal;
pub mod text;

pub use audio::{AudioBuffer, SpectrumData};
pub use buffer::{DepthBuffer, FrameBuffer, MultiLayerBuffer, PixelFormat};
pub use geometric::{Mask, Mesh3D, Particle, ParticleSystem, Shape};
pub use numeric::{Color, Interpolation, Keyframe, KeyframeCurve, Scalar, Vec2, Vec3, Vec4};
pub use temporal::{Clip, TimeRemap};
pub use text::{PlainText, RichText, TextStyle};

/// Stable category identifier for a concrete [`NodeData`] type.
///
/// Used for runtime type inspection and port-compatibility checks where static
/// trait bounds are not available (e.g. dynamically constructed graphs loaded
/// from a project file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataTypeId {
    // Buffer category
    FrameBuffer,
    DepthBuffer,
    MultiLayerBuffer,
    // Temporal category
    Clip,
    TimeRemap,
    // Geometric category
    Shape,
    Mask,
    Mesh3D,
    ParticleSystem,
    // Numeric category
    Scalar,
    Vec2,
    Vec3,
    Vec4,
    Color,
    Curve,
    // Audio category
    AudioBuffer,
    SpectrumData,
    // Text category
    PlainText,
    RichText,
}

impl DataTypeId {
    /// The broad category this type belongs to. Two types in the same category
    /// are connectable through a category-typed port.
    pub fn category(self) -> DataCategory {
        match self {
            DataTypeId::FrameBuffer | DataTypeId::DepthBuffer | DataTypeId::MultiLayerBuffer => {
                DataCategory::Buffer
            }
            DataTypeId::Clip | DataTypeId::TimeRemap => DataCategory::Temporal,
            DataTypeId::Shape
            | DataTypeId::Mask
            | DataTypeId::Mesh3D
            | DataTypeId::ParticleSystem => DataCategory::Geometric,
            DataTypeId::Scalar
            | DataTypeId::Vec2
            | DataTypeId::Vec3
            | DataTypeId::Vec4
            | DataTypeId::Color
            | DataTypeId::Curve => DataCategory::Numeric,
            DataTypeId::AudioBuffer | DataTypeId::SpectrumData => DataCategory::Audio,
            DataTypeId::PlainText | DataTypeId::RichText => DataCategory::Text,
        }
    }
}

/// The six top-level data categories of the Ravel type hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataCategory {
    Buffer,
    Temporal,
    Geometric,
    Numeric,
    Audio,
    Text,
}

/// Base trait implemented by every value that flows between nodes.
///
/// `Send + Sync + 'static` is required because evaluation happens across a
/// rayon work-stealing pool and results are shared through `Arc`.
pub trait NodeData: Send + Sync + 'static {
    /// Stable category identifier for this concrete type.
    fn type_id(&self) -> DataTypeId;

    /// Human-readable, stable type name (not localized — used for diagnostics).
    fn type_name(&self) -> &'static str;
}

/// Pixel-grid image data (RGBA / depth / multi-layer buffers).
pub trait BufferData: NodeData {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn pixel_format(&self) -> PixelFormat;
}

/// Time-based data such as clips and time-remap curves.
pub trait TemporalData: NodeData {
    fn duration(&self) -> Duration;
    fn frame_rate(&self) -> FrameRate;
}

/// 2D/3D geometry: shapes, masks, meshes, particle systems.
pub trait GeometricData: NodeData {
    fn bounds(&self) -> Rect;
    fn transform(&self) -> Transform2D;
}

/// Scalar and vector numeric values (also used for animation curves).
pub trait NumericData: NodeData {
    /// Representative scalar value (first component for vector types).
    fn as_f32(&self) -> f32;
    /// Number of components (1 for [`Scalar`], 4 for [`Color`], etc.).
    fn dimensions(&self) -> usize;
}

/// PCM audio buffers and spectral analysis results.
pub trait AudioData: NodeData {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn samples(&self) -> &[f32];
}

/// Plain and rich text data.
pub trait TextData: NodeData {
    fn as_str(&self) -> &str;
    fn style_info(&self) -> Option<&TextStyle>;
}

/// Rational frame rate (`num / den` frames per second), e.g. `30000/1001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const fn new(num: u32, den: u32) -> Self {
        FrameRate { num, den }
    }

    /// Frames per second as a floating-point value.
    pub fn fps(self) -> f64 {
        debug_assert!(self.den != 0, "frame rate denominator must be non-zero");
        self.num as f64 / self.den as f64
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::new(30, 1)
    }
}

/// A duration measured in seconds (32bit-float-internal processing means time
/// is always kept resolution-independent).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Duration {
    pub seconds: f64,
}

impl Duration {
    pub const fn from_seconds(seconds: f64) -> Self {
        Duration { seconds }
    }

    /// Construct a duration from a frame count at a given frame rate.
    pub fn from_frames(frames: u64, rate: FrameRate) -> Self {
        Duration {
            seconds: frames as f64 / rate.fps(),
        }
    }
}

/// A single point in time, expressed as an absolute frame index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TimeCode {
    pub frame: u64,
}

impl TimeCode {
    pub const fn new(frame: u64) -> Self {
        TimeCode { frame }
    }

    /// Convert to seconds at the given frame rate.
    pub fn to_seconds(self, rate: FrameRate) -> f64 {
        self.frame as f64 / rate.fps()
    }
}

/// A half-open range of frames `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeRange {
    pub start: TimeCode,
    pub end: TimeCode,
}

impl TimeRange {
    pub const fn new(start: TimeCode, end: TimeCode) -> Self {
        TimeRange { start, end }
    }

    /// Number of frames spanned by the range (saturating at zero).
    pub fn frame_count(self) -> u64 {
        self.end.frame.saturating_sub(self.start.frame)
    }
}

/// Axis-aligned rectangle in editor / image space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

/// A 2D affine transform stored as translation, rotation (radians) and scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub translation: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
}

impl Transform2D {
    pub fn identity() -> Self {
        Transform2D {
            translation: Vec2::new(0.0, 0.0),
            rotation: 0.0,
            scale: Vec2::new(1.0, 1.0),
        }
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Transform2D::identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_fps() {
        assert!((FrameRate::new(30, 1).fps() - 30.0).abs() < f64::EPSILON);
        assert!((FrameRate::new(30000, 1001).fps() - 29.970_029_97).abs() < 1e-6);
    }

    #[test]
    fn duration_from_frames() {
        let d = Duration::from_frames(60, FrameRate::new(30, 1));
        assert!((d.seconds - 2.0).abs() < 1e-9);
    }

    #[test]
    fn timecode_to_seconds() {
        let tc = TimeCode::new(45);
        assert!((tc.to_seconds(FrameRate::new(30, 1)) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn time_range_frame_count() {
        let r = TimeRange::new(TimeCode::new(10), TimeCode::new(25));
        assert_eq!(r.frame_count(), 15);
        // Reversed range saturates rather than underflowing.
        let r = TimeRange::new(TimeCode::new(25), TimeCode::new(10));
        assert_eq!(r.frame_count(), 0);
    }

    #[test]
    fn category_mapping() {
        assert_eq!(DataTypeId::FrameBuffer.category(), DataCategory::Buffer);
        assert_eq!(DataTypeId::Color.category(), DataCategory::Numeric);
        assert_eq!(DataTypeId::RichText.category(), DataCategory::Text);
    }
}

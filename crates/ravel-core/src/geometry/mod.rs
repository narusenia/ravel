// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Column-oriented geometry attributes with copy-on-write structural sharing.

mod attribute;
mod container;
mod field;
pub mod names;

pub use attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
pub use container::{Domain, Geometry, GeometrySummary, Primitive};
pub use field::{
    AddField, BlendField, CurveRemapField, ExpressionField, FalloffField, FalloffShape, Field,
    FieldError, FieldValue, MaxField, MultiplyField, NoiseField, apply_field,
};

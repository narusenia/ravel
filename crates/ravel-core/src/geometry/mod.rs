// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Column-oriented geometry attributes with copy-on-write structural sharing.

mod attribute;
mod container;
mod field;
pub mod names;
pub mod ops;
pub mod rotation;
pub mod triangulate;

pub use attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
pub use container::{Domain, Geometry, GeometrySummary, Positions, Primitive};
pub use field::{
    AddField, AngleField, AttributeField, BlendField, CombineMode, ComponentField, ComponentMask,
    ComposeField, ConstantField, CurveRemapField, ExpressionField, FalloffField, FalloffShape,
    Field, FieldApply, FieldError, FieldExpressionError, FieldSample, FieldValue,
    ImageSamplerField, LengthField, MaxField, MultiplyField, NoiseField, RampField, apply_field,
    component_index,
};
pub use ops::{
    AggregateMode, AttributeValue, ConnectInterpolation, ConnectMode, CurveUMode, GeometryOpError,
    PathSample, TransferMode, attribute_set, attribute_transfer, bounds_center, connect, curve_u,
    path_sample, promote_attribute,
};
pub use triangulate::Triangulator;

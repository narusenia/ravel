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
pub use container::{
    Domain, Geometry, GeometrySummary, InstanceImage, InstanceSource, InstanceTransform,
    MAX_INSTANCE_DEPTH, Positions, Primitive,
};
pub use field::{
    AddField, AngleField, AttributeField, BlendField, CombineMode, ComponentField, ComponentMask,
    ComposeField, ConstantField, CurlNoiseField, CurveRemapField, DirectionToField,
    ExpressionField, FalloffField, FalloffShape, Field, FieldApply, FieldError,
    FieldExpressionError, FieldSample, FieldValue, GradientField, ImageSamplerField, LengthField,
    MaxField, MultiplyField, NoiseField, RadialField, RampField, TimeField, TimeMode, apply_field,
    component_index,
};
pub use ops::{
    AggregateMode, AttributeValue, ConnectInterpolation, ConnectMode, CurveUMode, GeometryOpError,
    PathSample, SortMode, TransferMode, attribute_delete, attribute_set, attribute_set_in_group,
    attribute_transfer, bounds_center, connect, curve_u, element_hash, path_sample,
    promote_attribute, sort,
};
pub use triangulate::Triangulator;

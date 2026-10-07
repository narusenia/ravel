// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Column-oriented geometry attributes with copy-on-write structural sharing.

pub mod absent;
mod attribute;
mod container;
pub mod deform;
pub mod distribute;
mod field;
pub mod index_group;
pub mod names;
pub mod ops;
pub mod repeat;
pub mod rotation;
pub mod triangulate;

pub use attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
pub use container::{
    Domain, Geometry, GeometrySummary, InstanceColumns, InstanceImage, InstanceSource,
    InstanceTransform, MAX_INSTANCE_DEPTH, Positions, Primitive,
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
    Measure, PathArcTable, PathSample, SortMode, TransferMode, attribute_delete, attribute_set,
    attribute_set_in_group, attribute_transfer, blast, bounds_center, connect, curve_u,
    drawn_bounds, element_hash, expand_instances, measure, path_sample, promote_attribute,
    resample, sort, split_by_piece, stroke_reach,
};
pub use triangulate::Triangulator;

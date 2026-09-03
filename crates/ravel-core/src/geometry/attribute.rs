// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed structure-of-arrays columns and structurally shared attribute sets.

use crate::types::{Color, Vec2, Vec3, Vec4};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Attribute name storage.
///
/// [`SmolStr`](smol_str::SmolStr) stores names up to 23 bytes inline — every
/// reserved standard name fits — so keys and error values clone without heap
/// allocation.
pub type AttrName = smol_str::SmolStr;

/// The element type stored by an [`AttributeArray`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeType {
    F32,
    Vec2,
    Vec3,
    Vec4,
    Color,
    I32,
    Bool,
    Str,
}

impl fmt::Display for AttributeType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Errors produced while accessing or modifying geometry attributes.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GeometryError {
    #[error("attribute '{name}' has type {actual}, expected {expected}")]
    TypeMismatch {
        name: AttrName,
        expected: AttributeType,
        actual: AttributeType,
    },

    #[error("attribute '{name}' has length {actual}, expected {expected}")]
    LengthMismatch {
        name: AttrName,
        expected: usize,
        actual: usize,
    },

    #[error("attribute '{name}' was not found")]
    AttributeNotFound { name: AttrName },

    /// `P` accepts two column types (2D and 3D positions, REQ-3D-003), so a
    /// wrong one cannot be reported as a single expected type.
    #[error("position attribute '{name}' has type {actual}, expected Vec2 or Vec3")]
    PositionTypeMismatch {
        name: AttrName,
        actual: AttributeType,
    },

    /// A geometry carries 3D positions but the operation is only defined for
    /// planar ones. Never silently project onto xy — see the position
    /// dimension table in `docs/specifications/procedural-geometry.md`.
    #[error(
        "{operation} requires 2D positions: '{name}' is {actual}, and this operation is defined for Vec2 only"
    )]
    RequiresPlanarP {
        operation: &'static str,
        name: AttrName,
        actual: AttributeType,
    },

    /// A geometry carries mesh primitives but the operation is only defined
    /// for paths. Never silently skip the meshes — see the primitive kind
    /// table in `docs/specifications/procedural-geometry.md`.
    #[error(
        "{operation} requires path primitives: this geometry contains a Mesh, and this operation is defined for Path only"
    )]
    RequiresPathPrimitives { operation: &'static str },

    /// Hole ring starts handed to
    /// [`Triangulator`](super::triangulate::Triangulator) must be
    /// monotonically non-decreasing, because each ring spans from its own
    /// start to the next one. `earcut` panics on a descending pair, so the
    /// triangulator rejects it at the entry instead.
    #[error("hole ring {position} starts at {start}, before the previous ring's start {previous}")]
    HoleRingsOutOfOrder {
        position: usize,
        previous: usize,
        start: usize,
    },

    /// A hole ring start past the end of the vertex list. `earcut` panics
    /// while slicing the ring, so the triangulator rejects it at the entry.
    #[error("hole ring {position} starts at {start}, past the {vertex_count} vertices given")]
    HoleRingOutOfRange {
        position: usize,
        start: usize,
        vertex_count: usize,
    },

    /// The value handed to
    /// [`InstanceImage::new`](super::InstanceImage::new) is not a frame
    /// buffer.
    #[error("an instance image must be a frame buffer, but the value is data type {data_type}")]
    NotAFrameBuffer {
        /// Raw [`DataTypeId`](crate::id::DataTypeId) of the offending value.
        data_type: u32,
    },

    /// A frame buffer with no pixels has no rectangle to be stamped on.
    #[error("an instance image must have a non-zero resolution, but this one is {width}x{height}")]
    EmptyImage {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },

    /// More vertices than a triangle index can address. Mesh indices are
    /// `u32` ([`Geometry::indices`](super::Geometry::indices)), so a polygon
    /// past that bound has no representable triangulation.
    #[error("{count} vertices to triangulate, more than the {limit} a u32 index can address")]
    TooManyVertices { count: usize, limit: usize },
}

/// A homogeneous, column-oriented geometry attribute.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeArray {
    F32(Vec<f32>),
    Vec2(Vec<Vec2>),
    Vec3(Vec<Vec3>),
    Vec4(Vec<Vec4>),
    Color(Vec<Color>),
    I32(Vec<i32>),
    Bool(Vec<bool>),
    Str(Vec<String>),
}

macro_rules! typed_accessors {
    ($as_ref:ident, $as_mut:ident, $variant:ident, $ty:ty) => {
        pub fn $as_ref(&self, name: &str) -> Result<&[$ty], GeometryError> {
            match self {
                Self::$variant(values) => Ok(values),
                _ => Err(self.type_mismatch(name, AttributeType::$variant)),
            }
        }

        pub fn $as_mut(&mut self, name: &str) -> Result<&mut Vec<$ty>, GeometryError> {
            let actual = self.attr_type();
            match self {
                Self::$variant(values) => Ok(values),
                _ => Err(GeometryError::TypeMismatch {
                    name: name.into(),
                    expected: AttributeType::$variant,
                    actual,
                }),
            }
        }
    };
}

impl AttributeArray {
    /// Number of elements in the column.
    pub fn len(&self) -> usize {
        match self {
            Self::F32(values) => values.len(),
            Self::Vec2(values) => values.len(),
            Self::Vec3(values) => values.len(),
            Self::Vec4(values) => values.len(),
            Self::Color(values) => values.len(),
            Self::I32(values) => values.len(),
            Self::Bool(values) => values.len(),
            Self::Str(values) => values.len(),
        }
    }

    /// Whether the column contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element type stored by this column.
    pub fn attr_type(&self) -> AttributeType {
        match self {
            Self::F32(_) => AttributeType::F32,
            Self::Vec2(_) => AttributeType::Vec2,
            Self::Vec3(_) => AttributeType::Vec3,
            Self::Vec4(_) => AttributeType::Vec4,
            Self::Color(_) => AttributeType::Color,
            Self::I32(_) => AttributeType::I32,
            Self::Bool(_) => AttributeType::Bool,
            Self::Str(_) => AttributeType::Str,
        }
    }

    /// Approximate footprint of this column in bytes, storage included.
    ///
    /// Feeds `NodeData::byte_size` for [`Geometry`](super::Geometry), and
    /// through it the cache budget.
    pub fn byte_size(&self) -> u64 {
        let elements = self.len() as u64;
        let storage = match self {
            Self::F32(_) => elements * size_of::<f32>() as u64,
            Self::Vec2(_) => elements * size_of::<Vec2>() as u64,
            Self::Vec3(_) => elements * size_of::<Vec3>() as u64,
            Self::Vec4(_) => elements * size_of::<Vec4>() as u64,
            Self::Color(_) => elements * size_of::<Color>() as u64,
            Self::I32(_) => elements * size_of::<i32>() as u64,
            Self::Bool(_) => elements * size_of::<bool>() as u64,
            Self::Str(values) => values
                .iter()
                .map(|value| (size_of::<String>() + value.len()) as u64)
                .sum(),
        };
        size_of::<Self>() as u64 + storage
    }

    typed_accessors!(as_f32, as_f32_mut, F32, f32);
    typed_accessors!(as_vec2, as_vec2_mut, Vec2, Vec2);
    typed_accessors!(as_vec3, as_vec3_mut, Vec3, Vec3);
    typed_accessors!(as_vec4, as_vec4_mut, Vec4, Vec4);
    typed_accessors!(as_color, as_color_mut, Color, Color);
    typed_accessors!(as_i32, as_i32_mut, I32, i32);
    typed_accessors!(as_bool, as_bool_mut, Bool, bool);
    typed_accessors!(as_str, as_str_mut, Str, String);

    fn type_mismatch(&self, name: &str, expected: AttributeType) -> GeometryError {
        GeometryError::TypeMismatch {
            name: name.into(),
            expected,
            actual: self.attr_type(),
        }
    }
}

/// Named attribute columns with uniform length and copy-on-write mutation.
#[derive(Clone, Debug, Default)]
pub struct AttributeSet {
    columns: HashMap<AttrName, Arc<AttributeArray>>,
}

impl AttributeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the shared column for `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<AttributeArray>> {
        self.columns.get(name)
    }

    /// Inserts or replaces a column while preserving the set's uniform length.
    pub fn insert(
        &mut self,
        name: impl Into<AttrName>,
        column: AttributeArray,
    ) -> Result<Option<Arc<AttributeArray>>, GeometryError> {
        let name = name.into();
        if let Some(expected) = self
            .columns
            .iter()
            .find_map(|(existing_name, column)| (existing_name != &name).then(|| column.len()))
        {
            let actual = column.len();
            if actual != expected {
                return Err(GeometryError::LengthMismatch {
                    name,
                    expected,
                    actual,
                });
            }
        }

        Ok(self.columns.insert(name, Arc::new(column)))
    }

    /// Removes the column for `name`, returning it when it was there.
    ///
    /// Dropping a column cannot break the set's uniform length, so unlike
    /// [`insert`](Self::insert) this never fails. Removing the *last* column
    /// takes the set's [`element_count`](Self::element_count) to zero, which
    /// is why `Geometry` refuses to delete a position column.
    pub fn remove(&mut self, name: &str) -> Option<Arc<AttributeArray>> {
        self.columns.remove(name)
    }

    /// Returns a mutable column, cloning only that column when it is shared.
    ///
    /// The caller must not change the column's length: uniform length across
    /// a set is validated on [`insert`](Self::insert) and at `Geometry`
    /// construction, not on every mutation.
    pub fn make_mut(&mut self, name: &str) -> Result<&mut AttributeArray, GeometryError> {
        self.columns
            .get_mut(name)
            .map(Arc::make_mut)
            .ok_or_else(|| GeometryError::AttributeNotFound { name: name.into() })
    }

    /// Number of elements in the domain (the uniform column length; 0 when
    /// the set has no columns).
    pub fn element_count(&self) -> usize {
        self.columns.values().next().map_or(0, |c| c.len())
    }

    /// Iterates over `(name, column)` pairs in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&AttrName, &Arc<AttributeArray>)> {
        self.columns.iter()
    }

    /// Approximate footprint of every column in this set, in bytes.
    ///
    /// A column shared with another set (the copy-on-write `Arc`) is counted
    /// in both — the budget wants an upper bound on what dropping a cached
    /// value could free, not an exact heap census.
    pub fn byte_size(&self) -> u64 {
        self.columns
            .iter()
            .map(|(name, column)| (size_of::<AttrName>() + name.len()) as u64 + column.byte_size())
            .sum()
    }

    /// Attribute listing `(name, type)` sorted by name, for display.
    pub fn describe(&self) -> Vec<(AttrName, AttributeType)> {
        let mut listing: Vec<(AttrName, AttributeType)> = self
            .columns
            .iter()
            .map(|(name, column)| (name.clone(), column.attr_type()))
            .collect();
        listing.sort_by(|a, b| a.0.cmp(&b.0));
        listing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_columns() -> Vec<(&'static str, AttributeArray, AttributeType)> {
        vec![
            ("f32", AttributeArray::F32(vec![1.0]), AttributeType::F32),
            (
                "vec2",
                AttributeArray::Vec2(vec![Vec2(1.0, 2.0)]),
                AttributeType::Vec2,
            ),
            (
                "vec3",
                AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)]),
                AttributeType::Vec3,
            ),
            (
                "vec4",
                AttributeArray::Vec4(vec![Vec4(1.0, 2.0, 3.0, 4.0)]),
                AttributeType::Vec4,
            ),
            (
                "color",
                AttributeArray::Color(vec![Color {
                    r: 0.1,
                    g: 0.2,
                    b: 0.3,
                    a: 0.4,
                }]),
                AttributeType::Color,
            ),
            ("i32", AttributeArray::I32(vec![1]), AttributeType::I32),
            (
                "bool",
                AttributeArray::Bool(vec![true]),
                AttributeType::Bool,
            ),
            (
                "str",
                AttributeArray::Str(vec!["label".to_owned()]),
                AttributeType::Str,
            ),
        ]
    }

    #[test]
    fn insert_get_roundtrip_for_every_variant() {
        let mut attributes = AttributeSet::new();

        for (name, column, expected_type) in sample_columns() {
            attributes.insert(name, column.clone()).unwrap();
            let stored = attributes.get(name).unwrap();
            assert_eq!(stored.as_ref(), &column);
            assert_eq!(stored.attr_type(), expected_type);
        }
    }

    #[test]
    fn mutation_clones_only_the_edited_column() {
        let mut original = AttributeSet::new();
        original
            .insert("P", AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        original.insert("id", AttributeArray::I32(vec![7])).unwrap();
        let mut edited = original.clone();

        assert!(Arc::ptr_eq(
            original.get("P").unwrap(),
            edited.get("P").unwrap()
        ));
        assert!(Arc::ptr_eq(
            original.get("id").unwrap(),
            edited.get("id").unwrap()
        ));

        edited.make_mut("P").unwrap().as_vec2_mut("P").unwrap()[0] = Vec2(3.0, 4.0);

        assert!(!Arc::ptr_eq(
            original.get("P").unwrap(),
            edited.get("P").unwrap()
        ));
        assert!(Arc::ptr_eq(
            original.get("id").unwrap(),
            edited.get("id").unwrap()
        ));
        assert_eq!(
            original.get("P").unwrap().as_vec2("P").unwrap(),
            &[Vec2(0.0, 0.0)]
        );
    }

    #[test]
    fn rejects_mismatched_column_length() {
        let mut attributes = AttributeSet::new();
        attributes
            .insert("P", AttributeArray::Vec2(vec![Vec2(0.0, 0.0); 2]))
            .unwrap();

        assert_eq!(
            attributes.insert("id", AttributeArray::I32(vec![1])),
            Err(GeometryError::LengthMismatch {
                name: "id".into(),
                expected: 2,
                actual: 1,
            })
        );
        assert!(attributes.get("id").is_none());
    }

    #[test]
    fn typed_accessor_reports_type_mismatch() {
        let column = AttributeArray::I32(vec![1]);

        assert_eq!(
            column.as_f32("id"),
            Err(GeometryError::TypeMismatch {
                name: "id".into(),
                expected: AttributeType::F32,
                actual: AttributeType::I32,
            })
        );
    }
}

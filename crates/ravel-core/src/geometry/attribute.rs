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
/// This alias deliberately hides the backing type so it can be changed to a
/// compact string representation without changing the public API.
pub type AttrName = String;

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
                    name: name.to_owned(),
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
            name: name.to_owned(),
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

    /// Returns a mutable column, cloning only that column when it is shared.
    ///
    /// The caller must not change the column's length: uniform length across
    /// a set is validated on [`insert`](Self::insert) and at `Geometry`
    /// construction, not on every mutation.
    pub fn make_mut(&mut self, name: &str) -> Result<&mut AttributeArray, GeometryError> {
        self.columns
            .get_mut(name)
            .map(Arc::make_mut)
            .ok_or_else(|| GeometryError::AttributeNotFound {
                name: name.to_owned(),
            })
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
                name: "id".to_owned(),
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
                name: "id".to_owned(),
                expected: AttributeType::F32,
                actual: AttributeType::I32,
            })
        );
    }
}

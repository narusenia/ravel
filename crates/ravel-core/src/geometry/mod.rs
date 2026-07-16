// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Column-oriented geometry attributes with copy-on-write structural sharing.

mod attribute;

pub use attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};

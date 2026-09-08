// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color and shape mapping for the node editor canvas: DataTypeId →
//! port marker color/silhouette and NodeCategory → header tint. Category
//! colors are drawn from the port palette (same hues as `port_color`) so
//! a node's header and its port dots tell one consistent type story.

use gpui::{Hsla, hsla};
use ravel_core::id::DataTypeId;
use ravel_core::registry::NodeCategory;

/// Header tint color of a node, keyed on its template's category.
///
/// Categories are data-domain groupings, so each maps 1:1 onto the
/// [`port_color`] of its domain's data type: Geometry, Scene, Field, Image
/// (frame buffer), Color, Time (time code), and Utility (scalar). A
/// node's header therefore matches the port dots of the data it deals
/// with.
pub fn category_color(category: NodeCategory) -> Hsla {
    let data_type = match category {
        NodeCategory::Geometry => DataTypeId::GEOMETRY,
        NodeCategory::Scene => DataTypeId::SCENE,
        NodeCategory::Field => DataTypeId::FIELD,
        NodeCategory::Image => DataTypeId::FRAME_BUFFER,
        NodeCategory::Color => DataTypeId::COLOR,
        NodeCategory::Time => DataTypeId::TIME_CODE,
        NodeCategory::Utility => DataTypeId::SCALAR,
    };
    port_color(data_type)
}

/// Marker silhouette of a port, keyed on the port's data type so the
/// structurally different families read apart at a glance even for viewers
/// who cannot rely on the hue alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortShape {
    /// Everything without a dedicated silhouette (scalars, vectors, color,
    /// audio, text, …).
    Circle,
    /// `FRAME_BUFFER` — a rounded square, echoing an image plane.
    RoundedSquare,
    /// `GEOMETRY` — a diamond.
    Diamond,
    /// `FIELD` — a right-pointing triangle (a sampled function flowing
    /// into the node).
    Triangle,
    /// `SCENE` — a hexagon, reading as the silhouette of a volume rather
    /// than of the flat diamond a geometry gets.
    Hexagon,
}

pub fn port_shape(data_type: DataTypeId) -> PortShape {
    match data_type {
        DataTypeId::FRAME_BUFFER => PortShape::RoundedSquare,
        DataTypeId::GEOMETRY => PortShape::Diamond,
        DataTypeId::FIELD => PortShape::Triangle,
        DataTypeId::SCENE => PortShape::Hexagon,
        _ => PortShape::Circle,
    }
}

pub fn port_color(data_type: DataTypeId) -> Hsla {
    match data_type {
        DataTypeId::FRAME_BUFFER => hsla(0.08, 0.85, 0.55, 1.0),
        DataTypeId::SCALAR => hsla(0.0, 0.0, 0.6, 1.0),
        DataTypeId::VEC2 | DataTypeId::VEC3 | DataTypeId::VEC4 => hsla(0.75, 0.65, 0.55, 1.0),
        DataTypeId::COLOR => hsla(0.15, 0.85, 0.55, 1.0),
        DataTypeId::TIME_CODE => hsla(0.58, 0.70, 0.50, 1.0),
        DataTypeId::AUDIO_BUFFER => hsla(0.35, 0.70, 0.45, 1.0),
        DataTypeId::PLAIN_TEXT => hsla(0.0, 0.0, 0.85, 1.0),
        DataTypeId::GEOMETRY => hsla(0.48, 0.70, 0.50, 1.0),
        DataTypeId::FIELD => hsla(0.86, 0.68, 0.56, 1.0),
        // Chartreuse: the widest gap left in the palette, a tenth of the hue
        // circle from both the colour (0.15) and the audio (0.35) hues.
        DataTypeId::SCENE => hsla(0.25, 0.70, 0.50, 1.0),
        _ => hsla(0.0, 0.0, 0.5, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every category tint is exactly the port color of its domain's
    /// data type — headers and port dots share one palette.
    #[test]
    fn category_colors_are_their_domain_port_colors() {
        let expected = [
            (NodeCategory::Geometry, DataTypeId::GEOMETRY),
            (NodeCategory::Scene, DataTypeId::SCENE),
            (NodeCategory::Field, DataTypeId::FIELD),
            (NodeCategory::Image, DataTypeId::FRAME_BUFFER),
            (NodeCategory::Color, DataTypeId::COLOR),
            (NodeCategory::Time, DataTypeId::TIME_CODE),
            (NodeCategory::Utility, DataTypeId::SCALAR),
        ];
        for (category, data_type) in expected {
            assert_eq!(category_color(category), port_color(data_type));
        }
    }

    /// The structural families map to distinct silhouettes; every other type
    /// shares the circle.
    #[test]
    fn port_shape_maps_structural_types_to_distinct_silhouettes() {
        assert_eq!(
            port_shape(DataTypeId::FRAME_BUFFER),
            PortShape::RoundedSquare
        );
        assert_eq!(port_shape(DataTypeId::GEOMETRY), PortShape::Diamond);
        assert_eq!(port_shape(DataTypeId::FIELD), PortShape::Triangle);
        assert_eq!(port_shape(DataTypeId::SCENE), PortShape::Hexagon);
        assert_eq!(port_shape(DataTypeId::SCALAR), PortShape::Circle);
        assert_eq!(port_shape(DataTypeId::COLOR), PortShape::Circle);
        assert_eq!(port_shape(DataTypeId::AUDIO_BUFFER), PortShape::Circle);
    }

    /// A scene is not a geometry and must not be mistaken for one: the two
    /// differ in both hue and silhouette.
    #[test]
    fn scene_ports_are_distinguishable_from_every_other_family() {
        let families = [
            DataTypeId::FRAME_BUFFER,
            DataTypeId::GEOMETRY,
            DataTypeId::FIELD,
            DataTypeId::SCALAR,
            DataTypeId::COLOR,
            DataTypeId::AUDIO_BUFFER,
            DataTypeId::TIME_CODE,
            DataTypeId::PLAIN_TEXT,
        ];
        let scene = port_color(DataTypeId::SCENE);
        for other in families {
            assert_ne!(
                scene,
                port_color(other),
                "the scene port colour collides with {other:?}"
            );
            assert_ne!(
                port_shape(DataTypeId::SCENE),
                port_shape(other),
                "the scene port silhouette collides with {other:?}"
            );
        }
    }
}

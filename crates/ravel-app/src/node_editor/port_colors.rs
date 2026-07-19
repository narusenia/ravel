// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! DataTypeId → color and shape mapping for node editor port markers.

use gpui::Hsla;
use ravel_core::id::DataTypeId;

/// Marker silhouette of a port, keyed on the port's data type so the four
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
}

pub fn port_shape(data_type: DataTypeId) -> PortShape {
    match data_type {
        DataTypeId::FRAME_BUFFER => PortShape::RoundedSquare,
        DataTypeId::GEOMETRY => PortShape::Diamond,
        DataTypeId::FIELD => PortShape::Triangle,
        _ => PortShape::Circle,
    }
}

pub fn port_color(data_type: DataTypeId) -> Hsla {
    match data_type {
        DataTypeId::FRAME_BUFFER => Hsla {
            h: 0.08,
            s: 0.85,
            l: 0.55,
            a: 1.0,
        },
        DataTypeId::SCALAR => Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.6,
            a: 1.0,
        },
        DataTypeId::VEC2 | DataTypeId::VEC3 | DataTypeId::VEC4 => Hsla {
            h: 0.75,
            s: 0.65,
            l: 0.55,
            a: 1.0,
        },
        DataTypeId::COLOR => Hsla {
            h: 0.15,
            s: 0.85,
            l: 0.55,
            a: 1.0,
        },
        DataTypeId::TIME_CODE => Hsla {
            h: 0.58,
            s: 0.70,
            l: 0.50,
            a: 1.0,
        },
        DataTypeId::AUDIO_BUFFER => Hsla {
            h: 0.35,
            s: 0.70,
            l: 0.45,
            a: 1.0,
        },
        DataTypeId::PLAIN_TEXT => Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.85,
            a: 1.0,
        },
        DataTypeId::GEOMETRY => Hsla {
            h: 0.48,
            s: 0.70,
            l: 0.50,
            a: 1.0,
        },
        DataTypeId::FIELD => Hsla {
            h: 0.86,
            s: 0.68,
            l: 0.56,
            a: 1.0,
        },
        _ => Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four structural families map to distinct silhouettes; every
    /// other type shares the circle.
    #[test]
    fn port_shape_maps_structural_types_to_distinct_silhouettes() {
        assert_eq!(
            port_shape(DataTypeId::FRAME_BUFFER),
            PortShape::RoundedSquare
        );
        assert_eq!(port_shape(DataTypeId::GEOMETRY), PortShape::Diamond);
        assert_eq!(port_shape(DataTypeId::FIELD), PortShape::Triangle);
        assert_eq!(port_shape(DataTypeId::SCALAR), PortShape::Circle);
        assert_eq!(port_shape(DataTypeId::COLOR), PortShape::Circle);
        assert_eq!(port_shape(DataTypeId::AUDIO_BUFFER), PortShape::Circle);
    }
}

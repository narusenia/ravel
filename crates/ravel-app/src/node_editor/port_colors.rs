// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! DataTypeId → color mapping for node editor port dots.

use gpui::Hsla;
use ravel_core::id::DataTypeId;

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
        _ => Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        },
    }
}

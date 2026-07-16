// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for a selected Layer.

use super::{PropertyField, PropertySection};
use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::{BlendMode, Layer, LayerSource};
use ravel_core::eval::EvalContext;

pub fn sections_for_layer(layer: &Layer, ctx: &EvalContext) -> Vec<PropertySection> {
    vec![
        info_section(layer),
        transform_section(layer, ctx),
        timing_section(layer),
        compositing_section(layer),
    ]
}

fn info_section(layer: &Layer) -> PropertySection {
    let source_type = match &layer.source {
        LayerSource::Media { asset_id } => format!("Media ({asset_id})"),
        LayerSource::Solid { .. } => "Solid".into(),
        LayerSource::Shape { .. } => "Shape".into(),
        LayerSource::Text { .. } => "Text".into(),
        LayerSource::PreComp { comp_id } => format!("PreComp ({comp_id})"),
        LayerSource::Generator { .. } => "Generator".into(),
        LayerSource::Null => "Null".into(),
    };

    PropertySection {
        title: "Layer".into(),
        fields: vec![
            PropertyField::String {
                key: "name".into(),
                value: layer.name.clone(),
            },
            PropertyField::ReadOnly {
                key: "source".into(),
                value: source_type,
            },
            PropertyField::ReadOnly {
                key: "id".into(),
                value: format!("{}", layer.id),
            },
        ],
    }
}

fn channel_value(ch: &AnimationChannel, frame: u64, ctx: &EvalContext) -> f32 {
    ch.evaluate(frame, ctx)
}

fn transform_section(layer: &Layer, ctx: &EvalContext) -> PropertySection {
    let t = &layer.transform;
    // Keyframes live in layer-local time; the compiled DAG applies
    // `start_frame` via the TimeOffset node, so mirror that here.
    let frame = (ctx.frame as i64 - layer.start_frame).max(0) as u64;
    PropertySection {
        title: "Transform".into(),
        fields: vec![
            PropertyField::Float {
                key: "position_x".into(),
                value: channel_value(&t.position[0], frame, ctx),
                range: None,
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "position_y".into(),
                value: channel_value(&t.position[1], frame, ctx),
                range: None,
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_x".into(),
                value: channel_value(&t.scale[0], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_y".into(),
                value: channel_value(&t.scale[1], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "rotation".into(),
                value: channel_value(&t.rotation, frame, ctx),
                range: None,
                step: Some(0.1),
            },
            PropertyField::Float {
                key: "opacity".into(),
                value: channel_value(&layer.opacity, frame, ctx) * 100.0,
                range: Some(0.0..=100.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_x".into(),
                value: channel_value(&t.anchor_point[0], frame, ctx),
                range: None,
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_y".into(),
                value: channel_value(&t.anchor_point[1], frame, ctx),
                range: None,
                step: Some(1.0),
            },
        ],
    }
}

fn timing_section(layer: &Layer) -> PropertySection {
    PropertySection {
        title: "Timing".into(),
        fields: vec![
            PropertyField::Int {
                key: "start_frame".into(),
                value: layer.start_frame as i32,
                range: None,
                step: Some(1),
            },
            PropertyField::Int {
                key: "in_frame".into(),
                value: layer.in_frame as i32,
                range: Some(0..=i32::MAX),
                step: Some(1),
            },
            PropertyField::Int {
                key: "out_frame".into(),
                value: layer.out_frame as i32,
                range: Some(0..=i32::MAX),
                step: Some(1),
            },
            PropertyField::ReadOnly {
                key: "duration".into(),
                value: format!("{} frames", layer.duration()),
            },
        ],
    }
}

fn compositing_section(layer: &Layer) -> PropertySection {
    let blend_mode = match layer.blend_mode {
        BlendMode::Normal => "Normal",
        BlendMode::Add => "Add",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
    };

    PropertySection {
        title: "Compositing".into(),
        fields: vec![
            PropertyField::Enum {
                key: "blend_mode".into(),
                value: blend_mode.into(),
                options: vec![
                    "Normal".into(),
                    "Add".into(),
                    "Multiply".into(),
                    "Screen".into(),
                    "Overlay".into(),
                ],
            },
            PropertyField::Bool {
                key: "solo".into(),
                value: layer.solo,
            },
            PropertyField::Bool {
                key: "muted".into(),
                value: layer.muted,
            },
            PropertyField::Bool {
                key: "locked".into(),
                value: layer.locked,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::LayerSource;
    use ravel_core::id::LayerId;
    use ravel_core::types::{Color, FrameRate};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn solid_layer() -> Layer {
        Layer::new(
            LayerId::new(1),
            "Test Layer",
            LayerSource::Solid {
                color: Color::WHITE,
                width: 1920,
                height: 1080,
            },
        )
        .with_time(10, 0, 300)
    }

    #[test]
    fn sections_contains_four_groups() {
        let sections = sections_for_layer(&solid_layer(), &ctx());
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].title, "Layer");
        assert_eq!(sections[1].title, "Transform");
        assert_eq!(sections[2].title, "Timing");
        assert_eq!(sections[3].title, "Compositing");
    }

    #[test]
    fn transform_default_values() {
        let sections = sections_for_layer(&solid_layer(), &ctx());
        let transform = &sections[1];
        let pos_x = transform.fields.iter().find(|f| f.key() == "position_x");
        assert!(pos_x.is_some());
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn info_section_shows_source_type() {
        let sections = sections_for_layer(&solid_layer(), &ctx());
        let info = &sections[0];
        let source = info.fields.iter().find(|f| f.key() == "source");
        assert!(source.is_some());
        if let Some(PropertyField::ReadOnly { value, .. }) = source {
            assert_eq!(value, "Solid");
        }
    }

    #[test]
    fn transform_evaluates_in_layer_local_time() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let mut layer = solid_layer(); // start_frame = 10
        layer.transform.position[0] = AnimationChannel::keyframes(curve);

        // Comp frame 15 → layer-local frame 5 → midpoint of the curve.
        let ctx = EvalContext::new(15, FrameRate::new(30, 1), (1920, 1080));
        let sections = sections_for_layer(&layer, &ctx);
        let pos_x = sections[1].fields.iter().find(|f| f.key() == "position_x");
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.5).abs() < 1e-4);
        } else {
            panic!("position_x field missing");
        }
    }

    #[test]
    fn timing_section_shows_start_frame() {
        let sections = sections_for_layer(&solid_layer(), &ctx());
        let timing = &sections[2];
        let start = timing.fields.iter().find(|f| f.key() == "start_frame");
        if let Some(PropertyField::Int { value, .. }) = start {
            assert_eq!(*value, 10);
        }
    }
}

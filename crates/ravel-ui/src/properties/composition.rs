// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property fields of a Composition, and the reverse mapping that applies a
//! field edit back onto its settings (REQ-UI-013).
//!
//! A composition's name, resolution, frame rate, duration, and background are
//! plain fields — not `ParameterValue`s — so none of them can be keyframed and
//! none of the channel machinery in [`super::layer`] applies. The same fields
//! back the Properties composition target and the New/Settings dialogs, which
//! is why the section builder works on [`CompositionSettings`] rather than on a
//! composition that has to exist in the document yet.

use super::{PropertyField, PropertySection, PropertyValue};
use crate::document::CompositionSettings;
use ravel_core::types::FrameRate;

/// Field keys of the composition section, in display order.
pub const FIELD_NAME: &str = "name";
pub const FIELD_WIDTH: &str = "width";
pub const FIELD_HEIGHT: &str = "height";
pub const FIELD_FRAME_RATE: &str = "frame_rate";
pub const FIELD_DURATION: &str = "duration_frames";
pub const FIELD_BACKGROUND: &str = "background_color";

/// The composition section shown by the Properties panel and rendered into the
/// New/Settings dialogs.
pub fn sections_for_composition(settings: &CompositionSettings) -> Vec<PropertySection> {
    vec![PropertySection {
        title: "properties.section.composition".into(),
        fields: composition_fields(settings),
    }]
}

/// The composition fields on their own, for hosts that render them outside a
/// titled section (the dialogs).
pub fn composition_fields(settings: &CompositionSettings) -> Vec<PropertyField> {
    vec![
        PropertyField::String {
            key: FIELD_NAME.into(),
            value: settings.name.clone(),
        },
        PropertyField::Int {
            key: FIELD_WIDTH.into(),
            value: settings.resolution.0 as i32,
            range: Some(1..=i32::MAX),
            ui_range: Some(1..=7680),
            step: Some(1),
        },
        PropertyField::Int {
            key: FIELD_HEIGHT.into(),
            value: settings.resolution.1 as i32,
            range: Some(1..=i32::MAX),
            ui_range: Some(1..=4320),
            step: Some(1),
        },
        PropertyField::Float {
            key: FIELD_FRAME_RATE.into(),
            value: settings.frame_rate.as_f64() as f32,
            range: Some(f32::MIN_POSITIVE..=f32::MAX),
            ui_range: Some(1.0..=120.0),
            step: Some(0.01),
        },
        PropertyField::Int {
            key: FIELD_DURATION.into(),
            value: settings.duration_frames.min(i32::MAX as u64) as i32,
            range: Some(1..=i32::MAX),
            ui_range: Some(1..=36_000),
            step: Some(1),
        },
        PropertyField::Color {
            key: FIELD_BACKGROUND.into(),
            r: settings.background_color.r,
            g: settings.background_color.g,
            b: settings.background_color.b,
            a: settings.background_color.a,
        },
    ]
}

/// Apply an edited field onto `settings`. Returns whether anything changed —
/// an unknown key or a mismatched value type is ignored, exactly as the layer
/// and node mappings do.
///
/// Out-of-range numbers are clamped rather than rejected: the edit widgets
/// carry the same ranges, and a value that slipped past them must not be able
/// to produce a composition that cannot be built.
pub fn apply_composition_field(
    settings: &mut CompositionSettings,
    key: &str,
    value: &PropertyValue,
) -> bool {
    let edited = match (key, value) {
        (FIELD_NAME, PropertyValue::String(name)) => {
            let name = name.trim();
            // A composition with no name is unreachable in the Outliner.
            if name.is_empty() || name == settings.name {
                return false;
            }
            settings.name = name.to_string();
            true
        }
        (FIELD_WIDTH, PropertyValue::Int(width)) => {
            settings.resolution.0 = (*width).max(1) as u32;
            true
        }
        (FIELD_HEIGHT, PropertyValue::Int(height)) => {
            settings.resolution.1 = (*height).max(1) as u32;
            true
        }
        (FIELD_FRAME_RATE, PropertyValue::Float(fps)) => {
            settings.frame_rate = frame_rate_from_fps(*fps);
            true
        }
        (FIELD_DURATION, PropertyValue::Int(frames)) => {
            settings.duration_frames = (*frames).max(1) as u64;
            true
        }
        (FIELD_BACKGROUND, PropertyValue::Color { r, g, b, a }) => {
            settings.background_color = ravel_core::types::Color::new(*r, *g, *b, *a);
            true
        }
        _ => false,
    };
    edited
}

/// Turn a displayed frames-per-second number back into an exact rational rate.
///
/// The broadcast rates (23.976, 29.97, …) are `n * 1000 / 1001` and have to
/// stay exact: timecode and every frame↔time conversion derive from this
/// rational, so storing 29.97 as `2997/100` would drift against the intended
/// `30000/1001`. Integer rates stay `n/1`; anything else keeps two decimals.
pub fn frame_rate_from_fps(fps: f32) -> FrameRate {
    const NTSC_BASES: [u32; 6] = [24, 30, 48, 60, 120, 240];
    let fps = fps.max(f32::MIN_POSITIVE);
    if (fps.round() - fps).abs() < 1e-4 && fps.round() >= 1.0 {
        return FrameRate::new(fps.round() as u32, 1);
    }
    for base in NTSC_BASES {
        let ntsc = base as f32 * 1000.0 / 1001.0;
        if (fps - ntsc).abs() < 0.01 {
            return FrameRate::new(base * 1000, 1001);
        }
    }
    FrameRate::new(((fps * 100.0).round() as u32).max(1), 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::types::Color;

    fn settings() -> CompositionSettings {
        CompositionSettings {
            name: "Comp 1".into(),
            resolution: (1920, 1080),
            frame_rate: FrameRate::new(30, 1),
            duration_frames: 300,
            background_color: Color::BLACK,
        }
    }

    #[test]
    fn the_section_exposes_every_editable_field() {
        let sections = sections_for_composition(&settings());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, "properties.section.composition");
        let keys: Vec<&str> = sections[0].fields.iter().map(|field| field.key()).collect();
        assert_eq!(
            keys,
            [
                FIELD_NAME,
                FIELD_WIDTH,
                FIELD_HEIGHT,
                FIELD_FRAME_RATE,
                FIELD_DURATION,
                FIELD_BACKGROUND
            ]
        );
        assert!(
            !sections[0]
                .fields
                .iter()
                .any(|field| matches!(field, PropertyField::ReadOnly { .. })),
            "every composition field is editable"
        );
    }

    #[test]
    fn edits_round_trip_through_the_field_mapping() {
        let mut edited = settings();
        assert!(apply_composition_field(
            &mut edited,
            FIELD_NAME,
            &PropertyValue::String("Shot 2".into())
        ));
        assert!(apply_composition_field(
            &mut edited,
            FIELD_WIDTH,
            &PropertyValue::Int(1280)
        ));
        assert!(apply_composition_field(
            &mut edited,
            FIELD_HEIGHT,
            &PropertyValue::Int(720)
        ));
        assert!(apply_composition_field(
            &mut edited,
            FIELD_DURATION,
            &PropertyValue::Int(120)
        ));
        assert!(apply_composition_field(
            &mut edited,
            FIELD_BACKGROUND,
            &PropertyValue::Color {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 1.0
            }
        ));
        assert_eq!(edited.name, "Shot 2");
        assert_eq!(edited.resolution, (1280, 720));
        assert_eq!(edited.duration_frames, 120);
        assert_eq!(edited.background_color, Color::new(0.25, 0.5, 0.75, 1.0));

        // The edited settings still describe the same fields.
        let fields = composition_fields(&edited);
        assert!(matches!(
            &fields[0],
            PropertyField::String { value, .. } if value == "Shot 2"
        ));
    }

    #[test]
    fn unknown_keys_and_mismatched_types_change_nothing() {
        let mut edited = settings();
        assert!(!apply_composition_field(
            &mut edited,
            "nonexistent",
            &PropertyValue::Int(1)
        ));
        assert!(!apply_composition_field(
            &mut edited,
            FIELD_WIDTH,
            &PropertyValue::String("wide".into())
        ));
        assert!(
            !apply_composition_field(
                &mut edited,
                FIELD_NAME,
                &PropertyValue::String("   ".into())
            ),
            "a blank name is not an edit"
        );
        assert_eq!(edited, settings());
    }

    #[test]
    fn out_of_range_numbers_are_clamped_to_a_valid_composition() {
        let mut edited = settings();
        apply_composition_field(&mut edited, FIELD_WIDTH, &PropertyValue::Int(0));
        apply_composition_field(&mut edited, FIELD_HEIGHT, &PropertyValue::Int(-64));
        apply_composition_field(&mut edited, FIELD_DURATION, &PropertyValue::Int(0));
        assert_eq!(edited.resolution, (1, 1));
        assert_eq!(edited.duration_frames, 1);
    }

    #[test]
    fn broadcast_frame_rates_stay_exact_rationals() {
        assert_eq!(frame_rate_from_fps(30.0), FrameRate::new(30, 1));
        assert_eq!(frame_rate_from_fps(29.97), FrameRate::new(30_000, 1001));
        assert_eq!(frame_rate_from_fps(23.976), FrameRate::new(24_000, 1001));
        assert_eq!(frame_rate_from_fps(59.94), FrameRate::new(60_000, 1001));
        // A rate that is neither integer nor broadcast keeps two decimals.
        assert_eq!(frame_rate_from_fps(12.5), FrameRate::new(1250, 100));
        // Zero and negative input cannot produce an unconstructible rate.
        assert!(frame_rate_from_fps(0.0).num >= 1);
        assert!(frame_rate_from_fps(-5.0).num >= 1);
    }

    #[test]
    fn a_displayed_rate_edited_back_is_the_same_rate() {
        for rate in [
            FrameRate::new(30, 1),
            FrameRate::new(24, 1),
            FrameRate::new(30_000, 1001),
            FrameRate::new(24_000, 1001),
        ] {
            let mut edited = CompositionSettings {
                frame_rate: rate,
                ..settings()
            };
            let displayed = match &composition_fields(&edited)[3] {
                PropertyField::Float { value, .. } => *value,
                other => panic!("frame rate is a float field: {other:?}"),
            };
            apply_composition_field(
                &mut edited,
                FIELD_FRAME_RATE,
                &PropertyValue::Float(displayed),
            );
            assert_eq!(
                edited.frame_rate, rate,
                "{rate:?} must survive a round trip"
            );
        }
    }
}

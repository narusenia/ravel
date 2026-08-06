// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for a selected Layer, and the reverse mapping that
//! applies a field edit back onto the layer shell / its In-node custom
//! parameters (REQ-LAYER-002).

use super::{PropertyField, PropertySection, PropertyValue};
use crate::keyframes::{
    PropertyRowId, has_keyframe_at, insert_keyframe, remove_keyframe, set_channel_value,
};
use crate::panels::timeline::PropertyGroup;
use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::{AssetMetadata, AudioStreamMetadata, BlendMode, Composition, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::graph::ParameterValue;
use ravel_core::id::{LayerId, NodeId};
use ravel_core::network as net;

/// Field-key prefix of the In node's custom parameters.
pub const CUSTOM_FIELD_PREFIX: &str = "custom.";

/// Sections for one selected layer.
///
/// `comp` is the composition that owns the layer. One row describes the
/// layer's place in the stack rather than the layer itself — the Parent
/// picker lists the sibling layers it may inherit a transform from — so the
/// builder needs the neighbours as well as the layer.
///
/// `audio_asset` is the metadata of the asset the layer's [`AudioSource`]
/// points at, resolved by the caller from the document. It only feeds the
/// stream picker's option list: this crate never opens a file, and nothing on
/// the render path may probe one (audio-plan unit 4). `None` means the asset
/// is unknown or the layer has no audio.
pub fn sections_for_layer(
    layer: &Layer,
    comp: &Composition,
    ctx: &EvalContext,
    audio_asset: Option<&AssetMetadata>,
) -> Vec<PropertySection> {
    let mut sections = vec![
        info_section(layer),
        transform_section(layer, comp, ctx),
        timing_section(layer),
    ];
    if let Some(audio) = audio_section(layer, ctx, audio_asset) {
        sections.push(audio);
    }
    sections.push(compositing_section(layer));
    if let Some(custom) = custom_parameters_section(layer, ctx) {
        sections.push(custom);
    }
    sections
}

/// Displayed value of a field whose value is not the same across every
/// selected layer (REQ-UI-013 multi-selection, read-only in v1).
pub const MIXED_VALUE: &str = "—";

/// Locale keys for a read-only boolean value. The value strings of
/// [`PropertyField::ReadOnly`] are displayed verbatim unless they name a locale
/// key — this crate has no i18n dependency, so a state word is emitted as its
/// key and translated at the display boundary.
pub const VALUE_ON: &str = "properties.value.on";
pub const VALUE_OFF: &str = "properties.value.off";

/// The Parent picker's "no parent" option, emitted as a locale key for the
/// same reason as [`VALUE_ON`] / [`VALUE_OFF`]: it names a *state* rather
/// than carrying data, and this crate has no i18n dependency, so the host
/// translates it at the display boundary.
///
/// It is also the stored value of the option — [`apply_layer_field`] matches
/// the key itself, never the translated word — so switching language cannot
/// change what picking "(none)" does.
pub const PARENT_NONE: &str = "properties.value.none";

/// Property sections for a multi-layer selection: the selected count plus the
/// shell fields, read-only, with any field that differs between the layers
/// shown as [`MIXED_VALUE`] (REQ-UI-013).
///
/// Editing a whole selection at once is a later unit, so every field here is a
/// [`PropertyField::ReadOnly`] — the panel builds no editable widget it would
/// then have to route to several layers. The In node's custom parameters are
/// left out: they belong to one network and have no shared meaning across a
/// selection.
///
/// This is the multi-selection view even for a one-element slice: a caller whose
/// multi-layer target has lost all but one layer keeps a read-only panel instead
/// of gaining editable rows its edit path would then refuse. A single selected
/// layer is [`sections_for_layer`], reached through its own target.
pub fn sections_for_layers(
    layers: &[&Layer],
    comp: &Composition,
    ctx: &EvalContext,
) -> Vec<PropertySection> {
    if layers.is_empty() {
        return Vec::new();
    }
    let mut sections = vec![PropertySection {
        title: "properties.section.layers".into(),
        fields: vec![
            PropertyField::ReadOnly {
                key: "selected_count".into(),
                value: layers.len().to_string(),
            },
            merged_field(
                "name",
                layers
                    .iter()
                    .map(|layer| layer.name.clone())
                    .collect::<Vec<_>>(),
            ),
        ],
    }];
    let per_layer: Vec<Vec<PropertySection>> = layers
        .iter()
        .map(|layer| {
            vec![
                transform_section(layer, comp, ctx),
                timing_section(layer),
                compositing_section(layer),
            ]
        })
        .collect();
    sections.extend(merge_sections(&per_layer));
    sections
}

/// Collapse the same section list built for several layers into one read-only
/// list. The lists come from the same builders, so they share their shape and
/// can be merged field by field.
fn merge_sections(per_layer: &[Vec<PropertySection>]) -> Vec<PropertySection> {
    let Some(shape) = per_layer.first() else {
        return Vec::new();
    };
    shape
        .iter()
        .enumerate()
        .map(|(section_index, section)| PropertySection {
            title: section.title.clone(),
            fields: section
                .fields
                .iter()
                .enumerate()
                .map(|(field_index, field)| {
                    let values = per_layer.iter().filter_map(|sections| {
                        sections
                            .get(section_index)?
                            .fields
                            .get(field_index)
                            .map(field_display)
                    });
                    merged_field(field.key(), values.collect::<Vec<_>>())
                })
                .collect(),
        })
        .collect()
}

/// A read-only field showing the shared value, or [`MIXED_VALUE`] when the
/// selected layers disagree.
fn merged_field(key: &str, values: Vec<String>) -> PropertyField {
    let common = match values.split_first() {
        Some((first, rest)) if rest.iter().all(|value| value == first) => first.clone(),
        _ => MIXED_VALUE.to_string(),
    };
    PropertyField::ReadOnly {
        key: key.to_string(),
        value: common,
    }
}

/// The field's value as displayed text. Comparing the *displayed* text is what
/// decides "same value": two layers that read identically in the panel must not
/// be reported as differing.
fn field_display(field: &PropertyField) -> String {
    fn number(value: f32) -> String {
        let text = format!("{value:.3}");
        let trimmed = text.trim_end_matches('0').trim_end_matches('.');
        if trimmed.is_empty() || trimmed == "-" {
            "0".to_string()
        } else {
            trimmed.to_string()
        }
    }
    match field {
        PropertyField::Float { value, .. } => number(*value),
        PropertyField::Int { value, .. } => value.to_string(),
        PropertyField::Bool { value, .. } => if *value { VALUE_ON } else { VALUE_OFF }.to_string(),
        PropertyField::String { value, .. }
        | PropertyField::Enum { value, .. }
        | PropertyField::ReadOnly { value, .. } => value.clone(),
        PropertyField::Color { r, g, b, a, .. } => {
            let channel = |v: &f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!(
                "#{:02X}{:02X}{:02X}{:02X}",
                channel(r),
                channel(g),
                channel(b),
                channel(a)
            )
        }
        PropertyField::Vector { components, .. } => components
            .iter()
            .map(|value| number(*value))
            .collect::<Vec<_>>()
            .join(", "),
        // A multi-layer selection is read-only, so a curve only needs a text
        // form that differs when the curves differ. Comparing the control
        // points directly keeps "same value" honest where a point count
        // would call two different curves identical.
        PropertyField::Curve { curve, .. } => curve
            .points()
            .iter()
            .map(|point| format!("{}:{}", number(point.x), number(point.y)))
            .collect::<Vec<_>>()
            .join(", "),
        // A port list belongs to an interface node, never to a layer's shell
        // fields, so this arm is unreachable through `sections_for_layers`.
        // Naming the rows anyway keeps the "same displayed text = same value"
        // contract true if a future caller ever merges one.
        PropertyField::PortList { rows, .. } => rows
            .iter()
            .map(|row| format!("{}:{:?}", row.name, row.port_type))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn info_section(layer: &Layer) -> PropertySection {
    // Layer "kinds" are creation templates (REQ-LAYER-008); at runtime a
    // Layer kind is its network, except the shell marks frameless Audio layers.
    let source_type = if layer.has_frame_output() {
        format!("Network ({} nodes)", layer.network.node_count())
    } else if layer.audio.is_some() {
        "Audio".to_string()
    } else {
        "Null".to_string()
    };

    PropertySection {
        title: "properties.section.layer".into(),
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
    ch.evaluate(frame as f64, ctx)
}

/// The layer-local frame for channel display (REQ-LAYER-006).
fn layer_local_frame(layer: &Layer, ctx: &EvalContext) -> u64 {
    layer.local_frame(ctx.frame)
}

/// Dropdown label of one candidate parent: the layer's raw id first, then its
/// name.
///
/// The leading number is what [`parse_parent_option`] reads back out of the
/// selected option. Layer names are not unique and the shell stores a
/// [`LayerId`], so the name alone could not address a parent — the same
/// reason the audio stream picker leads with the container stream index.
fn parent_option_label(layer: &Layer) -> String {
    format!("{}: {}", layer.id.raw(), layer.name)
}

/// The layer id encoded in an option produced by [`parent_option_label`].
/// `None` for [`PARENT_NONE`] and for anything the picker never produced.
pub fn parse_parent_option(option: &str) -> Option<LayerId> {
    option
        .split(':')
        .next()
        .and_then(|id| id.trim().parse().ok())
        .map(LayerId::new)
}

/// The layers `layer` may take a transform from: every other layer of the
/// owning composition that does not already descend from it, in compositing
/// order.
///
/// Excluding the layer itself and its descendants is what keeps the picker
/// from closing a parenting cycle. Evaluation survives one — both
/// [`Composition::ancestors`] and the viewer's overlay walk carry a visited
/// guard — but a cycle is still an invalid document
/// (`validate_parenting_cycles`), so the UI must not be able to build one.
///
/// The walk is one ancestor chain per candidate, which is the whole cost of
/// the picker: layer stacks are small and the chains are short, so the list
/// is rebuilt with the section rather than cached into state that could go
/// stale against the document.
pub fn parent_candidates<'a>(comp: &'a Composition, layer: &Layer) -> Vec<&'a Layer> {
    comp.layers
        .iter()
        .filter(|candidate| candidate.id != layer.id && !comp.descends_from(candidate, layer.id))
        .collect()
}

/// The Parent picker (REQ-LAYER-001): the layer this one inherits P/R/S from,
/// or [`PARENT_NONE`].
///
/// A stored parent that the composition no longer holds reads as "no parent"
/// — `Composition::remove_layer` clears such links, so only a hand-built
/// document reaches here, and `Document::validate` rejects that one anyway.
fn parent_field(layer: &Layer, comp: &Composition) -> PropertyField {
    let mut options = vec![PARENT_NONE.to_string()];
    options.extend(
        parent_candidates(comp, layer)
            .into_iter()
            .map(parent_option_label),
    );
    let value = layer
        .parent
        .and_then(|id| comp.get_layer(id))
        .map(parent_option_label)
        .filter(|label| options.contains(label))
        .unwrap_or_else(|| PARENT_NONE.to_string());
    PropertyField::Enum {
        key: "parent".into(),
        value,
        options,
    }
}

fn transform_section(layer: &Layer, comp: &Composition, ctx: &EvalContext) -> PropertySection {
    let t = &layer.transform;
    // Keyframes live in layer-local time; mirror the shell processors'
    // `comp_frame - start_frame + in_frame` (REQ-LAYER-006).
    let frame = layer_local_frame(layer, ctx);
    PropertySection {
        title: "properties.section.transform".into(),
        fields: vec![
            // Parenting is a transform relationship — the parent chain is the
            // frame the rest of this section is expressed in — so the picker
            // leads the section it governs.
            parent_field(layer, comp),
            PropertyField::Float {
                key: "position_x".into(),
                value: channel_value(&t.position[0], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "position_y".into(),
                value: channel_value(&t.position[1], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_x".into(),
                value: channel_value(&t.scale[0], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                ui_range: Some(0.0..=400.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_y".into(),
                value: channel_value(&t.scale[1], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                ui_range: Some(0.0..=400.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "rotation".into(),
                value: channel_value(&t.rotation, frame, ctx),
                range: None,
                ui_range: Some(-360.0..=360.0),
                step: Some(0.1),
            },
            PropertyField::Float {
                key: "opacity".into(),
                value: channel_value(&layer.opacity, frame, ctx) * 100.0,
                range: Some(0.0..=100.0),
                ui_range: Some(0.0..=100.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_x".into(),
                value: channel_value(&t.anchor_point[0], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_y".into(),
                value: channel_value(&t.anchor_point[1], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
        ],
    }
}

fn timing_section(layer: &Layer) -> PropertySection {
    PropertySection {
        title: "properties.section.timing".into(),
        fields: vec![
            PropertyField::Int {
                key: "start_frame".into(),
                value: layer.start_frame as i32,
                range: None,
                ui_range: Some(-600..=600),
                step: Some(1),
            },
            PropertyField::Int {
                key: "in_frame".into(),
                value: layer.in_frame as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::Int {
                key: "out_frame".into(),
                value: layer.out_frame as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::ReadOnly {
                key: "duration".into(),
                value: format!("{} frames", layer.duration()),
            },
        ],
    }
}

/// Dropdown label of one audio stream: its container index first, then
/// whatever the probe recorded about it.
///
/// The leading number is the value the shell stores, so
/// [`parse_stream_index`] reads it back out of the selected option. The rest
/// is codec name, sample rate and channel count — numbers and identifiers,
/// deliberately not prose, because enum options reach the panel as literal
/// strings rather than locale keys.
fn audio_stream_label(stream: &AudioStreamMetadata) -> String {
    let mut details: Vec<String> = Vec::new();
    if let Some(codec) = &stream.codec {
        details.push(codec.clone());
    }
    if stream.sample_rate > 0 {
        details.push(format!("{} Hz", stream.sample_rate));
    }
    if stream.channels > 0 {
        details.push(format!("{} ch", stream.channels));
    }
    if details.is_empty() {
        stream.stream_index.to_string()
    } else {
        format!("{}: {}", stream.stream_index, details.join(" "))
    }
}

/// The container stream index encoded in a stream option produced by
/// [`audio_stream_label`].
pub fn parse_stream_index(option: &str) -> Option<usize> {
    option
        .split(':')
        .next()
        .and_then(|index| index.trim().parse().ok())
}

/// Options for the stream picker, built from the cached asset metadata.
///
/// A document written before the stream list existed knows only how many
/// audio streams the file had, so its indices are offered bare. The stored
/// index is always among the options — an offline asset, a missing asset, or
/// a file that lost the stream must still show what the layer plays instead
/// of silently displaying another stream.
fn audio_stream_options(stream_index: usize, asset: Option<&AssetMetadata>) -> Vec<String> {
    let mut options: Vec<String> = match asset {
        Some(metadata) if !metadata.audio_streams.is_empty() => metadata
            .audio_streams
            .iter()
            .map(audio_stream_label)
            .collect(),
        Some(metadata) => (0..metadata.audio_stream_count)
            .map(|index| index.to_string())
            .collect(),
        None => Vec::new(),
    };
    if !options
        .iter()
        .any(|option| parse_stream_index(option) == Some(stream_index))
    {
        options.push(stream_index.to_string());
        options.sort_by_key(|option| parse_stream_index(option).unwrap_or(usize::MAX));
    }
    options
}

fn audio_section(
    layer: &Layer,
    ctx: &EvalContext,
    audio_asset: Option<&AssetMetadata>,
) -> Option<PropertySection> {
    let audio = layer.audio.as_ref()?;
    let frame = layer_local_frame(layer, ctx);
    let stream_options = audio_stream_options(audio.stream_index, audio_asset);
    let stream_value = stream_options
        .iter()
        .find(|option| parse_stream_index(option) == Some(audio.stream_index))
        .cloned()
        .unwrap_or_else(|| audio.stream_index.to_string());
    Some(PropertySection {
        title: "properties.section.audio".into(),
        fields: vec![
            PropertyField::Float {
                key: "gain".into(),
                value: channel_value(&audio.gain, frame, ctx),
                range: Some(0.0..=f32::MAX),
                ui_range: Some(0.0..=2.0),
                step: Some(0.01),
            },
            PropertyField::Int {
                key: "fade_in_frames".into(),
                value: audio.fade_in_frames.min(i32::MAX as u64) as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::Int {
                key: "fade_out_frames".into(),
                value: audio.fade_out_frames.min(i32::MAX as u64) as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::Bool {
                key: "audio_muted".into(),
                value: audio.audio_muted,
            },
            // The stream is picked from what the container actually holds,
            // not typed as a free number: `stream_index` is a container
            // stream index, so a wrong value decodes nothing.
            PropertyField::Enum {
                key: "stream_index".into(),
                value: stream_value,
                options: stream_options,
            },
        ],
    })
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
        title: "properties.section.compositing".into(),
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
            PropertyField::Bool {
                key: "adjustment".into(),
                value: layer.adjustment,
            },
        ],
    }
}

/// The In node's custom parameters (custom output ports with a same-name
/// parameter), exposed for display/editing (REQ-LAYER-002). `None` when the
/// network has no In node or no custom parameters.
fn custom_parameters_section(layer: &Layer, ctx: &EvalContext) -> Option<PropertySection> {
    let in_node = net::find_in_node(&layer.network)?;
    let frame = layer_local_frame(layer, ctx);
    let mut fields = Vec::new();
    for port in &in_node.outputs {
        if matches!(
            port.name.as_str(),
            // `f` (PORT_FRAME_INDEX) is intentionally absent: the builtin
            // port carries no same-named parameter, so the lookup below
            // already skips it, while a legacy custom port named `f` (which
            // has one) keeps showing up as a custom parameter.
            net::PORT_BASE_GEOMETRY | net::PORT_TIME | net::PORT_SOURCE
        ) {
            continue;
        }
        let Some(param) = in_node.parameters.iter().find(|p| p.key == port.name) else {
            continue;
        };
        let key = format!("{CUSTOM_FIELD_PREFIX}{}", port.name);
        let field = match &param.value {
            ParameterValue::Float(v) => PropertyField::Float {
                key,
                value: *v,
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Channel(ch) => PropertyField::Float {
                key,
                value: ch.evaluate(frame as f64, ctx),
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Int(v) => PropertyField::Int {
                key,
                value: *v,
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Bool(v) => PropertyField::Bool { key, value: *v },
            ParameterValue::String(v) => PropertyField::String {
                key,
                value: v.clone(),
            },
            ParameterValue::Channel4(chs) => PropertyField::Color {
                key,
                r: chs[0].evaluate(frame as f64, ctx),
                g: chs[1].evaluate(frame as f64, ctx),
                b: chs[2].evaluate(frame as f64, ctx),
                a: chs[3].evaluate(frame as f64, ctx),
            },
            ParameterValue::Channel2(chs) => PropertyField::Vector {
                key,
                components: chs
                    .iter()
                    .map(|ch| ch.evaluate(frame as f64, ctx))
                    .collect(),
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Channel3(chs) => PropertyField::Vector {
                key,
                components: chs
                    .iter()
                    .map(|ch| ch.evaluate(frame as f64, ctx))
                    .collect(),
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::PathPoints(points) => PropertyField::ReadOnly {
                key,
                value: format!("{} points", points.len()),
            },
            // Curves reach the panel whole; the host renders the thumbnail
            // and the inline editor.
            ParameterValue::Curve(curve) => PropertyField::Curve {
                key,
                curve: curve.clone(),
            },
        };
        fields.push(field);
    }
    if fields.is_empty() {
        return None;
    }
    Some(PropertySection {
        title: "properties.section.parameters".into(),
        fields,
    })
}

/// Apply a Properties-panel field edit to the layer (shell attributes and
/// `custom.*` In-node parameters). Returns `false` for unknown or read-only
/// keys.
///
/// `local_frame` is the layer-local frame the edit applies at (REQ-LAYER-006):
/// transform / opacity / channel-backed custom parameters **insert or update
/// a keyframe** there when the channel is animated, and replace the constant
/// otherwise (REQ-LAYER-004). Non-animatable fields ignore it.
pub fn apply_layer_field(
    layer: &mut Layer,
    key: &str,
    value: &PropertyValue,
    local_frame: u64,
) -> bool {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        return apply_custom_parameter(layer, name, value, local_frame);
    }
    // Scale and opacity are displayed in percent.
    let channel_edit: Option<(PropertyGroup, usize, f32)> = match (key, value) {
        ("position_x", PropertyValue::Float(v)) => Some((PropertyGroup::Position, 0, *v)),
        ("position_y", PropertyValue::Float(v)) => Some((PropertyGroup::Position, 1, *v)),
        ("scale_x", PropertyValue::Float(v)) => Some((PropertyGroup::Scale, 0, *v / 100.0)),
        ("scale_y", PropertyValue::Float(v)) => Some((PropertyGroup::Scale, 1, *v / 100.0)),
        ("rotation", PropertyValue::Float(v)) => Some((PropertyGroup::Rotation, 0, *v)),
        ("opacity", PropertyValue::Float(v)) => {
            Some((PropertyGroup::Opacity, 0, (*v / 100.0).clamp(0.0, 1.0)))
        }
        ("gain", PropertyValue::Float(v)) if layer.audio.is_some() => {
            Some((PropertyGroup::AudioGain, 0, v.max(0.0)))
        }
        ("anchor_x", PropertyValue::Float(v)) => Some((PropertyGroup::AnchorPoint, 0, *v)),
        ("anchor_y", PropertyValue::Float(v)) => Some((PropertyGroup::AnchorPoint, 1, *v)),
        _ => None,
    };
    if let Some((group, component, value)) = channel_edit {
        return set_channel_value(
            layer,
            &PropertyRowId::Shell(group),
            component,
            local_frame,
            value,
        );
    }
    match (key, value) {
        ("name", PropertyValue::String(v)) => {
            layer.name = v.clone();
        }
        ("start_frame", PropertyValue::Int(v)) => {
            layer.start_frame = *v as i64;
        }
        // The display interval stays non-empty: `[in, out)` (REQ-LAYER-006).
        ("in_frame", PropertyValue::Int(v)) => {
            layer.in_frame = (*v.max(&0) as u64).min(layer.out_frame.saturating_sub(1));
        }
        ("out_frame", PropertyValue::Int(v)) => {
            layer.out_frame = (*v.max(&1) as u64).max(layer.in_frame + 1);
        }
        ("blend_mode", PropertyValue::String(v)) => {
            layer.blend_mode = match v.as_str() {
                "Normal" => BlendMode::Normal,
                "Add" => BlendMode::Add,
                "Multiply" => BlendMode::Multiply,
                "Screen" => BlendMode::Screen,
                "Overlay" => BlendMode::Overlay,
                _ => return false,
            };
        }
        // The picker's option carries the parent's layer id in front of its
        // name (see `parent_option_label`); `PARENT_NONE` clears the link.
        //
        // Only the self-parent is refused here: this function edits one
        // layer and cannot see the stack, so the *cycle* rule lives in
        // `parent_candidates`, which is what decides the options the picker
        // can produce at all.
        ("parent", PropertyValue::String(v)) => {
            if v == PARENT_NONE {
                layer.parent = None;
            } else {
                let Some(id) = parse_parent_option(v).filter(|id| *id != layer.id) else {
                    return false;
                };
                layer.parent = Some(id);
            }
        }
        ("solo", PropertyValue::Bool(v)) => layer.solo = *v,
        ("muted", PropertyValue::Bool(v)) => layer.muted = *v,
        ("locked", PropertyValue::Bool(v)) => layer.locked = *v,
        ("adjustment", PropertyValue::Bool(v)) => layer.adjustment = *v,
        ("fade_in_frames", PropertyValue::Int(v)) => {
            let Some(audio) = layer.audio.as_mut() else {
                return false;
            };
            audio.fade_in_frames = (*v).max(0) as u64;
        }
        ("fade_out_frames", PropertyValue::Int(v)) => {
            let Some(audio) = layer.audio.as_mut() else {
                return false;
            };
            audio.fade_out_frames = (*v).max(0) as u64;
        }
        ("audio_muted", PropertyValue::Bool(v)) => {
            let Some(audio) = layer.audio.as_mut() else {
                return false;
            };
            audio.audio_muted = *v;
        }
        ("stream_index", PropertyValue::Int(v)) => {
            let Some(audio) = layer.audio.as_mut() else {
                return false;
            };
            audio.stream_index = (*v).max(0) as usize;
        }
        // The picker's option carries the container stream index in front of
        // the stream's description (see `audio_stream_label`).
        ("stream_index", PropertyValue::String(v)) => {
            let Some(index) = parse_stream_index(v) else {
                return false;
            };
            let Some(audio) = layer.audio.as_mut() else {
                return false;
            };
            audio.stream_index = index;
        }
        _ => return false,
    }
    true
}

/// The animatable components backing a field key, for the key-toggle button:
/// the shell transform/opacity channels and `custom.*` In-node parameters
/// (`Float` converts to a channel on first key; `Int` / `Bool` / `String`
/// stay constant-only in v1, REQ-LAYER-004). Multi-component parameters
/// (vec/color) key all components together.
fn keyframe_components(layer: &Layer, key: &str) -> Option<(PropertyRowId, Vec<usize>)> {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        let in_node = net::find_in_node(&layer.network)?;
        let param = in_node.parameters.iter().find(|p| p.key == name)?;
        let count = match &param.value {
            ParameterValue::Float(_) | ParameterValue::Channel(_) => 1,
            ParameterValue::Channel2(_) => 2,
            ParameterValue::Channel3(_) => 3,
            ParameterValue::Channel4(_) => 4,
            _ => return None,
        };
        return Some((
            PropertyRowId::Network {
                node: in_node.id,
                key: name.to_string(),
            },
            (0..count).collect(),
        ));
    }
    let (group, component) = match key {
        "position_x" => (PropertyGroup::Position, 0),
        "position_y" => (PropertyGroup::Position, 1),
        "scale_x" => (PropertyGroup::Scale, 0),
        "scale_y" => (PropertyGroup::Scale, 1),
        "rotation" => (PropertyGroup::Rotation, 0),
        "opacity" => (PropertyGroup::Opacity, 0),
        "gain" if layer.audio.is_some() => (PropertyGroup::AudioGain, 0),
        "anchor_x" => (PropertyGroup::AnchorPoint, 0),
        "anchor_y" => (PropertyGroup::AnchorPoint, 1),
        _ => return None,
    };
    Some((PropertyRowId::Shell(group), vec![component]))
}

/// Whether the field's channel(s) have a keyframe at `local_frame` (all
/// components for vec/color fields). `None` when the field is not animatable.
pub fn layer_field_keyframed(layer: &Layer, key: &str, local_frame: u64) -> Option<bool> {
    let (row, components) = keyframe_components(layer, key)?;
    Some(
        components
            .iter()
            .all(|&c| has_keyframe_at(layer, &row, c, local_frame)),
    )
}

/// Toggle a keyframe at `local_frame` on the field's channel(s): inserts a
/// key holding the current value when any component lacks one, otherwise
/// removes the key from every component. Returns the new keyed state, or
/// `None` when the field is not animatable.
pub fn toggle_layer_keyframe(layer: &mut Layer, key: &str, local_frame: u64) -> Option<bool> {
    let (row, components) = keyframe_components(layer, key)?;
    if let PropertyRowId::Network { node, key } = &row {
        ensure_channel_parameter(layer, *node, key);
    }
    let keyed = components
        .iter()
        .all(|&c| has_keyframe_at(layer, &row, c, local_frame));
    if keyed {
        for c in components {
            remove_keyframe(layer, &row, c, local_frame);
        }
        Some(false)
    } else {
        // Partially keyed fields insert only the missing components so
        // existing keys keep their interpolation and tangents.
        for c in components {
            if !has_keyframe_at(layer, &row, c, local_frame) {
                insert_keyframe(layer, &row, c, local_frame);
            }
        }
        Some(true)
    }
}

/// Convert an In-node `Float` parameter to a constant channel so it can
/// carry keyframes. No-op for parameters that already are channels (or are
/// not key-editable at all).
fn ensure_channel_parameter(layer: &mut Layer, node: NodeId, key: &str) {
    let Some(node_ref) = layer.network.node(node) else {
        return;
    };
    let Some(param) = node_ref.parameters.iter().find(|p| p.key == key) else {
        return;
    };
    let ParameterValue::Float(value) = param.value else {
        return;
    };
    let mut updated = (**node_ref).clone();
    let param = updated
        .parameters
        .iter_mut()
        .find(|p| p.key == key)
        .expect("parameter checked above");
    param.value = ParameterValue::Channel(AnimationChannel::constant(value));
    layer.network = layer
        .network
        .clone()
        .replace_node(std::sync::Arc::new(updated));
}

/// Update the value of the In node's custom parameter `name` inside the
/// layer's owned network. Returns `false` when the parameter is missing or
/// the value type does not fit. Channel-backed parameters insert or update a
/// keyframe at `local_frame` instead of flattening to a constant
/// (REQ-LAYER-004).
fn apply_custom_parameter(
    layer: &mut Layer,
    name: &str,
    value: &PropertyValue,
    local_frame: u64,
) -> bool {
    let Some(in_node) = net::find_in_node(&layer.network) else {
        return false;
    };
    // Channel-backed params route through the keyframe model: keyframed
    // components get a key at the local frame, constant ones update their
    // constant (REQ-LAYER-004). Multi-component edits write every component.
    let param_value = in_node
        .parameters
        .iter()
        .find(|p| p.key == name)
        .map(|p| p.value.clone());
    let row = PropertyRowId::Network {
        node: in_node.id,
        key: name.to_string(),
    };
    match (&param_value, value) {
        (Some(ParameterValue::Channel(_)), PropertyValue::Float(v)) => {
            return set_channel_value(layer, &row, 0, local_frame, *v);
        }
        (Some(ParameterValue::Channel2(_)), PropertyValue::Vector(components))
            if components.len() == 2 =>
        {
            let mut applied = false;
            for (component, v) in components.iter().enumerate() {
                applied |= set_channel_value(layer, &row, component, local_frame, *v);
            }
            return applied;
        }
        (Some(ParameterValue::Channel3(_)), PropertyValue::Vector(components))
            if components.len() == 3 =>
        {
            let mut applied = false;
            for (component, v) in components.iter().enumerate() {
                applied |= set_channel_value(layer, &row, component, local_frame, *v);
            }
            return applied;
        }
        (Some(ParameterValue::Channel4(_)), PropertyValue::Color { r, g, b, a }) => {
            let mut applied = false;
            for (component, v) in [*r, *g, *b, *a].into_iter().enumerate() {
                applied |= set_channel_value(layer, &row, component, local_frame, v);
            }
            return applied;
        }
        _ => {}
    }
    let mut updated = (**in_node).clone();
    let Some(param) = updated.parameters.iter_mut().find(|p| p.key == name) else {
        return false;
    };
    match (&param.value, value) {
        (ParameterValue::Float(_), PropertyValue::Float(v)) => {
            param.value = ParameterValue::Float(*v);
        }
        (ParameterValue::Int(_), PropertyValue::Int(v)) => {
            param.value = ParameterValue::Int(*v);
        }
        (ParameterValue::Bool(_), PropertyValue::Bool(v)) => {
            param.value = ParameterValue::Bool(*v);
        }
        (ParameterValue::String(_), PropertyValue::String(v)) => {
            param.value = ParameterValue::String(v.clone());
        }
        // A curve edit replaces the whole control-point set: the editor
        // owns the ordering invariant, so there is nothing to merge here.
        (ParameterValue::Curve(_), PropertyValue::Curve(v)) => {
            param.value = ParameterValue::Curve(v.clone());
        }
        _ => return false,
    }
    layer.network = layer
        .network
        .clone()
        .replace_node(std::sync::Arc::new(updated));
    true
}

/// The In node's id, for parameter-scoped invalidation after a `custom.*`
/// edit.
pub fn in_node_id(layer: &Layer) -> Option<ravel_core::id::NodeId> {
    net::find_in_node(&layer.network).map(|n| n.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{CompId, DataTypeId, LayerId, NodeId};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn test_layer() -> Layer {
        let out = Node::new(NodeId::new(1), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new().add_node(out).unwrap();
        Layer::new(LayerId::new(1), "Test Layer", network).with_time(10, 0, 300)
    }

    /// A composition holding `layers` in the given order — what the section
    /// builders read the layer's neighbours from.
    fn comp_of(layers: &[&Layer]) -> Composition {
        let mut comp = Composition::new(
            CompId::new(1),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        );
        for layer in layers {
            comp = comp.add_layer((*layer).clone());
        }
        comp
    }

    /// Sections of a layer that is alone in its composition — the shape most
    /// of these tests exercise, where the Parent picker has no candidate.
    fn solo_sections(
        layer: &Layer,
        ctx: &EvalContext,
        audio_asset: Option<&AssetMetadata>,
    ) -> Vec<PropertySection> {
        sections_for_layer(layer, &comp_of(&[layer]), ctx, audio_asset)
    }

    /// The Parent picker of `layer` inside `comp`.
    fn parent_picker(layer: &Layer, comp: &Composition) -> (String, Vec<String>) {
        let field = sections_for_layer(layer, comp, &ctx(), None)
            .into_iter()
            .find(|section| section.title == "properties.section.transform")
            .expect("transform section")
            .fields
            .into_iter()
            .find(|field| field.key() == "parent")
            .expect("parent field");
        match field {
            PropertyField::Enum { value, options, .. } => (value, options),
            other => panic!("the Parent row is a picker, got {other:?}"),
        }
    }

    #[test]
    fn sections_contains_four_groups() {
        let sections = solo_sections(&test_layer(), &ctx(), None);
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].title, "properties.section.layer");
        assert_eq!(sections[1].title, "properties.section.transform");
        assert_eq!(sections[2].title, "properties.section.timing");
        assert_eq!(sections[3].title, "properties.section.compositing");
    }

    /// Three layers stacked bottom-to-top with ids 1..=3 and no parenting.
    fn stack() -> Vec<Layer> {
        (1..=3)
            .map(|id| {
                let mut layer = test_layer();
                layer.id = LayerId::new(id);
                layer.name = format!("L{id}");
                layer
            })
            .collect()
    }

    /// The picker offers "no parent" plus the sibling layers, addressed by
    /// their layer id so two layers sharing a name stay distinguishable.
    #[test]
    fn the_parent_picker_offers_no_parent_and_the_siblings() {
        let layers = stack();
        let comp = comp_of(&layers.iter().collect::<Vec<_>>());
        let (value, options) = parent_picker(&layers[0], &comp);
        assert_eq!(value, PARENT_NONE, "an unparented layer reads as (none)");
        assert_eq!(options, [PARENT_NONE, "2: L2", "3: L3"]);
    }

    /// A candidate that already descends from the layer would close a
    /// parenting cycle, so the picker never lists one — at any depth, and
    /// neither is the layer itself.
    #[test]
    fn the_parent_picker_omits_the_layer_and_its_descendants() {
        let mut layers = stack();
        // 1 ← 2 ← 3, plus an unrelated fourth layer.
        layers[1].parent = Some(LayerId::new(1));
        layers[2].parent = Some(LayerId::new(2));
        let mut other = test_layer();
        other.id = LayerId::new(4);
        other.name = "L4".into();
        layers.push(other);
        let comp = comp_of(&layers.iter().collect::<Vec<_>>());

        let (value, options) = parent_picker(&layers[0], &comp);
        assert_eq!(value, PARENT_NONE);
        assert_eq!(
            options,
            [PARENT_NONE, "4: L4"],
            "the direct child (2), the grandchild (3) and the layer itself are all cycles"
        );

        // The middle layer keeps its own parent as the selected option and may
        // still move to the unrelated layer, but not onto its own child.
        let (value, options) = parent_picker(&layers[1], &comp);
        assert_eq!(value, "1: L1");
        assert_eq!(options, [PARENT_NONE, "1: L1", "4: L4"]);

        assert_eq!(
            parent_candidates(&comp, &layers[0])
                .iter()
                .map(|l| l.id)
                .collect::<Vec<_>>(),
            [LayerId::new(4)]
        );
    }

    /// Picking an option parents the layer, and the child then inherits the
    /// parent's transform (REQ-LAYER-001).
    #[test]
    fn picking_a_parent_makes_the_child_follow_it() {
        use ravel_core::composition::transform::world_matrix;

        let mut layers = stack();
        layers[0].transform.position[0] = AnimationChannel::constant(100.0);
        layers[0].transform.position[1] = AnimationChannel::constant(40.0);
        let comp = comp_of(&[&layers[0], &layers[1]]);
        let before = world_matrix(&comp, &layers[1], &ctx()).apply(0.0, 0.0);
        assert_eq!(before, (0.0, 0.0), "an unparented child sits where it is");

        let (_, options) = parent_picker(&layers[1], &comp);
        let option = options
            .iter()
            .find(|option| parse_parent_option(option) == Some(LayerId::new(1)))
            .expect("the parent is among the options");
        assert!(apply_layer_field(
            &mut layers[1],
            "parent",
            &PropertyValue::String(option.clone()),
            0
        ));
        assert_eq!(layers[1].parent, Some(LayerId::new(1)));

        let comp = comp_of(&[&layers[0], &layers[1]]);
        let after = world_matrix(&comp, &layers[1], &ctx()).apply(0.0, 0.0);
        assert_eq!(after, (100.0, 40.0), "the child follows its parent");
        assert_eq!(parent_picker(&layers[1], &comp).0, "1: L1");
    }

    /// "(none)" clears the link; anything the picker never produced (a bare
    /// name, the layer itself) leaves the parent alone.
    #[test]
    fn clearing_the_parent_and_refusing_values_the_picker_never_produced() {
        let mut layer = test_layer();
        layer.parent = Some(LayerId::new(2));

        assert!(!apply_layer_field(
            &mut layer,
            "parent",
            &PropertyValue::String("L2".into()),
            0
        ));
        assert!(
            !apply_layer_field(
                &mut layer,
                "parent",
                &PropertyValue::String("1: Test Layer".into()),
                0
            ),
            "a layer cannot be its own parent"
        );
        assert_eq!(layer.parent, Some(LayerId::new(2)));

        assert!(apply_layer_field(
            &mut layer,
            "parent",
            &PropertyValue::String(PARENT_NONE.into()),
            0
        ));
        assert_eq!(layer.parent, None);
    }

    /// The Parent row is not animatable: it addresses a layer, so there is
    /// nothing for the key toggle to interpolate.
    #[test]
    fn the_parent_row_carries_no_keyframe_toggle() {
        assert_eq!(layer_field_keyframed(&test_layer(), "parent", 0), None);
    }

    /// The multi-layer view stays read-only even when the selection has shrunk
    /// to one layer: editable rows there would be refused by the edit path.
    #[test]
    fn a_shrunken_multi_selection_stays_read_only() {
        let layer = test_layer();
        let sections = sections_for_layers(&[&layer], &comp_of(&[&layer]), &ctx());
        assert_eq!(sections[0].title, "properties.section.layers");
        assert!(
            sections
                .iter()
                .flat_map(|section| &section.fields)
                .all(|field| matches!(field, PropertyField::ReadOnly { .. })),
            "every field of a multi-layer target is read-only: {sections:?}"
        );
        assert!(sections_for_layers(&[], &comp_of(&[]), &ctx()).is_empty());
    }

    /// A multi-layer selection reports its size, shows the fields the layers
    /// agree on, and marks the rest as mixed — read-only throughout, because
    /// editing a whole selection is a later unit.
    #[test]
    fn several_selected_layers_merge_into_read_only_common_fields() {
        let first = test_layer();
        let mut second = test_layer();
        second.name = "Other".into();
        second.transform.position[0] = AnimationChannel::constant(120.0);
        second.muted = true;

        let sections =
            sections_for_layers(&[&first, &second], &comp_of(&[&first, &second]), &ctx());
        assert_eq!(sections[0].title, "properties.section.layers");
        let field = |section: &PropertySection, key: &str| {
            section
                .fields
                .iter()
                .find(|field| field.key() == key)
                .unwrap_or_else(|| panic!("{key} missing"))
                .clone()
        };
        let read_only = |field: PropertyField| match field {
            PropertyField::ReadOnly { value, .. } => value,
            other => panic!("a multi-selection field must be read-only: {other:?}"),
        };

        assert_eq!(read_only(field(&sections[0], "selected_count")), "2");
        assert_eq!(read_only(field(&sections[0], "name")), MIXED_VALUE);

        let transform = &sections[1];
        assert_eq!(transform.title, "properties.section.transform");
        assert_eq!(read_only(field(transform, "position_x")), MIXED_VALUE);
        assert_eq!(
            read_only(field(transform, "position_y")),
            "0",
            "a field both layers share shows its value"
        );

        let compositing = &sections[3];
        assert_eq!(read_only(field(compositing, "muted")), MIXED_VALUE);
        assert_eq!(
            read_only(field(compositing, "locked")),
            VALUE_OFF,
            "a state word is a locale key, translated at the display boundary"
        );
        // The timing fields are identical, so they resolve rather than mix.
        assert_eq!(read_only(field(&sections[2], "start_frame")), "10");
        // Per-network custom parameters have no shared meaning here.
        assert_eq!(sections.len(), 4);
    }

    #[test]
    fn transform_default_values() {
        let sections = solo_sections(&test_layer(), &ctx(), None);
        let transform = &sections[1];
        let pos_x = transform.fields.iter().find(|f| f.key() == "position_x");
        assert!(pos_x.is_some());
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn info_section_shows_source_type() {
        let sections = solo_sections(&test_layer(), &ctx(), None);
        let info = &sections[0];
        let source = info.fields.iter().find(|f| f.key() == "source");
        assert!(source.is_some());
        if let Some(PropertyField::ReadOnly { value, .. }) = source {
            assert_eq!(value, "Network (1 nodes)");
        }
    }

    #[test]
    fn info_section_shows_null_for_frameless_network() {
        let layer = Layer::new(LayerId::new(9), "Null", Graph::new());
        let sections = solo_sections(&layer, &ctx(), None);
        let source = sections[0].fields.iter().find(|f| f.key() == "source");
        if let Some(PropertyField::ReadOnly { value, .. }) = source {
            assert_eq!(value, "Null");
        } else {
            panic!("source field missing");
        }
    }

    #[test]
    fn audio_section_is_conditional_and_edits_the_shell_source() {
        let mut layer = test_layer();
        assert!(
            solo_sections(&layer, &ctx(), None)
                .iter()
                .all(|section| section.title != "properties.section.audio")
        );

        layer.audio = Some(ravel_core::composition::AudioSource {
            asset_id: "dialogue".into(),
            stream_index: 2,
            gain: AnimationChannel::constant(0.75),
            fade_in_frames: 3,
            fade_out_frames: 4,
            audio_muted: false,
        });
        let sections = solo_sections(&layer, &ctx(), None);
        let audio = sections
            .iter()
            .find(|section| section.title == "properties.section.audio")
            .expect("audio section");
        assert_eq!(
            audio
                .fields
                .iter()
                .map(PropertyField::key)
                .collect::<Vec<_>>(),
            [
                "gain",
                "fade_in_frames",
                "fade_out_frames",
                "audio_muted",
                "stream_index",
            ]
        );

        assert!(apply_layer_field(
            &mut layer,
            "gain",
            &PropertyValue::Float(1.25),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "fade_in_frames",
            &PropertyValue::Int(12),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "fade_out_frames",
            &PropertyValue::Int(18),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "audio_muted",
            &PropertyValue::Bool(true),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "stream_index",
            &PropertyValue::Int(5),
            0
        ));
        let audio = layer.audio.as_ref().unwrap();
        assert!((audio.gain.evaluate(0.0, &ctx()) - 1.25).abs() < f32::EPSILON);
        assert_eq!(audio.fade_in_frames, 12);
        assert_eq!(audio.fade_out_frames, 18);
        assert!(audio.audio_muted);
        assert_eq!(audio.stream_index, 5);
    }

    fn asset_with_streams() -> AssetMetadata {
        AssetMetadata {
            audio_stream_count: 2,
            audio_streams: vec![
                AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("aac".into()),
                    sample_rate: 48_000,
                    channels: 2,
                },
                AudioStreamMetadata {
                    stream_index: 2,
                    codec: Some("pcm_s16le".into()),
                    sample_rate: 44_100,
                    channels: 1,
                },
            ],
            ..AssetMetadata::default()
        }
    }

    fn stream_field(layer: &Layer, asset: Option<&AssetMetadata>) -> PropertyField {
        solo_sections(layer, &ctx(), asset)
            .into_iter()
            .find(|section| section.title == "properties.section.audio")
            .expect("audio section")
            .fields
            .into_iter()
            .find(|field| field.key() == "stream_index")
            .expect("stream field")
    }

    /// The stream picker lists what the container holds, taken from the cached
    /// asset metadata — the panel never probes the file.
    #[test]
    fn stream_picker_lists_the_assets_audio_streams() {
        let mut layer = test_layer();
        layer.audio = Some(ravel_core::composition::AudioSource::new("clip", 2));
        let asset = asset_with_streams();

        let PropertyField::Enum { value, options, .. } = stream_field(&layer, Some(&asset)) else {
            panic!("the stream field is a picker, not a free number");
        };
        assert_eq!(
            options,
            ["1: aac 48000 Hz 2 ch", "2: pcm_s16le 44100 Hz 1 ch"]
        );
        assert_eq!(value, "2: pcm_s16le 44100 Hz 1 ch", "the stored stream");
    }

    /// Picking an option applies its container stream index to the shell.
    #[test]
    fn picking_a_stream_applies_its_container_index() {
        let mut layer = test_layer();
        layer.audio = Some(ravel_core::composition::AudioSource::new("clip", 1));
        assert!(apply_layer_field(
            &mut layer,
            "stream_index",
            &PropertyValue::String("2: pcm_s16le 44100 Hz 1 ch".into()),
            0
        ));
        assert_eq!(layer.audio.as_ref().unwrap().stream_index, 2);

        // A value the picker never produced is rejected rather than silently
        // resetting the stream to 0.
        assert!(!apply_layer_field(
            &mut layer,
            "stream_index",
            &PropertyValue::String("none".into()),
            0
        ));
        assert_eq!(layer.audio.as_ref().unwrap().stream_index, 2);
    }

    /// Without the stream list (older metadata) the bare indices are offered;
    /// with no metadata at all only the stored index is, so the panel always
    /// shows what the layer actually plays.
    #[test]
    fn stream_picker_falls_back_to_the_stored_index() {
        let mut layer = test_layer();
        layer.audio = Some(ravel_core::composition::AudioSource::new("clip", 3));

        let PropertyField::Enum { value, options, .. } = stream_field(&layer, None) else {
            panic!("expected a picker");
        };
        assert_eq!(options, ["3"]);
        assert_eq!(value, "3");

        let legacy = AssetMetadata {
            audio_stream_count: 2,
            ..AssetMetadata::default()
        };
        let PropertyField::Enum { value, options, .. } = stream_field(&layer, Some(&legacy)) else {
            panic!("expected a picker");
        };
        assert_eq!(
            options,
            ["0", "1", "3"],
            "counted streams plus the stored one"
        );
        assert_eq!(value, "3");
    }

    #[test]
    fn audio_gain_uses_layer_local_keyframes() {
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let mut layer = test_layer(); // start_frame = 10
        layer.audio = Some(ravel_core::composition::AudioSource {
            gain: AnimationChannel::keyframes(curve),
            ..Default::default()
        });
        let eval = EvalContext::new(15, FrameRate::new(30, 1), (1920, 1080));
        let sections = solo_sections(&layer, &eval, None);
        let gain = sections
            .iter()
            .find(|section| section.title == "properties.section.audio")
            .unwrap()
            .fields
            .iter()
            .find(|field| field.key() == "gain")
            .unwrap();
        let PropertyField::Float { value, .. } = gain else {
            panic!("gain must be numeric");
        };
        assert!((*value - 0.5).abs() < 1e-4);
    }

    #[test]
    fn transform_evaluates_in_layer_local_time() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let mut layer = test_layer(); // start_frame = 10
        layer.transform.position[0] = AnimationChannel::keyframes(curve);

        // Comp frame 15 → layer-local frame 5 → midpoint of the curve.
        let ctx = EvalContext::new(15, FrameRate::new(30, 1), (1920, 1080));
        let sections = solo_sections(&layer, &ctx, None);
        let pos_x = sections[1].fields.iter().find(|f| f.key() == "position_x");
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.5).abs() < 1e-4);
        } else {
            panic!("position_x field missing");
        }

        // Trimming the in edge shifts local time: comp 15 with in_frame 5
        // → local frame 10 → curve end (REQ-LAYER-006).
        let mut trimmed = layer.clone();
        trimmed.in_frame = 5;
        let sections = solo_sections(&trimmed, &ctx, None);
        let pos_x = sections[1].fields.iter().find(|f| f.key() == "position_x");
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!(
                (*value - 1.0).abs() < 1e-4,
                "trimmed local frame, got {value}"
            );
        } else {
            panic!("position_x field missing");
        }
    }

    fn layer_with_custom_param() -> Layer {
        use ravel_core::id::DataTypeId;
        let in_node = Node::new(NodeId::new(10), ravel_core::network::NET_IN_TYPE_KEY)
            .with_output(
                ravel_core::network::PORT_BASE_GEOMETRY,
                DataTypeId::GEOMETRY,
            )
            .with_output(ravel_core::network::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(3.5));
        let out = Node::new(NodeId::new(11), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out)
            .unwrap();
        Layer::new(LayerId::new(2), "Custom", network).with_time(0, 0, 300)
    }

    #[test]
    fn custom_parameters_expose_as_a_section() {
        let sections = solo_sections(&layer_with_custom_param(), &ctx(), None);
        let custom = sections
            .iter()
            .find(|s| s.title == "properties.section.parameters")
            .expect("custom section present");
        match &custom.fields[..] {
            [PropertyField::Float { key, value, .. }] => {
                assert_eq!(key, "custom.amount");
                assert!((*value - 3.5).abs() < f32::EPSILON);
            }
            other => panic!("unexpected custom fields: {other:?}"),
        }
        // Fixed ports never show up as parameters.
        assert!(
            !custom
                .fields
                .iter()
                .any(|f| f.key().contains("base_geometry"))
        );
    }

    /// A layer whose In node carries vector and color custom parameters.
    fn layer_with_multi_component_params() -> Layer {
        use ravel_core::id::DataTypeId;
        let in_node = Node::new(NodeId::new(10), ravel_core::network::NET_IN_TYPE_KEY)
            .with_output(
                ravel_core::network::PORT_BASE_GEOMETRY,
                DataTypeId::GEOMETRY,
            )
            .with_output("center", DataTypeId::VEC2)
            .with_output("tint", DataTypeId::COLOR)
            .with_param(
                "center",
                ParameterValue::Channel2([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(2.0),
                ]),
            )
            .with_param(
                "tint",
                ParameterValue::Channel4([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                ]),
            );
        let out = Node::new(NodeId::new(11), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out)
            .unwrap();
        Layer::new(LayerId::new(2), "Multi", network).with_time(0, 0, 300)
    }

    #[test]
    fn multi_component_params_expose_editable_fields() {
        let sections = solo_sections(&layer_with_multi_component_params(), &ctx(), None);
        let custom = sections
            .iter()
            .find(|s| s.title == "properties.section.parameters")
            .expect("custom section present");
        let center = custom
            .fields
            .iter()
            .find(|f| f.key() == "custom.center")
            .expect("center field");
        match center {
            PropertyField::Vector { components, .. } => assert_eq!(components, &[1.0, 2.0]),
            other => panic!("expected Vector, got {other:?}"),
        }
        let tint = custom
            .fields
            .iter()
            .find(|f| f.key() == "custom.tint")
            .expect("tint field");
        assert!(matches!(tint, PropertyField::Color { .. }));
    }

    #[test]
    fn apply_vector_and_color_custom_parameters() {
        let mut layer = layer_with_multi_component_params();
        assert!(apply_layer_field(
            &mut layer,
            "custom.center",
            &PropertyValue::Vector(vec![5.0, -3.0]),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "custom.tint",
            &PropertyValue::Color {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 0.5
            },
            0
        ));
        // Component-count mismatches are rejected.
        assert!(!apply_layer_field(
            &mut layer,
            "custom.center",
            &PropertyValue::Vector(vec![1.0, 2.0, 3.0]),
            0
        ));

        let c = ctx();
        let in_node = net::find_in_node(&layer.network).expect("in node");
        let Some(param) = in_node.parameters.iter().find(|p| p.key == "center") else {
            panic!("center param");
        };
        let ParameterValue::Channel2(chs) = &param.value else {
            panic!("expected Channel2");
        };
        assert!((chs[0].evaluate(0.0, &c) - 5.0).abs() < f32::EPSILON);
        assert!((chs[1].evaluate(0.0, &c) + 3.0).abs() < f32::EPSILON);
        let Some(param) = in_node.parameters.iter().find(|p| p.key == "tint") else {
            panic!("tint param");
        };
        let ParameterValue::Channel4(chs) = &param.value else {
            panic!("expected Channel4");
        };
        assert!((chs[2].evaluate(0.0, &c) - 0.75).abs() < f32::EPSILON);
        assert!((chs[3].evaluate(0.0, &c) - 0.5).abs() < f32::EPSILON);
    }

    /// A curve custom parameter reaches Properties as an editable curve row
    /// and an edited curve is written straight back into the In node.
    #[test]
    fn custom_curve_parameters_round_trip_through_the_curve_row() {
        use ravel_core::id::DataTypeId;
        use ravel_core::param_curve::CurveParam;
        let mut layer = layer_with_custom_param();
        let stored = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        let in_node = net::find_in_node(&layer.network).expect("in node");
        let mut updated = (**in_node).clone();
        updated.outputs.push(ravel_core::graph::OutputPort {
            name: "shape".into(),
            data_type: DataTypeId::SCALAR,
        });
        updated.parameters.push(ravel_core::graph::Parameter {
            key: "shape".into(),
            value: ParameterValue::Curve(stored.clone()),
        });
        layer.network = layer
            .network
            .clone()
            .replace_node(std::sync::Arc::new(updated));

        let sections = solo_sections(&layer, &ctx(), None);
        let field = sections
            .iter()
            .find(|s| s.title == "properties.section.parameters")
            .expect("custom section")
            .fields
            .iter()
            .find(|f| f.key() == "custom.shape")
            .cloned()
            .expect("shape field");
        match &field {
            PropertyField::Curve { curve, .. } => assert_eq!(curve, &stored),
            other => panic!("expected Curve, got {other:?}"),
        }

        let edited = CurveParam::linear([(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)]);
        assert!(apply_layer_field(
            &mut layer,
            "custom.shape",
            &PropertyValue::Curve(edited.clone()),
            0
        ));
        let in_node = net::find_in_node(&layer.network).expect("in node");
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "shape")
            .expect("shape param");
        assert_eq!(param.value, ParameterValue::Curve(edited));

        // A curve value cannot overwrite a parameter of another kind.
        assert!(!apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Curve(CurveParam::identity()),
            0
        ));
    }

    #[test]
    fn apply_layer_field_maps_shell_attributes() {
        let mut layer = test_layer();
        assert!(apply_layer_field(
            &mut layer,
            "name",
            &PropertyValue::String("Renamed Layer".into()),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "position_x",
            &PropertyValue::Float(42.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "scale_x",
            &PropertyValue::Float(50.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "opacity",
            &PropertyValue::Float(25.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "blend_mode",
            &PropertyValue::String("Multiply".into()),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "adjustment",
            &PropertyValue::Bool(true),
            0
        ));

        let c = ctx();
        assert_eq!(layer.name, "Renamed Layer");
        assert!((layer.transform.position[0].evaluate(0.0, &c) - 42.0).abs() < f32::EPSILON);
        assert!((layer.transform.scale[0].evaluate(0.0, &c) - 0.5).abs() < f32::EPSILON);
        assert!((layer.opacity.evaluate(0.0, &c) - 0.25).abs() < f32::EPSILON);
        assert_eq!(layer.blend_mode, BlendMode::Multiply);
        assert!(layer.adjustment);
        assert!(!apply_layer_field(
            &mut layer,
            "no_such_field",
            &PropertyValue::Float(1.0),
            0
        ));
    }

    #[test]
    fn apply_layer_field_keeps_the_display_interval_valid() {
        let mut layer = test_layer(); // in=0, out=300
        assert!(apply_layer_field(
            &mut layer,
            "in_frame",
            &PropertyValue::Int(400),
            0
        ));
        assert_eq!(layer.in_frame, 299, "in clamps below out");
        assert!(apply_layer_field(
            &mut layer,
            "out_frame",
            &PropertyValue::Int(0),
            0
        ));
        assert_eq!(layer.out_frame, 300, "out clamps above in");
    }

    /// Scrubbing an animated shell channel keys it at the edit frame instead
    /// of flattening the curve (REQ-LAYER-004).
    #[test]
    fn apply_layer_field_keys_animated_channels() {
        let mut layer = test_layer();
        assert!(toggle_layer_keyframe(&mut layer, "position_x", 0).unwrap());
        assert!(apply_layer_field(
            &mut layer,
            "position_x",
            &PropertyValue::Float(50.0),
            10
        ));
        let c = ctx();
        assert!((layer.transform.position[0].evaluate(0.0, &c) - 0.0).abs() < f32::EPSILON);
        assert!((layer.transform.position[0].evaluate(10.0, &c) - 50.0).abs() < f32::EPSILON);
        assert_eq!(layer_field_keyframed(&layer, "position_x", 10), Some(true));
        assert_eq!(layer_field_keyframed(&layer, "position_x", 5), Some(false));
    }

    /// The key toggle converts a constant custom parameter to a keyframed
    /// channel, and removes it again (REQ-LAYER-002/004).
    #[test]
    fn toggle_layer_keyframe_converts_custom_float_param() {
        let mut layer = layer_with_custom_param();
        assert_eq!(
            layer_field_keyframed(&layer, "custom.amount", 0),
            Some(false)
        );
        assert_eq!(
            toggle_layer_keyframe(&mut layer, "custom.amount", 4),
            Some(true)
        );
        // Keyframed with the constant value (3.5) at frame 4.
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("converted to a channel");
        };
        let c = ctx();
        assert!((ch.evaluate(4.0, &c) - 3.5).abs() < f32::EPSILON);
        // Scrubbing the keyframed param updates the curve, not the variant.
        assert!(apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Float(9.0),
            4
        ));
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("still a channel");
        };
        assert!((ch.evaluate(4.0, &c) - 9.0).abs() < f32::EPSILON);
        // Toggling off removes the last key → constant again.
        assert_eq!(
            toggle_layer_keyframe(&mut layer, "custom.amount", 4),
            Some(false)
        );
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("constant channel after last key removal");
        };
        assert_eq!(
            ch.source,
            ravel_core::animation::channel::ChannelSource::Constant(9.0)
        );
        // Non-animatable fields report None.
        assert_eq!(layer_field_keyframed(&layer, "start_frame", 0), None);
        assert_eq!(toggle_layer_keyframe(&mut layer, "start_frame", 0), None);
    }

    fn layer_with_vec2_param() -> Layer {
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        use ravel_core::types::Vec2;
        let mut keyed = KeyframeCurve::new();
        keyed.insert(3, 0.5, Interpolation::Bezier);
        keyed.modify(3, 0.5, Some((Vec2(-1.0, 0.0), Vec2(1.0, 0.0))));
        let in_node = Node::new(NodeId::new(20), ravel_core::network::NET_IN_TYPE_KEY)
            .with_output("offset", DataTypeId::VEC2)
            .with_param(
                "offset",
                ParameterValue::Channel2([
                    AnimationChannel::keyframes(keyed),
                    AnimationChannel::constant(0.25),
                ]),
            );
        let network = Graph::new().add_node(in_node).unwrap();
        Layer::new(LayerId::new(3), "V", network).with_time(0, 0, 300)
    }

    /// Enabling a partially keyed multi-component field inserts only the
    /// missing components; existing keys keep interpolation and tangents.
    #[test]
    fn partial_multi_component_toggle_preserves_existing_keys() {
        let mut layer = layer_with_vec2_param(); // component 0 keyed at 3, 1 constant
        assert_eq!(
            layer_field_keyframed(&layer, "custom.offset", 3),
            Some(false),
            "not all components keyed"
        );
        assert_eq!(
            toggle_layer_keyframe(&mut layer, "custom.offset", 3),
            Some(true)
        );

        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "offset")
            .unwrap();
        let ParameterValue::Channel2(chs) = &param.value else {
            panic!("still Channel2");
        };
        let ravel_core::animation::channel::ChannelSource::Keyframes(curve0) = &chs[0].source
        else {
            panic!("component 0 stays keyframed");
        };
        let key = &curve0.keyframes()[0];
        assert_eq!(
            key.interpolation,
            ravel_core::animation::interpolation::Interpolation::Bezier
        );
        assert_eq!(key.tangent_in, ravel_core::types::Vec2(-1.0, 0.0));
        assert_eq!(curve0.len(), 1, "no duplicate key inserted");
        let ravel_core::animation::channel::ChannelSource::Keyframes(curve1) = &chs[1].source
        else {
            panic!("component 1 became keyframed");
        };
        assert!((curve1.sample(3.0) - 0.25).abs() < f32::EPSILON);
    }

    /// Scrubbing an animated channel onto an existing key updates the value
    /// but keeps the key's interpolation mode and tangents.
    #[test]
    fn apply_layer_field_preserves_keyframe_tangents() {
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        use ravel_core::types::Vec2;
        let mut layer = test_layer();
        let mut curve = KeyframeCurve::new();
        curve.insert(5, 0.0, Interpolation::Bezier);
        curve.modify(5, 0.0, Some((Vec2(-2.0, 0.0), Vec2(2.0, 0.0))));
        layer.transform.position[0] = AnimationChannel::keyframes(curve);

        assert!(apply_layer_field(
            &mut layer,
            "position_x",
            &PropertyValue::Float(42.0),
            5
        ));
        let ravel_core::animation::channel::ChannelSource::Keyframes(curve) =
            &layer.transform.position[0].source
        else {
            panic!("stays keyframed");
        };
        let key = &curve.keyframes()[0];
        assert!((key.value - 42.0).abs() < f32::EPSILON);
        assert_eq!(key.interpolation, Interpolation::Bezier);
        assert_eq!(key.tangent_out, Vec2(2.0, 0.0));
    }

    #[test]
    fn apply_custom_parameter_updates_the_in_node() {
        let mut layer = layer_with_custom_param();
        assert!(apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Float(9.0),
            0
        ));
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let value = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .and_then(|p| p.value.as_float());
        assert_eq!(value, Some(9.0));

        // Type mismatches and unknown parameters are rejected.
        assert!(!apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Bool(true),
            0
        ));
        assert!(!apply_layer_field(
            &mut layer,
            "custom.missing",
            &PropertyValue::Float(1.0),
            0
        ));
    }

    #[test]
    fn timing_section_shows_start_frame() {
        let sections = solo_sections(&test_layer(), &ctx(), None);
        let timing = &sections[2];
        let start = timing.fields.iter().find(|f| f.key() == "start_frame");
        if let Some(PropertyField::Int { value, .. }) = start {
            assert_eq!(*value, 10);
        }
    }
}
